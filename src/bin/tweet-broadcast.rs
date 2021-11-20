use std::time::Duration;

use futures_util::{Stream, StreamExt, TryFutureExt};
use reqwest::Client;
use tokio::signal::unix as unix_signal;

use tweet_broadcast::{
    tweet,
    BackoffType,
    Error,
};

macro_rules! concat_param {
    ($param1:literal $(, $param:literal)*) => {
        concat!($param1 $(, ",", $param)*)
    };
}

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

fn make_stream(mut resp: reqwest::Response) -> impl Stream<Item = Result<tweet::TwitterResponse<tweet::Tweet>, Error>> {
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
                            yield serde_json::from_str(string)
                                .map_err(|e| Error::parse_error(string.to_owned(), e));
                        }
                        s = line.to_vec();
                    }
                }
            }
        }
    }
}

#[tokio::main]
async fn main() {
    let token = std::env::var("TWITTER_APP_TOKEN").expect("TWITTER_APP_TOKEN not found or invalid");
    let endpoint_url = create_endpoint_url();
    let client = Client::builder()
        .gzip(true)
        .brotli(true)
        .user_agent(concat!(
            env!("CARGO_PKG_NAME"),
            "/",
            env!("CARGO_PKG_VERSION")
        ))
        .build()
        .expect("Failed to build HTTP client");

    let platform = v8::Platform::new(0, false).make_shared();
    v8::V8::initialize_platform(platform);
    v8::V8::initialize();
    let mut isolate = v8::Isolate::new(Default::default());
    let mut router = tweet_broadcast::Router::new(&mut isolate);
    let mut route_fn = router.load().expect("Failed to load router");

    let mut sigterm = unix_signal::signal(unix_signal::SignalKind::terminate())
        .expect("Failed to listen SIGTERM");
    let mut sigint =
        unix_signal::signal(unix_signal::SignalKind::interrupt()).expect("Failed to listen SIGINT");
    let mut sigquit =
        unix_signal::signal(unix_signal::SignalKind::quit()).expect("Failed to listen SIGQUIT");

    let sigterm = sigterm.recv();
    futures_util::pin_mut!(sigterm);
    let sigint = sigint.recv();
    futures_util::pin_mut!(sigint);
    let sigquit = sigquit.recv();
    futures_util::pin_mut!(sigquit);
    let mut sig = futures_util::future::select_all([sigterm, sigint, sigquit]);

    let mut backoff_type = BackoffType::None;
    'retry: loop {
        if backoff_type.should_backoff() {
            let sleep_msecs = backoff_type.sleep_msecs();
            eprintln!("Waiting {} ms...", sleep_msecs);
            let sleep = tokio::time::sleep(Duration::from_millis(sleep_msecs));
            futures_util::pin_mut!(sleep);
            tokio::select! {
                _ = &mut sleep => {},
                _ = &mut sig => {
                    break;
                },
            }
        }

        let resp = client
            .get(endpoint_url.clone())
            .bearer_auth(&token)
            .send()
            .await;
        let resp = match resp {
            Ok(resp) => resp,
            Err(e) => {
                eprintln!("Failed to connect: {}", e);
                backoff_type.add_network();
                continue;
            }
        };

        let resp = match resp.error_for_status() {
            Ok(resp) => resp,
            Err(e) if e.status() == Some(reqwest::StatusCode::TOO_MANY_REQUESTS) => {
                eprintln!("Request is ratelimited");
                backoff_type.add_ratelimit();
                continue;
            }
            Err(e) if e.status().map(|s| s.is_server_error()).unwrap_or(false) => {
                eprintln!("Server side error: {}", e);
                backoff_type.add_server();
                continue;
            }
            Err(e) => {
                eprintln!("Unknown error: {}", e);
                break;
            }
        };

        backoff_type = BackoffType::None;
        let lines = make_stream(resp);
        futures_util::pin_mut!(lines);

        loop {
            let line_result = tokio::select! {
                r = lines.next() => {
                    match r {
                        Some(line_result) => line_result,
                        None => {
                            eprintln!("Stream closed");
                            backoff_type.add_network();
                            break;
                        },
                    }
                },
                _ = &mut sig => {
                    break 'retry;
                },
            };
            let line = match line_result {
                Ok(tweet::TwitterResponse::Error(e)) => {
                    eprintln!("Twitter error: {}", e);
                    backoff_type.add_server();
                    break;
                }
                Ok(tweet::TwitterResponse::Ok(line)) => line,
                Err(Error::Parse(e)) => {
                    eprintln!("Parse error: {}", e);
                    eprintln!("  while parsing: {}", e.string());
                    continue;
                }
                Err(e) => {
                    eprintln!("Stream error: {}", e);
                    backoff_type.add_network();
                    break;
                }
            };

            let routes = match route_fn.call(&line) {
                Ok(routes) => routes,
                Err(e) => {
                    eprintln!("Failed to route: {}", e);
                    eprintln!("Input: {:#?}", line);
                    continue;
                },
            };

            for route in routes {
                let client = client.clone();
                tokio::spawn(async move {
                    let result = client
                        .post(route.url)
                        .header("content-type", "application/json")
                        .body(serde_json::to_vec(&route.payload).unwrap())
                        .send()
                        .await;
                    match result {
                        Err(e) => {
                            eprintln!("Failed to send: {}", e);
                        }
                        Ok(resp) => {
                            if !resp.status().is_success() {
                                let resp = resp.text().await.unwrap();
                                eprintln!("Submission failed: {}", resp);
                            }
                        }
                    }
                });
            }

            let real_tweet = if let Some(rt_id) = line.data().get_retweet_source() {
                line.includes().get_tweet(rt_id).unwrap()
            } else {
                line.data()
            };
            let author_id = real_tweet.author_id().unwrap();
            let author = line.includes().get_user(author_id).unwrap();
            let tweet_metrics = real_tweet.metrics().unwrap();
            let user_metrics = author.metrics().unwrap();
            let score = tweet_broadcast::compute_score(tweet_metrics, user_metrics, real_tweet.created_at().unwrap());

            println!(
                "{author_name} (@{author_username}):{possibly_sensitive}",
                author_name = author.name(),
                author_username = author.username(),
                possibly_sensitive = if real_tweet.possibly_sensitive() { " [!]" } else { "" }
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
                println!("    {} ({}x{})", media.url().unwrap(), media.width(), media.height());
            }
            println!();
        }
    }
}
