#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("HTTP error: {0}")]
    Http(#[from] #[source] reqwest::Error),
    #[error("I/O error: {0}")]
    Io(#[from] #[source] std::io::Error),
    #[error(transparent)]
    Parse(#[from] ParseError),
}

impl Error {
    pub fn parse_error(s: String, error: serde_json::Error) -> Self {
        Self::Parse(ParseError::new(s, error))
    }
}

#[derive(Debug, thiserror::Error)]
#[error("parse error: {error}")]
pub struct ParseError {
    s: String,
    #[source]
    error: serde_json::Error,
}

impl ParseError {
    pub fn new(s: String, error: serde_json::Error) -> Self {
        Self { s, error }
    }

    pub fn string(&self) -> &str {
        &self.s
    }

    pub fn into_inner(self) -> serde_json::Error {
        self.error
    }
}

pub type Result<T> = std::result::Result<T, Error>;
