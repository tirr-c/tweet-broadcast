use std::future::Future;
use std::pin::Pin;

type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

pub trait CacheItem {
    fn key(&self) -> &str;
}

pub trait Cache {
    type Error: std::error::Error + Send + Sync + 'static;
}

pub trait LoadCache<Item: CacheItem>: Cache {
    fn load(&self, key: &str) -> BoxFuture<'_, Result<Item, Self::Error>>;
    fn has(&self, key: &str) -> BoxFuture<'_, Result<bool, Self::Error>>;
}

pub trait StoreCache<Item: CacheItem>: Cache {
    fn store(&self, item: &Item) -> BoxFuture<'_, Result<String, Self::Error>>;
}
