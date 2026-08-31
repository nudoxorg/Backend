use core::{fmt, str};
use std::io::ErrorKind;

use nudox_adaptive::ContentId;
use nudox_id::{ArtifactId, Domain, Encoding};
use serde::{Serialize, Serializer, ser::Error as _};
use wave_application_core::{Capability, InputText};

#[derive(Serialize)]
#[serde(remote = "nudox_compile_vocab::Language", rename_all = "snake_case")]
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
#[serde(remote = "nudox_compile_vocab::Stage")]
pub(super) enum StageWire {
    #[serde(rename = "parse")]
    Parse,
    #[serde(rename = "lower-ir")]
    LowerIr,
}

#[derive(Serialize)]
#[serde(remote = "nudox_compile_vocab::NativeTool", rename_all = "snake_case")]
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

impl From<nudox_adaptive::CapabilityKind> for CapabilityKind {
    fn from(capability: nudox_adaptive::CapabilityKind) -> Self {
        match capability {
            nudox_adaptive::CapabilityKind::Analyzer => Self::Analyzer,
            nudox_adaptive::CapabilityKind::Compiler => Self::Compiler,
            nudox_adaptive::CapabilityKind::Codec => Self::Codec,
            nudox_adaptive::CapabilityKind::Model => Self::Model,
        }
    }
}

impl From<nudox_adaptive::ExecutionPhase> for ExecutionPhase {
    fn from(phase: nudox_adaptive::ExecutionPhase) -> Self {
        match phase {
            nudox_adaptive::ExecutionPhase::LocalResidence => Self::LocalResidence,
            nudox_adaptive::ExecutionPhase::CapabilityBundle => Self::CapabilityBundle,
            nudox_adaptive::ExecutionPhase::Remote => Self::Remote,
        }
    }
}

impl From<nudox_adaptive::ResourceClass> for ResourceClass {
    fn from(resource: nudox_adaptive::ResourceClass) -> Self {
        match resource {
            nudox_adaptive::ResourceClass::Ram => Self::Ram,
            nudox_adaptive::ResourceClass::Nvme => Self::Nvme,
            nudox_adaptive::ResourceClass::Operations => Self::Operations,
            nudox_adaptive::ResourceClass::Retries => Self::Retries,
        }
    }
}

impl From<nudox_adaptive::InputClass> for InputClass {
    fn from(class: nudox_adaptive::InputClass) -> Self {
        match class {
            nudox_adaptive::InputClass::Local => Self::Local,
            nudox_adaptive::InputClass::Remote => Self::Remote,
            nudox_adaptive::InputClass::Demand => Self::Demand,
            nudox_adaptive::InputClass::Bundle => Self::Bundle,
        }
    }
}

impl From<nudox_adaptive::StorageTier> for StorageTier {
    fn from(tier: nudox_adaptive::StorageTier) -> Self {
        match tier {
            nudox_adaptive::StorageTier::Ram => Self::Ram,
            nudox_adaptive::StorageTier::Nvme => Self::Nvme,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::LanguageWire;
    use nudox_compile_vocab::Language;
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
