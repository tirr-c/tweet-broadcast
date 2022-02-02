use std::collections::HashSet;

use clap::Parser;
use tokio::signal::unix as unix_signal;

use tweet_fetch::TwitterClient;
use tweet_route::Router;

mod cache;
mod image;
mod list;
mod search;
mod stream;

#[derive(Debug, PartialEq, Eq, Hash, strum::EnumString, strum::Display)]
#[strum(serialize_all = "snake_case")]
enum Engine {
    FilteredStream,
    Search,
    List,
}

#[derive(Debug, Parser)]
#[clap(version)]
struct Args {
    #[clap(short, long, env = "TWITTER_CACHE", default_value = "./.tweets")]
    cache: std::path::PathBuf,
    #[clap(long, env = "TWITTER_SAVE_IMAGES")]
    save_images: bool,
    #[clap(short, long = "engine")]
    engines: Vec<Engine>,
}

#[tokio::main]
async fn main() {
    let Args {
        cache: cache_dir,
        save_images,
        mut engines,
    } = Args::parse();

    if engines.is_empty() {
        engines.push(Engine::FilteredStream);
        engines.push(Engine::List);
    }
    let engines = engines.into_iter().collect::<HashSet<_>>();

    std::fs::create_dir_all(&cache_dir).expect("Invalid cache directory");
    std::fs::create_dir_all(cache_dir.join("images")).unwrap();

    let token = std::env::var("TWITTER_APP_TOKEN").expect("TWITTER_APP_TOKEN not found or invalid");

    env_logger::init();
    let _sentry = sentry::init((
        std::env::var_os("SENTRY_DSN"),
        sentry::ClientOptions {
            release: sentry::release_name!(),
            ..Default::default()
        },
    ));

    let cache = cache::FsCache::new(&cache_dir, save_images);
    let client = TwitterClient::new(token);

    let platform = v8::Platform::new(0, false).make_shared();
    v8::V8::initialize_platform(platform);
    v8::V8::initialize();

    let mut sigterm = unix_signal::signal(unix_signal::SignalKind::terminate())
        .expect("Failed to listen SIGTERM");
    let mut sigint =
        unix_signal::signal(unix_signal::SignalKind::interrupt()).expect("Failed to listen SIGINT");
    let mut sigquit =
        unix_signal::signal(unix_signal::SignalKind::quit()).expect("Failed to listen SIGQUIT");

    let local_set = tokio::task::LocalSet::new();

    let stream_handle = if engines.contains(&Engine::FilteredStream) {
        log::info!("Enabling engine {}", Engine::FilteredStream);
        let client = client.clone();
        let cache = cache.clone();
        Some(local_set.spawn_local(async move {
            let script = tokio::fs::read_to_string("route.js").await.expect("Failed to load router");
            let mut router = Router::new(128 * 1024 * 1024, &script).expect("Failed to load router");
            loop {
                if let Err(e) = stream::run_line_loop(&client, &cache, &mut router).await {
                    log::error!("Stream error: {}", e);
                }
            }
        }))
    } else {
        None
    };
    let search_handle = if engines.contains(&Engine::Search) {
        log::info!("Enabling engine {}", Engine::Search);
        let client = client.clone();
        let cache = cache.clone();

        let config_path = cache_dir.join("searches/config.toml");
        let config = search::SearchConfig::from_config(config_path).await.expect("Failed to load config");

        Some(tokio::spawn(async move {
            let mut tracker = search::TrendingContext::new();

            log::info!("Initializing search terms");
            let mut heads = std::collections::HashMap::new();
            for term in config.terms() {
                let mut head = tweet_fetch::SearchHead::new(term.term.to_owned(), None);
                match head.fetch(&client).await {
                    Ok(tweet_model::ResponseItem {
                        data: tweets,
                        includes,
                        ..
                    }) => {
                        if term.trending {
                            for tweet in &tweets {
                                tracker.insert(tweet, &includes, term);
                            }
                        }
                    },
                    Err(e) => {
                        log::error!("Search init failed: {}", e);
                        sentry::capture_error(&e);
                        continue;
                    },
                };
                heads.insert(term.id.to_owned(), head);
            }

            let mut timer = tokio::time::interval(std::time::Duration::from_secs(30));
            let mut tick_count = 0;

            log::info!("Started search loop");
            loop {
                timer.tick().await;
                tick_count += 1;

                if tick_count % 6 == 0 {
                    tick_count = 0;
                    log::trace!("Running search fetch");

                    for term in config.terms() {
                        let id = term.id.to_owned();
                        let trending = term.trending;
                        let head = heads.get_mut(&id).unwrap();

                        match head.fetch(&client).await {
                            Ok(tweet_model::ResponseItem {
                                data: tweets,
                                includes,
                                ..
                            }) => {
                                if trending {
                                    for tweet in &tweets {
                                        tracker.insert(tweet, &includes, term);
                                    }
                                }
                            },
                            Err(e) => {
                                log::error!("Search failed: {}", e);
                                sentry::capture_error(&e);
                                continue;
                            },
                        };
                    }
                }

                log::trace!("Running tracker update");
                if let Err(e) = tracker.run_once(&client, &cache).await {
                    log::error!("Tracking failed: {}", e);
                    sentry::capture_error(AsRef::<dyn std::error::Error + 'static>::as_ref(&e));
                }
            }
        }))
    } else {
        None
    };
    let list_handle = if engines.contains(&Engine::List) {
        log::info!("Enabling engine {}", Engine::List);
        let client = client.clone();

        let config_path = cache_dir.join("lists/config.toml");
        let config = list::ListsConfig::from_config(config_path).await.expect("Failed to load config");
        Some(tokio::spawn(async move {
            let mut timer = tokio::time::interval(std::time::Duration::from_secs(60));
            log::info!("Started list fetch loop");

            let mut catchup = true;
            loop {
                timer.tick().await;
                log::debug!(
                    "Running list fetch{}",
                    if catchup { " (catch-up)" } else { "" }
                );

                list::run_list_once(&client, &config, catchup, &cache).await;
                catchup = false;
            }
        }))
    } else {
        None
    };

    let sig_handle = tokio::spawn(async move {
        let sigterm = sigterm.recv();
        tokio::pin!(sigterm);
        let sigint = sigint.recv();
        tokio::pin!(sigint);
        let sigquit = sigquit.recv();
        tokio::pin!(sigquit);

        futures_util::future::select_all([sigterm, sigint, sigquit]).await;
        if let Some(stream_handle) = &stream_handle {
            stream_handle.abort();
        }
        if let Some(search_handle) = &search_handle {
            search_handle.abort();
        }
        if let Some(list_handle) = &list_handle {
            list_handle.abort();
        }
        if let Some(stream_handle) = stream_handle {
            stream_handle.await.err();
        }
        if let Some(search_handle) = search_handle {
            search_handle.await.err();
        }
        if let Some(list_handle) = list_handle {
            list_handle.await.err();
        }
    });

    local_set.await;
    sig_handle.await.ok();
}
