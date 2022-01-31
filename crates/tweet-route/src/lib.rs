use tweet_model::{
    self as model,
    cache::*,
};

mod error;
mod score;

pub use error::Error;

fn load_script(
    isolate: &mut v8::OwnedIsolate,
    script: &str,
) -> Result<v8::Global<v8::Function>, Error> {
    let mut global_scope = v8::HandleScope::new(isolate);
    let ctx = v8::Context::new(&mut global_scope);
    let mut script_scope = v8::ContextScope::new(&mut global_scope, ctx);

    let mut try_catch = v8::TryCatch::new(&mut script_scope);

    let result = v8::String::new(&mut try_catch, script);
    let s = if let Some(s) = result {
        v8::Script::compile(&mut try_catch, s, None)
    } else {
        None
    };
    let _result = if let Some(s) = s {
        s.run(&mut try_catch)
    } else {
        None
    };
    if try_catch.has_caught() {
        let msg = try_catch.message().unwrap();
        let msg = msg.get(&mut try_catch).to_rust_string_lossy(&mut try_catch);
        return Err(Error::JsException(msg));
    }
    drop(try_catch);

    let ctx = script_scope.get_current_context();
    let global = ctx.global(&mut script_scope);
    let fn_name = v8::String::new(&mut script_scope, "route").unwrap();
    let route_fn = global.get(&mut script_scope, fn_name.into());
    let route_fn: v8::Local<'_, v8::Function> = if let Some(route_fn) = route_fn {
        if let Ok(route_fn) = route_fn.try_into() {
            route_fn
        } else {
            return Err(Error::FunctionNotFound(String::from("route")));
        }
    } else {
        return Err(Error::FunctionNotFound(String::from("route")));
    };

    let route_fn = v8::Global::new(&mut script_scope, route_fn);
    Ok(route_fn)
}

#[derive(Debug)]
pub struct Router {
    isolate: v8::OwnedIsolate,
    route_fn: v8::Global<v8::Function>,
}

impl Router {
    pub fn new(heap_limit: usize, script: &str) -> Result<Self, Error> {
        let mut isolate = v8::Isolate::new(v8::CreateParams::default().heap_limits(0, heap_limit));
        let route_fn = load_script(&mut isolate, script)?;
        Ok(Self { isolate, route_fn })
    }

    pub fn reload(&mut self, script: &str) -> Result<(), Error> {
        let route_fn = load_script(&mut self.isolate, script)?;
        self.route_fn = route_fn;
        Ok(())
    }

    pub async fn call<'data, Cache: LoadCache<model::Tweet>>(
        &mut self,
        res: &'data model::ResponseItem<model::Tweet, model::StreamMeta>,
        cache: &Cache,
    ) -> Result<RouteResult<'data>, Error> {
        let model::ResponseItem {
            data,
            includes,
            meta,
        } = res;
        let tweet = data
            .get_retweet_source()
            .map(|rt_id| includes.get_tweet(rt_id).unwrap());
        let original_data = if tweet.is_some() {
            let author_id = data.author_id().unwrap();
            let author = includes.get_user(author_id).unwrap();
            Some((data, author))
        } else {
            None
        };
        let tweet = tweet.unwrap_or(data);
        let author_id = tweet.author_id().unwrap();
        let author = includes.get_user(author_id).unwrap();

        let has_cache = cache.has(&tweet.id().to_owned()).await.unwrap_or(false);

        let tweet_metrics = tweet.metrics().unwrap();
        let user_metrics = author.metrics().unwrap();
        let score = score::compute_score(tweet_metrics, user_metrics, tweet.created_at().unwrap());

        let media = tweet
            .media_keys()
            .iter()
            .filter_map(|k| includes.get_media(k))
            .collect::<Vec<_>>();
        let tags = meta
            .matching_rules()
            .iter()
            .map(|x| x.tag())
            .collect::<Vec<_>>();

        let data = RoutePayload {
            tweet,
            author,
            original_tweet: original_data.as_ref().map(|&(tweet, _)| tweet),
            original_author: original_data.as_ref().map(|&(_, author)| author),
            media,
            score,
            tags,
            cached: has_cache,
        };

        let mut global_scope = v8::HandleScope::new(&mut self.isolate);
        let ctx = v8::Context::new(&mut global_scope);
        let mut script_scope = v8::ContextScope::new(&mut global_scope, ctx);

        let mut scope_val = v8::TryCatch::new(&mut script_scope);
        let scope = &mut scope_val;
        let data_obj = serde_v8::to_v8(scope, &data)?;

        let recv = v8::undefined(scope);
        let route_fn = self.route_fn.open(scope);
        let ret = route_fn.call(scope, recv.into(), &[data_obj]);
        if scope.has_caught() {
            let msg = scope.message().unwrap();
            let msg = msg.get(scope).to_rust_string_lossy(scope);
            return Err(Error::JsException(msg));
        }
        let ret = ret.unwrap();

        let routes = serde_v8::from_v8(scope, ret)?;
        drop(scope_val);
        script_scope.perform_microtask_checkpoint();

        Ok(RouteResult {
            payload: data,
            routes,
        })
    }
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RoutePayload<'a> {
    pub tweet: &'a model::Tweet,
    pub author: &'a model::User,
    pub original_tweet: Option<&'a model::Tweet>,
    pub original_author: Option<&'a model::User>,
    pub media: Vec<&'a model::Media>,
    pub score: f64,
    pub tags: Vec<&'a str>,
    pub cached: bool,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CacheData {
    tweet_id: String,
    author_id: String,
    target_tweet_id: Option<String>,
    target_author_id: Option<String>,
    media_keys: Vec<String>,
    score: f64,
    tags: Vec<String>,
}

impl CacheItem for CacheData {
    fn key(&self) -> &str {
        &self.tweet_id
    }
}

impl From<&'_ RoutePayload<'_>> for CacheData {
    fn from(payload: &'_ RoutePayload<'_>) -> Self {
        let target_tweet_id = payload.original_tweet.and(Some(payload.tweet)).map(|x| x.id().to_owned());
        let target_author_id = payload.original_author.and(Some(payload.author)).map(|x| x.id().to_owned());
        let tweet_id = payload.original_tweet.unwrap_or(payload.tweet).id().to_owned();
        let author_id = payload.original_author.unwrap_or(payload.author).id().to_owned();
        Self {
            tweet_id,
            author_id,
            target_tweet_id,
            target_author_id,
            media_keys: payload.media.iter().map(|&x| x.key().to_owned()).collect(),
            score: payload.score,
            tags: payload.tags.iter().map(|&x| x.to_owned()).collect(),
        }
    }
}

#[derive(Debug, serde::Deserialize)]
pub struct RouteResultItem {
    pub url: url::Url,
    pub payload: serde_json::Value,
}

#[derive(Debug)]
pub struct RouteResult<'a> {
    payload: RoutePayload<'a>,
    routes: Vec<RouteResultItem>,
}

impl<'a> RouteResult<'a> {
    pub fn payload(&self) -> &RoutePayload<'a> {
        &self.payload
    }

    pub async fn cache_recursive<Cache>(&self, cache: &Cache) -> Result<(), Cache::Error> where
        Cache: StoreCache<model::Tweet> + StoreCache<model::User> + StoreCache<model::Media> + StoreCache<CacheData>,
    {
        use futures_util::TryStreamExt;

        let payload = &self.payload;

        let futures = futures_util::stream::FuturesUnordered::new();
        futures.push(cache.store(&CacheData::from(payload)));
        futures.push(cache.store(payload.tweet));
        futures.push(cache.store(payload.author));
        if let Some(tweet) = payload.original_tweet {
            futures.push(cache.store(tweet));
        }
        if let Some(author) = payload.original_author {
            futures.push(cache.store(author));
        }
        for &media in &payload.media {
            futures.push(cache.store(media));
        }
        futures.try_collect::<Vec<_>>().await?;
        Ok(())
    }

    pub fn cached(&self) -> bool {
        self.payload.cached
    }

    pub fn routes(&self) -> &[RouteResultItem] {
        &self.routes
    }
}
