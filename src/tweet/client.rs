use std::ops::Deref;
use std::path::{Path, PathBuf};

use log::{error, info};
use reqwest::{
    header::{self, HeaderMap, HeaderValue},
    Client,
};
use sentry::Breadcrumb;

use crate::{
    tweet::{model, util},
    Backoff, BackoffType, Router,
};

mod list;
mod stream;
use stream::connect_once;

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
        let mut backoff = Backoff::new();
        backoff.backoff_fn(|duration| {
            let sleep_msecs = duration.as_millis();
            info!("Waiting {} ms...", sleep_msecs);
            sentry::add_breadcrumb(Breadcrumb {
                category: Some(String::from("network")),
                message: Some(format!("Waiting for {} ms", sleep_msecs)),
                level: sentry::Level::Info,
                ..Default::default()
            });
            Box::pin(tokio::time::sleep(duration))
        });

        loop {
            let resp = backoff
                .run_fn(|| async {
                    let err = match connect_once(self.client.clone()).await {
                        Ok(resp) => return Ok(resp),
                        Err(err) => err,
                    };

                    if err.is_connect() {
                        error!("Failed to connect: {}", err);
                        sentry::capture_error(&err);
                        return Err(BackoffType::Network);
                    }
                    if let Some(status) = err.status() {
                        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                            error!("Request is ratelimited");
                            sentry::capture_message(
                                "Request is ratelimited",
                                sentry::Level::Warning,
                            );
                            return Err(BackoffType::Ratelimit);
                        } else if status.is_server_error() {
                            error!("Server side error: {}", err);
                            sentry::capture_error(&err);
                            return Err(BackoffType::Server);
                        }
                    }

                    error!("Unknown error: {}", err);
                    sentry::capture_error(&err);
                    Err(BackoffType::Server)
                })
                .await;
            info!("Connected to filtered stream");

            stream::run_line_loop(self, &self.cache_dir, resp, router)
                .await
                .err();
        }
    }

    pub async fn run_list_loop(&self) -> Result<std::convert::Infallible, crate::Error> {
        use futures_util::StreamExt;

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

            let stream = config.run_once(self, &self.cache_dir, catchup);
            futures_util::pin_mut!(stream);

            while let Some((id, ret)) = stream.next().await {
                if let Err(e) = ret {
                    error!("List fetch for {} failed: {}", id, e);
                    let mut event = sentry::event_from_error(&e);
                    event.tags.insert(String::from("id"), id);
                    sentry::capture_event(event);
                } else {
                    log::debug!("List fetch for {} successful", id);
                }
            }

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
