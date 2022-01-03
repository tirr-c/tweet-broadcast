use tokio::signal::unix as unix_signal;

use tweet_broadcast::{tweet::TwitterClient, Router};

#[tokio::main]
async fn main() {
    env_logger::init();

    let token = std::env::var("TWITTER_APP_TOKEN").expect("TWITTER_APP_TOKEN not found or invalid");
    let cache_dir = std::env::var_os("TWITTER_CACHE")
        .or_else(|| {
            let path = std::env::current_dir().ok()?;
            Some(path.join(".tweets").into_os_string())
        })
        .and_then(|path| std::fs::create_dir_all(&path).ok().map(|_| path))
        .expect("Invalid cache directory");
    let cache_dir = std::path::PathBuf::from(cache_dir);
    std::fs::create_dir_all(cache_dir.join("meta")).unwrap();
    std::fs::create_dir_all(cache_dir.join("images")).unwrap();

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

    let stream_handle = {
        let client = client.clone();
        local_set.spawn_local(async move {
            let mut router = Router::new(128 * 1024 * 1024).expect("Failed to load router");
            loop {
                client.run_stream(&mut router).await.err();
            }
        })
    };
    let list_handle = {
        let client = client.clone();
        tokio::spawn(async move {
            loop {
                client.run_list_loop().await.err();
            }
        })
    };

    let sig_handle = tokio::spawn(async move {
        let sigterm = sigterm.recv();
        futures_util::pin_mut!(sigterm);
        let sigint = sigint.recv();
        futures_util::pin_mut!(sigint);
        let sigquit = sigquit.recv();
        futures_util::pin_mut!(sigquit);

        futures_util::future::select_all([sigterm, sigint, sigquit]).await;
        stream_handle.abort();
        list_handle.abort();
        stream_handle.await.err();
        list_handle.await.err();
    });

    local_set.await;
    sig_handle.await.ok();
}
