//! Defines json wire scalar behavior for `interface-protocol`, whose purpose is to decode and project the shared application vocabulary for external transports.
//! This module owns the json wire scalar invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
use core::{fmt, str};
use std::io::ErrorKind;

use backend_execution::adaptive::ContentId;
use backend_version::{ArtifactId, Domain, Encoding};
use interface_core::{Capability, InputText};
use serde::{Serialize, Serializer, ser::Error as _};

#[derive(Serialize)]
#[serde(remote = "backend_semantic::vocabulary::Language", rename_all = "snake_case")]
pub(super) enum LanguageWire {
    Rust,
    TypeScript,
    Python,
    Go,
    Java,
    CSharp,
    Clang,
}

#[derive(Serialize)]
#[serde(
    remote = "backend_semantic::vocabulary::AuthorityPhase",
    rename_all = "snake_case"
)]
pub(super) enum AuthorityPhaseWire {
    Open,
    Parse,
    Resolve,
    TypeCheck,
    Project,
}

#[derive(Serialize)]
#[serde(
    remote = "backend_semantic::vocabulary::AuthorityDiagnosticClass",
    rename_all = "snake_case"
)]
pub(super) enum AuthorityDiagnosticClassWire {
    Syntax,
    Binding,
    Type,
    Authority,
    Projection,
}

#[derive(Serialize)]
#[serde(
    remote = "backend_semantic::vocabulary::LanguageProfile",
    rename_all = "snake_case"
)]
pub(super) enum LanguageProfileWire {
    Rust(#[serde(with = "RustEditionWire")] backend_semantic::vocabulary::RustEdition),
    TypeScript(#[serde(with = "TypeScriptSourceWire")] backend_semantic::vocabulary::TypeScriptSource),
    Python(#[serde(with = "PythonVersionWire")] backend_semantic::vocabulary::PythonVersion),
    Go(#[serde(with = "GoVersionWire")] backend_semantic::vocabulary::GoVersion),
    Java(#[serde(with = "JavaReleaseWire")] backend_semantic::vocabulary::JavaRelease),
    CSharp(#[serde(with = "CSharpVersionWire")] backend_semantic::vocabulary::CSharpVersion),
    C(#[serde(with = "CStandardWire")] backend_semantic::vocabulary::CStandard),
    Cxx(#[serde(with = "CxxStandardWire")] backend_semantic::vocabulary::CxxStandard),
}

#[derive(Serialize)]
#[serde(remote = "backend_semantic::vocabulary::RustEdition", rename_all = "snake_case")]
pub(super) enum RustEditionWire {
    Rust2015,
    Rust2018,
    Rust2021,
    Rust2024,
}

#[derive(Serialize)]
#[serde(
    remote = "backend_semantic::vocabulary::TypeScriptSource",
    rename_all = "snake_case"
)]
pub(super) enum TypeScriptSourceWire {
    TypeScript,
    Tsx,
}

#[derive(Serialize)]
#[serde(
    remote = "backend_semantic::vocabulary::PythonVersion",
    rename_all = "snake_case"
)]
pub(super) enum PythonVersionWire {
    Python310,
    Python311,
    Python312,
    Python313,
    Python314,
}

#[derive(Serialize)]
#[serde(remote = "backend_semantic::vocabulary::GoVersion", rename_all = "snake_case")]
pub(super) enum GoVersionWire {
    Go122,
    Go123,
    Go124,
    Go125,
}

#[derive(Serialize)]
#[serde(remote = "backend_semantic::vocabulary::JavaRelease", rename_all = "snake_case")]
pub(super) enum JavaReleaseWire {
    Java8,
    Java11,
    Java17,
    Java21,
    Java25,
}

#[derive(Serialize)]
#[serde(
    remote = "backend_semantic::vocabulary::CSharpVersion",
    rename_all = "snake_case"
)]
pub(super) enum CSharpVersionWire {
    CSharp10,
    CSharp11,
    CSharp12,
    CSharp13,
    CSharp14,
}

#[derive(Serialize)]
#[serde(remote = "backend_semantic::vocabulary::CStandard", rename_all = "snake_case")]
pub(super) enum CStandardWire {
    C11,
    C17,
    C23,
}

#[derive(Serialize)]
#[serde(remote = "backend_semantic::vocabulary::CxxStandard", rename_all = "snake_case")]
pub(super) enum CxxStandardWire {
    Cxx17,
    Cxx20,
    Cxx23,
    Cxx26,
}

#[derive(Serialize)]
#[serde(remote = "backend_semantic::vocabulary::Stage")]
pub(super) enum StageWire {
    #[serde(rename = "parse")]
    Parse,
    #[serde(rename = "lower-ir")]
    LowerIr,
}

#[derive(Serialize)]
#[serde(remote = "backend_semantic::vocabulary::NativeTool", rename_all = "snake_case")]
pub(super) enum NativeToolWire {
    Rustc,
    Clang,
    Python,
    TypeScriptCompiler,
    GoCompiler,
    JavaCompiler,
    CSharpCompiler,
}

/*
 * The compiler vocabulary is deliberately not made to depend on serde.  These remote definitions
 * keep the application wire projection coupled to the public vocabulary's exhaustive shape while
 * avoiding a second set of conversion enums.
 */

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum CapabilityName {
    CompilerRegistry,
    CompilerOutput,
    Index,
    Graph,
    Vector,
    LocalAnalyzer,
    Remote,
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum CapabilityKind {
    Analyzer,
    Compiler,
    Codec,
    Model,
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum ExecutionPhase {
    LocalResidence,
    CapabilityBundle,
    Remote,
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum ResourceClass {
    Ram,
    Nvme,
    Operations,
    Retries,
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum InputClass {
    Local,
    Remote,
    Demand,
    Bundle,
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum StorageTier {
    Ram,
    Nvme,
}

pub(super) struct Text(pub(super) InputText);

impl Serialize for Text {
    fn serialize<Output>(&self, serializer: Output) -> Result<Output::Ok, Output::Error>
    where
        Output: Serializer,
    {
        let value = str::from_utf8(self.0.as_ref()).map_err(Output::Error::custom)?;
        serializer.serialize_str(value)
    }
}

pub(super) struct ContentText<DomainTag: Domain>(pub(super) ContentId<DomainTag>);

impl<DomainTag: Domain> Serialize for ContentText<DomainTag> {
    fn serialize<Output>(&self, serializer: Output) -> Result<Output::Ok, Output::Error>
    where
        Output: Serializer,
    {
        serializer.collect_str(&DisplayContent(self.0))
    }
}

struct DisplayContent<DomainTag: Domain>(ContentId<DomainTag>);

impl<DomainTag: Domain> fmt::Display for DisplayContent<DomainTag> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

struct DisplayArtifact<EncodingTag: Encoding, DomainTag: Domain>(
    ArtifactId<EncodingTag, DomainTag>,
);

impl<EncodingTag: Encoding, DomainTag: Domain> fmt::Display
    for DisplayArtifact<EncodingTag, DomainTag>
{
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// Serializes an identity without erasing its domain-specific type at the call site.
pub(super) fn serialize_content<DomainTag: Domain, Output: Serializer>(
    value: &ContentId<DomainTag>,
    serializer: Output,
) -> Result<Output::Ok, Output::Error> {
    serializer.collect_str(&DisplayContent(*value))
}

/// Serializes an artifact identity without converting it to an untyped intermediate value.
pub(super) fn serialize_artifact<EncodingTag: Encoding, DomainTag: Domain, Output: Serializer>(
    value: &ArtifactId<EncodingTag, DomainTag>,
    serializer: Output,
) -> Result<Output::Ok, Output::Error> {
    serializer.collect_str(&DisplayArtifact(*value))
}

/// Serializes the standard-library I/O category as the closed wire vocabulary.
#[allow(clippy::trivially_copy_pass_by_ref)]
pub(super) fn serialize_error_kind<Output: Serializer>(
    kind: &ErrorKind,
    serializer: Output,
) -> Result<Output::Ok, Output::Error> {
    let value = match *kind {
        ErrorKind::NotFound => "not_found",
        ErrorKind::PermissionDenied => "permission_denied",
        ErrorKind::ConnectionRefused => "connection_refused",
        ErrorKind::ConnectionReset => "connection_reset",
        ErrorKind::HostUnreachable => "host_unreachable",
        ErrorKind::NetworkUnreachable => "network_unreachable",
        ErrorKind::ConnectionAborted => "connection_aborted",
        ErrorKind::NotConnected => "not_connected",
        ErrorKind::AddrInUse => "addr_in_use",
        ErrorKind::AddrNotAvailable => "addr_not_available",
        ErrorKind::BrokenPipe => "broken_pipe",
        ErrorKind::AlreadyExists => "already_exists",
        ErrorKind::WouldBlock => "would_block",
        ErrorKind::InvalidInput => "invalid_input",
        ErrorKind::InvalidData => "invalid_data",
        ErrorKind::TimedOut => "timed_out",
        ErrorKind::WriteZero => "write_zero",
        ErrorKind::Interrupted => "interrupted",
        ErrorKind::Unsupported => "unsupported",
        ErrorKind::UnexpectedEof => "unexpected_eof",
        ErrorKind::OutOfMemory => "out_of_memory",
        _ => "other",
    };
    serializer.serialize_str(value)
}

impl From<Capability> for CapabilityName {
    fn from(capability: Capability) -> Self {
        match capability {
            Capability::CompilerRegistry => Self::CompilerRegistry,
            Capability::CompilerOutput => Self::CompilerOutput,
            Capability::Index => Self::Index,
            Capability::Graph => Self::Graph,
            Capability::Vector => Self::Vector,
            Capability::LocalAnalyzer => Self::LocalAnalyzer,
            Capability::Remote => Self::Remote,
        }
    }
}

impl From<backend_execution::adaptive::CapabilityKind> for CapabilityKind {
    fn from(capability: backend_execution::adaptive::CapabilityKind) -> Self {
        match capability {
            backend_execution::adaptive::CapabilityKind::Analyzer => Self::Analyzer,
            backend_execution::adaptive::CapabilityKind::Compiler => Self::Compiler,
            backend_execution::adaptive::CapabilityKind::Codec => Self::Codec,
            backend_execution::adaptive::CapabilityKind::Model => Self::Model,
        }
    }
}

impl From<backend_execution::adaptive::ExecutionPhase> for ExecutionPhase {
    fn from(phase: backend_execution::adaptive::ExecutionPhase) -> Self {
        match phase {
            backend_execution::adaptive::ExecutionPhase::LocalResidence => Self::LocalResidence,
            backend_execution::adaptive::ExecutionPhase::CapabilityBundle => Self::CapabilityBundle,
            backend_execution::adaptive::ExecutionPhase::Remote => Self::Remote,
        }
    }
}

impl From<backend_execution::adaptive::ResourceClass> for ResourceClass {
    fn from(resource: backend_execution::adaptive::ResourceClass) -> Self {
        match resource {
            backend_execution::adaptive::ResourceClass::Ram => Self::Ram,
            backend_execution::adaptive::ResourceClass::Nvme => Self::Nvme,
            backend_execution::adaptive::ResourceClass::Operations => Self::Operations,
            backend_execution::adaptive::ResourceClass::Retries => Self::Retries,
        }
    }
}

impl From<backend_execution::adaptive::InputClass> for InputClass {
    fn from(class: backend_execution::adaptive::InputClass) -> Self {
        match class {
            backend_execution::adaptive::InputClass::Local => Self::Local,
            backend_execution::adaptive::InputClass::Remote => Self::Remote,
            backend_execution::adaptive::InputClass::Demand => Self::Demand,
            backend_execution::adaptive::InputClass::Bundle => Self::Bundle,
        }
    }
}

impl From<backend_execution::adaptive::StorageTier> for StorageTier {
    fn from(tier: backend_execution::adaptive::StorageTier) -> Self {
        match tier {
            backend_execution::adaptive::StorageTier::Ram => Self::Ram,
            backend_execution::adaptive::StorageTier::Nvme => Self::Nvme,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::LanguageWire;
    use backend_semantic::vocabulary::Language;
    use serde::Serialize;

    #[derive(Serialize)]
    struct LanguageProjection(#[serde(with = "LanguageWire")] Language);

    #[test]
    fn language_wire_projection_covers_the_closed_vocab() -> Result<(), serde_json::Error> {
        const EXPECTED: &[&str] = &[
            "rust",
            "type_script",
            "python",
            "go",
            "java",
            "c_sharp",
            "clang",
        ];

        assert_eq!(Language::ALL.len(), EXPECTED.len());
        for (language, expected) in Language::ALL.into_iter().zip(EXPECTED) {
            assert_eq!(
                serde_json::to_string(&LanguageProjection(language))?,
                format!("\"{expected}\"")
            );
        }
        Ok(())
    }
}
