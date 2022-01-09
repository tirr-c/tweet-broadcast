use std::collections::HashMap;
use std::path::Path;

use futures_util::{FutureExt, Stream};
use serde::{Deserialize, Serialize};

use crate::{
    tweet::{concat_param, model, util},
    Error,
};

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct ListsConfig {
    lists: HashMap<String, ListMeta>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ListMeta {
    webhooks: Vec<reqwest::Url>,
}

impl ListsConfig {
    pub async fn from_cache_dir(cache_dir: impl AsRef<Path>) -> Result<Self, Error> {
        let cache_dir = cache_dir.as_ref();
        let data = tokio::fs::read(cache_dir.join("lists/config.toml")).await?;
        let config = toml::from_slice::<ListsConfig>(&data)?;
        Ok(config)
    }

    #[allow(dead_code)]
    pub async fn save_cache(&self, cache_dir: impl AsRef<Path>) -> Result<(), Error> {
        let cache_dir = cache_dir.as_ref();
        let path = cache_dir.join("lists/config.toml");
        let data = toml::to_vec(self).unwrap();
        tokio::fs::write(path, data).await?;
        Ok(())
    }
}

impl ListsConfig {
    pub fn run_once(
        &self,
        client: &reqwest::Client,
        cache_dir: impl AsRef<Path>,
        catchup: bool,
    ) -> impl Stream<Item = (String, Result<(), Error>)> {
        use futures_util::TryStreamExt;

        let cache_dir = cache_dir.as_ref();
        let lists_fut = futures_util::stream::FuturesUnordered::new();
        let webhook_client = reqwest::Client::builder().build().unwrap();

        for (id, meta) in self.lists.iter() {
            let id = id.clone();
            let list_id = id.clone();
            let cache_dir = cache_dir.to_owned();
            let client = client.clone();
            let webhooks = meta.webhooks.clone();
            let webhook_client = webhook_client.clone();
            lists_fut.push(
                async move {
                    let mut head = ListHead::from_cache_dir(list_id, &cache_dir).await?;
                    let first_time = head.head.is_none();

                    let mut tweets = head.load_and_update(&client, catchup).await?;

                    // augment
                    if !first_time && (!catchup || tweets.data().len() <= 5) {
                        let augment_data =
                            util::load_batch_augment_data(&client, tweets.as_view().map(|v| &**v))
                                .await?;
                        if let Some(augment_data) = augment_data {
                            tweets.augment(augment_data);
                        }
                    }

                    // split into views
                    let mut tweets_view = tweets.as_view().map(|v| v.iter());
                    let mut tweet_views = Vec::new();
                    let webhooks_fut = futures_util::stream::FuturesUnordered::new();

                    loop {
                        let tweet_view = tweets_view.ref_mut_map(|view| view.next());
                        let tweet_view = if let Some(&tweet) = tweet_view.view_data().as_ref() {
                            tweet_view.map(|_| tweet)
                        } else {
                            break;
                        };
                        tweet_views.push(tweet_view);
                    }

                    // execute webhooks
                    for webhook in webhooks {
                        let webhook_client = webhook_client.clone();
                        let tweet_views = &tweet_views;
                        let list_id = &head.id;

                        webhooks_fut.push(async move {
                            if catchup && tweet_views.len() > 5 {
                                send_catchup_webhook(
                                    webhook_client,
                                    webhook,
                                    list_id,
                                    tweet_views.len(),
                                )
                                .await?;
                            } else if first_time {
                                send_first_time_webhook(webhook_client, webhook, list_id).await?;
                            } else {
                                for &tweet_view in tweet_views {
                                    send_webhook(
                                        webhook_client.clone(),
                                        webhook.clone(),
                                        tweet_view,
                                    )
                                    .await?;
                                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                                }
                            }
                            Ok::<_, crate::Error>(())
                        });
                    }

                    webhooks_fut.try_collect::<()>().await?;
                    head.save_cache(&cache_dir).await?;
                    Ok(())
                }
                .map(|ret| (id, ret)),
            );
        }
        lists_fut
    }
}

async fn send_first_time_webhook(
    client: reqwest::Client,
    webhook_url: reqwest::Url,
    list_id: &str,
) -> Result<(), crate::Error> {
    let message = format!("List `{}` initialized", list_id,);
    let payload = serde_json::json!({
        "username": "tweet-broadcast",
        "content": message,
    });

    execute_webhook(&client, &webhook_url, &payload).await
}

async fn send_catchup_webhook(
    client: reqwest::Client,
    webhook_url: reqwest::Url,
    list_id: &str,
    tweet_count: usize,
) -> Result<(), crate::Error> {
    let message = format!(
        "Skipping {} tweet{} of list `{}` during list catch-up",
        tweet_count,
        if tweet_count == 1 { "" } else { "s" },
        list_id,
    );
    let payload = serde_json::json!({
        "username": "tweet-broadcast",
        "content": message,
    });

    execute_webhook(&client, &webhook_url, &payload).await
}

async fn send_webhook<'a, Data, Meta>(
    client: reqwest::Client,
    webhook_url: reqwest::Url,
    tweet: model::ResponseItemRef<'a, &'a model::Tweet, Data, Meta>,
) -> Result<(), crate::Error> {
    let original_tweet = *tweet.view_data();
    let original_author = tweet
        .includes()
        .get_user(original_tweet.author_id().unwrap())
        .unwrap();

    let tweet_data = original_tweet
        .referenced_tweets()
        .iter()
        .find(|t| t.ref_type() == model::TweetReferenceType::Retweeted);
    let tweet_data = if let Some(ref_tweet) = tweet_data {
        tweet.includes().get_tweet(ref_tweet.id()).unwrap()
    } else {
        original_tweet
    };
    let author = tweet
        .includes()
        .get_user(tweet_data.author_id().unwrap())
        .unwrap();

    let payload_media = tweet_data
        .media_keys()
        .iter()
        .map(|key| {
            let media = tweet.includes().get_media(key).unwrap();
            serde_json::json!({
                "url": media.url_orig(),
                "width": media.width(),
                "height": media.height(),
            })
        })
        .collect::<Vec<_>>();

    let mut payload_embed = vec![serde_json::json!({
        "author": {
            "name": format!("{} (@{})", author.name(), author.username()),
            "url": format!("https://twitter.com/{}", author.username()),
            "icon_url": author.profile_image_url_orig(),
        },
        "description": tweet_data.unescaped_text(),
        "timestamp": tweet_data.created_at(),
        "url": format!("https://twitter.com/{}/status/{}", author.username(), tweet_data.id()),
        "color": 1940464,
        "footer": {
            "text": "Twitter",
            "icon_url": "https://abs.twimg.com/favicons/favicon.png",
        },
        "image": payload_media.first(),
    })];
    payload_embed.extend(
        payload_media
            .into_iter()
            .map(|v| serde_json::json!({ "image": v }))
            .skip(1),
    );

    let payload = serde_json::json!({
        "username": format!("{} (@{})", original_author.name(), original_author.username()),
        "avatar_url": original_author.profile_image_url_orig(),
        "content": format!("https://twitter.com/{}/status/{}", author.username(), tweet_data.id()),
        "embeds": payload_embed,
    });

    execute_webhook(&client, &webhook_url, &payload).await
}

async fn execute_webhook(
    client: &reqwest::Client,
    url: &reqwest::Url,
    payload: &serde_json::Value,
) -> Result<(), crate::Error> {
    loop {
        log::trace!(
            "Sending payload {}",
            serde_json::to_string(payload).unwrap()
        );
        let resp = client
            .post(url.clone())
            .query(&[("wait", "true")])
            .json(payload)
            .send()
            .await?;

        if resp.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
            let duration = if let Some(reset_after) = resp.headers().get("x-ratelimit-reset-after")
            {
                let reset_after = reset_after.to_str().unwrap().parse::<f32>().unwrap();
                std::time::Duration::from_secs_f32(reset_after)
            } else if let Some(retry_after) = resp.headers().get(reqwest::header::RETRY_AFTER) {
                let retry_after = retry_after.to_str().unwrap().parse::<u64>().unwrap();
                std::time::Duration::from_secs(retry_after)
            } else {
                std::time::Duration::from_secs(5)
            };
            log::debug!("Webhook is ratelimited, retrying after {:?}", duration);
            tokio::time::sleep(duration).await;
        } else {
            resp.error_for_status()?;
            return Ok(());
        }
    }
}

#[derive(Debug, Clone)]
struct ListHead {
    id: String,
    head: Option<String>,
}

impl ListHead {
    async fn from_cache_dir(id: String, cache_dir: impl AsRef<Path>) -> Result<Self, Error> {
        let cache_dir = cache_dir.as_ref();
        let head = tokio::fs::read_to_string(cache_dir.join(format!("lists/{}", id))).await;
        let head = match head {
            Ok(head) => Some(head),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(e.into()),
        };
        Ok(Self { id, head })
    }

    async fn save_cache(&self, cache_dir: impl AsRef<Path>) -> Result<(), Error> {
        let cache_dir = cache_dir.as_ref();
        let path = cache_dir.join(format!("lists/{}", self.id));
        if let Some(head) = &self.head {
            tokio::fs::write(path, head.as_bytes()).await?;
        } else {
            if let Err(e) = tokio::fs::remove_file(path).await {
                if e.kind() != std::io::ErrorKind::NotFound {
                    return Err(e.into());
                }
            }
        }
        Ok(())
    }
}

impl ListHead {
    async fn load_and_update(
        &mut self,
        client: &reqwest::Client,
        catchup: bool,
    ) -> Result<model::ResponseItem<Vec<model::Tweet>>, Error> {
        let result = load_list_since(client, self, catchup).await?;
        if let Some(last_tweet) = result.data().last() {
            log::debug!(
                "List {}: {} new tweet(s), last tweet ID is {}",
                self.id,
                result.data().len(),
                last_tweet.id()
            );
            self.head = Some(last_tweet.id().to_owned());
        }
        Ok(result)
    }
}

fn create_endpoint_url(id: &str, max_results: u32, pagination_token: Option<&str>) -> reqwest::Url {
    let mut url =
        reqwest::Url::parse(&format!("https://api.twitter.com/2/lists/{}/tweets", id)).unwrap();
    url.query_pairs_mut()
        .append_pair("max_results", &max_results.to_string())
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
        .extend_pairs(pagination_token.map(|token| ("pagination_token", token)))
        .finish();
    url
}

async fn load_list_since(
    client: &reqwest::Client,
    list: &ListHead,
    catchup: bool,
) -> Result<model::ResponseItem<Vec<model::Tweet>>, Error> {
    let list_id = &list.id;
    let since_id = list.head.as_deref();
    let max_results = if since_id.is_some() {
        if catchup {
            100
        } else {
            10
        }
    } else {
        1
    };

    let make_request = |token: Option<String>| {
        let url = create_endpoint_url(list_id, max_results, token.as_deref());
        async {
            let base_ret = client
                .get(url)
                .send()
                .await?
                .json::<model::TwitterResponse<Vec<model::Tweet>, model::ListMeta>>()
                .await?;
            let base_ret = match base_ret {
                model::TwitterResponse::Error(err) => return Err(err.into()),
                model::TwitterResponse::Ok(ret) => ret,
            };
            Result::<_, Error>::Ok(base_ret.take_meta())
        }
    };

    let mut ret = async {
        let (mut base_ret, mut meta) = make_request(None).await?;
        let since_id = if let Some(since_id) = since_id {
            since_id
        } else {
            return Ok(base_ret);
        };

        let item_len = base_ret.data().len();
        base_ret.data_mut().retain(|item| item.id() > since_id);
        if item_len != base_ret.data().len() {
            return Ok(base_ret);
        }

        while let Some(token) = meta.next_token() {
            if base_ret.data().len() >= 500 {
                break;
            }

            let (mut ret, next_meta) = make_request(Some(token.to_owned())).await?;

            let item_len = ret.data().len();
            ret.data_mut().retain(|item| item.id() > since_id);
            if item_len != ret.data().len() {
                break;
            }
            base_ret.merge_in_place(ret, |base, v| base.extend(v));

            meta = next_meta;
        }

        Ok::<_, crate::Error>(base_ret)
    }
    .await?;

    ret.data_mut().reverse();
    Ok(ret)
}
