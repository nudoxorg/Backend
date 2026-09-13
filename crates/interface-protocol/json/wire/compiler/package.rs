//! Exact JSON projection for package compilation progress and entry failures.

use std::{io::ErrorKind, path::Path};

use backend_semantic::vocabulary::{NativeWorkerPanicClass, NativeWorkerPanicMessage};
use interface_core::{
    CompilerRuntimeCause, CompilerRuntimePanic, PackageCompilePhase, PackageDeclarationScopeCause,
    PackageEcosystem, PackagePathComponentError, PackageSourceCause, PackageSourceIoFact,
    PackageSourceIoPhase, PackageTextRange,
};
use serde::{Serialize, Serializer, ser::SerializeStruct};

use super::super::scalar::serialize_error_kind;

#[derive(Serialize)]
#[serde(
    remote = "interface_core::PackageCompilePhase",
    rename_all = "snake_case"
)]
pub(crate) enum PackageCompilePhaseWire {
    Locate,
    EnterSource,
    Authority,
    Lower,
    Publish,
    Reopen,
    Discover,
    Render,
}

#[derive(Serialize)]
#[serde(remote = "interface_core::PackageEcosystem", rename_all = "snake_case")]
enum PackageEcosystemWire {
    Cargo,
    Npm,
    Pypi,
    Golang,
    Maven,
    Nuget,
    Generic,
}

#[derive(Serialize)]
#[serde(remote = "interface_core::PackageTextRange")]
struct PackageTextRangeWire {
    start: u16,
    end: u16,
}

#[derive(Serialize)]
#[serde(
    remote = "interface_core::PackagePathComponentError",
    rename_all = "snake_case"
)]
enum PackagePathComponentErrorWire {
    Traversal,
    EncodedSeparator,
    InvalidUtf8,
}

#[derive(Serialize)]
#[serde(
    remote = "interface_core::PackageDeclarationScopeCause",
    rename_all = "snake_case"
)]
enum PackageDeclarationScopeCauseWire {
    EmptyEcosystem,
    EmptyPackage,
    EcosystemSeparator,
    PackageSeparator,
    LineageBackslash { segment: u8 },
    EmptySourcePath,
    SourceBackslash,
}

#[derive(Serialize)]
#[serde(
    remote = "interface_core::PackageSourceIoPhase",
    rename_all = "snake_case"
)]
enum PackageSourceIoPhaseWire {
    CanonicalizeStore,
    EnumerateRegistry,
    CanonicalizePackage,
    CanonicalizeSource,
    SourceMetadata,
    ReadSource,
}

#[derive(Serialize)]
#[serde(remote = "interface_core::PackageSourceIoFact")]
struct PackageSourceIoFactWire {
    #[serde(serialize_with = "serialize_error_kind")]
    kind: ErrorKind,
    raw_os_code: Option<i32>,
}

#[derive(Serialize)]
#[serde(
    remote = "interface_core::PackageSourceCause",
    tag = "kind",
    rename_all = "snake_case"
)]
pub(crate) enum PackageSourceCauseWire {
    RootUnavailable {
        #[serde(with = "PackageEcosystemWire")]
        ecosystem: PackageEcosystem,
    },
    SubpathRequired {
        #[serde(with = "PackageEcosystemWire")]
        ecosystem: PackageEcosystem,
    },
    InvalidComponent {
        #[serde(with = "PackageTextRangeWire")]
        range: PackageTextRange,
        #[serde(with = "PackagePathComponentErrorWire")]
        cause: PackagePathComponentError,
    },
    DeclarationScope {
        #[serde(with = "PackageDeclarationScopeCauseWire")]
        cause: PackageDeclarationScopeCause,
    },
    InvalidUtf8 {
        valid_up_to: usize,
        error_len: Option<u8>,
    },
    PackageUnavailable {
        path: Box<Path>,
    },
    PackageEscapesStore {
        store: Box<Path>,
        package: Box<Path>,
    },
    SourceUnavailable {
        path: Box<Path>,
    },
    SourceEscapesPackage {
        package: Box<Path>,
        source: Box<Path>,
    },
    SourceTooLarge {
        observed: u64,
        maximum: u64,
    },
    Io {
        #[serde(with = "PackageSourceIoPhaseWire")]
        phase: PackageSourceIoPhase,
        path: Box<Path>,
        #[serde(with = "PackageSourceIoFactWire")]
        source: PackageSourceIoFact,
    },
    RegistryNamespaceCapacity {
        observed: usize,
        maximum: usize,
    },
    RegistryNamespaceAmbiguous {
        first: Box<Path>,
        second: Box<Path>,
    },
}

#[derive(Serialize)]
#[serde(
    remote = "backend_semantic::vocabulary::NativeWorkerPanicClass",
    rename_all = "snake_case"
)]
enum RuntimePanicClassWire {
    StaticMessage,
    OwnedMessage,
    Opaque,
}

#[derive(Serialize)]
#[serde(remote = "interface_core::CompilerRuntimePanic")]
struct CompilerRuntimePanicWire {
    #[serde(with = "RuntimePanicClassWire")]
    class: NativeWorkerPanicClass,
    #[serde(serialize_with = "serialize_panic_message")]
    message: NativeWorkerPanicMessage,
}

#[derive(Serialize)]
#[serde(
    remote = "interface_core::CompilerRuntimeCause",
    tag = "kind",
    rename_all = "snake_case"
)]
pub(crate) enum CompilerRuntimeCauseWire {
    RequestInFlight,
    RequestOwnerStopped,
    ResponseOwnerStopped,
    WorkerPanic(#[serde(with = "CompilerRuntimePanicWire")] CompilerRuntimePanic),
}

fn serialize_panic_message<Output: Serializer>(
    message: &NativeWorkerPanicMessage,
    serializer: Output,
) -> Result<Output::Ok, Output::Error> {
    let mut state = serializer.serialize_struct("NativeWorkerPanicMessage", 3)?;
    state.serialize_field("byte_len", &message.byte_len)?;
    state.serialize_field("truncated", &message.truncated)?;
    state.serialize_field("bytes", &message.bytes[..message.byte_len])?;
    state.end()
}

const _: fn(&PackageCompilePhase) = |_| {};
const _: fn(&PackageSourceCause) = |_| {};
const _: fn(&CompilerRuntimeCause) = |_| {};
