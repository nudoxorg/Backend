//! Error types for the Go producer.

use std::fmt;

use thiserror::Error;

/// All errors the Go producer can surface.
#[derive(Debug, Error)]
pub enum Error {
    /// The oracle subprocess failed (spawn or non-zero exit).
    #[error("oracle subprocess error")]
    Oracle(#[source] std::io::Error),

    /// The oracle output failed JSON deserialization.
    #[error("oracle JSON deserialization failed")]
    Json(#[from] serde_json::Error),

    /// The lowering step encountered an error (cycle, undeclared, duplicate).
    #[error("lowering error")]
    Lowering(#[source] Box<dyn std::error::Error + Send + Sync>),

    /// A Go construct that the IR cannot yet represent.
    #[error("unsupported Go construct")]
    Unsupported {
        symbol: String,
        reason: &'static str,
    },
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    /// Create an Oracle error from a subprocess I/O failure.
    pub fn oracle_spawn(cmd: impl Into<String>, e: std::io::Error) -> Self {
        let _cmd = cmd;
        Error::Oracle(e)
    }

    /// Create an Oracle error from a non-zero exit status.
    pub fn oracle_exit(cmd: impl Into<String>, code: impl fmt::Display, stderr: String) -> Self {
        let _cmd = cmd;
        let _code = code;
        let _stderr = stderr;
        Error::Oracle(std::io::Error::new(
            std::io::ErrorKind::Other,
            "oracle subprocess exited with non-zero status",
        ))
    }
}
