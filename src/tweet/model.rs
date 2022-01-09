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
        } else if let Some(url) = &self.preview_image_url {
            Some(url.clone())
        } else {
            None
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ResponseItem<Data, Meta = Option<()>> {
    data: Data,
    #[serde(default)]
    includes: ResponseIncludes,
    matching_rules: Option<Vec<MatchingRule>>,
    errors: Option<Vec<TwitterError>>,
    meta: Meta,
}

impl<Data, Meta> ResponseItem<Data, Meta> {
    pub fn data(&self) -> &Data {
        &self.data
    }

    pub fn data_mut(&mut self) -> &mut Data {
        &mut self.data
    }

    pub fn meta(&self) -> &Meta {
        &self.meta
    }

    pub fn take_meta(self) -> (ResponseItem<Data>, Meta) {
        let Self {
            data,
            includes,
            matching_rules,
            errors,
            meta,
        } = self;
        let new_item = ResponseItem {
            data,
            includes,
            matching_rules,
            errors,
            meta: None,
        };
        (new_item, meta)
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
}

impl<Data, Meta> ResponseItem<Data, Meta> {
    pub fn as_view(&self) -> ResponseItemRef<'_, &Data, Data, Meta> {
        ResponseItemRef {
            view: &self.data,
            base: self,
        }
    }

    pub fn map<NewData>(self, f: impl FnOnce(Data) -> NewData) -> ResponseItem<NewData, Meta> {
        let Self {
            data,
            includes,
            matching_rules,
            errors,
            meta,
        } = self;

        ResponseItem {
            data: f(data),
            includes,
            matching_rules,
            errors,
            meta,
        }
    }

    pub fn merge<OtherData, OtherMeta, NewData>(
        self,
        other: ResponseItem<OtherData, OtherMeta>,
        merge_f: impl FnOnce(Data, OtherData) -> NewData,
    ) -> ResponseItem<NewData, Meta> {
        let Self {
            data,
            mut includes,
            matching_rules,
            errors,
            meta,
        } = self;
        includes.augment(other.includes);
        let matching_rules = match (matching_rules, other.matching_rules) {
            (None, None) => None,
            (Some(v), None) | (None, Some(v)) => Some(v),
            (Some(mut v1), Some(v2)) => {
                v1.extend(v2);
                Some(v1)
            }
        };
        let errors = match (errors, other.errors) {
            (None, None) => None,
            (Some(v), None) | (None, Some(v)) => Some(v),
            (Some(mut v1), Some(v2)) => {
                v1.extend(v2);
                Some(v1)
            }
        };

        ResponseItem {
            data: merge_f(data, other.data),
            includes,
            matching_rules,
            errors,
            meta,
        }
    }

    pub fn merge_in_place<OtherData, OtherMeta>(
        &mut self,
        other: ResponseItem<OtherData, OtherMeta>,
        merge_f: impl FnOnce(&mut Data, OtherData),
    ) -> &mut Self {
        self.includes.augment(other.includes);
        match (&mut self.matching_rules, other.matching_rules) {
            (matching_rules @ None, other_matching_rules) => {
                *matching_rules = other_matching_rules;
            }
            (Some(rules), Some(other_rules)) => {
                rules.extend(other_rules);
            }
            _ => {}
        }
        match (&mut self.errors, other.errors) {
            (errors @ None, other_errors) => {
                *errors = other_errors;
            }
            (Some(e), Some(other_e)) => {
                e.extend(other_e);
            }
            _ => {}
        }
        merge_f(&mut self.data, other.data);
        self
    }

    pub fn augment<OtherData, OtherMeta>(
        &mut self,
        other: ResponseItem<OtherData, OtherMeta>,
    ) -> &mut Self {
        self.merge_in_place(other, |_, _| {})
    }
}

#[derive(Debug)]
pub struct ResponseItemRef<'a, ViewData: 'a, Data, Meta> {
    view: ViewData,
    base: &'a ResponseItem<Data, Meta>,
}

impl<'a, ViewData: Clone + 'a, Data, Meta> Clone for ResponseItemRef<'a, ViewData, Data, Meta> {
    fn clone(&self) -> Self {
        Self {
            view: self.view.clone(),
            base: self.base,
        }
    }
}

impl<'a, ViewData: Copy + 'a, Data, Meta> Copy for ResponseItemRef<'a, ViewData, Data, Meta> {}

impl<'a, ViewData: 'a, Data, Meta> ResponseItemRef<'a, ViewData, Data, Meta> {
    pub fn view_data(&self) -> &ViewData {
        &self.view
    }

    pub fn view_data_mut(&mut self) -> &mut ViewData {
        &mut self.view
    }

    pub fn map<NewViewData: 'a>(
        self,
        f: impl FnOnce(ViewData) -> NewViewData,
    ) -> ResponseItemRef<'a, NewViewData, Data, Meta> {
        ResponseItemRef {
            view: f(self.view),
            base: self.base,
        }
    }

    pub fn ref_map<NewViewData: 'a>(
        &self,
        f: impl FnOnce(&ViewData) -> NewViewData,
    ) -> ResponseItemRef<'a, NewViewData, Data, Meta> {
        ResponseItemRef {
            view: f(&self.view),
            base: self.base,
        }
    }

    pub fn ref_mut_map<NewViewData: 'a>(
        &mut self,
        f: impl FnOnce(&mut ViewData) -> NewViewData,
    ) -> ResponseItemRef<'a, NewViewData, Data, Meta> {
        ResponseItemRef {
            view: f(&mut self.view),
            base: self.base,
        }
    }
}

impl<'a, ViewData: 'a, Data, Meta> std::ops::Deref for ResponseItemRef<'a, ViewData, Data, Meta> {
    type Target = ResponseItem<Data, Meta>;

    fn deref(&self) -> &Self::Target {
        self.base
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
    result_count: i32,
    previous_token: Option<String>,
    next_token: Option<String>,
}

impl ListMeta {
    pub fn result_count(&self) -> i32 {
        self.result_count
    }

    pub fn previous_token(&self) -> Option<&str> {
        self.previous_token.as_deref()
    }

    pub fn next_token(&self) -> Option<&str> {
        self.next_token.as_deref()
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
pub enum TwitterResponse<Data, Meta = Option<()>> {
    Error(ResponseError),
    Ok(ResponseItem<Data, Meta>),
}
