use std::path::{Path, PathBuf};
use std::ops::Deref;

use reqwest::{
    header::{self, HeaderMap, HeaderValue},
    Client,
};
use sentry::Breadcrumb;

use crate::{Backoff, BackoffType, Router};

mod stream;
use stream::connect_once;

#[derive(Debug, Clone)]
pub struct TwitterClient {
    client: reqwest::Client,
    cache_dir: PathBuf,
}

impl TwitterClient {
    pub fn new(token: impl AsRef<str>, cache_dir: impl Into<PathBuf>) -> Self {
        let token = token.as_ref();

        let mut headers = HeaderMap::new();
        headers.insert(header::AUTHORIZATION, HeaderValue::from_str(&format!("Bearer {}", token)).unwrap());

        let client = Client::builder()
            .gzip(true)
            .brotli(true)
            .user_agent(concat!(
                env!("CARGO_PKG_NAME"),
                "/",
                env!("CARGO_PKG_VERSION")
            ))
            .default_headers(headers)
            .build()
            .expect("Failed to build HTTP client");

        Self {
            client,
            cache_dir: cache_dir.into(),
        }
    }

    pub fn cache_dir(&self) -> &Path {
        &self.cache_dir
    }
}

impl TwitterClient {
    pub async fn run_stream(&self, router: &mut Router) -> Result<std::convert::Infallible, crate::Error> {
        let mut backoff = Backoff::new();
        backoff.backoff_fn(|duration| {
            let sleep_msecs = duration.as_millis();
            eprintln!("Waiting {} ms...", sleep_msecs);
            sentry::add_breadcrumb(Breadcrumb {
                category: Some(String::from("network")),
                message: Some(format!("Waiting for {} ms", sleep_msecs)),
                level: sentry::Level::Info,
                ..Default::default()
            });
            Box::pin(tokio::time::sleep(duration))
        });

        loop {
            let resp = backoff.run_fn(
                || async {
                    let err = match connect_once(self.client.clone()).await {
                        Ok(resp) => return Ok(resp),
                        Err(err) => err,
                    };

                    if err.is_connect() {
                        eprintln!("Failed to connect: {}", err);
                        sentry::capture_error(&err);
                        return Err(BackoffType::Network);
                    }
                    if let Some(status) = err.status() {
                        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                            eprintln!("Request is ratelimited");
                            sentry::capture_message("Request is ratelimited", sentry::Level::Warning);
                            return Err(BackoffType::Ratelimit);
                        } else if status.is_server_error() {
                            eprintln!("Server side error: {}", err);
                            sentry::capture_error(&err);
                            return Err(BackoffType::Server);
                        }
                    }

                    eprintln!("Unknown error: {}", err);
                    sentry::capture_error(&err);
                    Err(BackoffType::Server)
                },
            ).await;

            stream::run_line_loop(
                self.client.clone(),
                &self.cache_dir,
                resp,
                router,
            ).await.err();
        }
    }
}

impl Deref for TwitterClient {
    type Target = reqwest::Client;

    fn deref(&self) -> &Self::Target {
        &self.client
    }
}
