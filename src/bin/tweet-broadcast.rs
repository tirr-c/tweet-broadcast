use std::collections::HashSet;

use clap::Parser;
use tokio::signal::unix as unix_signal;

use tweet_broadcast::{tweet::TwitterClient, Router};

#[derive(Debug, PartialEq, Eq, Hash, strum::EnumString, strum::Display)]
#[strum(serialize_all = "snake_case")]
enum Engine {
    FilteredStream,
    List,
}

#[derive(Debug, Parser)]
#[clap(version)]
struct Args {
    #[clap(short, long, env = "TWITTER_CACHE", default_value = "./.tweets")]
    cache: std::path::PathBuf,
    #[clap(short, long = "engine")]
    engines: Vec<Engine>,
}

#[tokio::main]
async fn main() {
    let Args {
        cache: cache_dir,
        mut engines,
    } = Args::parse();

    if engines.len() == 0 {
        engines.push(Engine::FilteredStream);
        engines.push(Engine::List);
    }
    let engines = engines.into_iter().collect::<HashSet<_>>();

    std::fs::create_dir_all(&cache_dir).expect("Invalid cache directory");
    std::fs::create_dir_all(cache_dir.join("meta")).unwrap();
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

    let client = TwitterClient::new(token, cache_dir);

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
        Some(local_set.spawn_local(async move {
            let mut router = Router::new(128 * 1024 * 1024).expect("Failed to load router");
            loop {
                client.run_stream(&mut router).await.err();
            }
        }))
    } else {
        None
    };
    let list_handle = if engines.contains(&Engine::List) {
        log::info!("Enabling engine {}", Engine::List);
        let client = client.clone();
        Some(tokio::spawn(async move {
            let e = client.run_list_loop().await.unwrap_err();
            log::error!("List error: {}", e);
        }))
    } else {
        None
    };

    let sig_handle = tokio::spawn(async move {
        let sigterm = sigterm.recv();
        futures_util::pin_mut!(sigterm);
        let sigint = sigint.recv();
        futures_util::pin_mut!(sigint);
        let sigquit = sigquit.recv();
        futures_util::pin_mut!(sigquit);

        futures_util::future::select_all([sigterm, sigint, sigquit]).await;
        if let Some(stream_handle) = &stream_handle {
            stream_handle.abort();
        }
        if let Some(list_handle) = &list_handle {
            list_handle.abort();
        }
        if let Some(stream_handle) = stream_handle {
            stream_handle.await.err();
        }
        if let Some(list_handle) = list_handle {
            list_handle.await.err();
        }
    });

    local_set.await;
    sig_handle.await.ok();
}
