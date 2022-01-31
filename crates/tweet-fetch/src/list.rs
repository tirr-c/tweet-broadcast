use std::path::Path;

use tweet_model as model;
use crate::{
    util,
    concat_param,
    Error,
    TwitterClient,
};

#[derive(Debug, Clone)]
pub struct ListHead {
    id: String,
    head: Option<String>,
}

impl tweet_model::cache::CacheItem for ListHead {
    fn key(&self) -> &str {
        &self.id
    }
}

impl ListHead {
    pub fn new(id: String, head: Option<String>) -> Self {
        Self { id, head }
    }

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
