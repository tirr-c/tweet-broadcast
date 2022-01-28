use std::time::Duration;

use futures_util::{Stream, StreamExt, TryFutureExt};
use log::{error, info};
use sentry::Breadcrumb;

use crate::tweet::{concat_param, model};
use crate::{Error, Router};

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

pub async fn connect_once(client: reqwest::Client) -> reqwest::Result<reqwest::Response> {
    client
        .get(create_endpoint_url())
        .send()
        .await?
        .error_for_status()
}

fn make_stream(
    mut resp: reqwest::Response,
) -> impl Stream<Item = Result<model::TwitterResponse<model::Tweet, model::StreamMeta>, Error>> {
    async fn read_single(resp: &mut reqwest::Response) -> Result<Option<bytes::Bytes>, Error> {
        Ok(tokio::time::timeout(Duration::from_secs(30), resp.chunk())
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::TimedOut, e))
            .await??)
    }

    async_stream::stream! {
        let mut s = Vec::new();
        loop {
            match read_single(&mut resp).await {
                Err(e) => {
                    yield Err(e);
                    break;
                }
                Ok(None) => {
                    break;
                }
                Ok(Some(bytes)) => {
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
                            yield serde_json::from_str(string)
                                .map_err(|e| Error::parse_error(string.to_owned(), e));
                        }
                        s = line.to_vec();
                    }
                }
            }
        }
        drop(resp);
    }
}

pub async fn run_line_loop(
    client: &super::TwitterClient,
    cache_dir: &std::path::Path,
    resp: reqwest::Response,
    router: &mut Router,
) -> Result<std::convert::Infallible, Error> {
    let discord_client = reqwest::Client::builder().build().unwrap();

    let lines = make_stream(resp);
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

        let mut line = match line_result {
            Ok(model::TwitterResponse::Error(e)) => {
                error!("Twitter error: {}", e);
                sentry::capture_error(&e);
                return Err(e.into());
            }
            Ok(model::TwitterResponse::Ok(line)) => line,
            Err(Error::Parse(e)) => {
                error!("Parse error: {}", e);
                error!("  while parsing: {}", e.string());
                let mut ev = sentry::event_from_error(&e);
                ev.extra.insert(String::from("message"), e.string().into());
                sentry::capture_event(ev);
                continue;
            }
            Err(e) => {
                error!("Stream error: {}", e);
                sentry::capture_error(&e);
                return Err(e);
            }
        };

        match crate::tweet::util::load_batch_augment_data(
            client,
            std::slice::from_ref(&line.data),
            &line.includes,
        )
        .await
        {
            Ok(Some(augment_data)) => {
                line.includes.augment(augment_data.includes);
            }
            Ok(_) => {}
            Err(e) => {
                error!("Failed to retrieve original tweet: {}", e);
                sentry::capture_error(&e);
                return Err(e);
            }
        };

        let route_result = match router.call(&line, cache_dir).await {
            Ok(route_result) => route_result,
            Err(e) => {
                error!("Failed to route: {}", e);
                error!("Input: {:#?}", line);
                let mut ev = sentry::event_from_error(&e);
                ev.extra
                    .insert(String::from("data"), format!("{:?}", line).into());
                sentry::capture_event(ev);
                continue;
            }
        };

        let real_tweet = if let Some(rt_id) = line.data.get_retweet_source() {
            line.includes.get_tweet(rt_id).unwrap()
        } else {
            &line.data
        };
        let author_id = real_tweet.author_id().unwrap();
        let author = line.includes.get_user(author_id).unwrap();
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
                rules = line.meta.matching_rules().iter().map(|r| r.tag()).collect::<Vec<_>>(),
                score = score,
            );
        }
    }
}
