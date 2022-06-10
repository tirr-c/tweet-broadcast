use chrono::{Duration, Utc};
use futures_util::future::BoxFuture;

use tweet_model::{
    self as model,
    cache::*,
};

#[derive(Debug, Clone)]
pub struct FsCache {
    dir: std::path::PathBuf,
    remote: Option<RemoteConfig>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct RemoteConfig {
    endpoint: reqwest::Url,
    signing_key: String,
    #[serde(default)]
    no_save_images: bool,
    #[serde(skip, default)]
    client: reqwest::Client,
}

impl std::fmt::Debug for RemoteConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteConfig").field("endpoint", &self.endpoint).finish_non_exhaustive()
    }
}

impl RemoteConfig {
    fn sign(&self, message: &[u8]) -> (ring::hmac::Tag, i64) {
        let now = Utc::now();
        let expires_at = now + Duration::seconds(30);
        let expires_at_ts = expires_at.timestamp_millis();

        let mut sign_message = expires_at_ts.to_string().as_bytes().to_vec();
        sign_message.extend_from_slice(message);

        let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, self.signing_key.as_bytes());
        let tag = ring::hmac::sign(&key, &sign_message);
        (tag, expires_at_ts)
    }

    fn download_tweet_media(&self, id: &str) -> reqwest::Request {
        let body = serde_json::json!({ "id": id });
        let body = serde_json::to_vec(&body).unwrap();
        let (tag, expires_at_ts) = self.sign(&body);

        let mut request = reqwest::Request::new(reqwest::Method::POST, self.endpoint.clone());
        let headers = request.headers_mut();
        headers.insert(
            reqwest::header::HeaderName::from_static("x-expires"),
            expires_at_ts.to_string().parse().unwrap(),
        );
        headers.insert(
            reqwest::header::HeaderName::from_static("x-signature"),
            base64::encode(tag.as_ref()).parse().unwrap(),
        );
        *request.body_mut() = Some(body.into());

        request
    }
}

impl FsCache {
    pub async fn new(path: impl Into<std::path::PathBuf>, no_save_images: bool) -> Self {
        let dir = path.into();
        let remote = {
            let config_path = dir.join("remote.toml");
            match tokio::fs::read(config_path).await {
                Ok(buf) => {
                    match toml::from_slice::<RemoteConfig>(&buf) {
                        Ok(mut remote) => {
                            remote.no_save_images |= no_save_images;
                            Some(remote)
                        },
                        Err(e) => {
                            log::error!("Failed to read remote.toml: {}", e);
                            None
                        },
                    }
                },
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    None
                },
                Err(e) => {
                    log::error!("Failed to read remote.toml: {}", e);
                    None
                },
            }
        };
        Self {
            dir,
            remote,
        }
    }

    fn subpath(&self, path: impl AsRef<std::path::Path>) -> std::path::PathBuf {
        self.dir.join(path)
    }

    async fn ensure_dir(&self, dir: impl AsRef<std::path::Path>) -> Result<(), std::io::Error> {
        let path = self.dir.join(dir);
        tokio::fs::create_dir_all(path).await?;
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum FsError {
    #[error("HTTP error: {0}")]
    Http(#[from] #[source] reqwest::Error),
    #[error("I/O error: {0}")]
    Io(#[from] #[source] std::io::Error),
    #[error("Parse error: {0}")]
    Parse(#[from] #[source] serde_json::Error),
}

impl Cache for FsCache {
    type Error = FsError;
}

macro_rules! impl_cache {
    ($it:ty, $base:literal, load) => {
        impl LoadCache<$it> for FsCache {
            fn load(&self, key: &str) -> BoxFuture<'_, Result<$it, Self::Error>> {
                let path = self.subpath(format!(concat!($base, "/{}.json"), key));
                Box::pin(async {
                    let v = tokio::fs::read(path).await?;
                    let data = serde_json::from_slice::<$it>(&v)?;
                    Ok(data)
                })
            }

            fn has(&self, key: &str) -> BoxFuture<'_, Result<bool, Self::Error>> {
                let path = self.subpath(format!(concat!($base, "/{}.json"), key));
                Box::pin(async {
                    match tokio::fs::metadata(path).await {
                        Ok(_) => Ok(true),
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
                        Err(e) => Err(e.into()),
                    }
                })
            }
        }
    };
    ($it:ty, $base:literal, store) => {
        impl StoreCache<$it> for FsCache {
            fn store(&self, item: &$it) -> BoxFuture<'_, Result<String, Self::Error>> {
                let key = item.key().to_owned();
                let path = self.subpath(format!(concat!($base, "/{}.json"), key));
                let v = serde_json::to_vec(item).unwrap();
                Box::pin(async {
                    self.ensure_dir($base).await?;
                    tokio::fs::write(path, v).await?;
                    Ok(key)
                })
            }
        }
    };
    ($it:ty, $base:literal) => {
        impl_cache!($it, $base, load);
        impl_cache!($it, $base, store);
    };
}

impl_cache!(model::Tweet, "tweets", load);
impl StoreCache<model::Tweet> for FsCache {
    fn store(&self, item: &model::Tweet) -> BoxFuture<'_, Result<String, Self::Error>> {
        if let Some(remote) = &self.remote {
            let id = item.id().to_owned();
            let remote_media_save = remote.download_tweet_media(&id);
            let client = remote.client.clone();
            tokio::spawn(async move {
                let ret: Result<_, reqwest::Error> = async {
                    let res = client.execute(remote_media_save).await?;
                    let status = res.status();
                    let body = res.text().await?;
                    Ok((status, body))
                }.await;
                match ret {
                    Ok((status, body)) => {
                        if status != reqwest::StatusCode::OK {
                            log::error!(
                                "Remote download returned error for tweet ID {}: {}, {}",
                                id,
                                status,
                                body,
                            );
                        }
                        log::debug!("Remote download done for tweet ID {}", id);
                    },
                    Err(err) => {
                        log::error!("Remote download request failed: {}", err);
                    },
                }
            });
        }

        let key = item.key().to_owned();
        let path = self.subpath(format!("tweets/{}.json", key));
        let v = serde_json::to_vec(item).unwrap();
        Box::pin(async {
            self.ensure_dir("tweets").await?;
            tokio::fs::write(path, v).await?;
            Ok(key)
        })
    }
}

impl_cache!(model::User, "users");
impl_cache!(model::Media, "media");
impl_cache!(tweet_route::CacheData, "stream");

impl LoadCache<tweet_fetch::ListHead> for FsCache {
    fn load(&self, key: &str) -> BoxFuture<'_, Result<tweet_fetch::ListHead, Self::Error>> {
        let key = key.to_owned();
        let path = self.subpath(format!("lists/{}", key));
        Box::pin(async {
            let head = tokio::fs::read_to_string(path).await;
            let head = match head {
                Ok(head) => Some(head),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
                Err(e) => return Err(e.into()),
            };
            Ok(tweet_fetch::ListHead::new(key, head))
        })
    }

    fn has(&self, key: &str) -> BoxFuture<'_, Result<bool, Self::Error>> {
        let path = self.subpath(format!("lists/{}", key));
        Box::pin(async {
            match tokio::fs::metadata(path).await {
                Ok(_) => Ok(true),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
                Err(e) => Err(e.into()),
            }
        })
    }
}

impl StoreCache<tweet_fetch::ListHead> for FsCache {
    fn store(&self, item: &tweet_fetch::ListHead) -> BoxFuture<'_, Result<String, Self::Error>> {
        let key = item.key().to_owned();
        let head = item.head().map(|s| s.to_owned());
        let path = self.subpath(format!("lists/{}", key));
        Box::pin(async {
            self.ensure_dir("lists").await?;
            if let Some(head) = head {
                tokio::fs::write(path, head.as_bytes()).await?;
            } else if let Err(e) = tokio::fs::remove_file(path).await {
                if e.kind() != std::io::ErrorKind::NotFound {
                    return Err(e.into());
                }
            }
            Ok(key)
        })
    }
}

impl LoadCache<tweet_fetch::UserTimelineHead> for FsCache {
    fn load(&self, key: &str) -> BoxFuture<'_, Result<tweet_fetch::UserTimelineHead, Self::Error>> {
        let key = key.to_owned();
        let path = self.subpath(format!("users/{}", key));
        Box::pin(async {
            let head = tokio::fs::read_to_string(path).await;
            let head = match head {
                Ok(head) => Some(head),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
                Err(e) => return Err(e.into()),
            };
            Ok(tweet_fetch::UserTimelineHead::new(key, head))
        })
    }

    fn has(&self, key: &str) -> BoxFuture<'_, Result<bool, Self::Error>> {
        let path = self.subpath(format!("users/{}", key));
        Box::pin(async {
            match tokio::fs::metadata(path).await {
                Ok(_) => Ok(true),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
                Err(e) => Err(e.into()),
            }
        })
    }
}

impl StoreCache<tweet_fetch::UserTimelineHead> for FsCache {
    fn store(&self, item: &tweet_fetch::UserTimelineHead) -> BoxFuture<'_, Result<String, Self::Error>> {
        let key = item.key().to_owned();
        let head = item.head().map(|s| s.to_owned());
        let path = self.subpath(format!("users/{}", key));
        Box::pin(async {
            self.ensure_dir("users").await?;
            if let Some(head) = head {
                tokio::fs::write(path, head.as_bytes()).await?;
            } else if let Err(e) = tokio::fs::remove_file(path).await {
                if e.kind() != std::io::ErrorKind::NotFound {
                    return Err(e.into());
                }
            }
            Ok(key)
        })
    }
}
