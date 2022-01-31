#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("HTTP error: {0}")]
    Http(
        #[from]
        #[source]
        reqwest::Error,
    ),
    #[error("I/O error: {0}")]
    Io(
        #[from]
        #[source]
        std::io::Error,
    ),
    #[error("stream closed")]
    StreamClosed,
    #[error("route error: {0}")]
    Route(
        #[from]
        #[source]
        tweet_route::Error,
    ),
    #[error(transparent)]
    Twitter(#[from] tweet_model::ResponseError),
}
