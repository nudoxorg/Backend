//! Typed commands and results owned by the durable product service.

use crate::{
    CommandId, DependencyFacts, ForgeCoordinate, ForgeObjectId, ForgeRevision,
    PackageDependencyRecord, RegistryForgeAssociation, RegistryNativeMetadata,
};
use backend_advisory::{AdvisoryPackageDto, OverrideEvidence};
pub use backend_semantic::vocabulary::{PackageUrl as PackageCoordinate, RegistryEcosystem};
use serde::{Deserialize, Serialize};
use std::io::{self, Write};
use std::num::NonZeroU64;

/// Largest user-authored operand retained by the product service.
pub const MAX_PRODUCT_TEXT_BYTES: usize = 4096;
/// Largest row collection in one product request or reply.
pub const MAX_PRODUCT_ROWS: usize = 256;
/// Largest opaque continuation token accepted by the shared index-search surface.
pub const MAX_INDEX_SEARCH_CURSOR_BYTES: usize = 64 * 1024;
/// Maximum number of owner progress events returned by one index progress read.
pub const MAX_INDEX_PROGRESS_EVENTS: usize = 16;
/// Maximum human-readable detail retained in one derived-history status.
pub const MAX_SEMANTIC_HISTORY_STATUS_DETAIL_BYTES: usize = 1024;

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
    /// Returns the canonical spelling.
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Purl(v) => v.as_str(),
            Self::Local(v) => v.as_str(),
        }
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
    /// Canonical semantic image bytes covered by the manifest.
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

/// Bounded public summary of the successful typed V3 branch publication.
///
/// It contains the exact owner selection, native image, ref CAS result, and
/// parent lineage needed to investigate a published generation. The input
/// field deliberately states `Unproven`; this summary does not recreate the
/// compiler's source read frontier.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticHistoryPublicationProof {
    /// Exact committed product selection used by the publication fence.
    pub selection: SemanticHistorySelectionStamp,
    /// Full-image identity bound to that selection.
    pub image: SemanticHistoryImageIdentity,
    /// Branch tip at the successful CAS or validated ancestry read.
    pub reference_tip: [u8; 32],
    /// Commit proven reachable from `reference_tip`; equals the status commit.
    pub reachable_commit: [u8; 32],
    /// First-parent lineage recorded by the admitted history commit (at most 2).
    pub parent_commits: Box<[[u8; 32]]>,
    /// Exact persisted input authority level.
    pub input_replay_status: SemanticHistoryInputReplayStatus,
}

/// Typed status for derived native-image history publication.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum SemanticHistoryPublicationStatus {
    /// This semantic version is not the committed selected product version.
    NotSelected,
    /// The committed selection has not yet been reconciled with V3 history.
    NotRequested { selection_id: [u8; 32] },
    /// A bounded worker is producing and admitting history for this selection.
    Pending { selection_id: [u8; 32] },
    /// The committed selection is waiting for a bounded worker slot. The
    /// owner reschedules it from the current selected-marker inventory.
    Deferred {
        selection_id: [u8; 32],
        reason: String,
    },
    /// The exact selected image is durably published at this V3 branch commit.
    Published {
        selection_id: [u8; 32],
        commit: [u8; 32],
        reference: String,
        /// Exact, bounded proof summary for this branch commit.
        proof: SemanticHistoryPublicationProof,
    },
    /// History could not be produced or admitted for this selected image.
    Refused {
        selection_id: [u8; 32],
        reason: String,
    },
    /// The marker advanced while this derived-history job was running.
    Superseded { selection_id: [u8; 32] },
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
    Current { input_digest: [u8; 32] },
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
    fn admit(&self) -> Result<(), ProductAdmissionError> {
        self.profile.profile()?;
        if self.coordinate.package_type().language() != self.profile.profile()?.language()
            || self.artifacts == 0
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
                || proof.parent_commits.len() > 2
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

/// Terminal effect of one index job.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", content = "detail", rename_all = "kebab-case")]
pub enum IndexJobOutcome {
    /// Semantic admission and the owner publication completed.
    Published,
    /// The candidate was refused and did not replace the prior publication.
    Refused(ProductText),
    /// The requested cancellation was observed before publication completed.
    Cancelled,
    /// The owner could not establish a terminal publication result.
    Failed(ProductText),
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
        if let IndexCancelStatus::Terminal(terminal) = &self.status
            && terminal.ticket != self.ticket
        {
            return Err(ProductAdmissionError::IndexCancelTicketMismatch);
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
            Self::Terminal(_) | Self::Unknown { .. } => Ok(()),
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
    /// Start indexing one local project without waiting for compilation.
    IndexStart {
        /// Local project directory or pinned package coordinate.
        package: PackageReference,
        /// Requested compiler execution class.
        execution_intent: crate::CompileExecutionIntent,
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
            Self::IndexStart { .. } => CommandId::IndexStart,
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
        observed_at_millis: u64,
        valid_until_millis: u64,
    },
    /// The claim was recovered from durable history after a cold reopen.
    Historical { observed_at_millis: u64 },
    /// The source has not refreshed the claim within its freshness horizon.
    Expired {
        observed_at_millis: u64,
        valid_until_millis: u64,
    },
    /// The last source refresh failed, so this is the last durable claim.
    Unavailable {
        observed_at_millis: u64,
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
    /// Alternate source-provided advisory identifiers.
    pub aliases: Box<[ProductText]>,
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
        if advisory.aliases.len() > 32 {
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
        let source = crate::ForgeCoordinate::parse(self.forge_coordinate.as_str().to_owned())
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
    PackageVersion { coordinate: PackageCoordinate },
    /// The source resolved to a commit, but the manifest supplied no valid release PURL.
    PinnedRevision {
        requested_revision: ForgeRevision,
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
    LocalDeclaration(RegistryPackageRecord),
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
    pub dependencies: crate::DependencyFacts<Box<[crate::PackageDependencyRecord]>>,
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
    Dependencies(crate::DependencyFacts<Box<[crate::PackageDependencyRecord]>>),
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
    /// Immediate status returned after an index start request.
    IndexStarted(IndexStartResult),
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
            Self::IndexStarted(_) => CommandId::IndexStart,
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
            | Self::Owner(RegistryMetadata::Recorded(v)) => v.len(),
            Self::IndexSearchWithDiscovery(hits) => hits.len(),
            Self::PackageDetails { registry, forge } => registry.len().saturating_add(forge.len()),
            Self::IndexSearchPage(page) => page.hits.len(),
            Self::Dependencies(crate::DependencyFacts::Known(v)) => v.len(),
            Self::PackageGraphPage(page) => {
                page.admit()
                    .map_err(|_| ProductAdmissionError::PackageGraphPage)?;
                page.rows.len()
            }
            Self::SemanticVersions(records) => {
                let mut selected = 0_usize;
                for record in records {
                    record.admit()?;
                    selected = selected.saturating_add(usize::from(record.selected));
                }
                if selected > 1 {
                    return Err(ProductAdmissionError::SemanticVersionShape);
                }
                records.len()
            }
            Self::SemanticVersionSelected(record) => {
                record.admit()?;
                if !record.selected {
                    return Err(ProductAdmissionError::SemanticVersionShape);
                }
                1
            }
            Self::IndexStarted(_) | Self::IndexTerminal(_) => 1,
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
            Self::ProjectTree(tree)
                if tree.packages.len() > crate::browse::MAX_TREE_PACKAGES
                    || tree.direct.len() > tree.packages.len() =>
            {
                return Err(ProductAdmissionError::RowBound);
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
                            RegistrySearchHit::Acquired(record)
                            | RegistrySearchHit::LocalDeclaration(record) => {
                                admit_registry_record(record)?;
                            }
                            RegistrySearchHit::Discovered(candidate) => candidate.admit()?,
                            RegistrySearchHit::ForgeDiscovered(candidate) => candidate.admit()?,
                            RegistrySearchHit::ForgeSourcePin(candidate) => candidate.admit()?,
                            RegistrySearchHit::PackageGroup(group) => group.admit()?,
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
                            RegistrySearchHit::Acquired(record)
                            | RegistrySearchHit::LocalDeclaration(record) => {
                                admit_registry_record(record)?;
                            }
                            RegistrySearchHit::Discovered(candidate) => candidate.admit()?,
                            RegistrySearchHit::ForgeDiscovered(candidate) => candidate.admit()?,
                            RegistrySearchHit::ForgeSourcePin(candidate) => candidate.admit()?,
                            RegistrySearchHit::PackageGroup(group) => group.admit()?,
                        }
                    }
                }
                Self::Dependents(RegistryMetadata::Recorded(rows))
                | Self::Owner(RegistryMetadata::Recorded(rows)) => {
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
                Self::PackageProfile {
                    latest: Some(row),
                    candidate_authority,
                    ..
                } => {
                    admit_registry_record(row)?;
                    if candidate_authority != &row.authority {
                        return Err(ProductAdmissionError::RegistryAuthority);
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
            Self::IndexSearchWithDiscovery(hits) => hits.iter().fold(0_usize, |bound, hit| {
                bound.saturating_add(match hit {
                    RegistrySearchHit::Acquired(record)
                    | RegistrySearchHit::LocalDeclaration(record) => {
                        registry_package_record_bound(record)
                    }
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
            }),
            Self::SemanticVersionSelected(record) => fixed_record_bound()
                .saturating_add(package_reference_bound(&record.package))
                .saturating_add(record.coordinate.as_str().len())
                .saturating_add(serialized_json_size(&record.history_status)),
            Self::IndexStarted(result) => fixed_record_bound()
                .saturating_add(serde_json::to_vec(result).map_or(0, |bytes| bytes.len())),
            Self::IndexTerminal(terminal) => fixed_record_bound()
                .saturating_add(serde_json::to_vec(terminal).map_or(0, |bytes| bytes.len())),
            Self::IndexProgress(page) => fixed_record_bound()
                .saturating_add(serde_json::to_vec(page).map_or(0, |bytes| bytes.len())),
            Self::IndexCancellation(receipt) => fixed_record_bound()
                .saturating_add(serde_json::to_vec(receipt).map_or(0, |bytes| bytes.len())),
            Self::Dependents(RegistryMetadata::NotRecorded(reason))
            | Self::Owner(RegistryMetadata::NotRecorded(reason)) => text_bound(reason),
            Self::Dependencies(crate::DependencyFacts::Known(records)) => {
                records.iter().fold(0_usize, |bound, record| {
                    bound.saturating_add(dependency_record_bound(record))
                })
            }
            Self::Dependencies(crate::DependencyFacts::Unknown(reason))
            | Self::Dependencies(crate::DependencyFacts::Unavailable(reason)) => text_bound(reason),
            Self::PackageGraphPage(page) => fixed_record_bound()
                .saturating_add(serde_json::to_vec(page).map_or(0, |bytes| bytes.len())),
            Self::PackageProfile {
                latest,
                candidate_authority,
                ..
            } => latest
                .as_ref()
                .map_or(64, registry_package_record_bound)
                .saturating_add(
                    serde_json::to_vec(candidate_authority).map_or(0, |bytes| bytes.len()),
                ),
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

fn dependency_record_bound(record: &crate::PackageDependencyRecord) -> usize {
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
    /// Native registry metadata is malformed, oversized, or has a stale identity.
    NativeMetadata,
    /// Registry fact authority is inconsistent with the selected package facts.
    RegistryAuthority,
    /// An unacquired discovery candidate has invalid freshness evidence.
    RegistryDiscoveryFreshness,
    /// A registry discovery metadata facet is outside the product bounds.
    RegistryDiscoveryMetadata,
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
    /// An index progress page contains mismatched tickets or invalid sequence/profile facts.
    IndexProgressShape,
    /// An index cancellation terminal receipt belongs to a different ticket.
    IndexCancelTicketMismatch,
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
            Self::IndexProgressShape => {
                "index progress page has inconsistent sequence or profile facts"
            }
            Self::IndexCancelTicketMismatch => {
                "index cancellation terminal receipt has a different ticket"
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
