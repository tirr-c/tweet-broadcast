use std::time::Duration;

use futures_util::{Stream, StreamExt, TryFutureExt};
use log::{error, info};
use sentry::Breadcrumb;

use crate::tweet::{concat_param, model, util, TwitterClient};
use crate::{Backoff, BackoffType, Error, Router};

fn create_endpoint_url() -> reqwest::Url {
    const STREAM_ENDPOINT: &str = "https://api.twitter.com/2/tweets/search/stream";
    let mut url = reqwest::Url::parse(STREAM_ENDPOINT).unwrap();
    url.query_pairs_mut()
        .append_pair(
            "expansions",
            concat_param![
                "author_id",
                "referenced_tweets.id",
                "referenced_tweets.id.author_id",
                "attachments.media_keys"
            ],
        )
        .append_pair(
            "tweet.fields",
            concat_param![
                "created_at",
                "entities",
                "public_metrics",
                "possibly_sensitive"
            ],
        )
        .append_pair(
            "user.fields",
            concat_param!["profile_image_url", "public_metrics"],
        )
        .append_pair(
            "media.fields",
            concat_param!["width", "height", "url", "preview_image_url"],
        )
        .finish();
    url
}

async fn connect_once(client: &reqwest::Client) -> reqwest::Result<reqwest::Response> {
    client
        .get(create_endpoint_url())
        .send()
        .await?
        .error_for_status()
}

async fn connect_with_backoff(client: &TwitterClient) -> reqwest::Response {
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

    backoff
        .run_fn(|| async {
            let err = match connect_once(client).await {
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
                    sentry::capture_message("Request is ratelimited", sentry::Level::Warning);
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
        .await
}

fn make_stream(
    client: TwitterClient,
) -> impl Stream<Item = Result<model::ResponseItem<model::Tweet, model::StreamMeta>, Error>> {
    async fn read_single(resp: &mut reqwest::Response) -> Result<Option<bytes::Bytes>, Error> {
        Ok(tokio::time::timeout(Duration::from_secs(30), resp.chunk())
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::TimedOut, e))
            .await??)
    }

    async_stream::try_stream! {
        let mut resp = connect_with_backoff(&client).await;
        info!("Connected to filtered stream");

        let mut s = Vec::new();
        loop {
            match read_single(&mut resp).await? {
                None => {
                    break;
                }
                Some(bytes) => {
                    let mut lines = bytes.split(|&b| b == b'\n');
                    s.extend(lines.next().unwrap().iter().copied());
                    for line in lines {
                        let string = String::from_utf8_lossy(&s);
                        let string = string.as_ref().trim();
                        if !string.is_empty() {
                            sentry::add_breadcrumb(Breadcrumb {
                                category: Some(String::from("parse")),
                                message: Some(string.to_owned()),
                                level: sentry::Level::Info,
                                ..Default::default()
                            });
                            let res = serde_json::from_str::<model::TwitterResponse<_, _>>(string);
                            match res {
                                Ok(res) => {
                                    let mut item = res.into_result()?;
                                    let augment_data = util::load_batch_augment_data(
                                        &client,
                                        std::slice::from_ref(&item.data),
                                        &item.includes,
                                    ).await?;
                                    if let Some(augment_data) = augment_data {
                                        item.includes.augment(augment_data.includes);
                                    }
                                    yield item;
                                }
                                Err(e) => {
                                    error!("Parse error: {}", e);
                                    error!("while parsing: {}", string);
                                    let mut ev = sentry::event_from_error(&e);
                                    ev.extra.insert(String::from("message"), string.into());
                                    sentry::capture_event(ev);
                                }
                            }
                        }
                        s = line.to_vec();
                    }
                }
            }
        }
    }
}

pub async fn run_line_loop(
    client: &TwitterClient,
    cache_dir: &std::path::Path,
    router: &mut Router,
) -> Result<std::convert::Infallible, Error> {
    let discord_client = reqwest::Client::builder().build().unwrap();

    let lines = make_stream(client.clone());
    futures_util::pin_mut!(lines);

    loop {
        let line_result = match lines.next().await {
            Some(line_result) => line_result,
            None => {
                info!("Stream closed");
                sentry::capture_message("Stream closed", sentry::Level::Info);
                return Err(Error::StreamClosed);
            }
        };

        let tweet = match line_result {
            Ok(line) => line,
            Err(e) => {
                error!("Stream error: {}", e);
                sentry::capture_error(&e);
                return Err(e);
            }
        };

        let route_result = match router.call(&tweet, cache_dir).await {
            Ok(route_result) => route_result,
            Err(e) => {
                error!("Failed to route: {}", e);
                error!("Input: {:#?}", tweet);
                let mut ev = sentry::event_from_error(&e);
                ev.extra
                    .insert(String::from("data"), format!("{:?}", tweet).into());
                sentry::capture_event(ev);
                continue;
            }
        };

        let real_tweet = if let Some(rt_id) = tweet.data.get_retweet_source() {
            tweet.includes.get_tweet(rt_id).unwrap()
        } else {
            &tweet.data
        };
        let author_id = real_tweet.author_id().unwrap();
        let author = tweet.includes.get_user(author_id).unwrap();
        let tweet_metrics = real_tweet.metrics().unwrap();
        let user_metrics = author.metrics().unwrap();
        let score = crate::compute_score(
            tweet_metrics,
            user_metrics,
            real_tweet.created_at().unwrap(),
        );

        let routes = route_result.routes();
        let cached = route_result.cached();
        if routes.is_empty() {
            log::debug!(
                "No routes: {}{}, score: {:.4}",
                real_tweet.id(),
                if cached { " (cached)" } else { "" },
                score,
            );
        } else {
            if !cached {
                if let Err(e) = route_result.save_cache(cache_dir).await {
                    error!("Failed to save metadata: {}", e);
                    sentry::capture_error(&e);
                }
            }

            for route in routes {
                let client = discord_client.clone();
                let url = route.url.clone();
                let payload = serde_json::to_vec(&route.payload).unwrap();
                tokio::spawn(async move {
                    let result = client
                        .post(url)
                        .header("content-type", "application/json")
                        .body(payload)
                        .send()
                        .await;
                    match result {
                        Err(e) => {
                            error!("Failed to send: {}", e);
                            sentry::capture_error(&e);
                        }
                        Ok(resp) => {
                            if !resp.status().is_success() {
                                let resp = resp.text().await.unwrap();
                                error!("Submission failed: {}", resp);
                            }
                        }
                    }
                });
            }

            log::debug!(
                "Relaying tweet by @{author_username}, matching rule(s): {rules:?}, score: {score:.4}",
                author_username = author.username(),
                rules = tweet.meta.matching_rules().iter().map(|r| r.tag()).collect::<Vec<_>>(),
                score = score,
            );
        }
    }
}
