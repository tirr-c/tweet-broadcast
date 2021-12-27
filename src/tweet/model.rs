use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use url::Url;

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
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ResponseItem<Data> {
    data: Data,
    #[serde(default)]
    includes: ResponseIncludes,
    matching_rules: Option<Vec<MatchingRule>>,
    errors: Option<Vec<TwitterError>>,
}

impl<Data> ResponseItem<Data> {
    pub fn data(&self) -> &Data {
        &self.data
    }

    pub fn includes(&self) -> &ResponseIncludes {
        &self.includes
    }

    pub fn matching_rules(&self) -> Option<&[MatchingRule]> {
        self.matching_rules.as_deref()
    }

    pub fn errors(&self) -> &[TwitterError] {
        self.errors.as_deref().unwrap_or(&[])
    }

    pub fn augment<Other>(&mut self, other: ResponseItem<Other>) {
        self.includes.tweets.extend(other.includes.tweets);
        self.includes.users.extend(other.includes.users);
        self.includes.media.extend(other.includes.media);
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
pub struct TwitterError {
    message: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, thiserror::Error)]
#[error("{title}: {detail} {ty}")]
pub struct ResponseError {
    errors: Vec<TwitterError>,
    title: String,
    detail: String,
    #[serde(rename = "type")]
    ty: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum TwitterResponse<Data> {
    Error(ResponseError),
    Ok(ResponseItem<Data>),
}
