use thiserror::Error;

#[derive(Debug, Error)]
pub enum JsError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("JavaScript error: {0}")]
    Js(String),
    #[error("Compile error: {0}")]
    Compile(String),
    #[error("Bytecode error: {0}")]
    Bytecode(String),
    #[error("JavaScript exception: {message}")]
    Exception {
        message: String,
        stack: Option<String>,
    },
    #[error("Runtime error: {0}")]
    Runtime(String),
    #[error("Channel closed")]
    ChannelClosed,
}
