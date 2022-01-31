use futures_util::future::BoxFuture;

use tweet_model::{
    self as model,
    cache::*,
};

#[derive(Debug, Clone)]
pub struct FsCache {
    dir: std::path::PathBuf,
    client: Option<reqwest::Client>,
}

impl FsCache {
    pub fn new(path: impl Into<std::path::PathBuf>) -> Self {
        let enable_images = std::env::var("TWITTER_SAVE_IMAGES");
        let enable_images = enable_images.unwrap_or_else(|_| String::new()) == "1";
        let client = if enable_images {
            log::info!("Media downloading enabled");
            Some(reqwest::Client::builder().build().unwrap())
        } else {
            None
        };
        Self {
            dir: path.into(),
            client,
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

impl FsCache {
    pub async fn store_image(
        &self,
        media: &tweet_model::Media,
    ) -> Result<(), FsError> {
        use tokio::io::AsyncWriteExt;

        let client = if let Some(client) = &self.client {
            client
        } else {
            return Ok(());
        };

        self.ensure_dir("images").await?;
        let url = if let Some(url) = media.url_orig() {
            url
        } else {
            return Ok(());
        };
        let filename = AsRef::<std::path::Path>::as_ref(url.path());
        let ext = filename.extension();
        let mut filename = std::path::PathBuf::from(media.key());
        filename.set_extension(ext.unwrap_or(&std::ffi::OsString::new()));
        let path = self.dir.join("images").join(filename);

        let file = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .await;
        let mut file = match file {
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                return Ok(());
            },
            file => file?,
        };

        let path = path.to_string_lossy();
        log::debug!("Downloading {} into {}", url, path);
        let mut res = client
            .get(url)
            .send()
            .await?
            .error_for_status()?;
        while let Some(bytes) = res.chunk().await? {
            file.write(&bytes).await?;
        }
        file.flush().await?;
        log::debug!("Download done: {}", path);
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
impl_cache!(tweet_route::CacheData, "stream");

impl LoadCache<model::Media> for FsCache {
    fn load(&self, key: &str) -> BoxFuture<'_, Result<model::Media, Self::Error>> {
        let path = self.subpath(format!("media/{}.json", key));
        Box::pin(async {
            let v = tokio::fs::read(path).await?;
            let data = serde_json::from_slice::<model::Media>(&v)?;
            Ok(data)
        })
    }

    fn has(&self, key: &str) -> BoxFuture<'_, Result<bool, Self::Error>> {
        let path = self.subpath(format!("media/{}.json", key));
        Box::pin(async {
            match tokio::fs::metadata(path).await {
                Ok(_) => Ok(true),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
                Err(e) => Err(e.into()),
            }
        })
    }
}

impl StoreCache<model::Media> for FsCache {
    fn store(&self, item: &model::Media) -> BoxFuture<'_, Result<String, Self::Error>> {
        if self.client.is_some() {
            let cache = self.clone();
            let item = item.clone();
            tokio::spawn(async move {
                if let Err(e) = cache.store_image(&item).await {
                    log::error!("Media download failed: {}", e);
                    sentry::capture_error(&e);
                }
            });
        }

        let key = item.key().to_owned();
        let path = self.subpath(format!("media/{}.json", key));
        let v = serde_json::to_vec(item).unwrap();
        Box::pin(async {
            self.ensure_dir("media").await?;
            tokio::fs::write(path, v).await?;

            Ok(key)
        })
    }
}

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
