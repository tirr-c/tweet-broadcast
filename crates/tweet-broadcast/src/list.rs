use std::collections::HashMap;
use std::path::Path;

use eyre::Result;
use serde::{Deserialize, Serialize};

use tweet_fetch::{ListHead, TwitterClient};
use tweet_model::{
    self as model,
    cache::*,
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
    pub async fn from_config(config: impl AsRef<Path>) -> Result<Self> {
        let data = tokio::fs::read(config).await?;
        let config = toml::from_slice::<ListsConfig>(&data)?;
        Ok(config)
    }
}

impl ListsConfig {
    pub fn lists(&self) -> impl Iterator<Item = (&String, &ListMeta)> {
        self.lists.iter()
    }
}

async fn send_first_time_webhook(
    client: &reqwest::Client,
    webhook_url: &reqwest::Url,
    list_id: &str,
) -> Result<()> {
    let message = format!("List `{}` initialized", list_id,);
    let payload = serde_json::json!({
        "username": "tweet-broadcast",
        "content": message,
    });

    tweet_discord::execute_webhook(client, webhook_url, &payload).await?;
    Ok(())
}

async fn send_catchup_webhook(
    client: &reqwest::Client,
    webhook_url: &reqwest::Url,
    list_id: &str,
    tweet_count: usize,
) -> Result<()> {
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

    tweet_discord::execute_webhook(client, webhook_url, &payload).await?;
    Ok(())
}

pub async fn run_list_once<Cache: LoadCache<ListHead> + StoreCache<ListHead>>(
    client: &TwitterClient,
    config: &ListsConfig,
    catchup: bool,
    cache: &Cache,
) {
    use futures_util::{StreamExt, TryStreamExt};

    let webhook_client = reqwest::Client::builder().build().unwrap();

    let stream = futures_util::stream::FuturesUnordered::new();
    for (id, meta) in config.lists() {
        let webhook_client = &webhook_client;
        let fut = async move {
            let ret = async {
                let mut head = cache.load(id).await?;
                let first_time = head.head().is_none();
                let tweets = head.load_and_update(client, catchup).await?;
                cache.store(&head).await?;
                Ok::<_, eyre::Error>((tweets, first_time))
            }
            .await;
            let (tweets, first_time) = match ret {
                Ok(tweets) => tweets,
                Err(e) => {
                    log::error!("List fetch for {} failed: {}", id, e);
                    let mut event = sentry::event_from_error(AsRef::<dyn std::error::Error + 'static>::as_ref(&e));
                    event.tags.insert(String::from("id"), id.into());
                    sentry::capture_event(event);
                    return;
                }
            };
            let model::ResponseItem {
                data: tweets,
                includes,
                ..
            } = &tweets;

            let webhooks_fut = futures_util::stream::FuturesUnordered::new();
            for webhook in meta.webhooks() {
                let webhook_client = &webhook_client;
                webhooks_fut.push(async move {
                    if catchup && tweets.len() > 5 {
                        send_catchup_webhook(
                            webhook_client,
                            webhook,
                            id,
                            tweets.len(),
                        )
                        .await?;
                    } else if first_time {
                        send_first_time_webhook(webhook_client, webhook, id).await?;
                    } else {
                        for tweet in tweets {
                            tweet_discord::send_webhook(
                                webhook_client,
                                webhook,
                                tweet,
                                includes,
                            )
                            .await?;
                            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                        }
                    }
                    Ok::<_, eyre::Error>(())
                });
            }

            let ret = webhooks_fut.try_collect::<()>().await;
            if let Err(e) = ret {
                log::error!("Failed to send webhook for {}: {}", id, e);
                let mut event = sentry::event_from_error(AsRef::<dyn std::error::Error + 'static>::as_ref(&e));
                event.tags.insert(String::from("id"), id.into());
                sentry::capture_event(event);
                return;
            }

            log::debug!("List fetch for {} successful", id);
        };
        stream.push(fut);
    }
    stream.collect::<()>().await;
}
