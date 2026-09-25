//! Typed owned/compact/reopened observations and their domain-separated digests.
//!
//! This boundary reads immutable IR authority columns directly. It preserves
//! captured-empty planes and observer-unavailable planes as distinct values.

use super::*;

use backend_semantic::ir::{DeclarationIdentity, EntityId, ExternalId, TypeId};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum PlaneObservation {
    Captured,
    Unavailable,
    /// The current observer API cannot inspect this plane. This is not a
    /// semantic absence and is never accepted by a source-parity matcher.
    ObserverUnavailable,
    Unsupported,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum CountObservation {
    Exact(u32),
    Unavailable,
    Unsupported,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) enum ObservedTypeShape {
    Absent,
    Primitive(BuiltinType),
    Literal,
    Callable,
    Nominal,
    Structural,
    Reference,
    Generic,
    Computed,
    Unknown,
    Other,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct VersionObservation {
    pub(super) family: DeclarationFamilyId,
    pub(super) variant: VariantFingerprint,
    pub(super) core_payload: backend_semantic::ir::CorePayloadHash,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct OwnedObservation {
    pub(super) entity_count: u32,
    pub(super) image_provenance: ImageProvenance,
    pub(super) primary_matches: u16,
    pub(super) primary: Option<EntityObservation>,
    pub(super) declarations: u32,
    pub(super) generic_parameters: CountObservation,
    pub(super) documentation: PlaneObservation,
    pub(super) attributes: PlaneObservation,
    pub(super) source_plane: PlaneObservation,
    pub(super) occurrences: CountObservation,
    pub(super) links: CountObservation,
    pub(super) extension: PlaneObservation,
    pub(super) semantic_digest: Digest,
    /// Complete reader observation over the same finalized image.  This is
    /// kept beside the legacy compact-oriented fields so the source matrix
    /// can compare every canonical row and pooled plane without recovering
    /// facts from cardinality.
    pub(super) semantic: SemanticObservation,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum SemanticReaderStatus {
    Captured,
    ObserverUnavailable,
    Unsupported,
}

/// Exact, allocation-free semantic-image facts reduced to typed values and
/// domain-separated content digests.  The digest fields cover canonical row
/// order and all borrowed payloads; the census retains independently checked
/// counts and authority availability distributions.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct SemanticObservation {
    pub(super) status: SemanticReaderStatus,
    pub(super) identity: Option<backend_semantic::ir::SemanticImageIdentity>,
    pub(super) image: Option<ObservedImageFacts>,
    pub(super) census: Option<ObservedImageCensus>,
    pub(super) entities: Digest,
    pub(super) types: Digest,
    pub(super) externals: Digest,
    pub(super) links: Digest,
    pub(super) occurrences: Digest,
    pub(super) extensions: [Digest; 7],
    /// Source files and half-open spans in canonical declaration order.
    /// File atom bytes are included instead of transient atom coordinates.
    pub(super) source_spans: Digest,
    pub(super) canonical_type: RenderVerdict,
}

/// Scope atoms resolved to their exact bytes.  An owned image numbers atoms
/// in admission order while a reopened image numbers them in canonical wire
/// order, so scope coordinates are staging artifacts: only the bytes they
/// name may enter an owned-vs-reopened comparison.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ObservedScopeBytes {
    pub(super) ecosystem: Vec<u8>,
    pub(super) package: Vec<u8>,
    pub(super) path: Vec<u8>,
    pub(super) coordinate: Option<Vec<u8>>,
}

/// Image provenance with scope atoms resolved to content.  Every other cell
/// is already coordinate-free, so this enum compares equal across the
/// owned/reopened boundary exactly when the image proves the same
/// source, recipe, claim, and scope.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum ObservedImageProvenance {
    Unavailable,
    Captured {
        source: SourceIdentity,
        recipe: backend_semantic::vocabulary::CompileRecipeFact,
        claim: backend_semantic::ir::SemanticScopeClaim,
        scope: ObservedScopeBytes,
    },
}

/// Canonical image facts for owned-vs-reopened comparison.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ObservedImageFacts {
    pub(super) authority: backend_semantic::ir::SemanticImageAuthority,
    pub(super) provenance: ObservedImageProvenance,
}

/// Canonical image census: the product census with its embedded image facts
/// replaced by their canonical projection.  Every other cell is already
/// plain content (counts and availability distributions).
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ObservedImageCensus {
    pub(super) image: ObservedImageFacts,
    pub(super) entities: usize,
    pub(super) root_entities: usize,
    pub(super) typed_entities: usize,
    pub(super) source_bound_entities: usize,
    pub(super) member_references: usize,
    pub(super) documentation_fragments: usize,
    pub(super) attribute_references: usize,
    pub(super) types: usize,
    pub(super) external_targets: usize,
    pub(super) links: usize,
    pub(super) link_occurrences: usize,
    pub(super) occurrence_source_authority: backend_semantic::ir::AvailabilityCensus,
    pub(super) language_extensions: backend_semantic::ir::LanguageExtensionCensus,
    pub(super) entity_authority: backend_semantic::ir::EntityAuthorityCensus,
}

fn observed_scope_atom<R: backend_semantic::ir::SemanticReader + ?Sized>(
    reader: &R,
    id: backend_semantic::ir::AtomId,
) -> Vec<u8> {
    reader.atom(id).unwrap_or_default().to_vec()
}

/// Observes canonical image facts through one reader, resolving scope atoms
/// to their exact bytes.
fn observe_image_facts<R: backend_semantic::ir::SemanticReader + ?Sized>(
    reader: &R,
) -> ObservedImageFacts {
    let facts = reader.image_facts();
    let provenance = match facts.provenance {
        backend_semantic::ir::ImageProvenance::Unavailable => ObservedImageProvenance::Unavailable,
        backend_semantic::ir::ImageProvenance::Captured {
            source,
            recipe,
            claim,
            scope,
        } => ObservedImageProvenance::Captured {
            source,
            recipe,
            claim,
            scope: ObservedScopeBytes {
                ecosystem: observed_scope_atom(reader, scope.ecosystem),
                package: observed_scope_atom(reader, scope.package),
                path: observed_scope_atom(reader, scope.path),
                coordinate: scope
                    .coordinate
                    .map(|atom| observed_scope_atom(reader, atom)),
            },
        },
    };
    ObservedImageFacts {
        authority: facts.authority,
        provenance,
    }
}

/// Observes the canonical image census through one reader.
fn observe_image_census<R: backend_semantic::ir::SemanticReader + ?Sized>(
    reader: &R,
    image: &ObservedImageFacts,
) -> Option<ObservedImageCensus> {
    backend_semantic::ir::SemanticImageDiscovery::new(reader)
        .census()
        .ok()
        .map(|census| ObservedImageCensus {
            image: image.clone(),
            entities: census.entities,
            root_entities: census.root_entities,
            typed_entities: census.typed_entities,
            source_bound_entities: census.source_bound_entities,
            member_references: census.member_references,
            documentation_fragments: census.documentation_fragments,
            attribute_references: census.attribute_references,
            types: census.types,
            external_targets: census.external_targets,
            links: census.links,
            link_occurrences: census.link_occurrences,
            occurrence_source_authority: census.occurrence_source_authority,
            language_extensions: census.language_extensions,
            entity_authority: census.entity_authority,
        })
}

pub(super) fn digest_observed_provenance(value: &ObservedImageProvenance) -> Digest {
    let mut hasher = StableHasher::default();
    hash_observed_provenance(value, &mut hasher);
    hasher.digest()
}

fn hash_observed_provenance(value: &ObservedImageProvenance, hasher: &mut StableHasher) {
    match value {
        ObservedImageProvenance::Unavailable => 0_u8.hash(hasher),
        ObservedImageProvenance::Captured {
            source,
            recipe,
            claim,
            scope,
        } => {
            1_u8.hash(hasher);
            hash_source_identity(*source, hasher);
            digest_recipe(*recipe).hash(hasher);
            hash_scope_claim(*claim, hasher);
            scope.ecosystem.hash(hasher);
            scope.package.hash(hasher);
            scope.path.hash(hasher);
            scope.coordinate.hash(hasher);
        }
    }
}

fn hash_source_identity(value: SourceIdentity, hasher: &mut StableHasher) {
    value.identity.as_ref().hash(hasher);
    value.byte_len.hash(hasher);
}

fn hash_scope_claim(value: backend_semantic::ir::SemanticScopeClaim, hasher: &mut StableHasher) {
    value.identity.as_ref().hash(hasher);
}

fn hash_observed_image(value: &ObservedImageFacts, hasher: &mut StableHasher) {
    hash_image_authority(value.authority, hasher);
    hash_observed_provenance(&value.provenance, hasher);
}

fn hash_image_authority(
    value: backend_semantic::ir::SemanticImageAuthority,
    hasher: &mut StableHasher,
) {
    match value {
        backend_semantic::ir::SemanticImageAuthority::Shared => 0_u8.hash(hasher),
        backend_semantic::ir::SemanticImageAuthority::Language(profile) => {
            1_u8.hash(hasher);
            digest_typed(&profile).hash(hasher);
        }
    }
}

fn hash_observed_census(value: &ObservedImageCensus, hasher: &mut StableHasher) {
    hash_observed_image(&value.image, hasher);
    value.entities.hash(hasher);
    value.root_entities.hash(hasher);
    value.typed_entities.hash(hasher);
    value.source_bound_entities.hash(hasher);
    value.member_references.hash(hasher);
    value.documentation_fragments.hash(hasher);
    value.attribute_references.hash(hasher);
    value.types.hash(hasher);
    value.external_targets.hash(hasher);
    value.links.hash(hasher);
    value.link_occurrences.hash(hasher);
    hash_availability_census(value.occurrence_source_authority, hasher);
    value.language_extensions.typescript.hash(hasher);
    value.language_extensions.csharp.hash(hasher);
    value.language_extensions.go.hash(hasher);
    value.language_extensions.rust.hash(hasher);
    value.language_extensions.python.hash(hasher);
    value.language_extensions.java.hash(hasher);
    value.language_extensions.clang.hash(hasher);
    let authority = value.entity_authority;
    authority.parentage.unavailable.hash(hasher);
    authority.parentage.roots.hash(hasher);
    authority.parentage.bound.hash(hasher);
    authority
        .parentage
        .unrepresented_authority_owner
        .hash(hasher);
    hash_availability_census(authority.source, hasher);
    hash_availability_census(authority.source_file, hasher);
    hash_availability_census(authority.members, hasher);
    hash_availability_census(authority.semantic_type, hasher);
    hash_availability_census(authority.documentation, hasher);
    hash_availability_census(authority.visibility, hasher);
    hash_availability_census(authority.attributes, hasher);
    hash_availability_census(authority.language_extension, hasher);
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct EntityObservation {
    pub(super) id: EntityId,
    pub(super) kind: ItemKind,
    pub(super) name: Digest,
    pub(super) visibility: Visibility,
    pub(super) parent: Option<EntityId>,
    pub(super) authority: Option<EntityAuthorityFacts>,
    pub(super) members: u32,
    pub(super) member_order: Digest,
    pub(super) first_member: Option<Digest>,
    pub(super) type_shape: ObservedTypeShape,
    pub(super) source: Option<SourceSpan>,
    pub(super) version: VersionObservation,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct CompactObservation {
    pub(super) fragment: Digest,
    pub(super) census: backend_semantic::ir::SemanticCensus,
    pub(super) primary_matches: u16,
    pub(super) primary_kind: Option<EntityKind>,
    pub(super) primary_name: Digest,
    pub(super) primary_type: Option<TypeNode>,
    pub(super) semantic_data: PlaneObservation,
    pub(super) occurrences: PlaneObservation,
    pub(super) type_facts: PlaneObservation,
    pub(super) documentation: PlaneObservation,
    pub(super) extensions: PlaneObservation,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ReopenedObservation {
    pub(super) fragment: Digest,
    pub(super) source: SourceIdentity,
    pub(super) recipe: backend_semantic::vocabulary::CompileRecipeFact,
    pub(super) ranges: Digest,
    pub(super) census: backend_semantic::ir::SemanticCensus,
    pub(super) semantic_data: PlaneObservation,
    pub(super) occurrences: PlaneObservation,
    pub(super) type_facts: PlaneObservation,
    pub(super) documentation: PlaneObservation,
    pub(super) extensions: PlaneObservation,
    pub(super) semantic: SemanticObservation,
}

/// Optional semantic planes available from the reopened representation.  The
/// current publication API exposes only the compact fragment envelope, so its
/// adapter returns explicit observer-unavailable values.  Terra's forthcoming
/// `SemanticImageView` is intentionally the one narrow implementation point
/// that will replace this constructor; no comparison code may infer a plane
/// from census counts or section presence.
pub(super) fn observer_unavailable_semantic() -> SemanticObservation {
    SemanticObservation {
        status: SemanticReaderStatus::ObserverUnavailable,
        identity: None,
        image: None,
        census: None,
        entities: [0; 32],
        types: [0; 32],
        externals: [0; 32],
        links: [0; 32],
        occurrences: [0; 32],
        extensions: [[0; 32]; 7],
        source_spans: [0; 32],
        canonical_type: RenderVerdict::Unavailable,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum RenderVerdict {
    Rendered(Digest),
    Unsupported,
    Unavailable,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct CaseOutputObservation {
    pub(super) source: SourceIdentity,
    pub(super) recipe: backend_semantic::vocabulary::CompileRecipeFact,
    pub(super) owned: OwnedObservation,
    pub(super) compact: CompactObservation,
    pub(super) reopened: ReopenedObservation,
    pub(super) neutral_render: RenderVerdict,
    pub(super) dialect_render: RenderVerdict,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum CaseObservation {
    Output(CaseOutputObservation),
    LocallyUnavailable {
        key: CaseKey,
        cause: AuthorityUnavailableCause,
    },
    Terminal {
        key: CaseKey,
        source: SourceIdentity,
        terminal: CompileTerminalKind,
    },
}

/// Deterministic typed-observation hasher. The derive-key domain prevents
/// these audit digests from being mistaken for source/content identities, and
/// the streaming state keeps an arbitrarily large semantic image out of a
/// second unbounded staging buffer.
const OBSERVATION_HASH_KEY: &str = "nudox.compiler-corpus.observation.v1";

pub(super) struct StableHasher(blake3::Hasher);

impl Default for StableHasher {
    fn default() -> Self {
        Self(blake3::Hasher::new_derive_key(OBSERVATION_HASH_KEY))
    }
}

impl Hasher for StableHasher {
    fn finish(&self) -> u64 {
        let digest = self.0.clone().finalize();
        let mut bytes = [0_u8; 8];
        bytes.copy_from_slice(&digest.as_bytes()[..8]);
        u64::from_le_bytes(bytes)
    }

    fn write(&mut self, bytes: &[u8]) {
        self.0.update(bytes);
    }
}

impl StableHasher {
    fn digest(self) -> Digest {
        *self.0.finalize().as_bytes()
    }
}

pub(super) fn digest_u64(value: u64) -> Digest {
    let mut hasher = StableHasher::default();
    hasher.write(&value.to_le_bytes());
    hasher.digest()
}

pub(super) fn digest_typed<T: Hash>(value: &T) -> Digest {
    let mut hasher = StableHasher::default();
    value.hash(&mut hasher);
    hasher.digest()
}

pub(super) fn digest_entity_observation(value: Option<EntityObservation>) -> Digest {
    let mut hasher = StableHasher::default();
    hash_entity_observation(value, &mut hasher);
    hasher.digest()
}

pub(super) fn digest_fact_availability(value: FactAvailability) -> Digest {
    let mut hasher = StableHasher::default();
    hash_fact_availability(value, &mut hasher);
    hasher.digest()
}

pub(super) fn digest_image_provenance(value: ImageProvenance) -> Digest {
    let mut hasher = StableHasher::default();
    hash_image_provenance(value, &mut hasher);
    hasher.digest()
}

pub(super) fn digest_semantic_census(value: Option<&ObservedImageCensus>) -> Digest {
    let mut hasher = StableHasher::default();
    match value {
        Some(value) => {
            1_u8.hash(&mut hasher);
            hash_observed_census(value, &mut hasher);
        }
        None => 0_u8.hash(&mut hasher),
    }
    hasher.digest()
}

pub(super) fn digest_type_shape(value: ObservedTypeShape) -> Digest {
    let mut hasher = StableHasher::default();
    hash_type_shape(value, &mut hasher);
    hasher.digest()
}

pub(super) fn digest_bytes(bytes: &[u8]) -> Digest {
    let mut hasher = StableHasher::default();
    hasher.write(bytes);
    hasher.digest()
}

pub(super) fn range_manifest_digest(view: &FragmentView<'_>) -> Result<Digest, CorpusAuditError> {
    Ok(range_manifest_digest_from_manifest(
        FragmentRangeManifest::from_view(view)?,
    ))
}

pub(super) fn range_manifest_digest_from_manifest(ranges: FragmentRangeManifest) -> Digest {
    let mut hasher = StableHasher::default();
    ranges.fragment.hash(&mut hasher);
    ranges.fragment_length.hash(&mut hasher);
    ranges.source.hash_into(&mut hasher);
    ranges.recipe.identity.hash(&mut hasher);
    ranges.recipe.profile.hash(&mut hasher);
    u8::from(ranges.recipe.stage).hash(&mut hasher);
    u8::from(ranges.recipe.tool).hash(&mut hasher);
    ranges.recipe.toolchain.hash(&mut hasher);
    for range in ranges.ranges {
        u16::from(range.section).hash(&mut hasher);
        range.offset.hash(&mut hasher);
        range.length.hash(&mut hasher);
        range.identity.hash(&mut hasher);
    }
    hasher.digest()
}

pub(super) trait SourceIdentityHash {
    fn hash_into(&self, hasher: &mut StableHasher);
}

impl SourceIdentityHash for SourceIdentity {
    fn hash_into(&self, hasher: &mut StableHasher) {
        self.identity.hash(hasher);
        self.byte_len.hash(hasher);
    }
}

pub(super) fn item_kind(kind: EntityKind) -> ItemKind {
    match kind {
        EntityKind::Function => ItemKind::Function,
        EntityKind::Constant => ItemKind::Constant,
        EntityKind::Record => ItemKind::Record,
        EntityKind::Module => ItemKind::Module,
        EntityKind::Field => ItemKind::Field,
        EntityKind::Alias => ItemKind::TypeAlias,
        EntityKind::Trait => ItemKind::Trait,
        EntityKind::Implementation => ItemKind::Implementation,
        EntityKind::Enum => ItemKind::Enum,
        EntityKind::Variant => ItemKind::Variant,
        EntityKind::Static => ItemKind::Static,
        EntityKind::Reexport => ItemKind::Reexport,
        EntityKind::Parameter => ItemKind::Parameter,
        EntityKind::Macro | EntityKind::Namespace => kind,
    }
}

pub(super) fn version_observation(
    version: backend_semantic::ir::EntityVersion,
) -> VersionObservation {
    VersionObservation {
        family: version.family,
        variant: version.variant,
        core_payload: version.core_payload,
    }
}

fn member_order_digest(ir: &Ir, members: &[EntityId]) -> Digest {
    let mut hasher = StableHasher::default();
    for member in members {
        member.raw.hash(&mut hasher);
        if let Some(item) = ir.item(*member) {
            item.kind().hash(&mut hasher);
            item.name().hash(&mut hasher);
        }
    }
    hasher.digest()
}

fn type_shape(ir: &Ir, ty: Option<TypeId>) -> ObservedTypeShape {
    let Some(expression) = ty.and_then(|id| ir.ty(id)) else {
        return ObservedTypeShape::Absent;
    };
    match expression {
        TypeExpr::Unknown(_) => ObservedTypeShape::Unknown,
        TypeExpr::Computed(_) => ObservedTypeShape::Computed,
        TypeExpr::Concrete(ConcreteType::Builtin(builtin)) => ObservedTypeShape::Primitive(builtin),
        TypeExpr::Concrete(ConcreteType::Literal(_)) => ObservedTypeShape::Literal,
        TypeExpr::Concrete(ConcreteType::Function { .. }) => ObservedTypeShape::Callable,
        TypeExpr::Concrete(ConcreteType::Nominal(_))
        | TypeExpr::Concrete(ConcreteType::External(_)) => ObservedTypeShape::Nominal,
        TypeExpr::Concrete(ConcreteType::Reference { .. }) => ObservedTypeShape::Reference,
        TypeExpr::Concrete(ConcreteType::Parameter(_))
        | TypeExpr::Concrete(ConcreteType::Applied { .. })
        | TypeExpr::Concrete(ConcreteType::ImplTrait(_))
        | TypeExpr::Concrete(ConcreteType::DynTrait(_)) => ObservedTypeShape::Generic,
        TypeExpr::Concrete(
            ConcreteType::Tuple(_)
            | ConcreteType::Object(_)
            | ConcreteType::CxxReference { .. }
            | ConcreteType::CPointer { .. }
            | ConcreteType::CxxMemberPointer { .. }
            | ConcreteType::CQualified { .. }
            | ConcreteType::CBlockPointer { .. }
            | ConcreteType::NativeCharacter { .. }
            | ConcreteType::Pointer { .. }
            | ConcreteType::Slice(_)
            | ConcreteType::Array { .. }
            | ConcreteType::Optional(_)
            | ConcreteType::Union(_)
            | ConcreteType::Intersection(_)
            | ConcreteType::Wildcard(_)
            | ConcreteType::Annotated { .. }
            | ConcreteType::Inferred(_)
            | ConcreteType::QualifiedPath { .. }
            | ConcreteType::Map { .. }
            | ConcreteType::Channel { .. },
        ) => ObservedTypeShape::Structural,
    }
}

fn generic_parameters(ir: &Ir, language: CorpusLanguage, entity: EntityId) -> CountObservation {
    let extensions = ir.language_extensions();
    let count = match language {
        CorpusLanguage::TypeScript => extensions
            .typescript
            .get(entity)
            .and_then(|facts| ir.type_parameters(facts.type_parameters))
            .map(|parameters| parameters.len()),
        CorpusLanguage::CSharp => extensions
            .csharp
            .get(entity)
            .and_then(|facts| ir.type_parameters(facts.constraints))
            .map(|parameters| parameters.len()),
        CorpusLanguage::Go => extensions
            .go
            .get(entity)
            .and_then(|facts| ir.type_parameters(facts.type_parameters))
            .map(|parameters| parameters.len()),
        CorpusLanguage::Rust => extensions
            .rust
            .get(entity)
            .and_then(|facts| ir.type_parameters(facts.where_clauses))
            .map(|parameters| parameters.len()),
        CorpusLanguage::Clang => extensions
            .clang
            .get(entity)
            .and_then(|facts| ir.type_parameters(facts.templates))
            .map(|parameters| parameters.len()),
        CorpusLanguage::Python | CorpusLanguage::Java => None,
    };
    count.map_or(CountObservation::Unavailable, |count| {
        CountObservation::Exact(u32::try_from(count).unwrap_or(u32::MAX))
    })
}

fn semantic_digest(ir: &Ir) -> Digest {
    let mut hasher = StableHasher::default();
    ir.entity_count().hash(&mut hasher);
    let columns = ir.storage_columns();
    columns.types.headers.hash(&mut hasher);
    columns.types.pairs.hash(&mut hasher);
    columns.types.triples.hash(&mut hasher);
    columns.types.quads.hash(&mut hasher);
    columns.vcs.versions.hash(&mut hasher);
    for item in ir.items() {
        item.id().raw.hash(&mut hasher);
        item.kind().hash(&mut hasher);
        item.visibility().hash(&mut hasher);
        item.parent().hash(&mut hasher);
        item.semantic_type().hash(&mut hasher);
        item.name().hash(&mut hasher);
        item.members().hash(&mut hasher);
        item.docs().hash(&mut hasher);
        item.attributes().hash(&mut hasher);
        item.source().hash(&mut hasher);
        item.version().hash(&mut hasher);
        for (link_id, link) in item.links_from() {
            link_id.hash(&mut hasher);
            link.hash(&mut hasher);
        }
        for (occurrence_id, occurrence) in item.link_occurrences_from() {
            occurrence_id.hash(&mut hasher);
            occurrence.hash(&mut hasher);
        }
    }
    for raw in 0..columns.types.headers.len() {
        let id = TypeId::new(u32::try_from(raw).unwrap_or(u32::MAX));
        id.hash(&mut hasher);
        ir.ty(id).hash(&mut hasher);
    }
    hasher.digest()
}

fn hash_semantic_image_facts(
    value: backend_semantic::ir::SemanticImageFacts,
    hasher: &mut StableHasher,
) {
    value.authority.hash(hasher);
    match value.provenance {
        ImageProvenance::Unavailable => 0_u8.hash(hasher),
        ImageProvenance::Captured {
            source,
            recipe,
            claim,
            scope,
        } => {
            1_u8.hash(hasher);
            source.hash_into(hasher);
            digest_recipe(recipe).hash(hasher);
            claim.identity.hash(hasher);
            scope.ecosystem.hash(hasher);
            scope.package.hash(hasher);
            scope.path.hash(hasher);
        }
    }
}

fn hash_authority(value: EntityAuthorityFacts, hasher: &mut StableHasher) {
    match value.parentage {
        ParentageAuthority::Unavailable => 0_u8.hash(hasher),
        ParentageAuthority::Root => 1_u8.hash(hasher),
        ParentageAuthority::Bound(identity) => {
            2_u8.hash(hasher);
            identity.hash(hasher);
        }
        ParentageAuthority::UnrepresentedAuthorityOwner(owner) => {
            3_u8.hash(hasher);
            owner.hash(hasher);
        }
    }
    hash_fact_availability(value.source, hasher);
    hash_fact_availability(value.source_file, hasher);
    hash_fact_availability(value.members, hasher);
    hash_fact_availability(value.semantic_type, hasher);
    hash_fact_availability(value.documentation, hasher);
    hash_fact_availability(value.visibility, hasher);
    hash_fact_availability(value.attributes, hasher);
    hash_fact_availability(value.language_extension, hasher);
}

fn hash_atom<R: backend_semantic::ir::SemanticReader + ?Sized>(
    reader: &R,
    id: backend_semantic::ir::AtomId,
    hasher: &mut StableHasher,
) {
    id.hash(hasher);
    match reader.atom(id) {
        Some(bytes) => {
            1_u8.hash(hasher);
            bytes.hash(hasher);
        }
        None => 0_u8.hash(hasher),
    }
}

fn hash_optional_rows<T, I>(rows: Option<I>, hasher: &mut StableHasher)
where
    T: Hash,
    I: ExactSizeIterator<Item = T>,
{
    match rows {
        Some(rows) => {
            1_u8.hash(hasher);
            rows.len().hash(hasher);
            for row in rows {
                row.hash(hasher);
            }
        }
        None => 0_u8.hash(hasher),
    }
}

/// Canonical content observation over owned and reopened readers.
/// Row coordinates (entity/type/link/occurrence/list/atom ids) are staging
/// artifacts: an owned `Ir` numbers rows in admission order while a reopened
/// image numbers them in canonical wire order. Every digest below therefore
/// projects rows to stable content (atom bytes, declaration identities,
/// structural type fingerprints, external keys) and hashes sorted multisets
/// of those projections. Inner-list sequences are preserved in order: the
/// wire keeps each list's sequence and only reorders top-level rows, so an
/// order change inside one list still falsifies the digest.
struct CanonicalScope<'reader, R: backend_semantic::ir::SemanticReader + ?Sized> {
    reader: &'reader R,
    entities: std::collections::HashMap<EntityId, DeclarationIdentity>,
    externals: std::collections::HashMap<ExternalId, Digest>,
    types: std::collections::HashMap<TypeId, Digest>,
}

/// Maximum structural recursion depth for one type fingerprint. The admitted
/// type graph is a DAG (every child row is strictly backward), so this guard
/// never fires on a validated image; it keeps the observer total regardless.
const CANONICAL_TYPE_DEPTH: u32 = 128;

impl<'reader, R: backend_semantic::ir::SemanticReader + ?Sized> CanonicalScope<'reader, R> {
    fn new(reader: &'reader R) -> Self {
        let mut entities = std::collections::HashMap::new();
        for entity in reader.canonical_entities() {
            entities.insert(entity.id, entity.version.identity());
        }
        Self {
            reader,
            entities,
            externals: std::collections::HashMap::new(),
            types: std::collections::HashMap::new(),
        }
    }

    fn atom_bytes(&self, id: backend_semantic::ir::AtomId) -> Option<&'reader [u8]> {
        self.reader.atom(id)
    }

    fn entity_identity(&self, id: EntityId) -> Option<backend_semantic::ir::DeclarationIdentity> {
        self.entities.get(&id).copied()
    }

    fn hash_atom(&self, hasher: &mut StableHasher, id: backend_semantic::ir::AtomId) {
        match self.atom_bytes(id) {
            Some(bytes) => {
                1_u8.hash(hasher);
                bytes.len().hash(hasher);
                hasher.write(bytes);
            }
            None => 0_u8.hash(hasher),
        }
    }

    fn hash_optional_atom(
        &self,
        hasher: &mut StableHasher,
        id: Option<backend_semantic::ir::AtomId>,
    ) {
        match id {
            Some(id) => {
                1_u8.hash(hasher);
                self.hash_atom(hasher, id);
            }
            None => 0_u8.hash(hasher),
        }
    }

    fn hash_entity_ref(&self, hasher: &mut StableHasher, id: EntityId) {
        match self.entity_identity(id) {
            Some(identity) => {
                1_u8.hash(hasher);
                hasher.write(identity.family.as_bytes());
                hasher.write(identity.variant.as_bytes());
            }
            None => 0_u8.hash(hasher),
        }
    }

    fn hash_source(
        &self,
        hasher: &mut StableHasher,
        source: Option<backend_semantic::ir::SourceSpan>,
    ) {
        match source {
            Some(span) => {
                1_u8.hash(hasher);
                self.hash_atom(hasher, span.file());
                span.start().hash(hasher);
                span.end().hash(hasher);
            }
            None => 0_u8.hash(hasher),
        }
    }

    fn external_digest(&mut self, id: ExternalId) -> Digest {
        if let Some(hit) = self.externals.get(&id) {
            return *hit;
        }
        let mut hasher = StableHasher::default();
        match self.reader.external(id) {
            Some(backend_semantic::ir::ExternalTarget::Stable { target }) => {
                0_u8.hash(&mut hasher);
                target.fragment.as_ref().hash(&mut hasher);
                hasher.write(target.declaration.family.as_bytes());
                hasher.write(target.declaration.variant.as_bytes());
            }
            Some(backend_semantic::ir::ExternalTarget::Foreign(target)) => {
                1_u8.hash(&mut hasher);
                hasher.write(target.identity.foreign.as_bytes());
                match target.identity.variant {
                    backend_semantic::ir::VariantAvailability::Known(variant) => {
                        1_u8.hash(&mut hasher);
                        hasher.write(variant.as_bytes());
                    }
                    backend_semantic::ir::VariantAvailability::Unavailable => {
                        0_u8.hash(&mut hasher);
                    }
                }
                self.hash_foreign_origin(&mut hasher, target.origin);
                self.hash_atom(&mut hasher, target.path);
                self.hash_atom(&mut hasher, target.display);
                target.kind.hash(&mut hasher);
            }
            Some(backend_semantic::ir::ExternalTarget::FragmentEntity { target, display }) => {
                2_u8.hash(&mut hasher);
                target.fragment.as_ref().hash(&mut hasher);
                target.ordinal.hash(&mut hasher);
                self.hash_atom(&mut hasher, display);
            }
            None => {
                0xff_u8.hash(&mut hasher);
            }
        }
        let digest = hasher.digest();
        self.externals.insert(id, digest);
        digest
    }

    fn hash_foreign_origin(
        &self,
        hasher: &mut StableHasher,
        origin: backend_semantic::ir::ForeignTargetOrigin,
    ) {
        match origin {
            backend_semantic::ir::ForeignTargetOrigin::Package { ecosystem, package } => {
                0_u8.hash(hasher);
                self.hash_atom(hasher, ecosystem);
                self.hash_atom(hasher, package);
            }
            backend_semantic::ir::ForeignTargetOrigin::Namespace {
                ecosystem,
                namespace,
            } => {
                1_u8.hash(hasher);
                self.hash_atom(hasher, ecosystem);
                self.hash_atom(hasher, namespace);
            }
            backend_semantic::ir::ForeignTargetOrigin::Universe { ecosystem } => {
                2_u8.hash(hasher);
                self.hash_atom(hasher, ecosystem);
            }
            backend_semantic::ir::ForeignTargetOrigin::Unspecified { ecosystem } => {
                3_u8.hash(hasher);
                self.hash_atom(hasher, ecosystem);
            }
        }
    }

    fn type_digest(&mut self, id: backend_semantic::ir::TypeId, depth: u32) -> Digest {
        if let Some(hit) = self.types.get(&id) {
            return *hit;
        }
        if depth > CANONICAL_TYPE_DEPTH {
            let mut hasher = StableHasher::default();
            hasher.write(b"canonical.type-depth-saturated.v1\0");
            return hasher.digest();
        }
        let mut hasher = StableHasher::default();
        match self.reader.ty(id) {
            Some(backend_semantic::ir::TypeExpr::Concrete(node)) => {
                0_u8.hash(&mut hasher);
                self.hash_concrete_type(&mut hasher, node, depth);
            }
            Some(backend_semantic::ir::TypeExpr::Computed(node)) => {
                1_u8.hash(&mut hasher);
                self.hash_computed_type(&mut hasher, node, depth);
            }
            Some(backend_semantic::ir::TypeExpr::Unknown(node)) => {
                2_u8.hash(&mut hasher);
                node.reason.hash(&mut hasher);
                self.hash_optional_atom(&mut hasher, node.spelling);
            }
            None => {
                0xff_u8.hash(&mut hasher);
            }
        }
        let digest = hasher.digest();
        self.types.insert(id, digest);
        digest
    }
    fn hash_concrete_type(
        &mut self,
        hasher: &mut StableHasher,
        node: backend_semantic::ir::ConcreteType,
        depth: u32,
    ) {
        node.tag().hash(hasher);
        match node {
            backend_semantic::ir::ConcreteType::Builtin(shape) => {
                shape.hash(hasher);
            }
            backend_semantic::ir::ConcreteType::Literal(shape) => {
                self.hash_literal_type(hasher, shape);
            }
            backend_semantic::ir::ConcreteType::Nominal(entity) => {
                self.hash_entity_ref(hasher, entity);
            }
            backend_semantic::ir::ConcreteType::External(external) => {
                let digest = self.external_digest(external);
                digest.hash(hasher);
            }
            backend_semantic::ir::ConcreteType::Parameter(name) => {
                self.hash_atom(hasher, name);
            }
            backend_semantic::ir::ConcreteType::Applied {
                constructor,
                arguments,
            } => {
                let digest = self.type_digest(constructor, depth + 1);
                digest.hash(hasher);
                self.hash_type_list(hasher, arguments, depth + 1);
            }
            backend_semantic::ir::ConcreteType::Tuple(elements) => {
                self.hash_tuple_elements(hasher, elements, depth + 1);
            }
            backend_semantic::ir::ConcreteType::Object(members) => {
                self.hash_object_members(hasher, members, depth + 1);
            }
            backend_semantic::ir::ConcreteType::Function {
                parameters,
                results,
                abi,
                variadic,
                unsafe_,
            } => {
                self.hash_tuple_elements(hasher, parameters, depth + 1);
                self.hash_tuple_elements(hasher, results, depth + 1);
                self.hash_optional_atom(hasher, abi);
                variadic.hash(hasher);
                unsafe_.hash(hasher);
            }
            backend_semantic::ir::ConcreteType::Reference {
                target,
                mutability,
                lifetime,
            } => {
                let digest = self.type_digest(target, depth + 1);
                digest.hash(hasher);
                mutability.hash(hasher);
                self.hash_optional_atom(hasher, lifetime);
            }
            backend_semantic::ir::ConcreteType::CxxReference { target, category } => {
                let digest = self.type_digest(target, depth + 1);
                digest.hash(hasher);
                category.hash(hasher);
            }
            backend_semantic::ir::ConcreteType::CPointer { target } => {
                let digest = self.type_digest(target, depth + 1);
                digest.hash(hasher);
            }
            backend_semantic::ir::ConcreteType::CxxMemberPointer { owner, member } => {
                let owner = self.type_digest(owner, depth + 1);
                owner.hash(hasher);
                let member = self.type_digest(member, depth + 1);
                member.hash(hasher);
            }
            backend_semantic::ir::ConcreteType::CQualified { target, qualifiers } => {
                let digest = self.type_digest(target, depth + 1);
                digest.hash(hasher);
                qualifiers.hash(hasher);
            }
            backend_semantic::ir::ConcreteType::CBlockPointer { target } => {
                let digest = self.type_digest(target, depth + 1);
                digest.hash(hasher);
            }
            backend_semantic::ir::ConcreteType::NativeCharacter { role, width } => {
                role.hash(hasher);
                width.hash(hasher);
            }
            backend_semantic::ir::ConcreteType::Pointer { target, mutability } => {
                let digest = self.type_digest(target, depth + 1);
                digest.hash(hasher);
                mutability.hash(hasher);
            }
            backend_semantic::ir::ConcreteType::Slice(target) => {
                let digest = self.type_digest(target, depth + 1);
                digest.hash(hasher);
            }
            backend_semantic::ir::ConcreteType::Array { element, shape } => {
                let digest = self.type_digest(element, depth + 1);
                digest.hash(hasher);
                self.hash_array_shape(hasher, shape);
            }
            backend_semantic::ir::ConcreteType::Optional(target) => {
                let digest = self.type_digest(target, depth + 1);
                digest.hash(hasher);
            }
            backend_semantic::ir::ConcreteType::Union(members)
            | backend_semantic::ir::ConcreteType::Intersection(members)
            | backend_semantic::ir::ConcreteType::ImplTrait(members)
            | backend_semantic::ir::ConcreteType::DynTrait(members) => {
                self.hash_type_list(hasher, members, depth + 1);
            }
            backend_semantic::ir::ConcreteType::Wildcard(bound) => {
                self.hash_wildcard_bound(hasher, bound, depth + 1);
            }
            backend_semantic::ir::ConcreteType::Annotated { kind, target } => {
                kind.hash(hasher);
                let digest = self.type_digest(target, depth + 1);
                digest.hash(hasher);
            }
            backend_semantic::ir::ConcreteType::Inferred(spelling) => {
                self.hash_optional_atom(hasher, spelling);
            }
            backend_semantic::ir::ConcreteType::QualifiedPath {
                self_type,
                trait_type,
                segments,
                spelling,
            } => {
                let digest = self.type_digest(self_type, depth + 1);
                digest.hash(hasher);
                match trait_type {
                    Some(target) => {
                        1_u8.hash(hasher);
                        let digest = self.type_digest(target, depth + 1);
                        digest.hash(hasher);
                    }
                    None => 0_u8.hash(hasher),
                }
                match segments {
                    backend_semantic::ir::QualifiedSegments::Captured(atoms) => {
                        1_u8.hash(hasher);
                        self.hash_atom_list(hasher, atoms);
                    }
                    backend_semantic::ir::QualifiedSegments::Unavailable => {
                        0_u8.hash(hasher);
                    }
                }
                self.hash_atom(hasher, spelling);
            }
            backend_semantic::ir::ConcreteType::Map { key, value } => {
                let key = self.type_digest(key, depth + 1);
                key.hash(hasher);
                let value = self.type_digest(value, depth + 1);
                value.hash(hasher);
            }
            backend_semantic::ir::ConcreteType::Channel { direction, element } => {
                direction.hash(hasher);
                let digest = self.type_digest(element, depth + 1);
                digest.hash(hasher);
            }
        }
    }

    fn hash_literal_type(
        &self,
        hasher: &mut StableHasher,
        shape: backend_semantic::ir::LiteralType,
    ) {
        core::mem::discriminant(&shape).hash(hasher);
        match shape {
            backend_semantic::ir::LiteralType::String(atom)
            | backend_semantic::ir::LiteralType::Number(atom)
            | backend_semantic::ir::LiteralType::BigInt(atom) => {
                self.hash_atom(hasher, atom);
            }
            backend_semantic::ir::LiteralType::Boolean(value) => {
                value.hash(hasher);
            }
            backend_semantic::ir::LiteralType::Null
            | backend_semantic::ir::LiteralType::Undefined => {}
        }
    }

    fn hash_array_shape(&self, hasher: &mut StableHasher, shape: backend_semantic::ir::ArrayShape) {
        match shape {
            backend_semantic::ir::ArrayShape::Sequence => 0_u8.hash(hasher),
            backend_semantic::ir::ArrayShape::Rectangular { rank } => {
                1_u8.hash(hasher);
                rank.hash(hasher);
            }
            backend_semantic::ir::ArrayShape::FixedValue { length } => {
                2_u8.hash(hasher);
                length.hash(hasher);
            }
            backend_semantic::ir::ArrayShape::ConstExpression(atom) => {
                3_u8.hash(hasher);
                self.hash_atom(hasher, atom);
            }
            backend_semantic::ir::ArrayShape::Incomplete => 4_u8.hash(hasher),
        }
    }

    fn hash_wildcard_bound(
        &mut self,
        hasher: &mut StableHasher,
        bound: backend_semantic::ir::WildcardBound,
        depth: u32,
    ) {
        match bound {
            backend_semantic::ir::WildcardBound::Unbounded => 0_u8.hash(hasher),
            backend_semantic::ir::WildcardBound::Extends(target) => {
                1_u8.hash(hasher);
                let digest = self.type_digest(target, depth + 1);
                digest.hash(hasher);
            }
            backend_semantic::ir::WildcardBound::Super(target) => {
                2_u8.hash(hasher);
                let digest = self.type_digest(target, depth + 1);
                digest.hash(hasher);
            }
        }
    }

    fn hash_computed_type(
        &mut self,
        hasher: &mut StableHasher,
        node: backend_semantic::ir::ComputedType,
        depth: u32,
    ) {
        node.tag().hash(hasher);
        match node {
            backend_semantic::ir::ComputedType::KeyOf(target) => {
                let digest = self.type_digest(target, depth + 1);
                digest.hash(hasher);
            }
            backend_semantic::ir::ComputedType::TypeOf(query) => {
                self.hash_type_query(hasher, query);
            }
            backend_semantic::ir::ComputedType::IndexedAccess { object, index } => {
                let object = self.type_digest(object, depth + 1);
                object.hash(hasher);
                let index = self.type_digest(index, depth + 1);
                index.hash(hasher);
            }
            backend_semantic::ir::ComputedType::Conditional {
                check,
                extends,
                then_type,
                else_type,
                distributive,
            } => {
                let check = self.type_digest(check, depth + 1);
                check.hash(hasher);
                let extends = self.type_digest(extends, depth + 1);
                extends.hash(hasher);
                let then_type = self.type_digest(then_type, depth + 1);
                then_type.hash(hasher);
                let else_type = self.type_digest(else_type, depth + 1);
                else_type.hash(hasher);
                distributive.hash(hasher);
            }
            backend_semantic::ir::ComputedType::Mapped {
                parameter,
                constraint,
                name_as,
                value,
                readonly,
                optional,
            } => {
                self.hash_atom(hasher, parameter);
                let constraint = self.type_digest(constraint, depth + 1);
                constraint.hash(hasher);
                match name_as {
                    Some(target) => {
                        1_u8.hash(hasher);
                        let digest = self.type_digest(target, depth + 1);
                        digest.hash(hasher);
                    }
                    None => 0_u8.hash(hasher),
                }
                let value = self.type_digest(value, depth + 1);
                value.hash(hasher);
                readonly.hash(hasher);
                optional.hash(hasher);
            }
            backend_semantic::ir::ComputedType::Infer {
                parameter,
                constraint,
            } => {
                self.hash_atom(hasher, parameter);
                match constraint {
                    Some(target) => {
                        1_u8.hash(hasher);
                        let digest = self.type_digest(target, depth + 1);
                        digest.hash(hasher);
                    }
                    None => 0_u8.hash(hasher),
                }
            }
            backend_semantic::ir::ComputedType::TemplateLiteral(parts) => {
                self.hash_template_parts(hasher, parts, depth + 1);
            }
            backend_semantic::ir::ComputedType::Import {
                specifier,
                qualifier,
                arguments,
            } => {
                self.hash_atom(hasher, specifier);
                self.hash_atom_list(hasher, qualifier);
                self.hash_type_list(hasher, arguments, depth + 1);
            }
            backend_semantic::ir::ComputedType::Awaited(target) => {
                let digest = self.type_digest(target, depth + 1);
                digest.hash(hasher);
            }
            backend_semantic::ir::ComputedType::This => {}
        }
    }

    fn hash_type_query(
        &mut self,
        hasher: &mut StableHasher,
        query: backend_semantic::ir::TypeQuery,
    ) {
        match query {
            backend_semantic::ir::TypeQuery::Entity(entity) => {
                0_u8.hash(hasher);
                self.hash_entity_ref(hasher, entity);
            }
            backend_semantic::ir::TypeQuery::Path(atoms) => {
                1_u8.hash(hasher);
                self.hash_atom_list(hasher, atoms);
            }
            backend_semantic::ir::TypeQuery::External(external) => {
                2_u8.hash(hasher);
                let digest = self.external_digest(external);
                digest.hash(hasher);
            }
        }
    }

    fn hash_type_list(
        &mut self,
        hasher: &mut StableHasher,
        id: backend_semantic::ir::TypeListId,
        depth: u32,
    ) {
        match self.reader.types(id) {
            Some(rows) => {
                1_u8.hash(hasher);
                rows.len().hash(hasher);
                for row in rows {
                    let digest = self.type_digest(row, depth + 1);
                    digest.hash(hasher);
                }
            }
            None => 0_u8.hash(hasher),
        }
    }

    fn hash_atom_list(&self, hasher: &mut StableHasher, id: backend_semantic::ir::AtomListId) {
        match self.reader.atom_list(id) {
            Some(rows) => {
                1_u8.hash(hasher);
                rows.len().hash(hasher);
                for row in rows {
                    self.hash_atom(hasher, row);
                }
            }
            None => 0_u8.hash(hasher),
        }
    }

    fn hash_entity_list(&self, hasher: &mut StableHasher, id: backend_semantic::ir::EntityListId) {
        match self.reader.entity_list(id) {
            Some(rows) => {
                1_u8.hash(hasher);
                rows.len().hash(hasher);
                for row in rows {
                    self.hash_entity_ref(hasher, row);
                }
            }
            None => 0_u8.hash(hasher),
        }
    }

    fn hash_tuple_elements(
        &mut self,
        hasher: &mut StableHasher,
        id: backend_semantic::ir::TupleElementListId,
        depth: u32,
    ) {
        match self.reader.tuple_elements(id) {
            Some(rows) => {
                1_u8.hash(hasher);
                rows.len().hash(hasher);
                for row in rows {
                    match row.label {
                        Some(label) => {
                            1_u8.hash(hasher);
                            self.hash_atom(hasher, label);
                        }
                        None => 0_u8.hash(hasher),
                    }
                    let digest = self.type_digest(row.ty, depth + 1);
                    digest.hash(hasher);
                    row.kind.hash(hasher);
                }
            }
            None => 0_u8.hash(hasher),
        }
    }

    fn hash_object_members(
        &mut self,
        hasher: &mut StableHasher,
        id: backend_semantic::ir::ObjectMemberListId,
        depth: u32,
    ) {
        match self.reader.object_members(id) {
            Some(rows) => {
                1_u8.hash(hasher);
                rows.len().hash(hasher);
                for row in rows {
                    match row {
                        backend_semantic::ir::ObjectMember::Property {
                            key,
                            ty,
                            optional,
                            readonly,
                        } => {
                            0_u8.hash(hasher);
                            self.hash_property_key(hasher, key, depth + 1);
                            let digest = self.type_digest(ty, depth + 1);
                            digest.hash(hasher);
                            optional.hash(hasher);
                            readonly.hash(hasher);
                        }
                        backend_semantic::ir::ObjectMember::Method {
                            key,
                            signature,
                            optional,
                        } => {
                            1_u8.hash(hasher);
                            self.hash_property_key(hasher, key, depth + 1);
                            let digest = self.type_digest(signature, depth + 1);
                            digest.hash(hasher);
                            optional.hash(hasher);
                        }
                        backend_semantic::ir::ObjectMember::Index {
                            parameter,
                            key,
                            value,
                            readonly,
                        } => {
                            2_u8.hash(hasher);
                            self.hash_atom(hasher, parameter);
                            let key = self.type_digest(key, depth + 1);
                            key.hash(hasher);
                            let value = self.type_digest(value, depth + 1);
                            value.hash(hasher);
                            readonly.hash(hasher);
                        }
                        backend_semantic::ir::ObjectMember::Call(target) => {
                            3_u8.hash(hasher);
                            let digest = self.type_digest(target, depth + 1);
                            digest.hash(hasher);
                        }
                        backend_semantic::ir::ObjectMember::Construct(target) => {
                            4_u8.hash(hasher);
                            let digest = self.type_digest(target, depth + 1);
                            digest.hash(hasher);
                        }
                    }
                }
            }
            None => 0_u8.hash(hasher),
        }
    }

    fn hash_property_key(
        &mut self,
        hasher: &mut StableHasher,
        key: backend_semantic::ir::PropertyKey,
        depth: u32,
    ) {
        match key {
            backend_semantic::ir::PropertyKey::Named(atom) => {
                0_u8.hash(hasher);
                self.hash_atom(hasher, atom);
            }
            backend_semantic::ir::PropertyKey::Private(atom) => {
                1_u8.hash(hasher);
                self.hash_atom(hasher, atom);
            }
            backend_semantic::ir::PropertyKey::Numeric(atom) => {
                2_u8.hash(hasher);
                self.hash_atom(hasher, atom);
            }
            backend_semantic::ir::PropertyKey::Computed(target) => {
                3_u8.hash(hasher);
                let digest = self.type_digest(target, depth + 1);
                digest.hash(hasher);
            }
        }
    }

    fn hash_template_parts(
        &mut self,
        hasher: &mut StableHasher,
        id: backend_semantic::ir::TemplatePartListId,
        depth: u32,
    ) {
        match self.reader.template_parts(id) {
            Some(rows) => {
                1_u8.hash(hasher);
                rows.len().hash(hasher);
                for row in rows {
                    match row {
                        backend_semantic::ir::TemplatePart::Bytes(atom) => {
                            0_u8.hash(hasher);
                            self.hash_atom(hasher, atom);
                        }
                        backend_semantic::ir::TemplatePart::Placeholder(target) => {
                            1_u8.hash(hasher);
                            let digest = self.type_digest(target, depth + 1);
                            digest.hash(hasher);
                        }
                    }
                }
            }
            None => 0_u8.hash(hasher),
        }
    }

    fn hash_type_parameters(
        &mut self,
        hasher: &mut StableHasher,
        id: backend_semantic::ir::TypeParameterListId,
        depth: u32,
    ) {
        match self.reader.type_parameters(id) {
            Some(rows) => {
                1_u8.hash(hasher);
                rows.len().hash(hasher);
                for row in rows {
                    self.hash_atom(hasher, row.name);
                    match self.reader.type_parameter_bounds(row.bounds) {
                        Some(bounds) => {
                            1_u8.hash(hasher);
                            bounds.len().hash(hasher);
                            for bound in bounds {
                                match bound {
                                    backend_semantic::ir::TypeParameterBound::Type(target) => {
                                        0_u8.hash(hasher);
                                        let digest = self.type_digest(target, depth + 1);
                                        digest.hash(hasher);
                                    }
                                    backend_semantic::ir::TypeParameterBound::Lifetime(atom) => {
                                        1_u8.hash(hasher);
                                        self.hash_atom(hasher, atom);
                                    }
                                }
                            }
                        }
                        None => 0_u8.hash(hasher),
                    }
                    match row.default {
                        Some(target) => {
                            1_u8.hash(hasher);
                            let digest = self.type_digest(target, depth + 1);
                            digest.hash(hasher);
                        }
                        None => 0_u8.hash(hasher),
                    }
                    row.variance.hash(hasher);
                    match row.kind {
                        backend_semantic::ir::TypeParameterKind::Type { inference } => {
                            0_u8.hash(hasher);
                            inference.hash(hasher);
                        }
                        backend_semantic::ir::TypeParameterKind::ConstValue { value_type } => {
                            1_u8.hash(hasher);
                            let digest = self.type_digest(value_type, depth + 1);
                            digest.hash(hasher);
                        }
                        backend_semantic::ir::TypeParameterKind::Lifetime => {
                            2_u8.hash(hasher);
                        }
                    }
                    row.requirements.hash(hasher);
                }
            }
            None => 0_u8.hash(hasher),
        }
    }

    fn hash_free_predicates(
        &mut self,
        hasher: &mut StableHasher,
        id: backend_semantic::ir::FreePredicateListId,
        depth: u32,
    ) {
        match self.reader.free_predicates(id) {
            Some(rows) => {
                1_u8.hash(hasher);
                rows.len().hash(hasher);
                for row in rows {
                    let digest = self.type_digest(row.subject, depth + 1);
                    digest.hash(hasher);
                    match self.reader.type_parameter_bounds(row.bounds) {
                        Some(bounds) => {
                            1_u8.hash(hasher);
                            bounds.len().hash(hasher);
                            for bound in bounds {
                                match bound {
                                    backend_semantic::ir::TypeParameterBound::Type(target) => {
                                        0_u8.hash(hasher);
                                        let digest = self.type_digest(target, depth + 1);
                                        digest.hash(hasher);
                                    }
                                    backend_semantic::ir::TypeParameterBound::Lifetime(atom) => {
                                        1_u8.hash(hasher);
                                        self.hash_atom(hasher, atom);
                                    }
                                }
                            }
                        }
                        None => 0_u8.hash(hasher),
                    }
                }
            }
            None => 0_u8.hash(hasher),
        }
    }

    fn hash_docs(&mut self, hasher: &mut StableHasher, id: backend_semantic::ir::DocId) {
        match self.reader.docs(id) {
            Some(rows) => {
                1_u8.hash(hasher);
                rows.len().hash(hasher);
                for row in rows {
                    match row {
                        backend_semantic::ir::DocFragment::Text(text)
                        | backend_semantic::ir::DocFragment::Code(text) => {
                            core::mem::discriminant(&row).hash(hasher);
                            match self.reader.text(text) {
                                Some(value) => {
                                    1_u8.hash(hasher);
                                    value.len().hash(hasher);
                                    hasher.write(value.as_bytes());
                                }
                                None => 0_u8.hash(hasher),
                            }
                        }
                        backend_semantic::ir::DocFragment::Link { label, target } => {
                            2_u8.hash(hasher);
                            match self.reader.text(label) {
                                Some(value) => {
                                    1_u8.hash(hasher);
                                    value.len().hash(hasher);
                                    hasher.write(value.as_bytes());
                                }
                                None => 0_u8.hash(hasher),
                            }
                            self.hash_link_target(hasher, target);
                        }
                        backend_semantic::ir::DocFragment::SoftBreak => {
                            3_u8.hash(hasher);
                        }
                        backend_semantic::ir::DocFragment::HardBreak => {
                            4_u8.hash(hasher);
                        }
                    }
                }
            }
            None => 0_u8.hash(hasher),
        }
    }

    fn hash_link_target(
        &mut self,
        hasher: &mut StableHasher,
        target: backend_semantic::ir::LinkTarget,
    ) {
        match target {
            backend_semantic::ir::LinkTarget::Local(entity) => {
                0_u8.hash(hasher);
                self.hash_entity_ref(hasher, entity);
            }
            backend_semantic::ir::LinkTarget::External(external) => {
                1_u8.hash(hasher);
                let digest = self.external_digest(external);
                digest.hash(hasher);
            }
        }
    }

    fn link_projection(&mut self, link: backend_semantic::ir::Link) -> Digest {
        let mut hasher = StableHasher::default();
        self.hash_entity_ref(&mut hasher, link.from);
        self.hash_link_target(&mut hasher, link.target);
        link.kind.hash(&mut hasher);
        link.confidence.hash(&mut hasher);
        self.hash_source(&mut hasher, link.source);
        hasher.digest()
    }
}

/// Hashes a sorted multiset of canonical row projections. Sorting removes
/// the admission-vs-wire row-order artifact while every content bit of each
/// projection still falsifies the digest.
fn sorted_multiset(mut rows: Vec<Digest>) -> Digest {
    rows.sort();
    let mut hasher = StableHasher::default();
    rows.len().hash(&mut hasher);
    for row in rows {
        row.hash(&mut hasher);
    }
    hasher.digest()
}

fn semantic_entities_digest<R: backend_semantic::ir::SemanticReader + ?Sized>(
    reader: &R,
) -> Digest {
    let mut scope = CanonicalScope::new(reader);
    let mut rows = Vec::new();
    for entity in reader.canonical_entities() {
        let mut hasher = StableHasher::default();
        match scope.entity_identity(entity.id) {
            Some(identity) => {
                1_u8.hash(&mut hasher);
                hasher.write(identity.family.as_bytes());
                hasher.write(identity.variant.as_bytes());
            }
            None => 0_u8.hash(&mut hasher),
        }
        entity.kind.hash(&mut hasher);
        scope.hash_atom(&mut hasher, entity.name);
        entity.visibility.hash(&mut hasher);
        match entity.parent {
            Some(parent) => {
                1_u8.hash(&mut hasher);
                scope.hash_entity_ref(&mut hasher, parent);
            }
            None => 0_u8.hash(&mut hasher),
        }
        match entity.semantic_type {
            Some(target) => {
                1_u8.hash(&mut hasher);
                let digest = scope.type_digest(target, 0);
                digest.hash(&mut hasher);
            }
            None => 0_u8.hash(&mut hasher),
        }
        scope.hash_entity_list(&mut hasher, entity.members);
        scope.hash_docs(&mut hasher, entity.docs);
        scope.hash_atom_list(&mut hasher, entity.attributes);
        scope.hash_source(&mut hasher, entity.source);
        hash_authority(entity.authority, &mut hasher);
        entity.version.hash(&mut hasher);
        rows.push(hasher.digest());
    }
    sorted_multiset(rows)
}

fn semantic_types_digest<R: backend_semantic::ir::SemanticReader + ?Sized>(reader: &R) -> Digest {
    let mut scope = CanonicalScope::new(reader);
    let mut rows = Vec::new();
    for (id, _) in reader.canonical_types() {
        rows.push(scope.type_digest(id, 0));
    }
    let types = sorted_multiset(rows);
    // The extension planes stay covered by the types digest exactly as
    // before; each lane digest below is already canonical.
    let mut hasher = StableHasher::default();
    types.hash(&mut hasher);
    for lane in canonical_extension_digests(&mut scope) {
        lane.hash(&mut hasher);
    }
    hasher.digest()
}

pub(super) fn digest_source_span_rows(mut spans: Vec<(Vec<u8>, u32, u32)>) -> Digest {
    spans.sort();
    let mut hasher = StableHasher::default();
    spans.len().hash(&mut hasher);
    for (file, start, end) in spans {
        file.hash(&mut hasher);
        start.hash(&mut hasher);
        end.hash(&mut hasher);
    }
    hasher.digest()
}

fn semantic_source_spans_digest<R: backend_semantic::ir::SemanticReader + ?Sized>(
    reader: &R,
) -> Digest {
    let spans = reader
        .canonical_entities()
        .filter_map(|entity| entity.source)
        .map(|span| {
            (
                reader.atom(span.file()).unwrap_or_default().to_vec(),
                span.start(),
                span.end(),
            )
        })
        .collect::<Vec<_>>();
    digest_source_span_rows(spans)
}

fn hash_atom_reference<R: backend_semantic::ir::SemanticReader + ?Sized>(
    reader: &R,
    id: backend_semantic::ir::AtomId,
    hasher: &mut StableHasher,
) {
    id.hash(hasher);
    match reader.atom(id) {
        Some(bytes) => {
            1_u8.hash(hasher);
            bytes.hash(hasher);
        }
        None => 0_u8.hash(hasher),
    }
}

fn hash_type_list<R: backend_semantic::ir::SemanticReader + ?Sized>(
    reader: &R,
    id: backend_semantic::ir::TypeListId,
    hasher: &mut StableHasher,
) {
    id.hash(hasher);
    match reader.types(id) {
        Some(rows) => {
            1_u8.hash(hasher);
            rows.len().hash(hasher);
            for row in rows {
                row.hash(hasher);
            }
        }
        None => 0_u8.hash(hasher),
    }
}

fn hash_entity_list<R: backend_semantic::ir::SemanticReader + ?Sized>(
    reader: &R,
    id: backend_semantic::ir::EntityListId,
    hasher: &mut StableHasher,
) {
    id.hash(hasher);
    match reader.entity_list(id) {
        Some(rows) => {
            1_u8.hash(hasher);
            rows.len().hash(hasher);
            for row in rows {
                row.hash(hasher);
            }
        }
        None => 0_u8.hash(hasher),
    }
}

fn hash_atom_list<R: backend_semantic::ir::SemanticReader + ?Sized>(
    reader: &R,
    id: backend_semantic::ir::AtomListId,
    hasher: &mut StableHasher,
) {
    id.hash(hasher);
    match reader.atom_list(id) {
        Some(rows) => {
            1_u8.hash(hasher);
            rows.len().hash(hasher);
            for row in rows {
                hash_atom_reference(reader, row, hasher);
            }
        }
        None => 0_u8.hash(hasher),
    }
}

fn hash_tuple_elements<R: backend_semantic::ir::SemanticReader + ?Sized>(
    reader: &R,
    id: backend_semantic::ir::TupleElementListId,
    hasher: &mut StableHasher,
) {
    id.hash(hasher);
    match reader.tuple_elements(id) {
        Some(rows) => {
            1_u8.hash(hasher);
            rows.len().hash(hasher);
            for row in rows {
                row.hash(hasher);
                if let Some(label) = row.label {
                    hash_atom_reference(reader, label, hasher);
                }
            }
        }
        None => 0_u8.hash(hasher),
    }
}

fn hash_property_key<R: backend_semantic::ir::SemanticReader + ?Sized>(
    reader: &R,
    key: backend_semantic::ir::PropertyKey,
    hasher: &mut StableHasher,
) {
    match key {
        backend_semantic::ir::PropertyKey::Named(id)
        | backend_semantic::ir::PropertyKey::Private(id)
        | backend_semantic::ir::PropertyKey::Numeric(id) => hash_atom_reference(reader, id, hasher),
        backend_semantic::ir::PropertyKey::Computed(id) => id.hash(hasher),
    }
}

fn hash_object_members<R: backend_semantic::ir::SemanticReader + ?Sized>(
    reader: &R,
    id: backend_semantic::ir::ObjectMemberListId,
    hasher: &mut StableHasher,
) {
    id.hash(hasher);
    match reader.object_members(id) {
        Some(rows) => {
            1_u8.hash(hasher);
            rows.len().hash(hasher);
            for row in rows {
                row.hash(hasher);
                match row {
                    backend_semantic::ir::ObjectMember::Property { key, .. }
                    | backend_semantic::ir::ObjectMember::Method { key, .. } => {
                        hash_property_key(reader, key, hasher);
                    }
                    backend_semantic::ir::ObjectMember::Index { parameter, .. } => {
                        hash_atom_reference(reader, parameter, hasher);
                    }
                    backend_semantic::ir::ObjectMember::Call(_)
                    | backend_semantic::ir::ObjectMember::Construct(_) => {}
                }
            }
        }
        None => 0_u8.hash(hasher),
    }
}

fn hash_template_parts<R: backend_semantic::ir::SemanticReader + ?Sized>(
    reader: &R,
    id: backend_semantic::ir::TemplatePartListId,
    hasher: &mut StableHasher,
) {
    id.hash(hasher);
    match reader.template_parts(id) {
        Some(rows) => {
            1_u8.hash(hasher);
            rows.len().hash(hasher);
            for row in rows {
                row.hash(hasher);
                if let backend_semantic::ir::TemplatePart::Bytes(atom) = row {
                    hash_atom_reference(reader, atom, hasher);
                }
            }
        }
        None => 0_u8.hash(hasher),
    }
}

fn hash_parameter_bounds<R: backend_semantic::ir::SemanticReader + ?Sized>(
    reader: &R,
    id: backend_semantic::ir::TypeParameterBoundListId,
    hasher: &mut StableHasher,
) {
    id.hash(hasher);
    match reader.type_parameter_bounds(id) {
        Some(rows) => {
            1_u8.hash(hasher);
            rows.len().hash(hasher);
            for row in rows {
                row.hash(hasher);
                if let backend_semantic::ir::TypeParameterBound::Lifetime(atom) = row {
                    hash_atom_reference(reader, atom, hasher);
                }
            }
        }
        None => 0_u8.hash(hasher),
    }
}

fn hash_parameters<R: backend_semantic::ir::SemanticReader + ?Sized>(
    reader: &R,
    id: backend_semantic::ir::TypeParameterListId,
    hasher: &mut StableHasher,
) {
    id.hash(hasher);
    match reader.type_parameters(id) {
        Some(rows) => {
            1_u8.hash(hasher);
            rows.len().hash(hasher);
            for row in rows {
                row.hash(hasher);
                hash_atom_reference(reader, row.name, hasher);
                hash_parameter_bounds(reader, row.bounds, hasher);
            }
        }
        None => 0_u8.hash(hasher),
    }
}

fn hash_type_references<R: backend_semantic::ir::SemanticReader + ?Sized>(
    reader: &R,
    expression: TypeExpr,
    hasher: &mut StableHasher,
) {
    match expression {
        TypeExpr::Concrete(concrete) => match concrete {
            ConcreteType::Applied { arguments, .. }
            | ConcreteType::Union(arguments)
            | ConcreteType::Intersection(arguments)
            | ConcreteType::ImplTrait(arguments)
            | ConcreteType::DynTrait(arguments) => hash_type_list(reader, arguments, hasher),
            ConcreteType::Tuple(elements)
            | ConcreteType::Function {
                parameters: elements,
                ..
            } => {
                hash_tuple_elements(reader, elements, hasher);
                if let ConcreteType::Function { results, .. } = concrete {
                    hash_tuple_elements(reader, results, hasher);
                }
            }
            ConcreteType::Object(members) => hash_object_members(reader, members, hasher),
            ConcreteType::QualifiedPath { segments, .. } => {
                if let backend_semantic::ir::QualifiedSegments::Captured(segments) = segments {
                    hash_atom_list(reader, segments, hasher);
                }
            }
            ConcreteType::Wildcard(backend_semantic::ir::WildcardBound::Unbounded)
            | ConcreteType::Wildcard(backend_semantic::ir::WildcardBound::Extends(_))
            | ConcreteType::Wildcard(backend_semantic::ir::WildcardBound::Super(_))
            | ConcreteType::Builtin(_)
            | ConcreteType::Literal(_)
            | ConcreteType::Nominal(_)
            | ConcreteType::External(_)
            | ConcreteType::Parameter(_)
            | ConcreteType::Reference { .. }
            | ConcreteType::CxxReference { .. }
            | ConcreteType::CPointer { .. }
            | ConcreteType::CxxMemberPointer { .. }
            | ConcreteType::CQualified { .. }
            | ConcreteType::CBlockPointer { .. }
            | ConcreteType::NativeCharacter { .. }
            | ConcreteType::Pointer { .. }
            | ConcreteType::Slice(_)
            | ConcreteType::Array { .. }
            | ConcreteType::Optional(_)
            | ConcreteType::Annotated { .. }
            | ConcreteType::Inferred(_)
            | ConcreteType::Map { .. }
            | ConcreteType::Channel { .. } => {}
        },
        TypeExpr::Computed(computed) => match computed {
            backend_semantic::ir::ComputedType::TypeOf(backend_semantic::ir::TypeQuery::Path(
                path,
            )) => {
                hash_atom_list(reader, path, hasher);
            }
            backend_semantic::ir::ComputedType::TemplateLiteral(parts) => {
                hash_template_parts(reader, parts, hasher);
            }
            backend_semantic::ir::ComputedType::Import {
                qualifier,
                arguments,
                ..
            } => {
                hash_atom_list(reader, qualifier, hasher);
                hash_type_list(reader, arguments, hasher);
            }
            backend_semantic::ir::ComputedType::KeyOf(_)
            | backend_semantic::ir::ComputedType::TypeOf(_)
            | backend_semantic::ir::ComputedType::IndexedAccess { .. }
            | backend_semantic::ir::ComputedType::Conditional { .. }
            | backend_semantic::ir::ComputedType::Mapped { .. }
            | backend_semantic::ir::ComputedType::Infer { .. }
            | backend_semantic::ir::ComputedType::Awaited(_)
            | backend_semantic::ir::ComputedType::This => {}
        },
        TypeExpr::Unknown(_) => {}
    }
}

/// Canonical per-lane extension digests in [`observe_reader`] lane order.
/// Each row joins its entity identity with the lane facts, so reopened row
/// renumbering cannot falsify the digest while every content bit still can.
fn canonical_extension_digests<R: backend_semantic::ir::SemanticReader + ?Sized>(
    scope: &mut CanonicalScope<'_, R>,
) -> [Digest; 7] {
    [
        canonical_typescript_extensions(scope),
        canonical_csharp_extensions(scope),
        canonical_go_extensions(scope),
        canonical_rust_extensions(scope),
        canonical_python_extensions(scope),
        canonical_java_extensions(scope),
        canonical_clang_extensions(scope),
    ]
}

fn canonical_typescript_extensions<R: backend_semantic::ir::SemanticReader + ?Sized>(
    scope: &mut CanonicalScope<'_, R>,
) -> Digest {
    let mut rows = Vec::new();
    for (entity, facts) in scope.reader.typescript_extensions() {
        let mut hasher = StableHasher::default();
        scope.hash_entity_ref(&mut hasher, entity);
        scope.hash_type_parameters(&mut hasher, facts.type_parameters, 0);
        match facts.declared {
            Some(target) => {
                1_u8.hash(&mut hasher);
                let digest = scope.type_digest(target, 0);
                digest.hash(&mut hasher);
            }
            None => 0_u8.hash(&mut hasher),
        }
        match facts.observed {
            Some(target) => {
                1_u8.hash(&mut hasher);
                let digest = scope.type_digest(target, 0);
                digest.hash(&mut hasher);
            }
            None => 0_u8.hash(&mut hasher),
        }
        rows.push(hasher.digest());
    }
    sorted_multiset(rows)
}

fn canonical_csharp_extensions<R: backend_semantic::ir::SemanticReader + ?Sized>(
    scope: &mut CanonicalScope<'_, R>,
) -> Digest {
    let mut rows = Vec::new();
    for (entity, facts) in scope.reader.csharp_extensions() {
        let mut hasher = StableHasher::default();
        scope.hash_entity_ref(&mut hasher, entity);
        facts.nullability.hash(&mut hasher);
        facts.reference_kind.hash(&mut hasher);
        scope.hash_type_parameters(&mut hasher, facts.constraints, 0);
        facts.effects.hash(&mut hasher);
        scope.hash_atom_list(&mut hasher, facts.attributes);
        facts.partial.hash(&mut hasher);
        scope.hash_source(&mut hasher, facts.xml_provenance);
        rows.push(hasher.digest());
    }
    sorted_multiset(rows)
}

fn canonical_go_extensions<R: backend_semantic::ir::SemanticReader + ?Sized>(
    scope: &mut CanonicalScope<'_, R>,
) -> Digest {
    let mut rows = Vec::new();
    for (entity, facts) in scope.reader.go_extensions() {
        let mut hasher = StableHasher::default();
        scope.hash_entity_ref(&mut hasher, entity);
        scope.hash_type_list(&mut hasher, facts.signature.parameters, 0);
        scope.hash_type_list(&mut hasher, facts.signature.results, 0);
        facts.signature.variadic.hash(&mut hasher);
        scope.hash_type_parameters(&mut hasher, facts.type_parameters, 0);
        scope.hash_entity_list(&mut hasher, facts.fields);
        scope.hash_entity_list(&mut hasher, facts.method_set);
        scope.hash_atom_list(&mut hasher, facts.build_constraints);
        scope.hash_atom_list(&mut hasher, facts.constant_value);
        facts.constant_group.hash(&mut hasher);
        facts.constant_flags.hash(&mut hasher);
        rows.push(hasher.digest());
    }
    sorted_multiset(rows)
}

fn canonical_rust_extensions<R: backend_semantic::ir::SemanticReader + ?Sized>(
    scope: &mut CanonicalScope<'_, R>,
) -> Digest {
    let mut rows = Vec::new();
    for (entity, facts) in scope.reader.rust_extensions() {
        let mut hasher = StableHasher::default();
        scope.hash_entity_ref(&mut hasher, entity);
        facts.ownership.hash(&mut hasher);
        scope.hash_atom_list(&mut hasher, facts.lifetimes);
        scope.hash_type_parameters(&mut hasher, facts.where_clauses, 0);
        scope.hash_atom_list(&mut hasher, facts.macros);
        scope.hash_atom_list(&mut hasher, facts.const_defaults);
        scope.hash_free_predicates(&mut hasher, facts.free_predicates, 0);
        rows.push(hasher.digest());
    }
    sorted_multiset(rows)
}

fn canonical_python_extensions<R: backend_semantic::ir::SemanticReader + ?Sized>(
    scope: &mut CanonicalScope<'_, R>,
) -> Digest {
    let mut rows = Vec::new();
    for (entity, facts) in scope.reader.python_extensions() {
        let mut hasher = StableHasher::default();
        scope.hash_entity_ref(&mut hasher, entity);
        scope.hash_atom_list(&mut hasher, facts.decorators);
        facts.parameter_kind.hash(&mut hasher);
        facts.dynamic_confidence.hash(&mut hasher);
        rows.push(hasher.digest());
    }
    sorted_multiset(rows)
}

fn canonical_java_extensions<R: backend_semantic::ir::SemanticReader + ?Sized>(
    scope: &mut CanonicalScope<'_, R>,
) -> Digest {
    let mut rows = Vec::new();
    for (entity, facts) in scope.reader.java_extensions() {
        let mut hasher = StableHasher::default();
        scope.hash_entity_ref(&mut hasher, entity);
        scope.hash_type_list(&mut hasher, facts.throws, 0);
        scope.hash_atom_list(&mut hasher, facts.annotations);
        scope.hash_entity_list(&mut hasher, facts.overloads);
        scope.hash_entity_list(&mut hasher, facts.record_components);
        rows.push(hasher.digest());
    }
    sorted_multiset(rows)
}

fn canonical_clang_extensions<R: backend_semantic::ir::SemanticReader + ?Sized>(
    scope: &mut CanonicalScope<'_, R>,
) -> Digest {
    let mut rows = Vec::new();
    for (entity, facts) in scope.reader.clang_extensions() {
        let mut hasher = StableHasher::default();
        scope.hash_entity_ref(&mut hasher, entity);
        facts.qualifiers.hash(&mut hasher);
        facts.storage.hash(&mut hasher);
        facts.layout.hash(&mut hasher);
        scope.hash_type_parameters(&mut hasher, facts.templates, 0);
        scope.hash_atom_list(&mut hasher, facts.includes);
        rows.push(hasher.digest());
    }
    sorted_multiset(rows)
}

fn semantic_externals_digest<R: backend_semantic::ir::SemanticReader + ?Sized>(
    reader: &R,
) -> Digest {
    let mut scope = CanonicalScope::new(reader);
    let mut rows = Vec::new();
    for (id, _) in reader.canonical_externals() {
        rows.push(scope.external_digest(id));
    }
    sorted_multiset(rows)
}

fn semantic_links_digest<R: backend_semantic::ir::SemanticReader + ?Sized>(reader: &R) -> Digest {
    let mut scope = CanonicalScope::new(reader);
    let mut rows = Vec::new();
    for (_, link) in reader.canonical_links() {
        rows.push(scope.link_projection(link));
    }
    sorted_multiset(rows)
}

fn semantic_occurrences_digest<R: backend_semantic::ir::SemanticReader + ?Sized>(
    reader: &R,
) -> Digest {
    let mut scope = CanonicalScope::new(reader);
    let mut rows = Vec::new();
    for (id, occurrence) in reader.link_occurrences() {
        let mut hasher = StableHasher::default();
        match scope.reader.link(occurrence.link) {
            Some(link) => {
                1_u8.hash(&mut hasher);
                scope.hash_entity_ref(&mut hasher, link.from);
                scope.hash_link_target(&mut hasher, link.target);
                link.kind.hash(&mut hasher);
            }
            None => 0_u8.hash(&mut hasher),
        }
        occurrence.confidence.hash(&mut hasher);
        match scope.reader.occurrence_authority(id) {
            Some(authority) => {
                1_u8.hash(&mut hasher);
                hash_fact_availability(authority.source, &mut hasher);
            }
            None => 0_u8.hash(&mut hasher),
        }
        scope.hash_source(&mut hasher, occurrence.source);
        rows.push(hasher.digest());
    }
    sorted_multiset(rows)
}

fn canonical_type_render<R: backend_semantic::ir::SemanticReader + ?Sized>(
    reader: &R,
    primary: Option<EntityId>,
) -> RenderVerdict {
    let Some(entity) = primary.and_then(|id| reader.entity(id)) else {
        return RenderVerdict::Unavailable;
    };
    let Some(root) = entity.semantic_type else {
        return RenderVerdict::Unavailable;
    };
    let limits = backend_semantic::ir::CanonicalTypeRenderLimits::new(
        NonZeroUsize::new(1024).expect("nonzero canonical render depth"),
    );
    let Ok(prepared) = backend_semantic::ir::prepare_canonical_type(reader, root, limits) else {
        return RenderVerdict::Unavailable;
    };
    let mut output = vec![0_u8; prepared.encoded_len];
    match prepared.write_into(&mut output) {
        Ok(text) => RenderVerdict::Rendered(digest_bytes(text.as_bytes())),
        Err(_) => RenderVerdict::Unavailable,
    }
}

fn observe_reader<R: backend_semantic::ir::SemanticReader + ?Sized>(
    reader: &R,
    identity: Option<backend_semantic::ir::SemanticImageIdentity>,
    primary: Option<EntityId>,
) -> SemanticObservation {
    let mut scope = CanonicalScope::new(reader);
    let extensions = canonical_extension_digests(&mut scope);
    let image = observe_image_facts(reader);
    let census = observe_image_census(reader, &image);
    let status = if census.is_some() {
        SemanticReaderStatus::Captured
    } else {
        SemanticReaderStatus::ObserverUnavailable
    };
    SemanticObservation {
        status,
        identity,
        image: Some(image),
        census,
        entities: semantic_entities_digest(reader),
        types: semantic_types_digest(reader),
        externals: semantic_externals_digest(reader),
        links: semantic_links_digest(reader),
        occurrences: semantic_occurrences_digest(reader),
        extensions,
        source_spans: semantic_source_spans_digest(reader),
        canonical_type: canonical_type_render(reader, primary),
    }
}

fn owned_image_identity(ir: &Ir) -> Option<backend_semantic::ir::SemanticImageIdentity> {
    let length = backend_semantic::ir::full_semantic_image_len(ir).ok()?;
    let mut bytes = vec![0_u8; length];
    let written = backend_semantic::ir::encode_full_semantic_image(ir, &mut bytes).ok()?;
    (written == length)
        .then(|| backend_semantic::ir::SemanticImageIdentity::from_encoded_bytes(&bytes))
}

pub(super) fn observe_owned_semantic(ir: &Ir, primary: Option<EntityId>) -> SemanticObservation {
    observe_reader(ir, owned_image_identity(ir), primary)
}

pub(super) fn observe_reopened_semantic(
    image: &backend_semantic::ir::SemanticImageView<'_>,
    primary: Option<EntityId>,
) -> SemanticObservation {
    observe_reader(
        image,
        Some(backend_semantic::ir::SemanticImageIdentity::from_encoded_bytes(image.as_ref())),
        primary,
    )
}

/// Finds one source-bound declaration in a complete owned or reopened reader.
///
/// Name-only lookup is insufficient for grouped fixtures: Java overloads and
/// wrapper classes can share a spelling.  The source-name span is therefore a
/// mandatory containment key, while the returned multiplicity lets the caller
/// report duplicate or missing authority rows instead of picking the first
/// match.
pub(super) fn observe_entity_at_source<R: SemanticReader + ?Sized>(
    reader: &R,
    expected_kind: ItemKind,
    expected_name: &[u8],
    expected_name_start: u32,
    expected_name_end: u32,
    expected_file: &[u8],
) -> (u16, Option<EntityObservation>, u32, u32) {
    let candidates: Vec<_> = reader
        .canonical_entities()
        .filter(|entity| {
            entity.kind == expected_kind
                && reader
                    .atom(entity.name)
                    .is_some_and(|name| name == expected_name)
                && entity.source.is_some_and(|span| {
                    span.start() <= expected_name_start
                        && span.end() >= expected_name_end
                        && reader.atom(span.file()) == Some(expected_file)
                })
        })
        .collect();
    let matches = u16::try_from(candidates.len()).unwrap_or(u16::MAX);
    let Some(entity) = (candidates.len() == 1).then(|| candidates[0]) else {
        return (matches, None, 0, 0);
    };
    let members = reader.entity_list(entity.members).map_or(0, |members| {
        u32::try_from(members.len()).unwrap_or(u32::MAX)
    });
    let member_order = reader
        .entity_list(entity.members)
        .map_or([0_u8; 32], |members| {
            member_order_digest_reader(reader, members)
        });
    let first_member = reader
        .entity_list(entity.members)
        .and_then(|mut members| members.next())
        .and_then(|id| reader.entity(id))
        .and_then(|member| reader.atom(member.name))
        .map(digest_bytes);
    let occurrences = reader
        .link_occurrences()
        .filter(|(_, occurrence)| {
            reader
                .link(occurrence.link)
                .is_some_and(|link| link.from == entity.id)
        })
        .count();
    let links = reader.links_from(entity.id).len();
    let observed = EntityObservation {
        id: entity.id,
        kind: entity.kind,
        name: digest_bytes(reader.atom(entity.name).unwrap_or_default()),
        visibility: entity.visibility,
        parent: entity.parent,
        authority: Some(entity.authority),
        members,
        member_order,
        first_member,
        type_shape: type_shape_reader(reader, entity.semantic_type),
        source: entity.source,
        version: version_observation(entity.version),
    };
    (
        matches,
        Some(observed),
        u32::try_from(links).unwrap_or(u32::MAX),
        u32::try_from(occurrences).unwrap_or(u32::MAX),
    )
}

/// Canonical entity-observation comparison across the owned/reopened
/// boundary. Row coordinates (entity ids, source file atoms, member order
/// ids) are staging artifacts, so every id resolves to content before any
/// comparison: identities for entities, bytes for atoms and spans, ordered
/// member identities for member order.
pub(super) fn entity_observations_equal<Owned, Reopened>(
    owned_reader: &Owned,
    owned: &EntityObservation,
    reopened_reader: &Reopened,
    reopened: &EntityObservation,
) -> bool
where
    Owned: backend_semantic::ir::SemanticReader + ?Sized,
    Reopened: backend_semantic::ir::SemanticReader + ?Sized,
{
    owned.kind == reopened.kind
        && owned.name == reopened.name
        && owned.visibility == reopened.visibility
        && entity_parent_identity(owned_reader, owned.parent)
            == entity_parent_identity(reopened_reader, reopened.parent)
        && owned.authority == reopened.authority
        && owned.members == reopened.members
        && canonical_member_identities(owned_reader, owned.id)
            == canonical_member_identities(reopened_reader, reopened.id)
        && owned.first_member == reopened.first_member
        && owned.type_shape == reopened.type_shape
        && canonical_source(owned_reader, owned.source)
            == canonical_source(reopened_reader, reopened.source)
        && owned.version == reopened.version
}

fn entity_identity_of<R: backend_semantic::ir::SemanticReader + ?Sized>(
    reader: &R,
    id: EntityId,
) -> Option<DeclarationIdentity> {
    reader.entity(id).map(|entity| entity.version.identity())
}

fn entity_parent_identity<R: backend_semantic::ir::SemanticReader + ?Sized>(
    reader: &R,
    parent: Option<EntityId>,
) -> Option<Option<DeclarationIdentity>> {
    parent.map(|id| entity_identity_of(reader, id))
}

fn canonical_source<R: backend_semantic::ir::SemanticReader + ?Sized>(
    reader: &R,
    source: Option<SourceSpan>,
) -> Option<(Vec<u8>, u32, u32)> {
    source.map(|span| {
        (
            reader.atom(span.file()).unwrap_or_default().to_vec(),
            span.start(),
            span.end(),
        )
    })
}

fn canonical_member_identities<R: backend_semantic::ir::SemanticReader + ?Sized>(
    reader: &R,
    id: EntityId,
) -> Option<Vec<DeclarationIdentity>> {
    let entity = reader.entity(id)?;
    let members = reader.entity_list(entity.members)?;
    members
        .map(|member| entity_identity_of(reader, member))
        .collect()
}

fn member_order_digest_reader<R: SemanticReader + ?Sized>(
    reader: &R,
    members: R::Entities<'_>,
) -> Digest {
    let mut hasher = StableHasher::default();
    for member_id in members {
        member_id.raw.hash(&mut hasher);
        if let Some(member) = reader.entity(member_id) {
            member.kind.hash(&mut hasher);
            reader
                .atom(member.name)
                .unwrap_or_default()
                .hash(&mut hasher);
        }
    }
    hasher.digest()
}

pub(super) fn type_shape_reader<R: SemanticReader + ?Sized>(
    reader: &R,
    ty: Option<TypeId>,
) -> ObservedTypeShape {
    let Some(expression) = ty.and_then(|id| reader.ty(id)) else {
        return ObservedTypeShape::Absent;
    };
    match expression {
        TypeExpr::Unknown(_) => ObservedTypeShape::Unknown,
        TypeExpr::Computed(_) => ObservedTypeShape::Computed,
        TypeExpr::Concrete(ConcreteType::Builtin(builtin)) => ObservedTypeShape::Primitive(builtin),
        TypeExpr::Concrete(ConcreteType::Literal(_)) => ObservedTypeShape::Literal,
        TypeExpr::Concrete(ConcreteType::Function { .. }) => ObservedTypeShape::Callable,
        TypeExpr::Concrete(ConcreteType::Nominal(_))
        | TypeExpr::Concrete(ConcreteType::External(_)) => ObservedTypeShape::Nominal,
        TypeExpr::Concrete(ConcreteType::Reference { .. }) => ObservedTypeShape::Reference,
        TypeExpr::Concrete(ConcreteType::Parameter(_))
        | TypeExpr::Concrete(ConcreteType::Applied { .. })
        | TypeExpr::Concrete(ConcreteType::ImplTrait(_))
        | TypeExpr::Concrete(ConcreteType::DynTrait(_)) => ObservedTypeShape::Generic,
        TypeExpr::Concrete(
            ConcreteType::Tuple(_)
            | ConcreteType::Object(_)
            | ConcreteType::CxxReference { .. }
            | ConcreteType::CPointer { .. }
            | ConcreteType::CxxMemberPointer { .. }
            | ConcreteType::CQualified { .. }
            | ConcreteType::CBlockPointer { .. }
            | ConcreteType::NativeCharacter { .. }
            | ConcreteType::Pointer { .. }
            | ConcreteType::Slice(_)
            | ConcreteType::Array { .. }
            | ConcreteType::Optional(_)
            | ConcreteType::Union(_)
            | ConcreteType::Intersection(_)
            | ConcreteType::Wildcard(_)
            | ConcreteType::Annotated { .. }
            | ConcreteType::Inferred(_)
            | ConcreteType::QualifiedPath { .. }
            | ConcreteType::Map { .. }
            | ConcreteType::Channel { .. },
        ) => ObservedTypeShape::Structural,
    }
}

/// Borrows one complete authority row through the immutable IR columns. A
/// missing aligned row is observer unavailability, never an inferred empty
/// fact from the corresponding hot semantic value.
fn authority_facts(ir: &Ir, id: EntityId) -> Option<EntityAuthorityFacts> {
    let columns = ir.entity_authority_columns();
    let index = id.index();
    Some(EntityAuthorityFacts {
        parentage: *columns.parentage.get(index)?,
        source: *columns.source.get(index)?,
        source_file: *columns.source_file.get(index)?,
        members: *columns.members.get(index)?,
        semantic_type: *columns.semantic_type.get(index)?,
        documentation: *columns.documentation.get(index)?,
        visibility: *columns.visibility.get(index)?,
        attributes: *columns.attributes.get(index)?,
        language_extension: *columns.language_extension.get(index)?,
    })
}

const fn fact_plane(availability: FactAvailability) -> PlaneObservation {
    match availability {
        FactAvailability::Captured => PlaneObservation::Captured,
        FactAvailability::Unavailable => PlaneObservation::Unavailable,
    }
}

pub(super) fn observe_owned(
    ir: &Ir,
    package: CorpusPackage,
    rendered: &multilingual_corpus::RenderedPackage<'_>,
) -> OwnedObservation {
    let expected_kind = item_kind(rendered.expected.primary_kind);
    let candidates: Vec<_> = ir
        .items()
        .filter(|item| {
            item.kind() == expected_kind && item.name() == rendered.expected_symbol.as_bytes()
        })
        .collect();
    let primary_matches = u16::try_from(candidates.len()).unwrap_or(u16::MAX);
    let primary = (candidates.len() == 1).then(|| {
        let item = candidates[0];
        EntityObservation {
            id: item.id(),
            kind: item.kind(),
            name: digest_bytes(item.name()),
            visibility: item.visibility(),
            parent: item.parent(),
            authority: authority_facts(ir, item.id()),
            members: u32::try_from(item.members().len()).unwrap_or(u32::MAX),
            member_order: member_order_digest(ir, item.members()),
            first_member: item
                .members()
                .first()
                .and_then(|member| ir.item(*member))
                .map(|member| digest_bytes(member.name())),
            type_shape: type_shape(ir, item.semantic_type()),
            source: item.source(),
            version: version_observation(item.version()),
        }
    });
    let primary_id = primary.map(|item| item.id);
    let primary_authority = primary.and_then(|item| item.authority);
    let semantic = observe_owned_semantic(ir, primary_id);
    let documentation = primary_authority.map_or(PlaneObservation::ObserverUnavailable, |facts| {
        fact_plane(facts.documentation)
    });
    let attributes = primary_authority.map_or(PlaneObservation::ObserverUnavailable, |facts| {
        fact_plane(facts.attributes)
    });
    let source_plane = primary_authority.map_or(PlaneObservation::ObserverUnavailable, |facts| {
        fact_plane(facts.source)
    });
    let extension = primary_authority.map_or(PlaneObservation::ObserverUnavailable, |facts| {
        fact_plane(facts.language_extension)
    });
    let occurrences = primary_id.map_or(CountObservation::Unavailable, |id| {
        CountObservation::Exact(
            u32::try_from(ir.link_occurrences_from(id).len()).unwrap_or(u32::MAX),
        )
    });
    let links = primary_id.map_or(CountObservation::Unavailable, |id| {
        CountObservation::Exact(u32::try_from(ir.links_from(id).len()).unwrap_or(u32::MAX))
    });
    OwnedObservation {
        entity_count: u32::try_from(ir.entity_count()).unwrap_or(u32::MAX),
        image_provenance: ir.image_provenance(),
        primary_matches,
        primary,
        declarations: u32::try_from(ir.items().count()).unwrap_or(u32::MAX),
        generic_parameters: primary_id.map_or(CountObservation::Unavailable, |id| {
            generic_parameters(ir, package.language, id)
        }),
        documentation,
        attributes,
        source_plane,
        occurrences,
        links,
        extension,
        semantic_digest: semantic_digest(ir),
        semantic,
    }
}

pub(super) fn observe_compact(
    fragment: &FragmentView<'_>,
    _package: CorpusPackage,
    rendered: &multilingual_corpus::RenderedPackage<'_>,
) -> CompactObservation {
    let candidates: Vec<_> = fragment
        .entities()
        .filter(|entity| {
            entity.kind == rendered.expected.primary_kind
                && fragment
                    .atoms()
                    .nth(entity.name.index())
                    .is_some_and(|atom| atom.bytes == rendered.expected_symbol.as_bytes())
        })
        .collect();
    let primary_matches = u16::try_from(candidates.len()).unwrap_or(u16::MAX);
    let primary = (candidates.len() == 1).then(|| candidates[0]);
    let discovery = fragment.discover();
    CompactObservation {
        fragment: digest_bytes(fragment.as_ref()),
        census: discovery.census(),
        primary_matches,
        primary_kind: primary.map(|entity| entity.kind),
        primary_name: primary
            .and_then(|entity| fragment.atoms().nth(entity.name.index()))
            .map_or([0_u8; 32], |atom| digest_bytes(atom.bytes)),
        primary_type: primary
            .and_then(|entity| fragment.type_nodes().nth(entity.semantic_type.index())),
        semantic_data: if discovery.semantic_data().is_some() {
            PlaneObservation::Captured
        } else {
            PlaneObservation::Unavailable
        },
        occurrences: if discovery.occurrences().is_some() {
            PlaneObservation::Captured
        } else {
            PlaneObservation::Unavailable
        },
        type_facts: if discovery.type_facts().is_some() {
            PlaneObservation::Captured
        } else {
            PlaneObservation::Unavailable
        },
        documentation: if discovery.documentation().is_some() {
            PlaneObservation::Captured
        } else {
            PlaneObservation::Unavailable
        },
        extensions: if discovery
            .language_extensions()
            .is_ok_and(|value| value.is_some())
        {
            PlaneObservation::Captured
        } else {
            PlaneObservation::Unavailable
        },
    }
}

pub(super) fn render_neutral(ir: &Ir, primary: Option<EntityObservation>) -> RenderVerdict {
    let Some(primary) = primary else {
        return RenderVerdict::Unavailable;
    };
    canonical_type_render(ir, Some(primary.id))
}

/// Canonical language-neutral signature rendering for any complete semantic
/// reader, including a cold reopened image.  The grouped audit uses this
/// alongside the owned `Ir` renderer so a publication reader cannot silently
/// lose a declaration's type spelling.
pub(super) fn render_neutral_reader<R: SemanticReader + ?Sized>(
    reader: &R,
    primary: Option<EntityId>,
) -> RenderVerdict {
    canonical_type_render(reader, primary)
}

fn hash_count_observation(value: CountObservation, hasher: &mut StableHasher) {
    match value {
        CountObservation::Exact(count) => {
            0_u8.hash(hasher);
            count.hash(hasher);
        }
        CountObservation::Unavailable => 1_u8.hash(hasher),
        CountObservation::Unsupported => 2_u8.hash(hasher),
    }
}

fn hash_plane_observation(value: PlaneObservation, hasher: &mut StableHasher) {
    let tag = match value {
        PlaneObservation::Captured => 0_u8,
        PlaneObservation::Unavailable => 1_u8,
        PlaneObservation::ObserverUnavailable => 2_u8,
        PlaneObservation::Unsupported => 3_u8,
    };
    tag.hash(hasher);
}

fn hash_type_shape(value: ObservedTypeShape, hasher: &mut StableHasher) {
    match value {
        ObservedTypeShape::Absent => 0_u8.hash(hasher),
        ObservedTypeShape::Primitive(builtin) => {
            1_u8.hash(hasher);
            builtin.hash(hasher);
        }
        ObservedTypeShape::Literal => 2_u8.hash(hasher),
        ObservedTypeShape::Callable => 3_u8.hash(hasher),
        ObservedTypeShape::Nominal => 4_u8.hash(hasher),
        ObservedTypeShape::Structural => 5_u8.hash(hasher),
        ObservedTypeShape::Reference => 6_u8.hash(hasher),
        ObservedTypeShape::Generic => 7_u8.hash(hasher),
        ObservedTypeShape::Computed => 8_u8.hash(hasher),
        ObservedTypeShape::Unknown => 9_u8.hash(hasher),
        ObservedTypeShape::Other => 10_u8.hash(hasher),
    }
}

fn hash_type_node(value: Option<TypeNode>, hasher: &mut StableHasher) {
    match value {
        None => 0_u8.hash(hasher),
        Some(TypeNode::Primitive(primitive)) => {
            1_u8.hash(hasher);
            u32::from(primitive).hash(hasher);
        }
        Some(TypeNode::Reference(id)) => {
            2_u8.hash(hasher);
            id.hash(hasher);
        }
    }
}

fn hash_fact_availability(value: FactAvailability, hasher: &mut StableHasher) {
    match value {
        FactAvailability::Captured => 0_u8.hash(hasher),
        FactAvailability::Unavailable => 1_u8.hash(hasher),
    }
}

fn hash_authority_facts(value: Option<EntityAuthorityFacts>, hasher: &mut StableHasher) {
    let Some(value) = value else {
        0_u8.hash(hasher);
        return;
    };
    1_u8.hash(hasher);
    match value.parentage {
        ParentageAuthority::Unavailable => 0_u8.hash(hasher),
        ParentageAuthority::Root => 1_u8.hash(hasher),
        ParentageAuthority::Bound(identity) => {
            2_u8.hash(hasher);
            identity.hash(hasher);
        }
        ParentageAuthority::UnrepresentedAuthorityOwner(owner) => {
            3_u8.hash(hasher);
            owner.as_bytes().hash(hasher);
        }
    }
    hash_fact_availability(value.source, hasher);
    hash_fact_availability(value.source_file, hasher);
    hash_fact_availability(value.members, hasher);
    hash_fact_availability(value.semantic_type, hasher);
    hash_fact_availability(value.documentation, hasher);
    hash_fact_availability(value.visibility, hasher);
    hash_fact_availability(value.attributes, hasher);
    hash_fact_availability(value.language_extension, hasher);
}

fn hash_image_provenance(value: ImageProvenance, hasher: &mut StableHasher) {
    match value {
        ImageProvenance::Unavailable => 0_u8.hash(hasher),
        ImageProvenance::Captured {
            source,
            recipe,
            claim,
            scope,
        } => {
            1_u8.hash(hasher);
            source.hash_into(hasher);
            digest_recipe(recipe).hash(hasher);
            claim.identity.hash(hasher);
            scope.ecosystem.hash(hasher);
            scope.package.hash(hasher);
            scope.path.hash(hasher);
        }
    }
}

fn hash_census(value: backend_semantic::ir::SemanticCensus, hasher: &mut StableHasher) {
    value.entities.hash(hasher);
    value.atoms.hash(hasher);
    value.compact_type_nodes.hash(hasher);
    value.canonical_products.hash(hasher);
    value.canonical_product_children.hash(hasher);
    value.canonical_entity_roots.hash(hasher);
    value.occurrences.hash(hasher);
    value.declared_type_facts.hash(hasher);
    value.computed_type_facts.hash(hasher);
    value.documentation_fragments.hash(hasher);
    value.language_extension_section.hash(hasher);
    value.extension_pool_section.hash(hasher);
}

fn hash_availability_census(
    value: backend_semantic::ir::AvailabilityCensus,
    hasher: &mut StableHasher,
) {
    value.captured.hash(hasher);
    value.unavailable.hash(hasher);
}

fn hash_semantic_census(
    value: backend_semantic::ir::SemanticImageCensus,
    hasher: &mut StableHasher,
) {
    hash_semantic_image_facts(value.image, hasher);
    value.entities.hash(hasher);
    value.root_entities.hash(hasher);
    value.typed_entities.hash(hasher);
    value.source_bound_entities.hash(hasher);
    value.member_references.hash(hasher);
    value.documentation_fragments.hash(hasher);
    value.attribute_references.hash(hasher);
    value.types.hash(hasher);
    value.external_targets.hash(hasher);
    value.links.hash(hasher);
    value.link_occurrences.hash(hasher);
    hash_availability_census(value.occurrence_source_authority, hasher);
    value.language_extensions.typescript.hash(hasher);
    value.language_extensions.csharp.hash(hasher);
    value.language_extensions.go.hash(hasher);
    value.language_extensions.rust.hash(hasher);
    value.language_extensions.python.hash(hasher);
    value.language_extensions.java.hash(hasher);
    value.language_extensions.clang.hash(hasher);
    let authority = value.entity_authority;
    authority.parentage.unavailable.hash(hasher);
    authority.parentage.roots.hash(hasher);
    authority.parentage.bound.hash(hasher);
    authority
        .parentage
        .unrepresented_authority_owner
        .hash(hasher);
    hash_availability_census(authority.source, hasher);
    hash_availability_census(authority.source_file, hasher);
    hash_availability_census(authority.members, hasher);
    hash_availability_census(authority.semantic_type, hasher);
    hash_availability_census(authority.documentation, hasher);
    hash_availability_census(authority.visibility, hasher);
    hash_availability_census(authority.attributes, hasher);
    hash_availability_census(authority.language_extension, hasher);
}

fn hash_semantic_observation(value: &SemanticObservation, hasher: &mut StableHasher) {
    match value.status {
        SemanticReaderStatus::Captured => 0_u8.hash(hasher),
        SemanticReaderStatus::ObserverUnavailable => 1_u8.hash(hasher),
        SemanticReaderStatus::Unsupported => 2_u8.hash(hasher),
    }
    value.identity.hash(hasher);
    match value.image.as_ref() {
        Some(image) => {
            1_u8.hash(hasher);
            hash_observed_image(image, hasher);
        }
        None => 0_u8.hash(hasher),
    }
    match value.census.as_ref() {
        Some(census) => {
            1_u8.hash(hasher);
            hash_observed_census(census, hasher);
        }
        None => 0_u8.hash(hasher),
    }
    value.entities.hash(hasher);
    value.types.hash(hasher);
    value.externals.hash(hasher);
    value.links.hash(hasher);
    value.occurrences.hash(hasher);
    value.extensions.hash(hasher);
    value.source_spans.hash(hasher);
    digest_render(value.canonical_type).hash(hasher);
}

pub(super) fn digest_semantic(value: &SemanticObservation) -> Digest {
    let mut hasher = StableHasher::default();
    hash_semantic_observation(value, &mut hasher);
    hasher.digest()
}

pub(super) fn digest_recipe(recipe: backend_semantic::vocabulary::CompileRecipeFact) -> Digest {
    let mut hasher = StableHasher::default();
    recipe.identity.hash(&mut hasher);
    recipe.profile.hash(&mut hasher);
    u8::from(recipe.stage).hash(&mut hasher);
    u8::from(recipe.tool).hash(&mut hasher);
    recipe.toolchain.hash(&mut hasher);
    hasher.digest()
}

pub(super) fn digest_source(source: SourceIdentity) -> Digest {
    let mut hasher = StableHasher::default();
    source.hash_into(&mut hasher);
    hasher.digest()
}

fn hash_entity_observation(value: Option<EntityObservation>, hasher: &mut StableHasher) {
    let Some(value) = value else {
        0_u8.hash(hasher);
        return;
    };
    1_u8.hash(hasher);
    value.id.hash(hasher);
    value.kind.hash(hasher);
    value.name.hash(hasher);
    value.visibility.hash(hasher);
    value.parent.hash(hasher);
    hash_authority_facts(value.authority, hasher);
    value.members.hash(hasher);
    value.member_order.hash(hasher);
    value.first_member.hash(hasher);
    hash_type_shape(value.type_shape, hasher);
    value.source.hash(hasher);
    value.version.family.hash(hasher);
    value.version.variant.hash(hasher);
    value.version.core_payload.hash(hasher);
}

pub(super) fn digest_owned(value: &OwnedObservation) -> Digest {
    let mut hasher = StableHasher::default();
    value.entity_count.hash(&mut hasher);
    hash_image_provenance(value.image_provenance, &mut hasher);
    value.primary_matches.hash(&mut hasher);
    hash_entity_observation(value.primary, &mut hasher);
    value.declarations.hash(&mut hasher);
    hash_count_observation(value.generic_parameters, &mut hasher);
    hash_plane_observation(value.documentation, &mut hasher);
    hash_plane_observation(value.attributes, &mut hasher);
    hash_plane_observation(value.source_plane, &mut hasher);
    hash_count_observation(value.occurrences, &mut hasher);
    hash_count_observation(value.links, &mut hasher);
    hash_plane_observation(value.extension, &mut hasher);
    value.semantic_digest.hash(&mut hasher);
    hasher.digest()
}

pub(super) fn digest_compact(value: &CompactObservation) -> Digest {
    let mut hasher = StableHasher::default();
    value.fragment.hash(&mut hasher);
    hash_census(value.census, &mut hasher);
    value.primary_matches.hash(&mut hasher);
    value
        .primary_kind
        .map_or(0_u16, u16::from)
        .hash(&mut hasher);
    value.primary_name.hash(&mut hasher);
    hash_type_node(value.primary_type, &mut hasher);
    hash_plane_observation(value.semantic_data, &mut hasher);
    hash_plane_observation(value.occurrences, &mut hasher);
    hash_plane_observation(value.type_facts, &mut hasher);
    hash_plane_observation(value.documentation, &mut hasher);
    hash_plane_observation(value.extensions, &mut hasher);
    hasher.digest()
}

pub(super) fn digest_reopened(value: &ReopenedObservation) -> Digest {
    let mut hasher = StableHasher::default();
    value.fragment.hash(&mut hasher);
    value.source.hash_into(&mut hasher);
    value.recipe.identity.hash(&mut hasher);
    value.recipe.profile.hash(&mut hasher);
    u8::from(value.recipe.stage).hash(&mut hasher);
    u8::from(value.recipe.tool).hash(&mut hasher);
    value.recipe.toolchain.hash(&mut hasher);
    value.ranges.hash(&mut hasher);
    hash_census(value.census, &mut hasher);
    hash_plane_observation(value.semantic_data, &mut hasher);
    hash_plane_observation(value.occurrences, &mut hasher);
    hash_plane_observation(value.type_facts, &mut hasher);
    hash_plane_observation(value.documentation, &mut hasher);
    hash_plane_observation(value.extensions, &mut hasher);
    hash_semantic_observation(&value.semantic, &mut hasher);
    hasher.digest()
}

pub(super) fn digest_render(value: RenderVerdict) -> Digest {
    let mut hasher = StableHasher::default();
    match value {
        RenderVerdict::Rendered(digest) => {
            0_u8.hash(&mut hasher);
            digest.hash(&mut hasher);
        }
        RenderVerdict::Unsupported => 1_u8.hash(&mut hasher),
        RenderVerdict::Unavailable => 2_u8.hash(&mut hasher),
    }
    hasher.digest()
}

pub(super) fn digest_terminal(value: CompileTerminalKind) -> Digest {
    digest_u64(terminal_code(value))
}

const fn terminal_code(value: CompileTerminalKind) -> u64 {
    match value {
        CompileTerminalKind::SourceLength => 0,
        CompileTerminalKind::UnsupportedStage => 1,
        CompileTerminalKind::ToolchainSelectionMismatch => 2,
        CompileTerminalKind::ToolchainMismatch => 3,
        CompileTerminalKind::NativeWork => 4,
        CompileTerminalKind::NativeWorkCleanup => 5,
        CompileTerminalKind::ToolingUnavailable => 6,
        CompileTerminalKind::ToolStart => 7,
        CompileTerminalKind::MissingToolInput => 8,
        CompileTerminalKind::MissingToolInputCleanup => 9,
        CompileTerminalKind::MissingToolDiagnostic => 10,
        CompileTerminalKind::MissingToolDiagnosticCleanup => 11,
        CompileTerminalKind::ToolInput => 12,
        CompileTerminalKind::ToolInputCleanup => 13,
        CompileTerminalKind::ToolTerminate => 14,
        CompileTerminalKind::ToolWait => 15,
        CompileTerminalKind::ToolWaitCleanup => 16,
        CompileTerminalKind::ToolDiagnosticRead => 17,
        CompileTerminalKind::ToolDiagnosticReadCleanup => 18,
        CompileTerminalKind::NativeWorkerPanic => 19,
        CompileTerminalKind::Cancelled => 20,
        CompileTerminalKind::DeadlineExceeded => 21,
        CompileTerminalKind::DiagnosticLimit => 22,
        CompileTerminalKind::NativeRejected => 23,
        CompileTerminalKind::Authority => 24,
        CompileTerminalKind::AuthorityInputRequired => 25,
        CompileTerminalKind::AuthorityInputProfileMismatch => 26,
        CompileTerminalKind::LoweringUnsupported => 27,
        CompileTerminalKind::ExtensionAtomUnbound => 28,
        CompileTerminalKind::ExtensionTypeParametersUnbound => 29,
        CompileTerminalKind::ClangProjection => 32,
        CompileTerminalKind::Build => 33,
        CompileTerminalKind::Prepare => 34,
        CompileTerminalKind::Write => 35,
        CompileTerminalKind::Validate => 36,
    }
}

pub(super) fn digest_authority_unavailable(value: AuthorityUnavailableCause) -> Digest {
    let mut hasher = StableHasher::default();
    match value {
        AuthorityUnavailableCause::Native(cause) => {
            0_u8.hash(&mut hasher);
            u8::from(cause.tool).hash(&mut hasher);
            let kind = match cause.kind {
                NativeUnavailableKind::MissingPath => 0_u8,
                NativeUnavailableKind::MissingTool => 1,
                NativeUnavailableKind::Canonicalize => 2,
                NativeUnavailableKind::ProbeVersion => 3,
                NativeUnavailableKind::VersionRejected => 4,
                NativeUnavailableKind::EmptyVersion => 5,
                NativeUnavailableKind::Resolve => 6,
            };
            kind.hash(&mut hasher);
        }
        AuthorityUnavailableCause::RustAuthority => 1_u8.hash(&mut hasher),
        AuthorityUnavailableCause::GoOracle => 2_u8.hash(&mut hasher),
        AuthorityUnavailableCause::JavaHarness => 3_u8.hash(&mut hasher),
        AuthorityUnavailableCause::CSharpHelper => 4_u8.hash(&mut hasher),
        AuthorityUnavailableCause::TypeScriptChecker => 6_u8.hash(&mut hasher),
        AuthorityUnavailableCause::PythonChecker => 7_u8.hash(&mut hasher),
        AuthorityUnavailableCause::ObserverUnavailable => 8_u8.hash(&mut hasher),
        AuthorityUnavailableCause::Source(cause) => {
            5_u8.hash(&mut hasher);
            language_code(cause.language).hash(&mut hasher);
            let kind = match cause.kind {
                SourceUnavailableKind::RootUnset => 0_u8,
                SourceUnavailableKind::PackageDirectoryMissing => 1_u8,
                SourceUnavailableKind::SourceFileMissing => 2_u8,
                SourceUnavailableKind::RepositoryFixtureMissing => 3_u8,
                SourceUnavailableKind::ReadFailure => 4_u8,
                SourceUnavailableKind::SourceTooLarge => 5_u8,
                SourceUnavailableKind::SourceTreeTooDeep => 6_u8,
                SourceUnavailableKind::SourceDirectoryEntryCountExceeded => 7_u8,
                SourceUnavailableKind::SourceFileCountExceeded => 8_u8,
            };
            kind.hash(&mut hasher);
        }
        // A worker-process death digests its exact process facts so two
        // different crash shapes (signal versus exit, different signals)
        // never collapse into one availability identity.
        AuthorityUnavailableCause::RowProcessCrash { signal, code } => {
            9_u8.hash(&mut hasher);
            signal.hash(&mut hasher);
            code.hash(&mut hasher);
        }
        // A not-compared observer plane is its own availability identity so a
        // lane that cannot ground a plane never shares a digest with a row
        // that was verified or with a row whose authority was absent.
        AuthorityUnavailableCause::NotCompared => 10_u8.hash(&mut hasher),
    }
    hasher.digest()
}

const fn language_code(value: CorpusLanguage) -> u8 {
    match value {
        CorpusLanguage::Rust => 0,
        CorpusLanguage::TypeScript => 1,
        CorpusLanguage::Python => 2,
        CorpusLanguage::Go => 3,
        CorpusLanguage::Java => 4,
        CorpusLanguage::CSharp => 5,
        CorpusLanguage::Clang => 6,
    }
}

/// Byte-stability pins for the reopened observation digests.
///
/// The decoder may change representation (for example indexing typed edges by
/// role), but the hashed observation of an existing image must stay
/// byte-identical.  These tests rebuild one deterministic image per lane,
/// reopen it, observe it, and compare every lane digest against the pinned
/// constants recorded before the decoder change.
#[cfg(test)]
mod reopened_digest_pins {
    use super::*;

    use backend_semantic::ir::{
        CSharpFacts, CSharpMemberEffects, CSharpNullability, CSharpPartialRole,
        CSharpReferenceKind, CSharpVersion, ClangLayout, ClangQualifiers, ClangStorageClass,
        ConcreteType, CorePayloadHash, DeclarationFamilyId, EntityVersion, FreePredicate,
        IrBuilder, ItemKind, LanguageExtensionInput, LanguageProfile, ParentageAuthority,
        RustFacts, RustOwnership, TypeExpr, TypeParameter, TypeParameterBound,
        TypeParameterInference, TypeParameterKind, TypeParameterPrimaryRequirement,
        TypeParameterRequirements, Variance, VariantFingerprint, Visibility,
    };

    fn version(value: u8) -> EntityVersion {
        EntityVersion {
            family: DeclarationFamilyId::from_raw([value; 16]),
            variant: VariantFingerprint::from_raw([value.wrapping_add(1); 16]),
            core_payload: CorePayloadHash::from_raw([value.wrapping_add(2); 16]),
        }
    }

    fn authority() -> backend_semantic::ir::EntityAuthorityFacts {
        backend_semantic::ir::EntityAuthorityFacts {
            parentage: ParentageAuthority::Root,
            visibility: backend_semantic::ir::FactAvailability::Captured,
            semantic_type: backend_semantic::ir::FactAvailability::Captured,
            language_extension: backend_semantic::ir::FactAvailability::Captured,
            ..backend_semantic::ir::EntityAuthorityFacts::default()
        }
    }

    fn item<'facts>(
        version_value: u8,
        name: &'static [u8],
        semantic_type: Option<TypeId>,
        extension: Option<LanguageExtensionInput<'facts>>,
    ) -> backend_semantic::ir::TreeItemInput<'facts> {
        backend_semantic::ir::TreeItemInput {
            name,
            kind: ItemKind::Function,
            visibility: Visibility::Public,
            authority: authority(),
            parent: None,
            semantic_type,
            members: &[],
            docs: &[],
            attributes: &[],
            source: None,
            extension,
        }
    }

    fn pinned_parameter_image() -> Result<backend_semantic::ir::Ir, backend_semantic::ir::BuildError>
    {
        let mut builder = IrBuilder::new();
        builder
            .set_language_profile(LanguageProfile::CSharp(CSharpVersion::CSharp14))
            .expect("csharp profile admitted");
        let name_t = builder.intern_atom(b"T")?;
        let name_u = builder.intern_atom(b"U")?;
        let name_c = builder.intern_atom(b"C")?;
        let lifetime = builder.intern_atom(b"'scope")?;
        let bound_target = builder.intern_type(TypeExpr::Concrete(ConcreteType::Builtin(
            backend_semantic::ir::BuiltinType::String,
        )))?;
        let value_type = builder.intern_type(TypeExpr::Concrete(ConcreteType::Builtin(
            backend_semantic::ir::BuiltinType::U32,
        )))?;
        let default_type = builder.intern_type(TypeExpr::Concrete(ConcreteType::CPointer {
            target: bound_target,
        }))?;
        let bounds = builder.intern_type_parameter_bounds(&[
            TypeParameterBound::Type(bound_target),
            TypeParameterBound::Lifetime(lifetime),
        ])?;
        let empty_bounds = builder.intern_type_parameter_bounds(&[])?;
        let parameters = builder.intern_type_parameters(&[
            TypeParameter {
                name: name_t,
                bounds,
                default: Some(default_type),
                variance: Variance::Covariant,
                kind: TypeParameterKind::Type {
                    inference: TypeParameterInference::Ordinary,
                },
                requirements: TypeParameterRequirements {
                    primary: TypeParameterPrimaryRequirement::Default,
                    constructor: false,
                    allows_ref_like: false,
                },
            },
            TypeParameter {
                name: name_u,
                bounds: empty_bounds,
                default: None,
                variance: Variance::Invariant,
                kind: TypeParameterKind::ConstValue { value_type },
                requirements: TypeParameterRequirements::none(),
            },
            TypeParameter {
                name: name_c,
                bounds,
                default: None,
                variance: Variance::Contravariant,
                kind: TypeParameterKind::Lifetime,
                requirements: TypeParameterRequirements {
                    primary: TypeParameterPrimaryRequirement::Reference { nullable: true },
                    constructor: false,
                    allows_ref_like: false,
                },
            },
        ])?;
        let facts = CSharpFacts {
            nullability: CSharpNullability::Oblivious,
            reference_kind: CSharpReferenceKind::Value,
            constraints: parameters,
            effects: CSharpMemberEffects {
                is_async: true,
                is_iterator: false,
                is_extension: false,
            },
            attributes: builder.intern_attributes(&[])?,
            partial: CSharpPartialRole::None,
            xml_provenance: None,
        };
        let alternate = CSharpFacts {
            nullability: CSharpNullability::Nullable,
            reference_kind: CSharpReferenceKind::Ref,
            ..facts
        };
        let mut items = std::vec::Vec::new();
        for index in 0_u8..4 {
            let parameter_atom = builder.intern_atom(b"payload")?;
            let arguments = builder.intern_types(&[value_type, default_type])?;
            let function_parameters = builder.intern_tuple_elements(&[
                backend_semantic::ir::TupleElement {
                    label: Some(parameter_atom),
                    ty: value_type,
                    kind: backend_semantic::ir::TupleElementKind::Required,
                },
                backend_semantic::ir::TupleElement {
                    label: None,
                    ty: default_type,
                    kind: backend_semantic::ir::TupleElementKind::Rest,
                },
            ])?;
            let function_results = builder.intern_tuple_elements(&[])?;
            let semantic_type = builder.intern_type(match index {
                0 => TypeExpr::Concrete(ConcreteType::Applied {
                    constructor: bound_target,
                    arguments,
                }),
                1 => TypeExpr::Concrete(ConcreteType::Pointer {
                    target: value_type,
                    mutability: backend_semantic::ir::Mutability::Mutable,
                }),
                2 => TypeExpr::Concrete(ConcreteType::Function {
                    parameters: function_parameters,
                    results: function_results,
                    abi: None,
                    variadic: backend_semantic::ir::FunctionVariadicForm::TypedLast,
                    unsafe_: false,
                }),
                _ => TypeExpr::Concrete(ConcreteType::Map {
                    key: value_type,
                    value: default_type,
                }),
            })?;
            let extension = if index % 2 == 0 { &facts } else { &alternate };
            items.push(item(
                index.wrapping_add(1),
                b"pinned_function",
                Some(semantic_type),
                Some(LanguageExtensionInput::CSharp(extension)),
            ));
        }
        let versions = [version(1), version(2), version(3), version(4)];
        builder.add_borrowed_tree(backend_semantic::ir::BorrowedTree {
            versions: &versions,
            items: &items,
            links: &[],
        })?;
        builder.finish()
    }

    fn pinned_free_predicate_image()
    -> Result<backend_semantic::ir::Ir, backend_semantic::ir::BuildError> {
        let mut builder = IrBuilder::new();
        builder
            .set_language_profile(LanguageProfile::Rust(
                backend_semantic::ir::RustEdition::Rust2024,
            ))
            .expect("rust profile admitted");
        let name_t = builder.intern_atom(b"T")?;
        let lifetime = builder.intern_atom(b"'a")?;
        let inner = builder.intern_type(TypeExpr::Concrete(ConcreteType::Parameter(name_t)))?;
        let subject = builder.intern_type(TypeExpr::Concrete(ConcreteType::Slice(inner)))?;
        let bounds = builder.intern_type_parameter_bounds(&[
            TypeParameterBound::Lifetime(lifetime),
            TypeParameterBound::Type(subject),
        ])?;
        let parameters = builder.intern_type_parameters(&[TypeParameter {
            name: name_t,
            bounds,
            default: None,
            variance: Variance::Invariant,
            kind: TypeParameterKind::Type {
                inference: TypeParameterInference::Const,
            },
            requirements: TypeParameterRequirements::none(),
        }])?;
        let predicate_bounds =
            builder.intern_type_parameter_bounds(&[TypeParameterBound::Lifetime(lifetime)])?;
        let predicates = builder.intern_free_predicates(&[FreePredicate {
            subject,
            bounds: predicate_bounds,
        }])?;
        let facts = RustFacts {
            ownership: RustOwnership::SharedBorrow,
            lifetimes: builder.intern_attributes(&[lifetime])?,
            where_clauses: parameters,
            macros: builder.intern_attributes(&[])?,
            const_defaults: builder.intern_attributes(&[])?,
            free_predicates: predicates,
        };
        let items = [item(
            1,
            b"pinned_rust_function",
            Some(subject),
            Some(LanguageExtensionInput::Rust(&facts)),
        )];
        let versions = [version(1)];
        builder.add_borrowed_tree(backend_semantic::ir::BorrowedTree {
            versions: &versions,
            items: &items,
            links: &[],
        })?;
        builder.finish()
    }

    fn reopened_observation(
        ir: &backend_semantic::ir::Ir,
    ) -> Option<(SemanticObservation, [u8; 32])> {
        let length = backend_semantic::ir::full_semantic_image_len(ir).ok()?;
        let mut bytes = std::vec![0_u8; length];
        let written = backend_semantic::ir::encode_full_semantic_image(ir, &mut bytes).ok()?;
        assert_eq!(written, length);
        let image = backend_semantic::ir::SemanticImageView::reopen(&bytes).ok()?;
        let observation = observe_reopened_semantic(&image, None);
        let overall = digest_semantic(&observation);
        Some((observation, overall))
    }

    #[test]
    fn pinned_parameter_lane_observation_is_stable() {
        let ir = pinned_parameter_image().expect("pinned parameter image builds");
        let (observation, overall) = reopened_observation(&ir).expect("reopens and observes");
        assert_eq!(observation.status, SemanticReaderStatus::Captured);
        // Pinned against the pre-indexing linear-scan decoder; any decoder
        // change must keep these bytes identical.
        assert_eq!(
            overall,
            [
                0x32, 0x07, 0xd7, 0xcc, 0x26, 0x8c, 0x93, 0xab, 0x25, 0x26, 0x54, 0x46, 0x81, 0x01,
                0xff, 0xb9, 0x6a, 0xa3, 0x2d, 0x9d, 0xb4, 0xa2, 0xe6, 0x72, 0x40, 0x59, 0xcc, 0x77,
                0xef, 0xd0, 0x46, 0xf6,
            ]
        );
        assert_eq!(
            observation.types,
            [
                0xde, 0xf3, 0x23, 0x43, 0x21, 0x12, 0xd5, 0xa9, 0x95, 0xd7, 0x9f, 0x0d, 0xd1, 0xa4,
                0xae, 0x60, 0x47, 0xc8, 0xf4, 0x39, 0xac, 0x69, 0xc8, 0x5e, 0xed, 0xfb, 0x99, 0x35,
                0x2d, 0x94, 0x96, 0xe3,
            ]
        );
        assert_eq!(
            observation.extensions[1],
            [
                0x9a, 0xc2, 0x0e, 0xe3, 0xe3, 0x97, 0xda, 0x98, 0x18, 0x8f, 0x7c, 0xb7, 0xdd, 0x35,
                0x9a, 0xfb, 0xbe, 0x41, 0xbc, 0xf7, 0xbf, 0x9c, 0xd4, 0xcf, 0x09, 0xd0, 0x36, 0xf9,
                0xec, 0x5e, 0xe0, 0x19,
            ]
        );
    }

    #[test]
    fn pinned_free_predicate_lane_observation_is_stable() {
        let ir = pinned_free_predicate_image().expect("pinned predicate image builds");
        let (observation, overall) = reopened_observation(&ir).expect("reopens and observes");
        assert_eq!(observation.status, SemanticReaderStatus::Captured);
        assert_eq!(
            overall,
            [
                0x18, 0x96, 0xc8, 0xef, 0xab, 0xa8, 0xd9, 0xb7, 0xc0, 0x67, 0x08, 0x83, 0xeb, 0xac,
                0x11, 0xcd, 0x73, 0x47, 0xc6, 0xea, 0xdb, 0x97, 0xcb, 0x88, 0xf4, 0xd3, 0xeb, 0x5f,
                0x29, 0x63, 0x09, 0x37,
            ]
        );
        assert_eq!(
            observation.types,
            [
                0x2d, 0xe3, 0x68, 0x87, 0x08, 0xbf, 0x59, 0x13, 0x55, 0x7b, 0x09, 0xea, 0x2f, 0x4e,
                0x80, 0x5b, 0x3f, 0x7e, 0x5e, 0x79, 0x34, 0xb2, 0x94, 0x5e, 0xb4, 0xfe, 0x8f, 0x90,
                0x75, 0xf1, 0x97, 0x20,
            ]
        );
        assert_eq!(
            observation.extensions[3],
            [
                0x78, 0xad, 0x4f, 0x5c, 0xff, 0xf3, 0xef, 0x67, 0x55, 0x65, 0xe3, 0x96, 0xc3, 0x2b,
                0x64, 0x50, 0x88, 0xc2, 0x92, 0xdc, 0xe0, 0x61, 0x94, 0x1b, 0x7c, 0x3d, 0xd5, 0xb6,
                0xd7, 0x23, 0x9e, 0x3c,
            ]
        );
    }
}
