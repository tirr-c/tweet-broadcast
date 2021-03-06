use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use url::Url;

pub mod cache;
use cache::CacheItem;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Tweet {
    id: String,
    text: String,
    created_at: Option<DateTime<Utc>>,
    author_id: Option<String>,
    #[serde(default)]
    entities: Entities,
    #[serde(default)]
    attachments: Attachments,
    public_metrics: Option<TweetPublicMetrics>,
    possibly_sensitive: Option<bool>,
    #[serde(default)]
    referenced_tweets: Vec<ReferencedTweet>,
}

impl CacheItem for Tweet {
    fn key(&self) -> &str {
        self.id()
    }
}

impl Tweet {
    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn raw_text(&self) -> &str {
        &self.text
    }

    pub fn unescaped_text(&self) -> String {
        self.text
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&amp;", "&")
    }

    pub fn created_at(&self) -> Option<DateTime<Utc>> {
        self.created_at
    }

    pub fn author_id(&self) -> Option<&str> {
        self.author_id.as_deref()
    }

    pub fn entities(&self) -> &Entities {
        &self.entities
    }

    pub fn media_keys(&self) -> &[String] {
        &self.attachments.media_keys
    }

    pub fn metrics(&self) -> Option<&TweetPublicMetrics> {
        self.public_metrics.as_ref()
    }

    pub fn possibly_sensitive(&self) -> bool {
        self.possibly_sensitive.unwrap_or(false)
    }

    pub fn referenced_tweets(&self) -> &[ReferencedTweet] {
        &self.referenced_tweets
    }

    pub fn get_retweet_source(&self) -> Option<&str> {
        self.referenced_tweets
            .iter()
            .find(|t| t.ty == TweetReferenceType::Retweeted)
            .map(|t| &*t.id)
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct Attachments {
    media_keys: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct Entities {
    hashtags: Vec<Hashtag>,
    urls: Vec<UrlEntity>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Hashtag {
    start: usize,
    end: usize,
    tag: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct UrlEntity {
    start: usize,
    end: usize,
    url: Url,
    display_url: String,
    expanded_url: Url,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TweetPublicMetrics {
    pub reply_count: u64,
    pub retweet_count: u64,
    pub quote_count: u64,
    pub like_count: u64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ReferencedTweet {
    #[serde(rename = "type")]
    ty: TweetReferenceType,
    id: String,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TweetReferenceType {
    Retweeted,
    Quoted,
    RepliedTo,
}

impl ReferencedTweet {
    pub fn ref_type(&self) -> TweetReferenceType {
        self.ty
    }

    pub fn id(&self) -> &str {
        &self.id
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct User {
    id: String,
    name: String,
    username: String,
    profile_image_url: Option<Url>,
    public_metrics: Option<UserPublicMetrics>,
}

impl CacheItem for User {
    fn key(&self) -> &str {
        self.id()
    }
}

impl User {
    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn username(&self) -> &str {
        &self.username
    }

    pub fn profile_image_url(&self) -> Option<&Url> {
        self.profile_image_url.as_ref()
    }

    pub fn profile_image_url_orig(&self) -> Option<Url> {
        self.profile_image_url.as_ref().map(|url| {
            let mut url = url.clone();
            let filename = url.path_segments().unwrap().last().unwrap();
            let new_filename = filename.replace("_normal.", ".");
            url.path_segments_mut().unwrap().pop().push(&new_filename);
            url
        })
    }

    pub fn metrics(&self) -> Option<&UserPublicMetrics> {
        self.public_metrics.as_ref()
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct UserPublicMetrics {
    pub followers_count: u64,
    pub following_count: u64,
    pub tweet_count: u64,
    pub listed_count: u64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Media {
    media_key: String,
    width: u64,
    height: u64,
    #[serde(rename = "type")]
    ty: MediaType,
    url: Option<Url>,
    preview_image_url: Option<Url>,
}

impl CacheItem for Media {
    fn key(&self) -> &str {
        self.key()
    }
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaType {
    Photo,
    Video,
    AnimatedGif,
}

impl Media {
    pub fn key(&self) -> &str {
        &self.media_key
    }

    pub fn width(&self) -> u64 {
        self.width
    }

    pub fn height(&self) -> u64 {
        self.height
    }

    pub fn media_type(&self) -> MediaType {
        self.ty
    }

    pub fn url(&self) -> Option<&Url> {
        self.url.as_ref().or(self.preview_image_url.as_ref())
    }

    pub fn url_orig(&self) -> Option<Url> {
        if let Some(url) = &self.url {
            let mut url = url.clone();
            url.set_query(Some("name=orig"));
            Some(url)
        } else {
            self.preview_image_url.clone()
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct ResponseItem<Data, Meta = Option<()>> {
    pub data: Data,
    #[serde(default)]
    pub includes: ResponseIncludes,
    #[serde(flatten)]
    pub meta: Meta,
}

impl<Data: cache::CacheItem, Meta> ResponseItem<Data, Meta> {
    pub async fn cache_recursive<Cache>(&self, cache: &Cache) -> Result<(), Cache::Error> where
        Cache: cache::StoreCache<Data> + cache::StoreCache<Tweet> + cache::StoreCache<User> + cache::StoreCache<Media>,
    {
        use futures_util::TryStreamExt;

        let futures = futures_util::stream::FuturesUnordered::new();
        futures.push(cache.store(&self.data));
        for tweet in &self.includes.tweets {
            futures.push(cache.store(tweet));
        }
        for user in &self.includes.users {
            futures.push(cache.store(user));
        }
        for media in &self.includes.media {
            futures.push(cache.store(media));
        }
        futures.try_collect::<Vec<_>>().await?;
        Ok(())
    }
}

impl<Data, Meta> ResponseItem<Data, Meta> {
    pub fn get_media(&self, media_key: &str) -> Option<&Media> {
        self.includes.get_media(media_key)
    }

    pub fn get_tweet(&self, id: &str) -> Option<&Tweet> {
        self.includes.get_tweet(id)
    }

    pub fn get_user(&self, id: &str) -> Option<&User> {
        self.includes.get_user(id)
    }

    pub fn take_augment<OtherData, OtherMeta>(
        &mut self,
        other: &mut ResponseItem<OtherData, OtherMeta>,
    ) {
        self.includes.take_augment(&mut other.includes);
    }

    pub fn take_meta(self) -> (ResponseItem<Data>, Meta) {
        let Self {
            data,
            includes,
            meta,
        } = self;
        let ret = ResponseItem {
            data,
            includes,
            meta: None,
        };
        (ret, meta)
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct ResponseIncludes {
    tweets: Vec<Tweet>,
    users: Vec<User>,
    media: Vec<Media>,
}

impl ResponseIncludes {
    pub fn get_media(&self, media_key: &str) -> Option<&Media> {
        self.media.iter().find(|m| m.media_key == media_key)
    }

    pub fn get_tweet(&self, id: &str) -> Option<&Tweet> {
        self.tweets.iter().find(|t| t.id == id)
    }

    pub fn get_user(&self, id: &str) -> Option<&User> {
        self.users.iter().find(|u| u.id == id)
    }

    pub fn augment(&mut self, other: Self) {
        self.tweets.extend(other.tweets);
        self.users.extend(other.users);
        self.media.extend(other.media);
    }

    pub fn take_augment(&mut self, other: &mut Self) {
        let other = std::mem::take(other);
        self.augment(other);
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct StreamMeta {
    matching_rules: Vec<MatchingRule>,
}

impl StreamMeta {
    pub fn matching_rules(&self) -> &[MatchingRule] {
        &self.matching_rules
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct MatchingRule {
    id: String,
    tag: String,
}

impl MatchingRule {
    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn tag(&self) -> &str {
        &self.tag
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ListMeta {
    meta: ListMetaInner,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct ListMetaInner {
    result_count: i32,
    previous_token: Option<String>,
    next_token: Option<String>,
}

impl ListMeta {
    pub fn result_count(&self) -> i32 {
        self.meta.result_count
    }

    pub fn previous_token(&self) -> Option<&str> {
        self.meta.previous_token.as_deref()
    }

    pub fn next_token(&self) -> Option<&str> {
        self.meta.next_token.as_deref()
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SearchMeta {
    meta: SearchMetaInner,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct SearchMetaInner {
    result_count: i32,
    newest_id: Option<String>,
    oldest_id: Option<String>,
    next_token: Option<String>,
}

impl SearchMeta {
    pub fn result_count(&self) -> i32 {
        self.meta.result_count
    }

    pub fn newest_id(&self) -> Option<&str> {
        self.meta.newest_id.as_deref()
    }

    pub fn oldest_id(&self) -> Option<&str> {
        self.meta.oldest_id.as_deref()
    }

    pub fn next_token(&self) -> Option<&str> {
        self.meta.next_token.as_deref()
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TwitterError {
    title: String,
    detail: String,
    #[serde(rename = "type")]
    ty: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, thiserror::Error)]
#[error("{}", .errors.iter().map(|x| &*x.detail).collect::<Vec<_>>().join(" "))]
pub struct ResponseError {
    errors: Vec<TwitterError>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum TwitterResponse<Data, Meta = Option<()>> {
    Error(ResponseError),
    Ok(ResponseItem<Data, Meta>),
}

impl<Data, Meta> TwitterResponse<Data, Meta> {
    pub fn into_result(self) -> Result<ResponseItem<Data, Meta>, ResponseError> {
        match self {
            Self::Ok(v) => Ok(v),
            Self::Error(e) => Err(e),
        }
    }
}
