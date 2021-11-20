use crate::Error;

#[derive(Debug)]
pub struct Router<'s> {
    global_scope: v8::HandleScope<'s, ()>,
}

#[derive(Debug)]
pub struct RouterFn<'ctx, 's> {
    script_scope: v8::ContextScope<'ctx, v8::HandleScope<'s, v8::Context>>,
    route_fn: v8::Local<'ctx, v8::Function>,
}

impl<'s> Router<'s> {
    pub fn new(isolate: &'s mut v8::OwnedIsolate) -> Self {
        let global_scope = v8::HandleScope::new(isolate);
        Self { global_scope }
    }

    pub fn load(&mut self) -> Result<RouterFn<'_, 's>, Error> {
        let ctx = v8::Context::new(&mut self.global_scope);
        let mut script_scope = v8::ContextScope::new(&mut self.global_scope, ctx);

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

        Ok(RouterFn {
            script_scope,
            route_fn,
        })
    }
}

#[derive(Debug, serde::Deserialize)]
pub struct RouteResultItem {
    pub url: url::Url,
    pub payload: serde_json::Value,
}

impl<'ctx, 's> RouterFn<'ctx, 's> {
    pub fn call(
        &mut self,
        data: &crate::tweet::ResponseItem<crate::tweet::Tweet>,
    ) -> Result<Vec<RouteResultItem>, Error>
    {
        let tweet = if let Some(rt_id) = data.data().get_retweet_source() {
            Some(data.includes().get_tweet(rt_id).unwrap())
        } else {
            None
        };
        let original_data = if tweet.is_some() {
            let tweet = data.data();
            let author_id = tweet.author_id().unwrap();
            let author = data.includes().get_user(author_id).unwrap();
            Some((tweet, author))
        } else {
            None
        };
        let tweet = tweet.unwrap_or(data.data());
        let author_id = tweet.author_id().unwrap();
        let author = data.includes().get_user(author_id).unwrap();

        let tweet_metrics = tweet.metrics().unwrap();
        let user_metrics = author.metrics().unwrap();
        let score = crate::compute_score(tweet_metrics, user_metrics, tweet.created_at().unwrap());

        let scope = &mut v8::TryCatch::new(&mut self.script_scope);
        let data_obj = v8::Object::new(scope);

        {
            let tweet = serde_v8::to_v8(scope, tweet)?;
            let key = v8::String::new(scope, "tweet").unwrap();
            data_obj.set(scope, key.into(), tweet);

            let author = serde_v8::to_v8(scope, author)?;
            let key = v8::String::new(scope, "author").unwrap();
            data_obj.set(scope, key.into(), author);
        }

        if let Some((tweet, author)) = original_data {
            let tweet = serde_v8::to_v8(scope, tweet)?;
            let key = v8::String::new(scope, "originalTweet").unwrap();
            data_obj.set(scope, key.into(), tweet);

            let author = serde_v8::to_v8(scope, author)?;
            let key = v8::String::new(scope, "originalAuthor").unwrap();
            data_obj.set(scope, key.into(), author);
        }

        let media = tweet
            .media_keys()
            .iter()
            .filter_map(|k| data.includes().get_media(k))
            .collect::<Vec<_>>();
        let media = serde_v8::to_v8(scope, media)?;
        let key = v8::String::new(scope, "media").unwrap();
        data_obj.set(scope, key.into(), media);

        let score = v8::Number::new(scope, score);
        let key = v8::String::new(scope, "score").unwrap();
        data_obj.set(scope, key.into(), score.into());

        let tags = data
            .matching_rules()
            .unwrap_or(&[])
            .iter()
            .map(|x| x.tag())
            .collect::<Vec<_>>();
        let tags = serde_v8::to_v8(scope, tags)?;
        let key = v8::String::new(scope, "tags").unwrap();
        data_obj.set(scope, key.into(), tags);

        let recv = v8::undefined(scope);
        let ret = self.route_fn.call(
            scope,
            recv.into(),
            &[data_obj.into()],
        );
        if scope.has_caught() {
            let msg = scope.message().unwrap();
            let msg = msg.get(scope).to_rust_string_lossy(scope);
            return Err(Error::JsException(msg));
        }
        let ret = if let Some(ret) = ret {
            ret
        } else {
            todo!()
        };

        Ok(serde_v8::from_v8(scope, ret)?)
    }
}
