use futures_util::future::BoxFuture;

use tweet_model::{
    self as model,
    cache::*,
};

#[derive(Debug, Clone)]
pub struct FsCache(std::path::PathBuf);

impl FsCache {
    pub fn new(path: impl Into<std::path::PathBuf>) -> Self {
        Self(path.into())
    }

    fn subpath(&self, path: impl AsRef<std::path::Path>) -> std::path::PathBuf {
        self.0.join(path)
    }

    async fn ensure_dir(&self, dir: impl AsRef<std::path::Path>) -> Result<(), std::io::Error> {
        let path = self.0.join(dir);
        tokio::fs::create_dir_all(path).await?;
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum FsError {
    #[error("I/O error: {0}")]
    Io(#[from] #[source] std::io::Error),
    #[error("Parse error: {0}")]
    Parse(#[from] #[source] serde_json::Error),
}

impl Cache for FsCache {
    type Error = FsError;
}

macro_rules! impl_cache {
    ($it:ty, $base:literal) => {
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
}

impl_cache!(model::Tweet, "tweets");
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
