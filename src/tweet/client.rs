use std::ops::Deref;
use std::path::{Path, PathBuf};

use log::{error, info};
use reqwest::{
    header::{self, HeaderMap, HeaderValue},
    Client,
};

use crate::{
    tweet::{model, util},
    Router,
};

mod list;
mod stream;

pub use list::ListHead;

#[derive(Debug, Clone)]
pub struct TwitterClient {
    client: reqwest::Client,
    cache_dir: PathBuf,
}

impl TwitterClient {
    pub fn new(token: impl AsRef<str>, cache_dir: impl Into<PathBuf>) -> Self {
        let token = token.as_ref();

        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {}", token)).unwrap(),
        );

        let client = Client::builder()
            .gzip(true)
            .brotli(true)
            .user_agent(concat!(
                env!("CARGO_PKG_NAME"),
                "/",
                env!("CARGO_PKG_VERSION")
            ))
            .default_headers(headers)
            .build()
            .expect("Failed to build HTTP client");

        Self {
            client,
            cache_dir: cache_dir.into(),
        }
    }

    pub fn cache_dir(&self) -> &Path {
        &self.cache_dir
    }
}

impl TwitterClient {
    pub async fn retrieve(
        &self,
        ids: &[&str],
    ) -> Result<model::ResponseItem<Vec<model::Tweet>>, crate::Error> {
        use futures_util::{TryFutureExt, TryStreamExt};

        const TWEET_ENDPOINT: &str = "https://api.twitter.com/2/tweets";
        let mut url = TWEET_ENDPOINT.parse::<reqwest::Url>().unwrap();
        util::append_query_param_for_tweet(&mut url);

        Ok(match ids {
            [] => Default::default(),
            [id] => {
                url.path_segments_mut().unwrap().push(id);

                let res = self
                    .client
                    .get(url)
                    .send()
                    .await?
                    .error_for_status()?
                    .json::<model::TwitterResponse<model::Tweet>>()
                    .await?
                    .into_result()?;
                let model::ResponseItem {
                    data,
                    includes,
                    meta,
                } = res;
                model::ResponseItem {
                    data: vec![data],
                    includes,
                    meta,
                }
            }
            ids => {
                let mut req_fut = futures_util::stream::FuturesOrdered::new();
                for ids in ids.chunks(100) {
                    // 100 tweets at a time
                    let id_param = ids.join(",");
                    let mut url = url.clone();
                    url.query_pairs_mut().append_pair("ids", &id_param).finish();

                    req_fut.push(
                        self.client
                            .get(url)
                            .send()
                            .map_err(crate::Error::from)
                            .and_then(|resp| async move {
                                let resp = resp
                                    .error_for_status()?
                                    .json::<model::TwitterResponse<Vec<model::Tweet>>>()
                                    .await?
                                    .into_result()?;
                                Ok(resp)
                            }),
                    );
                }

                req_fut
                    .try_fold(
                        model::ResponseItem::<Vec<_>>::default(),
                        |mut base, tweets| async move {
                            base.data.extend(tweets.data);
                            base.includes.augment(tweets.includes);
                            Ok(base)
                        },
                    )
                    .await?
            }
        })
    }
}

impl TwitterClient {
    pub async fn run_stream(
        &self,
        router: &mut Router,
    ) -> Result<std::convert::Infallible, crate::Error> {
        loop {
            stream::run_line_loop(self, &self.cache_dir, router)
                .await
                .err();
        }
    }

    pub async fn run_list_loop(&self) -> Result<std::convert::Infallible, crate::Error> {
        use futures_util::{StreamExt, TryStreamExt};

        let webhook_client = reqwest::Client::builder().build().unwrap();
        let webhook_client = &webhook_client;

        let config = list::ListsConfig::from_cache_dir(&self.cache_dir).await?;
        let mut timer = tokio::time::interval(std::time::Duration::from_secs(60));
        info!("Started list fetch loop");

        let mut catchup = true;
        loop {
            timer.tick().await;
            log::debug!(
                "Running list fetch{}",
                if catchup { " (catch-up)" } else { "" }
            );

            let stream = futures_util::stream::FuturesUnordered::new();
            for (id, meta) in config.lists() {
                let fut = async move {
                    let ret = async {
                        let mut head =
                            ListHead::from_cache_dir(id.to_owned(), &self.cache_dir).await?;
                        let first_time = head.head().is_none();
                        let tweets = head.load_and_update(self, catchup).await?;
                        head.save_cache(&self.cache_dir).await?;
                        Ok::<_, crate::Error>((tweets, first_time))
                    }
                    .await;
                    let (tweets, first_time) = match ret {
                        Ok(tweets) => tweets,
                        Err(e) => {
                            error!("List fetch for {} failed: {}", id, e);
                            let mut event = sentry::event_from_error(&e);
                            event.tags.insert(String::from("id"), id.into());
                            sentry::capture_event(event);
                            return;
                        }
                    };
                    let model::ResponseItem {
                        data: tweets,
                        includes,
                        ..
                    } = &tweets;

                    let webhooks_fut = futures_util::stream::FuturesUnordered::new();
                    for webhook in meta.webhooks().iter().cloned() {
                        let webhook_client = webhook_client.clone();
                        webhooks_fut.push(async move {
                            if catchup && tweets.len() > 5 {
                                list::send_catchup_webhook(
                                    webhook_client,
                                    webhook,
                                    id,
                                    tweets.len(),
                                )
                                .await?;
                            } else if first_time {
                                list::send_first_time_webhook(webhook_client, webhook, id).await?;
                            } else {
                                for tweet in tweets {
                                    list::send_webhook(
                                        webhook_client.clone(),
                                        webhook.clone(),
                                        tweet,
                                        includes,
                                    )
                                    .await?;
                                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                                }
                            }
                            Ok::<_, crate::Error>(())
                        });
                    }

                    let ret = webhooks_fut.try_collect::<()>().await;
                    if let Err(e) = ret {
                        log::error!("Failed to send webhook for {}: {}", id, e);
                        let mut event = sentry::event_from_error(&e);
                        event.tags.insert(String::from("id"), id.into());
                        sentry::capture_event(event);
                        return;
                    }

                    log::debug!("List fetch for {} successful", id);
                };
                stream.push(fut);
            }
            stream.collect::<()>().await;

            catchup = false;
        }
    }
}

impl Deref for TwitterClient {
    type Target = reqwest::Client;

    fn deref(&self) -> &Self::Target {
        &self.client
    }
}
