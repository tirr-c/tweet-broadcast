use std::time::Duration;

use futures_util::{Stream, StreamExt, TryFutureExt};
use log::{error, info};
use sentry::Breadcrumb;

use crate::{Error, Router};
use crate::tweet::{concat_param, model};

fn create_endpoint_url() -> reqwest::Url {
    const STREAM_ENDPOINT: &'static str = "https://api.twitter.com/2/tweets/search/stream";
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
) -> impl Stream<Item = Result<model::TwitterResponse<model::Tweet>, Error>> {
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

async fn retrieve_single(
    client: reqwest::Client,
    id: &str,
) -> Result<model::TwitterResponse<model::Tweet>, Error> {
    const TWEET_ENDPOINT: &'static str = "https://api.twitter.com/2/tweets";
    let mut url = TWEET_ENDPOINT.parse::<reqwest::Url>().unwrap();
    url.path_segments_mut().unwrap().push(id);
    url.query_pairs_mut()
        .append_pair(
            "expansions",
            concat_param!["author_id", "attachments.media_keys"],
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

    let resp = client
        .get(url)
        .send()
        .await?
        .error_for_status()?;

    let resp = resp.json::<model::TwitterResponse<model::Tweet>>().await?;
    Ok(resp)
}

pub async fn run_line_loop(
    client: reqwest::Client,
    cache_dir: &std::path::Path,
    resp: reqwest::Response,
    router: &mut Router,
) -> Result<std::convert::Infallible, Error>
{
    let lines = make_stream(resp);
    futures_util::pin_mut!(lines);

    loop {
        let line_result = match lines.next().await {
            Some(line_result) => line_result,
            None => {
                info!("Stream closed");
                sentry::capture_message("Stream closed", sentry::Level::Info);
                return Err(Error::StreamClosed);
            },
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
                return Err(e.into());
            }
        };

        let real_tweet = if let Some(rt_id) = line.data().get_retweet_source() {
            line.includes().get_tweet(rt_id).unwrap()
        } else {
            line.data()
        };

        // check media first
        let media_keys = real_tweet.media_keys();
        let key_count = media_keys.len();
        let media = media_keys
            .iter()
            .filter_map(|k| line.includes().get_media(k))
            .collect::<Vec<_>>();

        if key_count != media.len() {
            // retrieve tweet again
            sentry::add_breadcrumb(Breadcrumb {
                category: Some(String::from("tweet")),
                message: Some(String::from("Media info missing, fetching tweet info")),
                level: sentry::Level::Info,
                data: [
                    (String::from("id"), line.data().id().into()),
                    (String::from("referenced_tweet_id"), real_tweet.id().into()),
                ]
                .into_iter()
                .collect(),
                ..Default::default()
            });
            let tweet = retrieve_single(client.clone(), real_tweet.id()).await;
            match tweet {
                Ok(model::TwitterResponse::Ok(t)) => {
                    line.augment(t);
                }
                Ok(model::TwitterResponse::Error(e)) => {
                    error!("Failed to retrieve original tweet: {}", e);
                    sentry::capture_error(&e);
                }
                Err(e) => {
                    error!("Failed to retrieve original tweet: {}", e);
                    sentry::capture_error(&e);
                }
            }
        }

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

        let real_tweet = if let Some(rt_id) = line.data().get_retweet_source() {
            line.includes().get_tweet(rt_id).unwrap()
        } else {
            line.data()
        };
        let author_id = real_tweet.author_id().unwrap();
        let author = line.includes().get_user(author_id).unwrap();
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
            println!(
                "No routes: {}{}",
                real_tweet.id(),
                if cached { " (cached)" } else { "" }
            );
            println!("  Score: {:.4}", score);
        } else {
            if !cached {
                if let Err(e) = route_result.save_cache(cache_dir).await {
                    error!("Failed to save metadata: {}", e);
                    sentry::capture_error(&e);
                }
            }

            for route in routes {
                let client = client.clone();
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

            println!(
                "{author_name} (@{author_username}):{possibly_sensitive}",
                author_name = author.name(),
                author_username = author.username(),
                possibly_sensitive = if real_tweet.possibly_sensitive() {
                    " [!]"
                } else {
                    ""
                }
            );
            for line in real_tweet.unescaped_text().split('\n') {
                println!("  {}", line);
            }
            print!("    Tags:");
            for rule in line.matching_rules().unwrap() {
                print!(" {}", rule.tag());
            }
            println!();
            println!("    Score: {:.4}", score);
            for key in real_tweet.media_keys() {
                let media = line.includes().get_media(key).unwrap();
                println!(
                    "    {} ({}x{})",
                    media.url().unwrap(),
                    media.width(),
                    media.height()
                );
            }
            println!();
        }
    }
}
