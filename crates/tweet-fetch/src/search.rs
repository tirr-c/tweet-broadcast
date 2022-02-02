use tweet_model as model;
use crate::{
    util,
    concat_param,
    Error,
    TwitterClient,
};

fn create_endpoint_url(
    term: &str,
    max_results: u32,
    since_id: Option<&str>,
    next_token: Option<&str>,
) -> reqwest::Url {
    let mut url =
        reqwest::Url::parse("https://api.twitter.com/2/tweets/search/recent").unwrap();
    url.query_pairs_mut()
        .append_pair("query", term)
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
        .extend_pairs(since_id.map(|id| ("since_id", id)))
        .extend_pairs(next_token.map(|token| ("next_token", token)))
        .finish();
    url
}

#[derive(Debug, Clone)]
pub struct SearchHead {
    term: String,
    head: Option<String>,
}

impl SearchHead {
    pub fn new(term: String, head: Option<String>) -> Self {
        Self { term, head }
    }
}

impl SearchHead {
    pub fn term(&self) -> &str {
        &self.term
    }

    pub fn head(&self) -> Option<&str> {
        self.head.as_deref()
    }

    pub fn is_unbound(&self) -> bool {
        self.head.is_none()
    }

    pub fn pager(&mut self) -> SearchPager<'_> {
        SearchPager {
            head: self,
            newest_id: None,
            next_token: Some(None),
        }
    }

    pub async fn fetch(
        &mut self,
        client: &TwitterClient,
    ) -> Result<model::ResponseItem<Vec<model::Tweet>>, Error> {
        self.pager().load_all(client).await
    }
}

#[derive(Debug)]
pub struct SearchPager<'head> {
    head: &'head mut SearchHead,
    newest_id: Option<String>,
    next_token: Option<Option<String>>,
}

impl SearchPager<'_> {
    pub fn is_unbound(&self) -> bool {
        self.head.is_unbound()
    }

    pub fn apply_head(self) {
        if let Some(id) = self.newest_id {
            self.head.head = Some(id);
        }
    }
}

impl SearchPager<'_> {
    pub async fn next(
        &mut self,
        client: &TwitterClient,
        max_results: u32,
    ) -> Result<Option<model::ResponseItem<Vec<model::Tweet>>>, Error> {
        let next_token = if let Some(token) = &self.next_token {
            token.as_deref()
        } else {
            return Ok(None);
        };
        let url = create_endpoint_url(
            self.head.term(),
            max_results,
            self.head.head(),
            next_token,
        );

        let mut backoff = crate::backoff::Backoff::new();
        let res = backoff.run_fn(move || {
            let url = url.clone();
            async {
                match client.get(url).send().await {
                    Ok(v) => Ok(v),
                    Err(_) => Err(crate::backoff::BackoffType::Network),
                }
            }
        }).await;
        let res = res
            .json::<model::TwitterResponse<Option<Vec<model::Tweet>>, model::SearchMeta>>()
            .await?
            .into_result()?;
        let (ret, meta) = res.take_meta();
        let mut ret = model::ResponseItem {
            data: if let Some(v) = ret.data { v } else { return Ok(None); },
            includes: ret.includes,
            meta: None,
        };

        // augment
        let augment_data =
            util::load_batch_augment_data(client, &ret.data, &ret.includes).await?;
        if let Some(model::ResponseItem { includes, .. }) = augment_data {
            ret.includes.augment(includes);
        }

        if self.newest_id.is_none() {
            self.newest_id = meta.newest_id().map(|s| s.to_owned());
        }
        self.next_token = meta.next_token().map(|s| Some(s.to_owned()));
        Ok(Some(ret))
    }

    pub async fn load_all(
        mut self,
        client: &TwitterClient,
    ) -> Result<model::ResponseItem<Vec<model::Tweet>>, Error> {
        if self.is_unbound() {
            let ret = self.next(client, 20).await?;
            self.apply_head();
            return Ok(if let Some(ret) = ret {
                ret
            } else {
                Default::default()
            });
        }

        let mut ret = model::ResponseItem::<Vec<model::Tweet>>::default();
        while let Some(tweets) = self.next(client, 100).await? {
            let model::ResponseItem {
                data,
                includes,
                ..
            } = tweets;
            ret.data.extend(data);
            ret.includes.augment(includes);
        }
        self.apply_head();
        Ok(ret)
    }
}
