use serde::Deserialize;
use wave_application_core::{NativeIoFact, NativeIoPhase};

#[derive(Debug, Deserialize, Eq, PartialEq)]
pub struct GoldenNativeIoFact {
    pub kind: GoldenErrorKind,
    pub raw_os_code: Option<i32>,
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum GoldenNativeIoPhase {
    Start,
    Input,
    Terminate,
    Wait,
    DiagnosticRead,
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum GoldenErrorKind {
    NotFound,
    PermissionDenied,
    ConnectionRefused,
    ConnectionReset,
    HostUnreachable,
    NetworkUnreachable,
    ConnectionAborted,
    NotConnected,
    AddrInUse,
    AddrNotAvailable,
    BrokenPipe,
    AlreadyExists,
    WouldBlock,
    InvalidInput,
    InvalidData,
    TimedOut,
    WriteZero,
    Interrupted,
    Unsupported,
    UnexpectedEof,
    OutOfMemory,
    Other,
}

impl From<NativeIoFact> for GoldenNativeIoFact {
    fn from(fact: NativeIoFact) -> Self {
        Self {
            kind: fact.kind.into(),
            raw_os_code: fact.raw_os_code,
        }
    }
}

impl From<NativeIoPhase> for GoldenNativeIoPhase {
    fn from(phase: NativeIoPhase) -> Self {
        match phase {
            NativeIoPhase::Start => Self::Start,
            NativeIoPhase::Input => Self::Input,
            NativeIoPhase::Terminate => Self::Terminate,
            NativeIoPhase::Wait => Self::Wait,
            NativeIoPhase::DiagnosticRead => Self::DiagnosticRead,
        }
    }
}

impl From<std::io::ErrorKind> for GoldenErrorKind {
    fn from(kind: std::io::ErrorKind) -> Self {
        match kind {
            std::io::ErrorKind::NotFound => Self::NotFound,
            std::io::ErrorKind::PermissionDenied => Self::PermissionDenied,
            std::io::ErrorKind::ConnectionRefused => Self::ConnectionRefused,
            std::io::ErrorKind::ConnectionReset => Self::ConnectionReset,
            std::io::ErrorKind::HostUnreachable => Self::HostUnreachable,
            std::io::ErrorKind::NetworkUnreachable => Self::NetworkUnreachable,
            std::io::ErrorKind::ConnectionAborted => Self::ConnectionAborted,
            std::io::ErrorKind::NotConnected => Self::NotConnected,
            std::io::ErrorKind::AddrInUse => Self::AddrInUse,
            std::io::ErrorKind::AddrNotAvailable => Self::AddrNotAvailable,
            std::io::ErrorKind::BrokenPipe => Self::BrokenPipe,
            std::io::ErrorKind::AlreadyExists => Self::AlreadyExists,
            std::io::ErrorKind::WouldBlock => Self::WouldBlock,
            std::io::ErrorKind::InvalidInput => Self::InvalidInput,
            std::io::ErrorKind::InvalidData => Self::InvalidData,
            std::io::ErrorKind::TimedOut => Self::TimedOut,
            std::io::ErrorKind::WriteZero => Self::WriteZero,
            std::io::ErrorKind::Interrupted => Self::Interrupted,
            std::io::ErrorKind::Unsupported => Self::Unsupported,
            std::io::ErrorKind::UnexpectedEof => Self::UnexpectedEof,
            std::io::ErrorKind::OutOfMemory => Self::OutOfMemory,
            _ => Self::Other,
        }
    }
}
