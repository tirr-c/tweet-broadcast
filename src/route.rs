use crate::{tweet::model, Error};

fn load_script(isolate: &mut v8::OwnedIsolate) -> Result<v8::Global<v8::Function>, Error> {
    let mut global_scope = v8::HandleScope::new(isolate);
    let ctx = v8::Context::new(&mut global_scope);
    let mut script_scope = v8::ContextScope::new(&mut global_scope, ctx);

    let mut try_catch = v8::TryCatch::new(&mut script_scope);
    let script = std::fs::read_to_string("./route.js")?;

    let result = v8::String::new(&mut try_catch, &script);
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
    pub fn new(heap_limit: usize) -> Result<Self, Error> {
        let mut isolate = v8::Isolate::new(v8::CreateParams::default().heap_limits(0, heap_limit));
        let route_fn = load_script(&mut isolate)?;
        Ok(Self { isolate, route_fn })
    }

    pub fn reload(&mut self) -> Result<(), Error> {
        let route_fn = load_script(&mut self.isolate)?;
        self.route_fn = route_fn;
        Ok(())
    }

    pub async fn call<'data>(
        &mut self,
        res: &'data model::ResponseItem<model::Tweet, model::StreamMeta>,
        cache_dir: impl AsRef<std::path::Path>,
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

        let meta_path = cache_dir.as_ref().join(format!("meta/{}.json", tweet.id()));
        let has_cache = tokio::fs::metadata(meta_path).await.is_ok();

        let tweet_metrics = tweet.metrics().unwrap();
        let user_metrics = author.metrics().unwrap();
        let score = crate::compute_score(tweet_metrics, user_metrics, tweet.created_at().unwrap());

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
struct RoutePayload<'a> {
    tweet: &'a model::Tweet,
    author: &'a model::User,
    original_tweet: Option<&'a model::Tweet>,
    original_author: Option<&'a model::User>,
    media: Vec<&'a model::Media>,
    score: f64,
    tags: Vec<&'a str>,
    cached: bool,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct CacheData {
    tweet: model::Tweet,
    author: model::User,
    original_tweet: Option<model::Tweet>,
    original_author: Option<model::User>,
    media: Vec<model::Media>,
    score: f64,
    tags: Vec<String>,
}

impl From<RoutePayload<'_>> for CacheData {
    fn from(payload: RoutePayload<'_>) -> Self {
        Self {
            tweet: payload.tweet.clone(),
            author: payload.author.clone(),
            original_tweet: payload.original_tweet.cloned(),
            original_author: payload.original_author.cloned(),
            media: payload.media.into_iter().cloned().collect(),
            score: payload.score,
            tags: payload.tags.into_iter().map(ToOwned::to_owned).collect(),
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

impl RouteResult<'_> {
    pub async fn save_cache(&self, cache_dir: impl AsRef<std::path::Path>) -> Result<(), Error> {
        let id = self.payload.tweet.id();
        let meta_path = cache_dir.as_ref().join(format!("meta/{}.json", id));
        let data = serde_json::to_vec_pretty(&self.payload).unwrap();
        tokio::fs::write(meta_path, data).await?;
        Ok(())
    }

    pub fn cached(&self) -> bool {
        self.payload.cached
    }

    pub fn routes(&self) -> &[RouteResultItem] {
        &self.routes
    }
}
