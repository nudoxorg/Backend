use nudox_compile_vocab::{
    InvalidUtf8Fact, NativeWorker, NativeWorkerPanic, NativeWorkerPanicClass,
    NativeWorkerPanicMessage,
};
use serde::{Serialize, Serializer, ser::SerializeStruct};

/// Remote serde definitions for closed native worker vocabulary.
#[derive(Serialize)]
#[serde(
    remote = "nudox_compile_vocab::NativeWorker",
    rename_all = "snake_case"
)]
enum NativeWorkerWire {
    SourceWriter,
    StandardOutputReader,
    StandardErrorReader,
}

#[derive(Serialize)]
#[serde(
    remote = "nudox_compile_vocab::NativeWorkerPanicClass",
    rename_all = "snake_case"
)]
enum NativeWorkerPanicClassWire {
    StaticMessage,
    OwnedMessage,
    Opaque,
}

#[derive(Serialize)]
#[serde(remote = "nudox_compile_vocab::InvalidUtf8Fact")]
struct InvalidUtf8FactWire {
    valid_up_to: usize,
    error_len: Option<usize>,
}

#[derive(Serialize)]
#[serde(remote = "nudox_compile_vocab::NativeWorkerPanic")]
struct NativeWorkerPanicWire {
    #[serde(with = "NativeWorkerWire")]
    worker: NativeWorker,
    #[serde(with = "NativeWorkerPanicClassWire")]
    class: NativeWorkerPanicClass,
    #[serde(serialize_with = "serialize_worker_message")]
    message: NativeWorkerPanicMessage,
}

pub(super) struct InvalidUtf8FactRef<'value>(pub(super) &'value InvalidUtf8Fact);

impl Serialize for InvalidUtf8FactRef<'_> {
    fn serialize<Output: Serializer>(
        &self,
        serializer: Output,
    ) -> Result<Output::Ok, Output::Error> {
        InvalidUtf8FactWire::serialize(self.0, serializer)
    }
}

pub(crate) struct NativeWorkerPanicRef<'value>(pub(crate) &'value NativeWorkerPanic);

impl Serialize for NativeWorkerPanicRef<'_> {
    fn serialize<Output: Serializer>(
        &self,
        serializer: Output,
    ) -> Result<Output::Ok, Output::Error> {
        NativeWorkerPanicWire::serialize(self.0, serializer)
    }
}

fn serialize_worker_message<Output: Serializer>(
    message: &NativeWorkerPanicMessage,
    serializer: Output,
) -> Result<Output::Ok, Output::Error> {
    let mut state = serializer.serialize_struct("NativeWorkerPanicMessage", 3)?;
    state.serialize_field("byte_len", &message.byte_len)?;
    state.serialize_field("truncated", &message.truncated)?;
    state.serialize_field("bytes", &message.bytes[..message.byte_len])?;
    state.end()
}

const _: fn(&NativeWorker) = |_| {};
const _: fn(&NativeWorkerPanic) = |_| {};
const _: fn(&NativeWorkerPanicClass) = |_| {};
const _: fn(&NativeWorkerPanicMessage) = |_| {};
