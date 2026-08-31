use serde::Deserialize;
use wave_application_core::{
    InvalidUtf8Fact, NativeWorker, NativeWorkerPanic, NativeWorkerPanicClass,
    NativeWorkerPanicMessage,
};

#[derive(Debug, Deserialize, Eq, PartialEq)]
pub struct GoldenInvalidUtf8Fact {
    pub valid_up_to: usize,
    pub error_len: Option<usize>,
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum GoldenNativeWorker {
    SourceWriter,
    StandardOutputReader,
    StandardErrorReader,
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum GoldenNativeWorkerPanicClass {
    StaticMessage,
    OwnedMessage,
    Opaque,
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
pub struct GoldenNativeWorkerPanicMessage {
    pub byte_len: usize,
    pub truncated: bool,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
pub struct GoldenNativeWorkerPanic {
    pub worker: GoldenNativeWorker,
    pub class: GoldenNativeWorkerPanicClass,
    pub message: GoldenNativeWorkerPanicMessage,
}

impl From<InvalidUtf8Fact> for GoldenInvalidUtf8Fact {
    fn from(fact: InvalidUtf8Fact) -> Self {
        Self {
            valid_up_to: fact.valid_up_to,
            error_len: fact.error_len,
        }
    }
}

impl From<NativeWorker> for GoldenNativeWorker {
    fn from(worker: NativeWorker) -> Self {
        match worker {
            NativeWorker::SourceWriter => Self::SourceWriter,
            NativeWorker::StandardOutputReader => Self::StandardOutputReader,
            NativeWorker::StandardErrorReader => Self::StandardErrorReader,
        }
    }
}

impl From<NativeWorkerPanicClass> for GoldenNativeWorkerPanicClass {
    fn from(class: NativeWorkerPanicClass) -> Self {
        match class {
            NativeWorkerPanicClass::StaticMessage => Self::StaticMessage,
            NativeWorkerPanicClass::OwnedMessage => Self::OwnedMessage,
            NativeWorkerPanicClass::Opaque => Self::Opaque,
        }
    }
}

impl From<NativeWorkerPanicMessage> for GoldenNativeWorkerPanicMessage {
    fn from(message: NativeWorkerPanicMessage) -> Self {
        Self {
            byte_len: message.byte_len,
            truncated: message.truncated,
            bytes: message.bytes[..message.byte_len].to_vec(),
        }
    }
}

impl From<NativeWorkerPanic> for GoldenNativeWorkerPanic {
    fn from(panic: NativeWorkerPanic) -> Self {
        Self {
            worker: panic.worker.into(),
            class: panic.class.into(),
            message: panic.message.into(),
        }
    }
}
