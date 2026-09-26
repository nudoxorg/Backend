use super::builder::{
    IrBuilder, atom, optional_id, validate_atom_list, validate_entity_list,
    validate_free_predicates, validate_type_list, validate_type_parameters,
};
use super::error::{BuildError, LanguageExtensionViolation, SemanticSpace};
use super::ids::{AtomListId, DocId, EntityListId, ItemKind, LinkId, LinkOccurrenceId, TypeListId};
use super::language_facts::LanguageExtensionInput;
use super::language_facts::{
    CSharpExtension, CSharpFacts, ClangExtension, ClangFacts, GoExtension, GoFacts, JavaExtension,
    JavaFacts, PythonExtension, PythonFacts, RustExtension, RustFacts, SemanticImageAuthority,
    TypeScriptExtension, TypeScriptFacts,
};
use super::packed_types::{
    FreePredicate, ObjectMember, TemplatePart, TupleElement, TypeInterner, TypeParameter,
    TypeParameterBound,
};
use super::relations::{
    Confidence, DocFragment, EntityVersion, ExternalTarget, Item, Link,
    LinkKind, LinkOccurrence, LinkTarget, SourceSpan,
};
use super::type_model::TypeColumns;
use super::type_model::Visibility;
use crate::ir::{
    AnnotationKind, AtomId, AtomTable, AtomTableView, AuthorityFactFault,
    AuthorityFactPlane, CapacityError, ChannelDirection, DeclarationFamilyId, DeclarationIdentity,
    DeclarationKey, DenseId, EntityAuthorityColumns, EntityAuthorityFacts, EntityId,
    ExternalDeclarationIdentity, ExternalEntityRef, FactAvailability, ImageProvenance,
    ImageProvenanceClaim, Interner, ListId, ListInterner, ListTable, ListTableView,
    OccurrenceAuthorityColumns, OccurrenceAuthorityFacts, PackageLineage, ParentageAuthority,
    PreimageOverflow, SemanticScopeClaim, SemanticScopeFacts, SourceIdentity, StableRef, TextId,
    Type, TypeId, VariantFingerprint,
    authority::{AuthorityColumns, OccurrenceAuthorityColumn},
    columnar::{RawColumn, Slab, SlabPlan},
    interner::{HashIndex, hash},
};
use crate::vocabulary::Language;
use core::{fmt, hash::Hash, marker::PhantomData};

mod indices;

/// Four-byte niche-packed optional dense coordinate used inside column families.
///
/// `u32::MAX` is reserved for absence. In practice an IR cannot materialize
/// that many rows in one address space, and the sentinel halves each optional
/// coordinate column compared with Rust's general `Option<DenseId<_>>` layout.
#[repr(transparent)]
pub struct OptionalId<Owner> {
    raw: u32,
    owner: PhantomData<fn() -> Owner>,
}

impl<Owner> Copy for OptionalId<Owner> {}

impl<Owner> Clone for OptionalId<Owner> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<Owner> OptionalId<Owner> {
    const NONE: u32 = u32::MAX;

    /// Packs an optional coordinate into its aligned four-byte lane.
    #[must_use]
    pub const fn new(value: Option<DenseId<Owner>>) -> Self {
        Self {
            raw: match value {
                Some(id) => id.raw,
                None => Self::NONE,
            },
            owner: PhantomData,
        }
    }

    /// Expands the sentinel niche into the strongly typed coordinate.
    #[must_use]
    pub const fn get(self) -> Option<DenseId<Owner>> {
        if self.raw == Self::NONE {
            None
        } else {
            Some(DenseId::new(self.raw))
        }
    }
}

impl<Owner> fmt::Debug for OptionalId<Owner> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.get().fmt(formatter)
    }
}
/// Entity-aligned constant-time index into a dense optional extension arena.
///
/// Each entity costs exactly four bytes regardless of `T`. Present values are
/// stored densely once, so large language or model-specific facts never widen
/// the hot entity families.
pub(super) struct SparseColumn<T> {
    pub(super) ordinals: Vec<u32>,
    pub(super) values: Vec<T>,
    pub(super) rows: usize,
}

impl<T> Default for SparseColumn<T> {
    fn default() -> Self {
        Self {
            ordinals: Vec::new(),
            values: Vec::new(),
            rows: 0,
        }
    }
}

impl<T> SparseColumn<T> {
    pub(super) fn reserve(&mut self, rows: usize, values: usize) {
        if values != 0 {
            self.ordinals.reserve(rows);
            self.values.reserve(values);
        }
    }

    pub(super) fn push(&mut self, value: Option<T>) -> Result<(), CapacityError> {
        let ordinal = match value {
            Some(value) => {
                if self.ordinals.is_empty() && self.rows != 0 {
                    self.ordinals.resize(self.rows, u32::MAX);
                }
                let ordinal = u32::try_from(self.values.len()).map_err(|_| CapacityError {
                    space: crate::ir::CapacitySpace::Value,
                    actual: self.values.len(),
                })?;
                self.values.push(value);
                ordinal
            }
            None => u32::MAX,
        };
        if ordinal != u32::MAX || !self.ordinals.is_empty() {
            self.ordinals.push(ordinal);
        }
        self.rows = self.rows.saturating_add(1);
        Ok(())
    }

    pub(super) fn get(&self, entity: EntityId) -> Option<&T> {
        self.get_index(entity.index())
    }

    fn set(&mut self, index: usize, value: Option<T>) -> Result<(), CapacityError> {
        if index >= self.rows {
            return Err(CapacityError {
                space: crate::ir::CapacitySpace::Value,
                actual: index,
            });
        }
        match value {
            Some(value) => {
                if self.ordinals.is_empty() {
                    self.ordinals.resize(self.rows, u32::MAX);
                }
                let known = self.ordinals[index];
                if known == u32::MAX {
                    let ordinal = u32::try_from(self.values.len()).map_err(|_| CapacityError {
                        space: crate::ir::CapacitySpace::Value,
                        actual: self.values.len(),
                    })?;
                    self.values.push(value);
                    self.ordinals[index] = ordinal;
                } else if let Some(slot) = self.values.get_mut(known as usize) {
                    *slot = value;
                }
            }
            None if !self.ordinals.is_empty() => self.ordinals[index] = u32::MAX,
            None => {}
        }
        Ok(())
    }

    fn get_index(&self, index: usize) -> Option<&T> {
        let ordinal = *self.ordinals.get(index)?;
        (ordinal != u32::MAX)
            .then(|| self.values.get(ordinal as usize))
            .flatten()
    }

    pub(super) fn view(&self) -> SparseColumnView<'_, T> {
        SparseColumnView {
            ordinals: &self.ordinals,
            values: &self.values,
            rows: self.rows,
        }
    }
}

/// Borrowed pointer-and-length view of an optional entity extension column.
#[derive(Debug)]
pub struct SparseColumnView<'ir, T> {
    ordinals: &'ir [u32],
    values: &'ir [T],
    rows: usize,
}

impl<T> Copy for SparseColumnView<'_, T> {}

impl<T> Clone for SparseColumnView<'_, T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<'ir, T> SparseColumnView<'ir, T> {
    /// Looks up one extension fact through its aligned entity ordinal.
    #[must_use]
    pub fn get(self, entity: EntityId) -> Option<&'ir T> {
        let ordinal = *self.ordinals.get(entity.index())?;
        (ordinal != u32::MAX)
            .then(|| self.values.get(ordinal as usize))
            .flatten()
    }

    /// Returns the dense present-value arena without scanning absent entities.
    #[must_use]
    pub const fn values(self) -> &'ir [T] {
        self.values
    }

    /// Returns the entity-aligned ordinal lane used for direct joins.
    ///
    /// An empty lane with nonzero [`Self::row_count`] is the canonical encoding
    /// for a universally absent extension, avoiding four zero-information bytes
    /// per entity. Once one value exists, the slice is fully row-aligned.
    #[must_use]
    pub const fn ordinals(self) -> &'ir [u32] {
        self.ordinals
    }

    /// Number of logical entity rows represented, including universal absence.
    #[must_use]
    pub const fn row_count(self) -> usize {
        self.rows
    }
}

/// One language's compact extension plane: entity rows point to a deduplicated
/// typed fact pool rather than carrying language-width facts in the hot row.
pub(super) struct LanguageExtensionPlane<Facts, Space> {
    pub(super) ids: SparseColumn<DenseId<Space>>,
    pub(super) facts: Interner<Facts, Space>,
}

impl<Facts, Space> Default for LanguageExtensionPlane<Facts, Space> {
    fn default() -> Self {
        Self {
            ids: SparseColumn::default(),
            facts: Interner::default(),
        }
    }
}

impl<Facts: Eq + Hash, Space> LanguageExtensionPlane<Facts, Space> {
    pub(super) fn reserve(&mut self, rows: usize, values: usize) {
        self.ids.reserve(rows, values);
        self.facts.reserve(values);
    }

    pub(super) fn push(&mut self, facts: Option<Facts>) -> Result<(), CapacityError> {
        let id = facts.map(|facts| self.facts.intern(facts)).transpose()?;
        self.ids.push(id)
    }

    pub(super) fn get(&self, entity: EntityId) -> Option<&Facts> {
        self.ids
            .get(entity)
            .and_then(|id| self.facts.get(DenseId::new(id.raw)))
    }

    pub(super) fn view(&self) -> LanguageExtensionColumnView<'_, Facts, Space> {
        LanguageExtensionColumnView {
            ids: self.ids.view(),
            facts: self.facts.as_slice(),
        }
    }
}

/// Borrowed per-language plane: aligned IDs and the matching typed fact pool.
#[derive(Debug)]
pub struct LanguageExtensionColumnView<'ir, Facts, Space> {
    /// Entity-aligned compact IDs; universal absence retains an empty lane.
    pub ids: SparseColumnView<'ir, DenseId<Space>>,
    /// Canonical dense fact pool addressed only by [`Self::ids`].
    pub facts: &'ir [Facts],
}

impl<Facts, Space> Copy for LanguageExtensionColumnView<'_, Facts, Space> {}

impl<Facts, Space> Clone for LanguageExtensionColumnView<'_, Facts, Space> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<'ir, Facts, Space> LanguageExtensionColumnView<'ir, Facts, Space> {
    /// Resolves one entity's typed language fact without allocating or scanning.
    #[must_use]
    pub fn get(self, entity: EntityId) -> Option<&'ir Facts> {
        self.ids
            .get(entity)
            .and_then(|id| self.facts.get(DenseId::<Space>::new(id.raw).index()))
    }
}

/// All closed language-extension planes owned by one semantic IR.
///
/// There is deliberately no erased extension map or tag/payload union. Every
/// language has a distinct sparse directory kind and a fact pool with its own
/// coordinate type, making cross-language substitution unrepresentable in the
/// in-memory model.
#[derive(Default)]
pub(super) struct LanguageExtensions {
    pub(super) typescript: LanguageExtensionPlane<TypeScriptFacts, TypeScriptExtension>,
    pub(super) csharp: LanguageExtensionPlane<CSharpFacts, CSharpExtension>,
    pub(super) go: LanguageExtensionPlane<GoFacts, GoExtension>,
    pub(super) rust: LanguageExtensionPlane<RustFacts, RustExtension>,
    pub(super) python: LanguageExtensionPlane<PythonFacts, PythonExtension>,
    pub(super) java: LanguageExtensionPlane<JavaFacts, JavaExtension>,
    pub(super) clang: LanguageExtensionPlane<ClangFacts, ClangExtension>,
}

impl LanguageExtensions {
    pub(super) fn reserve(&mut self, rows: usize, values: LanguageExtensionCounts) {
        self.typescript.reserve(rows, values.typescript);
        self.csharp.reserve(rows, values.csharp);
        self.go.reserve(rows, values.go);
        self.rust.reserve(rows, values.rust);
        self.python.reserve(rows, values.python);
        self.java.reserve(rows, values.java);
        self.clang.reserve(rows, values.clang);
    }

    pub(super) fn push(
        &mut self,
        input: Option<LanguageExtensionInput<'_>>,
    ) -> Result<(), CapacityError> {
        match input {
            Some(LanguageExtensionInput::TypeScript(facts)) => {
                self.typescript.push(Some(*facts))?;
                self.csharp.push(None)?;
                self.go.push(None)?;
                self.rust.push(None)?;
                self.python.push(None)?;
                self.java.push(None)?;
                self.clang.push(None)
            }
            Some(LanguageExtensionInput::CSharp(facts)) => {
                self.typescript.push(None)?;
                self.csharp.push(Some(*facts))?;
                self.go.push(None)?;
                self.rust.push(None)?;
                self.python.push(None)?;
                self.java.push(None)?;
                self.clang.push(None)
            }
            Some(LanguageExtensionInput::Go(facts)) => {
                self.typescript.push(None)?;
                self.csharp.push(None)?;
                self.go.push(Some(*facts))?;
                self.rust.push(None)?;
                self.python.push(None)?;
                self.java.push(None)?;
                self.clang.push(None)
            }
            Some(LanguageExtensionInput::Rust(facts)) => {
                self.typescript.push(None)?;
                self.csharp.push(None)?;
                self.go.push(None)?;
                self.rust.push(Some(*facts))?;
                self.python.push(None)?;
                self.java.push(None)?;
                self.clang.push(None)
            }
            Some(LanguageExtensionInput::Python(facts)) => {
                self.typescript.push(None)?;
                self.csharp.push(None)?;
                self.go.push(None)?;
                self.rust.push(None)?;
                self.python.push(Some(*facts))?;
                self.java.push(None)?;
                self.clang.push(None)
            }
            Some(LanguageExtensionInput::Java(facts)) => {
                self.typescript.push(None)?;
                self.csharp.push(None)?;
                self.go.push(None)?;
                self.rust.push(None)?;
                self.python.push(None)?;
                self.java.push(Some(*facts))?;
                self.clang.push(None)
            }
            Some(LanguageExtensionInput::Clang(facts)) => {
                self.typescript.push(None)?;
                self.csharp.push(None)?;
                self.go.push(None)?;
                self.rust.push(None)?;
                self.python.push(None)?;
                self.java.push(None)?;
                self.clang.push(Some(*facts))
            }
            None => {
                self.typescript.push(None)?;
                self.csharp.push(None)?;
                self.go.push(None)?;
                self.rust.push(None)?;
                self.python.push(None)?;
                self.java.push(None)?;
                self.clang.push(None)
            }
        }
    }

    pub(super) fn view(&self, authority: SemanticImageAuthority) -> LanguageExtensionsView<'_> {
        LanguageExtensionsView {
            authority,
            typescript: self.typescript.view(),
            csharp: self.csharp.view(),
            go: self.go.view(),
            rust: self.rust.view(),
            python: self.python.view(),
            java: self.java.view(),
            clang: self.clang.view(),
        }
    }

    pub(super) fn validate_entity(
        &self,
        builder: &IrBuilder,
        entity: EntityId,
    ) -> Result<(), BuildError> {
        if let Some(facts) = self.typescript.get(entity).copied() {
            validate_type_parameters(builder, facts.type_parameters)?;
            optional_id(facts.declared, builder.types.len(), SemanticSpace::Type)?;
            optional_id(facts.observed, builder.types.len(), SemanticSpace::Type)?;
        }
        if let Some(facts) = self.csharp.get(entity).copied() {
            validate_type_parameters(builder, facts.constraints)?;
            validate_atom_list(builder, facts.attributes)?;
            if let Some(span) = facts.xml_provenance {
                atom(builder, span.file())?;
            }
        }
        if let Some(facts) = self.go.get(entity).copied() {
            validate_type_list(builder, facts.signature.parameters)?;
            validate_type_list(builder, facts.signature.results)?;
            validate_type_parameters(builder, facts.type_parameters)?;
            validate_entity_list(builder, facts.fields)?;
            validate_entity_list(builder, facts.method_set)?;
            validate_atom_list(builder, facts.build_constraints)?;
            validate_atom_list(builder, facts.constant_value)?;
        }
        if let Some(facts) = self.rust.get(entity).copied() {
            validate_atom_list(builder, facts.lifetimes)?;
            validate_type_parameters(builder, facts.where_clauses)?;
            validate_atom_list(builder, facts.macros)?;
            validate_atom_list(builder, facts.const_defaults)?;
            validate_free_predicates(builder, facts.free_predicates)?;
        }
        if let Some(facts) = self.python.get(entity).copied() {
            validate_atom_list(builder, facts.decorators)?;
        }
        if let Some(facts) = self.java.get(entity).copied() {
            validate_type_list(builder, facts.throws)?;
            validate_atom_list(builder, facts.annotations)?;
            validate_entity_list(builder, facts.overloads)?;
            validate_entity_list(builder, facts.record_components)?;
        }
        if let Some(facts) = self.clang.get(entity).copied() {
            if facts.layout.align_bits == Some(0) {
                return Err(BuildError::LanguageExtension {
                    language: Language::Clang,
                    entity,
                    violation: LanguageExtensionViolation::ZeroLayoutAlignment,
                });
            }
            validate_type_parameters(builder, facts.templates)?;
            validate_atom_list(builder, facts.includes)?;
        }
        Ok(())
    }

    pub(super) fn has_entity(&self, entity: EntityId) -> bool {
        self.typescript.get(entity).is_some()
            || self.csharp.get(entity).is_some()
            || self.go.get(entity).is_some()
            || self.rust.get(entity).is_some()
            || self.python.get(entity).is_some()
            || self.java.get(entity).is_some()
            || self.clang.get(entity).is_some()
    }
}
/// Borrowed complete language-extension directory from an [`Ir`].
#[derive(Clone, Copy, Debug)]
pub struct LanguageExtensionsView<'ir> {
    /// The profile authority that selected every nonempty plane.
    pub authority: SemanticImageAuthority,
    pub typescript: LanguageExtensionColumnView<'ir, TypeScriptFacts, TypeScriptExtension>,
    pub csharp: LanguageExtensionColumnView<'ir, CSharpFacts, CSharpExtension>,
    pub go: LanguageExtensionColumnView<'ir, GoFacts, GoExtension>,
    pub rust: LanguageExtensionColumnView<'ir, RustFacts, RustExtension>,
    pub python: LanguageExtensionColumnView<'ir, PythonFacts, PythonExtension>,
    pub java: LanguageExtensionColumnView<'ir, JavaFacts, JavaExtension>,
    pub clang: LanguageExtensionColumnView<'ir, ClangFacts, ClangExtension>,
}
/// Present-value counts measured before a borrowed frontend stream is lowered.
#[derive(Clone, Copy, Default)]
pub(super) struct LanguageExtensionCounts {
    pub(super) typescript: usize,
    pub(super) csharp: usize,
    pub(super) go: usize,
    pub(super) rust: usize,
    pub(super) python: usize,
    pub(super) java: usize,
    pub(super) clang: usize,
}

impl LanguageExtensionCounts {
    pub(super) fn observe(&mut self, input: Option<LanguageExtensionInput<'_>>) {
        match input {
            Some(LanguageExtensionInput::TypeScript(_)) => self.typescript += 1,
            Some(LanguageExtensionInput::CSharp(_)) => self.csharp += 1,
            Some(LanguageExtensionInput::Go(_)) => self.go += 1,
            Some(LanguageExtensionInput::Rust(_)) => self.rust += 1,
            Some(LanguageExtensionInput::Python(_)) => self.python += 1,
            Some(LanguageExtensionInput::Java(_)) => self.java += 1,
            Some(LanguageExtensionInput::Clang(_)) => self.clang += 1,
            None => {}
        }
    }
}

/// Hot entity columns. Scans touch only the lanes required by a query.
pub(super) struct ItemColumns {
    pub(super) _slab: Slab,
    pub(super) names: RawColumn<AtomId>,
    pub(super) kinds: RawColumn<ItemKind>,
    pub(super) visibility: RawColumn<Visibility>,
    pub(super) parents: RawColumn<OptionalId<crate::ir::Entity>>,
    pub(super) semantic_types: RawColumn<OptionalId<Type>>,
    pub(super) members: RawColumn<EntityListId>,
    pub(super) docs: RawColumn<DocId>,
    pub(super) attributes: RawColumn<AtomListId>,
    pub(super) versions: RawColumn<EntityVersion>,
}

impl Default for ItemColumns {
    fn default() -> Self {
        Self::with_capacity(0)
    }
}

impl ItemColumns {
    pub(super) fn with_capacity(capacity: usize) -> Self {
        let mut plan = SlabPlan::default();
        let names = plan.column::<AtomId>(capacity);
        let kinds = plan.column::<ItemKind>(capacity);
        let visibility = plan.column::<Visibility>(capacity);
        let parents = plan.column::<OptionalId<crate::ir::Entity>>(capacity);
        let semantic_types = plan.column::<OptionalId<Type>>(capacity);
        let members = plan.column::<EntityListId>(capacity);
        let docs = plan.column::<DocId>(capacity);
        let attributes = plan.column::<AtomListId>(capacity);
        let versions = plan.column::<EntityVersion>(capacity);
        let slab = plan.allocate();
        Self {
            names: slab.bind(names),
            kinds: slab.bind(kinds),
            visibility: slab.bind(visibility),
            parents: slab.bind(parents),
            semantic_types: slab.bind(semantic_types),
            members: slab.bind(members),
            docs: slab.bind(docs),
            attributes: slab.bind(attributes),
            versions: slab.bind(versions),
            _slab: slab,
        }
    }

    pub(super) fn len(&self) -> usize {
        self.names.len()
    }

    pub(super) fn push(&mut self, version: EntityVersion, item: Item) {
        self.names.push(item.name);
        self.kinds.push(item.kind);
        self.visibility.push(item.visibility);
        self.parents.push(OptionalId::new(item.parent));
        self.semantic_types
            .push(OptionalId::new(item.semantic_type));
        self.members.push(item.members);
        self.docs.push(item.docs);
        self.attributes.push(item.attributes);
        self.versions.push(version);
    }

    pub(super) fn reserve_exact(&mut self, additional: usize) {
        let required = self.len().saturating_add(additional);
        if required <= self.names.capacity() {
            return;
        }
        let mut next = Self::with_capacity(required);
        next.names.extend_from_slice(&self.names);
        next.kinds.extend_from_slice(&self.kinds);
        next.visibility.extend_from_slice(&self.visibility);
        next.parents.extend_from_slice(&self.parents);
        next.semantic_types.extend_from_slice(&self.semantic_types);
        next.members.extend_from_slice(&self.members);
        next.docs.extend_from_slice(&self.docs);
        next.attributes.extend_from_slice(&self.attributes);
        next.versions.extend_from_slice(&self.versions);
        *self = next;
    }

    pub(super) fn reserve_one(&mut self) {
        if self.len() < self.names.capacity() {
            return;
        }
        let target = self.names.capacity().saturating_mul(2).max(8);
        self.reserve_exact(target.saturating_sub(self.len()));
    }
}

/// Cold source-location columns, loaded only by diagnostics or source links.
#[derive(Default)]
pub(super) struct SourceColumns {
    pub(super) files: Vec<OptionalId<crate::ir::AtomSpace>>,
    pub(super) starts: Vec<u32>,
    pub(super) ends: Vec<u32>,
    pub(super) rows: usize,
}

impl SourceColumns {
    pub(super) fn reserve(&mut self, rows: usize, values: usize) {
        if values != 0 {
            self.files.reserve(rows);
            self.starts.reserve(rows);
            self.ends.reserve(rows);
        }
    }

    pub(super) fn push(&mut self, source: Option<SourceSpan>) {
        if source.is_some() && self.files.is_empty() && self.rows != 0 {
            self.files
                .resize(self.rows, OptionalId::<crate::ir::AtomSpace>::new(None));
            self.starts.resize(self.rows, 0);
            self.ends.resize(self.rows, 0);
        }
        if source.is_some() || !self.files.is_empty() {
            self.files
                .push(OptionalId::new(source.map(SourceSpan::file)));
            self.starts.push(source.map_or(0, SourceSpan::start));
            self.ends.push(source.map_or(0, SourceSpan::end));
        }
        self.rows = self.rows.saturating_add(1);
    }

    pub(super) fn get(&self, index: usize) -> Option<SourceSpan> {
        SourceSpan::new(
            self.files.get(index).copied()?.get()?,
            *self.starts.get(index)?,
            *self.ends.get(index)?,
        )
    }
}
pub(super) struct IrIndices {
    pub(super) _slab: Slab,
    pub(super) instances: RawColumn<EntityId>,
    pub(super) kind: RawColumn<EntityId>,
    pub(super) name: RawColumn<EntityId>,
    pub(super) canonical_links: RawColumn<LinkId>,
    pub(super) outgoing: RawColumn<LinkId>,
    pub(super) outgoing_offsets: RawColumn<u32>,
    pub(super) incoming: RawColumn<LinkId>,
    pub(super) incoming_offsets: RawColumn<u32>,
    pub(super) occurrence_outgoing: RawColumn<LinkOccurrenceId>,
    pub(super) occurrence_outgoing_offsets: RawColumn<u32>,
}

/// Dense entity column family. Every slice has the same length and uses
/// [`EntityId`] as its direct array coordinate.
#[derive(Clone, Copy, Debug)]
pub struct EntityColumns<'ir> {
    pub names: &'ir [AtomId],
    pub kinds: &'ir [ItemKind],
    pub visibility: &'ir [Visibility],
    pub parents: &'ir [OptionalId<crate::ir::Entity>],
    pub semantic_types: &'ir [OptionalId<Type>],
    pub members: &'ir [EntityListId],
    pub docs: &'ir [DocId],
    pub attributes: &'ir [AtomListId],
}

/// Cold source column family aligned one-for-one with [`EntityColumns`].
#[derive(Clone, Copy, Debug)]
pub struct SourceColumnsView<'ir> {
    /// File lane, empty when every row has no source.
    pub files: &'ir [OptionalId<crate::ir::AtomSpace>],
    /// Start lane, empty when every row has no source.
    pub starts: &'ir [u32],
    /// End lane, empty when every row has no source.
    pub ends: &'ir [u32],
    pub(in crate::ir::semantic) rows: usize,
}

impl SourceColumnsView<'_> {
    /// Number of logical entity rows, including universal absence.
    #[must_use]
    pub const fn row_count(self) -> usize {
        self.rows
    }
}

/// Precomputed graph columns. Consumers traverse CSR slices directly; no
/// projection, sorting, or adjacency construction occurs when this view is made.
#[derive(Clone, Copy, Debug)]
pub struct GraphColumns<'ir> {
    /// Canonical relation rows, unique by `(from, target, kind)`.
    pub from: &'ir [EntityId],
    pub targets: &'ir [LinkTarget],
    pub kinds: &'ir [LinkKind],
    pub confidence: &'ir [Confidence],
    pub sources: SparseColumnView<'ir, SourceSpan>,
    pub outgoing: &'ir [LinkId],
    pub outgoing_offsets: &'ir [u32],
    pub incoming: &'ir [LinkId],
    pub incoming_offsets: &'ir [u32],
    /// Every observed source occurrence, including repeated sites for one
    /// canonical relation. Its CSR index is keyed by the relation's `from`.
    pub occurrences: LinkOccurrenceColumns<'ir>,
}

/// Typed dense columns for authority-observed graph source sites.
#[derive(Clone, Copy, Debug)]
pub struct LinkOccurrenceColumns<'ir> {
    pub links: &'ir [LinkId],
    pub confidence: &'ir [Confidence],
    pub sources: SparseColumnView<'ir, SourceSpan>,
    pub outgoing: &'ir [LinkOccurrenceId],
    pub outgoing_offsets: &'ir [u32],
}

pub(super) struct PackedLinks {
    pub(super) _slab: Slab,
    pub(super) from: RawColumn<EntityId>,
    pub(super) targets: RawColumn<LinkTarget>,
    pub(super) kinds: RawColumn<LinkKind>,
    pub(super) confidence: RawColumn<Confidence>,
    pub(super) sources: SparseColumn<SourceSpan>,
}

/// Compact site-evidence plane parallel to canonical graph links.
///
/// This is deliberately not interned: two equal source spans are still two
/// independent authority observations if they were emitted separately.
pub(super) struct PackedLinkOccurrences {
    pub(super) _slab: Slab,
    pub(super) links: RawColumn<LinkId>,
    pub(super) confidence: RawColumn<Confidence>,
    pub(super) sources: SparseColumn<SourceSpan>,
}

impl Default for PackedLinkOccurrences {
    fn default() -> Self {
        Self::with_capacity(0)
    }
}

impl PackedLinkOccurrences {
    pub(super) fn with_capacity(capacity: usize) -> Self {
        let mut plan = SlabPlan::default();
        let links = plan.column::<LinkId>(capacity);
        let confidence = plan.column::<Confidence>(capacity);
        let slab = plan.allocate();
        Self {
            links: slab.bind(links),
            confidence: slab.bind(confidence),
            _slab: slab,
            sources: SparseColumn::default(),
        }
    }

    pub(super) fn len(&self) -> usize {
        self.links.len()
    }

    pub(super) fn reserve_exact(&mut self, additional: usize, source_values: usize) {
        let required = self.len().saturating_add(additional);
        if required > self.links.capacity() {
            let mut next = Self::with_capacity(required);
            next.links.extend_from_slice(&self.links);
            next.confidence.extend_from_slice(&self.confidence);
            core::mem::swap(&mut next.sources, &mut self.sources);
            *self = next;
        }
        self.sources.reserve(required, source_values);
    }

    pub(super) fn reserve_one(&mut self, source_values: usize) {
        if self.len() < self.links.capacity() {
            self.sources
                .reserve(self.len().saturating_add(1), source_values);
            return;
        }
        let target = self.links.capacity().saturating_mul(2).max(8);
        self.reserve_exact(target.saturating_sub(self.len()), source_values);
    }

    pub(super) fn push(&mut self, occurrence: LinkOccurrence) -> Result<(), CapacityError> {
        self.links.push(occurrence.link);
        self.confidence.push(occurrence.confidence);
        self.sources.push(occurrence.source)
    }

    pub(super) fn get(&self, id: LinkOccurrenceId) -> Option<LinkOccurrence> {
        let index = id.index();
        Some(LinkOccurrence {
            link: *self.links.get(index)?,
            confidence: *self.confidence.get(index)?,
            source: self.sources.get_index(index).copied(),
        })
    }

    pub(super) fn view(&self) -> LinkOccurrenceColumns<'_> {
        LinkOccurrenceColumns {
            links: &self.links,
            confidence: &self.confidence,
            sources: self.sources.view(),
            outgoing: &[],
            outgoing_offsets: &[],
        }
    }
}

impl Default for PackedLinks {
    fn default() -> Self {
        Self::with_capacity(0)
    }
}

impl PackedLinks {
    pub(super) fn with_capacity(capacity: usize) -> Self {
        let mut plan = SlabPlan::default();
        let from = plan.column::<EntityId>(capacity);
        let targets = plan.column::<LinkTarget>(capacity);
        let kinds = plan.column::<LinkKind>(capacity);
        let confidence = plan.column::<Confidence>(capacity);
        let slab = plan.allocate();
        Self {
            from: slab.bind(from),
            targets: slab.bind(targets),
            kinds: slab.bind(kinds),
            confidence: slab.bind(confidence),
            _slab: slab,
            sources: SparseColumn::default(),
        }
    }

    pub(super) fn len(&self) -> usize {
        self.from.len()
    }

    pub(super) fn reserve_exact(&mut self, additional: usize, source_values: usize) {
        let required = self.len().saturating_add(additional);
        if required > self.from.capacity() {
            let mut next = Self::with_capacity(required);
            next.from.extend_from_slice(&self.from);
            next.targets.extend_from_slice(&self.targets);
            next.kinds.extend_from_slice(&self.kinds);
            next.confidence.extend_from_slice(&self.confidence);
            core::mem::swap(&mut next.sources, &mut self.sources);
            *self = next;
        }
        self.sources.reserve(required, source_values);
    }

    pub(super) fn reserve_one(&mut self, source_values: usize) {
        if self.len() < self.from.capacity() {
            self.sources
                .reserve(self.len().saturating_add(1), source_values);
            return;
        }
        let target = self.from.capacity().saturating_mul(2).max(8);
        self.reserve_exact(target.saturating_sub(self.len()), source_values);
    }

    pub(super) fn push(&mut self, link: Link) -> Result<(), CapacityError> {
        self.from.push(link.from);
        self.targets.push(link.target);
        self.kinds.push(link.kind);
        self.confidence.push(link.confidence);
        self.sources.push(link.source)
    }

    pub(super) fn replace(&mut self, id: LinkId, link: Link) -> Result<(), CapacityError> {
        let index = id.index();
        let Some(from) = self.from.as_mut_slice().get_mut(index) else {
            return Err(CapacityError {
                space: crate::ir::CapacitySpace::Value,
                actual: index,
            });
        };
        *from = link.from;
        self.targets.as_mut_slice()[index] = link.target;
        self.kinds.as_mut_slice()[index] = link.kind;
        self.confidence.as_mut_slice()[index] = link.confidence;
        self.sources.set(index, link.source)
    }

    pub(super) fn get(&self, id: LinkId) -> Option<Link> {
        let index = id.index();
        Some(Link {
            from: *self.from.get(index)?,
            target: *self.targets.get(index)?,
            kind: *self.kinds.get(index)?,
            confidence: *self.confidence.get(index)?,
            source: self.sources.get_index(index).copied(),
        })
    }

    pub(in crate::ir::semantic) fn view<'ir>(
        &'ir self,
        occurrences: LinkOccurrenceColumns<'ir>,
    ) -> GraphColumns<'ir> {
        GraphColumns {
            from: &self.from,
            targets: &self.targets,
            kinds: &self.kinds,
            confidence: &self.confidence,
            sources: self.sources.view(),
            outgoing: &[],
            outgoing_offsets: &[],
            incoming: &[],
            incoming_offsets: &[],
            occurrences,
        }
    }
}

/// Precomputed stable-order columns consumed directly by IR-VCS and storage.
#[derive(Clone, Copy, Debug)]
pub struct VcsColumns<'ir> {
    pub versions: &'ir [EntityVersion],
    /// Canonical exact local instance order, by `(family, variant)`.
    pub declaration_instances: &'ir [EntityId],
    pub stable_links: &'ir [LinkId],
}

/// Complete pointer-and-length storage view of the canonical semantic image.
///
/// This is a manifest of the actual compiler-owned columns, not a wire model.
/// In-process caches, pagers, renderers, and derived-index builders can retain
/// or scatter/gather these exact slices without serializing semantic rows.
#[derive(Clone, Copy, Debug)]
pub struct StorageColumns<'ir> {
    pub authority: SemanticImageAuthority,
    /// Image-level source, recipe, and scope authority when compiled.
    pub provenance: ImageProvenance,
    pub atoms: AtomTableView<'ir>,
    pub types: TypeColumns<'ir>,
    pub externals: &'ir [ExternalTarget],
    pub type_lists: ListTableView<'ir, TypeId>,
    pub entity_lists: ListTableView<'ir, EntityId>,
    pub atom_lists: ListTableView<'ir, AtomId>,
    pub docs: ListTableView<'ir, DocFragment>,
    pub tuple_elements: ListTableView<'ir, TupleElement>,
    pub object_members: ListTableView<'ir, ObjectMember>,
    pub template_parts: ListTableView<'ir, TemplatePart>,
    pub type_parameter_bounds: ListTableView<'ir, TypeParameterBound>,
    pub type_parameters: ListTableView<'ir, TypeParameter>,
    pub free_predicates: ListTableView<'ir, FreePredicate>,
    pub entities: EntityColumns<'ir>,
    /// Cold authority facts aligned exactly with `entities`.
    pub entity_authority: EntityAuthorityColumns<'ir>,
    /// Cold source availability aligned exactly with graph occurrences.
    pub occurrence_authority: OccurrenceAuthorityColumns<'ir>,
    pub sources: SourceColumnsView<'ir>,
    pub language_extensions: LanguageExtensionsView<'ir>,
    pub graph: GraphColumns<'ir>,
    pub vcs: VcsColumns<'ir>,
    pub kind_entities: &'ir [EntityId],
    pub kind_offsets: &'ir [u32; 16],
    pub name_entities: &'ir [EntityId],
}

impl StorageColumns<'_> {
    /// Exact logical bytes occupied by initialized canonical column elements.
    ///
    /// Vector capacities and allocator metadata are intentionally excluded;
    /// this is the representation payload a pager, capacity benchmark, or
    /// scatter/gather storage owner would retain.
    #[must_use]
    pub fn resident_bytes(self) -> usize {
        let mut total = 0_usize;
        macro_rules! add {
            ($slice:expr) => {
                total = total.saturating_add(core::mem::size_of_val($slice));
            };
        }
        add!(self.atoms.bytes);
        add!(self.atoms.ranges);
        add!(self.types.headers);
        add!(self.types.pairs);
        add!(self.types.triples);
        add!(self.types.quads);
        add!(self.externals);
        add!(self.type_lists.elements);
        add!(self.type_lists.ranges);
        add!(self.entity_lists.elements);
        add!(self.entity_lists.ranges);
        add!(self.atom_lists.elements);
        add!(self.atom_lists.ranges);
        add!(self.docs.elements);
        add!(self.docs.ranges);
        add!(self.tuple_elements.elements);
        add!(self.tuple_elements.ranges);
        add!(self.object_members.elements);
        add!(self.object_members.ranges);
        add!(self.template_parts.elements);
        add!(self.template_parts.ranges);
        add!(self.type_parameter_bounds.elements);
        add!(self.type_parameter_bounds.ranges);
        add!(self.type_parameters.elements);
        add!(self.type_parameters.ranges);
        add!(self.free_predicates.elements);
        add!(self.free_predicates.ranges);
        add!(self.entities.names);
        add!(self.entities.kinds);
        add!(self.entities.visibility);
        add!(self.entities.parents);
        add!(self.entities.semantic_types);
        add!(self.entities.members);
        add!(self.entities.docs);
        add!(self.entities.attributes);
        add!(self.entity_authority.parentage);
        add!(self.entity_authority.source);
        add!(self.entity_authority.source_file);
        add!(self.entity_authority.members);
        add!(self.entity_authority.semantic_type);
        add!(self.entity_authority.documentation);
        add!(self.entity_authority.visibility);
        add!(self.entity_authority.attributes);
        add!(self.entity_authority.language_extension);
        add!(self.occurrence_authority.source);
        add!(self.sources.files);
        add!(self.sources.starts);
        add!(self.sources.ends);
        add!(self.language_extensions.typescript.ids.ordinals());
        add!(self.language_extensions.typescript.ids.values());
        add!(self.language_extensions.typescript.facts);
        add!(self.language_extensions.csharp.ids.ordinals());
        add!(self.language_extensions.csharp.ids.values());
        add!(self.language_extensions.csharp.facts);
        add!(self.language_extensions.go.ids.ordinals());
        add!(self.language_extensions.go.ids.values());
        add!(self.language_extensions.go.facts);
        add!(self.language_extensions.rust.ids.ordinals());
        add!(self.language_extensions.rust.ids.values());
        add!(self.language_extensions.rust.facts);
        add!(self.language_extensions.python.ids.ordinals());
        add!(self.language_extensions.python.ids.values());
        add!(self.language_extensions.python.facts);
        add!(self.language_extensions.java.ids.ordinals());
        add!(self.language_extensions.java.ids.values());
        add!(self.language_extensions.java.facts);
        add!(self.language_extensions.clang.ids.ordinals());
        add!(self.language_extensions.clang.ids.values());
        add!(self.language_extensions.clang.facts);
        add!(self.graph.from);
        add!(self.graph.targets);
        add!(self.graph.kinds);
        add!(self.graph.confidence);
        add!(self.graph.sources.ordinals());
        add!(self.graph.sources.values());
        add!(self.graph.outgoing);
        add!(self.graph.outgoing_offsets);
        add!(self.graph.incoming);
        add!(self.graph.incoming_offsets);
        add!(self.graph.occurrences.links);
        add!(self.graph.occurrences.confidence);
        add!(self.graph.occurrences.sources.ordinals());
        add!(self.graph.occurrences.sources.values());
        add!(self.graph.occurrences.outgoing);
        add!(self.graph.occurrences.outgoing_offsets);
        add!(self.vcs.versions);
        add!(self.vcs.declaration_instances);
        add!(self.vcs.stable_links);
        add!(self.kind_entities);
        add!(self.kind_offsets);
        add!(self.name_entities);
        total
    }
}
