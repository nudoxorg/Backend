//! Typed commands and results owned by the durable product service.

use crate::interface::{
    CompilerAttempt, CompilerFragmentFailure, CompilerFragmentFaultFacts,
    CompilerFragmentFaultKind, CompilerToolFailure, CompilerToolIssue, CompilerToolRequirement,
    MAX_COMPILER_FRAGMENT_DETAIL_BYTES,
};
use crate::{
    CommandId, DependencyFacts, ForgeCoordinate, ForgeObjectId, ForgeRevision,
    PackageDependencyRecord, RegistryForgeAssociation, RegistryNativeMetadata,
};
use backend_advisory::{AdvisoryPackageDto, OverrideEvidence};
pub use backend_semantic::vocabulary::{PackageUrl as PackageCoordinate, RegistryEcosystem};
use backend_version::{CompileRecipeDomain, ContentId, Domain, HASH_BYTES, SourceFactDomain};
use serde::{
    Deserialize, Deserializer, Serialize, Serializer, de::Error as _, ser::SerializeStruct,
};
use std::collections::BTreeSet;
use std::io::{self, Write};
use std::num::{NonZeroU32, NonZeroU64};
use std::{fmt, str::FromStr};

#[path = "surface/package_compiler_failure.rs"]
mod package_compiler_failure;
pub use package_compiler_failure::{
    AuthorityClassFact, AuthorityPhaseFact, CompilerAuthorityDiagnosticFacts, CompilerLanguageFact,
    CompilerNativeToolFact, CompilerStageFact, PackageAnonymousCallableAnchorFaultFacts,
    PackageAnonymousCallableAnchorPoolFact, PackageCompilerFailureCause,
    PackageCompilerFailurePhase, PackageCompilerFragmentFaultFacts, PackageForeignKeyFaultFacts,
    PackageLanguageProjectionFault, PackageLineageFaultFacts, PackageLoweringFaultFacts,
    PackageParentageFact, PackageProjectionAdmissionFaultFacts,
    PackageProjectionConstructorFaultFacts, PackageProjectionSemanticTypeFaultFacts,
    PackageTypeCellFact, PackageTypeScriptProjectionFaultFacts, PackageTypeTagFact,
};

/// Largest user-authored operand retained by the product service.
pub const MAX_PRODUCT_TEXT_BYTES: usize = 4096;
/// Largest row collection in one product request or reply.
pub const MAX_PRODUCT_ROWS: usize = 256;
/// Maximum number of source-file members admitted in one selected Project frontier.
///
/// The engine's canonical Project membership validator uses this same bound;
/// semantic query annotations preserve the validated count without widening it.
pub const MAX_SELECTED_PROJECT_FRONTIER_FILES: usize = 100_000;
/// Largest opaque continuation token accepted by the shared index-search surface.
pub const MAX_INDEX_SEARCH_CURSOR_BYTES: usize = 64 * 1024;
/// Maximum number of owner progress events returned by one index progress read.
pub const MAX_INDEX_PROGRESS_EVENTS: usize = 16;
/// Maximum human-readable detail retained in one derived-history status.
pub(crate) const MAX_SEMANTIC_HISTORY_STATUS_DETAIL_BYTES: usize = 1024;

/// Nonempty, bounded, NUL-free product text.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ProductText(String);

impl ProductText {
    /// Admits one product operand.
    ///
    /// # Errors
    ///
    /// Returns [`ProductAdmissionError`] when the text is empty, contains NUL, or exceeds the
    /// fixed operand byte bound.
    pub fn new(value: impl Into<String>) -> Result<Self, ProductAdmissionError> {
        let value = value.into();
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return Err(ProductAdmissionError::EmptyText);
        }
        if trimmed.len() > MAX_PRODUCT_TEXT_BYTES || trimmed.bytes().any(|byte| byte == 0) {
            return Err(ProductAdmissionError::TextBound);
        }
        if trimmed.len() == value.len() {
            Ok(Self(value))
        } else {
            Ok(Self(trimmed.to_owned()))
        }
    }

    /// Creates text from a compile-time literal that is part of the product
    /// protocol. Callers use this only for fixed protocol reason strings;
    /// user-authored values must continue through [`Self::new`].
    #[must_use]
    pub fn from_static(value: &'static str) -> Self {
        debug_assert!(!value.trim().is_empty());
        debug_assert!(value.len() <= MAX_PRODUCT_TEXT_BYTES);
        debug_assert!(!value.bytes().any(|byte| byte == 0));
        Self(value.to_owned())
    }
    /// Returns the admitted text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub(crate) fn retained_capacity(&self) -> usize {
        self.0.capacity()
    }
}

impl TryFrom<String> for ProductText {
    type Error = ProductAdmissionError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}
impl From<ProductText> for String {
    fn from(value: ProductText) -> Self {
        value.0
    }
}

/// Stable typed identity tag for one [`PackageReference`] coordinate.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum PackageReferenceKind {
    /// Canonical version-pinned package URL.
    Purl = 1,
    /// Canonical label in the immutable local view.
    Local = 2,
}

impl PackageReferenceKind {
    /// Returns the stable wire and persistence tag.
    #[must_use]
    pub const fn tag(self) -> u8 {
        self as u8
    }
}

impl TryFrom<u8> for PackageReferenceKind {
    type Error = ProductAdmissionError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::Purl),
            2 => Ok(Self::Local),
            _ => Err(ProductAdmissionError::PackageReference),
        }
    }
}

/// An admitted pinned purl or local canonical package label.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "kebab-case")]
pub enum PackageReference {
    /// Canonical version-pinned package URL.
    Purl(PackageCoordinate),
    /// Canonical label in the immutable local view.
    Local(ProductText),
}

impl PackageReference {
    /// Parses the package namespace once at the command boundary.
    ///
    /// # Errors
    ///
    /// Returns [`ProductAdmissionError`] when a package URL is malformed or a local label fails
    /// text admission.
    pub fn parse(value: impl Into<String>) -> Result<Self, ProductAdmissionError> {
        let value = value.into();
        if value.starts_with("pkg:") {
            PackageCoordinate::parse(value)
                .map(Self::Purl)
                .map_err(|_| ProductAdmissionError::PackageReference)
        } else {
            ProductText::new(value).map(Self::Local)
        }
    }

    /// Constructs a reference using an explicit kind instead of inferring the
    /// kind from its spelling.
    pub fn from_kind(
        kind: PackageReferenceKind,
        value: impl Into<String>,
    ) -> Result<Self, ProductAdmissionError> {
        let value = value.into();
        match kind {
            PackageReferenceKind::Purl => PackageCoordinate::parse(value)
                .map(Self::Purl)
                .map_err(|_| ProductAdmissionError::PackageReference),
            PackageReferenceKind::Local => ProductText::new(value).map(Self::Local),
        }
    }

    /// Returns the stable typed identity of this reference.
    #[must_use]
    pub const fn kind(&self) -> PackageReferenceKind {
        match self {
            Self::Purl(_) => PackageReferenceKind::Purl,
            Self::Local(_) => PackageReferenceKind::Local,
        }
    }

    /// Returns the canonical spelling.
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Purl(v) => v.as_str(),
            Self::Local(v) => v.as_str(),
        }
    }

    /// Orders references by canonical spelling, then by their typed identity.
    ///
    /// This preserves the existing spelling order while distinguishing a
    /// local label from a PURL with the same display text.
    #[must_use]
    pub fn canonical_cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.as_str()
            .cmp(other.as_str())
            .then_with(|| self.kind().cmp(&other.kind()))
    }
}

/// A bounded unique project name.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ProjectName(ProductText);

impl ProjectName {
    /// Admits a project name no longer than 64 bytes.
    ///
    /// # Errors
    ///
    /// Returns [`ProductAdmissionError`] when the name is empty, too long, or contains control
    /// characters.
    pub fn new(value: impl Into<String>) -> Result<Self, ProductAdmissionError> {
        let value = ProductText::new(value)?;
        if value.as_str().len() > 64 || value.as_str().bytes().any(|b| b.is_ascii_control()) {
            return Err(ProductAdmissionError::ProjectName);
        }
        Ok(Self(value))
    }
    /// Returns the admitted name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}
impl TryFrom<String> for ProjectName {
    type Error = ProductAdmissionError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}
impl From<ProjectName> for String {
    fn from(value: ProjectName) -> Self {
        value.0.into()
    }
}

/// Stable, never-reused project identity.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProjectId(NonZeroU64);
impl ProjectId {
    /// Wraps a nonzero identity.
    #[must_use]
    pub const fn new(value: NonZeroU64) -> Self {
        Self(value)
    }
    /// Returns the numeric identity.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

/// A project selected by stable identity or unique name.
#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(tag = "by", content = "value", rename_all = "kebab-case")]
pub enum ProjectSelector {
    /// Stable identity.
    Id(ProjectId),
    /// Unique name.
    Name(ProjectName),
}
impl ProjectSelector {
    /// Parses digits as an identity and other text as a name.
    ///
    /// # Errors
    ///
    /// Returns [`ProductAdmissionError`] when a nonnumeric selector is not an admitted project
    /// name.
    pub fn parse(value: &str) -> Result<Self, ProductAdmissionError> {
        value
            .parse::<u64>()
            .ok()
            .and_then(NonZeroU64::new)
            .map(ProjectId::new)
            .map_or_else(
                || ProjectName::new(value).map(Self::Name),
                |id| Ok(Self::Id(id)),
            )
    }
}

/// Stable session-tree node identity.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TreeNodeId(NonZeroU64);
impl TreeNodeId {
    /// Wraps a nonzero node identity.
    #[must_use]
    pub const fn new(value: NonZeroU64) -> Self {
        Self(value)
    }
    /// Returns the numeric identity.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

/// Which product surface opened a session node.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "surface", content = "client", rename_all = "kebab-case")]
pub enum TreeOpener {
    /// Desktop application.
    Desktop,
    /// Command line.
    Cli,
    /// MCP client with its admitted name.
    Mcp(ProductText),
}

/// What one shared session node displays.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "subject", content = "value", rename_all = "kebab-case")]
pub enum TreeSubject {
    /// Local or registry package.
    Package(PackageReference),
    /// Declaration label.
    Declaration(ProductText),
    /// Registry exploration query.
    Explore(Option<ProductText>),
    /// Documentation search query.
    Search(ProductText),
    /// Registry publisher handle.
    Owner(ProductText),
}

/// Canonical two-byte compiler language profile used by semantic history.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SemanticLanguageProfile([u8; 2]);

impl SemanticLanguageProfile {
    /// Encodes one closed compiler language profile.
    #[must_use]
    pub fn new(profile: backend_semantic::vocabulary::LanguageProfile) -> Self {
        Self(profile.into())
    }

    /// Reopens the closed compiler language profile.
    ///
    /// # Errors
    /// Returns a product admission error for an unknown language/profile pair.
    pub fn profile(
        self,
    ) -> Result<backend_semantic::vocabulary::LanguageProfile, ProductAdmissionError> {
        backend_semantic::vocabulary::LanguageProfile::try_from(self.0)
            .map_err(|_| ProductAdmissionError::SemanticVersionShape)
    }

    /// Returns the canonical profile bytes.
    #[must_use]
    pub const fn to_bytes(self) -> [u8; 2] {
        self.0
    }

    /// Admits one closed profile by the spelling every surface uses.
    ///
    /// The nine product profiles are the closed set the built-in semantic
    /// plane advertises; a surface that accepts free text here would let a
    /// caller ask for a language the engine cannot answer for.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        backend_semantic::vocabulary::LanguageProfile::PRODUCT_PROFILES
            .into_iter()
            .map(Self::new)
            .find(|profile| profile.name() == Some(name))
    }

    /// Returns the closed profile spelling shared by every surface.
    #[must_use]
    pub fn name(self) -> Option<&'static str> {
        use backend_semantic::vocabulary::{LanguageProfile, TypeScriptSource};
        match self.profile().ok()? {
            LanguageProfile::Rust(_) => Some("rust"),
            LanguageProfile::TypeScript(TypeScriptSource::TypeScript) => Some("typescript"),
            LanguageProfile::TypeScript(TypeScriptSource::Tsx) => Some("tsx"),
            LanguageProfile::Python(_) => Some("python"),
            LanguageProfile::Go(_) => Some("go"),
            LanguageProfile::Java(_) => Some("java"),
            LanguageProfile::CSharp(_) => Some("csharp"),
            LanguageProfile::C(_) => Some("c"),
            LanguageProfile::Cxx(_) => Some("cpp"),
        }
    }

    /// Returns every closed profile spelling, in product order.
    #[must_use]
    pub fn names() -> Vec<&'static str> {
        backend_semantic::vocabulary::LanguageProfile::PRODUCT_PROFILES
            .into_iter()
            .map(Self::new)
            .filter_map(Self::name)
            .collect()
    }
}

/// Exact immutable compiler generation binding exposed to product clients.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SemanticGenerationId([u8; 32]);

impl SemanticGenerationId {
    /// Creates a product identity from an admitted compiler binding identity.
    #[must_use]
    pub const fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Returns the exact compiler binding identity bytes.
    #[must_use]
    pub const fn to_bytes(self) -> [u8; 32] {
        self.0
    }
}

/// One immutable compiler publication available for exact selection.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticVersionRecord {
    /// Product package whose semantic plane owns this generation.
    pub package: PackageReference,
    /// Exact compiler package URL, including version, qualifiers, and subpath.
    pub coordinate: PackageCoordinate,
    /// Closed source language and dialect profile.
    pub profile: SemanticLanguageProfile,
    /// Exact immutable compilation binding.
    pub generation: SemanticGenerationId,
    /// Verified generation root selected by the compiler journal.
    pub generation_root: [u8; 32],
    /// Verified dependency-set commitment selected by the compiler journal.
    pub dependency_set: [u8; 32],
    /// Semantic manifest committed by the generation binding.
    pub manifest: [u8; 32],
    /// Number of semantic image artifacts in the manifest.
    pub artifacts: u32,
    /// Encoded canonical compiler-manifest bytes committed by this generation.
    /// This is not the aggregate payload extent of the semantic images.
    pub semantic_bytes: u32,
    /// Whether the compiler authority covered the complete declared scope.
    pub complete: bool,
    /// Whether the mutable target row currently selects this generation.
    pub selected: bool,
    /// Whether this generation was compiled for the latest admitted source input.
    ///
    /// This is distinct from `selected`: an operator may deliberately select a
    /// retained historical generation while the newest source observation
    /// continues to name a newer input. Older peers that do not provide this
    /// field decode as `Unverified` instead of being claimed as current.
    #[serde(default)]
    pub freshness: SemanticVersionFreshness,
    /// Status of the selected native-image typed V3 history sidecar.
    ///
    /// This is keyed by the exact committed owner-selection stamp. It is
    /// separate from source-input freshness: a selected compiler generation
    /// can be current even while its derived history publication is pending
    /// or refused.
    #[serde(default)]
    pub history_status: SemanticHistoryPublicationStatus,
    /// Current checked Project membership associated with this selected local
    /// generation in the immutable owner snapshot that answered the query.
    /// This is a query-time annotation, not part of the retained compiler
    /// history or its generation identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_source_frontier: Option<SelectedProjectSourceFrontier>,
}

/// Exact selected local Project frontier observed by one semantic-version query.
///
/// The relation root binds the result to one immutable owner snapshot, while
/// `source_version` and `file_count` come from the Project row and its fully
/// resolved, validated membership. This value is only attached to selected
/// semantic rows as current-view evidence; it is not persisted compiler history.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SelectedProjectSourceFrontier {
    /// Exact local package label requested by the caller.
    pub package: PackageReference,
    /// Selected source-relation root in the answering owner snapshot.
    pub source_relation_root: [u8; 32],
    /// Content identity recorded by the canonical Project row.
    pub source_version: [u8; 32],
    /// Number of resolved file keys in the complete canonical Project membership.
    pub file_count: u32,
}

/// Exact selected-generation stamp captured by native history publication.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticHistorySelectionStamp {
    /// Turso authority namespace bytes.
    pub namespace: [u8; 16],
    /// Selected language profile.
    pub profile: SemanticLanguageProfile,
    /// Digest of the selected source coordinate.
    pub source_coordinate: [u8; 32],
    /// Monotonic committed selection revision.
    pub selection_revision: u64,
    /// Exact workspace-selected semantic root.
    pub selected_root: [u8; 32],
    /// Immutable selected closure identity.
    pub closure_id: [u8; 32],
    /// Root of the selected semantic image catalog.
    pub catalog_root: [u8; 32],
}

/// Exact native image authenticated by the selected catalog and manifest.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticHistoryImageIdentity {
    /// Ordinal in the selected image catalog.
    pub artifact_ordinal: u32,
    /// Canonical semantic generation identity of the full image.
    pub semantic_generation: [u8; 32],
    /// Exact manifest root named by the selected image key.
    pub manifest_root: [u8; 32],
    /// Identity hash of the complete encoded semantic image.
    pub image_identity: [u8; 32],
}

/// Authority recoverable from the durable typed V3 input claim.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SemanticHistoryInputReplayStatus {
    /// The opaque persisted input claim does not recreate a read frontier.
    Unproven,
}

/// One image and its independently admitted V3 history commit inside a
/// complete package publication.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticHistoryImagePublicationProof {
    /// Full-image identity bound to this catalog member.
    pub image: SemanticHistoryImageIdentity,
    /// Immutable V3 history commit for this image.
    pub history_commit: [u8; 32],
    /// First-parent lineage recorded by this admitted commit (at most 2).
    pub parent_commits: Box<[[u8; 32]]>,
}

/// Bounded public proof of an atomic complete-package V3 publication.
///
/// Every selected catalog image is listed in canonical order with its own
/// independently admitted history commit. The package identity commits the
/// exact ordered set; the public reference CAS occurs only after all listed
/// members have been verified. The input field deliberately states
/// `Unproven`; this summary does not recreate compiler source-read authority.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticHistoryPublicationProof {
    /// Exact committed product selection used by the publication fence.
    pub selection: SemanticHistorySelectionStamp,
    /// Package name used by the native semantic target.
    pub target_package: String,
    /// Exact source/version coordinate used by the native semantic target.
    pub target_coordinate: String,
    /// Deterministic digest of the complete ordered image set and selection.
    pub package_identity: [u8; 32],
    /// Every full-image identity and V3 commit, in catalog order.
    pub images: Box<[SemanticHistoryImagePublicationProof]>,
    /// Public branch tip at the single successful package CAS.
    pub reference_tip: [u8; 32],
    /// Final package commit proven reachable from `reference_tip`.
    pub reachable_commit: [u8; 32],
    /// Exact persisted input authority level for every listed member.
    pub input_replay_status: SemanticHistoryInputReplayStatus,
}

impl SemanticHistoryPublicationProof {
    /// Recomputes the package-set identity from public proof fields. The
    /// selected catalog root commits each complete manifest, including its
    /// input claim and compiler recipe; the ordered images bind their exact
    /// native bytes to those manifest entries.
    #[must_use]
    pub fn recompute_package_identity(&self) -> [u8; 32] {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"backend.replication.selected-native-history-package.v1\0");
        for field in [
            self.target_package.as_bytes(),
            self.target_coordinate.as_bytes(),
        ] {
            hasher.update(&(field.len() as u64).to_le_bytes());
            hasher.update(field);
        }
        hasher.update(&self.selection.profile.to_bytes());
        hasher.update(&self.selection.namespace);
        hasher.update(&self.selection.profile.to_bytes());
        hasher.update(&self.selection.source_coordinate);
        hasher.update(&self.selection.selection_revision.to_le_bytes());
        hasher.update(&self.selection.selected_root);
        hasher.update(&self.selection.closure_id);
        hasher.update(&self.selection.catalog_root);
        hasher.update(&(self.images.len() as u64).to_le_bytes());
        for image in self.images.iter() {
            hasher.update(&image.image.artifact_ordinal.to_le_bytes());
            hasher.update(&image.image.semantic_generation);
            hasher.update(&image.image.manifest_root);
            hasher.update(&image.image.image_identity);
        }
        *hasher.finalize().as_bytes()
    }
}

/// Typed status for derived native-image history publication.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum SemanticHistoryPublicationStatus {
    /// This semantic version is not the committed selected product version.
    NotSelected,
    /// The committed selection has not yet been reconciled with V3 history.
    NotRequested {
        /// Identity of the selected product version awaiting V3 reconciliation.
        selection_id: [u8; 32],
    },
    /// A bounded worker is producing and admitting history for this selection.
    Pending {
        /// Identity of the selected product version being processed.
        selection_id: [u8; 32],
    },
    /// The committed selection is waiting for a bounded worker slot. The
    /// owner reschedules it from the current selected-marker inventory.
    Deferred {
        /// Identity of the selected product version waiting for a worker slot.
        selection_id: [u8; 32],
        /// Bounded detail explaining why the job was deferred.
        reason: String,
    },
    /// The exact selected image is durably published at this V3 branch commit.
    Published {
        /// Identity of the selected product version whose image was published.
        selection_id: [u8; 32],
        /// Commit containing the published derived history.
        commit: [u8; 32],
        /// Reference name containing that commit.
        reference: String,
        /// Exact, bounded proof summary for this branch commit.
        proof: SemanticHistoryPublicationProof,
    },
    /// History could not be produced or admitted for this selected image.
    Refused {
        /// Identity of the selected product version whose history was refused.
        selection_id: [u8; 32],
        /// Bounded detail describing the production or admission refusal.
        reason: String,
    },
    /// The marker advanced while this derived-history job was running.
    Superseded {
        /// Identity of the selection whose worker job was superseded by a newer marker.
        selection_id: [u8; 32],
    },
}

impl Default for SemanticHistoryPublicationStatus {
    fn default() -> Self {
        Self::NotSelected
    }
}

/// Source-input status for one immutable semantic generation.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum SemanticVersionFreshness {
    /// The selected generation's exact compiler input digest matches the latest observation.
    Current {
        /// Digest of the selected generation's compiler inputs, matching the latest observation.
        input_digest: [u8; 32],
    },
    /// The generation uses an older input than the latest admitted observation.
    Historical {
        /// Exact input digest retained by this generation.
        selected_input: [u8; 32],
        /// Latest exact input digest observed for this semantic target.
        latest_input: [u8; 32],
    },
    /// The sender predates persisted input freshness or no typed evidence was available.
    Unverified,
}

impl Default for SemanticVersionFreshness {
    fn default() -> Self {
        Self::Unverified
    }
}

impl SemanticVersionRecord {
    pub(crate) fn admit(&self) -> Result<(), ProductAdmissionError> {
        self.profile.profile()?;
        if self.coordinate.package_type().language() != self.profile.profile()?.language()
            || self.artifacts == 0
        {
            return Err(ProductAdmissionError::SemanticVersionShape);
        }
        if self
            .selected_source_frontier
            .as_ref()
            .is_some_and(|frontier| {
                !self.selected
                    || self.package != frontier.package
                    || !matches!(&frontier.package, PackageReference::Local(_))
                    || match usize::try_from(frontier.file_count) {
                        Ok(count) => count > MAX_SELECTED_PROJECT_FRONTIER_FILES,
                        Err(_) => true,
                    }
            })
        {
            return Err(ProductAdmissionError::SemanticVersionShape);
        }
        if let SemanticHistoryPublicationStatus::Published {
            commit,
            reference,
            proof,
            ..
        } = &self.history_status
            && (reference != "selected-native-v3"
                || proof.reachable_commit != *commit
                || proof.reference_tip != proof.reachable_commit
                || proof.images.is_empty()
                || proof.target_package.is_empty()
                || proof.target_package.len() > MAX_PRODUCT_TEXT_BYTES
                || proof.target_coordinate.is_empty()
                || proof.target_coordinate.len() > MAX_PRODUCT_TEXT_BYTES
                || proof.package_identity != proof.recompute_package_identity()
                || usize::try_from(self.artifacts).ok() != Some(proof.images.len())
                || proof.images.iter().enumerate().any(|(ordinal, image)| {
                    usize::try_from(image.image.artifact_ordinal).ok() != Some(ordinal)
                        || image.parent_commits.len() > 1
                        || (ordinal > 0
                            && image.parent_commits.as_ref()
                                != [proof.images[ordinal - 1].history_commit])
                })
                || proof.images.last().map(|image| image.history_commit) != Some(*commit)
                || proof.selection.profile != self.profile)
        {
            return Err(ProductAdmissionError::SemanticVersionShape);
        }
        if match &self.history_status {
            SemanticHistoryPublicationStatus::Deferred { reason, .. }
            | SemanticHistoryPublicationStatus::Refused { reason, .. } => {
                reason.len() > MAX_SEMANTIC_HISTORY_STATUS_DETAIL_BYTES
            }
            _ => false,
        } {
            return Err(ProductAdmissionError::SemanticVersionShape);
        }
        Ok(())
    }
}

/// Owner-issued identity for one package-scoped index operation.
///
/// The package is part of the ticket so an old or mismatched cancellation
/// request cannot address another project's active job.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IndexJobTicket {
    id: NonZeroU64,
    owner_epoch: [u8; 16],
    package: PackageReference,
}

impl IndexJobTicket {
    /// Creates an owner ticket from a nonzero sequence and exact package.
    #[must_use]
    pub const fn new(id: NonZeroU64, owner_epoch: [u8; 16], package: PackageReference) -> Self {
        Self {
            id,
            owner_epoch,
            package,
        }
    }

    /// Returns the owner-local job sequence.
    #[must_use]
    pub const fn id(&self) -> u64 {
        self.id.get()
    }

    /// Returns the process-local owner epoch that prevents a stale ticket from matching after
    /// the owner restarts and its sequence begins again.
    #[must_use]
    pub const fn owner_epoch(&self) -> [u8; 16] {
        self.owner_epoch
    }

    /// Returns the package bound to this job.
    #[must_use]
    pub const fn package(&self) -> &PackageReference {
        &self.package
    }
}

/// Caller-owned, durable identity for one requested index operation.
///
/// The caller must persist this key before sending an operation start. Unlike
/// [`IndexJobTicket`], it is stable across owner restarts and can be queried
/// without retaining a process-local ticket.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct IndexOperationKey([u8; 32]);

impl IndexOperationKey {
    /// Admits one nonzero operation key.
    ///
    /// # Errors
    /// Returns [`ProductAdmissionError::IndexOperationKey`] for the reserved
    /// all-zero key.
    pub fn from_bytes(bytes: [u8; 32]) -> Result<Self, ProductAdmissionError> {
        if bytes.iter().all(|byte| *byte == 0) {
            return Err(ProductAdmissionError::IndexOperationKey);
        }
        Ok(Self(bytes))
    }

    /// Returns the exact key bytes for durable caller storage.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Copies the exact key bytes.
    #[must_use]
    pub const fn to_bytes(self) -> [u8; 32] {
        self.0
    }

    /// Parses the canonical lowercase 64-character hexadecimal form.
    ///
    /// # Errors
    /// Returns [`ProductAdmissionError::IndexOperationKey`] for uppercase,
    /// malformed, wrong-length, or reserved all-zero values.
    pub fn parse_hex(value: &str) -> Result<Self, ProductAdmissionError> {
        if value.len() != 64 || value.bytes().any(|byte| byte.is_ascii_uppercase()) {
            return Err(ProductAdmissionError::IndexOperationKey);
        }
        let mut bytes = [0_u8; 32];
        for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
            let high = hex_nibble(pair[0]).ok_or(ProductAdmissionError::IndexOperationKey)?;
            let low = hex_nibble(pair[1]).ok_or(ProductAdmissionError::IndexOperationKey)?;
            bytes[index] = (high << 4) | low;
        }
        Self::from_bytes(bytes)
    }

    /// Returns the canonical lowercase hexadecimal form.
    #[must_use]
    pub fn to_hex(self) -> String {
        let mut value = String::with_capacity(64);
        for byte in self.0 {
            use fmt::Write as _;
            let _ = write!(value, "{byte:02x}");
        }
        value
    }
}

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}

impl TryFrom<[u8; 32]> for IndexOperationKey {
    type Error = ProductAdmissionError;

    fn try_from(value: [u8; 32]) -> Result<Self, Self::Error> {
        Self::from_bytes(value)
    }
}

impl TryFrom<String> for IndexOperationKey {
    type Error = ProductAdmissionError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse_hex(&value)
    }
}

impl From<IndexOperationKey> for [u8; 32] {
    fn from(value: IndexOperationKey) -> Self {
        value.0
    }
}

impl From<IndexOperationKey> for String {
    fn from(value: IndexOperationKey) -> Self {
        value.to_hex()
    }
}

impl fmt::Display for IndexOperationKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.to_hex())
    }
}

impl FromStr for IndexOperationKey {
    type Err = ProductAdmissionError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse_hex(value)
    }
}

/// Durable receipt for the exact workspace and product view published by an
/// index operation. The view rows remain available through the normal paged
/// view API; this compact receipt binds that view to the committed intent.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IndexOperationPublicationReceipt {
    /// Content identity of the workspace intent published for this operation.
    /// It is absent when indexing found no workspace mutation to commit.
    #[serde(with = "option_hex_32")]
    request_identity: Option<[u8; 32]>,
    /// Identity of the checked workspace commit that selected the intent.
    #[serde(with = "hex_32")]
    commit_identity: [u8; 32],
    /// Root selected by the workspace owner.
    #[serde(with = "hex_32")]
    workspace_root: [u8; 32],
    /// Sequence selected by the workspace owner.
    workspace_sequence: u64,
    /// Exact immutable product-view row root after publication.
    #[serde(with = "hex_32")]
    view_root: [u8; 32],
    /// Exact immutable product-view version after publication.
    #[serde(with = "hex_32")]
    view_version: [u8; 32],
    /// Exact product-view recipe after publication.
    #[serde(with = "hex_32")]
    view_recipe: [u8; 32],
    /// Authenticated control-cursor bytes for this published view revision.
    revision_cursor: Box<[u8]>,
}

impl IndexOperationPublicationReceipt {
    /// Constructs a receipt from the exact view and cursor held by the owner
    /// after publication.
    ///
    /// # Errors
    /// Returns [`ProductAdmissionError::IndexOperationShape`] when an identity
    /// is reserved, the workspace revision is invalid, or the cursor does not
    /// encode this exact published view.
    pub fn from_published_view(
        request_identity: Option<[u8; 32]>,
        commit_identity: [u8; 32],
        workspace_root: [u8; 32],
        workspace_sequence: u64,
        view: &crate::ViewRoot,
        cursor: crate::Cursor,
    ) -> Result<Self, ProductAdmissionError> {
        let expected_cursor = crate::Cursor::for_view_root(view);
        if cursor != expected_cursor || cursor.query_offset() != 0 {
            return Err(ProductAdmissionError::IndexOperationShape);
        }
        Self::from_checked_parts(
            request_identity,
            commit_identity,
            workspace_root,
            workspace_sequence,
            *view.root().as_bytes(),
            *view.version().as_bytes(),
            *view.recipe().as_bytes(),
            cursor.encode_control(),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn from_checked_parts(
        request_identity: Option<[u8; 32]>,
        commit_identity: [u8; 32],
        workspace_root: [u8; 32],
        workspace_sequence: u64,
        view_root: [u8; 32],
        view_version: [u8; 32],
        view_recipe: [u8; 32],
        revision_cursor: Box<[u8]>,
    ) -> Result<Self, ProductAdmissionError> {
        // This is structural admission for a serialized receipt, not proof
        // that these bytes came from an authorized cursor owner. Publication
        // authority remains with the separately retained owner evidence.
        let matches = |offset: usize, expected: &[u8; 32]| {
            revision_cursor
                .get(offset..offset.saturating_add(32))
                .is_some_and(|value| value == expected)
        };
        let has_nonzero_identity = |range: std::ops::Range<usize>| {
            revision_cursor
                .get(range)
                .is_some_and(|value| value.iter().any(|byte| *byte != 0))
        };
        if request_identity.is_some_and(|identity| identity.iter().all(|byte| *byte == 0))
            || [
                commit_identity,
                workspace_root,
                view_root,
                view_version,
                view_recipe,
            ]
            .iter()
            .any(|identity| identity.iter().all(|byte| *byte == 0))
            || revision_cursor.len() != crate::cursor::CURSOR_CONTROL_BYTES
            || revision_cursor.get(..2)
                != Some(crate::cursor::CURSOR_SCHEMA.to_be_bytes().as_slice())
            || !matches(2, &view_recipe)
            || !matches(34, &view_version)
            || !has_nonzero_identity(66..98)
            || !has_nonzero_identity(98..130)
            || revision_cursor.get(130..132)
                != Some(crate::cursor::CURSOR_SCHEMA.to_be_bytes().as_slice())
            || !matches(132, &view_root)
        {
            return Err(ProductAdmissionError::IndexOperationShape);
        }
        Ok(Self {
            request_identity,
            commit_identity,
            workspace_root,
            workspace_sequence,
            view_root,
            view_version,
            view_recipe,
            revision_cursor,
        })
    }

    /// Returns the committed workspace request identity.
    #[must_use]
    pub const fn request_identity(&self) -> Option<&[u8; 32]> {
        self.request_identity.as_ref()
    }

    /// Returns the exact checked commit identity.
    #[must_use]
    pub const fn commit_identity(&self) -> &[u8; 32] {
        &self.commit_identity
    }

    /// Returns the selected workspace root.
    #[must_use]
    pub const fn workspace_root(&self) -> &[u8; 32] {
        &self.workspace_root
    }

    /// Returns the selected workspace sequence.
    #[must_use]
    pub const fn workspace_sequence(&self) -> u64 {
        self.workspace_sequence
    }

    /// Returns the exact product view row root.
    #[must_use]
    pub const fn view_root(&self) -> &[u8; 32] {
        &self.view_root
    }

    /// Returns the exact immutable view version.
    #[must_use]
    pub const fn view_version(&self) -> &[u8; 32] {
        &self.view_version
    }

    /// Returns the exact product view recipe identity.
    #[must_use]
    pub const fn view_recipe(&self) -> &[u8; 32] {
        &self.view_recipe
    }

    /// Returns the fixed-width authenticated cursor bytes.
    #[must_use]
    pub fn revision_cursor(&self) -> &[u8] {
        &self.revision_cursor
    }
}

/// Coverage of one exact generation carried by a source-capture receipt.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "coverage", content = "detail", rename_all = "kebab-case")]
pub enum IndexOperationSemanticCoverage {
    /// Every member of the declared semantic scope was covered.
    Complete,
    /// A checked portion of the declared scope was covered.
    Partial {
        /// Number of units the generation completed.
        completed: u32,
        /// Total units in the declared semantic scope.
        total: u32,
    },
}

/// Coherent old generation retained while a new source capture is pending or
/// failed.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IndexOperationPriorSemantic {
    /// Exact generation identity.
    #[serde(with = "hex_32")]
    pub generation: [u8; 32],
    /// Coverage written with the same generation.
    pub coverage: IndexOperationSemanticCoverage,
}

/// Typed reason a source-captured semantic profile did not publish.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum IndexOperationSemanticUnavailableReason {
    /// The selected compiler or oracle was absent.
    Toolchain,
    /// Required project/package authority was absent.
    ProjectAuthority,
    /// The refresh was cancelled.
    Cancelled,
    /// Source or compiler admission refused the refresh.
    Rejected,
}

/// Independent semantic outcome for one captured profile.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", content = "detail", rename_all = "kebab-case")]
pub enum IndexOperationSemanticProfileState {
    /// Semantic work has not reached a terminal result for this capture.
    Pending {
        /// Previous coherent generation retained as stale evidence, if any.
        prior: Option<IndexOperationPriorSemantic>,
    },
    /// No coherent semantic generation was selected.
    Unavailable {
        /// Typed reason the profile has no selected generation.
        reason: IndexOperationSemanticUnavailableReason,
    },
    /// Refresh failed while preserving the prior generation as stale evidence.
    Failed {
        /// Previous coherent generation retained as stale evidence.
        prior: IndexOperationPriorSemantic,
        /// Typed reason the refresh failed.
        reason: IndexOperationSemanticUnavailableReason,
    },
    /// A generation was selected for this source capture.
    Published {
        /// Exact selected semantic generation.
        #[serde(with = "hex_32")]
        generation: [u8; 32],
        /// Coverage selected with the generation.
        coverage: IndexOperationSemanticCoverage,
    },
}

/// Exact source and compiler-observation facts for one closed profile.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IndexOperationSourceProfile {
    /// Closed compiler profile identity.
    pub profile: SemanticLanguageProfile,
    /// Product source frontier compiled for this profile.
    #[serde(with = "hex_32")]
    pub source_version: [u8; 32],
    /// Exact compiler input digest named by the authority observation.
    #[serde(with = "hex_32")]
    pub input_digest: [u8; 32],
    /// Exact durable authority observation sequence.
    pub observation_sequence: u64,
    /// Number of files in this profile's source frontier.
    pub source_count: u64,
    /// Typed state of the semantic refresh.
    pub state: IndexOperationSemanticProfileState,
}

/// Durable receipt for structural source admission, independent of semantic
/// publication and its later outcome.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IndexOperationSourceCaptureReceipt {
    /// Caller-owned operation identity bound into the source workspace intent.
    pub operation_key: IndexOperationKey,
    /// Exact selected source-capture commit identity.
    #[serde(with = "hex_32")]
    pub commit_identity: [u8; 32],
    /// Workspace root containing the structural source rows and capture marker.
    #[serde(with = "hex_32")]
    pub workspace_root: [u8; 32],
    /// Workspace sequence that selected the source capture.
    pub workspace_sequence: u64,
    /// Closed profiles, sorted by their canonical encoded identity.
    pub profiles: Box<[IndexOperationSourceProfile]>,
}

impl IndexOperationSourceCaptureReceipt {
    /// Admits a source receipt copied from the selected owner root.
    ///
    /// # Errors
    /// Rejects a mismatched key, reserved root identity, invalid sequence,
    /// empty/oversized profile list, duplicate profile, or malformed state.
    pub fn from_checked_parts(
        operation_key: IndexOperationKey,
        commit_identity: [u8; 32],
        workspace_root: [u8; 32],
        workspace_sequence: u64,
        profiles: Box<[IndexOperationSourceProfile]>,
    ) -> Result<Self, ProductAdmissionError> {
        if [commit_identity, workspace_root]
            .iter()
            .any(|identity| identity.iter().all(|byte| *byte == 0))
            || workspace_sequence == 0
            || profiles.is_empty()
            || profiles.len() > 16
            || profiles
                .windows(2)
                .any(|window| window[0].profile >= window[1].profile)
            || profiles.iter().any(|profile| {
                profile.source_version.iter().all(|byte| *byte == 0)
                    || profile.input_digest.iter().all(|byte| *byte == 0)
                    || profile.observation_sequence == 0
                    || !valid_index_operation_profile_state(profile.state)
            })
        {
            return Err(ProductAdmissionError::IndexOperationShape);
        }
        Ok(Self {
            operation_key,
            commit_identity,
            workspace_root,
            workspace_sequence,
            profiles,
        })
    }

    /// Returns the key bound by this structural root marker.
    #[must_use]
    pub const fn operation_key(&self) -> IndexOperationKey {
        self.operation_key
    }

    /// Returns the exact checked source-capture commit identity.
    #[must_use]
    pub const fn commit_identity(&self) -> &[u8; 32] {
        &self.commit_identity
    }

    /// Returns the workspace root containing the structural source rows.
    #[must_use]
    pub const fn workspace_root(&self) -> &[u8; 32] {
        &self.workspace_root
    }

    /// Returns the sequence that selected the source-capture root.
    #[must_use]
    pub const fn workspace_sequence(&self) -> u64 {
        self.workspace_sequence
    }

    /// Returns the exact per-profile capture and semantic states.
    #[must_use]
    pub fn profiles(&self) -> &[IndexOperationSourceProfile] {
        &self.profiles
    }

    fn admit(&self, operation_key: IndexOperationKey) -> Result<(), ProductAdmissionError> {
        if self.operation_key != operation_key {
            return Err(ProductAdmissionError::IndexOperationShape);
        }
        Self::from_checked_parts(
            self.operation_key,
            self.commit_identity,
            self.workspace_root,
            self.workspace_sequence,
            self.profiles.clone(),
        )
        .map(|_| ())
    }

    /// Admits the complete terminal partition planned before a partial commit.
    ///
    /// # Errors
    /// Rejects missing, duplicate, foreign, pending or contradictory outcomes.
    /// The receipt retains every exact source/input/observation tuple; this
    /// check establishes profile shape, never workspace publication authority.
    pub fn admit_partial_refusals(
        &self,
        refused_profiles: &[IndexOperationProfileRefusal],
    ) -> Result<(), ProductAdmissionError> {
        self.admit(self.operation_key)?;
        let profiles = self.profiles();
        let published = profiles
            .iter()
            .filter(|profile| {
                matches!(
                    profile.state,
                    IndexOperationSemanticProfileState::Published { .. }
                )
            })
            .count();
        if published == 0
            || published == profiles.len()
            || refused_profiles.len() != profiles.len() - published
            || refused_profiles
                .windows(2)
                .any(|pair| pair[0].profile >= pair[1].profile)
        {
            return Err(ProductAdmissionError::IndexOperationShape);
        }
        for profile in profiles {
            let expected_reason = match profile.state {
                IndexOperationSemanticProfileState::Published { .. } => continue,
                IndexOperationSemanticProfileState::Unavailable { reason }
                | IndexOperationSemanticProfileState::Failed { reason, .. } => reason,
                IndexOperationSemanticProfileState::Pending { .. } => {
                    return Err(ProductAdmissionError::IndexOperationShape);
                }
            };
            let refusal = refused_profiles
                .iter()
                .find(|refusal| refusal.profile == profile.profile)
                .ok_or(ProductAdmissionError::IndexOperationShape)?;
            if refusal.reason != expected_reason
                || refusal.reason == IndexOperationSemanticUnavailableReason::Cancelled
            {
                return Err(ProductAdmissionError::IndexOperationShape);
            }
            if let Some(failure) = &refusal.compiler_failure {
                if refusal.reason != IndexOperationSemanticUnavailableReason::Rejected {
                    return Err(ProductAdmissionError::IndexOperationShape);
                }
                failure.encode_bounded_json()?;
                if let PackageCompilerFailureCause::Authority {
                    diagnostic: Some(facts),
                    ..
                } = failure.cause()
                {
                    let language = profile.profile.profile()?.language();
                    if (facts.python_failure.is_some()
                        && language != backend_semantic::vocabulary::Language::Python)
                        || (facts.typescript_failure.is_some()
                            && language != backend_semantic::vocabulary::Language::TypeScript)
                        || matches!(
                            facts.python_failure,
                            Some(crate::interface::PythonAuthorityFailureKind::Cancelled)
                        )
                        || facts
                            .typescript_failure
                            .is_some_and(|failure| failure.is_cancelled())
                    {
                        return Err(ProductAdmissionError::IndexOperationShape);
                    }
                }
                if let PackageCompilerFailureCause::Toolchain {
                    language, stage, ..
                }
                | PackageCompilerFailureCause::ToolingUnavailable {
                    language, stage, ..
                }
                | PackageCompilerFailureCause::RequiredTool {
                    language, stage, ..
                } = failure.cause()
                    && (*language
                        != CompilerLanguageFact::from(profile.profile.profile()?.language())
                        || *stage != CompilerStageFact::LowerIr)
                {
                    return Err(ProductAdmissionError::IndexOperationShape);
                }
            }
        }
        Ok(())
    }
}

fn valid_index_operation_profile_state(state: IndexOperationSemanticProfileState) -> bool {
    let valid_prior = |prior: IndexOperationPriorSemantic| {
        prior.generation.iter().any(|byte| *byte != 0)
            && match prior.coverage {
                IndexOperationSemanticCoverage::Complete => true,
                IndexOperationSemanticCoverage::Partial { completed, total } => {
                    completed > 0 && total > completed
                }
            }
    };
    match state {
        IndexOperationSemanticProfileState::Pending { prior } => prior.is_none_or(valid_prior),
        IndexOperationSemanticProfileState::Unavailable { .. } => true,
        IndexOperationSemanticProfileState::Failed { prior, .. } => valid_prior(prior),
        IndexOperationSemanticProfileState::Published {
            generation,
            coverage,
        } => {
            generation.iter().any(|byte| *byte != 0)
                && match coverage {
                    IndexOperationSemanticCoverage::Complete => true,
                    IndexOperationSemanticCoverage::Partial { completed, total } => {
                        completed > 0 && total > completed
                    }
                }
        }
    }
}

/// One unavailable profile retained in an exact partial publication receipt.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IndexOperationProfileRefusal {
    /// Closed profile identity from the committed source capture.
    pub profile: SemanticLanguageProfile,
    /// Closed reason this profile did not publish a refreshed generation.
    pub reason: IndexOperationSemanticUnavailableReason,
    /// Source-bound compiler refusal when the compiler supplied one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compiler_failure: Option<PackageCompilerFailure>,
}

/// Durable state retained for one caller-owned index operation key.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", content = "detail", rename_all = "kebab-case")]
pub enum IndexOperationState {
    /// The owner durably accepted the request before starting work.
    Accepted,
    /// The owner is currently working on this exact operation.
    Active {
        /// Current process-local cancellation and progress ticket.
        ticket: IndexJobTicket,
        /// Current coarse work stage.
        stage: IndexJobStage,
    },
    /// The admitted workspace intent and its derived product view were durably published.
    Published(IndexOperationPublicationReceipt),
    /// The exact workspace/view commit selected useful profile generations
    /// and explicitly refused other captured profiles. This is terminal but
    /// never establishes complete semantic coverage of the application.
    PartiallyPublished {
        /// Exact committed workspace and derived view receipt.
        receipt: IndexOperationPublicationReceipt,
        /// Every failed/unavailable captured profile, in canonical order.
        refused_profiles: Box<[IndexOperationProfileRefusal]>,
    },
    /// The operation ended without publishing its requested mutation.
    Failed {
        /// Stable terminal failure category.
        reason: IndexOperationFailureReason,
        /// Bounded explanatory detail.
        detail: ProductText,
        /// Exact typed compiler refusal, when compilation produced one. Absence
        /// does not establish that compilation was attempted or succeeded.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        compiler_failure: Option<PackageCompilerFailure>,
    },
    /// The retained evidence cannot prove whether the exact publication completed.
    Unresolved {
        /// Stable reason the owner cannot prove a terminal result.
        reason: IndexOperationUnresolvedReason,
        /// Bounded explanatory detail.
        detail: ProductText,
    },
}

/// Why an operation ended without publishing its requested mutation.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum IndexOperationFailureReason {
    /// The exact operation was cancelled before commit.
    Cancelled,
    /// Source, compiler, or semantic admission refused the operation.
    Refused,
    /// The owner could not complete the operation before commit.
    WorkerFailed,
    /// A capacity refusal that was durably recorded for an admitted key.
    LedgerFull,
}

/// Why a retained operation lacks enough evidence for a terminal answer.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum IndexOperationUnresolvedReason {
    /// The owner restarted after recording the expected commit but before it
    /// could prove the resulting view publication.
    RestartedDuringPublication,
    /// Structural source capture committed, but the owner restarted before
    /// the semantic worker delivered a terminal result.
    SemanticWorkInterruptedAfterCapture,
    /// The selected workspace is neither the recorded base nor the exact
    /// request identity recorded before commit.
    WorkspaceEvidenceMismatch,
    /// The product view does not match the exact selected workspace head.
    ViewEvidenceMismatch,
    /// Durable terminal-receipt persistence failed after publication.
    ReceiptPersistenceFailed,
}

/// Exact durable index-operation status for one caller-owned key.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IndexOperationStatus {
    /// Caller-owned durable operation identity.
    pub operation_key: IndexOperationKey,
    /// Hash of the canonical package and execution request.
    #[serde(with = "hex_32")]
    pub request_digest: [u8; 32],
    /// Exact package requested by the caller.
    pub package: PackageReference,
    /// Requested compiler execution class.
    pub execution_intent: crate::CompileExecutionIntent,
    /// Current or terminal evidence for this operation.
    pub state: IndexOperationState,
    /// Structural source receipt with separate per-profile semantic results.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_capture: Option<IndexOperationSourceCaptureReceipt>,
}

impl IndexOperationStatus {
    /// Creates a status whose request digest is derived from its exact
    /// package and execution intent.
    #[must_use]
    pub fn new(
        operation_key: IndexOperationKey,
        package: PackageReference,
        execution_intent: crate::CompileExecutionIntent,
        state: IndexOperationState,
    ) -> Self {
        Self {
            operation_key,
            request_digest: index_operation_request_digest(&package, execution_intent),
            package,
            execution_intent,
            state,
            source_capture: None,
        }
    }

    /// Adds the exact source capture independently of the operation outcome.
    #[must_use]
    pub fn with_source_capture(
        mut self,
        source_capture: Option<IndexOperationSourceCaptureReceipt>,
    ) -> Self {
        self.source_capture = source_capture;
        self
    }
}

/// Keyed lookup result; absence is explicitly not success.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", content = "detail", rename_all = "kebab-case")]
pub enum IndexOperationObservation {
    /// The exact key was accepted and has retained evidence.
    Known(IndexOperationStatus),
    /// The key was consumed, but its full terminal receipt has aged out of the
    /// bounded detail window. It can never be accepted again.
    OutsideReceiptWindow {
        /// Key that was queried.
        operation_key: IndexOperationKey,
        /// Canonical package and execution request digest retained as a compact tombstone.
        #[serde(with = "hex_32")]
        request_digest: [u8; 32],
    },
    /// No retained operation has this key. The caller must not infer success.
    Unknown {
        /// Key that was queried.
        operation_key: IndexOperationKey,
    },
}

impl IndexOperationObservation {
    fn admit(&self) -> Result<(), ProductAdmissionError> {
        match self {
            Self::Unknown { .. } => Ok(()),
            Self::OutsideReceiptWindow { .. } => Ok(()),
            Self::Known(status) => {
                if let Some(source_capture) = &status.source_capture {
                    source_capture.admit(status.operation_key)?;
                    let any_pending = source_capture.profiles().iter().any(|profile| {
                        matches!(
                            profile.state,
                            IndexOperationSemanticProfileState::Pending { .. }
                        )
                    });
                    let any_published = source_capture.profiles().iter().any(|profile| {
                        matches!(
                            profile.state,
                            IndexOperationSemanticProfileState::Published { .. }
                        )
                    });
                    match &status.state {
                        IndexOperationState::Published(_) if any_pending => {
                            return Err(ProductAdmissionError::IndexOperationShape);
                        }
                        IndexOperationState::Failed { .. } if any_pending || any_published => {
                            return Err(ProductAdmissionError::IndexOperationShape);
                        }
                        _ => {}
                    }
                }
                if let IndexOperationState::PartiallyPublished {
                    receipt,
                    refused_profiles,
                } = &status.state
                {
                    let capture = status
                        .source_capture
                        .as_ref()
                        .ok_or(ProductAdmissionError::IndexOperationShape)?;
                    capture.admit_partial_refusals(refused_profiles)?;
                    if receipt.workspace_sequence() <= capture.workspace_sequence()
                        || receipt.request_identity().is_none()
                    {
                        return Err(ProductAdmissionError::IndexOperationShape);
                    }
                }
                if let IndexOperationState::Failed {
                    reason,
                    compiler_failure: Some(failure),
                    ..
                } = &status.state
                {
                    if *reason != IndexOperationFailureReason::Refused {
                        return Err(ProductAdmissionError::IndexOperationShape);
                    }
                    failure.encode_bounded_json()?;
                }
                match &status.state {
                    IndexOperationState::Active { ticket, .. }
                        if ticket.package() != &status.package =>
                    {
                        return Err(ProductAdmissionError::IndexOperationShape);
                    }
                    IndexOperationState::Published(receipt)
                    | IndexOperationState::PartiallyPublished { receipt, .. }
                        if IndexOperationPublicationReceipt::from_checked_parts(
                            receipt.request_identity().copied(),
                            *receipt.commit_identity(),
                            *receipt.workspace_root(),
                            receipt.workspace_sequence(),
                            *receipt.view_root(),
                            *receipt.view_version(),
                            *receipt.view_recipe(),
                            receipt.revision_cursor().to_vec().into_boxed_slice(),
                        )
                        .is_err() =>
                    {
                        return Err(ProductAdmissionError::IndexOperationShape);
                    }
                    _ => {}
                }
                if status.request_digest
                    != index_operation_request_digest(&status.package, status.execution_intent)
                {
                    return Err(ProductAdmissionError::IndexOperationShape);
                }
                Ok(())
            }
        }
    }
}

/// Canonical request digest shared by the client and durable owner.
#[must_use]
pub fn index_operation_request_digest(
    package: &PackageReference,
    execution_intent: crate::CompileExecutionIntent,
) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"backend.library.index-operation-request.v1\0");
    hasher.update(&[package.kind().tag()]);
    let bytes = package.as_str().as_bytes();
    hasher.update(&u64::try_from(bytes.len()).unwrap_or(u64::MAX).to_be_bytes());
    hasher.update(bytes);
    hasher.update(&[match execution_intent {
        crate::CompileExecutionIntent::Interactive => 1,
        crate::CompileExecutionIntent::Background => 2,
    }]);
    *hasher.finalize().as_bytes()
}

mod hex_32 {
    use serde::{Deserialize, Deserializer, Serializer, de};

    pub(super) fn serialize<S>(value: &[u8; 32], serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut hex = String::with_capacity(64);
        for byte in value {
            use core::fmt::Write as _;
            let _ = write!(hex, "{byte:02x}");
        }
        serializer.serialize_str(&hex)
    }

    pub(super) fn deserialize<'de, D>(deserializer: D) -> Result<[u8; 32], D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        parse(&value).ok_or_else(|| de::Error::custom("invalid hex identity"))
    }

    pub(super) fn parse(value: &str) -> Option<[u8; 32]> {
        if value.len() != 64 || value.bytes().any(|byte| byte.is_ascii_uppercase()) {
            return None;
        }
        let mut bytes = [0_u8; 32];
        for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
            let high = nibble(pair[0])?;
            let low = nibble(pair[1])?;
            bytes[index] = (high << 4) | low;
        }
        Some(bytes)
    }

    fn nibble(byte: u8) -> Option<u8> {
        match byte {
            b'0'..=b'9' => Some(byte - b'0'),
            b'a'..=b'f' => Some(byte - b'a' + 10),
            _ => None,
        }
    }
}

mod option_hex_32 {
    use serde::{Deserialize, Deserializer, Serializer, de};

    pub(super) fn serialize<S>(value: &Option<[u8; 32]>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match value {
            None => serializer.serialize_none(),
            Some(bytes) => {
                let mut hex = String::with_capacity(64);
                for byte in bytes {
                    use core::fmt::Write as _;
                    let _ = write!(hex, "{byte:02x}");
                }
                serializer.serialize_some(&hex)
            }
        }
    }

    pub(super) fn deserialize<'de, D>(deserializer: D) -> Result<Option<[u8; 32]>, D::Error>
    where
        D: Deserializer<'de>,
    {
        Option::<String>::deserialize(deserializer)?
            .map(|value| {
                super::hex_32::parse(&value)
                    .ok_or_else(|| de::Error::custom("invalid optional hex identity"))
            })
            .transpose()
    }
}

/// Coarse owner progress for one index job.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum IndexJobStage {
    /// A pinned package is being acquired from its configured registry.
    Acquiring,
    /// A verified registry archive is being extracted into owner staging.
    Staging,
    /// The source tree is being read and admitted.
    Scanning,
    /// The selected compiler profiles are running.
    Compiling,
    /// Admitted compiler output is crossing the product publication boundary.
    Publishing,
}

/// Immediate answer to an index start request.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", content = "data", rename_all = "kebab-case")]
pub enum IndexStartResult {
    /// The owner accepted the job and issued its exact cancellation ticket.
    Started {
        /// Owner-issued job ticket.
        ticket: IndexJobTicket,
        /// Current coarse progress stage.
        stage: IndexJobStage,
    },
    /// Scanning or compilation reached a terminal outcome before the reply.
    Terminal(IndexJobTerminal),
}

/// Truthful terminal receipt for one owner-managed index operation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IndexJobTerminal {
    /// Exact job and project this outcome belongs to.
    pub ticket: IndexJobTicket,
    /// What the owner durably did with the admitted job.
    pub outcome: IndexJobOutcome,
}

/// Compact, identity-bound refusal emitted for one concrete package source
/// member whose compact semantic fragment could not be built, written, or
/// independently reopened.
///
/// The original compiler authority remains the source and recipe identity;
/// `detail` is a bounded explanation only. The phase is derived from the
/// closed `kind` family and is repeated on the wire for direct presentation,
/// where deserialization verifies that the two agree.
#[derive(Clone)]
pub struct PackageCompilerFailure {
    relative_path: ProductText,
    source_identity: ContentId<SourceFactDomain>,
    source_byte_len: u32,
    recipe_identity: Option<ContentId<CompileRecipeDomain>>,
    cause: PackageCompilerFailureCause,
    detail: String,
    detail_truncated: bool,
    /// Native diagnostic prefix retained for in-process local debugging only.
    /// This field is intentionally omitted by the manual wire serializer.
    local_diagnostic: Option<Box<[u8]>>,
}

impl fmt::Debug for PackageCompilerFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PackageCompilerFailure")
            .field("relative_path", &self.relative_path)
            .field("source_identity", &self.source_identity)
            .field("source_byte_len", &self.source_byte_len)
            .field("recipe_identity", &self.recipe_identity)
            .field("cause", &self.cause)
            .field("detail", &self.detail)
            .field("detail_truncated", &self.detail_truncated)
            .field(
                "local_diagnostic_retained_bytes",
                &self.local_diagnostic.as_ref().map(|bytes| bytes.len()),
            )
            .finish()
    }
}

impl PartialEq for PackageCompilerFailure {
    fn eq(&self, other: &Self) -> bool {
        self.relative_path == other.relative_path
            && self.source_identity == other.source_identity
            && self.source_byte_len == other.source_byte_len
            && self.recipe_identity == other.recipe_identity
            && self.cause == other.cause
            && self.detail == other.detail
            && self.detail_truncated == other.detail_truncated
    }
}

impl Eq for PackageCompilerFailure {}

impl PackageCompilerFailure {
    /// Maximum serialized JSON representation, including worst-case path and
    /// detail escaping.
    pub const MAX_ENCODED_BYTES: usize = 8 * 1024;
    /// Maximum accepted package-relative member path in this refusal summary.
    pub const MAX_RELATIVE_PATH_BYTES: usize = 3_072;

    /// Builds a bounded summary from the exact package member and retained
    /// attempt authorities at the compiler terminal boundary.
    ///
    /// # Errors
    ///
    /// Returns [`ProductAdmissionError::PackageCompilerFailureShape`] when
    /// the package path is not canonical or a projected fact violates its
    /// fixed bound.
    pub fn from_fragment_failure(
        relative_path: &str,
        attempt: CompilerAttempt,
        failure: &CompilerFragmentFailure,
    ) -> Result<Self, ProductAdmissionError> {
        let relative_path = package_relative_source_path(relative_path)?;
        let fragment = PackageCompilerFragmentFaultFacts::new(failure.kind(), failure.facts())?;
        let cause = PackageCompilerFailureCause::Fragment(fragment);
        let (detail, detail_truncated) = detail_for_package_cause(&cause);
        let summary = Self {
            relative_path,
            source_identity: attempt.source.identity,
            source_byte_len: attempt.source.byte_len,
            recipe_identity: Some(attempt.recipe),
            cause,
            detail,
            detail_truncated,
            local_diagnostic: None,
        };
        summary.validate()?;
        Ok(summary)
    }

    /// Projects a package member's exact compiler terminal into a bounded
    /// structured refusal. Pre-recipe setup failures retain their source
    /// identity and truthfully omit a recipe that was never established.
    pub fn from_package_terminal(
        relative_path: &str,
        terminal: &crate::interface::CompilerTerminal,
    ) -> Result<Option<Self>, ProductAdmissionError> {
        let Some((recipe_identity, cause)) =
            package_compiler_failure::package_failure_from_terminal(terminal)?
        else {
            return Ok(None);
        };
        let source = match terminal {
            crate::interface::CompilerTerminal::Toolchain { source, .. }
            | crate::interface::CompilerTerminal::ToolingUnavailable { source, .. }
            | crate::interface::CompilerTerminal::RequiredTool { source, .. } => *source,
            crate::interface::CompilerTerminal::Compile { attempted, .. } => attempted.source,
            _ => return Ok(None),
        };
        let relative_path = package_relative_source_path(relative_path)?;
        let (detail, detail_truncated) = detail_for_package_cause(&cause);
        let local_diagnostic = match terminal {
            crate::interface::CompilerTerminal::Compile {
                cause:
                    crate::interface::CompilerCause::Authority {
                        diagnostic: Some(diagnostic),
                        ..
                    },
                ..
            } => Some(Box::<[u8]>::from(
                diagnostic.retained_bytes_for_local_debug(),
            )),
            _ => None,
        };
        let summary = Self {
            relative_path,
            source_identity: source.identity,
            source_byte_len: source.byte_len,
            recipe_identity,
            cause,
            detail,
            detail_truncated,
            local_diagnostic,
        };
        summary.validate()?;
        Ok(Some(summary))
    }

    /// Package-relative source member that reached the compiler terminal.
    #[must_use]
    pub fn relative_path(&self) -> &str {
        self.relative_path.as_str()
    }

    /// Exact compiler input content identity, retaining the SourceFact domain.
    #[must_use]
    pub const fn source_identity(&self) -> ContentId<SourceFactDomain> {
        self.source_identity
    }

    /// Exact byte extent bound into the failed compiler attempt.
    #[must_use]
    pub const fn source_byte_len(&self) -> u32 {
        self.source_byte_len
    }

    /// Exact recipe identity attempted for this source member.
    #[must_use]
    pub const fn recipe_identity(&self) -> Option<ContentId<CompileRecipeDomain>> {
        self.recipe_identity
    }

    /// Closed preparation, write, or validation phase derived from `kind`.
    #[must_use]
    pub const fn phase(&self) -> PackageCompilerFailurePhase {
        self.cause.phase()
    }

    /// Specific closed semantic/IR error variant.
    #[must_use]
    pub const fn cause(&self) -> &PackageCompilerFailureCause {
        &self.cause
    }

    /// Structured bounded cause retained for CLI, MCP, and operation receipts.
    #[must_use]
    pub const fn facts(&self) -> &PackageCompilerFailureCause {
        &self.cause
    }

    /// Returns the retained native diagnostic prefix for local debugging only.
    ///
    /// This accessor never participates in serialization. Callers must keep the
    /// returned bytes on the local diagnostic path and use [`Self::facts`] for
    /// CLI, MCP, receipts, and other public product surfaces. Deserialized
    /// summaries return `None`, because raw compiler output is intentionally not
    /// persisted on the wire.
    #[must_use]
    pub fn retained_diagnostic_for_local_debug(&self) -> Option<&[u8]> {
        self.local_diagnostic.as_deref()
    }

    /// Stable specific variant tag, including its phase family.
    #[must_use]
    pub const fn kind_tag(&self) -> &'static str {
        self.cause.kind_tag()
    }

    /// Native executable selected by the compiler registry for setup failures.
    #[must_use]
    pub const fn required_native_tool(&self) -> Option<CompilerNativeToolFact> {
        self.cause.required_native_tool()
    }

    /// Configured executable family when a selected-tool mismatch occurred.
    #[must_use]
    pub const fn configured_native_tool(&self) -> Option<CompilerNativeToolFact> {
        self.cause.configured_native_tool()
    }

    /// Whether the selected tool must be configured before retrying.
    #[must_use]
    pub const fn requires_tool_configuration(&self) -> bool {
        self.cause.requires_tool_configuration()
    }

    /// Exact auxiliary or native tool requirement that prevented setup.
    #[must_use]
    pub const fn required_tool_issue(&self) -> Option<crate::interface::CompilerToolIssue> {
        self.cause.required_tool_issue()
    }

    /// Exact environment variable to set or repair before retrying, when known.
    #[must_use]
    pub const fn required_configuration_variable(&self) -> Option<&'static str> {
        self.cause.required_configuration_variable()
    }

    /// Sanitized human explanation, capped at 384 UTF-8 bytes.
    #[must_use]
    pub fn detail(&self) -> &str {
        &self.detail
    }

    /// Whether the human explanation was shortened or unavailable.
    #[must_use]
    pub const fn detail_truncated(&self) -> bool {
        self.detail_truncated
    }

    /// Conservative encoded-size admission bound for capture and reply owners.
    #[must_use]
    pub const fn encoded_size_bound(&self) -> usize {
        Self::MAX_ENCODED_BYTES
    }

    /// Encodes this summary as canonical bounded JSON for operation receipts.
    ///
    /// # Errors
    ///
    /// Returns an admission error when the DTO is inconsistent, JSON
    /// serialization fails, or the encoded value exceeds the fixed cap.
    pub fn encode_bounded_json(&self) -> Result<Vec<u8>, ProductAdmissionError> {
        self.validate()?;
        let encoded = serde_json::to_vec(self)
            .map_err(|_| ProductAdmissionError::PackageCompilerFailureShape)?;
        if encoded.len() > Self::MAX_ENCODED_BYTES {
            return Err(ProductAdmissionError::PackageCompilerFailureShape);
        }
        Ok(encoded)
    }

    /// Decodes a bounded receipt payload and revalidates every closed field.
    ///
    /// # Errors
    ///
    /// Returns an admission error for oversized, malformed, unknown-field, or
    /// internally inconsistent input.
    pub fn decode_bounded_json(bytes: &[u8]) -> Result<Self, ProductAdmissionError> {
        if bytes.len() > Self::MAX_ENCODED_BYTES {
            return Err(ProductAdmissionError::PackageCompilerFailureShape);
        }
        serde_json::from_slice(bytes)
            .map_err(|_| ProductAdmissionError::PackageCompilerFailureShape)
    }

    fn validate(&self) -> Result<(), ProductAdmissionError> {
        if !is_canonical_package_relative_source_path(self.relative_path.as_str())
            || self.detail.len() > MAX_COMPILER_FRAGMENT_DETAIL_BYTES
            || self.detail.chars().any(char::is_control)
            || self
                .detail
                .chars()
                .any(|character| matches!(character, '"' | '\\'))
            || (self.detail.is_empty() && !self.detail_truncated)
            || !package_cause_is_valid(&self.cause)
            || (self.recipe_identity.is_none()
                != matches!(
                    &self.cause,
                    PackageCompilerFailureCause::Toolchain { .. }
                        | PackageCompilerFailureCause::ToolingUnavailable { .. }
                        | PackageCompilerFailureCause::RequiredTool { .. }
                ))
        {
            return Err(ProductAdmissionError::PackageCompilerFailureShape);
        }
        Ok(())
    }
}

fn compiler_fault_facts_match_kind(
    kind: CompilerFragmentFaultKind,
    facts: CompilerFragmentFaultFacts,
) -> bool {
    use crate::interface::{
        BuildFaultKind as B, CompilerFragmentNestedFaultKind as N, PrepareFaultKind as P,
        ValidateFaultKind as V, WriteFaultKind as W,
    };
    use CompilerFragmentFaultFacts as F;
    match kind {
        CompilerFragmentFaultKind::Build(B::InvalidTreeEntity) => {
            matches!(facts, F::TreeEntity { .. })
        }
        CompilerFragmentFaultKind::Build(B::Dangling) => matches!(facts, F::Dangling { .. }),
        CompilerFragmentFaultKind::Build(B::InvalidOccurrenceSpan) => {
            matches!(facts, F::OccurrenceSpan { .. })
        }
        CompilerFragmentFaultKind::Build(B::SignatureCarrierRoleCount) => {
            nested_fault_is(facts, N::SignatureCarrierRoleCount)
        }
        CompilerFragmentFaultKind::Build(B::SignatureCarrierRoleKind) => {
            nested_fault_is(facts, N::SignatureCarrierRoleKind)
        }
        CompilerFragmentFaultKind::Build(B::SignatureCarrierRoleOwnerKind) => {
            nested_fault_is(facts, N::SignatureCarrierRoleOwnerKind)
        }
        CompilerFragmentFaultKind::Build(B::SignatureCarrierBindingOwnerSet) => {
            nested_fault_is(facts, N::SignatureCarrierBindingOwnerSet)
        }
        CompilerFragmentFaultKind::Build(B::SignatureCarrierBindingSignature) => {
            nested_fault_is(facts, N::SignatureCarrierBindingSignature)
        }
        CompilerFragmentFaultKind::Build(B::SignatureCarrierBindingCounts) => {
            nested_fault_is(facts, N::SignatureCarrierBindingCounts)
        }
        CompilerFragmentFaultKind::Build(B::SignatureCarrierBindingEdgeRole) => {
            nested_fault_is(facts, N::SignatureCarrierBindingEdgeRole)
        }
        CompilerFragmentFaultKind::Build(B::SignatureCarrierBindingTargetCount) => {
            nested_fault_is(facts, N::SignatureCarrierBindingTargetCount)
        }
        CompilerFragmentFaultKind::Build(B::SignatureCarrierBindingTargetKind) => {
            nested_fault_is(facts, N::SignatureCarrierBindingTargetKind)
        }
        CompilerFragmentFaultKind::Build(B::SignatureCarrierBindingType) => {
            nested_fault_is(facts, N::SignatureCarrierBindingType)
        }
        CompilerFragmentFaultKind::Build(B::SignatureCarrierBindingEdgeMismatch) => {
            nested_fault_is(facts, N::SignatureCarrierBindingEdgeMismatch)
        }
        CompilerFragmentFaultKind::Build(
            B::AnonymousCallableName
            | B::TypedDeclarationKey
            | B::AnonymousCallableSourceUnavailable
            | B::AnonymousCallableAnchorInvalid
            | B::AnonymousCallableSourceCoordinate
            | B::AnonymousCallableInstance
            | B::Capacity
            | B::RecursiveType
            | B::CallableElement
            | B::MissingTypedVariadicParameter
            | B::EmptyQualifiedPath
            | B::TypeParameterRequirements
            | B::EmptyCxxQualification
            | B::IllegalCQualifierTarget
            | B::IllegalCxxMemberPointerOwner
            | B::InvalidDocumentationUtf8
            | B::SignatureCarrierRoleAlreadyCaptured
            | B::SignatureCarrierBindingsAlreadyCaptured
            | B::ParentCycle
            | B::DeclarationKey
            | B::ScopedDeclarationPreimage
            | B::ForeignKeyPreimage
            | B::DuplicateDeclarationIdentity
            | B::TreeVersionCount
            | B::AuthorityRowCount
            | B::OccurrenceAuthorityRowCount
            | B::AuthorityFacts
            | B::OccurrenceAuthorityFacts
            | B::ImageProvenanceLineage
            | B::ImageProvenanceScope
            | B::ImageProvenanceScopePreimage
            | B::ImageProvenanceRecipe
            | B::ImageProvenanceRebind
            | B::LanguageExtension
            | B::LanguageProfileMismatch
            | B::LanguageProfileRebind,
        ) => matches!(facts, F::None),
        CompilerFragmentFaultKind::Prepare(P::Count) => matches!(facts, F::Count { .. }),
        CompilerFragmentFaultKind::Prepare(P::AtomBytePoolOverflow) => {
            matches!(
                facts,
                F::RecordCoordinate {
                    lane: crate::interface::CompilerFragmentRecordLane::Atom,
                    ..
                }
            )
        }
        CompilerFragmentFaultKind::Prepare(P::LayoutOverflow) => {
            matches!(facts, F::LayoutOverflow { .. })
        }
        CompilerFragmentFaultKind::Prepare(P::NativeCount) => {
            matches!(facts, F::NativeCount { .. })
        }
        CompilerFragmentFaultKind::Prepare(P::OutputLength) => {
            matches!(facts, F::OutputLength { .. })
        }
        CompilerFragmentFaultKind::Prepare(P::Entity) => nested_fault_is_one_of(
            facts,
            &[
                N::EntityTypeReference,
                N::EntityNameReference,
                N::EntityKindTag,
                N::EntityReservedBits,
            ],
        ),
        CompilerFragmentFaultKind::Prepare(P::TypeNode) => nested_fault_is_one_of(
            facts,
            &[
                N::TypeNodeReservedBytes,
                N::TypeNodeTag,
                N::TypeNodePrimitive,
                N::TypeNodeEdge,
            ],
        ),
        CompilerFragmentFaultKind::Prepare(P::SemanticData) => nested_fault_is_one_of(
            facts,
            &[
                N::CanonicalDataCount,
                N::CanonicalDataNativeCount,
                N::CanonicalDataNativeWork,
                N::CanonicalDataCanonicalCountOverflow,
                N::CanonicalDataCanonicalCountMismatch,
                N::CanonicalDataScratch,
                N::CanonicalDataOutputTooSmall,
                N::CanonicalDataProductHead,
                N::CanonicalDataProductList,
                N::CanonicalDataConstructorCount,
                N::CanonicalDataConstructorTag,
                N::CanonicalDataConstructorReservedPayload,
                N::CanonicalDataConstructorArityOverflow,
                N::CanonicalDataConstructorArity,
                N::CanonicalDataProductChildRole,
                N::CanonicalDataListExtent,
                N::CanonicalDataNativeExtent,
                N::CanonicalDataProductChild,
                N::CanonicalDataOutputLength,
                N::CanonicalDataCanonicalListExtent,
                N::CanonicalDataCanonicalAtom,
                N::CanonicalDataCanonicalProduct,
                N::CanonicalDataCanonicalList,
                N::CanonicalDataRefinementBound,
                N::CanonicalDataInternTableFull,
                N::CanonicalDataInternEntry,
                N::CanonicalDataResourceCounterOverflow,
                N::CanonicalDataBudgetAdmission,
                N::CanonicalDataBudgetExceeded,
            ],
        ),
        CompilerFragmentFaultKind::Prepare(P::SemanticDataOverflow) => {
            matches!(facts, F::SemanticDataOverflow { .. })
        }
        CompilerFragmentFaultKind::Prepare(P::SemanticEntityRoots) => {
            matches!(facts, F::SemanticEntityRoots { .. })
        }
        CompilerFragmentFaultKind::Prepare(P::SemanticAtomLength) => {
            matches!(facts, F::AtomLength { .. })
        }
        CompilerFragmentFaultKind::Prepare(P::OccurrenceLane) => nested_fault_is_one_of(
            facts,
            &[
                N::OccurrenceOwner,
                N::OccurrenceLocalTarget,
                N::OccurrenceTargetTag,
                N::OccurrenceOriginTag,
                N::OccurrenceReferenceKind,
                N::OccurrenceConfidence,
                N::OccurrenceSpan,
                N::OccurrenceKindCell,
                N::OccurrenceEmptyPath,
                N::OccurrenceTruncated,
                N::OccurrenceTrailingBytes,
                N::OccurrenceLegacyStableTarget,
                N::OccurrenceAuthorityDomain,
                N::OccurrenceAuthorityWidth,
            ],
        ),
        CompilerFragmentFaultKind::Prepare(
            P::TypeFacts | P::Documentation | P::ExtensionPools | P::ExtensionPoolsMismatch,
        ) => matches!(facts, F::None),
        CompilerFragmentFaultKind::Write(W::OutputTooSmall) => {
            matches!(facts, F::OutputTooSmall { .. })
        }
        CompilerFragmentFaultKind::Write(W::AtomLength | W::SemanticAtomLength) => {
            matches!(facts, F::AtomLength { .. })
        }
        CompilerFragmentFaultKind::Write(W::AtomExtent) => {
            matches!(facts, F::AtomCoordinate { .. })
        }
        CompilerFragmentFaultKind::Write(W::ExtensionSection) => matches!(facts, F::None),
        CompilerFragmentFaultKind::Validate(V::Entity) => {
            nested_fault_is(facts, N::EntityTypeReference)
        }
        CompilerFragmentFaultKind::Validate(V::EntityRecord) => nested_fault_is_one_of(
            facts,
            &[
                N::EntityTypeReference,
                N::EntityNameReference,
                N::EntityKindTag,
                N::EntityReservedBits,
            ],
        ),
        CompilerFragmentFaultKind::Validate(V::Atom) => {
            nested_fault_is_one_of(facts, &[N::AtomRange, N::AtomEmpty])
        }
        CompilerFragmentFaultKind::Validate(V::TypeNode) => nested_fault_is_one_of(
            facts,
            &[
                N::TypeNodeReservedBytes,
                N::TypeNodeTag,
                N::TypeNodePrimitive,
                N::TypeNodeEdge,
            ],
        ),
        CompilerFragmentFaultKind::Validate(V::SemanticData) => nested_fault_is_one_of(
            facts,
            &[
                N::SemanticDataHeader,
                N::SemanticDataAtomLength,
                N::SemanticDataProductHead,
                N::SemanticDataProductList,
                N::SemanticDataConstructorCount,
                N::SemanticDataEntityRootCount,
                N::SemanticDataEntityRoot,
                N::SemanticDataConstructorTag,
                N::SemanticDataConstructorReservedPayload,
                N::SemanticDataConstructorArityOverflow,
                N::SemanticDataConstructorArity,
                N::SemanticDataListExtent,
                N::SemanticDataChildRoleCode,
                N::SemanticDataChildRole,
                N::SemanticDataChildTag,
                N::SemanticDataLocalChild,
                N::SemanticDataLocalReserved,
                N::SemanticDataExternalAuthority,
                N::SemanticDataTrailing,
            ],
        ),
        CompilerFragmentFaultKind::Validate(V::Occurrences) => nested_fault_is_one_of(
            facts,
            &[
                N::OccurrenceOwner,
                N::OccurrenceLocalTarget,
                N::OccurrenceTargetTag,
                N::OccurrenceOriginTag,
                N::OccurrenceReferenceKind,
                N::OccurrenceConfidence,
                N::OccurrenceSpan,
                N::OccurrenceKindCell,
                N::OccurrenceEmptyPath,
                N::OccurrenceTruncated,
                N::OccurrenceTrailingBytes,
                N::OccurrenceLegacyStableTarget,
                N::OccurrenceAuthorityDomain,
                N::OccurrenceAuthorityWidth,
            ],
        ),
        CompilerFragmentFaultKind::Validate(V::TruncatedHeader) => {
            nested_fault_is(facts, N::ValidateTruncatedHeader)
        }
        CompilerFragmentFaultKind::Validate(V::Magic) => nested_fault_is(facts, N::ValidateMagic),
        CompilerFragmentFaultKind::Validate(V::Schema) => nested_fault_is(facts, N::ValidateSchema),
        CompilerFragmentFaultKind::Validate(V::DeclaredLength) => {
            nested_fault_is(facts, N::ValidateDeclaredLength)
        }
        CompilerFragmentFaultKind::Validate(V::Extent) => nested_fault_is(facts, N::ValidateExtent),
        CompilerFragmentFaultKind::Validate(V::WireWidth) => {
            nested_fault_is(facts, N::ValidateWireWidth)
        }
        CompilerFragmentFaultKind::Validate(
            V::ExtensionPoolPair
            | V::Documentation
            | V::ExtensionPools
            | V::LanguageExtensions
            | V::Directory
            | V::MissingSection
            | V::SourceIdentity
            | V::RecipeFact
            | V::TypeFacts,
        ) => matches!(facts, F::None),
    }
}

fn nested_fault_is(
    facts: CompilerFragmentFaultFacts,
    expected: crate::interface::CompilerFragmentNestedFaultKind,
) -> bool {
    matches!(facts, CompilerFragmentFaultFacts::Nested { fault } if fault.kind() == expected)
}

fn nested_fault_is_one_of(
    facts: CompilerFragmentFaultFacts,
    expected: &[crate::interface::CompilerFragmentNestedFaultKind],
) -> bool {
    matches!(facts, CompilerFragmentFaultFacts::Nested { fault } if expected.contains(&fault.kind()))
}

fn package_relative_source_path(value: &str) -> Result<ProductText, ProductAdmissionError> {
    if value.trim() != value || !is_canonical_package_relative_source_path(value) {
        return Err(ProductAdmissionError::PackageCompilerFailureShape);
    }
    ProductText::new(value.to_owned())
        .map_err(|_| ProductAdmissionError::PackageCompilerFailureShape)
}

fn sanitize_package_compiler_detail(value: &str, source_truncated: bool) -> (String, bool) {
    let mut retained = String::new();
    if retained
        .try_reserve_exact(MAX_COMPILER_FRAGMENT_DETAIL_BYTES)
        .is_err()
    {
        return (retained, true);
    }
    let mut truncated = source_truncated;
    for character in value.chars() {
        let character = if character.is_control() || matches!(character, '"' | '\\') {
            ' '
        } else {
            character
        };
        if retained.len().saturating_add(character.len_utf8()) > MAX_COMPILER_FRAGMENT_DETAIL_BYTES
        {
            truncated = true;
            break;
        }
        retained.push(character);
    }
    (retained, truncated)
}

fn detail_for_package_cause(cause: &PackageCompilerFailureCause) -> (String, bool) {
    if let PackageCompilerFailureCause::Authority {
        diagnostic:
            Some(CompilerAuthorityDiagnosticFacts {
                python_failure: Some(failure),
                ..
            }),
        ..
    } = cause
    {
        return sanitize_package_compiler_detail(failure.detail(), false);
    }
    if let PackageCompilerFailureCause::Authority {
        diagnostic:
            Some(CompilerAuthorityDiagnosticFacts {
                typescript_failure: Some(failure),
                ..
            }),
        ..
    } = cause
    {
        return sanitize_package_compiler_detail(failure.detail(), false);
    }
    let prefix = match cause {
        PackageCompilerFailureCause::Fragment(_) => "compact fragment fault",
        PackageCompilerFailureCause::Toolchain {
            selected,
            configured: None,
            ..
        } => {
            let text = format!(
                "{} is selected but is not configured; set {} or configure the project-local tool at {}",
                selected.executable(),
                selected.configuration_variable(),
                selected
                    .project_local_path()
                    .unwrap_or("a valid executable path")
            );
            return sanitize_package_compiler_detail(&text, false);
        }
        PackageCompilerFailureCause::Toolchain {
            selected,
            configured: Some(configured),
            ..
        } => {
            let text = format!(
                "{} is required but {} is configured; set {} to the selected tool path",
                selected.executable(),
                configured.executable(),
                selected.configuration_variable()
            );
            return sanitize_package_compiler_detail(&text, false);
        }
        PackageCompilerFailureCause::ToolingUnavailable { tool, .. } => {
            let local_path = tool
                .project_local_path()
                .unwrap_or("a valid executable path");
            let text = format!(
                "{} is unavailable; configure {} with a valid tool path, for example {}",
                tool.executable(),
                tool.configuration_variable(),
                local_path,
            );
            return sanitize_package_compiler_detail(&text, false);
        }
        PackageCompilerFailureCause::RequiredTool { issue, .. } => {
            let (requirement, action) = match (issue.requirement, issue.failure) {
                (CompilerToolRequirement::PythonChecker, CompilerToolFailure::Missing) => (
                    "pyrefly is required for Python semantic checking",
                    "set NUDOX_PYREFLY to its absolute executable path",
                ),
                (CompilerToolRequirement::PythonChecker, CompilerToolFailure::ProbeFailed) => (
                    "the configured pyrefly checker failed its bounded --version probe",
                    "verify the NUDOX_PYREFLY executable and retry",
                ),
                (CompilerToolRequirement::Native(_), CompilerToolFailure::Missing) => (
                    "a required compiler executable is not configured",
                    "set the exact NUDOX compiler variable to its absolute executable path",
                ),
                (CompilerToolRequirement::Native(_), CompilerToolFailure::ProbeFailed) => (
                    "the configured compiler executable failed its bounded --version probe",
                    "verify the selected executable and retry",
                ),
            };
            let text = format!("{requirement}; {action}");
            return sanitize_package_compiler_detail(&text, false);
        }
        PackageCompilerFailureCause::Lowering(_) => "lowering fault",
        PackageCompilerFailureCause::Authority { phase, .. } => match phase {
            AuthorityPhaseFact::Open => "open authority fault",
            AuthorityPhaseFact::Parse => "parse authority fault",
            AuthorityPhaseFact::Resolve => "resolve authority fault",
            AuthorityPhaseFact::TypeCheck => "type check authority fault",
            AuthorityPhaseFact::Project => "project authority fault",
        },
    };
    let tag = cause.kind_tag().replace('_', " ");
    let text = format!("{prefix}: {tag}");
    sanitize_package_compiler_detail(&text, false)
}

fn package_cause_is_valid(cause: &PackageCompilerFailureCause) -> bool {
    match cause {
        PackageCompilerFailureCause::Fragment(fault) => {
            compiler_fault_facts_match_kind(fault.kind(), fault.facts())
        }
        PackageCompilerFailureCause::Toolchain {
            selected,
            configured,
            ..
        } => configured.is_none_or(|configured| *selected != configured),
        PackageCompilerFailureCause::ToolingUnavailable { .. }
        | PackageCompilerFailureCause::RequiredTool { .. }
        | PackageCompilerFailureCause::Lowering(_) => true,
        PackageCompilerFailureCause::Authority {
            diagnostic: Some(facts),
            ..
        } => !(facts.python_failure.is_some() && facts.typescript_failure.is_some()),
        PackageCompilerFailureCause::Authority {
            diagnostic: None, ..
        } => true,
    }
}

fn is_canonical_package_relative_source_path(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= PackageCompilerFailure::MAX_RELATIVE_PATH_BYTES
        && !value.starts_with('/')
        && !value.contains('\\')
        && !value.chars().any(char::is_control)
        && value
            .split('/')
            .all(|component| !component.is_empty() && component != "." && component != "..")
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PackageCompilerFailureWire {
    relative_path: String,
    source_identity: String,
    source_byte_len: u32,
    recipe_identity: Option<String>,
    phase: PackageCompilerFailurePhase,
    kind_tag: String,
    cause: PackageCompilerFailureCause,
    detail: String,
    detail_truncated: bool,
}

struct ContentIdDisplay<'a, DomainTag>(&'a ContentId<DomainTag>);

impl<DomainTag: Domain> Serialize for ContentIdDisplay<'_, DomainTag> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self.0)
    }
}

impl Serialize for PackageCompilerFailure {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut state = serializer.serialize_struct("PackageCompilerFailure", 9)?;
        state.serialize_field("relative_path", &self.relative_path)?;
        state.serialize_field("source_identity", &ContentIdDisplay(&self.source_identity))?;
        state.serialize_field("source_byte_len", &self.source_byte_len)?;
        state.serialize_field(
            "recipe_identity",
            &self.recipe_identity.as_ref().map(ContentIdDisplay),
        )?;
        state.serialize_field("phase", &self.phase())?;
        state.serialize_field("kind_tag", self.kind_tag())?;
        state.serialize_field("cause", &self.cause)?;
        state.serialize_field("detail", &self.detail)?;
        state.serialize_field("detail_truncated", &self.detail_truncated)?;
        state.end()
    }
}

impl<'de> Deserialize<'de> for PackageCompilerFailure {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let wire = PackageCompilerFailureWire::deserialize(deserializer)?;
        let relative_path =
            package_relative_source_path(&wire.relative_path).map_err(D::Error::custom)?;
        let source_identity = parse_content_identity::<SourceFactDomain>(&wire.source_identity)
            .map_err(D::Error::custom)?;
        let recipe_identity = wire
            .recipe_identity
            .as_deref()
            .map(parse_content_identity::<CompileRecipeDomain>)
            .transpose()
            .map_err(D::Error::custom)?;
        let summary = Self {
            relative_path,
            source_identity,
            source_byte_len: wire.source_byte_len,
            recipe_identity,
            cause: wire.cause,
            detail: wire.detail,
            detail_truncated: wire.detail_truncated,
            local_diagnostic: None,
        };
        summary.validate().map_err(D::Error::custom)?;
        if wire.phase != summary.phase() || wire.kind_tag != summary.kind_tag() {
            return Err(D::Error::custom(
                ProductAdmissionError::PackageCompilerFailureShape,
            ));
        }
        if summary.recipe_identity.is_none()
            != matches!(
                &summary.cause,
                PackageCompilerFailureCause::Toolchain { .. }
                    | PackageCompilerFailureCause::ToolingUnavailable { .. }
                    | PackageCompilerFailureCause::RequiredTool { .. }
            )
        {
            return Err(D::Error::custom(
                ProductAdmissionError::PackageCompilerFailureShape,
            ));
        }
        Ok(summary)
    }
}

fn parse_content_identity<DomainTag: Domain>(
    value: &str,
) -> Result<ContentId<DomainTag>, &'static str> {
    let encoded = value
        .strip_prefix("content:")
        .ok_or("content identity prefix is invalid")?;
    if encoded.len() != HASH_BYTES * 2 {
        return Err("content identity width is invalid");
    }
    let mut bytes = [0; HASH_BYTES];
    for (index, pair) in encoded.as_bytes().chunks_exact(2).enumerate() {
        let high = lower_hex_nibble(pair[0]).ok_or("content identity is not lowercase hex")?;
        let low = lower_hex_nibble(pair[1]).ok_or("content identity is not lowercase hex")?;
        bytes[index] = (high << 4) | low;
    }
    ContentId::<DomainTag>::try_from(bytes).map_err(|_| "content identity domain is invalid")
}

fn lower_hex_nibble(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        _ => None,
    }
}

/// Terminal effect of one index job.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "state",
    content = "detail",
    rename_all = "kebab-case",
    deny_unknown_fields
)]
pub enum IndexJobOutcome {
    /// Semantic admission and the owner publication completed.
    Published,
    /// The candidate was refused and did not replace the prior publication.
    Refused(ProductText),
    /// A package compiler refusal with a typed, source-bound cause.
    RefusedWithCompilerFailure {
        /// Human-readable refusal retained for older surface presenters.
        detail: ProductText,
        /// Exact package member, compiler authority, and closed compact-fragment cause.
        failure: PackageCompilerFailure,
    },
    /// The requested cancellation was observed before publication completed.
    Cancelled,
    /// The owner could not establish a terminal publication result.
    Failed(ProductText),
}

impl IndexJobOutcome {
    fn admit(&self) -> Result<(), ProductAdmissionError> {
        if let Self::RefusedWithCompilerFailure { failure, .. } = self {
            failure.validate()?;
        }
        Ok(())
    }
}

impl IndexJobTerminal {
    fn admit(&self) -> Result<(), ProductAdmissionError> {
        self.outcome.admit()
    }
}

impl IndexStartResult {
    fn admit(&self) -> Result<(), ProductAdmissionError> {
        if let Self::Terminal(terminal) = self {
            terminal.admit()?;
        }
        Ok(())
    }
}

/// Immediate result of a cancellation request.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", content = "terminal", rename_all = "kebab-case")]
pub enum IndexCancelStatus {
    /// The exact active job received a cancellation request.
    Requested,
    /// The job had already reached a terminal state.
    Terminal(IndexJobTerminal),
    /// No active or retained terminal job matched the exact ticket.
    Unknown,
}

/// Ticket-bound result of one index cancellation request.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IndexCancelReceipt {
    /// Exact job requested by the caller.
    pub ticket: IndexJobTicket,
    /// Immediate owner result for that exact job.
    pub status: IndexCancelStatus,
}

impl IndexCancelReceipt {
    fn admit(&self) -> Result<(), ProductAdmissionError> {
        if let IndexCancelStatus::Terminal(terminal) = &self.status {
            if terminal.ticket != self.ticket {
                return Err(ProductAdmissionError::IndexCancelTicketMismatch);
            }
            terminal.admit()?;
        }
        Ok(())
    }
}

/// One typed milestone emitted while an owner-managed index job is running.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "kebab-case", deny_unknown_fields)]
pub enum IndexJobProgressKind {
    /// The owner entered a new bounded operation stage.
    StageChanged {
        /// Current operation stage.
        stage: IndexJobStage,
    },
    /// One exact semantic compiler profile was admitted to the compiler owner.
    ProfileStarted {
        /// Exact profile selected from the admitted package sources.
        profile: SemanticLanguageProfile,
        /// One-based profile position in this job.
        ordinal: u16,
        /// Number of profiles admitted for this job.
        total: u16,
    },
    /// One exact semantic compiler profile finished and its immutable candidate was staged.
    ProfileAdmitted {
        /// Exact profile selected from the admitted package sources.
        profile: SemanticLanguageProfile,
        /// One-based profile position in this job.
        ordinal: u16,
        /// Number of profiles admitted for this job.
        total: u16,
    },
}

impl IndexJobProgressKind {
    fn admit(&self) -> Result<(), ProductAdmissionError> {
        match self {
            Self::StageChanged { .. } => Ok(()),
            Self::ProfileStarted {
                profile,
                ordinal,
                total,
            }
            | Self::ProfileAdmitted {
                profile,
                ordinal,
                total,
            } => {
                profile
                    .profile()
                    .map_err(|_| ProductAdmissionError::IndexProgressShape)?;
                if *ordinal > 0 && *ordinal <= *total {
                    Ok(())
                } else {
                    Err(ProductAdmissionError::IndexProgressShape)
                }
            }
        }
    }
}

/// One sequence-numbered owner progress fact, scoped to an exact job ticket.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IndexJobProgressEvent {
    /// Exact job and package the event belongs to.
    pub ticket: IndexJobTicket,
    /// Strictly increasing sequence within this job.
    pub sequence: u64,
    /// Typed stage or profile milestone.
    pub kind: IndexJobProgressKind,
}

impl IndexJobProgressEvent {
    fn admit(&self) -> Result<(), ProductAdmissionError> {
        if self.sequence == 0 {
            return Err(ProductAdmissionError::IndexProgressShape);
        }
        self.kind.admit()
    }
}

/// One bounded page from the owner's retained index progress stream.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IndexProgressPage {
    /// Exact job and package the event page belongs to.
    pub ticket: IndexJobTicket,
    /// Current stage of the admitted job.
    pub stage: IndexJobStage,
    /// Ordered progress events after the caller's cursor.
    pub events: Box<[IndexJobProgressEvent]>,
    /// Cursor after the last event in this page (or the input cursor when empty).
    pub next_sequence: u64,
    /// True when older events before this page have aged out of the bounded owner buffer.
    pub truncated: bool,
    /// True when more events after this page are immediately available.
    pub has_more: bool,
}

impl IndexProgressPage {
    fn admit(&self) -> Result<(), ProductAdmissionError> {
        if self.events.len() > MAX_INDEX_PROGRESS_EVENTS {
            return Err(ProductAdmissionError::RowBound);
        }
        let mut prior_sequence = 0_u64;
        for event in &self.events {
            event.admit()?;
            if event.ticket != self.ticket || event.sequence <= prior_sequence {
                return Err(ProductAdmissionError::IndexProgressShape);
            }
            prior_sequence = event.sequence;
        }
        if prior_sequence > self.next_sequence
            || (!self.events.is_empty() && prior_sequence != self.next_sequence)
            || (self.has_more && self.events.len() != MAX_INDEX_PROGRESS_EVENTS)
        {
            return Err(ProductAdmissionError::IndexProgressShape);
        }
        Ok(())
    }
}

/// Immediate observation of one exact owner index ticket.
///
/// Polling never holds the owner connection while work runs. A caller can
/// advance the cursor from `Pending` pages until it observes a terminal
/// receipt or an unknown ticket after retention expiry/owner restart.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", content = "detail", rename_all = "kebab-case")]
pub enum IndexJobObservation {
    /// The job is still active and has a bounded progress page.
    Pending(IndexProgressPage),
    /// The exact ticket reached a durable terminal outcome.
    Terminal(IndexJobTerminal),
    /// The ticket is no longer active or retained by this owner process.
    Unknown {
        /// Ticket the caller asked about.
        ticket: IndexJobTicket,
        /// Current owner epoch, which differs from the ticket's epoch after a restart.
        current_owner_epoch: [u8; 16],
    },
}

impl IndexJobObservation {
    fn admit(&self) -> Result<(), ProductAdmissionError> {
        match self {
            Self::Pending(page) => page.admit(),
            Self::Terminal(terminal) => terminal.admit(),
            Self::Unknown { .. } => Ok(()),
        }
    }
}

/// Exact request for one daemon-owned product operation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "kebab-case", deny_unknown_fields)]
pub enum SurfaceCommand {
    /// Read the complete advisory decision for one exact package version.
    Advisory {
        /// Package locator.
        package: PackageReference,
        /// Optional checked override for a blocked decision.
        override_evidence: Option<OverrideEvidence>,
    },
    /// Read several declarations independently.
    Read {
        /// Canonical declaration labels.
        locators: Box<[ProductText]>,
    },
    /// Find the source-verified sites where one declaration is used.
    References {
        /// Exact declaration coordinate whose uses are requested.
        target: ProductText,
    },
    /// Compare declaration sets between package versions.
    Diff {
        /// Older package.
        from: PackageReference,
        /// Newer package.
        to: PackageReference,
    },
    /// Browse the local registry catalog.
    Explore {
        /// Optional case-insensitive filter.
        query: Option<ProductText>,
        /// Bounded page size.
        limit: u16,
    },
    /// Read one local registry package.
    Package {
        /// Package locator.
        package: PackageReference,
    },
    /// Acquire a project or package directly from a pinned code-forge source.
    ForgeAdd {
        /// Canonical forge URL with an explicit tag, branch, or commit.
        coordinate: ProductText,
    },
    /// Reference an already acquired code-forge source from the local cache.
    ForgeReference {
        /// Canonical forge URL with an explicit tag, branch, or commit.
        coordinate: ProductText,
    },
    /// Read reverse dependency metadata.
    Dependents {
        /// Package locator.
        package: PackageReference,
    },
    /// Read outgoing dependency metadata.
    Dependencies {
        /// Package locator.
        package: PackageReference,
    },
    /// Read one root and facts-witness-bound dependency graph page.
    PackageGraphPage {
        /// Bounded, authority-aware page request.
        request: crate::PackageGraphPageRequest,
    },
    /// Read packages associated with one publisher.
    Owner {
        /// Registry publisher handle.
        owner: ProductText,
    },
    /// Search the local registry catalog.
    IndexSearch {
        /// Search query.
        query: ProductText,
        /// Bounded page size.
        limit: u16,
        /// Opaque continuation returned by the prior page.
        cursor: Option<IndexSearchCursor>,
    },
    /// Read versions recorded under one package name.
    PackageVersions {
        /// Package locator.
        package: PackageReference,
    },
    /// Read immutable compiler generations retained for one package.
    SemanticVersions {
        /// Exact product package reference.
        package: PackageReference,
    },
    /// Read compiler-owned member/type shapes for exact selected keys.
    SemanticShapes {
        /// Source selection and bounded full-key operands.
        request: crate::SemanticShapeReadRequest,
    },
    /// Read one snapshot-bound page of exact selected local Project File rows.
    PackageSourceMembership {
        /// Exact package, expected selected root, and optional continuation.
        request: crate::PackageSourceMembershipPageRequestV1,
    },
    /// Select one exact retained compiler generation for product projection.
    SelectSemanticVersion {
        /// Exact product package reference.
        package: PackageReference,
        /// Exact compiler coordinate for the target language lane.
        coordinate: PackageCoordinate,
        /// Closed language and dialect profile.
        profile: SemanticLanguageProfile,
        /// Immutable binding identity to select.
        generation: SemanticGenerationId,
    },
    /// Read the newest local package plus version count.
    PackageProfile {
        /// Package locator.
        package: PackageReference,
    },
    /// Follow one package for releases.
    Subscribe {
        /// Package locator.
        package: PackageReference,
        /// Optional project folder.
        project: Option<ProjectSelector>,
    },
    /// Stop following one package.
    Unsubscribe {
        /// Package locator.
        package: PackageReference,
    },
    /// List subscriptions.
    Subscriptions,
    /// Read followed-package releases.
    Releases {
        /// Mark returned newest releases as seen.
        mark_seen: bool,
    },
    /// List projects.
    Projects,
    /// Create a project.
    ProjectCreate {
        /// Unique name.
        name: ProjectName,
        /// Optional absolute supported lockfile path.
        lockfile: Option<ProductText>,
    },
    /// Delete a project.
    ProjectDelete {
        /// Project selector.
        project: ProjectSelector,
    },
    /// Add one project member.
    ProjectAdd {
        /// Project selector.
        project: ProjectSelector,
        /// Pinned package.
        package: PackageReference,
    },
    /// Remove one project member.
    ProjectRemove {
        /// Project selector.
        project: ProjectSelector,
        /// Pinned package.
        package: PackageReference,
    },
    /// Reconcile from the bound lockfile.
    ProjectSync {
        /// Project selector.
        project: ProjectSelector,
    },
    /// Read the shared session tree.
    Tree,
    /// Open or focus a tree subject.
    TreeOpen {
        /// Subject to open.
        subject: TreeSubject,
        /// Optional parent.
        parent: Option<TreeNodeId>,
        /// Optional title.
        title: Option<ProductText>,
        /// Calling surface.
        opener: TreeOpener,
    },
    /// Close a node or branch.
    TreeClose {
        /// Node to close.
        node: TreeNodeId,
        /// Include descendants.
        branch: bool,
    },
    /// Read one project's dependency tree.
    ProjectTree {
        /// Absolute project directory (any directory inside the workspace).
        root: ProductText,
    },
    /// Read one bounded UTF-8 file under a currently revalidated Cargo
    /// package source root, bound to its exact ProjectTree request.
    CargoPackageSourceFile {
        /// Exact source-qualified package and ProjectTree binding.
        request: crate::CargoPackageSourceRequestV1,
        /// Canonical package-relative path.
        path: crate::CargoPackageSourcePathV1,
    },
    /// List bounded source and documentation file addresses under an exact
    /// currently observed Cargo source receipt. Every listed path needs its
    /// own `CargoPackageSourceFile` read before displaying bytes.
    CargoPackageSourceInventory {
        /// Exact source-qualified package and ProjectTree binding.
        request: crate::CargoPackageSourceRequestV1,
    },
    /// Read the bounded README selected by the exact Cargo package manifest
    /// and source authority. The request cannot supply a path or project root.
    CargoPackageReadme {
        /// Exact package and requested-project binding from a current tree.
        request: crate::CargoPackageReadmeRequestV1,
    },
    /// Follow one relative Markdown link from an exact owner-returned README
    /// origin. The caller supplies no root path.
    CargoPackageReadmeLink {
        /// Owner-issued README origin and bounded relative href.
        request: crate::CargoPackageReadmeLinkRequestV1,
    },
    /// Start indexing one local project without waiting for compilation.
    IndexStart {
        /// Local project directory or pinned package coordinate.
        package: PackageReference,
        /// Requested compiler execution class.
        execution_intent: crate::CompileExecutionIntent,
    },
    /// Start or replay one operation under a caller-persisted durable key.
    IndexOperationStart {
        /// Durable identity created and persisted by the caller before dispatch.
        operation_key: IndexOperationKey,
        /// Local project directory or pinned package coordinate.
        package: PackageReference,
        /// Requested compiler execution class.
        execution_intent: crate::CompileExecutionIntent,
    },
    /// Read durable status for one exact caller-owned operation key.
    IndexOperationStatus {
        /// Durable identity originally supplied to [`Self::IndexOperationStart`].
        operation_key: IndexOperationKey,
    },
    /// Wait for the terminal receipt of one exact index job.
    IndexAwait {
        /// Owner-issued job and package identity.
        ticket: IndexJobTicket,
    },
    /// Read a bounded page from one exact index job's retained progress stream.
    IndexProgress {
        /// Owner-issued job and package identity.
        ticket: IndexJobTicket,
        /// Return only events with a sequence greater than this cursor.
        after_sequence: u64,
    },
    /// Request cancellation of one exact active index job.
    IndexCancel {
        /// Owner-issued job and package identity.
        ticket: IndexJobTicket,
    },
    /// Refresh every configured advisory source.
    AdvisoryRefresh,
}

impl SurfaceCommand {
    /// Returns this command's exact registry identity.
    #[must_use]
    pub const fn id(&self) -> CommandId {
        match self {
            Self::Advisory { .. } => CommandId::Advisory,
            Self::Read { .. } => CommandId::Read,
            Self::References { .. } => CommandId::References,
            Self::Diff { .. } => CommandId::Diff,
            Self::Explore { .. } => CommandId::Explore,
            Self::Package { .. } => CommandId::Package,
            Self::ForgeAdd { .. } => CommandId::ForgeAdd,
            Self::ForgeReference { .. } => CommandId::ForgeReference,
            Self::Dependents { .. } => CommandId::Dependents,
            Self::Dependencies { .. } => CommandId::Dependencies,
            Self::PackageGraphPage { .. } => CommandId::PackageGraphPage,
            Self::Owner { .. } => CommandId::Owner,
            Self::IndexSearch { .. } => CommandId::IndexSearch,
            Self::PackageVersions { .. } => CommandId::PackageVersions,
            Self::SemanticVersions { .. } => CommandId::SemanticVersions,
            Self::SemanticShapes { .. } => CommandId::SemanticShapes,
            Self::PackageSourceMembership { .. } => CommandId::PackageSourceMembership,
            Self::SelectSemanticVersion { .. } => CommandId::SelectSemanticVersion,
            Self::PackageProfile { .. } => CommandId::PackageProfile,
            Self::Subscribe { .. } => CommandId::Subscribe,
            Self::Unsubscribe { .. } => CommandId::Unsubscribe,
            Self::Subscriptions => CommandId::Subscriptions,
            Self::Releases { .. } => CommandId::Releases,
            Self::Projects => CommandId::Projects,
            Self::ProjectCreate { .. } => CommandId::ProjectCreate,
            Self::ProjectDelete { .. } => CommandId::ProjectDelete,
            Self::ProjectAdd { .. } => CommandId::ProjectAdd,
            Self::ProjectRemove { .. } => CommandId::ProjectRemove,
            Self::ProjectSync { .. } => CommandId::ProjectSync,
            Self::Tree => CommandId::Tree,
            Self::TreeOpen { .. } => CommandId::TreeOpen,
            Self::TreeClose { .. } => CommandId::TreeClose,
            Self::ProjectTree { .. } => CommandId::ProjectTree,
            Self::CargoPackageSourceFile { .. } => CommandId::CargoPackageSourceFile,
            Self::CargoPackageSourceInventory { .. } => CommandId::CargoPackageSourceInventory,
            Self::CargoPackageReadme { .. } => CommandId::CargoPackageReadme,
            Self::CargoPackageReadmeLink { .. } => CommandId::CargoPackageReadmeLink,
            Self::IndexStart { .. } => CommandId::IndexStart,
            Self::IndexOperationStart { .. } => CommandId::IndexStart,
            Self::IndexOperationStatus { .. } => CommandId::IndexProgress,
            Self::IndexAwait { .. } => CommandId::IndexAwait,
            Self::IndexProgress { .. } => CommandId::IndexProgress,
            Self::IndexCancel { .. } => CommandId::IndexCancel,
            Self::AdvisoryRefresh => CommandId::AdvisoryRefresh,
        }
    }
    /// Verifies collection and page bounds after syntactic decoding.
    ///
    /// # Errors
    ///
    /// Returns [`ProductAdmissionError`] when a request collection or page size violates the
    /// fixed product row bound.
    pub fn admit(&self) -> Result<(), ProductAdmissionError> {
        match self {
            Self::Read { locators } if locators.len() > MAX_PRODUCT_ROWS => {
                Err(ProductAdmissionError::RowBound)
            }
            Self::Explore { limit, .. } | Self::IndexSearch { limit, .. }
                if *limit == 0 || usize::from(*limit) > MAX_PRODUCT_ROWS =>
            {
                Err(ProductAdmissionError::RowBound)
            }
            Self::SelectSemanticVersion {
                coordinate,
                profile,
                ..
            } if coordinate.package_type().language() != profile.profile()?.language() => {
                Err(ProductAdmissionError::SemanticVersionShape)
            }
            Self::PackageGraphPage { request } => request
                .admit()
                .map_err(|_| ProductAdmissionError::PackageGraphPage),
            Self::CargoPackageSourceFile { request, path }
                if !request.has_admissible_shape() || !path.has_admissible_shape() =>
            {
                Err(ProductAdmissionError::CargoSourceShape)
            }
            Self::CargoPackageSourceInventory { request } if !request.has_admissible_shape() => {
                Err(ProductAdmissionError::CargoSourceShape)
            }
            Self::PackageSourceMembership { request } if !request.has_admissible_shape() => {
                Err(ProductAdmissionError::PackageSourceMembershipShape)
            }
            Self::CargoPackageReadme { request } if !request.has_admissible_shape() => {
                Err(ProductAdmissionError::CargoSourceShape)
            }
            Self::CargoPackageReadmeLink { request } if !request.has_admissible_shape() => {
                Err(ProductAdmissionError::CargoSourceShape)
            }
            _ => Ok(()),
        }
    }
}

/// Compact declaration result used by batched reads.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeclarationRecord {
    /// Requested label.
    pub label: ProductText,
    /// Stable row digest bytes when found.
    pub stable_id: Option<[u8; 32]>,
    /// Canonical signature when captured.
    pub signature: Option<ProductText>,
}

/// How a declaration changed between package versions.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DeclarationChange {
    /// Present only in the newer package.
    Added,
    /// Present only in the older package.
    Removed,
    /// Present in both with different signatures.
    Changed,
    /// The family remains present, but overload identity churn prevents a
    /// sound one-to-one pairing.
    Indeterminate,
}

/// Exact stable identity of one semantic declaration instance.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticDeclarationIdentity {
    /// Stable declaration family.
    pub family: [u8; 16],
    /// Structural fingerprint of this family member.
    pub variant: [u8; 16],
}

/// Closed semantic relation vocabulary emitted by compiler authorities.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SemanticLinkKind {
    /// Direct callable invocation.
    Calls,
    /// Dynamically or statically dispatched method invocation.
    MethodCall,
    /// Use of a declaration as a type.
    TypeReference,
    /// Value read.
    Reads,
    /// Value write.
    Writes,
    /// Module or package import.
    Imports,
    /// Interface or trait implementation.
    Implements,
    /// Member override.
    Overrides,
    /// Public re-export.
    Reexports,
    /// Class or type inheritance.
    Inherits,
    /// Documentation reference.
    Documents,
}

/// Strength of the authority evidence for one semantic relation.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SemanticConfidence {
    /// Parsed syntax proves only the written relationship.
    Syntactic,
    /// A language adapter inferred the relationship without full authority.
    Heuristic,
    /// A language index resolved the relationship.
    Indexed,
    /// An imported semantic artifact resolved the relationship.
    Imported,
    /// The native compiler or oracle resolved the relationship.
    Compiler,
}

/// Exact endpoint of a semantic graph relation.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(tag = "scope", rename_all = "kebab-case", deny_unknown_fields)]
pub enum SemanticLinkTarget {
    /// Declaration in the same package image.
    Local {
        /// Exact local declaration identity.
        declaration: SemanticDeclarationIdentity,
    },
    /// Declaration in another immutable semantic fragment.
    Stable {
        /// Immutable target fragment identity.
        fragment: [u8; 32],
        /// Exact target declaration identity.
        declaration: SemanticDeclarationIdentity,
    },
    /// Unresolved declaration retained under its foreign authority key.
    Foreign {
        /// Stable foreign declaration family.
        declaration: [u8; 16],
        /// Structural variant when the authority could recover it.
        variant: Option<[u8; 16]>,
    },
    /// Exact entity ordinal in another immutable fragment.
    FragmentEntity {
        /// Immutable target fragment identity.
        fragment: [u8; 32],
        /// Target entity ordinal within that fragment.
        ordinal: u32,
    },
}

/// Captured source site attached to semantic relation evidence.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticSourceSpan {
    /// Canonical package-relative source path.
    pub file: ProductText,
    /// Inclusive UTF-8 byte start.
    pub start: u32,
    /// Exclusive UTF-8 byte end.
    pub end: u32,
}

/// Authority evidence retained for one canonical semantic relation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticLinkEvidence {
    /// Strongest authority classification observed for this relation.
    pub confidence: SemanticConfidence,
    /// Representative source site, when the authority captured one.
    pub source: Option<SemanticSourceSpan>,
}

/// One exact graph relation delta attached to its source declaration.
///
/// Evidence presence is encoded by the variant, so an added link cannot carry
/// older evidence and an evidence change cannot omit either observation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "change", rename_all = "kebab-case", deny_unknown_fields)]
pub enum SemanticLinkDelta {
    /// Relation present only in the newer generation.
    Added {
        /// Exact source declaration.
        from: SemanticDeclarationIdentity,
        /// Exact local or external endpoint.
        target: SemanticLinkTarget,
        /// Compiler-defined relation kind.
        relation: SemanticLinkKind,
        /// Newer authority evidence.
        evidence: SemanticLinkEvidence,
    },
    /// Relation present only in the older generation.
    Removed {
        /// Exact source declaration.
        from: SemanticDeclarationIdentity,
        /// Exact local or external endpoint.
        target: SemanticLinkTarget,
        /// Compiler-defined relation kind.
        relation: SemanticLinkKind,
        /// Older authority evidence.
        evidence: SemanticLinkEvidence,
    },
    /// Stable relation whose representative authority evidence changed.
    EvidenceChanged {
        /// Exact source declaration.
        from: SemanticDeclarationIdentity,
        /// Exact local or external endpoint.
        target: SemanticLinkTarget,
        /// Compiler-defined relation kind.
        relation: SemanticLinkKind,
        /// Older authority evidence.
        before: SemanticLinkEvidence,
        /// Newer authority evidence.
        after: SemanticLinkEvidence,
    },
}

impl SemanticLinkDelta {
    /// Returns the exact declaration that owns this outgoing relation.
    #[must_use]
    pub const fn source_declaration(&self) -> SemanticDeclarationIdentity {
        match self {
            Self::Added { from, .. }
            | Self::Removed { from, .. }
            | Self::EvidenceChanged { from, .. } => *from,
        }
    }

    fn matches_declaration_sides(
        &self,
        before: Option<SemanticDeclarationIdentity>,
        after: Option<SemanticDeclarationIdentity>,
    ) -> bool {
        match self {
            Self::Added { from, .. } => Some(*from) == after,
            Self::Removed { from, .. } => Some(*from) == before,
            Self::EvidenceChanged {
                from,
                before: older,
                after: newer,
                ..
            } => Some(*from) == before && Some(*from) == after && older != newer,
        }
    }
}

/// One source-verified use of a declaration, as answered by a references
/// query.
///
/// Every record is provenance-bearing by construction: the authority
/// classification and the captured source site ride in `evidence`, the exact
/// endpoint resolution rides in `target`, and `site` names the declaration
/// whose source contains the use.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReferenceRecord {
    /// Canonical coordinate of the declaration that uses the target.
    pub site: ProductText,
    /// Exact local or foreign endpoint this occurrence resolves to.
    pub target: SemanticLinkTarget,
    /// Compiler-defined relation that makes this site a use.
    pub relation: SemanticLinkKind,
    /// Authority classification and captured source span of the use.
    pub evidence: SemanticLinkEvidence,
}

/// One declaration and graph difference between package generations.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiffRecord {
    /// Declaration label.
    pub label: ProductText,
    /// Exact change.
    pub change: DeclarationChange,
    /// Exact older declaration identity, when one existed.
    pub before: Option<SemanticDeclarationIdentity>,
    /// Exact newer declaration identity, when one exists.
    pub after: Option<SemanticDeclarationIdentity>,
    /// Bounded graph relation changes sourced by this declaration.
    pub links: Box<[SemanticLinkDelta]>,
}

impl DiffRecord {
    const fn has_valid_identity_shape(&self) -> bool {
        match self.change {
            DeclarationChange::Added => self.before.is_none() && self.after.is_some(),
            DeclarationChange::Removed => self.before.is_some() && self.after.is_none(),
            DeclarationChange::Changed => self.before.is_some() && self.after.is_some(),
            DeclarationChange::Indeterminate => self.before.is_some() ^ self.after.is_some(),
        }
    }
}

/// Completeness of one registry fact group on a selected package release.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RegistryPackageFactCompleteness {
    /// The source supplied the complete fact group.
    Complete,
    /// The source supplied only part of the fact group.
    Partial,
    /// The source does not record this fact group.
    NotRecorded,
    /// The source does not support this fact group.
    Unsupported,
    /// The source could not provide this fact group.
    Unavailable,
    /// Completeness is not known.
    Unknown,
}

/// Kind of source proof that observed selected registry release facts.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum RegistryPackageFactProof {
    /// A durable acquisition receipt selected a verified source snapshot.
    AcquisitionReceipt {
        /// Identity of the accepted acquisition receipt.
        receipt: [u8; 32],
        /// Identity of the selected source snapshot.
        snapshot: [u8; 32],
    },
    /// A source supplied an authenticated negative fact with a bounded lease.
    SourceNegativeFact {
        /// Registry source authority identity.
        authority: [u8; 32],
        /// Source proof identity.
        source_proof: [u8; 32],
        /// Source cursor at which the fact was observed.
        cursor: [u8; 32],
        /// Source-reported observation time in Unix milliseconds.
        observed_at_millis: u64,
        /// Source-reported expiration time in Unix milliseconds.
        expires_at_millis: u64,
        /// Registry policy epoch used to admit the proof.
        policy_epoch: u64,
        /// Typed meaning of the negative fact.
        fact: RegistryNegativeFactKind,
    },
}

/// Typed category of a source-reported negative package fact.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RegistryNegativeFactKind {
    /// The exact release does not exist at this source.
    NotFound,
    /// The exact release has been yanked.
    Yanked,
    /// Advisory policy blocks this release.
    AdvisoryBlocked,
    /// The source does not support this package coordinate.
    Unsupported,
}

/// Freshness and provenance of the selected release-fact observation.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case")]
pub enum RegistryPackageFactFreshness {
    /// A source proof observed these facts during its validity window.
    Current {
        /// Time of the accepted observation in Unix milliseconds.
        observed_at_millis: u64,
        /// End of the observation's validity window in Unix milliseconds.
        valid_until_millis: u64,
        /// Proof that selected the observed facts.
        proof: RegistryPackageFactProof,
    },
    /// Durable source facts remain selected, but this process cannot attest a
    /// current observation after reopening or expiry.
    Historical,
}

/// Versioned source authority paired with a registry package reply.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistryPackageFactAuthority {
    /// Registry source identity.
    pub source: [u8; 32],
    /// Versioned root of the source facts selected for this row.
    pub source_facts_root: [u8; 32],
    /// Provenance identity of the selected package publication.
    pub source_provenance: [u8; 32],
    /// Content identity of the selected release facts.
    pub facts_version: [u8; 32],
    /// Content identity of the selected advisory facts.
    pub advisory_facts_version: [u8; 32],
    /// Identity of the complete selected-facts decision.
    pub selection_version: [u8; 32],
    /// Completeness of release-standing facts.
    pub standing: RegistryPackageFactCompleteness,
    /// Completeness of release download-count facts.
    pub downloads: RegistryPackageFactCompleteness,
    /// Completeness of advisory facts.
    pub advisories: RegistryPackageFactCompleteness,
    /// Freshness of package standing and download facts.
    pub release_facts_freshness: RegistryPackageFactFreshness,
    /// Freshness of advisory-feed facts, tracked independently.
    pub advisory_freshness: backend_advisory::FreshnessState,
}

/// One locally committed registry publication.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistryPackageRecord {
    /// Canonical version-pinned purl.
    pub coordinate: PackageReference,
    /// Closed ecosystem spelling.
    pub ecosystem: RegistryEcosystem,
    /// Registry-native package name.
    pub name: ProductText,
    /// Immutable version.
    pub version: ProductText,
    /// Verified archive byte count.
    pub bytes: u64,
    /// Current registry selection policy for this immutable release.
    pub standing: RegistryReleaseStanding,
    /// Latest download observation, with missing data kept distinct from zero.
    pub downloads: RegistryDownloadCount,
    /// Content identity of the registry fact groups above.
    pub facts_version: [u8; 32],
    /// Authority, completeness, and freshness for a registry-selected row.
    /// Local manifest and synthetic records leave this unset.
    #[serde(default)]
    pub authority: Option<RegistryPackageFactAuthority>,
    /// Identity of the versioned native metadata DTO.
    pub native_metadata_version: [u8; 32],
    /// Complete bounded native registry metadata for this release.
    pub native_metadata: RegistryNativeMetadata,
    /// Versioned, typed forge lineage facts for this release.
    #[serde(default)]
    pub forge_sources: Box<[RegistryForgeAssociation]>,
    /// Complete typed advisory evidence and acquisition decision for this version.
    pub advisory: AdvisoryPackageDto,
}

/// Freshness of an unacquired package claim returned by catalog discovery.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case")]
pub enum RegistryDiscoveryFreshness {
    /// The source claim was observed in this process and remains inside its
    /// configured freshness horizon.
    Current {
        /// Unix-millisecond time when the source claim was observed.
        observed_at_millis: u64,
        /// Unix-millisecond end of the claim's configured freshness horizon.
        valid_until_millis: u64,
    },
    /// The claim comes from historical source state or an incomplete backfill.
    Historical {
        /// Unix-millisecond time attached to the source observation.
        observed_at_millis: u64,
    },
    /// The source has not refreshed the claim within its freshness horizon.
    Expired {
        /// Unix-millisecond time when the source claim was observed.
        observed_at_millis: u64,
        /// Unix-millisecond end of the claim's configured freshness horizon.
        valid_until_millis: u64,
    },
    /// The last source refresh failed, so this is the last durable claim.
    Unavailable {
        /// Unix-millisecond time attached to the last available source claim.
        observed_at_millis: u64,
        /// Whether the last available claim is historical or from an incomplete backfill.
        historical: bool,
    },
}

/// Completeness of the source view that produced an unacquired candidate.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RegistryDiscoveryCompleteness {
    /// Every catalog event through the reported source cursor was processed.
    CompleteThroughCursor,
    /// A mutable or page-bounded source view can omit candidates.
    Windowed,
    /// The source does not provide this discovery feed.
    Unsupported,
    /// A valid response could not be admitted in full.
    Incomplete,
}

/// Source-reported release standing for an unacquired candidate.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RegistryDiscoveryStanding {
    /// The source reports this release as published or listed.
    Published,
    /// The source reports this release as yanked or unlisted.
    Yanked,
    /// The source reports this release was deleted or withdrawn.
    Withdrawn,
    /// A source package recipe exists, but no binary release is asserted.
    RecipeAvailable,
}

/// A source-attributed optional metadata claim. `Absent` means the registry
/// schema does not publish the facet; `Unknown` means this observation did not
/// establish it. Neither state is equivalent to a known empty value.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", content = "value", rename_all = "kebab-case")]
pub enum RegistryEvidenceFacet<T> {
    /// The source reported this value, including an empty collection or zero.
    Known(T),
    /// The source schema does not expose this facet.
    Absent,
    /// This observation did not establish whether a value exists.
    Unknown,
}

impl<T> Default for RegistryEvidenceFacet<T> {
    fn default() -> Self {
        Self::Unknown
    }
}

/// Advisory evidence attached to the enclosing registry source claim.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistryDiscoveryAdvisory {
    /// Source-native advisory identifier.
    pub id: ProductText,
    /// Alternate source-provided advisory identifiers, with source coverage.
    pub aliases: RegistryEvidenceFacet<Box<[ProductText]>>,
    /// Source-provided short summary.
    pub summary: RegistryEvidenceFacet<ProductText>,
    /// Source-provided severity label, without inferred scoring.
    pub severity: RegistryEvidenceFacet<ProductText>,
    /// Source-provided fixed versions, without inferred version ordering.
    pub fixed_in: RegistryEvidenceFacet<Box<[ProductText]>>,
}

/// Advisory and download observations for one exact registry release.
/// Every known facet is attributed to the enclosing candidate's `source`.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistryDiscoveryMetadata {
    /// Exact per-release download count. Zero remains distinct from absence.
    pub downloads: RegistryEvidenceFacet<u64>,
    /// Advisory source facts for this release.
    pub advisories: RegistryEvidenceFacet<Box<[RegistryDiscoveryAdvisory]>>,
    /// Explicit source-reported yank state, separate from package standing.
    pub yanked: RegistryEvidenceFacet<bool>,
}

/// One discovered release claim, kept distinct by source identity.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistryDiscoveryCandidate {
    /// Stable source identity digest. Competing sources remain separate rows.
    pub source: [u8; 32],
    /// Exact version-pinned coordinate from the source.
    pub coordinate: PackageCoordinate,
    /// Source-reported standing; this does not imply local acquisition.
    pub standing: RegistryDiscoveryStanding,
    /// Source completeness attached to this claim.
    pub completeness: RegistryDiscoveryCompleteness,
    /// Whether the source's last observation reached its captured high watermark.
    pub caught_up: bool,
    /// Historical/current observation evidence.
    pub freshness: RegistryDiscoveryFreshness,
    /// Digest of the exact source record that produced this claim.
    pub proof: [u8; 32],
    /// Optional source-attributed advisory, download, and yank facts.
    #[serde(default)]
    pub metadata: RegistryDiscoveryMetadata,
}

/// Exact registry metadata lookup, without acquisition or compiler authority.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case", deny_unknown_fields)]
pub enum RegistryPackageDiscoveryObservation {
    /// Source-attributed claims for the requested immutable release.
    Observed {
        /// Separate claims from each source; these are never acquired records.
        candidates: Box<[RegistryDiscoveryCandidate]>,
    },
    /// A validated complete package document did not contain this release.
    Missing {
        /// Exact metadata endpoint identity.
        source: [u8; 32],
        /// Digest of the response establishing absence.
        proof: [u8; 32],
        /// Time of this negative observation.
        observed_at_millis: u64,
    },
    /// The configured metadata authority could not establish an answer.
    Unavailable {
        /// Endpoint identity, when a compatible authority was configured.
        source: Option<[u8; 32]>,
        /// Bounded explanation; absence must never be inferred from this state.
        reason: ProductText,
    },
}

impl RegistryDiscoveryCandidate {
    /// Checks the user-facing candidate boundary after decoding.
    pub fn admit(&self) -> Result<(), ProductAdmissionError> {
        match self.freshness {
            RegistryDiscoveryFreshness::Current {
                observed_at_millis,
                valid_until_millis,
            }
            | RegistryDiscoveryFreshness::Expired {
                observed_at_millis,
                valid_until_millis,
            } if valid_until_millis < observed_at_millis => {
                Err(ProductAdmissionError::RegistryDiscoveryFreshness)
            }
            RegistryDiscoveryFreshness::Current { .. }
            | RegistryDiscoveryFreshness::Historical { .. }
            | RegistryDiscoveryFreshness::Expired { .. }
            | RegistryDiscoveryFreshness::Unavailable { .. } => {
                admit_registry_discovery_metadata(&self.metadata)
            }
        }
    }
}

fn admit_registry_discovery_metadata(
    metadata: &RegistryDiscoveryMetadata,
) -> Result<(), ProductAdmissionError> {
    let RegistryEvidenceFacet::Known(advisories) = &metadata.advisories else {
        return Ok(());
    };
    if advisories.len() > 128 {
        return Err(ProductAdmissionError::RegistryDiscoveryMetadata);
    }
    for advisory in advisories.iter() {
        if matches!(&advisory.aliases, RegistryEvidenceFacet::Known(values)
            if values.len() > 32 || values.iter().any(|value| value.as_str().len() > MAX_PRODUCT_TEXT_BYTES))
        {
            return Err(ProductAdmissionError::RegistryDiscoveryMetadata);
        }
        for facet in [&advisory.summary, &advisory.severity] {
            if matches!(facet, RegistryEvidenceFacet::Known(value) if value.as_str().len() > 4096) {
                return Err(ProductAdmissionError::RegistryDiscoveryMetadata);
            }
        }
        if matches!(&advisory.fixed_in, RegistryEvidenceFacet::Known(values) if values.len() > 256 || values.iter().any(|value| value.as_str().len() > 4096))
        {
            return Err(ProductAdmissionError::RegistryDiscoveryMetadata);
        }
    }
    Ok(())
}

/// Opaque continuation for a bounded index-search page.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct IndexSearchCursor(String);

impl IndexSearchCursor {
    /// Admits a bounded URL-safe opaque token.
    pub fn new(value: impl Into<String>) -> Result<Self, ProductAdmissionError> {
        let value = value.into();
        if value.is_empty()
            || value.len() > MAX_INDEX_SEARCH_CURSOR_BYTES
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        {
            return Err(ProductAdmissionError::IndexSearchCursor);
        }
        Ok(Self(value))
    }

    /// Returns the opaque token unchanged for a subsequent request.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for IndexSearchCursor {
    type Error = ProductAdmissionError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<IndexSearchCursor> for String {
    fn from(value: IndexSearchCursor) -> Self {
        value.0
    }
}

/// Whether the search plane can prove an exact total or only a lower bound.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", content = "value", rename_all = "kebab-case")]
pub enum IndexSearchResultCount {
    /// Exact number of distinct results matching the request.
    Exact(u32),
    /// At least this many results matched; more remain beyond the bounded probe.
    AtLeast(u32),
    /// The search projection could not establish a count.
    Unknown,
}

/// One page from the stable selected index snapshot.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IndexSearchPage {
    /// Structural index snapshot shared by every page in a cursor chain.
    pub snapshot: [u8; 32],
    /// Wall-clock time at which mutable freshness/evidence overlays were read.
    pub evaluated_at_millis: u64,
    /// Distinct canonical package lineages and local declaration matches.
    pub hits: Box<[RegistrySearchHit]>,
    /// Opaque continuation for the next bounded page.
    pub next_cursor: Option<IndexSearchCursor>,
    /// Exact count or lower bound, when the projections can establish one.
    pub result_count: IndexSearchResultCount,
}

/// A package/version found in an acquired forge tree. The PURL identifies the
/// package version; typed repository metadata keeps an identical registry
/// coordinate visibly attributable to its forge source.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ForgeDiscoveryCandidate {
    /// Stable identity of the exact forge repository/ref/subdirectory claim.
    pub source: [u8; 32],
    /// Canonical package/version identity derived from the source manifest.
    pub coordinate: PackageCoordinate,
    /// Exact canonical repository coordinate that was acquired.
    pub forge_coordinate: ProductText,
    /// Commit resolved by the forge authority.
    pub commit: ForgeFact<ProductText>,
    /// Manifest corresponding to this package/version hit.
    pub manifest: ForgeManifestRecord,
    /// Repository metadata, including typed unknown README and license facts.
    pub metadata: ForgeRepositoryMetadataRecord,
}

impl ForgeDiscoveryCandidate {
    /// Checks that the source coordinate and package identity remain bound.
    pub fn admit(&self) -> Result<(), ProductAdmissionError> {
        if let Some(metadata) = &self.manifest.python_metadata {
            metadata.admit()?;
        }
        let source = ForgeCoordinate::parse(self.forge_coordinate.as_str().to_owned())
            .map_err(|_| ProductAdmissionError::ForgeSearchShape)?;
        let parsed = PackageCoordinate::parse(self.coordinate.as_str().to_owned())
            .map_err(|_| ProductAdmissionError::ForgeSearchShape)?;
        if !source.identity_is_valid()
            || source.identity() != self.source
            || parsed != self.coordinate
            || self.manifest.path.as_str().is_empty()
            || self.manifest.ecosystem.as_str() != self.coordinate.package_type().as_str()
        {
            return Err(ProductAdmissionError::ForgeSearchShape);
        }
        Ok(())
    }
}

/// A forge manifest row that preserves source revision pins separately from
/// actual package versions and leaves registry-only facts unknown.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ForgePackageDetailRecord {
    /// Exact canonical repository/ref/subdirectory claim.
    pub source: ForgeCoordinate,
    /// Stable identity of the source authority.
    pub source_id: [u8; 32],
    /// Exact commit selected for this source claim.
    pub resolved_commit: ForgeObjectId,
    /// Exact tree selected by the authority, when reported.
    pub resolved_tree: Option<ForgeObjectId>,
    /// Package PURL only when the manifest recorded a valid name and version.
    pub package_coordinate: Option<PackageCoordinate>,
    /// Whether this row represents a package release or only a source pin.
    pub pin: ForgePackagePin,
    /// Source manifest facts with unavailable values preserved.
    pub manifest: ForgePackageManifestDetail,
    /// Metadata returned by the forge authority.
    pub metadata: ForgeRepositoryMetadataRecord,
    /// Registry-only evidence stays distinct from forge source facts.
    pub registry: ForgePackageRegistryEvidence,
}

impl ForgePackageDetailRecord {
    /// Validates source, manifest, pin, and optional release-coordinate agreement.
    pub fn admit(&self) -> Result<(), ProductAdmissionError> {
        if !self.source.identity_is_valid()
            || self.source_id != self.source.identity()
            || self.manifest.path.as_str().is_empty()
        {
            return Err(ProductAdmissionError::ForgePackageDetailShape);
        }
        if let Some(metadata) = &self.manifest.python_metadata {
            metadata.admit()?;
        }
        match (&self.pin, &self.package_coordinate) {
            (ForgePackagePin::PackageVersion { coordinate }, Some(package_coordinate))
                if coordinate == package_coordinate
                    && forge_manifest_coordinate(&self.manifest).as_ref() == Some(coordinate) => {}
            (
                ForgePackagePin::PinnedRevision {
                    requested_revision,
                    resolved_commit,
                },
                None,
            ) if requested_revision == self.source.revision()
                && resolved_commit == &self.resolved_commit => {}
            _ => return Err(ProductAdmissionError::ForgePackageDetailShape),
        }
        Ok(())
    }
}

/// Whether a forge manifest establishes a package version or only a source pin.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", content = "value", rename_all = "kebab-case")]
pub enum ForgePackagePin {
    /// The manifest itself recorded this exact package version.
    PackageVersion {
        /// Version-pinned package coordinate parsed from the manifest.
        coordinate: PackageCoordinate,
    },
    /// The source resolved to a commit, but the manifest supplied no valid release PURL.
    PinnedRevision {
        /// Revision requested by the source coordinate.
        requested_revision: ForgeRevision,
        /// Commit to which the source revision resolved.
        resolved_commit: ForgeObjectId,
    },
}

/// Manifest details whose name and version availability is source-attributed.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ForgePackageManifestDetail {
    /// Path relative to the selected repository subdirectory.
    pub path: ProductText,
    /// Ecosystem parser that admitted the manifest.
    pub ecosystem: RegistryEcosystem,
    /// Registry-native package name, when the manifest reported one.
    pub name: ForgeFact<ProductText>,
    /// Registry-native package version, when the manifest reported one.
    pub version: ForgeFact<ProductText>,
    /// Dependency facts admitted from the manifest.
    pub dependencies: DependencyFacts<Box<[PackageDependencyRecord]>>,
    /// Static Python declarations with source evidence, independent of registry authority.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub python_metadata: Option<crate::PythonProjectMetadata>,
}

/// Registry-only evidence attached to a forge manifest.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ForgePackageRegistryEvidence {
    /// Registry yank/unlist state; forge acquisition cannot establish it.
    pub yanked: RegistryEvidenceFacet<bool>,
    /// Download telemetry; forge acquisition cannot establish it.
    pub downloads: RegistryDownloadCount,
    /// Advisory coverage, including an explicit unknown state.
    pub advisory: AdvisoryPackageDto,
}

fn forge_manifest_coordinate(manifest: &ForgePackageManifestDetail) -> Option<PackageCoordinate> {
    let (ForgeFact::Recorded(name), ForgeFact::Recorded(version)) =
        (&manifest.name, &manifest.version)
    else {
        return None;
    };
    let package_name = if manifest.ecosystem == RegistryEcosystem::Maven {
        name.as_str().replace(':', "/")
    } else {
        name.as_str().to_owned()
    };
    PackageCoordinate::parse(format!(
        "pkg:{}/{}@{}",
        manifest.ecosystem.package_type().as_str(),
        package_name,
        version.as_str()
    ))
    .ok()
}

/// One local declaration in combined search, backed by the selected source view.
/// A declaration has a source location, not a registry ecosystem or release.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalDeclarationSearchRecord {
    /// Exact declaration coordinate accepted by document and source reads.
    pub coordinate: ProductText,
    /// Name supplied by the selected declaration row.
    pub name: ProductText,
    /// Source path, never a package release version.
    pub path: ProductText,
    /// Captured one-based start line; absent when only the label supplied a path.
    pub line: Option<NonZeroU32>,
}

impl LocalDeclarationSearchRecord {
    fn admit(&self) -> Result<(), ProductAdmissionError> {
        if self
            .coordinate
            .as_str()
            .rsplit_once("::")
            .is_some_and(|(scope, name)| !scope.is_empty() && name == self.name.as_str())
        {
            Ok(())
        } else {
            Err(ProductAdmissionError::LocalDeclarationSearchShape)
        }
    }
}

/// Ranked catalog-search item, with acquisition and discovery kept as
/// different variants and source conflicts kept as separate candidates.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", content = "value", rename_all = "kebab-case")]
pub enum RegistrySearchHit {
    /// A package record backed by locally acquired registry artifacts.
    Acquired(RegistryPackageRecord),
    /// A source-only package claim. No archive or compiler artifact exists yet.
    Discovered(RegistryDiscoveryCandidate),
    /// A package/version discovered in a code-forge source tree.
    ForgeDiscovered(ForgeDiscoveryCandidate),
    /// A code-forge source pin whose manifest does not establish a package version.
    ForgeSourcePin(ForgePackageDetailRecord),
    /// A local indexed declaration returned by the combined search surface.
    LocalDeclaration(LocalDeclarationSearchRecord),
    /// One source-scoped canonical package lineage with bounded release facets.
    PackageGroup(RegistryPackageSearchGroup),
}

/// Which source plane supplies every release in a lineage group.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RegistrySearchGroupKind {
    /// Locally acquired releases from one recorded registry authority.
    Acquired,
    /// Source-only registry observations that remain unacquired.
    Discovered,
    /// Package manifests found in one acquired forge source.
    Forge,
}

/// Whether the bounded version facets themselves matched the query or are
/// source-backed representatives for a lineage-level metadata match.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RegistryReleaseMatchScope {
    /// Every listed release matched the query on its own indexed facts.
    ReleaseMatches,
    /// The lineage matched by combining indexed facts across releases; the
    /// listed releases are representatives and are not claimed to match.
    LineageMetadataOnly,
}

/// One bounded set of version-specific facets for a canonical package name.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistryPackageSearchGroup {
    /// Group's source-plane type.
    pub kind: RegistrySearchGroupKind,
    /// Stable identity of the source making this claim.
    pub source: [u8; 32],
    /// Canonical ecosystem for the lineage.
    pub ecosystem: RegistryEcosystem,
    /// Canonical package lineage without its version pin.
    pub lineage: ProductText,
    /// Release-specific evidence, ordered by the search projection. Consult
    /// `release_match_scope` before presenting these as query matches.
    pub releases: Box<[RegistrySearchRelease]>,
    /// Describes whether listed releases matched individually or represent a
    /// lineage-level metadata match.
    pub release_match_scope: RegistryReleaseMatchScope,
    /// True when more releases in `release_match_scope` were withheld by the
    /// per-group bound. For `LineageMetadataOnly`, this means more source
    /// releases exist in the lineage, not that they matched the query.
    pub more_releases: bool,
}

/// One exact, version-pinned member of a package-lineage result.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", content = "value", rename_all = "kebab-case")]
pub enum RegistrySearchRelease {
    /// Release acquired into the local registry catalog.
    Acquired(RegistryPackageRecord),
    /// Source-only registry release claim.
    Discovered(RegistryDiscoveryCandidate),
    /// Manifest-backed package found in a forge source tree.
    ForgeDiscovered(ForgeDiscoveryCandidate),
}

impl RegistryPackageSearchGroup {
    /// Validates release grouping, provenance, and nested collection bounds.
    pub fn admit(&self) -> Result<(), ProductAdmissionError> {
        if self.releases.is_empty() || self.releases.len() > 16 {
            return Err(ProductAdmissionError::RegistrySearchGroup);
        }
        let mut coordinates = std::collections::BTreeSet::new();
        for release in self.releases.iter() {
            let (kind, source, ecosystem, coordinate) = match release {
                RegistrySearchRelease::Acquired(record) => {
                    admit_registry_record(record)?;
                    let source = record
                        .authority
                        .map_or([0; 32], |authority| authority.source);
                    let PackageReference::Purl(coordinate) = &record.coordinate else {
                        return Err(ProductAdmissionError::RegistrySearchGroup);
                    };
                    (
                        RegistrySearchGroupKind::Acquired,
                        source,
                        record.ecosystem,
                        coordinate.clone(),
                    )
                }
                RegistrySearchRelease::Discovered(candidate) => {
                    candidate.admit()?;
                    let coordinate =
                        PackageCoordinate::parse(candidate.coordinate.as_str().to_owned())
                            .map_err(|_| ProductAdmissionError::RegistrySearchGroup)?;
                    (
                        RegistrySearchGroupKind::Discovered,
                        candidate.source,
                        coordinate
                            .package_type()
                            .registry()
                            .ok_or(ProductAdmissionError::RegistrySearchGroup)?,
                        coordinate,
                    )
                }
                RegistrySearchRelease::ForgeDiscovered(candidate) => {
                    candidate.admit()?;
                    let coordinate =
                        PackageCoordinate::parse(candidate.coordinate.as_str().to_owned())
                            .map_err(|_| ProductAdmissionError::RegistrySearchGroup)?;
                    let ecosystem = coordinate
                        .package_type()
                        .registry()
                        .unwrap_or(RegistryEcosystem::Cpp);
                    (
                        RegistrySearchGroupKind::Forge,
                        candidate.source,
                        ecosystem,
                        coordinate,
                    )
                }
            };
            let lineage = registry_package_lineage(&coordinate, ecosystem);
            if kind != self.kind
                || source != self.source
                || ecosystem != self.ecosystem
                || lineage != self.lineage.as_str()
                || !coordinates.insert(coordinate.as_str().to_owned())
            {
                return Err(ProductAdmissionError::RegistrySearchGroup);
            }
        }
        Ok(())
    }
}

fn registry_package_lineage(
    coordinate: &PackageCoordinate,
    ecosystem: RegistryEcosystem,
) -> String {
    let path = coordinate.lineage_name();
    if matches!(ecosystem, RegistryEcosystem::Maven | RegistryEcosystem::Cpp) {
        path.rsplit_once('/').map_or_else(
            || path.to_owned(),
            |(namespace, name)| format!("{namespace}:{name}"),
        )
    } else {
        path.to_owned()
    }
}

/// A bounded fact whose absence is preserved as a typed state.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", content = "value", rename_all = "kebab-case")]
pub enum ForgeFact<T> {
    /// The configured forge authority reported this value.
    Recorded(T),
    /// The authority did not report this value or it was unreachable.
    Unavailable(ProductText),
}

/// Typed repository metadata shown on a forge-backed package page.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ForgeRepositoryMetadataRecord {
    /// Repository owner or namespace.
    pub owner: ForgeFact<ProductText>,
    /// Description, when reported.
    pub description: ForgeFact<ProductText>,
    /// SPDX or forge license label, when reported.
    pub license: ForgeFact<ProductText>,
    /// README content or a bounded unavailable reason.
    pub readme: ForgeFact<ProductText>,
    /// Topics reported by the authority.
    pub topics: ForgeFact<Box<[ProductText]>>,
    /// Stars reported by the authority.
    pub stars: ForgeFact<u64>,
    /// Forks reported by the authority.
    pub forks: ForgeFact<u64>,
}

/// One package manifest discovered in an acquired forge tree.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ForgeManifestRecord {
    /// Path relative to the selected repository subdirectory.
    pub path: ProductText,
    /// Ecosystem recognized from the manifest.
    pub ecosystem: ProductText,
    /// Immutable package name when the manifest records one.
    pub name: Option<ProductText>,
    /// Immutable package version when the manifest records one.
    pub version: Option<ProductText>,
    /// Dependency facts admitted from this source manifest. Unknown and
    /// unavailable facts remain distinct from a known empty edge set.
    pub dependencies: DependencyFacts<Box<[PackageDependencyRecord]>>,
    /// Static Python declarations with source evidence, independent of registry authority.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub python_metadata: Option<crate::PythonProjectMetadata>,
}

/// Product DTO for a source acquired from GitHub, GitLab, Codeberg, or generic HTTPS Git.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ForgePackageRecord {
    /// Canonical coordinate as entered by the client.
    pub coordinate: ProductText,
    /// Canonical provider spelling.
    pub provider: ProductText,
    /// Owner/namespace from the coordinate.
    pub owner: ProductText,
    /// Repository name from the coordinate.
    pub repository: ProductText,
    /// Explicit revision spelling.
    pub revision: ProductText,
    /// Optional monorepo subdirectory.
    pub subdir: Option<ProductText>,
    /// Exact resolved commit object, if available.
    pub commit: ForgeFact<ProductText>,
    /// Exact resolved tree object, if the authority reports one.
    pub tree: ForgeFact<ProductText>,
    /// Bounded repository metadata.
    pub metadata: ForgeRepositoryMetadataRecord,
    /// Manifests admitted into the shared dependency graph.
    pub manifests: Box<[ForgeManifestRecord]>,
    /// Archive/tree availability state.
    pub source: ForgeFact<ProductText>,
}

/// Registry policy applied to an immutable release.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RegistryReleaseStanding {
    /// Offered to new resolutions.
    Available,
    /// Withdrawn by the publisher.
    Yanked,
    /// Retained with a publisher replacement recommendation.
    Deprecated,
    /// Addressable but hidden from normal listings.
    Unlisted,
    /// Excluded by ecosystem version policy.
    Retracted,
    /// Previously observed and now deleted upstream.
    Removed,
}

/// Download telemetry without conflating absent data with zero downloads.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", content = "value", rename_all = "kebab-case")]
pub enum RegistryDownloadCount {
    /// Exact cumulative release count.
    Exact(u64),
    /// Estimated count.
    Approximate(u64),
    /// Registry exposes no usable count and the source's typed coverage is
    /// carried without encoding meaning in human text.
    Unavailable(RegistryFactAvailability),
}

/// Typed coverage for a registry fact that is absent from a reply.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RegistryFactAvailability {
    /// The source does not publish the fact.
    NotRecorded,
    /// The source explicitly does not support the fact.
    Unsupported,
    /// The source should provide the fact, but it could not be obtained.
    Unavailable,
    /// The source answered with an older frontier.
    Stale,
    /// The source did not establish coverage.
    Unknown,
}

impl Default for RegistryFactAvailability {
    fn default() -> Self {
        Self::Unknown
    }
}

/// Availability of registry facts not present in every configured feed.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", content = "detail", rename_all = "kebab-case")]
pub enum RegistryMetadata<T> {
    /// Facts were recorded.
    Recorded(T),
    /// Exact observed facts, with additional rows withheld because the
    /// producer could not prove their source or coverage.
    Partial {
        /// Only facts whose identity was established.
        value: T,
        /// Why this answer is incomplete.
        reason: ProductText,
    },
    /// The configured feed does not publish this fact.
    NotRecorded(ProductText),
}

/// One durable package subscription.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubscriptionRecord {
    /// Followed package.
    pub package: PackageReference,
    /// Optional project folder.
    pub project: Option<ProjectId>,
    /// Newest version marked seen.
    pub seen: Option<ProductText>,
}

/// One release from the local registry catalog.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseRecord {
    /// Followed package.
    pub package: PackageReference,
    /// Recorded version.
    pub version: ProductText,
    /// Whether this request marked it seen.
    pub seen: bool,
}

/// One durable project folder.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectRecord {
    /// Stable identity.
    pub id: ProjectId,
    /// Unique name.
    pub name: ProjectName,
    /// Optional absolute lockfile path.
    pub lockfile: Option<ProductText>,
    /// Pinned members in canonical order.
    pub members: Box<[PackageReference]>,
    /// Indexed Cargo manifest names for members, parallel to `members`.
    #[serde(default = "empty_product_text_box")]
    pub member_manifest_names: Box<[ProductText]>,
}

fn empty_product_text_box() -> Box<[ProductText]> {
    Box::new([])
}

/// One shared session-tree node.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TreeNodeRecord {
    /// Stable identity.
    pub id: TreeNodeId,
    /// Parent node.
    pub parent: Option<TreeNodeId>,
    /// Displayed subject.
    pub subject: TreeSubject,
    /// Short title.
    pub title: ProductText,
    /// Calling surface.
    pub opener: TreeOpener,
    /// Whether this node is active.
    pub active: bool,
}

/// Typed result algebra for durable product commands.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "result", content = "data", rename_all = "kebab-case")]
pub enum SurfaceReply {
    /// Complete advisory facts and typed acquisition decision.
    Advisory(AdvisoryPackageDto),
    /// Per-locator declaration results.
    Read(Box<[DeclarationRecord]>),
    /// Source-verified uses of one declaration.
    References {
        /// Requested target coordinate, verbatim.
        target: ProductText,
        /// Bounded use sites, ordered by source position.
        references: Box<[ReferenceRecord]>,
    },
    /// Declaration-level differences.
    Diff(Box<[DiffRecord]>),
    /// Bounded catalog page.
    Explored(Box<[RegistryPackageRecord]>),
    /// Exact package records.
    Package(Box<[RegistryPackageRecord]>),
    /// Exact source metadata, kept separate from acquired package authority.
    PackageDiscovery {
        /// Exact version-pinned package requested by the caller.
        package: PackageCoordinate,
        /// Positive, negative, or unavailable metadata evidence.
        observation: RegistryPackageDiscoveryObservation,
    },
    /// Exact package records from registry and forge authorities, kept apart.
    PackageDetails {
        /// Acquired registry facts matching the requested PURL.
        registry: Box<[RegistryPackageRecord]>,
        /// Forge manifests matching the requested PURL.
        forge: Box<[ForgePackageDetailRecord]>,
    },
    /// Result of acquiring a forge source.
    ForgePackageAdded(ForgePackageRecord),
    /// Result of referencing a cached forge source.
    ForgePackageReferenced(ForgePackageRecord),
    /// Reverse dependency facts.
    Dependents(RegistryMetadata<Box<[RegistryPackageRecord]>>),
    /// Outgoing dependency facts.
    Dependencies(DependencyFacts<Box<[PackageDependencyRecord]>>),
    /// One immutable, bounded package graph page.
    PackageGraphPage(crate::PackageGraphPage),
    /// Publisher facts.
    Owner(RegistryMetadata<Box<[RegistryPackageRecord]>>),
    /// Bounded local index matches.
    IndexSearch(Box<[RegistryPackageRecord]>),
    /// Globally ranked search matches with acquired packages and source-only
    /// candidates represented by distinct typed hit variants.
    IndexSearchWithDiscovery(Box<[RegistrySearchHit]>),
    /// Cursor-based cross-plane page bound to one selected structural snapshot.
    IndexSearchPage(IndexSearchPage),
    /// Recorded versions.
    PackageVersions(Box<[RegistryPackageRecord]>),
    /// Immutable compiler generations for one exact package.
    SemanticVersions(Box<[SemanticVersionRecord]>),
    /// Egress view exported after direct shape certificate admission.
    SemanticShapes(crate::SemanticShapeExport),
    /// One bounded page of exact selected Project source membership.
    PackageSourceMembershipPage(crate::PackageSourceMembershipPageResultV1),
    /// Immediate status returned after an index start request.
    IndexStarted(IndexStartResult),
    /// Immediate status for a start or replay using a durable operation key.
    IndexOperationStarted(IndexOperationObservation),
    /// Durable status read by caller-owned operation key.
    IndexOperationStatus(IndexOperationObservation),
    /// Terminal receipt returned by an index waiter.
    IndexTerminal(IndexJobTerminal),
    /// Bounded typed progress facts returned for one exact index job.
    IndexProgress(IndexJobObservation),
    /// Immediate result of an owner-issued cancellation request.
    IndexCancellation(IndexCancelReceipt),
    /// Exact compiler generation selected by the durable owner.
    SemanticVersionSelected(SemanticVersionRecord),
    /// Latest package and history count.
    PackageProfile {
        /// Latest record.
        latest: Option<RegistryPackageRecord>,
        /// Recorded version count.
        versions: u64,
        /// Authority for the newest recorded release considered for latest.
        /// This remains populated when `latest` is withheld as historical.
        #[serde(default)]
        candidate_authority: Option<RegistryPackageFactAuthority>,
        /// Source packaging declarations from the selected local project; no registry authority implied.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source_metadata: Option<crate::PythonProjectMetadata>,
    },
    /// Subscription after mutation.
    Subscribed(SubscriptionRecord),
    /// Whether a subscription was removed.
    Unsubscribed(bool),
    /// Current subscriptions.
    Subscriptions(Box<[SubscriptionRecord]>),
    /// Current release feed.
    Releases(Box<[ReleaseRecord]>),
    /// Current projects.
    Projects(Box<[ProjectRecord]>),
    /// Created project.
    ProjectCreated(ProjectRecord),
    /// Deleted project identity.
    ProjectDeleted(ProjectId),
    /// Project after a member add.
    ProjectAdded(ProjectRecord),
    /// Project after a member removal.
    ProjectRemoved(ProjectRecord),
    /// Project after lockfile reconciliation.
    ProjectSynced(ProjectRecord),
    /// Current shared tree.
    Tree(Box<[TreeNodeRecord]>),
    /// Opened or focused node.
    TreeOpened(TreeNodeRecord),
    /// Number of closed nodes.
    TreeClosed(u64),
    /// One project's dependency tree.
    ProjectTree(Box<crate::browse::ProjectTree>),
    /// One source-file read under exact Cargo source authority.
    CargoPackageSourceFile(crate::CargoPackageSourceFileResultV1),
    /// Bounded source-file addresses under exact Cargo source authority.
    CargoPackageSourceInventory(crate::CargoPackageSourceInventoryResultV1),
    /// README selected by an exact Cargo package manifest and authority.
    CargoPackageReadme(crate::CargoPackageReadmeResultV1),
    /// Text or an anchor followed from an exact package README origin.
    CargoPackageReadmeLink(crate::CargoPackageReadmeLinkResultV1),
    /// Each advisory source after a refresh.
    AdvisoryRefreshed(Box<[crate::browse::AdvisorySourceState]>),
}

impl SurfaceReply {
    /// Returns the exact command identity required by this reply.
    #[must_use]
    pub const fn id(&self) -> CommandId {
        match self {
            Self::Advisory(_) => CommandId::Advisory,
            Self::Read(_) => CommandId::Read,
            Self::References { .. } => CommandId::References,
            Self::Diff(_) => CommandId::Diff,
            Self::Explored(_) => CommandId::Explore,
            Self::Package(_) => CommandId::Package,
            Self::PackageDiscovery { .. } => CommandId::Package,
            Self::PackageDetails { .. } => CommandId::Package,
            Self::ForgePackageAdded(_) => CommandId::ForgeAdd,
            Self::ForgePackageReferenced(_) => CommandId::ForgeReference,
            Self::Dependents(_) => CommandId::Dependents,
            Self::Dependencies(_) => CommandId::Dependencies,
            Self::PackageGraphPage(_) => CommandId::PackageGraphPage,
            Self::Owner(_) => CommandId::Owner,
            Self::IndexSearch(_) => CommandId::IndexSearch,
            Self::IndexSearchWithDiscovery(_) => CommandId::IndexSearch,
            Self::IndexSearchPage(_) => CommandId::IndexSearch,
            Self::PackageVersions(_) => CommandId::PackageVersions,
            Self::SemanticVersions(_) => CommandId::SemanticVersions,
            Self::SemanticShapes(_) => CommandId::SemanticShapes,
            Self::PackageSourceMembershipPage(_) => CommandId::PackageSourceMembership,
            Self::IndexStarted(_) => CommandId::IndexStart,
            Self::IndexOperationStarted(_) => CommandId::IndexStart,
            Self::IndexOperationStatus(_) => CommandId::IndexProgress,
            Self::IndexTerminal(_) => CommandId::IndexAwait,
            Self::IndexProgress(_) => CommandId::IndexProgress,
            Self::IndexCancellation(_) => CommandId::IndexCancel,
            Self::SemanticVersionSelected(_) => CommandId::SelectSemanticVersion,
            Self::PackageProfile { .. } => CommandId::PackageProfile,
            Self::Subscribed(_) => CommandId::Subscribe,
            Self::Unsubscribed(_) => CommandId::Unsubscribe,
            Self::Subscriptions(_) => CommandId::Subscriptions,
            Self::Releases(_) => CommandId::Releases,
            Self::Projects(_) => CommandId::Projects,
            Self::ProjectCreated(_) => CommandId::ProjectCreate,
            Self::ProjectDeleted(_) => CommandId::ProjectDelete,
            Self::ProjectAdded(_) => CommandId::ProjectAdd,
            Self::ProjectRemoved(_) => CommandId::ProjectRemove,
            Self::ProjectSynced(_) => CommandId::ProjectSync,
            Self::Tree(_) => CommandId::Tree,
            Self::TreeOpened(_) => CommandId::TreeOpen,
            Self::TreeClosed(_) => CommandId::TreeClose,
            Self::ProjectTree(_) => CommandId::ProjectTree,
            Self::CargoPackageSourceFile(_) => CommandId::CargoPackageSourceFile,
            Self::CargoPackageSourceInventory(_) => CommandId::CargoPackageSourceInventory,
            Self::CargoPackageReadme(_) => CommandId::CargoPackageReadme,
            Self::CargoPackageReadmeLink(_) => CommandId::CargoPackageReadmeLink,
            Self::AdvisoryRefreshed(_) => CommandId::AdvisoryRefresh,
        }
    }
    /// Verifies command identity and reply collection bounds.
    ///
    /// # Errors
    ///
    /// Returns [`ProductAdmissionError`] when the reply belongs to another command or exceeds the
    /// fixed product row bound.
    #[allow(
        clippy::match_same_arms,
        reason = "distinct typed reply collections share only their length admission rule"
    )]
    pub fn admit(&self, expected: CommandId) -> Result<(), ProductAdmissionError> {
        if self.id() != expected {
            return Err(ProductAdmissionError::CommandMismatch);
        }
        let count = match self {
            Self::Advisory(_) => 1,
            Self::Read(v) => v.len(),
            Self::References { references, .. } => references.len(),
            Self::Diff(rows) => rows.iter().try_fold(rows.len(), |count, row| {
                if !row.has_valid_identity_shape()
                    || row
                        .links
                        .iter()
                        .any(|link| !link.matches_declaration_sides(row.before, row.after))
                {
                    return Err(ProductAdmissionError::DiffShape);
                }
                count
                    .checked_add(row.links.len())
                    .ok_or(ProductAdmissionError::RowBound)
            })?,
            Self::Explored(v)
            | Self::Package(v)
            | Self::IndexSearch(v)
            | Self::PackageVersions(v)
            | Self::Dependents(RegistryMetadata::Recorded(v))
            | Self::Owner(RegistryMetadata::Recorded(v))
            | Self::Dependents(RegistryMetadata::Partial { value: v, .. })
            | Self::Owner(RegistryMetadata::Partial { value: v, .. }) => v.len(),
            Self::IndexSearchWithDiscovery(hits) => hits.len(),
            Self::PackageDiscovery { observation, .. } => match observation {
                RegistryPackageDiscoveryObservation::Observed { candidates } => candidates.len(),
                _ => 1,
            },
            Self::PackageDetails { registry, forge } => registry.len().saturating_add(forge.len()),
            Self::IndexSearchPage(page) => page.hits.len(),
            Self::Dependencies(DependencyFacts::Known(v)) => v.len(),
            Self::PackageGraphPage(page) => {
                page.admit()
                    .map_err(|_| ProductAdmissionError::PackageGraphPage)?;
                page.rows.len()
            }
            Self::SemanticVersions(records) => {
                let mut selected_targets = BTreeSet::new();
                let mut response_package = None;
                let mut selected_frontier: Option<Option<&SelectedProjectSourceFrontier>> = None;
                for record in records {
                    record.admit()?;
                    if response_package.is_some_and(|prior| prior != &record.package) {
                        return Err(ProductAdmissionError::SemanticVersionShape);
                    }
                    response_package = Some(&record.package);
                    if record.selected {
                        if !selected_targets.insert((record.coordinate.as_str(), record.profile)) {
                            return Err(ProductAdmissionError::SemanticVersionShape);
                        }
                        let frontier = record.selected_source_frontier.as_ref();
                        if selected_frontier.is_some_and(|prior| prior != frontier) {
                            return Err(ProductAdmissionError::SemanticVersionShape);
                        }
                        selected_frontier = Some(frontier);
                    }
                }
                records.len()
            }
            Self::SemanticShapes(export) => {
                export
                    .encode_bounded_json()
                    .map_err(|_| ProductAdmissionError::SemanticVersionShape)?;
                export.entry_count()
            }
            Self::PackageSourceMembershipPage(result) => {
                if !result.has_admissible_shape() {
                    return Err(ProductAdmissionError::PackageSourceMembershipShape);
                }
                match result {
                    crate::PackageSourceMembershipPageResultV1::Page { files, .. } => files.len(),
                    crate::PackageSourceMembershipPageResultV1::Stale { .. }
                    | crate::PackageSourceMembershipPageResultV1::Unavailable { .. } => 1,
                }
            }
            Self::SemanticVersionSelected(record) => {
                record.admit()?;
                if !record.selected {
                    return Err(ProductAdmissionError::SemanticVersionShape);
                }
                1
            }
            Self::IndexStarted(result) => {
                result.admit()?;
                1
            }
            Self::IndexTerminal(terminal) => {
                terminal.admit()?;
                1
            }
            Self::IndexOperationStarted(observation) | Self::IndexOperationStatus(observation) => {
                observation.admit()?;
                1
            }
            Self::IndexCancellation(receipt) => {
                receipt.admit()?;
                1
            }
            Self::IndexProgress(observation) => {
                observation.admit()?;
                match observation {
                    IndexJobObservation::Pending(page) => page.events.len(),
                    IndexJobObservation::Terminal(_) | IndexJobObservation::Unknown { .. } => 1,
                }
            }
            Self::Subscriptions(v) => v.len(),
            Self::Releases(v) => v.len(),
            Self::Projects(v) => v.len(),
            Self::Tree(v) => v.len(),
            Self::AdvisoryRefreshed(v) => v.len(),
            Self::CargoPackageSourceFile(result) if !result.has_admissible_shape() => {
                return Err(ProductAdmissionError::CargoSourceShape);
            }
            Self::CargoPackageSourceInventory(result) if !result.has_admissible_shape() => {
                return Err(ProductAdmissionError::CargoSourceShape);
            }
            Self::CargoPackageReadme(result) if !result.has_admissible_shape() => {
                return Err(ProductAdmissionError::CargoSourceShape);
            }
            Self::CargoPackageReadmeLink(result) if !result.has_admissible_shape() => {
                return Err(ProductAdmissionError::CargoSourceShape);
            }
            Self::ProjectTree(tree)
                if tree.packages.len() > crate::browse::MAX_TREE_PACKAGES
                    || tree.direct.len() > tree.packages.len() =>
            {
                return Err(ProductAdmissionError::RowBound);
            }
            Self::ProjectTree(tree) if !tree.has_admissible_shape() => {
                return Err(ProductAdmissionError::CargoSourceShape);
            }
            _ => 1,
        };
        if count > MAX_PRODUCT_ROWS {
            Err(ProductAdmissionError::RowBound)
        } else {
            match self {
                Self::Explored(rows)
                | Self::Package(rows)
                | Self::IndexSearch(rows)
                | Self::PackageVersions(rows) => {
                    for row in rows {
                        admit_registry_record(row)?;
                    }
                }
                Self::IndexSearchWithDiscovery(hits) => {
                    for hit in hits {
                        match hit {
                            RegistrySearchHit::Acquired(record) => admit_registry_record(record)?,
                            RegistrySearchHit::LocalDeclaration(record) => record.admit()?,
                            RegistrySearchHit::Discovered(candidate) => candidate.admit()?,
                            RegistrySearchHit::ForgeDiscovered(candidate) => candidate.admit()?,
                            RegistrySearchHit::ForgeSourcePin(candidate) => candidate.admit()?,
                            RegistrySearchHit::PackageGroup(group) => group.admit()?,
                        }
                    }
                }
                Self::PackageDiscovery {
                    package,
                    observation,
                } => {
                    if let RegistryPackageDiscoveryObservation::Observed { candidates } =
                        observation
                    {
                        if candidates.is_empty() {
                            return Err(ProductAdmissionError::RegistryAuthority);
                        }
                        for candidate in candidates {
                            candidate.admit()?;
                            if &candidate.coordinate != package {
                                return Err(ProductAdmissionError::RegistryAuthority);
                            }
                        }
                    }
                }
                Self::IndexSearchPage(page) => {
                    if page.hits.len() > MAX_PRODUCT_ROWS {
                        return Err(ProductAdmissionError::RowBound);
                    }
                    if page.next_cursor.as_ref().is_some_and(|cursor| {
                        IndexSearchCursor::new(cursor.as_str().to_owned()).is_err()
                    }) {
                        return Err(ProductAdmissionError::IndexSearchCursor);
                    }
                    for hit in page.hits.iter() {
                        match hit {
                            RegistrySearchHit::Acquired(record) => admit_registry_record(record)?,
                            RegistrySearchHit::LocalDeclaration(record) => record.admit()?,
                            RegistrySearchHit::Discovered(candidate) => candidate.admit()?,
                            RegistrySearchHit::ForgeDiscovered(candidate) => candidate.admit()?,
                            RegistrySearchHit::ForgeSourcePin(candidate) => candidate.admit()?,
                            RegistrySearchHit::PackageGroup(group) => group.admit()?,
                        }
                    }
                }
                Self::Dependents(RegistryMetadata::Recorded(rows))
                | Self::Owner(RegistryMetadata::Recorded(rows))
                | Self::Dependents(RegistryMetadata::Partial { value: rows, .. })
                | Self::Owner(RegistryMetadata::Partial { value: rows, .. }) => {
                    for row in rows {
                        admit_registry_record(row)?;
                    }
                }
                Self::PackageDetails { registry, forge } => {
                    for row in registry {
                        admit_registry_record(row)?;
                    }
                    for row in forge {
                        row.admit()?;
                    }
                }
                Self::ForgePackageAdded(record) | Self::ForgePackageReferenced(record) => {
                    for manifest in &record.manifests {
                        if let Some(metadata) = &manifest.python_metadata {
                            metadata.admit()?;
                        }
                    }
                }
                Self::PackageProfile {
                    latest,
                    candidate_authority,
                    source_metadata,
                    ..
                } => {
                    if let Some(row) = latest {
                        admit_registry_record(row)?;
                        if candidate_authority != &row.authority {
                            return Err(ProductAdmissionError::RegistryAuthority);
                        }
                    }
                    if let Some(metadata) = source_metadata {
                        metadata.admit()?;
                        if latest.as_ref().is_some_and(|row| {
                            row.authority.is_none()
                                && row.ecosystem == RegistryEcosystem::Pypi
                                && (metadata.name.recorded().map(String::as_str)
                                    != Some(row.name.as_str())
                                    || metadata.version.recorded().map(String::as_str)
                                        != Some(row.version.as_str())
                                    || metadata.digest() != row.facts_version)
                        }) {
                            return Err(ProductAdmissionError::PythonMetadata);
                        }
                    }
                }
                _ => {}
            }
            Ok(())
        }
    }

    pub(crate) fn encoded_size_bound(&self) -> usize {
        const ENVELOPE_BYTES: usize = 512;
        let payload = match self {
            Self::Read(records) => records.iter().fold(0_usize, |bound, record| {
                bound.saturating_add(declaration_record_bound(record))
            }),
            Self::Advisory(value) => fixed_record_bound()
                .saturating_add(serde_json::to_vec(value).map_or(0, |bytes| bytes.len())),
            Self::References { target, references } => references.iter().fold(
                fixed_record_bound().saturating_add(text_bound(target)),
                |bound, record| {
                    bound
                        .saturating_add(fixed_record_bound())
                        .saturating_add(text_bound(&record.site))
                        .saturating_add(semantic_link_evidence_bound(&record.evidence))
                },
            ),
            Self::Diff(records) => records.iter().fold(0_usize, |bound, record| {
                bound.saturating_add(diff_record_bound(record))
            }),
            Self::Explored(records)
            | Self::Package(records)
            | Self::IndexSearch(records)
            | Self::PackageVersions(records)
            | Self::Dependents(RegistryMetadata::Recorded(records))
            | Self::Owner(RegistryMetadata::Recorded(records)) => registry_records_bound(records),
            Self::Dependents(RegistryMetadata::Partial { value, reason })
            | Self::Owner(RegistryMetadata::Partial { value, reason }) => {
                registry_records_bound(value).saturating_add(text_bound(reason))
            }
            Self::IndexSearchWithDiscovery(hits) => hits.iter().fold(0_usize, |bound, hit| {
                bound.saturating_add(match hit {
                    RegistrySearchHit::Acquired(record) => registry_package_record_bound(record),
                    RegistrySearchHit::LocalDeclaration(record) => fixed_record_bound()
                        .saturating_add(text_bound(&record.coordinate))
                        .saturating_add(text_bound(&record.name))
                        .saturating_add(text_bound(&record.path)),
                    RegistrySearchHit::Discovered(candidate) => {
                        fixed_record_bound().saturating_add(candidate.coordinate.as_str().len())
                    }
                    RegistrySearchHit::ForgeDiscovered(candidate) => fixed_record_bound()
                        .saturating_add(
                            serde_json::to_vec(candidate).map_or(0, |bytes| bytes.len()),
                        ),
                    RegistrySearchHit::ForgeSourcePin(candidate) => fixed_record_bound()
                        .saturating_add(
                            serde_json::to_vec(candidate).map_or(0, |bytes| bytes.len()),
                        ),
                    RegistrySearchHit::PackageGroup(group) => fixed_record_bound()
                        .saturating_add(serde_json::to_vec(group).map_or(0, |bytes| bytes.len())),
                })
            }),
            Self::IndexSearchPage(page) => fixed_record_bound()
                .saturating_add(serde_json::to_vec(page).map_or(0, |bytes| bytes.len())),
            Self::PackageDiscovery {
                package,
                observation,
            } => fixed_record_bound()
                .saturating_add(package.as_str().len())
                .saturating_add(serialized_json_size(observation)),
            Self::ForgePackageAdded(record) | Self::ForgePackageReferenced(record) => {
                fixed_record_bound()
                    .saturating_add(serde_json::to_vec(record).map_or(0, |bytes| bytes.len()))
            }
            Self::PackageDetails { registry, forge } => registry_records_bound(registry)
                .saturating_add(forge.iter().fold(0_usize, |bound, record| {
                    bound.saturating_add(
                        fixed_record_bound().saturating_add(
                            serde_json::to_vec(record).map_or(0, |bytes| bytes.len()),
                        ),
                    )
                })),
            Self::SemanticVersions(records) => records.iter().fold(0_usize, |bound, record| {
                bound
                    .saturating_add(fixed_record_bound())
                    .saturating_add(package_reference_bound(&record.package))
                    .saturating_add(record.coordinate.as_str().len())
                    .saturating_add(serialized_json_size(&record.history_status))
                    .saturating_add(serialized_json_size(&record.selected_source_frontier))
            }),
            Self::SemanticVersionSelected(record) => fixed_record_bound()
                .saturating_add(package_reference_bound(&record.package))
                .saturating_add(record.coordinate.as_str().len())
                .saturating_add(serialized_json_size(&record.history_status))
                .saturating_add(serialized_json_size(&record.selected_source_frontier)),
            Self::SemanticShapes(export) => export
                .encode_bounded_json()
                .map_or(usize::MAX, |bytes| bytes.len()),
            Self::PackageSourceMembershipPage(result) => {
                serde_json::to_vec(result).map_or(0, |bytes| bytes.len())
            }
            Self::IndexStarted(result) => fixed_record_bound()
                .saturating_add(serde_json::to_vec(result).map_or(0, |bytes| bytes.len())),
            Self::IndexOperationStarted(observation) | Self::IndexOperationStatus(observation) => {
                fixed_record_bound()
                    .saturating_add(serde_json::to_vec(observation).map_or(0, |bytes| bytes.len()))
            }
            Self::IndexTerminal(terminal) => fixed_record_bound()
                .saturating_add(serde_json::to_vec(terminal).map_or(0, |bytes| bytes.len())),
            Self::IndexProgress(page) => fixed_record_bound()
                .saturating_add(serde_json::to_vec(page).map_or(0, |bytes| bytes.len())),
            Self::IndexCancellation(receipt) => fixed_record_bound()
                .saturating_add(serde_json::to_vec(receipt).map_or(0, |bytes| bytes.len())),
            Self::Dependents(RegistryMetadata::NotRecorded(reason))
            | Self::Owner(RegistryMetadata::NotRecorded(reason)) => text_bound(reason),
            Self::Dependencies(DependencyFacts::Known(records)) => {
                records.iter().fold(0_usize, |bound, record| {
                    bound.saturating_add(dependency_record_bound(record))
                })
            }
            Self::Dependencies(DependencyFacts::Unknown(reason))
            | Self::Dependencies(DependencyFacts::Unavailable(reason)) => text_bound(reason),
            Self::PackageGraphPage(page) => fixed_record_bound()
                .saturating_add(serde_json::to_vec(page).map_or(0, |bytes| bytes.len())),
            Self::PackageProfile {
                latest,
                candidate_authority,
                source_metadata,
                ..
            } => latest
                .as_ref()
                .map_or(64, registry_package_record_bound)
                .saturating_add(
                    serde_json::to_vec(candidate_authority).map_or(0, |bytes| bytes.len()),
                )
                .saturating_add(serde_json::to_vec(source_metadata).map_or(0, |bytes| bytes.len())),
            Self::Subscribed(record) => subscription_record_bound(record),
            Self::Unsubscribed(_) | Self::ProjectDeleted(_) | Self::TreeClosed(_) => 64,
            Self::Subscriptions(records) => records.iter().fold(0_usize, |bound, record| {
                bound.saturating_add(subscription_record_bound(record))
            }),
            Self::Releases(records) => records.iter().fold(0_usize, |bound, record| {
                bound.saturating_add(release_record_bound(record))
            }),
            Self::Projects(records) => records.iter().fold(0_usize, |bound, record| {
                bound.saturating_add(project_record_bound(record))
            }),
            Self::ProjectCreated(record)
            | Self::ProjectAdded(record)
            | Self::ProjectRemoved(record)
            | Self::ProjectSynced(record) => project_record_bound(record),
            Self::Tree(records) => records.iter().fold(0_usize, |bound, record| {
                bound.saturating_add(tree_node_record_bound(record))
            }),
            Self::TreeOpened(record) => tree_node_record_bound(record),
            Self::ProjectTree(tree) => serde_json::to_vec(tree).map_or(0, |bytes| bytes.len()),
            Self::CargoPackageSourceFile(result) => {
                serde_json::to_vec(result).map_or(0, |bytes| bytes.len())
            }
            Self::CargoPackageSourceInventory(result) => {
                serde_json::to_vec(result).map_or(0, |bytes| bytes.len())
            }
            Self::CargoPackageReadme(result) => {
                serde_json::to_vec(result).map_or(0, |bytes| bytes.len())
            }
            Self::CargoPackageReadmeLink(result) => {
                serde_json::to_vec(result).map_or(0, |bytes| bytes.len())
            }
            Self::AdvisoryRefreshed(states) => {
                serde_json::to_vec(states).map_or(0, |bytes| bytes.len())
            }
        };
        ENVELOPE_BYTES.saturating_add(payload)
    }
}

const fn fixed_record_bound() -> usize {
    512
}

#[derive(Default)]
struct JsonSizeCounter(usize);

impl Write for JsonSizeCounter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0 = self.0.saturating_add(bytes.len());
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Counts the encoded size without allocating a temporary serialized copy.
fn serialized_json_size(value: &impl Serialize) -> usize {
    let mut counter = JsonSizeCounter::default();
    if serde_json::to_writer(&mut counter, value).is_ok() {
        counter.0
    } else {
        usize::MAX
    }
}

fn text_bound(text: &ProductText) -> usize {
    text.as_str().len().saturating_add(32)
}

fn optional_text_bound(text: Option<&ProductText>) -> usize {
    text.map_or(16, text_bound)
}

fn package_reference_bound(package: &PackageReference) -> usize {
    package.as_str().len().saturating_add(64)
}

fn declaration_record_bound(record: &DeclarationRecord) -> usize {
    fixed_record_bound()
        .saturating_add(text_bound(&record.label))
        .saturating_add(optional_text_bound(record.signature.as_ref()))
}

fn diff_record_bound(record: &DiffRecord) -> usize {
    record.links.iter().fold(
        fixed_record_bound().saturating_add(text_bound(&record.label)),
        |bound, link| bound.saturating_add(semantic_link_delta_bound(link)),
    )
}

fn semantic_link_delta_bound(delta: &SemanticLinkDelta) -> usize {
    let evidence = match delta {
        SemanticLinkDelta::Added { evidence, .. } | SemanticLinkDelta::Removed { evidence, .. } => {
            semantic_link_evidence_bound(evidence)
        }
        SemanticLinkDelta::EvidenceChanged { before, after, .. } => {
            semantic_link_evidence_bound(before).saturating_add(semantic_link_evidence_bound(after))
        }
    };
    fixed_record_bound().saturating_add(evidence)
}

fn semantic_link_evidence_bound(evidence: &SemanticLinkEvidence) -> usize {
    evidence.source.as_ref().map_or(128, |source| {
        fixed_record_bound().saturating_add(text_bound(&source.file))
    })
}

fn registry_records_bound(records: &[RegistryPackageRecord]) -> usize {
    records.iter().fold(0_usize, |bound, record| {
        bound.saturating_add(registry_package_record_bound(record))
    })
}

fn registry_package_record_bound(record: &RegistryPackageRecord) -> usize {
    // The closed authority DTO contains six 32-byte identities, at most three
    // more in a negative-fact proof, and a bounded set of enum/integer fields.
    // 2 KiB safely covers their JSON form without allocating on each reply.
    const REGISTRY_AUTHORITY_BOUND: usize = 2_048;
    fixed_record_bound()
        .saturating_add(package_reference_bound(&record.coordinate))
        .saturating_add(text_bound(&record.name))
        .saturating_add(text_bound(&record.version))
        .saturating_add(serde_json::to_vec(&record.native_metadata).map_or(0, |bytes| bytes.len()))
        .saturating_add(serde_json::to_vec(&record.forge_sources).map_or(0, |bytes| bytes.len()))
        .saturating_add(if record.authority.is_some() {
            REGISTRY_AUTHORITY_BOUND
        } else {
            0
        })
        .saturating_add(serde_json::to_vec(&record.advisory).map_or(0, |bytes| bytes.len()))
}

fn admit_registry_record(record: &RegistryPackageRecord) -> Result<(), ProductAdmissionError> {
    let native_identity_matches = record
        .native_metadata
        .identity()
        .is_ok_and(|identity| identity == record.native_metadata_version);
    if record.native_metadata.admit().is_err() || !native_identity_matches {
        return Err(ProductAdmissionError::NativeMetadata);
    }
    if record.authority.is_some_and(|authority| {
        authority.facts_version != record.facts_version
            || matches!(
                authority.release_facts_freshness,
                RegistryPackageFactFreshness::Current {
                    observed_at_millis,
                    valid_until_millis,
                    ..
                } if valid_until_millis < observed_at_millis
            )
    }) {
        return Err(ProductAdmissionError::RegistryAuthority);
    }
    if record.forge_sources.len() > crate::MAX_REGISTRY_FORGE_ASSOCIATIONS
        || record
            .forge_sources
            .iter()
            .any(|association| association.admit_for_registry(&record.coordinate).is_err())
    {
        return Err(ProductAdmissionError::ForgeAssociation);
    }
    Ok(())
}

fn dependency_record_bound(record: &PackageDependencyRecord) -> usize {
    fixed_record_bound()
        .saturating_add(package_reference_bound(&record.source))
        .saturating_add(text_bound(&record.target.name))
        .saturating_add(text_bound(&record.target.requirement))
        .saturating_add(
            record
                .target
                .resolved
                .as_ref()
                .map_or(0, package_reference_bound),
        )
}

fn subscription_record_bound(record: &SubscriptionRecord) -> usize {
    fixed_record_bound()
        .saturating_add(package_reference_bound(&record.package))
        .saturating_add(optional_text_bound(record.seen.as_ref()))
}

fn release_record_bound(record: &ReleaseRecord) -> usize {
    fixed_record_bound()
        .saturating_add(package_reference_bound(&record.package))
        .saturating_add(text_bound(&record.version))
}

fn project_record_bound(record: &ProjectRecord) -> usize {
    record.members.iter().fold(
        fixed_record_bound()
            .saturating_add(record.name.as_str().len())
            .saturating_add(optional_text_bound(record.lockfile.as_ref())),
        |bound, package| bound.saturating_add(package_reference_bound(package)),
    )
}

fn tree_node_record_bound(record: &TreeNodeRecord) -> usize {
    fixed_record_bound()
        .saturating_add(tree_subject_bound(&record.subject))
        .saturating_add(text_bound(&record.title))
        .saturating_add(match &record.opener {
            TreeOpener::Mcp(client) => text_bound(client),
            TreeOpener::Desktop | TreeOpener::Cli => 32,
        })
}

fn tree_subject_bound(subject: &TreeSubject) -> usize {
    match subject {
        TreeSubject::Package(package) => package_reference_bound(package),
        TreeSubject::Declaration(text) | TreeSubject::Search(text) | TreeSubject::Owner(text) => {
            text_bound(text)
        }
        TreeSubject::Explore(query) => optional_text_bound(query.as_ref()),
    }
}

/// Semantic refusal while admitting a parsed product value.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProductAdmissionError {
    /// Required text was blank.
    EmptyText,
    /// Text exceeded its byte bound or contained NUL.
    TextBound,
    /// Package locator grammar was invalid.
    PackageReference,
    /// Project name grammar was invalid.
    ProjectName,
    /// A collection exceeded its row bound.
    RowBound,
    /// Reply and command identities differ.
    CommandMismatch,
    /// A semantic diff contradicts its declaration sides or graph evidence lifecycle.
    DiffShape,
    /// A semantic generation record or selection is internally inconsistent.
    SemanticVersionShape,
    /// A dependency row has a stale or duplicated content identity.
    DependencyShape,
    /// Static Python declarations have malformed bounds or detached source evidence.
    PythonMetadata,
    /// Native registry metadata is malformed, oversized, or has a stale identity.
    NativeMetadata,
    /// Registry fact authority is inconsistent with the selected package facts.
    RegistryAuthority,
    /// An unacquired discovery candidate has invalid freshness evidence.
    RegistryDiscoveryFreshness,
    /// A registry discovery metadata facet is outside the product bounds.
    RegistryDiscoveryMetadata,
    /// A local search hit has no declaration scope or contradicts its source name.
    LocalDeclarationSearchShape,
    /// An index-search continuation is malformed or exceeds its byte bound.
    IndexSearchCursor,
    /// A forge search hit is not bound to its source or manifest coordinate.
    ForgeSearchShape,
    /// A forge package detail has inconsistent source, manifest, pin, or registry facts.
    ForgePackageDetailShape,
    /// A package-lineage search group mixes source, ecosystem, or coordinates.
    RegistrySearchGroup,
    /// Registry-to-forge lineage facts are malformed, stale, or attached to another package.
    ForgeAssociation,
    /// A package graph request or page is malformed or outside its bound.
    PackageGraphPage,
    /// Cargo source package or relative file authority is malformed.
    CargoSourceShape,
    /// Selected Project source-membership request or page is malformed.
    PackageSourceMembershipShape,
    /// An index progress page contains mismatched tickets or invalid sequence/profile facts.
    IndexProgressShape,
    /// An index cancellation terminal receipt belongs to a different ticket.
    IndexCancelTicketMismatch,
    /// A durable operation key is reserved, malformed, or has invalid evidence.
    IndexOperationKey,
    /// A durable operation receipt contradicts its package, revision, or publication evidence.
    IndexOperationShape,
    /// A package compiler refusal summary is malformed, unbounded, or internally inconsistent.
    PackageCompilerFailureShape,
}
impl core::fmt::Display for ProductAdmissionError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::EmptyText => "product text is empty",
            Self::TextBound => "product text exceeds its bound or contains NUL",
            Self::PackageReference => "package reference is not canonical",
            Self::ProjectName => "project name is invalid",
            Self::RowBound => "product row count exceeds its bound",
            Self::CommandMismatch => "product reply does not match its command",
            Self::DiffShape => "semantic diff has inconsistent identities or evidence",
            Self::SemanticVersionShape => "semantic version selection is inconsistent",
            Self::DependencyShape => "dependency fact has an invalid or duplicate identity",
            Self::PythonMetadata => "Python metadata has invalid source evidence or bounds",
            Self::NativeMetadata => "native registry metadata is invalid or has a stale identity",
            Self::RegistryAuthority => {
                "registry fact authority is inconsistent with selected package facts"
            }
            Self::RegistryDiscoveryFreshness => {
                "registry discovery freshness evidence is inconsistent"
            }
            Self::RegistryDiscoveryMetadata => {
                "registry discovery metadata exceeds its evidence bounds"
            }
            Self::LocalDeclarationSearchShape => {
                "local search declaration identity is inconsistent"
            }
            Self::IndexSearchCursor => "index-search cursor is malformed or too large",
            Self::ForgeSearchShape => "forge search candidate is not bound to its source manifest",
            Self::ForgePackageDetailShape => {
                "forge package detail has inconsistent source, manifest, or pin facts"
            }
            Self::RegistrySearchGroup => {
                "registry search group has inconsistent lineage or provenance"
            }
            Self::ForgeAssociation => {
                "registry-to-forge lineage is invalid or has a stale identity"
            }
            Self::PackageGraphPage => "package graph request or page is invalid",
            Self::CargoSourceShape => "Cargo source authority or relative file path is invalid",
            Self::PackageSourceMembershipShape => {
                "selected Project source membership is malformed or outside its bound"
            }
            Self::IndexProgressShape => {
                "index progress page has inconsistent sequence or profile facts"
            }
            Self::IndexCancelTicketMismatch => {
                "index cancellation terminal receipt has a different ticket"
            }
            Self::IndexOperationKey => "index operation key is malformed or reserved",
            Self::IndexOperationShape => "index operation status has inconsistent evidence",
            Self::PackageCompilerFailureShape => {
                "package compiler failure summary is malformed or inconsistent"
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn compiler_failure(path: &str) -> PackageCompilerFailure {
        use crate::interface::{CompilerFragmentFailure, SourceAuthority};
        use backend_semantic::ir::{BuildError, EntityId};

        let attempt = CompilerAttempt {
            source: SourceAuthority {
                identity: ContentId::<SourceFactDomain>::from_canonical_bytes(b"source bytes"),
                byte_len: 12,
            },
            recipe: ContentId::<CompileRecipeDomain>::from_canonical_bytes(b"recipe bytes"),
        };
        let failure = CompilerFragmentFailure::build(BuildError::InvalidOccurrenceSpan {
            owner: EntityId::new(7),
            start: 18,
            end: 24,
        });
        PackageCompilerFailure::from_fragment_failure(path, attempt, &failure)
            .expect("a bounded package compiler failure")
    }

    #[test]
    fn package_compiler_failure_json_keeps_path_authorities_and_specific_facts() {
        let failure = compiler_failure("src/recovery.ts");
        let encoded = serde_json::to_vec(&failure).expect("encode compiler failure");
        let decoded: PackageCompilerFailure =
            serde_json::from_slice(&encoded).expect("decode compiler failure");
        assert_eq!(decoded, failure);
        assert_eq!(decoded.relative_path(), "src/recovery.ts");
        assert_eq!(decoded.source_byte_len(), 12);
        assert_eq!(decoded.kind_tag(), "build_invalid_occurrence_span");
        assert_eq!(decoded.phase(), PackageCompilerFailurePhase::Prepare);
        assert!(decoded.detail().contains("occurrence span"));

        let command_failure = crate::CommandFailure::CompilerRefused {
            detail: "local compiler rejected src/recovery.ts".to_owned(),
            failure: failure.clone(),
        };
        let displayed = command_failure.to_string();
        assert!(displayed.contains("src/recovery.ts"));
        assert!(displayed.contains("build_invalid_occurrence_span"));
        assert!(displayed.contains("occurrence span"));
        assert!(!displayed.contains("facts="));

        let underscored_path = compiler_failure("src/foo_bar.ts");
        assert_eq!(underscored_path.relative_path(), "src/foo_bar.ts");
        assert!(
            crate::CommandFailure::CompilerRefused {
                detail: "compiler refused source member".to_owned(),
                failure: underscored_path,
            }
            .to_string()
            .contains("src/foo_bar.ts")
        );

        let json: serde_json::Value = serde_json::from_slice(&encoded).expect("JSON object");
        assert_eq!(json["cause"]["family"], "fragment");
        assert_eq!(json["cause"]["fault"]["kind"]["family"], "build");
        assert_eq!(
            json["cause"]["fault"]["kind"]["kind"],
            "invalid_occurrence_span"
        );
        assert_eq!(json["cause"]["fault"]["facts"]["kind"], "occurrence_span");
        assert_eq!(json["cause"]["fault"]["facts"]["owner"], 7);
        assert_eq!(json["cause"]["fault"]["facts"]["start"], 18);
        assert_eq!(json["cause"]["fault"]["facts"]["end"], 24);
        assert!(
            json["recipe_identity"]
                .as_str()
                .is_some_and(|value| value.starts_with("content:"))
        );
        assert_eq!(json["phase"], "prepare");
        assert!(encoded.len() <= PackageCompilerFailure::MAX_ENCODED_BYTES);
        assert_eq!(
            failure.encoded_size_bound(),
            PackageCompilerFailure::MAX_ENCODED_BYTES
        );

        let outcome = IndexJobOutcome::RefusedWithCompilerFailure {
            detail: ProductText::new("local compiler refused src/recovery.ts")
                .expect("bounded refusal text"),
            failure: failure.clone(),
        };
        let outcome_json = serde_json::to_value(&outcome).expect("index outcome JSON");
        assert_eq!(outcome_json["state"], "refused-with-compiler-failure");
        let decoded_outcome = serde_json::from_value::<IndexJobOutcome>(outcome_json.clone())
            .expect("decode typed compiler-refusal outcome");
        assert_eq!(decoded_outcome, outcome);
        let mut unknown = outcome_json;
        unknown["detail"]["extra"] = serde_json::json!("unknown future member");
        assert!(serde_json::from_value::<IndexJobOutcome>(unknown).is_err());
    }

    #[test]
    fn package_compiler_failure_rejects_inconsistent_and_unbounded_wire_data() {
        let failure = compiler_failure("src/recovery.ts");
        let original = serde_json::to_value(&failure).expect("encode compiler failure");

        for (field, value) in [
            ("phase", serde_json::json!("write")),
            ("kind_tag", serde_json::json!("prepare_count")),
            ("relative_path", serde_json::json!("../recovery.ts")),
        ] {
            let mut malformed = original.clone();
            malformed[field] = value;
            assert!(
                serde_json::from_value::<PackageCompilerFailure>(malformed).is_err(),
                "accepted malformed `{field}`"
            );
        }

        let mut malformed_facts = original.clone();
        malformed_facts["cause"]["fault"]["facts"]["kind"] = serde_json::json!("tree_entity");
        assert!(serde_json::from_value::<PackageCompilerFailure>(malformed_facts).is_err());

        let mut mismatched_fragment = original.clone();
        mismatched_fragment["cause"]["fault"]["kind"]["kind"] = serde_json::json!("dangling");
        assert!(
            serde_json::from_value::<PackageCompilerFailure>(mismatched_fragment.clone()).is_err()
        );
        assert!(
            serde_json::from_value::<PackageCompilerFailureCause>(
                mismatched_fragment["cause"].clone()
            )
            .is_err()
        );

        let mut unknown = original;
        unknown["arbitrary_output"] = serde_json::json!("private marker");
        assert!(serde_json::from_value::<PackageCompilerFailure>(unknown).is_err());
    }

    #[test]
    fn package_compiler_failure_worst_case_escaped_path_fits_capture_bound() {
        // Quotes are legal filesystem path bytes and are worst-case JSON escapes
        // among the admitted characters (control bytes and backslashes are banned).
        let path = "\"".repeat(PackageCompilerFailure::MAX_RELATIVE_PATH_BYTES);
        let failure = compiler_failure(&path);
        let encoded = failure
            .encode_bounded_json()
            .expect("encode worst-case escaped path");
        assert!(encoded.len() <= PackageCompilerFailure::MAX_ENCODED_BYTES);
        assert_eq!(
            PackageCompilerFailure::decode_bounded_json(&encoded),
            Ok(failure)
        );
        let mixed_path = format!("{}{}", "\"".repeat(512), "雪".repeat(682));
        let mixed = compiler_failure(&mixed_path);
        let mixed_encoded = mixed
            .encode_bounded_json()
            .expect("mixed unicode and escaped path");
        assert!(mixed_encoded.len() <= PackageCompilerFailure::MAX_ENCODED_BYTES);
    }

    #[test]
    fn package_setup_failure_names_the_selected_tool_without_inventing_a_recipe() {
        use crate::interface::CompilerTerminal;
        use backend_semantic::vocabulary::{Language, NativeTool, Stage};

        let source = crate::interface::SourceAuthority {
            identity: ContentId::<SourceFactDomain>::from_canonical_bytes(b"typescript bytes"),
            byte_len: 16,
        };
        let terminal = CompilerTerminal::Toolchain {
            source,
            language: Language::TypeScript,
            stage: Stage::LowerIr,
            selected: NativeTool::TypeScriptCompiler,
            configured: None,
        };
        let failure = PackageCompilerFailure::from_package_terminal("src/index.ts", &terminal)
            .expect("valid package summary")
            .expect("toolchain terminal is projected");

        assert_eq!(failure.recipe_identity(), None);
        assert_eq!(failure.phase(), PackageCompilerFailurePhase::Setup);
        assert_eq!(failure.kind_tag(), "toolchain_configuration_mismatch");
        assert_eq!(
            failure.required_native_tool(),
            Some(CompilerNativeToolFact::TypeScriptCompiler)
        );
        assert_eq!(failure.configured_native_tool(), None);
        assert!(failure.requires_tool_configuration());
        assert!(failure.detail().contains("NUDOX_TSC"));
        assert!(failure.detail().contains("node_modules/.bin/tsc"));

        let encoded = failure.encode_bounded_json().expect("encode setup failure");
        let json: serde_json::Value = serde_json::from_slice(&encoded).expect("setup JSON");
        assert_eq!(json["cause"]["family"], "toolchain");
        assert_eq!(json["cause"]["fault"]["selected"], "type_script_compiler");
        assert_eq!(
            json["cause"]["fault"]["configured"],
            serde_json::Value::Null
        );
        assert_eq!(json["recipe_identity"], serde_json::Value::Null);
        assert!(PackageCompilerFailure::decode_bounded_json(&encoded).is_ok());

        let unavailable = CompilerTerminal::ToolingUnavailable {
            source,
            language: Language::Python,
            stage: Stage::Parse,
            tool: NativeTool::Python,
        };
        let unavailable = PackageCompilerFailure::from_package_terminal(
            "tests/testserver/__init__.py",
            &unavailable,
        )
        .expect("valid unavailable-tool summary")
        .expect("unavailable terminal is projected");
        assert_eq!(unavailable.recipe_identity(), None);
        assert_eq!(unavailable.kind_tag(), "tooling_unavailable");
        assert_eq!(
            unavailable.required_native_tool(),
            Some(CompilerNativeToolFact::Python)
        );
        assert!(unavailable.detail().contains("NUDOX_PYTHON"));
        assert!(unavailable.detail().contains(".venv/bin/python"));
    }

    #[test]
    fn package_required_python_checker_failure_is_typed_and_actionable() {
        use crate::interface::{
            CompilerTerminal, CompilerToolFailure, CompilerToolIssue, CompilerToolRequirement,
            SourceAuthority,
        };
        use backend_semantic::vocabulary::{Language, Stage};

        let source_bytes = b"from quart import Quart";
        let source = SourceAuthority {
            identity: ContentId::<SourceFactDomain>::from_canonical_bytes(source_bytes),
            byte_len: u32::try_from(source_bytes.len()).expect("source length"),
        };

        for (failure_kind, expected_tag, expected_failure) in [
            (
                CompilerToolFailure::Missing,
                "required_tool_missing",
                "missing",
            ),
            (
                CompilerToolFailure::ProbeFailed,
                "required_tool_probe_failed",
                "probe_failed",
            ),
        ] {
            let terminal = CompilerTerminal::RequiredTool {
                source,
                language: Language::Python,
                stage: Stage::LowerIr,
                issue: CompilerToolIssue {
                    requirement: CompilerToolRequirement::PythonChecker,
                    failure: failure_kind,
                },
            };
            let failure =
                PackageCompilerFailure::from_package_terminal("src/quart/__init__.py", &terminal)
                    .expect("valid setup summary")
                    .expect("required checker failure is projected");

            assert_eq!(failure.source_identity(), source.identity);
            assert_eq!(failure.source_byte_len(), source.byte_len);
            assert_eq!(failure.recipe_identity(), None);
            assert_eq!(failure.phase(), PackageCompilerFailurePhase::Setup);
            assert_eq!(failure.kind_tag(), expected_tag);
            assert_eq!(
                failure.required_tool_issue(),
                Some(CompilerToolIssue {
                    requirement: CompilerToolRequirement::PythonChecker,
                    failure: failure_kind,
                })
            );
            assert_eq!(
                failure.required_configuration_variable(),
                Some("NUDOX_PYREFLY")
            );
            assert!(failure.requires_tool_configuration());
            assert!(failure.detail().contains("NUDOX_PYREFLY"));

            let encoded = failure.encode_bounded_json().expect("bounded failure JSON");
            let json: serde_json::Value =
                serde_json::from_slice(&encoded).expect("typed failure object");
            assert_eq!(json["phase"], "setup");
            assert_eq!(json["kind_tag"], expected_tag);
            assert_eq!(json["cause"]["family"], "required_tool");
            assert_eq!(
                json["cause"]["fault"]["issue"]["requirement"],
                "python_checker"
            );
            assert_eq!(json["cause"]["fault"]["issue"]["failure"], expected_failure);
            assert_eq!(
                PackageCompilerFailure::decode_bounded_json(&encoded),
                Ok(failure.clone())
            );

            let outcome = IndexJobOutcome::RefusedWithCompilerFailure {
                detail: ProductText::new(failure.detail()).expect("bounded setup detail"),
                failure: failure.clone(),
            };
            let outcome_json = serde_json::to_value(&outcome).expect("full operation DTO");
            assert_eq!(outcome_json["state"], "refused-with-compiler-failure");
            assert_eq!(
                outcome_json["detail"]["failure"]["cause"]["fault"]["issue"]["requirement"],
                "python_checker"
            );
            let displayed = crate::CommandFailure::CompilerRefused {
                detail: failure.detail().to_owned(),
                failure,
            }
            .to_string();
            assert!(displayed.contains("NUDOX_PYREFLY"));
        }
    }

    #[test]
    fn package_lowering_failure_keeps_nested_source_recovery_reasons_distinct() {
        use crate::interface::{
            CompilerAttempt, CompilerCause, CompilerTerminal, LoweringCause, SourceAuthority,
        };
        use backend_semantic::vocabulary::{
            LoweringUnsupported, ProjectionAdmissionFault, ProjectionSemanticTypeFault,
            ProjectionSemanticTypeTag, ProjectionTypeCell,
        };

        let attempt = CompilerAttempt {
            source: SourceAuthority {
                identity: ContentId::<SourceFactDomain>::from_canonical_bytes(
                    b"typescript recovery source",
                ),
                byte_len: 27,
            },
            recipe: ContentId::<CompileRecipeDomain>::from_canonical_bytes(
                b"typescript recovery recipe",
            ),
        };
        let angular = CompilerTerminal::Compile {
            attempted: attempt,
            cause: CompilerCause::Lowering(LoweringCause::new(LoweringUnsupported::FactRejected {
                fact: 14,
                name_len: 8,
                cause: ProjectionAdmissionFault::TypeChild {
                    position: 2,
                    cause: ProjectionSemanticTypeFault::ChildNameRequired {
                        tag: ProjectionSemanticTypeTag::AnonymousRecord,
                        position: 2,
                    },
                },
            })),
        };
        let zod = CompilerTerminal::Compile {
            attempted: attempt,
            cause: CompilerCause::Lowering(LoweringCause::new(LoweringUnsupported::FactRejected {
                fact: 6,
                name_len: 11,
                cause: ProjectionAdmissionFault::TypeRecord {
                    cause: ProjectionSemanticTypeFault::MissingCell {
                        tag: ProjectionSemanticTypeTag::Mapped,
                        cell: ProjectionTypeCell::Text,
                    },
                },
            })),
        };
        let angular = PackageCompilerFailure::from_package_terminal("src/angular.ts", &angular)
            .expect("angular summary")
            .expect("angular fault projected");
        let zod = PackageCompilerFailure::from_package_terminal("src/zod.ts", &zod)
            .expect("zod summary")
            .expect("zod fault projected");
        assert_ne!(angular.kind_tag(), zod.kind_tag());
        assert_eq!(
            angular.kind_tag(),
            "lowering_projection_type_child_child_name_required"
        );
        assert_eq!(
            zod.kind_tag(),
            "lowering_projection_type_record_missing_cell"
        );
        assert_eq!(angular.phase(), PackageCompilerFailurePhase::Lowering);

        let angular_json: serde_json::Value = serde_json::from_slice(
            &angular
                .encode_bounded_json()
                .expect("encode angular refusal"),
        )
        .expect("angular JSON");
        assert_eq!(
            angular_json["cause"]["fault"]["cause"]["fault"],
            "type_child"
        );
        assert_eq!(angular_json["cause"]["fault"]["cause"]["position"], 2);
        assert_eq!(
            angular_json["cause"]["fault"]["cause"]["cause"]["fault"],
            "child_name_required"
        );
        assert_eq!(
            angular_json["cause"]["fault"]["cause"]["cause"]["tag"],
            "anonymous_record"
        );
        assert_eq!(
            angular_json["cause"]["fault"]["cause"]["cause"]["position"],
            2
        );

        let zod_json: serde_json::Value =
            serde_json::from_slice(&zod.encode_bounded_json().expect("encode zod refusal"))
                .expect("zod JSON");
        assert_eq!(zod_json["cause"]["fault"]["cause"]["fault"], "type_record");
        assert_eq!(
            zod_json["cause"]["fault"]["cause"]["cause"]["fault"],
            "missing_cell"
        );
        assert_eq!(
            zod_json["cause"]["fault"]["cause"]["cause"]["tag"],
            "mapped"
        );
        assert_eq!(zod_json["cause"]["fault"]["cause"]["cause"]["cell"], "text");
    }

    #[test]
    fn package_authority_summary_never_copies_native_output() {
        use crate::interface::{
            AuthorityDiagnosticClass, AuthorityPhase, CompilerAttempt, CompilerCause,
            CompilerDiagnostic, CompilerTerminal, SourceAuthority,
        };

        let attempt = CompilerAttempt {
            source: SourceAuthority {
                identity: ContentId::<SourceFactDomain>::from_canonical_bytes(b"authority source"),
                byte_len: 16,
            },
            recipe: ContentId::<CompileRecipeDomain>::from_canonical_bytes(b"authority recipe"),
        };
        let diagnostic = CompilerDiagnostic::from_native(b"PRIVATE-RAW-COMPILER-OUTPUT", 27, false)
            .expect("retained diagnostic");
        let terminal = CompilerTerminal::Compile {
            attempted: attempt,
            cause: CompilerCause::Authority {
                phase: AuthorityPhase::TypeCheck,
                class: AuthorityDiagnosticClass::Type,
                diagnostic: Some(diagnostic),
            },
        };
        let failure = PackageCompilerFailure::from_package_terminal("src/authority.ts", &terminal)
            .expect("authority summary")
            .expect("authority fault projected");
        let encoded = failure
            .encode_bounded_json()
            .expect("bounded authority JSON");
        assert!(!String::from_utf8_lossy(&encoded).contains("PRIVATE-RAW-COMPILER-OUTPUT"));
        assert!(failure.detail().contains("type check"));
        assert!(failure.detail().contains("type"));
        let json: serde_json::Value = serde_json::from_slice(&encoded).expect("authority JSON");
        assert_eq!(json["cause"]["fault"]["phase"], "type_check");
        assert_eq!(json["cause"]["fault"]["class"], "type");
        assert_eq!(json["cause"]["fault"]["diagnostic"]["observed_bytes"], 27);
        assert!(json["cause"]["fault"]["diagnostic"].get("bytes").is_none());
        assert_eq!(
            failure.retained_diagnostic_for_local_debug(),
            Some(b"PRIVATE-RAW-COMPILER-OUTPUT".as_slice())
        );
        assert!(
            PackageCompilerFailure::decode_bounded_json(&encoded)
                .expect("wire refusal decodes")
                .retained_diagnostic_for_local_debug()
                .is_none()
        );
    }

    #[test]
    fn python_authority_failure_retains_closed_cause_and_local_only_diagnostic() {
        use crate::interface::{
            AuthorityDiagnosticClass, AuthorityPhase, CompilerAttempt, CompilerCause,
            CompilerDiagnostic, CompilerTerminal, PythonAuthorityFailureKind, SourceAuthority,
        };

        let attempt = CompilerAttempt {
            source: SourceAuthority {
                identity: ContentId::<SourceFactDomain>::from_canonical_bytes(
                    b"python package module.py",
                ),
                byte_len: 29,
            },
            recipe: ContentId::<CompileRecipeDomain>::from_canonical_bytes(
                b"python requests checker recipe",
            ),
        };
        let native_primary = b"private checker identity details: digest=secret";
        let diagnostic =
            CompilerDiagnostic::from_native(native_primary, native_primary.len(), false)
                .expect("bounded primary diagnostic")
                .with_python_failure(PythonAuthorityFailureKind::ProducerIdentityMismatch);
        let terminal = CompilerTerminal::Compile {
            attempted: attempt,
            cause: CompilerCause::Authority {
                phase: AuthorityPhase::TypeCheck,
                class: AuthorityDiagnosticClass::Type,
                diagnostic: Some(diagnostic),
            },
        };
        let failure =
            PackageCompilerFailure::from_package_terminal("src/package/module.py", &terminal)
                .expect("Python failure projection")
                .expect("authority terminal projects");

        assert_eq!(failure.kind_tag(), "python_producer_identity_mismatch");
        assert!(
            failure
                .detail()
                .contains("did not match the admitted native producer")
        );
        assert_eq!(
            failure.retained_diagnostic_for_local_debug(),
            Some(native_primary.as_slice())
        );

        let encoded = failure
            .encode_bounded_json()
            .expect("bounded typed refusal");
        assert!(!format!("{failure:?}").contains("digest=secret"));
        assert!(!String::from_utf8_lossy(&encoded).contains("digest=secret"));
        let json: serde_json::Value = serde_json::from_slice(&encoded).expect("failure JSON");
        assert_eq!(json["kind_tag"], "python_producer_identity_mismatch");
        assert_eq!(
            json["cause"]["fault"]["diagnostic"]["python_failure"],
            "producer_identity_mismatch"
        );
        assert_eq!(json["relative_path"], "src/package/module.py");
        assert!(json["cause"]["fault"]["diagnostic"].get("bytes").is_none());

        let reopened =
            PackageCompilerFailure::decode_bounded_json(&encoded).expect("typed failure reopens");
        assert_eq!(reopened, failure);
        assert_eq!(reopened.kind_tag(), failure.kind_tag());
        assert!(reopened.retained_diagnostic_for_local_debug().is_none());
    }

    #[test]
    fn typescript_authority_failure_preserves_exact_cause_without_native_text() {
        use crate::interface::{
            AuthorityDiagnosticClass, AuthorityPhase, CompilerAttempt, CompilerCause,
            CompilerDiagnostic, CompilerTerminal, SourceAuthority,
            TypeScriptAuthorityFailureKind as F,
        };
        for kind in [
            F::HostCompilerApiBridge,
            F::HostCompilerIoClosureMismatch,
            F::CheckerPackageSourceMissing,
            F::CheckerExit,
            F::NativeProjectCheckDeadline,
            F::NativeProjectCheckWorkBudgetExhausted,
        ] {
            let private = b"private absolute path /home/user/app/node_modules; native stderr";
            let terminal = CompilerTerminal::Compile {
                attempted: CompilerAttempt {
                    source: SourceAuthority {
                        identity: ContentId::<SourceFactDomain>::from_canonical_bytes(
                            b"export const x = 1",
                        ),
                        byte_len: 18,
                    },
                    recipe: ContentId::<CompileRecipeDomain>::from_canonical_bytes(
                        b"actual-typed-ts-cause-control",
                    ),
                },
                cause: CompilerCause::Authority {
                    phase: AuthorityPhase::TypeCheck,
                    class: AuthorityDiagnosticClass::Authority,
                    diagnostic: CompilerDiagnostic::from_native(private, private.len(), false)
                        .map(|value| value.with_typescript_failure(kind)),
                },
            };
            let failure =
                PackageCompilerFailure::from_package_terminal("eslint.config.mjs", &terminal)
                    .expect("closed projection")
                    .expect("authority refusal");
            assert_eq!(failure.kind_tag(), kind.kind_tag());
            assert_eq!(failure.detail(), kind.detail());
            assert!(!failure.cause().requires_tool_configuration());
            assert_eq!(
                failure.retained_diagnostic_for_local_debug(),
                Some(private.as_slice())
            );
            let encoded = failure
                .encode_bounded_json()
                .expect("bounded typed public refusal");
            assert!(!String::from_utf8_lossy(&encoded).contains("/home/user"));
            assert!(!String::from_utf8_lossy(&encoded).contains("native stderr"));
            let reopened = PackageCompilerFailure::decode_bounded_json(&encoded)
                .expect("strict cold wire refusal");
            assert_eq!(reopened, failure);
            assert!(reopened.retained_diagnostic_for_local_debug().is_none());
            let mut json: serde_json::Value =
                serde_json::from_slice(&encoded).expect("control JSON");
            json["cause"]["fault"]["diagnostic"]["python_failure"] =
                serde_json::json!("project_panic");
            assert!(
                PackageCompilerFailure::decode_bounded_json(
                    &serde_json::to_vec(&json).expect("mutant")
                )
                .is_err(),
                "contradictory language markers refused"
            );
            json["cause"]["fault"]["diagnostic"]["python_failure"] = serde_json::Value::Null;
            json["cause"]["fault"]["diagnostic"]["typescript_failure"] =
                serde_json::json!("invented_cause");
            assert!(
                PackageCompilerFailure::decode_bounded_json(
                    &serde_json::to_vec(&json).expect("mutant")
                )
                .is_err(),
                "unknown closed TS causes refused"
            );
        }
    }

    #[test]
    fn package_reference_kind_preserves_explicit_local_identity() {
        let text = "pkg:cargo/widget@1.0.0";
        let purl = PackageReference::from_kind(PackageReferenceKind::Purl, text)
            .expect("explicit purl kind");
        let local = PackageReference::from_kind(PackageReferenceKind::Local, text)
            .expect("explicit local kind may share the display spelling");

        assert_eq!(
            PackageReferenceKind::try_from(1),
            Ok(PackageReferenceKind::Purl)
        );
        assert_eq!(
            PackageReferenceKind::try_from(2),
            Ok(PackageReferenceKind::Local)
        );
        assert_eq!(
            PackageReferenceKind::try_from(3),
            Err(ProductAdmissionError::PackageReference)
        );
        assert_eq!(PackageReferenceKind::Purl.tag(), 1);
        assert_eq!(PackageReferenceKind::Local.tag(), 2);
        assert_eq!(
            PackageReference::from_kind(PackageReferenceKind::Purl, "local label"),
            Err(ProductAdmissionError::PackageReference)
        );
        assert_eq!(purl, PackageReference::parse(text).expect("parsed purl"));
        assert_eq!(local.as_str(), purl.as_str());
        assert_eq!(local.kind(), PackageReferenceKind::Local);
        assert_eq!(purl.kind(), PackageReferenceKind::Purl);
        assert_eq!(purl.canonical_cmp(&local), std::cmp::Ordering::Less);
        assert_eq!(
            serde_json::to_string(&local).expect("tagged local encoding"),
            r#"{"kind":"local","value":"pkg:cargo/widget@1.0.0"}"#
        );
        assert_eq!(
            serde_json::from_str::<PackageReference>(
                r#"{"kind":"local","value":"pkg:cargo/widget@1.0.0"}"#
            )
            .expect("tagged local decoding"),
            local
        );
    }

    #[test]
    fn selected_source_frontier_is_bound_to_the_selected_local_package() {
        let package =
            PackageReference::parse("/workspace/large-project").expect("local project package");
        let coordinate = PackageCoordinate::parse("pkg:cargo/large-project@1.0.0")
            .expect("semantic package coordinate");
        let frontier = SelectedProjectSourceFrontier {
            package: package.clone(),
            source_relation_root: [0x21; 32],
            source_version: [0x22; 32],
            file_count: 2_916,
        };
        let record = SemanticVersionRecord {
            package: package.clone(),
            coordinate,
            profile: SemanticLanguageProfile::from_name("rust").expect("Rust profile"),
            generation: SemanticGenerationId::new([0x23; 32]),
            generation_root: [0x24; 32],
            dependency_set: [0x25; 32],
            manifest: [0x26; 32],
            artifacts: 1,
            semantic_bytes: 1,
            complete: true,
            selected: true,
            freshness: SemanticVersionFreshness::Current {
                input_digest: [0x27; 32],
            },
            history_status: SemanticHistoryPublicationStatus::NotSelected,
            selected_source_frontier: Some(frontier.clone()),
        };
        record
            .admit()
            .expect("frontier belongs to selected local row");
        let mut empty_frontier = record.clone();
        if let Some(frontier) = &mut empty_frontier.selected_source_frontier {
            frontier.file_count = 0;
        }
        empty_frontier
            .admit()
            .expect("an empty Project membership is a valid observed state");

        let mut oversized_frontier = record.clone();
        if let Some(frontier) = &mut oversized_frontier.selected_source_frontier {
            frontier.file_count = u32::try_from(MAX_SELECTED_PROJECT_FRONTIER_FILES + 1)
                .expect("frontier limit fits the wire count");
        }
        assert_eq!(
            oversized_frontier.admit(),
            Err(ProductAdmissionError::SemanticVersionShape)
        );
        let encoded = serde_json::to_vec(&record).expect("frontier JSON");
        let decoded: SemanticVersionRecord =
            serde_json::from_slice(&encoded).expect("frontier JSON decode");
        assert_eq!(decoded, record);

        let mut older = serde_json::to_value(&record).expect("semantic record JSON");
        older
            .as_object_mut()
            .expect("record JSON object")
            .remove("selected_source_frontier");
        let older: SemanticVersionRecord =
            serde_json::from_value(older).expect("older semantic record decodes");
        assert_eq!(older.selected_source_frontier, None);

        let mut wrong_package = record.clone();
        wrong_package.selected_source_frontier = Some(SelectedProjectSourceFrontier {
            package: PackageReference::parse("/workspace/other-project")
                .expect("other local project"),
            ..frontier.clone()
        });
        assert_eq!(
            wrong_package.admit(),
            Err(ProductAdmissionError::SemanticVersionShape)
        );

        let mut unselected = record.clone();
        unselected.selected = false;
        assert_eq!(
            unselected.admit(),
            Err(ProductAdmissionError::SemanticVersionShape)
        );

        let mut registry_package = record;
        registry_package.package = PackageReference::Purl(
            PackageCoordinate::parse("pkg:cargo/large-project@1.0.0").expect("registry package"),
        );
        assert_eq!(
            registry_package.admit(),
            Err(ProductAdmissionError::SemanticVersionShape)
        );
    }

    #[test]
    fn semantic_versions_admit_one_selection_per_coordinate_and_profile() {
        let package = PackageReference::parse("/workspace/multi-language").expect("local project");
        let frontier = SelectedProjectSourceFrontier {
            package: package.clone(),
            source_relation_root: [0x31; 32],
            source_version: [0x32; 32],
            file_count: 3_000,
        };
        let record = |coordinate: &str, profile: &str, generation: u8| SemanticVersionRecord {
            package: package.clone(),
            coordinate: PackageCoordinate::parse(coordinate).expect("semantic coordinate"),
            profile: SemanticLanguageProfile::from_name(profile).expect("profile"),
            generation: SemanticGenerationId::new([generation; 32]),
            generation_root: [generation.wrapping_add(1); 32],
            dependency_set: [generation.wrapping_add(2); 32],
            manifest: [generation.wrapping_add(3); 32],
            artifacts: 1,
            semantic_bytes: 1,
            complete: true,
            selected: true,
            freshness: SemanticVersionFreshness::Current {
                input_digest: [generation.wrapping_add(4); 32],
            },
            history_status: SemanticHistoryPublicationStatus::NotSelected,
            selected_source_frontier: Some(frontier.clone()),
        };

        let rust = record("pkg:cargo/workspace@1.0.0", "rust", 0x40);
        let csharp = record("pkg:nuget/workspace@1.0.0", "csharp", 0x50);
        let alias = record("pkg:cargo/workspace-helper@1.0.0", "rust", 0x60);
        assert_eq!(
            SurfaceReply::SemanticVersions(vec![rust.clone(), csharp, alias].into_boxed_slice())
                .admit(CommandId::SemanticVersions),
            Ok(()),
            "the response contains two profiles and a distinct supported package coordinate"
        );

        let mut retained_history = rust.clone();
        retained_history.selected = false;
        retained_history.selected_source_frontier = None;
        retained_history.generation = SemanticGenerationId::new([0x61; 32]);
        retained_history.freshness = SemanticVersionFreshness::Historical {
            selected_input: [0x62; 32],
            latest_input: [0x63; 32],
        };
        assert_eq!(
            SurfaceReply::SemanticVersions(vec![rust.clone(), retained_history].into_boxed_slice())
                .admit(CommandId::SemanticVersions),
            Ok(()),
            "a retained unselected generation may share its exact target with the selection"
        );

        let duplicate_target = record("pkg:cargo/workspace@1.0.0", "rust", 0x70);
        assert_eq!(
            SurfaceReply::SemanticVersions(vec![rust.clone(), duplicate_target].into_boxed_slice())
                .admit(CommandId::SemanticVersions),
            Err(ProductAdmissionError::SemanticVersionShape),
            "one coordinate/profile target cannot select two generations"
        );

        let missing_frontier = record("pkg:nuget/workspace@1.0.0", "csharp", 0x80);
        let mut missing_frontier = missing_frontier;
        missing_frontier.selected_source_frontier = None;
        assert_eq!(
            SurfaceReply::SemanticVersions(vec![rust.clone(), missing_frontier].into_boxed_slice())
                .admit(CommandId::SemanticVersions),
            Err(ProductAdmissionError::SemanticVersionShape),
            "all selected profiles in one query must share evidence presence and identity"
        );

        let mut other_frontier = frontier.clone();
        other_frontier.source_relation_root = [0x99; 32];
        let mut conflicting = record("pkg:nuget/workspace@1.0.0", "csharp", 0x90);
        conflicting.selected_source_frontier = Some(other_frontier);
        assert_eq!(
            SurfaceReply::SemanticVersions(vec![rust, conflicting].into_boxed_slice())
                .admit(CommandId::SemanticVersions),
            Err(ProductAdmissionError::SemanticVersionShape),
            "selected profiles cannot claim different owner snapshots"
        );
    }

    fn operation_receipt_view() -> crate::ViewRoot {
        let root = crate::view_state_root(&[]);
        let basis = crate::Basis::new(root, crate::object_version(b"index-operation-source"));
        let frontier = crate::canonical::Frontier::new(
            crate::branch_key("main"),
            crate::log_key("library"),
            crate::cursor::CURSOR_SCHEMA,
            root,
            0,
        );
        crate::ViewRoot::new_incomplete(
            crate::view_key(b"index-operation-test-view"),
            basis,
            frontier,
            Vec::new(),
            Vec::new(),
        )
        .expect("incomplete test view")
    }

    #[test]
    fn partial_registry_metadata_survives_wire_and_admission_with_zero_observed_rows() {
        let reply = SurfaceReply::Dependents(RegistryMetadata::Partial {
            value: Box::new([]),
            reason: ProductText::from_static("source authority for name-only edges is unresolved"),
        });
        reply
            .admit(CommandId::Dependents)
            .expect("partial reply admission");
        let encoded = serde_json::to_vec(&reply).expect("wire");
        let decoded: SurfaceReply = serde_json::from_slice(&encoded).expect("partial wire decode");
        assert_eq!(decoded, reply);
        assert!(reply.encoded_size_bound() >= encoded.len());
    }

    #[test]
    fn registry_advisory_alias_coverage_survives_wire_and_admission_bounds() {
        let aliases_states: [RegistryEvidenceFacet<Box<[ProductText]>>; 3] = [
            RegistryEvidenceFacet::Known(Vec::new().into_boxed_slice()),
            RegistryEvidenceFacet::Absent,
            RegistryEvidenceFacet::Unknown,
        ];
        for aliases in aliases_states {
            let metadata = RegistryDiscoveryMetadata {
                advisories: RegistryEvidenceFacet::Known(
                    vec![RegistryDiscoveryAdvisory {
                        id: ProductText::new("CVE-2026-1").expect("advisory id"),
                        aliases,
                        summary: RegistryEvidenceFacet::Unknown,
                        severity: RegistryEvidenceFacet::Absent,
                        fixed_in: RegistryEvidenceFacet::Unknown,
                    }]
                    .into_boxed_slice(),
                ),
                ..RegistryDiscoveryMetadata::default()
            };
            admit_registry_discovery_metadata(&metadata).expect("bounded advisory facets");
            let encoded = serde_json::to_vec(&metadata).expect("metadata encoding");
            let decoded: RegistryDiscoveryMetadata =
                serde_json::from_slice(&encoded).expect("metadata decoding");
            assert_eq!(decoded, metadata);
        }

        let aliases = (0..=32)
            .map(|index| ProductText::new(format!("CVE-ALIAS-{index}")))
            .collect::<Result<Vec<_>, _>>()
            .expect("bounded fixture aliases")
            .into_boxed_slice();
        let oversized = RegistryDiscoveryMetadata {
            advisories: RegistryEvidenceFacet::Known(
                vec![RegistryDiscoveryAdvisory {
                    id: ProductText::new("CVE-2026-2").expect("advisory id"),
                    aliases: RegistryEvidenceFacet::Known(aliases),
                    summary: RegistryEvidenceFacet::Unknown,
                    severity: RegistryEvidenceFacet::Unknown,
                    fixed_in: RegistryEvidenceFacet::Unknown,
                }]
                .into_boxed_slice(),
            ),
            ..RegistryDiscoveryMetadata::default()
        };
        assert_eq!(
            admit_registry_discovery_metadata(&oversized),
            Err(ProductAdmissionError::RegistryDiscoveryMetadata)
        );

        let oversized_text = RegistryDiscoveryMetadata {
            advisories: RegistryEvidenceFacet::Known(
                vec![RegistryDiscoveryAdvisory {
                    id: ProductText::new("CVE-2026-3").expect("advisory id"),
                    aliases: RegistryEvidenceFacet::Known(
                        vec![ProductText("x".repeat(MAX_PRODUCT_TEXT_BYTES + 1))]
                            .into_boxed_slice(),
                    ),
                    summary: RegistryEvidenceFacet::Unknown,
                    severity: RegistryEvidenceFacet::Unknown,
                    fixed_in: RegistryEvidenceFacet::Unknown,
                }]
                .into_boxed_slice(),
            ),
            ..RegistryDiscoveryMetadata::default()
        };
        assert_eq!(
            admit_registry_discovery_metadata(&oversized_text),
            Err(ProductAdmissionError::RegistryDiscoveryMetadata)
        );
    }

    #[test]
    fn durable_index_operation_key_and_receipt_use_checked_hex_identities() {
        let key = IndexOperationKey::from_bytes([0x0a; 32]).expect("operation key");
        let key_hex = "0a".repeat(32);
        assert_eq!(key.to_hex(), key_hex);
        assert_eq!(IndexOperationKey::parse_hex(&key_hex), Ok(key));
        assert!(IndexOperationKey::parse_hex(&key_hex.to_uppercase()).is_err());
        assert!(IndexOperationKey::from_bytes([0; 32]).is_err());
        assert_eq!(
            serde_json::to_string(&key).expect("key json"),
            format!("\"{key_hex}\"")
        );
        assert!(
            serde_json::from_str::<IndexOperationKey>(&format!("\"{}\"", "00".repeat(32))).is_err()
        );

        let view = operation_receipt_view();
        let cursor = crate::Cursor::for_view_root(&view);
        let receipt = IndexOperationPublicationReceipt::from_published_view(
            Some([1; 32]),
            [2; 32],
            [3; 32],
            1,
            &view,
            cursor,
        )
        .expect("checked publication receipt");
        let genesis_no_op = IndexOperationPublicationReceipt::from_published_view(
            None, [2; 32], [3; 32], 0, &view, cursor,
        )
        .expect("genesis no-op still names a valid selected revision");
        assert!(genesis_no_op.request_identity().is_none());
        let status = IndexOperationStatus {
            operation_key: key,
            request_digest: index_operation_request_digest(
                &PackageReference::parse("/workspace/demo").expect("package"),
                crate::CompileExecutionIntent::Interactive,
            ),
            package: PackageReference::parse("/workspace/demo").expect("package"),
            execution_intent: crate::CompileExecutionIntent::Interactive,
            source_capture: None,
            state: IndexOperationState::Published(receipt),
        };
        let observation = IndexOperationObservation::Known(status);
        observation.admit().expect("operation status admission");
        let encoded = serde_json::to_vec(&observation).expect("operation status json");
        let decoded: IndexOperationObservation =
            serde_json::from_slice(&encoded).expect("operation status decode");
        assert_eq!(decoded, observation);
        assert!(
            encoded
                .windows(key_hex.len())
                .any(|window| window == key_hex.as_bytes())
        );
        let outside_window = IndexOperationObservation::OutsideReceiptWindow {
            operation_key: key,
            request_digest: index_operation_request_digest(
                &PackageReference::parse("/workspace/demo").expect("package"),
                crate::CompileExecutionIntent::Interactive,
            ),
        };
        let outside_reply = SurfaceReply::IndexOperationStatus(outside_window.clone());
        outside_reply
            .admit(CommandId::IndexProgress)
            .expect("archived operation remains a consumed-key receipt");
        assert_eq!(
            serde_json::from_slice::<IndexOperationObservation>(
                &serde_json::to_vec(&outside_window).expect("outside-window json")
            )
            .expect("outside-window decode"),
            outside_window
        );

        let command = SurfaceCommand::IndexOperationStart {
            operation_key: key,
            package: PackageReference::parse("/workspace/demo").expect("package"),
            execution_intent: crate::CompileExecutionIntent::Interactive,
        };
        assert_eq!(command.id(), CommandId::IndexStart);
        let encoded = serde_json::to_vec(&command).expect("operation start json");
        let decoded: SurfaceCommand =
            serde_json::from_slice(&encoded).expect("operation start decode");
        assert_eq!(decoded, command);
        let lookup = SurfaceCommand::IndexOperationStatus { operation_key: key };
        assert_eq!(lookup.id(), CommandId::IndexProgress);
        assert_eq!(
            serde_json::from_slice::<SurfaceCommand>(
                &serde_json::to_vec(&lookup).expect("operation status request")
            )
            .expect("operation status request decode"),
            lookup
        );
    }

    #[test]
    fn partial_operation_requires_exact_terminal_profile_partition_and_receipt() {
        let key = IndexOperationKey::from_bytes([0x61; 32]).expect("operation key");
        let mut profiles = vec![
            IndexOperationSourceProfile {
                profile: SemanticLanguageProfile::from_name("python").expect("Python profile"),
                source_version: [5; 32],
                input_digest: [6; 32],
                observation_sequence: 7,
                source_count: 2,
                state: IndexOperationSemanticProfileState::Published {
                    generation: [8; 32],
                    coverage: IndexOperationSemanticCoverage::Complete,
                },
            },
            IndexOperationSourceProfile {
                profile: SemanticLanguageProfile::from_name("typescript")
                    .expect("TypeScript profile"),
                source_version: [5; 32],
                input_digest: [9; 32],
                observation_sequence: 10,
                source_count: 3,
                state: IndexOperationSemanticProfileState::Unavailable {
                    reason: IndexOperationSemanticUnavailableReason::Rejected,
                },
            },
        ];
        profiles.sort_by_key(|profile| profile.profile);
        let refused_profile = profiles
            .iter()
            .find(|profile| {
                matches!(
                    profile.state,
                    IndexOperationSemanticProfileState::Unavailable { .. }
                )
            })
            .expect("refused profile")
            .profile;
        let capture = IndexOperationSourceCaptureReceipt::from_checked_parts(
            key,
            [2; 32],
            [3; 32],
            8,
            profiles.into_boxed_slice(),
        )
        .expect("source receipt");
        let view = operation_receipt_view();
        let receipt = IndexOperationPublicationReceipt::from_published_view(
            Some([1; 32]),
            [11; 32],
            [12; 32],
            9,
            &view,
            crate::Cursor::for_view_root(&view),
        )
        .expect("publication receipt");
        let status = IndexOperationStatus::new(
            key,
            PackageReference::parse("/workspace/demo").expect("package"),
            crate::CompileExecutionIntent::Interactive,
            IndexOperationState::PartiallyPublished {
                receipt,
                refused_profiles: vec![IndexOperationProfileRefusal {
                    profile: refused_profile,
                    reason: IndexOperationSemanticUnavailableReason::Rejected,
                    compiler_failure: Some(compiler_failure("src/recovery.ts")),
                }]
                .into_boxed_slice(),
            },
        )
        .with_source_capture(Some(capture));
        let admit = |status| {
            SurfaceReply::IndexOperationStatus(IndexOperationObservation::Known(status))
                .admit(CommandId::IndexProgress)
        };
        assert_eq!(admit(status.clone()), Ok(()));
        for (language, stage, expected) in [
            (
                backend_semantic::vocabulary::Language::TypeScript,
                backend_semantic::vocabulary::Stage::LowerIr,
                Ok(()),
            ),
            (
                backend_semantic::vocabulary::Language::Python,
                backend_semantic::vocabulary::Stage::LowerIr,
                Err(ProductAdmissionError::IndexOperationShape),
            ),
            (
                backend_semantic::vocabulary::Language::TypeScript,
                backend_semantic::vocabulary::Stage::Parse,
                Err(ProductAdmissionError::IndexOperationShape),
            ),
        ] {
            let terminal = crate::interface::CompilerTerminal::ToolingUnavailable {
                source: crate::interface::SourceAuthority {
                    identity: ContentId::<SourceFactDomain>::from_canonical_bytes(b"source bytes"),
                    byte_len: 12,
                },
                language,
                stage,
                tool: if language == backend_semantic::vocabulary::Language::Python {
                    backend_semantic::vocabulary::NativeTool::Python
                } else {
                    backend_semantic::vocabulary::NativeTool::TypeScriptCompiler
                },
            };
            let mut setup = status.clone();
            let IndexOperationState::PartiallyPublished {
                refused_profiles, ..
            } = &mut setup.state
            else {
                unreachable!()
            };
            refused_profiles[0].compiler_failure =
                PackageCompilerFailure::from_package_terminal("src/recovery.ts", &terminal)
                    .expect("bounded typed setup failure");
            assert_eq!(admit(setup), expected);
        }
        let mut cancelled_partition = status.clone();
        let mut cancelled_profiles = cancelled_partition
            .source_capture
            .as_ref()
            .expect("capture")
            .profiles()
            .to_vec();
        cancelled_profiles
            .iter_mut()
            .find(|profile| profile.profile == refused_profile)
            .expect("refused")
            .state = IndexOperationSemanticProfileState::Unavailable {
            reason: IndexOperationSemanticUnavailableReason::Cancelled,
        };
        cancelled_partition.source_capture = Some(
            IndexOperationSourceCaptureReceipt::from_checked_parts(
                key,
                [2; 32],
                [3; 32],
                8,
                cancelled_profiles.into_boxed_slice(),
            )
            .expect("typed cancelled capture"),
        );
        let IndexOperationState::PartiallyPublished {
            refused_profiles, ..
        } = &mut cancelled_partition.state
        else {
            unreachable!()
        };
        refused_profiles[0].reason = IndexOperationSemanticUnavailableReason::Cancelled;
        refused_profiles[0].compiler_failure = None;
        assert_eq!(
            admit(cancelled_partition),
            Err(ProductAdmissionError::IndexOperationShape),
            "caller cancellation cannot be represented as partial publication"
        );
        let encoded = serde_json::to_vec(&IndexOperationObservation::Known(status.clone()))
            .expect("partial wire");
        assert_eq!(
            serde_json::from_slice::<IndexOperationObservation>(&encoded).expect("partial decode"),
            IndexOperationObservation::Known(status.clone())
        );
        assert_eq!(
            crate::DTO_VERSION,
            23,
            "partial publication uses the closed wire-23 cohort"
        );
        let command = crate::CommandDto::new(
            61,
            crate::Command::Surface(SurfaceCommand::IndexOperationStatus { operation_key: key }),
        );
        let reply = crate::ReplyDto::new(
            61,
            crate::CommandReply::Surface(SurfaceReply::IndexOperationStatus(
                IndexOperationObservation::Known(status.clone()),
            )),
        );
        let encoded_reply = serde_json::to_vec(&reply).expect("partial reply envelope");
        let decoded_reply =
            crate::decode_reply_body(&encoded_reply).expect("wire-23 partial reply");
        crate::admit_reply(&command, &decoded_reply).expect("admit exact partial status route");
        assert_eq!(decoded_reply, reply);
        let mut old_peer: serde_json::Value =
            serde_json::from_slice(&encoded_reply).expect("encoded reply fields");
        old_peer["version"] = serde_json::json!(22);
        let old_bytes = serde_json::to_vec(&old_peer).expect("old peer version header");
        let error = crate::decode_reply_body(&old_bytes).expect_err("wire-22 partial peer refused");
        assert!(error.contains("reply DTO version 22"));
        assert!(error.contains("this build supports 23"));
        assert!(error.contains("same build"));
        let mut missing = status.clone();
        missing.source_capture = None;
        assert_eq!(
            admit(missing),
            Err(ProductAdmissionError::IndexOperationShape)
        );
        for state in [
            IndexOperationSemanticProfileState::Pending { prior: None },
            IndexOperationSemanticProfileState::Published {
                generation: [8; 32],
                coverage: IndexOperationSemanticCoverage::Complete,
            },
        ] {
            let mut malformed = status.clone();
            malformed
                .source_capture
                .as_mut()
                .expect("capture")
                .profiles
                .iter_mut()
                .find(|profile| profile.profile == refused_profile)
                .expect("refusal")
                .state = state;
            assert_eq!(
                admit(malformed),
                Err(ProductAdmissionError::IndexOperationShape)
            );
        }
        let mut mismatch = status.clone();
        let IndexOperationState::PartiallyPublished {
            refused_profiles, ..
        } = &mut mismatch.state
        else {
            unreachable!()
        };
        refused_profiles[0].reason = IndexOperationSemanticUnavailableReason::Toolchain;
        assert_eq!(
            admit(mismatch),
            Err(ProductAdmissionError::IndexOperationShape)
        );
        let mut duplicated = status.clone();
        let IndexOperationState::PartiallyPublished {
            refused_profiles, ..
        } = &mut duplicated.state
        else {
            unreachable!()
        };
        *refused_profiles =
            vec![refused_profiles[0].clone(), refused_profiles[0].clone()].into_boxed_slice();
        assert_eq!(
            admit(duplicated),
            Err(ProductAdmissionError::IndexOperationShape)
        );
    }

    #[test]
    fn durable_compiler_refusal_requires_closed_refusal_state_on_wire() {
        let key = IndexOperationKey::from_bytes([0x47; 32]).expect("operation key");
        let failure = compiler_failure("src/recovery.ts");
        let observation = |reason| {
            IndexOperationObservation::Known(IndexOperationStatus::new(
                key,
                PackageReference::parse("/workspace/demo").expect("package"),
                crate::CompileExecutionIntent::Interactive,
                IndexOperationState::Failed {
                    reason,
                    detail: ProductText::from_static("short human explanation"),
                    compiler_failure: Some(failure.clone()),
                },
            ))
        };
        let command = crate::CommandDto::new(
            53,
            crate::Command::Surface(SurfaceCommand::IndexOperationStatus { operation_key: key }),
        );
        let reply = crate::ReplyDto::new(
            53,
            crate::CommandReply::Surface(SurfaceReply::IndexOperationStatus(observation(
                IndexOperationFailureReason::Refused,
            ))),
        );
        let encoded = serde_json::to_vec(&reply).expect("bounded typed status wire");
        let decoded = crate::decode_reply_body(&encoded).expect("decode typed status");
        crate::admit_reply(&command, &decoded).expect("admit typed refusal route");
        assert_eq!(decoded, reply);
        for reason in [
            IndexOperationFailureReason::WorkerFailed,
            IndexOperationFailureReason::Cancelled,
        ] {
            let reply = SurfaceReply::IndexOperationStatus(observation(reason));
            assert_eq!(
                reply.admit(CommandId::IndexProgress),
                Err(ProductAdmissionError::IndexOperationShape)
            );
        }
    }

    #[test]
    fn operation_receipt_rejects_unbound_root_or_unbounded_cursor() {
        let view = operation_receipt_view();
        let cursor = crate::Cursor::for_view_root(&view);
        assert!(
            IndexOperationPublicationReceipt::from_published_view(
                Some([1; 32]),
                [2; 32],
                [3; 32],
                1,
                &view,
                crate::Cursor::new(),
            )
            .is_err()
        );
        assert!(
            IndexOperationPublicationReceipt::from_published_view(
                Some([0; 32]),
                [2; 32],
                [3; 32],
                1,
                &view,
                cursor,
            )
            .is_err()
        );
    }

    #[test]
    fn operation_status_does_not_treat_source_capture_as_semantic_publication() {
        let key = IndexOperationKey::from_bytes([0x19; 32]).expect("operation key");
        let package = PackageReference::parse("/workspace/demo").expect("package");
        let profile = IndexOperationSourceProfile {
            profile: SemanticLanguageProfile::from_name("rust").expect("Rust profile"),
            source_version: [5; 32],
            input_digest: [6; 32],
            observation_sequence: 7,
            source_count: 1,
            state: IndexOperationSemanticProfileState::Pending { prior: None },
        };
        let capture = IndexOperationSourceCaptureReceipt::from_checked_parts(
            key,
            [2; 32],
            [3; 32],
            8,
            vec![profile].into_boxed_slice(),
        )
        .expect("checked structural receipt");
        let view = operation_receipt_view();
        let publication = IndexOperationPublicationReceipt::from_published_view(
            Some([1; 32]),
            [2; 32],
            [3; 32],
            9,
            &view,
            crate::Cursor::for_view_root(&view),
        )
        .expect("checked publication receipt");
        let status = IndexOperationStatus::new(
            key,
            package,
            crate::CompileExecutionIntent::Interactive,
            IndexOperationState::Published(publication),
        )
        .with_source_capture(Some(capture));
        let reply = SurfaceReply::IndexOperationStatus(IndexOperationObservation::Known(status));
        assert_eq!(
            reply.admit(CommandId::IndexProgress),
            Err(ProductAdmissionError::IndexOperationShape),
            "a semantic terminal cannot carry a source profile that is still pending"
        );
    }

    #[test]
    fn operation_receipt_admission_rejects_malformed_embedded_cursor_identities() {
        let key = IndexOperationKey::from_bytes([0x0a; 32]).expect("operation key");
        let package = PackageReference::parse("/workspace/demo").expect("package");
        let view = operation_receipt_view();
        let receipt = IndexOperationPublicationReceipt::from_published_view(
            Some([1; 32]),
            [2; 32],
            [3; 32],
            1,
            &view,
            crate::Cursor::for_view_root(&view),
        )
        .expect("checked publication receipt");
        let status = IndexOperationStatus {
            operation_key: key,
            request_digest: index_operation_request_digest(
                &package,
                crate::CompileExecutionIntent::Interactive,
            ),
            package,
            execution_intent: crate::CompileExecutionIntent::Interactive,
            source_capture: None,
            state: IndexOperationState::Published(receipt.clone()),
        };

        for (range, replacement) in [
            (
                130..132,
                Some((crate::cursor::CURSOR_SCHEMA + 1).to_be_bytes().to_vec()),
            ),
            (66..98, None),
            (98..130, None),
        ] {
            let mut malformed_receipt = receipt.clone();
            let mut cursor = malformed_receipt.revision_cursor().to_vec();
            if let Some(replacement) = replacement {
                cursor[range.clone()].copy_from_slice(&replacement);
            } else {
                cursor[range.clone()].fill(0);
            }
            malformed_receipt.revision_cursor = cursor.into_boxed_slice();

            let mut malformed_status = status.clone();
            malformed_status.state = IndexOperationState::Published(malformed_receipt);
            let malformed = IndexOperationObservation::Known(malformed_status);
            let wire = serde_json::to_vec(&malformed).expect("malformed receipt wire");
            let decoded: IndexOperationObservation =
                serde_json::from_slice(&wire).expect("malformed receipt decodes before admission");
            assert!(
                matches!(
                    decoded.admit(),
                    Err(ProductAdmissionError::IndexOperationShape)
                ),
                "cursor identity bytes at {range:?} must be structurally rejected"
            );
        }
    }

    #[test]
    fn json_status_budget_counts_encoded_bytes_without_underestimating() {
        let statuses = [
            SemanticHistoryPublicationStatus::NotSelected,
            SemanticHistoryPublicationStatus::Deferred {
                selection_id: [3; 32],
                reason: "line\n\u{1}".repeat(150),
            },
            SemanticHistoryPublicationStatus::Refused {
                selection_id: [9; 32],
                reason: "x".repeat(MAX_SEMANTIC_HISTORY_STATUS_DETAIL_BYTES),
            },
            SemanticHistoryPublicationStatus::Superseded {
                selection_id: [7; 32],
            },
        ];

        for status in statuses {
            let expected = serde_json::to_vec(&status)
                .expect("status serializes")
                .len();
            assert_eq!(serialized_json_size(&status), expected);
        }
    }

    #[test]
    fn owner_index_job_contract_round_trips_with_project_bound_terminal() {
        let package = PackageReference::parse("/workspace/demo").expect("package");
        let ticket = IndexJobTicket::new(
            NonZeroU64::new(7).expect("nonzero ticket"),
            [3; 16],
            package.clone(),
        );
        let command = SurfaceCommand::IndexStart {
            package: package.clone(),
            execution_intent: crate::CompileExecutionIntent::Interactive,
        };
        command.admit().expect("start admission");
        let encoded = serde_json::to_vec(&command).expect("start encoding");
        let decoded: SurfaceCommand = serde_json::from_slice(&encoded).expect("start decoding");
        assert_eq!(decoded, command);
        assert_eq!(decoded.id(), CommandId::IndexStart);

        let progress_command = SurfaceCommand::IndexProgress {
            ticket: ticket.clone(),
            after_sequence: 3,
        };
        progress_command
            .admit()
            .expect("progress request admission");
        let progress_command_wire =
            serde_json::to_vec(&progress_command).expect("progress request encoding");
        let decoded_progress_command: SurfaceCommand =
            serde_json::from_slice(&progress_command_wire).expect("progress request decoding");
        assert_eq!(decoded_progress_command, progress_command);
        assert_eq!(decoded_progress_command.id(), CommandId::IndexProgress);

        let started = SurfaceReply::IndexStarted(IndexStartResult::Started {
            ticket: ticket.clone(),
            stage: IndexJobStage::Compiling,
        });
        started.admit(CommandId::IndexStart).expect("start reply");
        let terminal = SurfaceReply::IndexTerminal(IndexJobTerminal {
            ticket: ticket.clone(),
            outcome: IndexJobOutcome::Refused(
                ProductText::new("compiler refused input").expect("bounded refusal"),
            ),
        });
        terminal
            .admit(CommandId::IndexAwait)
            .expect("terminal reply");
        let encoded = serde_json::to_vec(&terminal).expect("terminal encoding");
        let decoded: SurfaceReply = serde_json::from_slice(&encoded).expect("terminal decoding");
        assert_eq!(decoded, terminal);

        let cancellation = SurfaceReply::IndexCancellation(IndexCancelReceipt {
            ticket: ticket.clone(),
            status: IndexCancelStatus::Requested,
        });
        cancellation
            .admit(CommandId::IndexCancel)
            .expect("cancellation reply");
        let cancellation_wire =
            serde_json::to_vec(&cancellation).expect("cancellation reply encoding");
        let decoded_cancellation: SurfaceReply =
            serde_json::from_slice(&cancellation_wire).expect("cancellation reply decoding");
        assert_eq!(decoded_cancellation, cancellation);
        let mismatched_cancellation = SurfaceReply::IndexCancellation(IndexCancelReceipt {
            ticket: ticket.clone(),
            status: IndexCancelStatus::Terminal(IndexJobTerminal {
                ticket: IndexJobTicket::new(
                    NonZeroU64::new(8).expect("nonzero id"),
                    [3; 16],
                    PackageReference::parse("pkg:cargo/demo@1.0.0").expect("package"),
                ),
                outcome: IndexJobOutcome::Cancelled,
            }),
        });
        assert_eq!(
            mismatched_cancellation.admit(CommandId::IndexCancel),
            Err(ProductAdmissionError::IndexCancelTicketMismatch)
        );

        let progress =
            SurfaceReply::IndexProgress(IndexJobObservation::Pending(IndexProgressPage {
                ticket: ticket.clone(),
                stage: IndexJobStage::Compiling,
                events: vec![
                    IndexJobProgressEvent {
                        ticket: ticket.clone(),
                        sequence: 4,
                        kind: IndexJobProgressKind::StageChanged {
                            stage: IndexJobStage::Compiling,
                        },
                    },
                    IndexJobProgressEvent {
                        ticket: ticket.clone(),
                        sequence: 5,
                        kind: IndexJobProgressKind::ProfileStarted {
                            profile: SemanticLanguageProfile::from_name("rust").expect("profile"),
                            ordinal: 1,
                            total: 2,
                        },
                    },
                ]
                .into_boxed_slice(),
                next_sequence: 5,
                truncated: false,
                has_more: false,
            }));
        progress
            .admit(CommandId::IndexProgress)
            .expect("progress reply admission");
        let progress_wire = serde_json::to_vec(&progress).expect("progress reply encoding");
        let decoded_progress: SurfaceReply =
            serde_json::from_slice(&progress_wire).expect("progress reply decoding");
        assert_eq!(decoded_progress, progress);

        let malformed =
            SurfaceReply::IndexProgress(IndexJobObservation::Pending(IndexProgressPage {
                ticket: ticket.clone(),
                stage: IndexJobStage::Compiling,
                events: vec![IndexJobProgressEvent {
                    ticket: ticket.clone(),
                    sequence: 1,
                    kind: IndexJobProgressKind::ProfileAdmitted {
                        profile: SemanticLanguageProfile::from_name("rust").expect("profile"),
                        ordinal: 0,
                        total: 1,
                    },
                }]
                .into_boxed_slice(),
                next_sequence: 1,
                truncated: false,
                has_more: false,
            }));
        assert_eq!(
            malformed.admit(CommandId::IndexProgress),
            Err(ProductAdmissionError::IndexProgressShape)
        );

        let observed_terminal =
            SurfaceReply::IndexProgress(IndexJobObservation::Terminal(IndexJobTerminal {
                ticket: ticket.clone(),
                outcome: IndexJobOutcome::Cancelled,
            }));
        observed_terminal
            .admit(CommandId::IndexProgress)
            .expect("terminal observation admission");
        let unknown = SurfaceReply::IndexProgress(IndexJobObservation::Unknown {
            ticket,
            current_owner_epoch: [4; 16],
        });
        unknown
            .admit(CommandId::IndexProgress)
            .expect("unknown observation admission");
    }

    #[test]
    fn advisory_command_and_reply_round_trip_with_safe_unknown_state() {
        let command = SurfaceCommand::Advisory {
            package: PackageReference::parse("pkg:cargo/demo@1.0.0").expect("package"),
            override_evidence: Some(OverrideEvidence {
                actor: "release-bot".to_owned(),
                reason: "reviewed emergency pin".to_owned(),
                policy_version: 3,
                expires_at: Some(4_102_444_800),
            }),
        };
        command.admit().expect("advisory command admission");
        let command_json = serde_json::to_vec(&command).expect("command encoding");
        let decoded: SurfaceCommand =
            serde_json::from_slice(&command_json).expect("command decoding");
        assert_eq!(decoded, command);

        let reply = SurfaceReply::Advisory(AdvisoryPackageDto::unknown());
        reply
            .admit(CommandId::Advisory)
            .expect("advisory reply admission");
        let reply_json = serde_json::to_vec(&reply).expect("reply encoding");
        let decoded: SurfaceReply = serde_json::from_slice(&reply_json).expect("reply decoding");
        assert_eq!(decoded, reply);
        let SurfaceReply::Advisory(dto) = decoded else {
            panic!("advisory reply shape");
        };
        // Unknown coverage warns without blocking acquisition (e7aeabfb1).
        assert!(matches!(
            dto.decision,
            backend_advisory::AcquisitionDecision::Warn(_)
        ));
    }

    #[test]
    fn registry_download_coverage_round_trips_as_a_typed_state() {
        let native_metadata =
            RegistryNativeMetadata::unavailable(RegistryEcosystem::Cargo, "test fixture");
        let facts_version = [2; 32];
        let record = RegistryPackageRecord {
            coordinate: PackageReference::parse("pkg:cargo/demo@1.0.0").expect("package"),
            ecosystem: RegistryEcosystem::Cargo,
            name: ProductText::new("demo").expect("name"),
            version: ProductText::new("1.0.0").expect("version"),
            bytes: 12,
            standing: RegistryReleaseStanding::Available,
            downloads: RegistryDownloadCount::Unavailable(RegistryFactAvailability::Unsupported),
            facts_version,
            authority: Some(RegistryPackageFactAuthority {
                source: [3; 32],
                source_facts_root: [4; 32],
                source_provenance: [5; 32],
                facts_version,
                advisory_facts_version: [6; 32],
                selection_version: [7; 32],
                standing: RegistryPackageFactCompleteness::Complete,
                downloads: RegistryPackageFactCompleteness::Unsupported,
                advisories: RegistryPackageFactCompleteness::Unknown,
                release_facts_freshness: RegistryPackageFactFreshness::Current {
                    observed_at_millis: 10,
                    valid_until_millis: 20,
                    proof: RegistryPackageFactProof::AcquisitionReceipt {
                        receipt: [8; 32],
                        snapshot: [9; 32],
                    },
                },
                advisory_freshness: backend_advisory::FreshnessState::Stale,
            }),
            native_metadata_version: native_metadata
                .identity()
                .expect("native metadata identity"),
            native_metadata,
            forge_sources: Box::new([]),
            advisory: AdvisoryPackageDto::unknown(),
        };
        let reply = SurfaceReply::Explored(vec![record].into_boxed_slice());
        let encoded = serde_json::to_vec(&reply).expect("registry reply encoding");
        let decoded: SurfaceReply =
            serde_json::from_slice(&encoded).expect("registry reply decoding");
        assert_eq!(decoded, reply);
        let SurfaceReply::Explored(records) = decoded else {
            panic!("registry reply shape");
        };
        assert_eq!(
            records[0].downloads,
            RegistryDownloadCount::Unavailable(RegistryFactAvailability::Unsupported)
        );
        assert_eq!(
            records[0]
                .authority
                .expect("authority")
                .release_facts_freshness,
            RegistryPackageFactFreshness::Current {
                observed_at_millis: 10,
                valid_until_millis: 20,
                proof: RegistryPackageFactProof::AcquisitionReceipt {
                    receipt: [8; 32],
                    snapshot: [9; 32],
                },
            }
        );
    }
}
