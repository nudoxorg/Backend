use thiserror::Error;

#[derive(Debug, Error)]
pub enum NewDocsError {
    #[error("setup error")]
    SetupError,

    #[error("invalid entry")]
    InvalidEntry,

    #[error("network error")]
    NetworkError,

    #[error("parsing error")]
    ParsingError,

    #[error("file not found")]
    FileNotFound,

    #[error("invalid configuration")]
    InvalidConfiguration,

    #[error("crates.io api error: {0}")]
    CratesIo(#[from] crates_io_api::Error),
}
