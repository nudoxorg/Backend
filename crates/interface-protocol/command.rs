//! Defines command behavior for `interface-protocol`, whose purpose is to decode and project the shared application vocabulary for external transports.
//! This module owns the command invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Closed raw application commands shared by positional and JSON-RPC adapters.

use backend_semantic::vocabulary::{LanguageProfile, Stage};
use interface_core::InputText;
use serde::{Deserialize, Deserializer, de::Error as _};

/// A bounded numeric transport value accepted in either native JSON or CLI text form.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub(crate) enum RawNumber {
    /// Native JSON unsigned integer retained without formatting or reparsing.
    Number(u64),
    /// Decimal text retained for positional CLI parity.
    Text(String),
}

fn deserialize_profile<'de, Decoder>(decoder: Decoder) -> Result<LanguageProfile, Decoder::Error>
where
    Decoder: Deserializer<'de>,
{
    let value = String::deserialize(decoder)?;
    LanguageProfile::try_from(value.as_str()).map_err(Decoder::Error::custom)
}

/// Closed compiler stage token at the one adapter boundary that interprets transport text.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
pub(crate) enum RawStage {
    /// Native syntax validation.
    #[serde(rename = "parse")]
    Parse,
    /// Native syntax validation plus compact IR lowering.
    #[serde(rename = "lower-ir")]
    LowerIr,
}

impl TryFrom<InputText> for RawStage {
    type Error = InputText;

    fn try_from(value: InputText) -> Result<Self, Self::Error> {
        match &*value {
            "parse" => Ok(Self::Parse),
            "lower-ir" => Ok(Self::LowerIr),
            _ => Err(value),
        }
    }
}

impl From<RawStage> for Stage {
    fn from(value: RawStage) -> Self {
        match value {
            RawStage::Parse => Self::Parse,
            RawStage::LowerIr => Self::LowerIr,
        }
    }
}

/// Compiler vocabulary and source fields.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawGenerate {
    /// Request correlation.
    pub(crate) correlation: u64,
    /// Compiler source-profile token.
    #[serde(deserialize_with = "deserialize_profile")]
    pub(crate) profile: LanguageProfile,
    /// Compiler stage token.
    pub(crate) stage: RawStage,
    /// Source text.
    pub(crate) source: String,
}

/// Compiler vocabulary and pinned package-url fields.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawCompilePackage {
    /// Request correlation.
    pub(crate) correlation: u64,
    /// Compiler source-profile token.
    #[serde(deserialize_with = "deserialize_profile")]
    pub(crate) profile: LanguageProfile,
    /// Compiler stage token.
    pub(crate) stage: RawStage,
    /// Exact pinned package URL.
    pub(crate) purl: String,
}

/// Shared immutable snapshot selector.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawSnapshot {
    /// Request correlation.
    pub(crate) correlation: u64,
    /// Snapshot selector.
    pub(crate) snapshot: String,
}

/// Lexical query fields.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawSearch {
    /// Request correlation.
    pub(crate) correlation: u64,
    /// Snapshot selector.
    pub(crate) snapshot: String,
    /// Lexical query.
    pub(crate) query: String,
    /// Result limit in native JSON or CLI text form.
    pub(crate) limit: RawNumber,
}

/// Shared graph and vector retrieval fields.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawRetrieval {
    /// Request correlation.
    pub(crate) correlation: u64,
    /// Snapshot selector.
    pub(crate) snapshot: String,
    /// Result limit in native JSON or CLI text form.
    pub(crate) limit: RawNumber,
}

/// Argument-free capability health fields.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawHealth {
    /// Request correlation.
    pub(crate) correlation: u64,
}

/// Shared local recovery and release policy.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawPolicy {
    /// Request correlation.
    pub(crate) correlation: u64,
    /// Generation authority.
    pub(crate) generation: String,
    /// Snapshot authority.
    pub(crate) snapshot: String,
    /// Analyzer bundle identity.
    pub(crate) bundle: String,
    /// Free RAM budget.
    pub(crate) ram_free: RawNumber,
    /// Free `NVMe` budget.
    pub(crate) nvme_free: RawNumber,
    /// Operation budget.
    pub(crate) operations: RawNumber,
    /// Retry budget.
    pub(crate) retries: RawNumber,
    /// Memory pressure token.
    pub(crate) memory_pressure: String,
    /// Storage pressure token.
    pub(crate) storage_pressure: String,
    /// Battery state token.
    pub(crate) battery: String,
}

/// Inconsistent recovery authorities and policy budget.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawInconsistentPolicy {
    /// Request correlation.
    pub(crate) correlation: u64,
    /// Expected generation authority.
    pub(crate) expected_generation: String,
    /// Expected snapshot authority.
    pub(crate) expected_snapshot: String,
    /// Observed generation authority.
    pub(crate) observed_generation: String,
    /// Observed snapshot authority.
    pub(crate) observed_snapshot: String,
    /// Analyzer bundle identity.
    pub(crate) bundle: String,
    /// Free RAM budget.
    pub(crate) ram_free: RawNumber,
    /// Free `NVMe` budget.
    pub(crate) nvme_free: RawNumber,
    /// Operation budget.
    pub(crate) operations: RawNumber,
    /// Retry budget.
    pub(crate) retries: RawNumber,
    /// Memory pressure token.
    pub(crate) memory_pressure: String,
    /// Storage pressure token.
    pub(crate) storage_pressure: String,
    /// Battery state token.
    pub(crate) battery: String,
}

/// Shared execution operation selector.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawOperation {
    /// Request correlation.
    pub(crate) correlation: u64,
    /// Service execution operation key.
    pub(crate) operation: RawNumber,
}

/// One closed application command before transport-independent typed conversion.
#[derive(Debug, Deserialize)]
#[serde(tag = "action", rename_all = "kebab-case")]
pub(crate) enum RawApplicationCommand {
    /// Compiler vocabulary request.
    Generate(RawGenerate),
    /// Pinned local package compilation request.
    CompilePackage(RawCompilePackage),
    /// Snapshot status request.
    #[serde(rename = "status")]
    SnapshotStatus(RawSnapshot),
    /// Lexical retrieval request.
    Search(RawSearch),
    /// Graph retrieval request.
    Graph(RawRetrieval),
    /// Vector retrieval request.
    Vector(RawRetrieval),
    /// Locality request.
    Locality(RawSnapshot),
    /// Journaled, idempotent removal of one local snapshot.
    RemoveIndex(RawSnapshot),
    /// Capability health request.
    Health(RawHealth),
    /// Local recovery policy request.
    RecoverLocal(RawPolicy),
    /// Inconsistent remote recovery policy request.
    RecoverInconsistent(RawInconsistentPolicy),
    /// Local release policy request.
    ReleaseLocal(RawPolicy),
    /// Execution polling request.
    PollExecution(RawOperation),
    /// Execution cancellation request.
    Cancel(RawOperation),
}
