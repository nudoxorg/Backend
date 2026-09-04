//! Typed owned/compact/reopened observations and their domain-separated digests.
//!
//! This boundary reads immutable IR authority columns directly. It preserves
//! captured-empty planes and observer-unavailable planes as distinct values.

use super::*;

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
    pub(super) core_payload: compiler_ir::CorePayloadHash,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
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
    pub(super) census: compiler_ir::SemanticCensus,
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ReopenedObservation {
    pub(super) fragment: Digest,
    pub(super) source: SourceIdentity,
    pub(super) recipe: compiler_vocabulary::CompileRecipeFact,
    pub(super) ranges: Digest,
    pub(super) census: compiler_ir::SemanticCensus,
    pub(super) semantic_data: PlaneObservation,
    pub(super) occurrences: PlaneObservation,
    pub(super) type_facts: PlaneObservation,
    pub(super) documentation: PlaneObservation,
    pub(super) extensions: PlaneObservation,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum RenderVerdict {
    Rendered(Digest),
    Unsupported,
    Unavailable,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct CaseOutputObservation {
    pub(super) source: SourceIdentity,
    pub(super) recipe: compiler_vocabulary::CompileRecipeFact,
    pub(super) owned: OwnedObservation,
    pub(super) compact: CompactObservation,
    pub(super) reopened: ReopenedObservation,
    pub(super) neutral_render: RenderVerdict,
    pub(super) dialect_render: RenderVerdict,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
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

pub(super) fn digest_bytes(bytes: &[u8]) -> Digest {
    let mut hasher = StableHasher::default();
    hasher.write(bytes);
    hasher.digest()
}


pub(super) fn range_manifest_digest(view: &FragmentView<'_>) -> Result<Digest, CorpusAuditError> {
    Ok(range_manifest_digest_from_manifest(FragmentRangeManifest::from_view(view)?))
}

pub(super) fn range_manifest_digest_from_manifest(
    ranges: FragmentRangeManifest,
) -> Digest {
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
    }
}

pub(super) fn version_observation(version: compiler_ir::EntityVersion) -> VersionObservation {
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
        TypeExpr::Concrete(ConcreteType::Builtin(builtin)) => {
            ObservedTypeShape::Primitive(builtin)
        }
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

fn generic_parameters(
    ir: &Ir,
    language: CorpusLanguage,
    entity: EntityId,
) -> CountObservation {
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
        .filter(|item| item.kind() == expected_kind && item.name() == rendered.expected_symbol.as_bytes())
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
        primary_type: primary.and_then(|entity| fragment.type_nodes().nth(entity.semantic_type.index())),
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
        extensions: if discovery.language_extensions().is_ok_and(|value| value.is_some()) {
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
    let Some(signature) = ir.signature(primary.id) else {
        return RenderVerdict::Unavailable;
    };
    let mut text = String::new();
    if write!(&mut text, "{signature}").is_err() {
        return RenderVerdict::Unavailable;
    }
    RenderVerdict::Rendered(digest_bytes(text.as_bytes()))
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
            primitive.hash(hasher);
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
            claim.declaration_key.hash(hasher);
            scope.ecosystem.hash(hasher);
            scope.package.hash(hasher);
            scope.path.hash(hasher);
        }
    }
}

fn hash_census(value: compiler_ir::SemanticCensus, hasher: &mut StableHasher) {
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

pub(super) fn digest_recipe(recipe: compiler_vocabulary::CompileRecipeFact) -> Digest {
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

pub(super) fn digest_owned(value: OwnedObservation) -> Digest {
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

pub(super) fn digest_compact(value: CompactObservation) -> Digest {
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

pub(super) fn digest_reopened(value: ReopenedObservation) -> Digest {
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
        CompileTerminalKind::FactRejected => 29,
        CompileTerminalKind::CSharpProjection => 30,
        CompileTerminalKind::ClangProjection => 31,
        CompileTerminalKind::Build => 32,
        CompileTerminalKind::Prepare => 33,
        CompileTerminalKind::Write => 34,
        CompileTerminalKind::Validate => 35,
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
    }
    hasher.digest()
}
