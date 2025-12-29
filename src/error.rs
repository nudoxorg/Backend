use thiserror::Error;

#[derive(Debug, Error)]
pub enum NewDocsError {
    #[error("setup error")]
    SetupError,

    #[error("invalid entry")]
    InvalidEntry,

    #[error("Network error: {0}")]
    NetworkError(String),

    #[error("Parsing error: {0}")]
    ParsingError(String),

    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),

    #[error("Process error: {0}")]
    ProcessError(String),

    #[error("Item not found: {0}")]
    NotFound(String),

    #[error("Invalid entry: {0}")]
    InvalidEntry(String),

    #[error("Feature not implemented")]
    NotImplemented,

    #[error("network error: {0}")]
    NetworkError(String),

    #[error("parsing error: {0}")]
    ParsingError(String),

    #[error("file not found")]
    FileNotFound,

    #[error("invalid configuration")]
    InvalidConfiguration,

    #[error("crates.io api error: {0}")]
    CratesIo(#[from] crates_io_api::Error),
}
