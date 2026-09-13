//! Bounded client projection of executable capability state.

pub use compiler_vocabulary::LanguageOracleTask;
use compiler_vocabulary::{LanguageProfile, NativeTool};

/// Maximum capability rows admitted in one health response.
pub const MAX_CAPABILITY_INVENTORY: usize = 64;

/// Stable logical identity of one advertised capability slot.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct CapabilityId([u8; 32]);

impl CapabilityId {
    /// Admits an identity already checked by the producing service.
    #[must_use]
    pub const fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Returns the complete fixed-width identity.
    #[must_use]
    pub const fn as_bytes(self) -> [u8; 32] {
        self.0
    }

    fn for_label(label: &[u8]) -> Self {
        Self(*blake3::hash(label).as_bytes())
    }
}

/// Identity of every compatibility-relevant embedding recipe field.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct EmbeddingRecipeId([u8; 32]);

impl EmbeddingRecipeId {
    /// Admits a recipe identity from a verified capability manifest.
    #[must_use]
    pub const fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Returns the complete fixed-width identity.
    #[must_use]
    pub const fn as_bytes(self) -> [u8; 32] {
        self.0
    }
}

/// Capability family and its compatibility identity.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum CapabilityFamily {
    /// One syntax-only frontend, explicitly separated from semantic authority.
    StructuralFrontend {
        /// Exact source grammar/profile accepted by the frontend.
        profile: LanguageProfile,
    },
    /// One profile-specific compiler semantic oracle.
    LanguageOracle {
        /// Exact language profile whose meaning is supplied by the oracle.
        profile: LanguageProfile,
        /// Semantic operation supplied by the oracle.
        task: LanguageOracleTask,
    },
    /// Embedding execution, optionally naming the exact recipe when configured.
    Embedding {
        /// Exact vector-space recipe, absent when no model is configured.
        recipe: Option<EmbeddingRecipeId>,
    },
}

/// Source projection supplied to an embedding producer.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum EmbeddingSource {
    /// Raw UTF-8 source bytes.
    RawUtf8,
    /// One canonical compiler declaration.
    SemanticDeclaration,
    /// Canonical documentation text.
    Documentation,
    /// One row from the admitted compiler/structural semantic evidence corpus.
    SemanticEvidence,
}

/// Model pooling operation.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum EmbeddingPooling {
    /// First/CLS token.
    Cls,
    /// Mean of admitted token vectors.
    Mean,
    /// Last token.
    LastToken,
}

/// Vector normalization rule.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum EmbeddingNormalization {
    /// Preserve model output.
    None,
    /// Normalize to unit L2 length.
    UnitL2,
}

/// Coordinate representation emitted by inference.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum EmbeddingEncoding {
    /// IEEE 754 binary32.
    Float32,
    /// IEEE 754 binary16.
    Float16,
    /// Signed quantized byte with recipe-owned scale semantics.
    Signed8,
}

/// Metric belonging to the embedding space.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum EmbeddingMetric {
    /// Cosine distance/similarity.
    Cosine,
    /// Dot product.
    Dot,
    /// Euclidean distance.
    Euclidean,
}

/// Full vector-space recipe retained by capability health.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct EmbeddingCapabilityRecipe {
    /// Identity of every compatibility-relevant field below.
    identity: EmbeddingRecipeId,
    /// Verified model artifact identity.
    pub model: [u8; 32],
    /// Verified tokenizer artifact identity.
    pub tokenizer: [u8; 32],
    /// Typed input projection.
    pub source: EmbeddingSource,
    /// Maximum UTF-8 input bytes admitted per embedding invocation, when byte-bounded.
    pub maximum_input_bytes: Option<u32>,
    /// Maximum tokenizer output admitted per chunk, when token-bounded.
    pub maximum_tokens: Option<u32>,
    /// Tokens repeated between adjacent chunks.
    pub overlap_tokens: u16,
    /// Number of emitted coordinates.
    pub dimensions: u32,
    /// Search metric.
    pub metric: EmbeddingMetric,
    /// Model pooling rule.
    pub pooling: EmbeddingPooling,
    /// Coordinate normalization rule.
    pub normalization: EmbeddingNormalization,
    /// Coordinate representation.
    pub encoding: EmbeddingEncoding,
    /// Exact query-side treatment identity.
    pub query_treatment: [u8; 32],
    /// Exact document-side treatment identity.
    pub document_treatment: [u8; 32],
}

impl EmbeddingCapabilityRecipe {
    /// Constructs a recipe whose identity is derived from every compatibility
    /// relevant field.  Producers should use this constructor instead of
    /// accepting an independently supplied identity.
    #[allow(
        clippy::too_many_arguments,
        reason = "each vector-space compatibility fact is independently meaningful"
    )]
    #[must_use]
    pub fn new(
        model: [u8; 32],
        tokenizer: [u8; 32],
        source: EmbeddingSource,
        maximum_input_bytes: Option<u32>,
        maximum_tokens: Option<u32>,
        overlap_tokens: u16,
        dimensions: u32,
        metric: EmbeddingMetric,
        pooling: EmbeddingPooling,
        normalization: EmbeddingNormalization,
        encoding: EmbeddingEncoding,
        query_treatment: [u8; 32],
        document_treatment: [u8; 32],
    ) -> Self {
        let mut recipe = Self {
            identity: EmbeddingRecipeId::new([0; 32]),
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
        };
        recipe.identity = embedding_authority_recipe(&recipe);
        recipe
    }

    /// Recomputes the canonical identity from the recipe fields.
    #[must_use]
    pub fn recomputed_identity(&self) -> EmbeddingRecipeId {
        embedding_authority_recipe(self)
    }

    /// Returns the identity claimed by this recipe.
    #[must_use]
    pub const fn identity(&self) -> EmbeddingRecipeId {
        self.identity
    }

    /// Replaces the identity with an untrusted wire claim for admission
    /// verification.  Callers must compare [`Self::recomputed_identity`]
    /// before accepting the resulting recipe.
    #[must_use]
    pub(crate) fn with_claimed_identity(mut self, claimed_identity: EmbeddingRecipeId) -> Self {
        self.identity = claimed_identity;
        self
    }
}

/// Derives the canonical identity of an embedding authority claim.
///
/// The identity commits to the model and tokenizer artifacts, input source and
/// bounds, chunk overlap, coordinate shape and semantics, and both treatment
/// identities.  Optional bounds carry an explicit presence tag so a missing
/// bound cannot alias a zero bound.
#[must_use]
pub fn embedding_authority_recipe(value: &EmbeddingCapabilityRecipe) -> EmbeddingRecipeId {
    let mut canonical = blake3::Hasher::new();
    canonical.update(b"backend.embedding-authority-recipe.v1\0");
    canonical.update(&value.model);
    canonical.update(&value.tokenizer);
    canonical.update(&[embedding_source_code(value.source)]);
    encode_optional_u32(value.maximum_input_bytes, &mut canonical);
    encode_optional_u32(value.maximum_tokens, &mut canonical);
    canonical.update(&value.overlap_tokens.to_be_bytes());
    canonical.update(&value.dimensions.to_be_bytes());
    canonical.update(&[
        embedding_metric_code(value.metric),
        embedding_pooling_code(value.pooling),
        embedding_normalization_code(value.normalization),
        embedding_encoding_code(value.encoding),
    ]);
    canonical.update(&value.query_treatment);
    canonical.update(&value.document_treatment);
    EmbeddingRecipeId::new(*canonical.finalize().as_bytes())
}

fn encode_optional_u32(value: Option<u32>, output: &mut blake3::Hasher) {
    match value {
        Some(value) => {
            output.update(&[1]);
            output.update(&value.to_be_bytes());
        }
        None => {
            output.update(&[0]);
        }
    }
}

const fn embedding_source_code(value: EmbeddingSource) -> u8 {
    match value {
        EmbeddingSource::RawUtf8 => 0,
        EmbeddingSource::SemanticDeclaration => 1,
        EmbeddingSource::Documentation => 2,
        EmbeddingSource::SemanticEvidence => 3,
    }
}

const fn embedding_metric_code(value: EmbeddingMetric) -> u8 {
    match value {
        EmbeddingMetric::Cosine => 0,
        EmbeddingMetric::Dot => 1,
        EmbeddingMetric::Euclidean => 2,
    }
}

const fn embedding_pooling_code(value: EmbeddingPooling) -> u8 {
    match value {
        EmbeddingPooling::Cls => 0,
        EmbeddingPooling::Mean => 1,
        EmbeddingPooling::LastToken => 2,
    }
}

const fn embedding_normalization_code(value: EmbeddingNormalization) -> u8 {
    match value {
        EmbeddingNormalization::None => 0,
        EmbeddingNormalization::UnitL2 => 1,
    }
}

const fn embedding_encoding_code(value: EmbeddingEncoding) -> u8 {
    match value {
        EmbeddingEncoding::Float32 => 0,
        EmbeddingEncoding::Float16 => 1,
        EmbeddingEncoding::Signed8 => 2,
    }
}

/// Scope of the package-authority commitment advertised by a compiler lane.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum PackageAuthorityIdentity {
    /// Identity of an immutable helper/resolver artifact closure and policy.
    ImmutableClosure([u8; 32]),
    /// Host-local configuration fingerprint. It proves local drift but is not
    /// a cross-host equivalence witness.
    LocalConfiguration([u8; 32]),
}

impl PackageAuthorityIdentity {
    /// Returns the fixed-width commitment.
    #[must_use]
    pub const fn as_bytes(self) -> [u8; 32] {
        match self {
            Self::ImmutableClosure(value) | Self::LocalConfiguration(value) => value,
        }
    }

    /// True only for an immutable closure suitable for cross-host comparison.
    #[must_use]
    pub const fn is_portable(self) -> bool {
        matches!(self, Self::ImmutableClosure(_))
    }
}

/// Derives the canonical identity of one compiler semantic operation from
/// every compatibility and authority fact advertised by its capability row.
#[must_use]
pub fn compiler_authority_recipe(
    manifest: [u8; 32],
    profile: LanguageProfile,
    task: LanguageOracleTask,
    protocol_abi: u16,
    toolchain: NativeTool,
    toolchain_identity: [u8; 32],
    package_authority: PackageAuthorityIdentity,
) -> [u8; 32] {
    let mut recipe = blake3::Hasher::new();
    recipe.update(b"backend.compiler-capability-recipe.v1\0");
    recipe.update(&manifest);
    recipe.update(&<[u8; 2]>::from(profile));
    recipe.update(&[task as u8, u8::from(toolchain)]);
    recipe.update(&protocol_abi.to_be_bytes());
    recipe.update(&toolchain_identity);
    recipe.update(&[u8::from(package_authority.is_portable())]);
    recipe.update(&package_authority.as_bytes());
    *recipe.finalize().as_bytes()
}

/// Evidence owned by the producer of one executable capability row.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum CapabilityAuthority {
    /// A syntax frontend whose producer identity can only authorize structural fallback facts.
    Structural {
        /// Exact frontend producer/version identity.
        producer: [u8; 32],
    },
    /// A compiler-owned semantic lane and all of its authority commitments.
    Compiler {
        /// Exact profile/task recipe identity.
        recipe: [u8; 32],
        /// Closed native tool family selected by the compiler owner.
        toolchain: NativeTool,
        /// Identity of the bounded native tool version observation.
        toolchain_identity: [u8; 32],
        /// Identity of the exact package-authority configuration.
        package_authority: PackageAuthorityIdentity,
    },
    /// Verified embedding artifacts and the complete vector-space recipe.
    Embedding(EmbeddingCapabilityRecipe),
}

/// Runtime target admitted by the capability owner.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum CapabilityTarget {
    /// Native executable bound to one OS and CPU architecture.
    Native {
        /// Stable operating-system code.
        os: u8,
        /// Stable architecture code.
        architecture: u8,
    },
    /// Portable executable/model bound to one managed runtime code.
    Managed(u8),
}

/// Honest lifecycle state projected by the capability owner.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum CapabilityLifecycle {
    /// Declared but unavailable, with an explicit stable reason.
    Unavailable(CapabilityUnavailable),
    /// An admitted authority is running its bounded identity/readiness probe.
    Probing,
    /// Verified immutable bytes are installed.
    Installed,
    /// Installed bytes have a live residence lease.
    Resident,
    /// A runtime owns the resident bytes but has no fresh readiness proof.
    Active,
    /// A fresh bounded runtime probe succeeded.
    Ready,
    /// Runtime authority was explicitly revoked.
    Revoked,
}

/// Stable reason a declared capability cannot execute.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum CapabilityUnavailable {
    /// No authenticated manifest is configured.
    NoManifest,
    /// Artifact acquisition has not completed.
    NotInstalled,
    /// The authenticated target is unsupported on this host.
    UnsupportedTarget,
    /// The declared protocol ABI is unsupported.
    UnsupportedAbi,
    /// A required model, tokenizer, runtime, or toolchain dependency is missing.
    MissingDependency,
    /// The live readiness probe failed.
    ProbeFailed,
}

/// One bounded, typed capability health row.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct CapabilityStatus {
    id: CapabilityId,
    family: CapabilityFamily,
    manifest: Option<[u8; 32]>,
    target: Option<CapabilityTarget>,
    protocol_abi: Option<u16>,
    authority: Option<CapabilityAuthority>,
    lifecycle: CapabilityLifecycle,
}

impl CapabilityStatus {
    fn has_coherent_authority(&self) -> bool {
        let absent = self.manifest.is_none()
            && self.target.is_none()
            && self.protocol_abi.is_none()
            && self.authority.is_none();
        if matches!(
            self.lifecycle,
            CapabilityLifecycle::Unavailable(_) | CapabilityLifecycle::Probing
        ) {
            return absent;
        }
        let (Some(manifest), Some(_target), Some(protocol_abi)) =
            (self.manifest, self.target, self.protocol_abi)
        else {
            return false;
        };
        match (self.family, self.authority) {
            (
                CapabilityFamily::StructuralFrontend { .. },
                Some(CapabilityAuthority::Structural { producer }),
            ) => manifest == producer,
            (
                CapabilityFamily::LanguageOracle { profile, task },
                Some(CapabilityAuthority::Compiler {
                    recipe,
                    toolchain,
                    toolchain_identity,
                    package_authority,
                }),
            ) => {
                toolchain == profile.language().native_tool()
                    && recipe
                        == compiler_authority_recipe(
                            manifest,
                            profile,
                            task,
                            protocol_abi,
                            toolchain,
                            toolchain_identity,
                            package_authority,
                        )
            }
            (
                CapabilityFamily::Embedding {
                    recipe: Some(expected),
                },
                Some(CapabilityAuthority::Embedding(observed)),
            ) => {
                expected == observed.identity()
                    && observed.identity() == observed.recomputed_identity()
                    && observed.dimensions != 0
                    && (observed.maximum_input_bytes.is_some_and(|bound| bound != 0)
                        || observed.maximum_tokens.is_some_and(|bound| bound != 0))
                    && observed
                        .maximum_tokens
                        .is_none_or(|bound| u32::from(observed.overlap_tokens) < bound)
            }
            (CapabilityFamily::Embedding { recipe: None }, _) | (_, None) | (_, Some(_)) => false,
        }
    }

    /// Constructs an explicitly unavailable declaration without inventing manifest facts.
    #[must_use]
    pub const fn unavailable(
        id: CapabilityId,
        family: CapabilityFamily,
        reason: CapabilityUnavailable,
    ) -> Self {
        Self {
            id,
            family,
            manifest: None,
            target: None,
            protocol_abi: None,
            authority: None,
            lifecycle: CapabilityLifecycle::Unavailable(reason),
        }
    }

    /// Constructs a manifest-free bounded probe observation.
    #[must_use]
    pub const fn probing(id: CapabilityId, family: CapabilityFamily) -> Self {
        Self {
            id,
            family,
            manifest: None,
            target: None,
            protocol_abi: None,
            authority: None,
            lifecycle: CapabilityLifecycle::Probing,
        }
    }

    /// Constructs a manifest-backed lifecycle observation.
    #[must_use]
    pub const fn observed(
        id: CapabilityId,
        family: CapabilityFamily,
        manifest: [u8; 32],
        target: CapabilityTarget,
        protocol_abi: u16,
        authority: CapabilityAuthority,
        lifecycle: CapabilityLifecycle,
    ) -> Self {
        Self {
            id,
            family,
            manifest: Some(manifest),
            target: Some(target),
            protocol_abi: Some(protocol_abi),
            authority: Some(authority),
            lifecycle,
        }
    }

    /// Logical capability identity.
    #[must_use]
    pub const fn id(&self) -> CapabilityId {
        self.id
    }
    /// Capability family and recipe slot.
    #[must_use]
    pub const fn family(&self) -> CapabilityFamily {
        self.family
    }
    /// Authenticated manifest identity, when admitted.
    #[must_use]
    pub const fn manifest(&self) -> Option<[u8; 32]> {
        self.manifest
    }
    /// Admitted target, when a manifest is available.
    #[must_use]
    pub const fn target(&self) -> Option<CapabilityTarget> {
        self.target
    }
    /// Admitted protocol ABI, when a manifest is available.
    #[must_use]
    pub const fn protocol_abi(&self) -> Option<u16> {
        self.protocol_abi
    }
    /// Typed producer authority, available only after manifest admission.
    #[must_use]
    pub const fn authority(&self) -> Option<CapabilityAuthority> {
        self.authority
    }
    /// Current honest lifecycle observation.
    #[must_use]
    pub const fn lifecycle(&self) -> CapabilityLifecycle {
        self.lifecycle
    }
}

/// Bounded, sorted, duplicate-free capability inventory.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CapabilityInventory(Box<[CapabilityStatus]>);

impl CapabilityInventory {
    /// Admits a bounded inventory after checking canonical identity order.
    ///
    /// # Errors
    ///
    /// Returns [`CapabilityInventoryError`] for overflow, duplicate, or unsorted rows.
    pub fn try_new(rows: Vec<CapabilityStatus>) -> Result<Self, CapabilityInventoryError> {
        if rows.len() > MAX_CAPABILITY_INVENTORY {
            return Err(CapabilityInventoryError::TooMany);
        }
        if rows.windows(2).any(|pair| pair[0].id >= pair[1].id) {
            return Err(CapabilityInventoryError::Order);
        }
        if rows.windows(2).any(|pair| pair[0].family == pair[1].family) {
            return Err(CapabilityInventoryError::DuplicateFamily);
        }
        if rows.iter().any(|row| !row.has_coherent_authority()) {
            return Err(CapabilityInventoryError::Authority);
        }
        Ok(Self(rows.into_boxed_slice()))
    }

    /// Returns every admitted status row.
    #[must_use]
    pub fn as_slice(&self) -> &[CapabilityStatus] {
        &self.0
    }

    /// Declares all built-in language oracle slots and embedding execution as unavailable.
    ///
    /// This is the honest baseline until the engine supplies manifest-backed observations.
    #[must_use]
    pub fn explicitly_unavailable() -> Self {
        let tasks = [
            LanguageOracleTask::TypeCheck,
            LanguageOracleTask::SemanticIndex,
        ];
        let mut rows =
            Vec::with_capacity(LanguageProfile::PRODUCT_PROFILES.len() * (tasks.len() + 1) + 1);
        for profile in LanguageProfile::PRODUCT_PROFILES {
            let profile_bytes = <[u8; 2]>::from(profile);
            let label = format!("structural-frontend/{profile:?}");
            rows.push(CapabilityStatus::unavailable(
                CapabilityId::for_label(label.as_bytes()),
                CapabilityFamily::StructuralFrontend { profile },
                CapabilityUnavailable::NoManifest,
            ));
            for task in tasks {
                let family = CapabilityFamily::LanguageOracle { profile, task };
                let label = format!(
                    "language-oracle/{:02x}{:02x}/{task:?}",
                    profile_bytes[0], profile_bytes[1]
                );
                rows.push(CapabilityStatus::unavailable(
                    CapabilityId::for_label(label.as_bytes()),
                    family,
                    CapabilityUnavailable::NoManifest,
                ));
            }
        }
        rows.push(CapabilityStatus::unavailable(
            CapabilityId::for_label(b"embedding/default"),
            CapabilityFamily::Embedding { recipe: None },
            CapabilityUnavailable::NoManifest,
        ));
        rows.sort_unstable_by_key(CapabilityStatus::id);
        Self(rows.into_boxed_slice())
    }
}

/// Inventory admission failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CapabilityInventoryError {
    /// Row count exceeds [`MAX_CAPABILITY_INVENTORY`].
    TooMany,
    /// Rows are duplicated or not strictly sorted by identity.
    Order,
    /// Two rows advertise the same logical family slot.
    DuplicateFamily,
    /// A family, lifecycle, and producer-authority payload disagree.
    Authority,
}

impl std::fmt::Display for CapabilityInventoryError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "invalid capability inventory: {self:?}")
    }
}

impl std::error::Error for CapabilityInventoryError {}

#[cfg(test)]
mod tests {
    use super::*;
    use compiler_vocabulary::{LanguageProfile, RustEdition, TypeScriptSource};

    fn compiler_status(
        id: [u8; 32],
        family_profile: LanguageProfile,
        family_task: LanguageOracleTask,
        authority_profile: LanguageProfile,
        package_authority: PackageAuthorityIdentity,
    ) -> CapabilityStatus {
        let manifest = [3; 32];
        let toolchain_identity = [4; 32];
        let protocol_abi = 7;
        let toolchain = family_profile.language().native_tool();
        CapabilityStatus::observed(
            CapabilityId::new(id),
            CapabilityFamily::LanguageOracle {
                profile: family_profile,
                task: family_task,
            },
            manifest,
            CapabilityTarget::Native {
                os: 1,
                architecture: 1,
            },
            protocol_abi,
            CapabilityAuthority::Compiler {
                recipe: compiler_authority_recipe(
                    manifest,
                    authority_profile,
                    family_task,
                    protocol_abi,
                    toolchain,
                    toolchain_identity,
                    package_authority,
                ),
                toolchain,
                toolchain_identity,
                package_authority,
            },
            CapabilityLifecycle::Ready,
        )
    }

    fn embedding_recipe() -> EmbeddingCapabilityRecipe {
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

    fn embedding_status(
        id: [u8; 32],
        family_identity: [u8; 32],
        authority: EmbeddingCapabilityRecipe,
    ) -> CapabilityStatus {
        CapabilityStatus::observed(
            CapabilityId::new(id),
            CapabilityFamily::Embedding {
                recipe: Some(EmbeddingRecipeId::new(family_identity)),
            },
            [14; 32],
            CapabilityTarget::Managed(1),
            1,
            CapabilityAuthority::Embedding(authority),
            CapabilityLifecycle::Ready,
        )
    }

    #[test]
    fn inventory_is_bounded_sorted_and_explicit() {
        let inventory = CapabilityInventory::explicitly_unavailable();
        assert_eq!(inventory.as_slice().len(), 28);
        assert!(inventory.as_slice().iter().all(|status| matches!(
            status.lifecycle(),
            CapabilityLifecycle::Unavailable(CapabilityUnavailable::NoManifest)
        )));
        let duplicate = CapabilityStatus::unavailable(
            CapabilityId::new([1; 32]),
            CapabilityFamily::Embedding { recipe: None },
            CapabilityUnavailable::NoManifest,
        );
        assert_eq!(
            CapabilityInventory::try_new(vec![duplicate, duplicate]),
            Err(CapabilityInventoryError::Order)
        );
        assert_eq!(
            CapabilityInventory::try_new(vec![duplicate; MAX_CAPABILITY_INVENTORY + 1]),
            Err(CapabilityInventoryError::TooMany)
        );
    }

    #[test]
    fn compiler_authority_is_bound_to_profile_task_and_native_tool() {
        let profile = LanguageProfile::Rust(RustEdition::Rust2024);
        let task = LanguageOracleTask::SemanticIndex;
        let manifest = [3; 32];
        let toolchain_identity = [4; 32];
        let package_authority = PackageAuthorityIdentity::ImmutableClosure([5; 32]);
        let protocol_abi = 7;
        let valid = CapabilityStatus::observed(
            CapabilityId::new([1; 32]),
            CapabilityFamily::LanguageOracle { profile, task },
            manifest,
            CapabilityTarget::Native {
                os: 1,
                architecture: 1,
            },
            protocol_abi,
            CapabilityAuthority::Compiler {
                recipe: compiler_authority_recipe(
                    manifest,
                    profile,
                    task,
                    protocol_abi,
                    NativeTool::Rustc,
                    toolchain_identity,
                    package_authority,
                ),
                toolchain: NativeTool::Rustc,
                toolchain_identity,
                package_authority,
            },
            CapabilityLifecycle::Ready,
        );
        assert!(CapabilityInventory::try_new(vec![valid]).is_ok());

        let cross_language = CapabilityStatus::observed(
            CapabilityId::new([2; 32]),
            CapabilityFamily::LanguageOracle { profile, task },
            manifest,
            CapabilityTarget::Native {
                os: 1,
                architecture: 1,
            },
            protocol_abi,
            CapabilityAuthority::Compiler {
                recipe: compiler_authority_recipe(
                    manifest,
                    profile,
                    task,
                    protocol_abi,
                    NativeTool::Python,
                    toolchain_identity,
                    package_authority,
                ),
                toolchain: NativeTool::Python,
                toolchain_identity,
                package_authority,
            },
            CapabilityLifecycle::Ready,
        );
        assert_eq!(
            CapabilityInventory::try_new(vec![cross_language]),
            Err(CapabilityInventoryError::Authority)
        );

        let wrong_task_recipe = CapabilityStatus::observed(
            CapabilityId::new([3; 32]),
            CapabilityFamily::LanguageOracle { profile, task },
            manifest,
            CapabilityTarget::Native {
                os: 1,
                architecture: 1,
            },
            protocol_abi,
            CapabilityAuthority::Compiler {
                recipe: compiler_authority_recipe(
                    manifest,
                    profile,
                    LanguageOracleTask::TypeCheck,
                    protocol_abi,
                    NativeTool::Rustc,
                    toolchain_identity,
                    package_authority,
                ),
                toolchain: NativeTool::Rustc,
                toolchain_identity,
                package_authority,
            },
            CapabilityLifecycle::Ready,
        );
        assert_eq!(
            CapabilityInventory::try_new(vec![wrong_task_recipe]),
            Err(CapabilityInventoryError::Authority)
        );
    }

    #[test]
    fn compiler_authority_rejects_a_family_profile_mismatch() {
        let family = LanguageProfile::TypeScript(TypeScriptSource::TypeScript);
        let authority_profile = LanguageProfile::Rust(RustEdition::Rust2024);
        let status = compiler_status(
            [20; 32],
            family,
            LanguageOracleTask::SemanticIndex,
            authority_profile,
            PackageAuthorityIdentity::ImmutableClosure([21; 32]),
        );
        assert_eq!(
            CapabilityInventory::try_new(vec![status]),
            Err(CapabilityInventoryError::Authority)
        );
    }

    #[test]
    fn compiler_authority_rejects_a_mutated_package_scope_claim() {
        let profile = LanguageProfile::Rust(RustEdition::Rust2024);
        let immutable = PackageAuthorityIdentity::ImmutableClosure([30; 32]);
        let local = PackageAuthorityIdentity::LocalConfiguration([30; 32]);
        assert_ne!(
            compiler_authority_recipe(
                [31; 32],
                profile,
                LanguageOracleTask::SemanticIndex,
                7,
                profile.language().native_tool(),
                [32; 32],
                immutable,
            ),
            compiler_authority_recipe(
                [31; 32],
                profile,
                LanguageOracleTask::SemanticIndex,
                7,
                profile.language().native_tool(),
                [32; 32],
                local,
            )
        );
        let status = compiler_status(
            [33; 32],
            profile,
            LanguageOracleTask::SemanticIndex,
            profile,
            immutable,
        );
        let mut forged = status;
        forged.authority = Some(CapabilityAuthority::Compiler {
            recipe: compiler_authority_recipe(
                [3; 32],
                profile,
                LanguageOracleTask::SemanticIndex,
                7,
                profile.language().native_tool(),
                [4; 32],
                immutable,
            ),
            toolchain: profile.language().native_tool(),
            toolchain_identity: [4; 32],
            package_authority: local,
        });
        assert_eq!(
            CapabilityInventory::try_new(vec![forged]),
            Err(CapabilityInventoryError::Authority)
        );
    }

    #[test]
    fn embedding_authority_rejects_recipe_and_resource_bound_forgery() {
        let valid = embedding_recipe();
        let valid_identity = valid.identity().as_bytes();
        assert!(
            CapabilityInventory::try_new(vec![embedding_status([41; 32], valid_identity, valid,)])
                .is_ok()
        );

        let mismatched_family = embedding_status([42; 32], [43; 32], valid);
        assert_eq!(
            CapabilityInventory::try_new(vec![mismatched_family]),
            Err(CapabilityInventoryError::Authority)
        );

        let mut no_dimensions = valid;
        no_dimensions.dimensions = 0;
        assert_eq!(
            CapabilityInventory::try_new(vec![embedding_status(
                [44; 32],
                valid_identity,
                no_dimensions,
            )]),
            Err(CapabilityInventoryError::Authority)
        );

        let mut no_input_bound = valid;
        no_input_bound.maximum_input_bytes = None;
        no_input_bound.maximum_tokens = None;
        assert_eq!(
            CapabilityInventory::try_new(vec![embedding_status(
                [45; 32],
                valid_identity,
                no_input_bound,
            )]),
            Err(CapabilityInventoryError::Authority)
        );

        let mut overlap_exceeds_bound = valid;
        overlap_exceeds_bound.overlap_tokens = 128;
        assert_eq!(
            CapabilityInventory::try_new(vec![embedding_status(
                [46; 32],
                valid_identity,
                overlap_exceeds_bound,
            )]),
            Err(CapabilityInventoryError::Authority)
        );
    }

    #[test]
    fn embedding_authority_identity_commits_every_compatibility_field() {
        let valid = embedding_recipe();
        assert_eq!(valid.identity(), valid.recomputed_identity());
        let family_identity = valid.identity().as_bytes();
        let mutations: [(&str, fn(&mut EmbeddingCapabilityRecipe)); 13] = [
            ("model", |recipe| recipe.model = [61; 32]),
            ("tokenizer", |recipe| recipe.tokenizer = [62; 32]),
            ("source", |recipe| recipe.source = EmbeddingSource::RawUtf8),
            ("maximum_input_bytes", |recipe| {
                recipe.maximum_input_bytes = Some(2048)
            }),
            ("maximum_tokens", |recipe| recipe.maximum_tokens = Some(64)),
            ("overlap_tokens", |recipe| recipe.overlap_tokens = 8),
            ("dimensions", |recipe| recipe.dimensions = 512),
            ("metric", |recipe| recipe.metric = EmbeddingMetric::Dot),
            ("pooling", |recipe| recipe.pooling = EmbeddingPooling::Cls),
            ("normalization", |recipe| {
                recipe.normalization = EmbeddingNormalization::None
            }),
            ("encoding", |recipe| {
                recipe.encoding = EmbeddingEncoding::Signed8
            }),
            ("query_treatment", |recipe| {
                recipe.query_treatment = [63; 32]
            }),
            ("document_treatment", |recipe| {
                recipe.document_treatment = [64; 32]
            }),
        ];

        for (index, (field, mutate)) in mutations.into_iter().enumerate() {
            let mut forged = valid;
            mutate(&mut forged);
            assert_ne!(
                forged.identity(),
                forged.recomputed_identity(),
                "mutation must change the canonical identity: {field}"
            );
            assert_eq!(
                CapabilityInventory::try_new(vec![embedding_status(
                    [70 + index as u8; 32],
                    family_identity,
                    forged,
                )]),
                Err(CapabilityInventoryError::Authority),
                "mutated field was admitted: {field}"
            );
        }
    }

    #[test]
    fn inventory_rejects_duplicate_profile_and_task_slots_even_with_new_ids() {
        let profile = LanguageProfile::Rust(RustEdition::Rust2024);
        let first = compiler_status(
            [50; 32],
            profile,
            LanguageOracleTask::SemanticIndex,
            profile,
            PackageAuthorityIdentity::ImmutableClosure([51; 32]),
        );
        let second = compiler_status(
            [52; 32],
            profile,
            LanguageOracleTask::SemanticIndex,
            profile,
            PackageAuthorityIdentity::ImmutableClosure([53; 32]),
        );
        assert_eq!(
            CapabilityInventory::try_new(vec![first, second]),
            Err(CapabilityInventoryError::DuplicateFamily)
        );
    }
}
