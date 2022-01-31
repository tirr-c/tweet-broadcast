use eyre::Result;

use tweet_fetch::TwitterClient;
use tweet_model::{
    self as model,
    cache::*,
};
use tweet_route::Router;

pub async fn run_line_loop<Cache>(
    client: &TwitterClient,
    cache: &Cache,
    router: &mut Router,
) -> Result<std::convert::Infallible>
where
    Cache: LoadCache<model::Tweet> + StoreCache<model::Tweet> + StoreCache<model::User> + StoreCache<model::Media> + StoreCache<tweet_route::CacheData>,
{
    use futures_util::StreamExt;
    let discord_client = reqwest::Client::builder().build().unwrap();

    let lines = client.make_stream();
    tokio::pin!(lines);

    loop {
        let tweet = match lines.next().await {
            Some(line_result) => line_result?,
            None => {
                eyre::bail!("stream closed");
            }
        };

        let route_result = match router.call(&tweet, cache).await {
            Ok(route_result) => route_result,
            Err(e) => {
                log::error!("Failed to route: {}, input: {:?}", e, tweet);
                let mut ev = sentry::event_from_error(&e);
                ev.extra
                    .insert(String::from("data"), format!("{:?}", tweet).into());
                sentry::capture_event(ev);
                continue;
            }
        };

        let payload = route_result.payload();
        let routes = route_result.routes();
        let cached = route_result.cached();
        if routes.is_empty() {
            log::debug!(
                "No routes: {}{}, score: {:.4}",
                payload.tweet.id(),
                if cached { " (cached)" } else { "" },
                payload.score,
            );
        } else {
            if !cached {
                let ret = async {
                    futures_util::try_join!(
                        cache.store(&tweet_route::CacheData::from(payload)),
                        route_result.cache_recursive(cache),
                    )?;
                    Ok::<_, Cache::Error>(())
                }.await;
                if let Err(e) = ret {
                    log::error!("Failed to save metadata: {}", e);
                    sentry::capture_error(&e);
                }
            }

            log::debug!(
                "Relaying tweet {id} by @{author_username}, matching rule(s): {rules:?}, score: {score:.4}",
                id = payload.tweet.id(),
                author_username = payload.author.username(),
                rules = payload.tags,
                score = payload.score,
            );

            let webhook_fut = futures_util::stream::FuturesUnordered::new();
            for route in routes {
                webhook_fut.push(async {
                    let result = tweet_discord::execute_webhook(
                        &discord_client,
                        &route.url,
                        &route.payload,
                    ).await;
                    if let Err(e) = result {
                        log::error!("Failed to send: {}", e);
                        sentry::capture_error(&e);
                    }
                });
            }
            webhook_fut.collect::<()>().await;
        }
    }
}
