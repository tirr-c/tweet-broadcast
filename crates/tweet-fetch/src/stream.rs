use std::time::Duration;

use futures_util::{Stream, TryFutureExt};
use log::{error, info};

use tweet_model as model;
use crate::{
    backoff::{Backoff, BackoffType},
    util,
    concat_param,
    Error,
    TwitterClient,
};

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
                return Err(BackoffType::Network);
            }
            if let Some(status) = err.status() {
                if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                    error!("Request is ratelimited");
                    return Err(BackoffType::Ratelimit);
                } else if status.is_server_error() {
                    error!("Server side error: {}", err);
                    return Err(BackoffType::Server);
                }
            }

            error!("Unknown error: {}", err);
            Err(BackoffType::Server)
        })
        .await
}

pub fn make_stream(
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
                                    error!("Parse error: {}, while parsing: {}", e, string);
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
