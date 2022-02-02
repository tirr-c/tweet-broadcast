use std::collections::{BinaryHeap, HashMap};
use std::path::Path;

use chrono::{DateTime, Utc};
use eyre::Result;
use serde::{Deserialize, Serialize};

use tweet_fetch::TwitterClient;
use tweet_model::{
    self as model,
    cache::*,
};

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct SearchConfig {
    terms: HashMap<String, SearchTermMetaInner>,
}

#[derive(Debug, Serialize, Deserialize)]
struct SearchTermMetaInner {
    term: String,
    #[serde(default)]
    trending: bool,
    webhooks: Vec<reqwest::Url>,
}

#[derive(Debug)]
pub struct SearchTermMeta<'a> {
    pub id: &'a str,
    pub term: &'a str,
    pub trending: bool,
    pub webhooks: &'a [reqwest::Url],
}

impl SearchConfig {
    pub async fn from_config(config: impl AsRef<Path>) -> Result<Self> {
        let data = tokio::fs::read(config).await?;
        let config = toml::from_slice::<SearchConfig>(&data)?;
        Ok(config)
    }

    pub fn terms(&self) -> impl Iterator<Item = SearchTermMeta<'_>> {
        self.terms
            .iter()
            .map(|(id, meta)| SearchTermMeta {
                id,
                term: &meta.term,
                trending: meta.trending,
                webhooks: &meta.webhooks,
            })
    }
}

#[derive(Debug)]
struct TrendingEntry<'a> {
    check_due_at: DateTime<Utc>,
    tweet_id: String,
    created_at: DateTime<Utc>,
    webhooks: &'a [reqwest::Url],
    previous_score: f64,
    penalty: u32,
}

impl PartialEq for TrendingEntry<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.check_due_at == other.check_due_at
    }
}
impl Eq for TrendingEntry<'_> {}

impl PartialOrd for TrendingEntry<'_> {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.check_due_at.cmp(&other.check_due_at))
    }
}
impl Ord for TrendingEntry<'_> {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.check_due_at.cmp(&other.check_due_at)
    }
}

impl TrendingEntry<'_> {
    fn elapsed(&self) -> chrono::Duration {
        Utc::now() - self.created_at
    }

    fn needs_check(&self) -> bool {
        self.check_due_at <= Utc::now()
    }
}

#[derive(Debug, Default)]
pub struct TrendingContext<'conf> {
    tracking: BinaryHeap<std::cmp::Reverse<TrendingEntry<'conf>>>,
}

impl<'conf> TrendingContext<'conf> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(
        &mut self,
        tweet: &model::Tweet,
        includes: &model::ResponseIncludes,
        notify_webhooks: &'conf [reqwest::Url],
    ) {
        self.insert_inner(tweet, includes, notify_webhooks, None, None)
    }

    fn insert_inner(
        &mut self,
        tweet: &model::Tweet,
        includes: &model::ResponseIncludes,
        notify_webhooks: &'conf [reqwest::Url],
        previous_entry: Option<&TrendingEntry<'conf>>,
        score: Option<f64>,
    ) {
        if tweet.get_retweet_source().is_some() {
            // ignore retweets
            return;
        }
        let created_at = if let Some(at) = tweet.created_at() {
            at
        } else {
            return;
        };
        let author_metrics = tweet
            .author_id()
            .and_then(|id| includes.get_user(id))
            .and_then(|u| u.metrics());
        let author_metrics = if let Some(m) = author_metrics {
            m
        } else {
            return;
        };
        let tweet_id = tweet.id().to_owned();

        let mut delay_min = 60.0f64 / 15.0f64.powf(1.0f64.min(author_metrics.followers_count as f64 / 1000.0));
        let (base, penalty) = if let Some(score) = score {
            delay_min *= 0.98f64.powf(score);
            let penalty = if let Some(entry) = previous_entry {
                if score - entry.previous_score < 1.0 {
                    if entry.penalty == 0 {
                        1
                    } else {
                        entry.penalty.saturating_mul(2)
                    }
                } else {
                    entry.penalty.saturating_sub(2)
                }
            } else {
                0
            };
            (Utc::now(), penalty)
        } else {
            (created_at, 0)
        };
        delay_min *= (1 + penalty) as f64;
        let delay_duration = std::time::Duration::from_secs_f64(delay_min * 60.0f64);
        let check_due_at = base + chrono::Duration::from_std(delay_duration).unwrap();
        if let Some(score) = score {
            log::debug!("Tweet {}: check in {:.4} minutes (score: {:.4})", tweet_id, delay_min, score);
        } else {
            log::debug!("Tweet {}: check at {}", tweet_id, check_due_at);
        }

        let entry = TrendingEntry {
            check_due_at,
            tweet_id,
            created_at,
            webhooks: notify_webhooks,
            previous_score: score.unwrap_or(0.0),
            penalty,
        };
        self.tracking.push(std::cmp::Reverse(entry));
    }

    pub async fn run_once<Cache>(
        &mut self,
        client: &TwitterClient,
        cache: &Cache
    ) -> Result<()>
    where
        Cache: LoadCache<model::Tweet> + StoreCache<model::Tweet> + StoreCache<model::User> + StoreCache<model::Media>,
    {
        use futures_util::{TryFutureExt, TryStreamExt};

        let now = Utc::now();
        let mut needs_check = Vec::new();
        while let Some(entry) = self.tracking.peek_mut() {
            if entry.0.check_due_at > now {
                break;
            }
            needs_check.push(std::collections::binary_heap::PeekMut::pop(entry).0);
        }
        let ids = needs_check
            .iter()
            .map(|e| &*e.tweet_id)
            .collect::<Vec<_>>();
        let entry_map = needs_check
            .iter()
            .map(|e| (e.tweet_id.clone(), e))
            .collect::<HashMap<_, _>>();
        let model::ResponseItem {
            data: tweets,
            includes,
            ..
        } = client.retrieve(&ids).await?;

        let futures = futures_util::stream::FuturesUnordered::new();
        let cache_futures = futures_util::stream::FuturesUnordered::new();
        for tweet in &tweets {
            if cache.has(tweet.id()).await? {
                log::debug!("Tweet {} is cached, skipping", tweet.id());
                continue;
            }
            let tweet_metrics = tweet.metrics();
            let author = tweet
                .author_id()
                .and_then(|id| includes.get_user(id));
            let author_metrics = author.and_then(|a| a.metrics());
            let created_at = tweet.created_at().unwrap();
            let score = tweet_route::compute_score(
                tweet_metrics.unwrap(),
                author_metrics.unwrap(),
                created_at,
            );
            let &entry = entry_map.get(tweet.id()).unwrap();
            let webhooks = entry.webhooks;

            if score > 30.0 {
                log::debug!(
                    "Relaying tweet {id} by @{author_username}, score: {score:.4}",
                    id = tweet.id(),
                    author_username = author.unwrap().username(),
                    score = score,
                );
                for webhook in webhooks {
                    futures.push(tweet_discord::send_webhook(
                        client,
                        webhook,
                        tweet,
                        &includes,
                    ));
                }

                cache_futures.push(cache.store(tweet));
                cache_futures.push(cache.store(author.unwrap()));
                for media_key in tweet.media_keys() {
                    cache_futures.push(cache.store(includes.get_media(media_key).unwrap()));
                }
                continue;
            }

            let elapsed = now - created_at;
            if score < 2.0 && elapsed >= chrono::Duration::hours(3) {
                log::debug!("Tweet {}: untracking (score: {:.4})", tweet.id(), score);
                continue;
            }
            if score >= 2.0 && elapsed >= chrono::Duration::days(2) {
                log::debug!("Tweet {}: untracking (score: {:.4})", tweet.id(), score);
                continue;
            }

            // insert again
            self.insert_inner(tweet, &includes, webhooks, Some(entry), Some(score));
        }
        futures_util::try_join!(
            cache_futures.try_collect::<Vec<_>>().map_err(eyre::Report::new),
            futures.try_collect::<()>().map_err(eyre::Report::new),
        )?;
        Ok(())
    }
}
