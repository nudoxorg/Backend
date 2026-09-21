//! Canonical references to compiler semantic publications.
//!
//! Compiler publication owns immutable fragment, image, manifest, binding, and
//! journal bytes. This relation retains one checked claim for the complete
//! atomic publication. Reopening and lending semantic views remains the
//! publication owner's responsibility.

use std::{mem::size_of, num::NonZeroU32, sync::Arc};

use super::complete_coverage;
use crate::publication::{
    OpenedSemanticArtifactCursor, OpenedSemanticGeneration,
    binding::{
        COMPILATION_BINDING_BYTES, CompilationBindingFacts, CompilationBindingIdentity,
        CompilationBindingView,
    },
    manifest::{CompilationManifestFacts, CompilationManifestFormat, CompilationManifestIdentity},
};
use crate::workspace::{WorkspaceRelationHandle, WorkspaceSnapshot};
use backend_execution::AuthorityVersion;
use backend_library::{PackageKey, PackageReference, package_key};
use backend_semantic::ir::{
    ImageProvenance, PackageLineage, SemanticCoreReader, SemanticImageAuthority,
};
use backend_semantic::vocabulary::{LanguageProfile, PackageUrl, Stage};
use backend_store::hydration::VerifiedGenerationFacts;
use backend_version::{
    CanonicalRelation, Relation, RelationDecodeError, RelationState, StateRoot, WorkspaceRoot,
};
use backend_version::{ContentId, DependencySetDomain, GenerationId};

const MAGIC: &[u8; 4] = b"PSP1";
const IDENTITY_BYTES: usize = size_of::<[u8; 32]>();
const GENERATION_BYTES: usize = IDENTITY_BYTES * 2;
const MANIFEST_BYTES: usize = IDENTITY_BYTES + size_of::<u8>() + size_of::<u32>() * 2;
const BINDING_FACT_BYTES: usize = IDENTITY_BYTES + GENERATION_BYTES + IDENTITY_BYTES;
const CLAIM_BYTES: usize = MANIFEST_BYTES + BINDING_FACT_BYTES;

/// Package-scoped identity of one compiler authority transaction.
///
/// The product package reference and exact compiler coordinate are retained as
/// already-parsed values. The former joins this row to the product frontier;
/// the latter preserves namespace, version, qualifiers, and subpath through
/// the compiler boundary.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct ProductSemanticPublicationKey {
    package: PackageReference,
    coordinate: PackageUrl,
    profile: LanguageProfile,
    selection: SemanticPublicationSelection,
}

/// Which immutable compiler publication a semantic relation row names.
///
/// Every package/profile target owns exactly one mutable [`Selected`](Self::Selected)
/// row. Published claims are also retained under their binding identity, so a
/// caller can reopen an exact older generation without interpreting display
/// labels or relying on insertion order.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum SemanticPublicationSelection {
    /// The publication currently selected for product projection.
    Selected,
    /// One immutable compiler generation retained by exact binding identity.
    Generation(CompilationBindingIdentity),
}

/// A semantic history key and its value disagree about the selected generation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SemanticPublicationSelectionError {
    /// An immutable generation row carried an unavailable terminal.
    GenerationUnavailable,
    /// The generation key names a different binding from the publication claim.
    Binding {
        /// Binding identity committed by the history key.
        expected: CompilationBindingIdentity,
        /// Binding identity carried by the publication claim.
        observed: CompilationBindingIdentity,
    },
}

impl std::fmt::Display for SemanticPublicationSelectionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::GenerationUnavailable => {
                formatter.write_str("an immutable semantic generation cannot be unavailable")
            }
            Self::Binding { .. } => formatter
                .write_str("semantic generation key does not match its publication binding"),
        }
    }
}

impl std::error::Error for SemanticPublicationSelectionError {}

impl ProductSemanticPublicationKey {
    /// Admits one complete compiler transaction key.
    ///
    /// # Errors
    /// Returns an error when the compiler coordinate belongs to another
    /// language family than the selected profile.
    pub fn new(
        package: PackageReference,
        coordinate: PackageUrl,
        profile: LanguageProfile,
    ) -> Result<Self, &'static str> {
        if coordinate.package_type().language() != profile.language() {
            return Err("semantic compiler coordinate and language profile differ");
        }
        PackageLineage::new(
            coordinate.package_type().as_str(),
            coordinate.lineage_name(),
        )
        .map_err(|_| "semantic package lineage is malformed")?;
        Ok(Self {
            package,
            coordinate,
            profile,
            selection: SemanticPublicationSelection::Selected,
        })
    }

    /// Returns a key for one exact immutable compiler generation of this target.
    #[must_use]
    pub fn for_generation(&self, identity: CompilationBindingIdentity) -> Self {
        Self {
            package: self.package.clone(),
            coordinate: self.coordinate.clone(),
            profile: self.profile,
            selection: SemanticPublicationSelection::Generation(identity),
        }
    }

    /// Reopens an exact generation key from its product transport bytes.
    ///
    /// # Errors
    /// Returns an error unless the bytes carry the compiler publication
    /// encoding and domain tags required by a binding identity.
    pub fn for_generation_bytes(&self, identity: [u8; 32]) -> Result<Self, &'static str> {
        let identity = CompilationBindingIdentity::try_from(identity.as_slice())
            .map_err(|_| "semantic generation binding identity is malformed")?;
        Ok(self.for_generation(identity))
    }

    /// Returns the selected or immutable-generation role of this row.
    #[must_use]
    pub const fn selection(&self) -> SemanticPublicationSelection {
        self.selection
    }

    /// Returns whether this is the one row product surfaces may project.
    #[must_use]
    pub const fn is_selected(&self) -> bool {
        matches!(self.selection, SemanticPublicationSelection::Selected)
    }

    /// Verifies that an immutable history key names the binding carried by its
    /// publication value. Selected rows admit both published and explicitly
    /// unavailable terminals.
    ///
    /// # Errors
    /// Returns a typed mismatch when a history row is unavailable or carries a
    /// claim for another binding identity.
    pub fn admit_record(
        &self,
        record: &ProductSemanticPublicationRecord,
    ) -> Result<(), SemanticPublicationSelectionError> {
        let SemanticPublicationSelection::Generation(expected) = self.selection else {
            return Ok(());
        };
        let ProductSemanticPublicationRecord::Published { claim, .. } = record else {
            return Err(SemanticPublicationSelectionError::GenerationUnavailable);
        };
        let observed = claim.binding().identity;
        if expected != observed {
            return Err(SemanticPublicationSelectionError::Binding { expected, observed });
        }
        Ok(())
    }

    /// Borrows the admitted package lineage.
    ///
    /// # Errors
    /// Returns an error only if an internal immutable-key invariant was violated.
    pub fn lineage(&self) -> Result<PackageLineage<'_>, &'static str> {
        PackageLineage::new(
            self.coordinate.package_type().as_str(),
            self.coordinate.lineage_name(),
        )
        .map_err(|_| "stored semantic package lineage is malformed")
    }

    /// Returns the stable product package key derived from the retained
    /// package reference.
    #[must_use]
    pub fn package_key(&self) -> PackageKey {
        package_key(self.package.as_str())
    }

    /// Returns the closed language profile.
    #[must_use]
    pub const fn profile(&self) -> LanguageProfile {
        self.profile
    }

    /// Returns the exact product package reference.
    #[must_use]
    pub const fn package(&self) -> &PackageReference {
        &self.package
    }

    /// Returns the exact version-pinned compiler coordinate.
    #[must_use]
    pub const fn coordinate(&self) -> &PackageUrl {
        &self.coordinate
    }

    /// Verifies that one reopened image belongs to this exact semantic target
    /// before product code projects any of its rows.
    ///
    /// # Errors
    /// Returns a typed mismatch when image authority, recipe, or retained
    /// package lineage differs from this publication key.
    pub fn admit_image<Reader: SemanticCoreReader + ?Sized>(
        &self,
        image: &Reader,
    ) -> Result<(), SemanticPublicationTargetError> {
        let facts = image.image_facts();
        let expected_authority = SemanticImageAuthority::Language(self.profile);
        if facts.authority != expected_authority {
            return Err(SemanticPublicationTargetError::Authority {
                expected: expected_authority,
                observed: facts.authority,
            });
        }
        let ImageProvenance::Captured { recipe, scope, .. } = facts.provenance else {
            return Err(SemanticPublicationTargetError::ProvenanceUnavailable);
        };
        if recipe.profile != self.profile {
            return Err(SemanticPublicationTargetError::RecipeProfile {
                expected: self.profile,
                observed: recipe.profile,
            });
        }
        if recipe.stage != Stage::LowerIr {
            return Err(SemanticPublicationTargetError::RecipeStage {
                observed: recipe.stage,
            });
        }
        let lineage = self
            .lineage()
            .map_err(|_| SemanticPublicationTargetError::StoredLineage)?;
        if image.atom(scope.ecosystem) != Some(lineage.ecosystem.as_bytes()) {
            return Err(SemanticPublicationTargetError::Ecosystem);
        }
        if image.atom(scope.package) != Some(lineage.name.as_bytes()) {
            return Err(SemanticPublicationTargetError::Package);
        }
        let Some(coordinate) = scope.coordinate else {
            return Err(SemanticPublicationTargetError::CoordinateUnavailable);
        };
        if image.atom(coordinate) != Some(self.coordinate.as_str().as_bytes()) {
            return Err(SemanticPublicationTargetError::Coordinate);
        }
        Ok(())
    }
}

/// Why a reopened semantic image cannot be projected under a product key.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SemanticPublicationTargetError {
    /// Image-wide authority names another language profile.
    Authority {
        /// Authority required by the persisted product key.
        expected: SemanticImageAuthority,
        /// Authority retained by the reopened image.
        observed: SemanticImageAuthority,
    },
    /// A product publication cannot use a manually assembled image without
    /// captured compiler provenance.
    ProvenanceUnavailable,
    /// The compiler recipe names another language profile.
    RecipeProfile {
        /// Profile required by the persisted product key.
        expected: LanguageProfile,
        /// Profile retained by the compiler recipe.
        observed: LanguageProfile,
    },
    /// The image came from a compiler phase that does not produce complete IR.
    RecipeStage {
        /// Stage retained by the compiler recipe.
        observed: Stage,
    },
    /// An immutable key violated its already-admitted lineage invariant.
    StoredLineage,
    /// The image's package ecosystem differs from the compiler coordinate.
    Ecosystem,
    /// The image's namespace/name lineage differs from the compiler coordinate.
    Package,
    /// The image predates exact coordinate commitment and cannot prove a
    /// version-pinned product claim.
    CoordinateUnavailable,
    /// The image's exact PackageUrl differs, including version, qualifier, or
    /// subpath cells.
    Coordinate,
}

impl std::fmt::Display for SemanticPublicationTargetError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "semantic publication target mismatch: {self:?}")
    }
}

impl std::error::Error for SemanticPublicationTargetError {}

/// Constant-size, locality-independent claim for one semantic generation.
///
/// It deliberately excludes journal sequence, offsets, and checksums: those are
/// locality evidence and differ when the same generation is replicated. An
/// owner activates the claim only after reopening the named manifest closure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SemanticPublicationClaim {
    manifest: CompilationManifestFacts,
    binding: CompilationBindingFacts,
}

impl SemanticPublicationClaim {
    /// Admits mutually consistent manifest and binding facts.
    ///
    /// # Errors
    /// Returns an error unless the manifest is semantic and the binding names it.
    pub fn admit(
        manifest: CompilationManifestFacts,
        binding: CompilationBindingFacts,
    ) -> Result<Self, &'static str> {
        if manifest.format != CompilationManifestFormat::SemanticV2 {
            return Err("compiler publication does not name semantic images");
        }
        if manifest.fragment_count == 0 {
            return Err("semantic publication has no artifacts");
        }
        if manifest.identity != binding.manifest {
            return Err("manifest and binding identities differ");
        }
        let mut bytes = [0_u8; COMPILATION_BINDING_BYTES];
        let rebuilt =
            CompilationBindingView::write_into(binding.generation, binding.manifest, &mut bytes)
                .map_err(|_| "semantic compilation binding is malformed")?;
        if rebuilt.identity != binding.identity {
            return Err("semantic compilation binding identity does not match its facts");
        }
        Ok(Self { manifest, binding })
    }

    /// Returns the claimed semantic manifest.
    #[must_use]
    pub const fn manifest(self) -> CompilationManifestFacts {
        self.manifest
    }

    /// Returns the generation binding used to locate and verify the closure.
    #[must_use]
    pub const fn binding(self) -> CompilationBindingFacts {
        self.binding
    }

    /// Activates this persisted claim with a publication-owner reopened proof.
    ///
    /// # Errors
    /// Returns an error when the owner reopened a different selected closure.
    pub fn activate<'manifest, 'facts, 'fragments, 'semantic>(
        self,
        opened: OpenedSemanticGeneration<'manifest, 'facts, 'fragments, 'semantic>,
    ) -> Result<
        ActivatedSemanticPublication<'manifest, 'facts, 'fragments, 'semantic>,
        SemanticActivationError,
    > {
        if *opened.manifest != self.manifest {
            return Err(SemanticActivationError::Manifest {
                expected: self.manifest,
                observed: *opened.manifest,
            });
        }
        if opened.binding != self.binding {
            return Err(SemanticActivationError::Binding {
                expected: Box::new(self.binding),
                observed: Box::new(opened.binding),
            });
        }
        Ok(ActivatedSemanticPublication { opened })
    }
}

/// Owner-activated semantic publication lending verified manifest artifacts.
pub struct ActivatedSemanticPublication<'manifest, 'facts, 'fragments, 'semantic> {
    opened: OpenedSemanticGeneration<'manifest, 'facts, 'fragments, 'semantic>,
}

impl<'fragments, 'semantic> ActivatedSemanticPublication<'_, '_, 'fragments, 'semantic> {
    /// Iterates manifest-bound compact and rich semantic artifacts.
    #[must_use]
    pub fn artifacts(&self) -> OpenedSemanticArtifactCursor<'_, 'fragments, 'semantic> {
        self.opened.artifacts()
    }
}

/// A persisted claim named a different closure than the publication owner reopened.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SemanticActivationError {
    /// The owner reopened a different semantic manifest.
    Manifest {
        /// Manifest named by the persisted claim.
        expected: CompilationManifestFacts,
        /// Manifest proven by the publication owner.
        observed: CompilationManifestFacts,
    },
    /// The owner reopened a different generation binding.
    Binding {
        /// Binding named by the persisted claim.
        expected: Box<CompilationBindingFacts>,
        /// Binding proven by the publication owner.
        observed: Box<CompilationBindingFacts>,
    },
}

impl std::fmt::Display for SemanticActivationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Manifest { .. } => formatter
                .write_str("semantic publication claim does not match the owner-selected manifest"),
            Self::Binding { .. } => formatter.write_str(
                "semantic publication claim does not match the owner-selected generation binding",
            ),
        }
    }
}

impl std::error::Error for SemanticActivationError {}

/// Admitted partial project coverage.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PartialSemanticCoverage {
    completed: NonZeroU32,
    total: NonZeroU32,
}

impl PartialSemanticCoverage {
    /// Admits a partial observation count.
    ///
    /// # Errors
    /// Returns an error unless both counts are nonzero and completed work is
    /// strictly smaller than the declared scope.
    pub const fn new(completed: NonZeroU32, total: NonZeroU32) -> Result<Self, &'static str> {
        if completed.get() >= total.get() {
            return Err("partial semantic scope must be smaller than its total");
        }
        Ok(Self { completed, total })
    }

    const fn completed(self) -> NonZeroU32 {
        self.completed
    }

    const fn total(self) -> NonZeroU32 {
        self.total
    }
}

/// Coverage of the project/package scope represented by one publication.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SemanticPublicationCoverage {
    /// The authority observed every member of its declared scope.
    Complete,
    /// Every published artifact is complete, while the authority observed only
    /// a checked portion of the declared project scope.
    Partial(PartialSemanticCoverage),
}

/// Exact reason no semantic publication exists for one selected compilation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum SemanticUnavailableReason {
    /// The selected compiler or oracle is absent.
    Toolchain = 1,
    /// Project/package authority required by the language is absent.
    ProjectAuthority = 2,
    /// The authority transaction was cancelled.
    Cancelled = 3,
    /// The authority rejected source or configuration.
    Rejected = 4,
}

impl SemanticUnavailableReason {
    /// Returns the stable lowercase name carried by surface diagnostics.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Toolchain => "toolchain",
            Self::ProjectAuthority => "project authority",
            Self::Cancelled => "cancelled",
            Self::Rejected => "rejected",
        }
    }

    const fn from_tag(tag: u8) -> Option<Self> {
        match tag {
            1 => Some(Self::Toolchain),
            2 => Some(Self::ProjectAuthority),
            3 => Some(Self::Cancelled),
            4 => Some(Self::Rejected),
            _ => None,
        }
    }
}

impl std::fmt::Display for SemanticUnavailableReason {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.name())
    }
}

/// Typed semantic terminal retained for one compiler transaction.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProductSemanticPublicationRecord {
    /// An atomically published generation with explicit project-scope coverage.
    Published {
        /// Project/package scope coverage associated with the publication.
        coverage: SemanticPublicationCoverage,
        /// Complete journal-selected publication claim.
        claim: SemanticPublicationClaim,
    },
    /// No publication was produced; the exact cause remains reconstructible.
    Unavailable(SemanticUnavailableReason),
}

/// Canonical relation from compiler transactions to semantic publications.
#[derive(Debug)]
pub struct ProductSemanticPublicationRelation;

impl Relation for ProductSemanticPublicationRelation {
    const DOMAIN: u8 = 0x97;
    const TYPE: u16 = 3;
    const VERSION: u8 = 3;
    type Key = ProductSemanticPublicationKey;
    type Value = ProductSemanticPublicationRecord;

    fn encode_key(key: &Self::Key, output: &mut Vec<u8>) {
        push_text(key.package.as_str(), output);
        push_text(key.coordinate.as_str(), output);
        output.extend_from_slice(&<[u8; 2]>::from(key.profile));
        match key.selection {
            SemanticPublicationSelection::Selected => output.push(0),
            SemanticPublicationSelection::Generation(identity) => {
                output.push(1);
                output.extend_from_slice(identity.as_ref());
            }
        }
    }

    fn encode_value(value: &Self::Value, output: &mut Vec<u8>) {
        output.extend_from_slice(MAGIC);
        match value {
            ProductSemanticPublicationRecord::Published { coverage, claim } => {
                output.push(1);
                encode_coverage(*coverage, output);
                encode_claim(*claim, output);
            }
            ProductSemanticPublicationRecord::Unavailable(reason) => {
                output.push(2);
                output.push(*reason as u8);
            }
        }
    }
}

impl CanonicalRelation for ProductSemanticPublicationRelation {
    fn decode_key(bytes: &[u8]) -> Result<Self::Key, RelationDecodeError> {
        let (package, rest) = take_text(bytes)?;
        let package = PackageReference::parse(package.to_owned())
            .map_err(|_| RelationDecodeError::Malformed)?;
        let (coordinate, rest) = take_text(rest)?;
        let coordinate =
            PackageUrl::parse(coordinate.to_owned()).map_err(|_| RelationDecodeError::Malformed)?;
        let (profile, rest) = rest
            .split_first_chunk::<2>()
            .ok_or(RelationDecodeError::Malformed)?;
        let profile =
            LanguageProfile::try_from(*profile).map_err(|_| RelationDecodeError::Malformed)?;
        let (&selection, rest) = rest.split_first().ok_or(RelationDecodeError::Malformed)?;
        let key = Self::Key::new(package, coordinate, profile)
            .map_err(|_| RelationDecodeError::Malformed)?;
        match selection {
            0 if rest.is_empty() => Ok(key),
            1 => {
                let (identity, rest) = take_identity(rest)?;
                if !rest.is_empty() {
                    return Err(RelationDecodeError::Malformed);
                }
                let identity = CompilationBindingIdentity::try_from(identity)
                    .map_err(|_| RelationDecodeError::Malformed)?;
                Ok(key.for_generation(identity))
            }
            _ => Err(RelationDecodeError::Malformed),
        }
    }

    fn decode_value(bytes: &[u8]) -> Result<Self::Value, RelationDecodeError> {
        let rest = bytes
            .strip_prefix(MAGIC)
            .ok_or(RelationDecodeError::Malformed)?;
        let (&tag, rest) = rest.split_first().ok_or(RelationDecodeError::Malformed)?;
        match tag {
            1 => decode_published(rest),
            2 => {
                let [reason] = rest else {
                    return Err(RelationDecodeError::Malformed);
                };
                Ok(ProductSemanticPublicationRecord::Unavailable(
                    SemanticUnavailableReason::from_tag(*reason)
                        .ok_or(RelationDecodeError::Malformed)?,
                ))
            }
            _ => Err(RelationDecodeError::Malformed),
        }
    }
}

/// Authenticated local-retention facts for the selected semantic publication
/// relation. These facts come from the checked root summary and closure; route
/// selection never substitutes request-provided cardinalities.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SemanticPublicationRetentionFacts {
    /// Semantic compiler transaction rows in the selected relation.
    pub relation_rows: u64,
    /// Height of the selected canonical relation root.
    pub relation_level: u16,
    /// Immutable objects retained by the selected workspace closure.
    pub retained_objects: u64,
}

/// A checked semantic publication relation selected by the workspace owner.
///
/// The handle keeps its authenticated store and root alive while local or
/// remote execution streams compiler publication claims. It never copies the
/// relation into an untyped product projection.
#[derive(Clone, Debug)]
pub struct ProductSemanticPublicationSnapshot {
    workspace: WorkspaceRoot,
    relation: WorkspaceRelationHandle<ProductSemanticPublicationRelation>,
    manifest: Arc<[u8]>,
    authority: [u8; backend_version::ID_BYTES],
    retention: SemanticPublicationRetentionFacts,
}

impl ProductSemanticPublicationSnapshot {
    /// Opens the semantic publication relation selected by a checked workspace.
    ///
    /// # Errors
    /// Returns an error when the workspace manifest or semantic relation proof
    /// cannot be reopened and admitted.
    pub fn from_workspace(snapshot: &WorkspaceSnapshot) -> Result<Self, String> {
        snapshot
            .manifest()
            .validate()
            .map_err(|error| error.to_string())?;
        let relation = snapshot
            .relation::<ProductSemanticPublicationRelation>()
            .map_err(|error| error.to_string())?;
        let relation_root = relation.root_node().map_err(|error| error.to_string())?;
        let (_, target) = snapshot
            .transition_relation_roots::<ProductSemanticPublicationRelation>()
            .ok_or_else(|| {
                "selected workspace has no semantic publication transition".to_owned()
            })?;
        if target != *relation.root().as_bytes() {
            return Err(
                "semantic publication transition target differs from selected relation".to_owned(),
            );
        }
        Ok(Self {
            workspace: snapshot.root(),
            relation,
            manifest: Arc::from(snapshot.manifest().encode().into_boxed_slice()),
            authority: *snapshot.manifest().authority(),
            retention: SemanticPublicationRetentionFacts {
                relation_rows: relation_root.row_count(),
                relation_level: relation_root.level(),
                retained_objects: snapshot.closure().manifest().object_count(),
            },
        })
    }

    /// Returns the checked workspace root that selected these publications.
    #[must_use]
    pub const fn workspace_root(&self) -> WorkspaceRoot {
        self.workspace
    }

    /// Returns the checked semantic publication relation root.
    #[must_use]
    pub fn relation_root(&self) -> StateRoot<ProductSemanticPublicationRelation> {
        self.relation.root()
    }

    /// Borrows the owner-held relation handle for bounded page reads.
    #[must_use]
    pub const fn relation(&self) -> &WorkspaceRelationHandle<ProductSemanticPublicationRelation> {
        &self.relation
    }

    /// Returns authenticated arrangement and closure retention facts.
    #[must_use]
    pub const fn retention_facts(&self) -> SemanticPublicationRetentionFacts {
        self.retention
    }

    /// Returns the exact selected workspace manifest bytes.
    #[must_use]
    pub fn manifest_bytes(&self) -> &[u8] {
        &self.manifest
    }

    /// Returns the authority identity admitted by the workspace manifest.
    #[must_use]
    pub const fn authority_bytes(&self) -> &[u8; backend_version::ID_BYTES] {
        &self.authority
    }

    /// Returns the immutable semantic input capability for this relation.
    #[must_use]
    pub fn input(&self) -> SemanticPublicationInput {
        SemanticPublicationInput {
            root: self.relation.root(),
        }
    }
}

/// Immutable execution input derived only from an admitted compiler semantic
/// publication relation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SemanticPublicationInput {
    root: StateRoot<ProductSemanticPublicationRelation>,
}

impl SemanticPublicationInput {
    /// Creates an input capability from an admitted semantic relation state.
    #[must_use]
    pub fn from_checked_relation(
        relation: &RelationState<ProductSemanticPublicationRelation>,
    ) -> Self {
        Self {
            root: relation.root(),
        }
    }

    /// Returns the checked semantic publication relation root.
    #[must_use]
    pub const fn root(self) -> StateRoot<ProductSemanticPublicationRelation> {
        self.root
    }
}

/// Builds a small, authority-covered semantic relation for protocol fixtures.
/// Production composition always opens [`ProductSemanticPublicationSnapshot`]
/// from the workspace owner instead.
pub(super) fn semantic_publication_fixture_with_authority(
    expanded: bool,
    authority: AuthorityVersion,
) -> Result<RelationState<ProductSemanticPublicationRelation>, String> {
    let mut coordinates = vec!["pkg:cargo/backend-builtin@1.0.0"];
    if expanded {
        coordinates.push("pkg:cargo/backend-extra@1.0.0");
    }
    let entries = coordinates
        .into_iter()
        .map(|coordinate| {
            let package = PackageReference::parse(coordinate.to_owned())
                .map_err(|_| "semantic fixture package reference".to_owned())?;
            let coordinate = PackageUrl::parse(coordinate.to_owned())
                .map_err(|_| "semantic fixture compiler coordinate".to_owned())?;
            let key = ProductSemanticPublicationKey::new(
                package,
                coordinate,
                LanguageProfile::Rust(backend_semantic::vocabulary::RustEdition::Rust2024),
            )
            .map_err(str::to_owned)?;
            Ok((
                key,
                ProductSemanticPublicationRecord::Unavailable(
                    SemanticUnavailableReason::ProjectAuthority,
                ),
            ))
        })
        .collect::<Result<Vec<_>, String>>()?;
    RelationState::from_entries(entries, complete_coverage(authority)?)
        .map_err(|error| error.to_string())
}

fn encode_coverage(coverage: SemanticPublicationCoverage, output: &mut Vec<u8>) {
    match coverage {
        SemanticPublicationCoverage::Complete => output.push(1),
        SemanticPublicationCoverage::Partial(partial) => {
            output.push(2);
            output.extend_from_slice(&partial.completed().get().to_be_bytes());
            output.extend_from_slice(&partial.total().get().to_be_bytes());
        }
    }
}

fn decode_published(bytes: &[u8]) -> Result<ProductSemanticPublicationRecord, RelationDecodeError> {
    let (&tag, mut rest) = bytes.split_first().ok_or(RelationDecodeError::Malformed)?;
    let coverage = match tag {
        1 => SemanticPublicationCoverage::Complete,
        2 => {
            let (completed, next) = take_u32(rest)?;
            let (total, next) = take_u32(next)?;
            rest = next;
            let completed = NonZeroU32::new(completed).ok_or(RelationDecodeError::Malformed)?;
            let total = NonZeroU32::new(total).ok_or(RelationDecodeError::Malformed)?;
            SemanticPublicationCoverage::Partial(
                PartialSemanticCoverage::new(completed, total)
                    .map_err(|_| RelationDecodeError::Malformed)?,
            )
        }
        _ => return Err(RelationDecodeError::Malformed),
    };
    let claim = decode_claim(rest)?;
    Ok(ProductSemanticPublicationRecord::Published { coverage, claim })
}

fn encode_claim(claim: SemanticPublicationClaim, output: &mut Vec<u8>) {
    let manifest = claim.manifest();
    let binding = claim.binding();
    output.extend_from_slice(manifest.identity.as_ref());
    output.push(manifest_format_tag(manifest.format));
    output.extend_from_slice(&manifest.fragment_count.to_be_bytes());
    output.extend_from_slice(&manifest.byte_length.to_be_bytes());
    output.extend_from_slice(binding.identity.as_ref());
    encode_generation(binding.generation, output);
    output.extend_from_slice(binding.manifest.as_ref());
}

fn decode_claim(bytes: &[u8]) -> Result<SemanticPublicationClaim, RelationDecodeError> {
    if bytes.len() != CLAIM_BYTES {
        return Err(RelationDecodeError::Malformed);
    }
    let (manifest_identity, rest) = take_identity(bytes)?;
    let (&manifest_format, rest) = rest.split_first().ok_or(RelationDecodeError::Malformed)?;
    let (fragment_count, rest) = take_u32(rest)?;
    let (manifest_length, rest) = take_u32(rest)?;
    let (binding_identity, rest) = take_identity(rest)?;
    let (binding_generation, rest) = take_generation(rest)?;
    let (binding_manifest, rest) = take_identity(rest)?;
    if !rest.is_empty() {
        return Err(RelationDecodeError::Malformed);
    }
    let manifest_identity = CompilationManifestIdentity::try_from(manifest_identity)
        .map_err(|_| RelationDecodeError::Malformed)?;
    SemanticPublicationClaim::admit(
        CompilationManifestFacts {
            identity: manifest_identity,
            format: match manifest_format {
                2 => CompilationManifestFormat::SemanticV2,
                _ => return Err(RelationDecodeError::Malformed),
            },
            fragment_count,
            byte_length: manifest_length,
        },
        CompilationBindingFacts {
            identity: CompilationBindingIdentity::try_from(binding_identity)
                .map_err(|_| RelationDecodeError::Malformed)?,
            generation: binding_generation,
            manifest: CompilationManifestIdentity::try_from(binding_manifest)
                .map_err(|_| RelationDecodeError::Malformed)?,
        },
    )
    .map_err(|_| RelationDecodeError::Malformed)
}

const fn manifest_format_tag(format: CompilationManifestFormat) -> u8 {
    match format {
        CompilationManifestFormat::CompactV1 => 1,
        CompilationManifestFormat::SemanticV2 => 2,
    }
}

fn encode_generation(generation: VerifiedGenerationFacts, output: &mut Vec<u8>) {
    output.extend_from_slice(generation.pinned_root.as_ref());
    output.extend_from_slice(generation.dep_set.as_ref());
}

fn take_generation(bytes: &[u8]) -> Result<(VerifiedGenerationFacts, &[u8]), RelationDecodeError> {
    let (root, rest) = take_identity(bytes)?;
    let (dependencies, rest) = take_identity(rest)?;
    Ok((
        VerifiedGenerationFacts {
            pinned_root: GenerationId::try_from(root)
                .map_err(|_| RelationDecodeError::Malformed)?,
            dep_set: ContentId::<DependencySetDomain>::try_from(dependencies)
                .map_err(|_| RelationDecodeError::Malformed)?,
        },
        rest,
    ))
}

fn push_text(value: &str, output: &mut Vec<u8>) {
    output.extend_from_slice(&u32::try_from(value.len()).unwrap_or(u32::MAX).to_be_bytes());
    output.extend_from_slice(value.as_bytes());
}

fn take_text(bytes: &[u8]) -> Result<(&str, &[u8]), RelationDecodeError> {
    let (length, rest) = take_u32(bytes)?;
    let length = usize::try_from(length).map_err(|_| RelationDecodeError::Malformed)?;
    let (value, rest) = rest
        .split_at_checked(length)
        .ok_or(RelationDecodeError::Malformed)?;
    let value = std::str::from_utf8(value).map_err(|_| RelationDecodeError::Malformed)?;
    Ok((value, rest))
}

fn take_identity(bytes: &[u8]) -> Result<(&[u8], &[u8]), RelationDecodeError> {
    bytes
        .split_at_checked(32)
        .ok_or(RelationDecodeError::Malformed)
}

fn take_u32(bytes: &[u8]) -> Result<(u32, &[u8]), RelationDecodeError> {
    let (value, rest) = bytes
        .split_first_chunk::<4>()
        .ok_or(RelationDecodeError::Malformed)?;
    Ok((u32::from_be_bytes(*value), rest))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::driver::{CompiledFragment, CompiledSemantic};
    use crate::publication::{
        OpenSemanticPublicationScratch, PublishControl, SemanticPublicationScratch,
        manifest::SemanticImageRegion, open_published_semantic, publish_semantic,
    };
    use backend_semantic::ir::{
        AtomId, AtomInput, EntityKind, EntityRecord, FragmentView, IrBuilder, PreparedFragment,
        PrimitiveType, SourceIdentity, TypeId, TypeNode,
    };
    use backend_semantic::vocabulary::{
        CStandard, CompileRecipeFact, CxxStandard, LanguageProfile, NativeTool, RustEdition, Stage,
    };
    use backend_store::journal::{DurablePublisher, PublicationLimits, PublicationPaths};
    use backend_version::{
        IrManifestDomain, IrManifestEncoding, SourceFactDomain, ToolchainDomain,
    };
    use std::{fs, num::NonZeroUsize};

    fn test_error(error: impl std::fmt::Display) -> std::io::Error {
        std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string())
    }

    fn key() -> Result<ProductSemanticPublicationKey, &'static str> {
        ProductSemanticPublicationKey::new(
            PackageReference::parse("pkg:cargo/sample@1.0.0".to_owned())
                .map_err(|_| "package reference")?,
            PackageUrl::parse("pkg:cargo/sample@1.0.0".to_owned())
                .map_err(|_| "compiler coordinate")?,
            LanguageProfile::Rust(RustEdition::Rust2024),
        )
    }

    fn claim() -> Result<SemanticPublicationClaim, &'static str> {
        let generation = VerifiedGenerationFacts {
            pinned_root: GenerationId::from_canonical_bytes(b"generation"),
            dep_set: ContentId::from_canonical_bytes(b"dependency set"),
        };
        let manifest =
            backend_version::ArtifactId::<IrManifestEncoding, IrManifestDomain>::from_encoded_bytes(
                b"manifest",
            );
        let mut binding_bytes = [0_u8; COMPILATION_BINDING_BYTES];
        let binding = CompilationBindingView::write_into(generation, manifest, &mut binding_bytes)
            .map_err(|_| "binding write failed")?;
        SemanticPublicationClaim::admit(
            CompilationManifestFacts {
                identity: manifest,
                format: CompilationManifestFormat::SemanticV2,
                fragment_count: 70_000,
                byte_length: 9_000_000,
            },
            *binding,
        )
    }

    #[test]
    fn key_and_publication_round_trip_without_image_bytes() -> Result<(), &'static str> {
        let key = key()?;
        let value = ProductSemanticPublicationRecord::Published {
            coverage: SemanticPublicationCoverage::Partial(PartialSemanticCoverage::new(
                NonZeroU32::new(70_000).ok_or("zero completed")?,
                NonZeroU32::new(100_000).ok_or("zero total")?,
            )?),
            claim: claim()?,
        };
        let mut key_bytes = Vec::new();
        ProductSemanticPublicationRelation::encode_key(&key, &mut key_bytes);
        let mut value_bytes = Vec::new();
        ProductSemanticPublicationRelation::encode_value(&value, &mut value_bytes);
        let decoded_key = ProductSemanticPublicationRelation::decode_key(&key_bytes)
            .map_err(|_| "key decode failed")?;
        let decoded_value = ProductSemanticPublicationRelation::decode_value(&value_bytes)
            .map_err(|_| "value decode failed")?;
        assert_eq!(decoded_key, key);
        assert_eq!(
            decoded_key.package_key(),
            package_key(key.package().as_str())
        );
        assert_eq!(decoded_key.coordinate().version(), "1.0.0");
        assert_eq!(decoded_key.lineage()?.name, "sample");
        assert_eq!(decoded_value, value);
        assert_eq!(value_bytes.len(), MAGIC.len() + 1 + 1 + 8 + CLAIM_BYTES);
        Ok(())
    }

    #[test]
    fn semantic_key_retains_each_identity_and_rejects_language_rebinding()
    -> Result<(), &'static str> {
        let product = PackageReference::parse("pkg:cargo/acme/sample@1.0.0".to_owned())
            .map_err(|_| "package reference")?;
        let coordinate =
            PackageUrl::parse("pkg:cargo/acme/sample@1.0.0?source=registry#src/lib".to_owned())
                .map_err(|_| "compiler coordinate")?;
        let key = ProductSemanticPublicationKey::new(
            product.clone(),
            coordinate.clone(),
            LanguageProfile::Rust(RustEdition::Rust2024),
        )?;

        assert_eq!(key.package(), &product);
        assert_eq!(key.coordinate(), &coordinate);
        assert_eq!(key.lineage()?.name, "acme/sample");
        assert_eq!(key.coordinate().qualifiers(), Some("source=registry"));
        assert_eq!(key.coordinate().subpath(), Some("src/lib"));
        assert!(
            ProductSemanticPublicationKey::new(
                product,
                coordinate,
                LanguageProfile::TypeScript(
                    backend_semantic::vocabulary::TypeScriptSource::TypeScript
                ),
            )
            .is_err()
        );
        Ok(())
    }

    #[test]
    fn hostile_manifest_or_binding_substitution_is_rejected() -> Result<(), &'static str> {
        let mut encoded = Vec::new();
        ProductSemanticPublicationRelation::encode_value(
            &ProductSemanticPublicationRecord::Published {
                coverage: SemanticPublicationCoverage::Complete,
                claim: claim()?,
            },
            &mut encoded,
        );
        let manifest_identity_offset = MAGIC.len() + 2;
        encoded[manifest_identity_offset] ^= 1;
        assert!(ProductSemanticPublicationRelation::decode_value(&encoded).is_err());

        let mut encoded = Vec::new();
        ProductSemanticPublicationRelation::encode_value(
            &ProductSemanticPublicationRecord::Published {
                coverage: SemanticPublicationCoverage::Complete,
                claim: claim()?,
            },
            &mut encoded,
        );
        let binding_identity_offset = MAGIC.len() + 2 + MANIFEST_BYTES;
        encoded[binding_identity_offset] ^= 1;
        assert!(ProductSemanticPublicationRelation::decode_value(&encoded).is_err());
        assert!(ProductSemanticPublicationRelation::decode_key(&[0; 64]).is_err());
        Ok(())
    }

    #[test]
    fn publication_owner_reopen_is_required_for_activation()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = std::env::temp_dir().join(format!(
            "backend-semantic-activation-{}",
            std::process::id()
        ));
        if directory.exists() {
            fs::remove_dir_all(&directory)?;
        }
        fs::create_dir(&directory)?;
        let publisher = DurablePublisher::create(
            &PublicationPaths::in_directory(&directory.join("journal")),
            PublicationLimits::new(NonZeroUsize::MIN, NonZeroUsize::MIN)?,
        )?;
        let artifacts = directory.join("artifacts");

        let source_bytes = b"pub const READY: bool = true;";
        let source = SourceIdentity {
            identity: ContentId::<SourceFactDomain>::from_canonical_bytes(source_bytes),
            byte_len: u32::try_from(source_bytes.len())?,
        };
        let recipe = CompileRecipeFact::derive(
            LanguageProfile::Rust(RustEdition::Rust2024),
            Stage::LowerIr,
            NativeTool::Rustc,
            source.identity,
            ContentId::<ToolchainDomain>::from_canonical_bytes(b"activation-test-rustc"),
        );
        let entities = [EntityRecord {
            semantic_type: TypeId::new(0),
            name: AtomId::new(0),
            kind: EntityKind::Constant,
        }];
        let types = [TypeNode::Primitive(PrimitiveType::Bool)];
        let atoms = [AtomInput { bytes: b"READY" }];
        let prepared = PreparedFragment::prepare(source, recipe, &entities, &types, &atoms)?;
        let mut fragment_output = [0_u8; 512];
        let fragment = FragmentView::validate(prepared.write_into(&mut fragment_output)?)?;
        let artifact = CompiledFragment {
            source: fragment.source,
            recipe: fragment.recipe,
            fragment,
        };
        let coordinate = PackageUrl::parse("pkg:cargo/activation-fixture@1.0.0".to_owned())
            .map_err(test_error)?;
        let mut builder = IrBuilder::new();
        builder.set_image_provenance_for_package(
            artifact.source,
            artifact.recipe,
            &coordinate,
            "src/lib.rs",
        )?;
        let semantic = CompiledSemantic {
            artifact,
            ir: builder.finish()?,
        };
        let mut manifest_output = [0_u8; 1024];
        let mut manifest_facts = [None; 1];
        let mut ordinals = [0_usize; 1];
        let mut image_plan = [SemanticImageRegion::EMPTY; 1];
        let mut semantic_output = [0_u8; 16_384];
        let mut locality_output = [0_u8; 1024];
        let mut binding_output = [0_u8; COMPILATION_BINDING_BYTES];
        let published = publish_semantic(
            &publisher,
            &artifacts,
            &[semantic],
            PublishControl::Continue,
            SemanticPublicationScratch {
                manifest_output: &mut manifest_output,
                manifest_facts: &mut manifest_facts,
                ordinals: &mut ordinals,
                semantic_image_plan: &mut image_plan,
                semantic_image_output: &mut semantic_output,
                locality_output: &mut locality_output,
                binding_output: &mut binding_output,
            },
        )?;
        let claim = SemanticPublicationClaim::admit(published.manifest, published.binding)?;

        let mut reopened_manifest = [0_u8; 1024];
        let mut reopened_facts = [None; 1];
        let mut reopened_fragment = [0_u8; 512];
        let mut reopened_semantic = [0_u8; 16_384];
        let mut reopened_locality = [0_u8; 1024];
        let opened = open_published_semantic(
            &publisher,
            &artifacts,
            OpenSemanticPublicationScratch {
                manifest_output: &mut reopened_manifest,
                manifest_facts: &mut reopened_facts,
                fragment_output: &mut reopened_fragment,
                semantic_image_output: &mut reopened_semantic,
                locality_output: &mut reopened_locality,
            },
        )?
        .ok_or("missing semantic publication")?;
        {
            let artifact = opened
                .artifacts()
                .next()
                .ok_or("missing semantic artifact")??;
            let matching = ProductSemanticPublicationKey::new(
                PackageReference::parse("activation-fixture".to_owned()).map_err(test_error)?,
                coordinate.clone(),
                LanguageProfile::Rust(RustEdition::Rust2024),
            )?;
            matching.admit_image(&artifact.semantic_image)?;

            for hostile_coordinate in [
                "pkg:cargo/activation-fixture@2.0.0",
                "pkg:cargo/activation-fixture@1.0.0?source=mirror",
                "pkg:cargo/activation-fixture@1.0.0#vendor",
            ] {
                let hostile = ProductSemanticPublicationKey::new(
                    PackageReference::parse("activation-fixture".to_owned()).map_err(test_error)?,
                    PackageUrl::parse(hostile_coordinate.to_owned()).map_err(test_error)?,
                    LanguageProfile::Rust(RustEdition::Rust2024),
                )?;
                assert_eq!(
                    hostile.admit_image(&artifact.semantic_image),
                    Err(SemanticPublicationTargetError::Coordinate)
                );
            }

            let foreign_package = ProductSemanticPublicationKey::new(
                PackageReference::parse("foreign".to_owned()).map_err(test_error)?,
                PackageUrl::parse("pkg:cargo/foreign@1.0.0".to_owned()).map_err(test_error)?,
                LanguageProfile::Rust(RustEdition::Rust2024),
            )?;
            assert_eq!(
                foreign_package.admit_image(&artifact.semantic_image),
                Err(SemanticPublicationTargetError::Package)
            );

            let foreign_profile = ProductSemanticPublicationKey::new(
                PackageReference::parse("activation-fixture".to_owned()).map_err(test_error)?,
                PackageUrl::parse("pkg:cargo/activation-fixture@1.0.0".to_owned())
                    .map_err(test_error)?,
                LanguageProfile::Rust(RustEdition::Rust2021),
            )?;
            assert!(matches!(
                foreign_profile.admit_image(&artifact.semantic_image),
                Err(SemanticPublicationTargetError::Authority { .. })
            ));
        }
        let activated = claim.activate(opened.into_generation())?;
        assert_eq!(activated.artifacts().count(), 1);

        let foreign_manifest =
            backend_version::ArtifactId::<IrManifestEncoding, IrManifestDomain>::from_encoded_bytes(
                b"foreign manifest",
            );
        let mut foreign_binding_bytes = [0_u8; COMPILATION_BINDING_BYTES];
        let foreign_binding = CompilationBindingView::write_into(
            published.binding.generation,
            foreign_manifest,
            &mut foreign_binding_bytes,
        )?;
        let foreign_manifest_claim = SemanticPublicationClaim::admit(
            CompilationManifestFacts {
                identity: foreign_manifest,
                ..published.manifest
            },
            *foreign_binding,
        )?;
        let mut manifest_two = [0_u8; 1024];
        let mut facts_two = [None; 1];
        let mut fragment_two = [0_u8; 512];
        let mut semantic_two = [0_u8; 16_384];
        let mut locality_two = [0_u8; 1024];
        let opened = open_published_semantic(
            &publisher,
            &artifacts,
            OpenSemanticPublicationScratch {
                manifest_output: &mut manifest_two,
                manifest_facts: &mut facts_two,
                fragment_output: &mut fragment_two,
                semantic_image_output: &mut semantic_two,
                locality_output: &mut locality_two,
            },
        )?
        .ok_or("missing semantic publication")?;
        assert!(matches!(
            foreign_manifest_claim.activate(opened.into_generation()),
            Err(SemanticActivationError::Manifest { .. })
        ));

        let foreign_generation = VerifiedGenerationFacts {
            pinned_root: GenerationId::from_canonical_bytes(b"foreign generation"),
            dep_set: published.binding.generation.dep_set,
        };
        let mut foreign_binding_bytes = [0_u8; COMPILATION_BINDING_BYTES];
        let foreign_binding = CompilationBindingView::write_into(
            foreign_generation,
            published.manifest.identity,
            &mut foreign_binding_bytes,
        )?;
        let foreign_binding_claim =
            SemanticPublicationClaim::admit(published.manifest, *foreign_binding)?;
        let mut manifest_three = [0_u8; 1024];
        let mut facts_three = [None; 1];
        let mut fragment_three = [0_u8; 512];
        let mut semantic_three = [0_u8; 16_384];
        let mut locality_three = [0_u8; 1024];
        let opened = open_published_semantic(
            &publisher,
            &artifacts,
            OpenSemanticPublicationScratch {
                manifest_output: &mut manifest_three,
                manifest_facts: &mut facts_three,
                fragment_output: &mut fragment_three,
                semantic_image_output: &mut semantic_three,
                locality_output: &mut locality_three,
            },
        )?
        .ok_or("missing semantic publication")?;
        assert!(matches!(
            foreign_binding_claim.activate(opened.into_generation()),
            Err(SemanticActivationError::Binding { .. })
        ));

        publisher.shutdown()?;
        fs::remove_dir_all(directory)?;
        Ok(())
    }

    #[test]
    fn partial_coverage_has_one_canonical_representation() {
        let one = NonZeroU32::MIN;
        let two = NonZeroU32::new(2).unwrap_or(NonZeroU32::MIN);
        assert!(PartialSemanticCoverage::new(one, two).is_ok());
        assert!(PartialSemanticCoverage::new(two, two).is_err());
    }

    #[test]
    fn exact_coordinate_and_profile_are_retained_in_the_canonical_key() -> Result<(), String> {
        let package = PackageReference::parse("pkg:generic/acme/widget@1.0.0".to_owned())
            .map_err(|error| error.to_string())?;
        let coordinate = PackageUrl::parse(
            "pkg:generic/acme/widget@1.0.0?source=compile_commands#src/widget.c".to_owned(),
        )
        .map_err(|error| error.to_string())?;
        let c = ProductSemanticPublicationKey::new(
            package.clone(),
            coordinate.clone(),
            LanguageProfile::C(CStandard::C17),
        )
        .map_err(str::to_owned)?;
        let cxx = ProductSemanticPublicationKey::new(
            package.clone(),
            coordinate.clone(),
            LanguageProfile::Cxx(CxxStandard::Cxx20),
        )
        .map_err(str::to_owned)?;
        let qualified = ProductSemanticPublicationKey::new(
            package,
            PackageUrl::parse(
                "pkg:generic/acme/widget@1.0.0?source=archive#src/widget.c".to_owned(),
            )
            .map_err(|error| error.to_string())?,
            LanguageProfile::C(CStandard::C17),
        )
        .map_err(str::to_owned)?;

        let mut c_bytes = Vec::new();
        ProductSemanticPublicationRelation::encode_key(&c, &mut c_bytes);
        let mut cxx_bytes = Vec::new();
        ProductSemanticPublicationRelation::encode_key(&cxx, &mut cxx_bytes);
        let mut qualified_bytes = Vec::new();
        ProductSemanticPublicationRelation::encode_key(&qualified, &mut qualified_bytes);

        assert_ne!(
            c_bytes, cxx_bytes,
            "C and C++ profiles must never share a key"
        );
        assert_ne!(
            c_bytes, qualified_bytes,
            "PURL qualifiers must affect the key"
        );
        assert_eq!(
            ProductSemanticPublicationRelation::decode_key(&c_bytes)
                .map_err(|error| error.to_string())?,
            c
        );
        assert_eq!(
            ProductSemanticPublicationRelation::decode_key(&qualified_bytes)
                .map_err(|error| error.to_string())?,
            qualified
        );
        Ok(())
    }

    #[test]
    fn same_target_generations_are_retained_and_selected_exactly() -> Result<(), String> {
        let selected_key = key().map_err(str::to_owned)?;
        let first = claim().map_err(str::to_owned)?;
        let second_generation = VerifiedGenerationFacts {
            pinned_root: GenerationId::from_canonical_bytes(b"second generation"),
            dep_set: first.binding().generation.dep_set,
        };
        let mut binding_bytes = [0_u8; COMPILATION_BINDING_BYTES];
        let second_binding = CompilationBindingView::write_into(
            second_generation,
            first.manifest().identity,
            &mut binding_bytes,
        )
        .map_err(|error| error.to_string())?;
        let second = SemanticPublicationClaim::admit(first.manifest(), *second_binding)
            .map_err(str::to_owned)?;
        let first_value = ProductSemanticPublicationRecord::Published {
            coverage: SemanticPublicationCoverage::Complete,
            claim: first,
        };
        let second_value = ProductSemanticPublicationRecord::Published {
            coverage: SemanticPublicationCoverage::Complete,
            claim: second,
        };
        let first_key = selected_key.for_generation(first.binding().identity);
        let second_key = selected_key.for_generation(second.binding().identity);
        first_key
            .admit_record(&first_value)
            .map_err(|error| error.to_string())?;
        second_key
            .admit_record(&second_value)
            .map_err(|error| error.to_string())?;

        let relation = RelationState::<ProductSemanticPublicationRelation>::from_entries(
            [
                (selected_key.clone(), second_value.clone()),
                (first_key.clone(), first_value.clone()),
                (second_key.clone(), second_value.clone()),
            ],
            complete_coverage(AuthorityVersion::from_value(b"semantic history authority"))?,
        )
        .map_err(|error| error.to_string())?;
        assert_eq!(relation.get(&selected_key), Some(&second_value));
        assert_eq!(relation.get(&first_key), Some(&first_value));
        assert_eq!(relation.get(&second_key), Some(&second_value));
        assert!(selected_key.is_selected());
        assert!(!first_key.is_selected());

        let mut first_key = Vec::new();
        let mut second_key = Vec::new();
        ProductSemanticPublicationRelation::encode_key(
            relation
                .iter()
                .find_map(|(key, value)| (value == &first_value).then_some(key))
                .ok_or_else(|| "missing first semantic history row".to_owned())?,
            &mut first_key,
        );
        ProductSemanticPublicationRelation::encode_key(
            relation
                .iter()
                .find_map(|(key, value)| {
                    (!key.is_selected() && value == &second_value).then_some(key)
                })
                .ok_or_else(|| "missing second semantic history row".to_owned())?,
            &mut second_key,
        );
        assert_ne!(first_key, second_key);
        let decoded_first = ProductSemanticPublicationRelation::decode_key(&first_key)
            .map_err(|error| error.to_string())?;
        assert_eq!(
            decoded_first.selection(),
            SemanticPublicationSelection::Generation(first.binding().identity)
        );

        assert!(matches!(
            decoded_first.admit_record(&second_value),
            Err(SemanticPublicationSelectionError::Binding { .. })
        ));
        assert_eq!(
            decoded_first.admit_record(&ProductSemanticPublicationRecord::Unavailable(
                SemanticUnavailableReason::Rejected,
            )),
            Err(SemanticPublicationSelectionError::GenerationUnavailable)
        );
        Ok(())
    }

    #[test]
    fn same_target_selection_survives_durable_relation_reopen() -> Result<(), String> {
        let selected_key = key().map_err(str::to_owned)?;
        let first = claim().map_err(str::to_owned)?;
        let second_generation = VerifiedGenerationFacts {
            pinned_root: GenerationId::from_canonical_bytes(b"durable second generation"),
            dep_set: first.binding().generation.dep_set,
        };
        let mut binding_bytes = [0_u8; COMPILATION_BINDING_BYTES];
        let second_binding = CompilationBindingView::write_into(
            second_generation,
            first.manifest().identity,
            &mut binding_bytes,
        )
        .map_err(|error| error.to_string())?;
        let second = SemanticPublicationClaim::admit(first.manifest(), *second_binding)
            .map_err(str::to_owned)?;
        let first_value = ProductSemanticPublicationRecord::Published {
            coverage: SemanticPublicationCoverage::Complete,
            claim: first,
        };
        let second_value = ProductSemanticPublicationRecord::Published {
            coverage: SemanticPublicationCoverage::Complete,
            claim: second,
        };
        let first_key = selected_key.for_generation(first.binding().identity);
        let second_key = selected_key.for_generation(second.binding().identity);
        let authority = AuthorityVersion::from_value(b"durable semantic history authority");
        let relation = RelationState::<ProductSemanticPublicationRelation>::from_entries(
            [
                (selected_key.clone(), second_value.clone()),
                (first_key.clone(), first_value.clone()),
                (second_key.clone(), second_value.clone()),
            ],
            complete_coverage(authority).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;

        let directory = std::env::temp_dir().join(format!(
            "backend-semantic-history-reopen-{}-{}",
            std::process::id(),
            second.binding().identity.as_ref()[..8]
                .iter()
                .fold(0_u64, |value, byte| value * 256 + u64::from(*byte)),
        ));
        let _ = fs::remove_dir_all(&directory);
        let registry = backend_store::RelationAdmissionRegistry::default()
            .with_relation::<ProductSemanticPublicationRelation>()
            .map_err(|error| format!("register semantic relation: {error:?}"))?;
        let store = backend_store::FileStore::open_with_registry(
            &directory,
            8 * 1024 * 1024,
            registry.clone(),
        )
        .map_err(|error| format!("open history store: {error:?}"))?;
        store
            .write_relation_state(&relation)
            .map_err(|error| format!("write history relation: {error:?}"))?;
        drop(store);

        let reopened = Arc::new(
            backend_store::FileStore::open_with_registry(&directory, 8 * 1024 * 1024, registry)
                .map_err(|error| format!("reopen history store: {error:?}"))?,
        );
        let handle = WorkspaceRelationHandle::<ProductSemanticPublicationRelation>::open(
            Arc::clone(&reopened),
            relation.root().to_bytes(),
        )
        .map_err(|error| format!("open history relation: {error:?}"))?;
        assert_eq!(
            handle
                .lookup(&selected_key)
                .map_err(|error| format!("selected lookup: {error:?}"))?,
            Some(second_value.clone())
        );
        assert_eq!(
            handle
                .lookup(&first_key)
                .map_err(|error| format!("first lookup: {error:?}"))?,
            Some(first_value.clone())
        );
        assert_eq!(
            handle
                .lookup(&second_key)
                .map_err(|error| format!("second lookup: {error:?}"))?,
            Some(second_value.clone())
        );
        assert_eq!(
            handle
                .page(None, 8)
                .map_err(|error| format!("history page: {error:?}"))?
                .entries()
                .len(),
            3
        );

        let unknown = ProductSemanticPublicationKey::new(
            PackageReference::parse("pkg:cargo/unknown@1.0.0".to_owned())
                .map_err(|_| "unknown package")?,
            PackageUrl::parse("pkg:cargo/unknown@1.0.0".to_owned())
                .map_err(|_| "unknown coordinate")?,
            LanguageProfile::Rust(RustEdition::Rust2024),
        )?;
        assert!(
            handle
                .lookup(&unknown)
                .map_err(|error| format!("unknown lookup: {error:?}"))?
                .is_none()
        );
        assert!(matches!(
            first_key.admit_record(&second_value),
            Err(SemanticPublicationSelectionError::Binding { .. })
        ));
        assert_eq!(
            first_key.admit_record(&ProductSemanticPublicationRecord::Unavailable(
                SemanticUnavailableReason::Rejected,
            )),
            Err(SemanticPublicationSelectionError::GenerationUnavailable)
        );
        drop(reopened);
        fs::remove_dir_all(directory).map_err(|error| error.to_string())?;
        Ok(())
    }

    #[test]
    fn unavailable_authority_causes_are_closed_and_trailing_bytes_rejected() {
        for reason in [
            SemanticUnavailableReason::Toolchain,
            SemanticUnavailableReason::ProjectAuthority,
            SemanticUnavailableReason::Cancelled,
            SemanticUnavailableReason::Rejected,
        ] {
            let value = ProductSemanticPublicationRecord::Unavailable(reason);
            let mut bytes = Vec::new();
            ProductSemanticPublicationRelation::encode_value(&value, &mut bytes);
            assert_eq!(
                ProductSemanticPublicationRelation::decode_value(&bytes),
                Ok(value)
            );
            bytes.push(0);
            assert!(ProductSemanticPublicationRelation::decode_value(&bytes).is_err());
        }
    }
}
