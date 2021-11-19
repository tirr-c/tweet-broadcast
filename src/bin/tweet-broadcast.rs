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

    async_stream::try_stream! {
        let mut s = Vec::new();
        while let Some(bytes) = read_single(&mut resp).await? {
            let mut lines = bytes.split(|&b| b == b'\n');
            s.extend(lines.next().unwrap().iter().copied());
            for line in lines {
                let string = String::from_utf8_lossy(&s);
                let string = string.as_ref().trim();
                if !string.is_empty() {
                    let line = serde_json::from_str(string)?;
                    yield line;
                }
                s = line.to_vec();
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
                Ok(line) => line,
                Err(e) => {
                    eprintln!("Stream error: {}", e);
                    backoff_type.add_network();
                    break;
                }
            };
            println!("{:#?}", line);
        }
    }
}