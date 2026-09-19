//! Wire projection for bounded executable-capability health.

use super::{BasisWire, CoverageWire, CursorWire};
use backend_semantic::vocabulary::{LanguageProfile, NativeTool};
use serde::{Deserialize, Serialize};

use crate::{
    CapabilityAuthority, CapabilityFamily, CapabilityId, CapabilityInventory, CapabilityLifecycle,
    CapabilityStatus, CapabilityTarget, CapabilityUnavailable, EmbeddingCapabilityRecipe,
    EmbeddingEncoding, EmbeddingMetric, EmbeddingNormalization, EmbeddingPooling,
    EmbeddingRecipeId, EmbeddingSource, LanguageOracleTask, MAX_CAPABILITY_INVENTORY,
    FaultRows, IngestProgress, LanguageRows, PackageAuthorityIdentity, SourceLanguage,
    SourceUnavailableReason, decode_id, encode_id,
};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CapabilityInventoryWire {
    rows: Vec<CapabilityStatusWire>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct HealthWire {
    pub(crate) root: String,
    pub(crate) source: String,
    pub(crate) cursor: CursorWire,
    pub(crate) basis: BasisWire,
    pub(crate) coverage: Vec<CoverageWire>,
    pub(crate) row_count: u64,
    pub(crate) capabilities: CapabilityInventoryWire,
    /// Additive since the first health wire. A producer that publishes no
    /// ingest counts omits the field entirely and every reader admits the
    /// report with an empty [`IngestProgress`].
    #[serde(default)]
    pub(crate) progress: IngestProgressWire,
}

/// Wire projection of the owner's typed ingest counts.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct IngestProgressWire {
    #[serde(default)]
    pub(crate) files_discovered: u64,
    #[serde(default)]
    pub(crate) files_indexed: u64,
    #[serde(default)]
    pub(crate) files_unavailable: u64,
    #[serde(default)]
    pub(crate) declarations: u64,
    #[serde(default)]
    pub(crate) languages: Vec<LanguageRowsWire>,
    #[serde(default)]
    pub(crate) faults: Vec<FaultRowsWire>,
}

/// One language's file and declaration contribution on the wire.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LanguageRowsWire {
    pub(crate) language: LanguageWire,
    pub(crate) files: u64,
    pub(crate) declarations: u64,
}

/// One terminal's file count on the wire.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FaultRowsWire {
    pub(crate) reason: SourceFaultWire,
    pub(crate) files: u64,
}

/// Closed language token shared by every surface.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum LanguageWire {
    Rust,
    TypeScript,
    Python,
    Go,
    Java,
    CSharp,
    Clang,
}

/// Closed per-file terminal token shared by every surface.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum SourceFaultWire {
    Unreadable,
    NotText,
    TooLarge,
    Unparsed,
}

impl From<SourceLanguage> for LanguageWire {
    fn from(value: SourceLanguage) -> Self {
        match value {
            SourceLanguage::Rust => Self::Rust,
            SourceLanguage::TypeScript => Self::TypeScript,
            SourceLanguage::Python => Self::Python,
            SourceLanguage::Go => Self::Go,
            SourceLanguage::Java => Self::Java,
            SourceLanguage::CSharp => Self::CSharp,
            SourceLanguage::Clang => Self::Clang,
        }
    }
}

impl From<LanguageWire> for SourceLanguage {
    fn from(value: LanguageWire) -> Self {
        match value {
            LanguageWire::Rust => Self::Rust,
            LanguageWire::TypeScript => Self::TypeScript,
            LanguageWire::Python => Self::Python,
            LanguageWire::Go => Self::Go,
            LanguageWire::Java => Self::Java,
            LanguageWire::CSharp => Self::CSharp,
            LanguageWire::Clang => Self::Clang,
        }
    }
}

impl From<SourceUnavailableReason> for SourceFaultWire {
    fn from(value: SourceUnavailableReason) -> Self {
        match value {
            SourceUnavailableReason::Unreadable => Self::Unreadable,
            SourceUnavailableReason::NotText => Self::NotText,
            SourceUnavailableReason::TooLarge => Self::TooLarge,
            SourceUnavailableReason::Unparsed => Self::Unparsed,
        }
    }
}

impl From<SourceFaultWire> for SourceUnavailableReason {
    fn from(value: SourceFaultWire) -> Self {
        match value {
            SourceFaultWire::Unreadable => Self::Unreadable,
            SourceFaultWire::NotText => Self::NotText,
            SourceFaultWire::TooLarge => Self::TooLarge,
            SourceFaultWire::Unparsed => Self::Unparsed,
        }
    }
}

/// Projects the owner's ingest counts onto the wire.
pub(crate) fn progress_to_wire(progress: &IngestProgress) -> IngestProgressWire {
    IngestProgressWire {
        files_discovered: progress.files_discovered(),
        files_indexed: progress.files_indexed(),
        files_unavailable: progress.files_unavailable(),
        declarations: progress.declarations(),
        languages: progress
            .languages()
            .iter()
            .map(|row| LanguageRowsWire {
                language: row.language().into(),
                files: row.files(),
                declarations: row.declarations(),
            })
            .collect(),
        faults: progress
            .faults()
            .iter()
            .map(|row| FaultRowsWire {
                reason: row.reason().into(),
                files: row.files(),
            })
            .collect(),
    }
}

/// Admits one wire progress report.
///
/// The bounds and the accounting invariant are re-checked here rather than
/// trusted, because this value crosses a process boundary.
pub(crate) fn progress_from_wire(wire: IngestProgressWire) -> Result<IngestProgress, String> {
    let languages = wire
        .languages
        .into_iter()
        .map(|row| LanguageRows::new(row.language.into(), row.files, row.declarations))
        .collect::<Vec<_>>();
    let faults = wire
        .faults
        .into_iter()
        .map(|row| FaultRows::new(row.reason.into(), row.files))
        .collect::<Vec<_>>();
    IngestProgress::new(
        wire.files_discovered,
        wire.files_indexed,
        wire.files_unavailable,
        wire.declarations,
        languages,
        faults,
    )
    .map_err(|error| error.to_string())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CapabilityStatusWire {
    id: String,
    family: FamilyWire,
    manifest: Option<String>,
    target: Option<TargetWire>,
    protocol_abi: Option<u16>,
    authority: Option<AuthorityWire>,
    lifecycle: LifecycleWire,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum FamilyWire {
    StructuralFrontend { profile: String },
    LanguageOracle { profile: String, task: String },
    Embedding { recipe: Option<String> },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum AuthorityWire {
    Structural {
        producer: String,
    },
    Compiler {
        recipe: String,
        toolchain: u8,
        toolchain_identity: String,
        package_authority: PackageAuthorityWire,
    },
    Embedding {
        recipe: String,
        model: String,
        tokenizer: String,
        source: String,
        maximum_input_bytes: Option<u32>,
        maximum_tokens: Option<u32>,
        overlap_tokens: u16,
        dimensions: u32,
        metric: String,
        pooling: String,
        normalization: String,
        encoding: String,
        query_treatment: String,
        document_treatment: String,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "scope", rename_all = "snake_case", deny_unknown_fields)]
enum PackageAuthorityWire {
    ImmutableClosure { identity: String },
    LocalConfiguration { fingerprint: String },
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum TargetWire {
    Native { os: u8, architecture: u8 },
    Managed { runtime: u8 },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "state", content = "reason", rename_all = "snake_case")]
enum LifecycleWire {
    Unavailable(String),
    Probing,
    Installed,
    Resident,
    Active,
    Ready,
    Revoked,
}

pub(crate) fn inventory_to_wire(value: &CapabilityInventory) -> CapabilityInventoryWire {
    CapabilityInventoryWire {
        rows: value.as_slice().iter().map(to_wire).collect(),
    }
}

pub(crate) fn inventory_from_wire(
    value: CapabilityInventoryWire,
) -> Result<CapabilityInventory, String> {
    if value.rows.len() > MAX_CAPABILITY_INVENTORY {
        return Err("capability inventory exceeds its row bound".to_owned());
    }
    let rows = value
        .rows
        .into_iter()
        .map(from_wire)
        .collect::<Result<Vec<_>, _>>()?;
    CapabilityInventory::try_new(rows).map_err(|error| error.to_string())
}

fn to_wire(value: &CapabilityStatus) -> CapabilityStatusWire {
    CapabilityStatusWire {
        id: encode_id(&value.id().as_bytes()),
        family: family_to_wire(value.family()),
        manifest: value.manifest().map(|id| encode_id(&id)),
        target: value.target().map(target_to_wire),
        protocol_abi: value.protocol_abi(),
        authority: value.authority().map(authority_to_wire),
        lifecycle: lifecycle_to_wire(value.lifecycle()),
    }
}

fn from_wire(value: CapabilityStatusWire) -> Result<CapabilityStatus, String> {
    let id = CapabilityId::new(decode_id(&value.id).map_err(|error| error.to_string())?);
    let family = family_from_wire(value.family)?;
    let lifecycle = lifecycle_from_wire(value.lifecycle)?;
    match (
        value.manifest,
        value.target,
        value.protocol_abi,
        value.authority,
        lifecycle,
    ) {
        (None, None, None, None, CapabilityLifecycle::Unavailable(reason)) => {
            Ok(CapabilityStatus::unavailable(id, family, reason))
        }
        (None, None, None, None, CapabilityLifecycle::Probing) => {
            Ok(CapabilityStatus::probing(id, family))
        }
        (Some(manifest), Some(target), Some(abi), Some(authority), state) => {
            Ok(CapabilityStatus::observed(
                id,
                family,
                decode_id(&manifest).map_err(|error| error.to_string())?,
                target_from_wire(target),
                abi,
                authority_from_wire(authority)?,
                state,
            ))
        }
        _ => Err("incoherent capability manifest, target, ABI, and lifecycle facts".to_owned()),
    }
}

fn family_to_wire(value: CapabilityFamily) -> FamilyWire {
    match value {
        CapabilityFamily::StructuralFrontend { profile } => FamilyWire::StructuralFrontend {
            profile: profile_name(profile).to_owned(),
        },
        CapabilityFamily::LanguageOracle { profile, task } => FamilyWire::LanguageOracle {
            profile: profile_name(profile).to_owned(),
            task: task_name(task).to_owned(),
        },
        CapabilityFamily::Embedding { recipe } => FamilyWire::Embedding {
            recipe: recipe.map(|id| encode_id(&id.as_bytes())),
        },
    }
}

fn family_from_wire(value: FamilyWire) -> Result<CapabilityFamily, String> {
    match value {
        FamilyWire::StructuralFrontend { profile } => Ok(CapabilityFamily::StructuralFrontend {
            profile: LanguageProfile::try_from(profile.as_str())
                .map_err(|_| "unknown structural frontend profile".to_owned())?,
        }),
        FamilyWire::LanguageOracle { profile, task } => Ok(CapabilityFamily::LanguageOracle {
            profile: LanguageProfile::try_from(profile.as_str())
                .map_err(|_| "unknown language-oracle profile".to_owned())?,
            task: task_from_name(&task)?,
        }),
        FamilyWire::Embedding { recipe } => Ok(CapabilityFamily::Embedding {
            recipe: recipe
                .map(|id| {
                    decode_id(&id)
                        .map(EmbeddingRecipeId::new)
                        .map_err(|error| error.to_string())
                })
                .transpose()?,
        }),
    }
}

fn authority_to_wire(value: CapabilityAuthority) -> AuthorityWire {
    match value {
        CapabilityAuthority::Structural { producer } => AuthorityWire::Structural {
            producer: encode_id(&producer),
        },
        CapabilityAuthority::Compiler {
            recipe,
            toolchain,
            toolchain_identity,
            package_authority,
        } => AuthorityWire::Compiler {
            recipe: encode_id(&recipe),
            toolchain: toolchain.into(),
            toolchain_identity: encode_id(&toolchain_identity),
            package_authority: match package_authority {
                PackageAuthorityIdentity::ImmutableClosure(identity) => {
                    PackageAuthorityWire::ImmutableClosure {
                        identity: encode_id(&identity),
                    }
                }
                PackageAuthorityIdentity::LocalConfiguration(fingerprint) => {
                    PackageAuthorityWire::LocalConfiguration {
                        fingerprint: encode_id(&fingerprint),
                    }
                }
            },
        },
        CapabilityAuthority::Embedding(recipe) => AuthorityWire::Embedding {
            recipe: encode_id(&recipe.identity().as_bytes()),
            model: encode_id(&recipe.model),
            tokenizer: encode_id(&recipe.tokenizer),
            source: embedding_source_name(recipe.source).to_owned(),
            maximum_input_bytes: recipe.maximum_input_bytes,
            maximum_tokens: recipe.maximum_tokens,
            overlap_tokens: recipe.overlap_tokens,
            dimensions: recipe.dimensions,
            metric: embedding_metric_name(recipe.metric).to_owned(),
            pooling: embedding_pooling_name(recipe.pooling).to_owned(),
            normalization: embedding_normalization_name(recipe.normalization).to_owned(),
            encoding: embedding_encoding_name(recipe.encoding).to_owned(),
            query_treatment: encode_id(&recipe.query_treatment),
            document_treatment: encode_id(&recipe.document_treatment),
        },
    }
}

fn authority_from_wire(value: AuthorityWire) -> Result<CapabilityAuthority, String> {
    Ok(match value {
        AuthorityWire::Structural { producer } => CapabilityAuthority::Structural {
            producer: decode_id(&producer).map_err(|error| error.to_string())?,
        },
        AuthorityWire::Compiler {
            recipe,
            toolchain,
            toolchain_identity,
            package_authority,
        } => CapabilityAuthority::Compiler {
            recipe: decode_id(&recipe).map_err(|error| error.to_string())?,
            toolchain: NativeTool::try_from(toolchain)
                .map_err(|_| "unknown compiler capability toolchain".to_owned())?,
            toolchain_identity: decode_id(&toolchain_identity)
                .map_err(|error| error.to_string())?,
            package_authority: match package_authority {
                PackageAuthorityWire::ImmutableClosure { identity } => {
                    PackageAuthorityIdentity::ImmutableClosure(
                        decode_id(&identity).map_err(|error| error.to_string())?,
                    )
                }
                PackageAuthorityWire::LocalConfiguration { fingerprint } => {
                    PackageAuthorityIdentity::LocalConfiguration(
                        decode_id(&fingerprint).map_err(|error| error.to_string())?,
                    )
                }
            },
        },
        AuthorityWire::Embedding {
            recipe,
            model,
            tokenizer,
            source,
            maximum_input_bytes,
            maximum_tokens,
            overlap_tokens,
            dimensions,
            metric,
            pooling,
            normalization,
            encoding,
            query_treatment,
            document_treatment,
        } => {
            let claimed_identity =
                EmbeddingRecipeId::new(decode_id(&recipe).map_err(|error| error.to_string())?);
            let canonical = EmbeddingCapabilityRecipe::new(
                decode_id(&model).map_err(|error| error.to_string())?,
                decode_id(&tokenizer).map_err(|error| error.to_string())?,
                embedding_source_from_name(&source)?,
                maximum_input_bytes,
                maximum_tokens,
                overlap_tokens,
                dimensions,
                embedding_metric_from_name(&metric)?,
                embedding_pooling_from_name(&pooling)?,
                embedding_normalization_from_name(&normalization)?,
                embedding_encoding_from_name(&encoding)?,
                decode_id(&query_treatment).map_err(|error| error.to_string())?,
                decode_id(&document_treatment).map_err(|error| error.to_string())?,
            );
            CapabilityAuthority::Embedding(canonical.with_claimed_identity(claimed_identity))
        }
    })
}

const fn target_to_wire(value: CapabilityTarget) -> TargetWire {
    match value {
        CapabilityTarget::Native { os, architecture } => TargetWire::Native { os, architecture },
        CapabilityTarget::Managed(runtime) => TargetWire::Managed { runtime },
    }
}
const fn target_from_wire(value: TargetWire) -> CapabilityTarget {
    match value {
        TargetWire::Native { os, architecture } => CapabilityTarget::Native { os, architecture },
        TargetWire::Managed { runtime } => CapabilityTarget::Managed(runtime),
    }
}

fn lifecycle_to_wire(value: CapabilityLifecycle) -> LifecycleWire {
    match value {
        CapabilityLifecycle::Unavailable(reason) => {
            LifecycleWire::Unavailable(unavailable_name(reason).to_owned())
        }
        CapabilityLifecycle::Probing => LifecycleWire::Probing,
        CapabilityLifecycle::Installed => LifecycleWire::Installed,
        CapabilityLifecycle::Resident => LifecycleWire::Resident,
        CapabilityLifecycle::Active => LifecycleWire::Active,
        CapabilityLifecycle::Ready => LifecycleWire::Ready,
        CapabilityLifecycle::Revoked => LifecycleWire::Revoked,
    }
}
fn lifecycle_from_wire(value: LifecycleWire) -> Result<CapabilityLifecycle, String> {
    Ok(match value {
        LifecycleWire::Unavailable(reason) => {
            CapabilityLifecycle::Unavailable(unavailable_from_name(&reason)?)
        }
        LifecycleWire::Probing => CapabilityLifecycle::Probing,
        LifecycleWire::Installed => CapabilityLifecycle::Installed,
        LifecycleWire::Resident => CapabilityLifecycle::Resident,
        LifecycleWire::Active => CapabilityLifecycle::Active,
        LifecycleWire::Ready => CapabilityLifecycle::Ready,
        LifecycleWire::Revoked => CapabilityLifecycle::Revoked,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn recipe() -> EmbeddingCapabilityRecipe {
        EmbeddingCapabilityRecipe::new(
            [10; 32],
            [11; 32],
            EmbeddingSource::SemanticEvidence,
            Some(4096),
            Some(128),
            16,
            384,
            EmbeddingMetric::Cosine,
            EmbeddingPooling::Mean,
            EmbeddingNormalization::UnitL2,
            EmbeddingEncoding::Float32,
            [12; 32],
            [13; 32],
        )
    }

    fn inventory() -> CapabilityInventory {
        let recipe = recipe();
        let identity = recipe.identity();
        CapabilityInventory::try_new(vec![CapabilityStatus::observed(
            CapabilityId::new([1; 32]),
            CapabilityFamily::Embedding {
                recipe: Some(identity),
            },
            [2; 32],
            CapabilityTarget::Managed(1),
            1,
            CapabilityAuthority::Embedding(recipe),
            CapabilityLifecycle::Ready,
        )])
        .expect("canonical embedding inventory")
    }

    #[test]
    fn embedding_capability_wire_round_trip_preserves_canonical_recipe() {
        let expected = inventory();
        let wire = inventory_to_wire(&expected);
        assert_eq!(
            inventory_from_wire(wire).expect("decode inventory"),
            expected
        );
    }

    #[test]
    fn embedding_capability_wire_rejects_a_forged_claimed_identity() {
        let mut wire = inventory_to_wire(&inventory());
        let Some(AuthorityWire::Embedding { recipe, .. }) = wire.rows[0].authority.as_mut() else {
            panic!("embedding authority");
        };
        *recipe = encode_id(&[0xaa; 32]);
        let error = inventory_from_wire(wire).expect_err("forged recipe identity");
        assert!(error.contains("Authority"), "unexpected error: {error}");
    }
}

fn profile_name(value: LanguageProfile) -> &'static str {
    match <[u8; 2]>::from(value) {
        [0, 0] => "rust-2015",
        [0, 1] => "rust-2018",
        [0, 2] => "rust-2021",
        [0, 3] => "rust-2024",
        [1, 0] => "typescript",
        [1, 1] => "tsx",
        [2, 0] => "python-3.10",
        [2, 1] => "python-3.11",
        [2, 2] => "python-3.12",
        [2, 3] => "python-3.13",
        [2, 4] => "python-3.14",
        [3, 0] => "go-1.22",
        [3, 1] => "go-1.23",
        [3, 2] => "go-1.24",
        [3, 3] => "go-1.25",
        [4, 0] => "java-8",
        [4, 1] => "java-11",
        [4, 2] => "java-17",
        [4, 3] => "java-21",
        [4, 4] => "java-25",
        [5, 0] => "csharp-10",
        [5, 1] => "csharp-11",
        [5, 2] => "csharp-12",
        [5, 3] => "csharp-13",
        [5, 4] => "csharp-14",
        [6, 0] => "c-11",
        [6, 1] => "c-17",
        [6, 2] => "c-23",
        [6, 128] => "cxx-17",
        [6, 129] => "cxx-20",
        [6, 130] => "cxx-23",
        [6, 131] => "cxx-26",
        _ => unreachable!("closed language profile"),
    }
}
const fn task_name(value: LanguageOracleTask) -> &'static str {
    match value {
        LanguageOracleTask::Parse => "parse",
        LanguageOracleTask::TypeCheck => "type_check",
        LanguageOracleTask::SemanticIndex => "semantic_index",
    }
}
fn task_from_name(value: &str) -> Result<LanguageOracleTask, String> {
    match value {
        "parse" => Ok(LanguageOracleTask::Parse),
        "type_check" => Ok(LanguageOracleTask::TypeCheck),
        "semantic_index" => Ok(LanguageOracleTask::SemanticIndex),
        _ => Err("unknown language-oracle task".to_owned()),
    }
}

const fn embedding_source_name(value: EmbeddingSource) -> &'static str {
    match value {
        EmbeddingSource::RawUtf8 => "raw_utf8",
        EmbeddingSource::SemanticDeclaration => "semantic_declaration",
        EmbeddingSource::Documentation => "documentation",
        EmbeddingSource::SemanticEvidence => "semantic_evidence",
    }
}

fn embedding_source_from_name(value: &str) -> Result<EmbeddingSource, String> {
    match value {
        "raw_utf8" => Ok(EmbeddingSource::RawUtf8),
        "semantic_declaration" => Ok(EmbeddingSource::SemanticDeclaration),
        "documentation" => Ok(EmbeddingSource::Documentation),
        "semantic_evidence" => Ok(EmbeddingSource::SemanticEvidence),
        _ => Err("unknown embedding source projection".to_owned()),
    }
}

const fn embedding_metric_name(value: EmbeddingMetric) -> &'static str {
    match value {
        EmbeddingMetric::Cosine => "cosine",
        EmbeddingMetric::Dot => "dot",
        EmbeddingMetric::Euclidean => "euclidean",
    }
}

fn embedding_metric_from_name(value: &str) -> Result<EmbeddingMetric, String> {
    match value {
        "cosine" => Ok(EmbeddingMetric::Cosine),
        "dot" => Ok(EmbeddingMetric::Dot),
        "euclidean" => Ok(EmbeddingMetric::Euclidean),
        _ => Err("unknown embedding metric".to_owned()),
    }
}

const fn embedding_pooling_name(value: EmbeddingPooling) -> &'static str {
    match value {
        EmbeddingPooling::Cls => "cls",
        EmbeddingPooling::Mean => "mean",
        EmbeddingPooling::LastToken => "last_token",
    }
}

fn embedding_pooling_from_name(value: &str) -> Result<EmbeddingPooling, String> {
    match value {
        "cls" => Ok(EmbeddingPooling::Cls),
        "mean" => Ok(EmbeddingPooling::Mean),
        "last_token" => Ok(EmbeddingPooling::LastToken),
        _ => Err("unknown embedding pooling".to_owned()),
    }
}

const fn embedding_normalization_name(value: EmbeddingNormalization) -> &'static str {
    match value {
        EmbeddingNormalization::None => "none",
        EmbeddingNormalization::UnitL2 => "unit_l2",
    }
}

fn embedding_normalization_from_name(value: &str) -> Result<EmbeddingNormalization, String> {
    match value {
        "none" => Ok(EmbeddingNormalization::None),
        "unit_l2" => Ok(EmbeddingNormalization::UnitL2),
        _ => Err("unknown embedding normalization".to_owned()),
    }
}

const fn embedding_encoding_name(value: EmbeddingEncoding) -> &'static str {
    match value {
        EmbeddingEncoding::Float32 => "float32",
        EmbeddingEncoding::Float16 => "float16",
        EmbeddingEncoding::Signed8 => "signed8",
    }
}

fn embedding_encoding_from_name(value: &str) -> Result<EmbeddingEncoding, String> {
    match value {
        "float32" => Ok(EmbeddingEncoding::Float32),
        "float16" => Ok(EmbeddingEncoding::Float16),
        "signed8" => Ok(EmbeddingEncoding::Signed8),
        _ => Err("unknown embedding encoding".to_owned()),
    }
}

const fn unavailable_name(value: CapabilityUnavailable) -> &'static str {
    match value {
        CapabilityUnavailable::NoManifest => "no_manifest",
        CapabilityUnavailable::NotInstalled => "not_installed",
        CapabilityUnavailable::UnsupportedTarget => "unsupported_target",
        CapabilityUnavailable::UnsupportedAbi => "unsupported_abi",
        CapabilityUnavailable::MissingDependency => "missing_dependency",
        CapabilityUnavailable::ProbeFailed => "probe_failed",
    }
}
fn unavailable_from_name(value: &str) -> Result<CapabilityUnavailable, String> {
    match value {
        "no_manifest" => Ok(CapabilityUnavailable::NoManifest),
        "not_installed" => Ok(CapabilityUnavailable::NotInstalled),
        "unsupported_target" => Ok(CapabilityUnavailable::UnsupportedTarget),
        "unsupported_abi" => Ok(CapabilityUnavailable::UnsupportedAbi),
        "missing_dependency" => Ok(CapabilityUnavailable::MissingDependency),
        "probe_failed" => Ok(CapabilityUnavailable::ProbeFailed),
        _ => Err("unknown capability unavailability reason".to_owned()),
    }
}
