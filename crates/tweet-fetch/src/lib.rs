use std::ops::Deref;

use reqwest::{
    header::{self, HeaderMap, HeaderValue},
    Client,
};

use tweet_model as model;

mod backoff;
mod error;
mod list;
mod stream;
#[macro_use]
mod util;

use concat_param;
pub use error::Error;
pub use list::ListHead;

#[derive(Debug, Clone)]
pub struct TwitterClient {
    client: reqwest::Client,
}

impl TwitterClient {
    pub fn new(token: impl AsRef<str>) -> Self {
        let token = token.as_ref();

        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {}", token)).unwrap(),
        );

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
        }
    }
}

impl TwitterClient {
    pub async fn retrieve(
        &self,
        ids: &[&str],
    ) -> Result<model::ResponseItem<Vec<model::Tweet>>, Error> {
        use futures_util::{TryFutureExt, TryStreamExt};

        const TWEET_ENDPOINT: &str = "https://api.twitter.com/2/tweets";
        let mut url = TWEET_ENDPOINT.parse::<reqwest::Url>().unwrap();
        util::append_query_param_for_tweet(&mut url);

        Ok(match ids {
            [] => Default::default(),
            [id] => {
                url.path_segments_mut().unwrap().push(id);

                let res = self
                    .client
                    .get(url)
                    .send()
                    .await?
                    .error_for_status()?
                    .json::<model::TwitterResponse<model::Tweet>>()
                    .await?
                    .into_result()?;
                let model::ResponseItem {
                    data,
                    includes,
                    meta,
                } = res;
                model::ResponseItem {
                    data: vec![data],
                    includes,
                    meta,
                }
            }
            ids => {
                let mut req_fut = futures_util::stream::FuturesOrdered::new();
                for ids in ids.chunks(100) {
                    // 100 tweets at a time
                    let id_param = ids.join(",");
                    let mut url = url.clone();
                    url.query_pairs_mut().append_pair("ids", &id_param).finish();

                    req_fut.push(
                        self.client
                            .get(url)
                            .send()
                            .map_err(Error::from)
                            .and_then(|resp| async move {
                                let resp = resp
                                    .error_for_status()?
                                    .json::<model::TwitterResponse<Vec<model::Tweet>>>()
                                    .await?
                                    .into_result()?;
                                Ok(resp)
                            }),
                    );
                }

                req_fut
                    .try_fold(
                        model::ResponseItem::<Vec<_>>::default(),
                        |mut base, tweets| async move {
                            base.data.extend(tweets.data);
                            base.includes.augment(tweets.includes);
                            Ok(base)
                        },
                    )
                    .await?
            }
        })
    }

    pub fn make_stream(&self) -> impl futures_util::Stream<Item = Result<model::ResponseItem<model::Tweet, model::StreamMeta>, Error>> {
        stream::make_stream(self.clone())
    }
}

impl Deref for TwitterClient {
    type Target = reqwest::Client;

    fn deref(&self) -> &Self::Target {
        &self.client
    }
}
