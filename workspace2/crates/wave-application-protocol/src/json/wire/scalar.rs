use core::{fmt, str};

use nudox_adaptive::ContentId;
use nudox_id::Domain;
use serde::{Serialize, Serializer, ser::Error as _};
use wave_application_core::{Capability, InputText};

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Language {
    Rust,
    TypeScript,
    Python,
    Go,
    Java,
    CSharp,
    Clang,
}

#[derive(Clone, Copy, Serialize)]
pub(super) enum Stage {
    #[serde(rename = "parse")]
    Parse,
    #[serde(rename = "lower-ir")]
    LowerIr,
}

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

impl From<nudox_compile_vocab::Language> for Language {
    fn from(language: nudox_compile_vocab::Language) -> Self {
        match language {
            nudox_compile_vocab::Language::Rust => Self::Rust,
            nudox_compile_vocab::Language::TypeScript => Self::TypeScript,
            nudox_compile_vocab::Language::Python => Self::Python,
            nudox_compile_vocab::Language::Go => Self::Go,
            nudox_compile_vocab::Language::Java => Self::Java,
            nudox_compile_vocab::Language::CSharp => Self::CSharp,
            nudox_compile_vocab::Language::Clang => Self::Clang,
        }
    }
}

impl From<nudox_compile_vocab::Stage> for Stage {
    fn from(stage: nudox_compile_vocab::Stage) -> Self {
        match stage {
            nudox_compile_vocab::Stage::Parse => Self::Parse,
            nudox_compile_vocab::Stage::LowerIr => Self::LowerIr,
        }
    }
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
    use super::Language;

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

        assert_eq!(nudox_compile_vocab::Language::ALL.len(), EXPECTED.len());
        for (language, expected) in nudox_compile_vocab::Language::ALL.into_iter().zip(EXPECTED) {
            assert_eq!(
                serde_json::to_string(&Language::from(language))?,
                format!("\"{expected}\"")
            );
        }
        Ok(())
    }
}
