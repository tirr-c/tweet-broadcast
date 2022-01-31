#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("JS function {0} not found, or is not a function")]
    FunctionNotFound(String),
    #[error("uncaught exception: {0}")]
    JsException(String),
    #[error("cannot convert V8 data: {0}")]
    JsInterop(
        #[from]
        #[source]
        serde_v8::Error,
    ),
}
