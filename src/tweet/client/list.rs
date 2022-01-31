use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::{
    tweet::{concat_param, model, util, TwitterClient},
    Error,
};

#[derive(Debug, Serialize, Deserialize)]
pub struct ListMeta {
    webhooks: Vec<reqwest::Url>,
}

impl ListMeta {
    pub fn webhooks(&self) -> &[reqwest::Url] {
        &self.webhooks
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct ListsConfig {
    lists: HashMap<String, ListMeta>,
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
    pub fn lists(&self) -> impl Iterator<Item = (&String, &ListMeta)> {
        self.lists.iter()
    }
}

pub async fn send_first_time_webhook(
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

pub async fn send_catchup_webhook(
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

pub async fn send_webhook(
    client: reqwest::Client,
    webhook_url: reqwest::Url,
    tweet: &model::Tweet,
    includes: &model::ResponseIncludes,
) -> Result<(), crate::Error> {
    let original_tweet = tweet;
    let original_author = includes
        .get_user(original_tweet.author_id().unwrap())
        .unwrap();

    let tweet_data = original_tweet
        .referenced_tweets()
        .iter()
        .find(|t| t.ref_type() == model::TweetReferenceType::Retweeted);
    let tweet_data = if let Some(ref_tweet) = tweet_data {
        includes.get_tweet(ref_tweet.id()).unwrap()
    } else {
        original_tweet
    };
    let author = includes.get_user(tweet_data.author_id().unwrap()).unwrap();

    let payload_media = tweet_data
        .media_keys()
        .iter()
        .map(|key| {
            let media = includes.get_media(key).unwrap();
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
pub struct ListHead {
    id: String,
    head: Option<String>,
}

impl ListHead {
    pub async fn from_cache_dir(id: String, cache_dir: impl AsRef<Path>) -> Result<Self, Error> {
        let cache_dir = cache_dir.as_ref();
        let head = tokio::fs::read_to_string(cache_dir.join(format!("lists/{}", id))).await;
        let head = match head {
            Ok(head) => Some(head),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(e.into()),
        };
        Ok(Self { id, head })
    }

    pub async fn save_cache(&self, cache_dir: impl AsRef<Path>) -> Result<(), Error> {
        let cache_dir = cache_dir.as_ref();
        let path = cache_dir.join(format!("lists/{}", self.id));
        if let Some(head) = &self.head {
            tokio::fs::write(path, head.as_bytes()).await?;
        } else if let Err(e) = tokio::fs::remove_file(path).await {
            if e.kind() != std::io::ErrorKind::NotFound {
                return Err(e.into());
            }
        }
        Ok(())
    }
}

impl ListHead {
    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn head(&self) -> Option<&str> {
        self.head.as_deref()
    }

    pub async fn load_and_update(
        &mut self,
        client: &TwitterClient,
        catchup: bool,
    ) -> Result<model::ResponseItem<Vec<model::Tweet>>, Error> {
        let mut res = load_list_since(client, self, catchup).await?;
        if let Some(last_tweet) = res.data.last() {
            let updating = self.head.is_some();

            log::debug!(
                "List {}: {} new tweet(s), last tweet ID is {}",
                self.id,
                res.data.len(),
                last_tweet.id()
            );
            self.head = Some(last_tweet.id().to_owned());

            // augment
            if updating && (!catchup || res.data.len() <= 5) {
                let augment_data =
                    util::load_batch_augment_data(client, &res.data, &res.includes).await?;
                if let Some(model::ResponseItem { includes, .. }) = augment_data {
                    res.includes.augment(includes);
                }
            }
        }
        Ok(res)
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
    client: &TwitterClient,
    list: &ListHead,
    catchup: bool,
) -> Result<model::ResponseItem<Vec<model::Tweet>>, Error> {
    let list_id = &list.id;
    let since_id = list.head.as_deref();
    let max_results = if since_id.is_some() {
        if catchup {
            100
        } else {
            5
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
            Result::<_, Error>::Ok(base_ret)
        }
    };

    let since_id = if let Some(since_id) = since_id {
        since_id
    } else {
        let (mut ret, _) = make_request(None).await?.take_meta();
        ret.data.reverse();
        return Ok(ret);
    };

    let mut ret = model::ResponseItem::<Vec<model::Tweet>>::default();
    let mut next_token = Some(None::<String>);

    while let Some(token) = next_token {
        let model::ResponseItem {
            data,
            includes,
            meta: next_meta,
        } = make_request(token).await?;

        let data = data
            .into_iter()
            .take_while(|item| item.id() > since_id)
            .collect::<Vec<_>>();
        let data_len = data.len();

        ret.data.extend(data);
        ret.includes.augment(includes);

        if data_len as i32 != next_meta.result_count() {
            break;
        }
        next_token = next_meta.next_token().map(|s| Some(s.to_owned()));
    }

    ret.data.reverse();
    Ok(ret)
}
