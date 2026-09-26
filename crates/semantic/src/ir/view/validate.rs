//! Defines view validate behavior for `backend-semantic::ir`, whose purpose is to encode, validate, map, and borrow canonical compiler IR fragments.
//! This module owns the view validate invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
use crate::ir::{AtomId, EntityId, TypeId};

use crate::ir::{
    view::{
        DirectoryFault, FragmentError, FragmentView, WireField,
        directory::{DirectoryState, ParsedDirectoryEntry, directory_fault},
    },
    wire::{
        ATOM_RECORD_BYTES, ByteLength, DIRECTORY_ENTRY_LAYOUT, ENTITY_BYTES, FragmentLayout,
        HEADER_LAYOUT, RECIPE_FACT_BYTES, SOURCE_IDENTITY_BYTES, SectionCount, SectionKind,
        SectionRequirement, TYPE_NODE_BYTES, atom_fault, decode_entity, decode_recipe_fact,
        decode_source_identity, decode_type_node, entity_fault, entity_name_fault, read_u16,
        read_u32, type_node_fault,
    },
};

mod semantic_data;

use semantic_data::validate_semantic_data;

impl<'fragment> FragmentView<'fragment> {
    pub fn validate(envelope: &'fragment [u8]) -> Result<Self, FragmentError> {
        let layout = validate_fragment_layout(envelope)?;
        Ok(Self::from_validated_layout(envelope, layout))
    }

    /// Schema proven by the enclosing fragment envelope.
    #[must_use]
    pub const fn schema(&self) -> u16 {
        self.layout.schema
    }

    pub(crate) fn from_validated_layout(envelope: &'fragment [u8], layout: FragmentLayout) -> Self {
        let entity_lane = &envelope[layout.entities.range()];
        let atom_lane = &envelope[layout.atoms.range()];
        let semantic_data_lane = layout.semantic_data.map(|lane| &envelope[lane.range()]);
        let occurrence_lane = layout.occurrences.map(|lane| &envelope[lane.range()]);
        let type_fact_lane = layout.type_facts.map(|lane| &envelope[lane.range()]);
        let documentation_lane = layout.documentation.map(|lane| &envelope[lane.range()]);
        let language_extension_lane = layout
            .language_extensions
            .map(|lane| &envelope[lane.range()]);
        let extension_pool_lane = layout.extension_pools.map(|lane| &envelope[lane.range()]);
        Self {
            envelope,
            entities: entity_lane,
            type_nodes: &envelope[layout.type_nodes.range()],
            atoms: atom_lane,
            atom_bytes: &envelope[layout.atom_bytes.range()],
            semantic_data_lane,
            occurrence_lane,
            type_fact_lane,
            documentation_lane,
            language_extension_lane,
            extension_pool_lane,
            source: layout.source,
            recipe: layout.recipe,
            layout,
        }
    }

    /// Lazily decodes the validated occurrence fact plane, or `None` when
    /// the fragment commits no occurrence section.
    pub fn occurrences(&self) -> Option<crate::ir::semantic_facts::OccurrenceCursor<'fragment>> {
        self.occurrence_lane.map(|payload| {
            crate::ir::semantic_facts::OccurrenceCursor::new(payload, self.layout.schema)
        })
    }

    /// The raw validated occurrence section payload, or `None` when the
    /// fragment commits no occurrence section. Hostile-mutation tests use
    /// the slice to address individual wire cells.
    pub fn occurrence_payload(&self) -> Option<&'fragment [u8]> {
        self.occurrence_lane
    }

    pub fn type_facts(&self) -> Option<crate::ir::type_facts::TypeFactCursor<'fragment>> {
        self.type_fact_lane
            .map(|payload| crate::ir::type_facts::TypeFactCursor::new(payload, self.layout.schema))
    }

    /// Opens the validated canonical semantic-product graph. Schema-3-and-later
    /// values additionally bind each entity to an exact canonical root;
    /// legacy schema values remain explicitly root-unavailable.
    #[must_use]
    pub fn semantic_data(&self) -> Option<crate::ir::SemanticDataView<'fragment>> {
        self.semantic_data_lane
            .zip(self.layout.semantic_data_layout)
            .map(|(payload, layout)| crate::ir::SemanticDataView::from_validated(payload, layout))
    }

    /// Exact declared/computed geometry of the validated type-fact plane.
    ///
    /// Callers reopening extension lists must use this rather than inferring
    /// a type limit from entity cardinality.
    #[must_use]
    pub const fn type_fact_counts(&self) -> Option<crate::ir::TypeFactCounts> {
        self.layout.type_fact_counts
    }

    pub fn type_fact_payload(&self) -> Option<&'fragment [u8]> {
        self.type_fact_lane
    }

    /// Lazily decodes the validated documentation plane, or `None` when the
    /// fragment commits no documentation section.
    pub fn docs(&self) -> Option<crate::ir::docs_facts::DocFactCursor<'fragment>> {
        self.documentation_lane
            .map(crate::ir::docs_facts::DocFactCursor::new)
    }

    /// The validated language-extension section payload, or `None` when the
    /// fragment commits no extension section.
    pub fn language_extension_payload(&self) -> Option<&'fragment [u8]> {
        self.language_extension_lane
    }

    /// The validated extension pooled-lane payload, or `None` when the
    /// fragment commits no pooled-lane section.
    pub fn extension_pool_payload(&self) -> Option<&'fragment [u8]> {
        self.extension_pool_lane
    }

    /// Reopens the extension pools from the proof established during fragment
    /// validation. This only derives borrowed offsets; it does not re-walk
    /// payload grammar or shared references.
    pub(crate) fn validated_extension_pools(
        &self,
    ) -> Option<Result<crate::ir::ReopenedExtensionPools<'fragment>, crate::ir::ExtensionPoolFault>> {
        self.extension_pool_lane.map(|payload| {
            crate::ir::extension_pools::reopen_validated_extension_pools(self.layout.schema, payload)
        })
    }

    /// Reopens extension columns from the whole-fragment proof. This derives
    /// only their borrowed directory offsets; it never revalidates the paired
    /// language section or its shared pool references.
    pub(crate) fn validated_language_extensions(
        &self,
    ) -> Option<
        Result<
            crate::ir::ReopenedLanguageExtensionSection<'fragment>,
            crate::ir::LanguageExtensionReopenError,
        >,
    > {
        self.language_extension_lane.map(|payload| {
            crate::ir::semantic_extension_section::reopen_validated_language_extension_section(
                payload,
                crate::ir::SemanticImageAuthority::Language(self.recipe.profile),
                u32::from(self.layout.entities.count),
            )
        })
    }
}

pub(crate) fn validate_fragment_layout(envelope: &[u8]) -> Result<FragmentLayout, FragmentError> {
    let layout = validate_layout(envelope)?;
    {
        let entity_lane = &envelope[layout.entities.range()];
        for (ordinal, record) in
            (0..u32::from(layout.entities.count)).zip(entity_lane.chunks_exact(ENTITY_BYTES))
        {
            let entity = match decode_entity(record) {
                Ok(entity) => entity,
                Err(fault) => {
                    return Err(FragmentError::EntityRecord {
                        ordinal: EntityId::new(ordinal),
                        fault,
                    });
                }
            };
            if let Some(fault) = entity_fault(entity.semantic_type, layout.type_nodes.count) {
                return Err(FragmentError::Entity {
                    ordinal: EntityId::new(ordinal),
                    fault,
                });
            }
            if let Some(fault) = entity_name_fault(entity.name, layout.atoms.count) {
                return Err(FragmentError::EntityRecord {
                    ordinal: EntityId::new(ordinal),
                    fault: fault.into(),
                });
            }
        }
    }
    {
        let atom_lane = &envelope[layout.atoms.range()];
        for (ordinal, record) in
            (0..u32::from(layout.atoms.count)).zip(atom_lane.chunks_exact(ATOM_RECORD_BYTES))
        {
            let ordinal = AtomId::new(ordinal);
            if let Some(fault) = atom_fault(ordinal, record, layout.atom_bytes.count) {
                return Err(FragmentError::Atom { fault });
            }
            let start = read_u32(record, 0);
            usize::try_from(start).map_err(|source| FragmentError::WireWidth {
                field: WireField::AtomStart { ordinal },
                actual: start,
                source,
            })?;
            let length = read_u32(record, size_of::<u32>());
            usize::try_from(length).map_err(|source| FragmentError::WireWidth {
                field: WireField::AtomLength { ordinal },
                actual: length,
                source,
            })?;
        }
    }
    {
        let type_lane = &envelope[layout.type_nodes.range()];
        for (ordinal, record) in
            (0..u32::from(layout.type_nodes.count)).zip(type_lane.chunks_exact(TYPE_NODE_BYTES))
        {
            let node = match decode_type_node(record) {
                Ok(node) => node,
                Err(fault) => {
                    return Err(FragmentError::TypeNode {
                        ordinal: TypeId::new(ordinal),
                        fault,
                    });
                }
            };
            if let Some(fault) = type_node_fault(node, layout.type_nodes.count) {
                return Err(FragmentError::TypeNode {
                    ordinal: TypeId::new(ordinal),
                    fault,
                });
            }
        }
    }
    Ok(layout)
}

fn validate_layout(envelope: &[u8]) -> Result<FragmentLayout, FragmentError> {
    if envelope.len() < HEADER_LAYOUT.encoded_len {
        return Err(FragmentError::TruncatedHeader {
            required: HEADER_LAYOUT.encoded_len,
            actual: envelope.len(),
        });
    }
    let mut actual_magic = [0; size_of::<[u8; 4]>()];
    actual_magic.copy_from_slice(&envelope[HEADER_LAYOUT.magic..HEADER_LAYOUT.schema]);
    if actual_magic != crate::ir::FRAGMENT_MAGIC {
        return Err(FragmentError::Magic {
            actual: actual_magic,
        });
    }
    let schema = read_u16(envelope, HEADER_LAYOUT.schema);
    if !(1..=crate::ir::FRAGMENT_SCHEMA).contains(&schema) {
        return Err(FragmentError::Schema { actual: schema });
    }
    let section_count = SectionCount::from(read_u16(envelope, HEADER_LAYOUT.section_count));
    let declared_wire_length = ByteLength::from(read_u32(envelope, HEADER_LAYOUT.declared_length));
    let declared_length =
        usize::try_from(declared_wire_length).map_err(|source| FragmentError::WireWidth {
            field: WireField::DeclaredLength,
            actual: u32::from(declared_wire_length),
            source,
        })?;
    if declared_length != envelope.len() {
        return Err(FragmentError::DeclaredLength {
            declared: declared_length,
            actual: envelope.len(),
        });
    }
    let directory_bytes = usize::from(section_count)
        .checked_mul(DIRECTORY_ENTRY_LAYOUT.encoded_len)
        .ok_or(FragmentError::Extent {
            required: usize::MAX,
            actual: envelope.len(),
        })?;
    let directory_end = HEADER_LAYOUT
        .encoded_len
        .checked_add(directory_bytes)
        .ok_or(FragmentError::Extent {
            required: usize::MAX,
            actual: envelope.len(),
        })?;
    if directory_end > envelope.len() {
        return Err(FragmentError::Extent {
            required: directory_end,
            actual: envelope.len(),
        });
    }

    let mut state = DirectoryState::first(directory_end);
    let mut entities = None;
    let mut type_nodes = None;
    let mut source_identity = None;
    let mut atoms = None;
    let mut atom_bytes = None;
    let mut recipe_fact = None;
    let mut semantic_data = None;
    let mut semantic_data_layout = None;
    let mut occurrences = None;
    let mut type_facts = None;
    let mut type_fact_counts = None;
    let mut documentation = None;
    let mut language_extensions = None;
    let mut extension_pools = None;
    for _ in 0..u16::from(section_count) {
        let (next_state, entry) = state.parse(envelope)?;
        match SectionKind::try_from(entry.kind) {
            Ok(SectionKind::EntityTypes) => {
                validate_known_length(&entry, ENTITY_BYTES)?;
                require_known(&entry)?;
                entities = Some(entry.lane);
            }
            Ok(SectionKind::TypeNodes) => {
                validate_known_length(&entry, TYPE_NODE_BYTES)?;
                require_known(&entry)?;
                type_nodes = Some(entry.lane);
            }
            Ok(SectionKind::SourceIdentity) => {
                validate_known_length(&entry, SOURCE_IDENTITY_BYTES)?;
                require_known(&entry)?;
                if u32::from(entry.lane.count) != 1 {
                    return Err(directory_fault(
                        entry.ordinal,
                        DirectoryFault::Count {
                            kind: entry.kind,
                            expected: 1,
                            actual: u32::from(entry.lane.count),
                        },
                    ));
                }
                source_identity = Some(entry.lane);
            }
            Ok(SectionKind::AtomRecords) => {
                validate_known_length(&entry, ATOM_RECORD_BYTES)?;
                require_known(&entry)?;
                atoms = Some(entry.lane);
            }
            Ok(SectionKind::AtomBytes) => {
                validate_known_length(&entry, 1)?;
                require_known(&entry)?;
                atom_bytes = Some(entry.lane);
            }
            Ok(SectionKind::RecipeFact) => {
                validate_known_length(&entry, RECIPE_FACT_BYTES)?;
                require_known(&entry)?;
                if u32::from(entry.lane.count) != 1 {
                    return Err(directory_fault(
                        entry.ordinal,
                        DirectoryFault::Count {
                            kind: entry.kind,
                            expected: 1,
                            actual: u32::from(entry.lane.count),
                        },
                    ));
                }
                recipe_fact = Some(entry.lane);
            }
            Ok(SectionKind::SemanticData) => {
                validate_known_length(&entry, 1)?;
                require_known(&entry)?;
                let entity_count = u32::from(
                    entities
                        .ok_or(FragmentError::MissingSection {
                            section: SectionKind::EntityTypes,
                        })?
                        .count,
                );
                semantic_data_layout = Some(validate_semantic_data(
                    &envelope[entry.lane.range()],
                    schema,
                    entity_count,
                )?);
                semantic_data = Some(entry.lane);
            }
            Ok(SectionKind::Occurrences) => {
                validate_known_length(&entry, 1)?;
                require_known(&entry)?;
                let entity_count = u32::from(
                    entities
                        .ok_or(FragmentError::MissingSection {
                            section: SectionKind::EntityTypes,
                        })?
                        .count,
                );
                let payload = &envelope[entry.lane.range()];
                crate::ir::semantic_facts::validate_occurrence_payload(payload, entity_count, schema)
                    .map_err(|fault| FragmentError::Occurrences {
                    fault: crate::ir::semantic_facts::occurrence_view_fault(fault),
                })?;
                occurrences = Some(entry.lane);
            }
            Ok(SectionKind::TypeFacts) => {
                validate_known_length(&entry, 1)?;
                require_known(&entry)?;
                let entity_count = u32::from(
                    entities
                        .ok_or(FragmentError::MissingSection {
                            section: SectionKind::EntityTypes,
                        })?
                        .count,
                );
                let counts = crate::ir::type_facts::validate_payload(
                    &envelope[entry.lane.range()],
                    entity_count,
                    schema,
                )
                .map_err(|fault| FragmentError::TypeFacts { fault })?;
                type_facts = Some(entry.lane);
                type_fact_counts = Some(counts);
            }
            Ok(SectionKind::Documentation) => {
                validate_known_length(&entry, 1)?;
                require_known(&entry)?;
                let entity_count = u32::from(
                    entities
                        .ok_or(FragmentError::MissingSection {
                            section: SectionKind::EntityTypes,
                        })?
                        .count,
                );
                crate::ir::docs_facts::validate_doc_payload(
                    &envelope[entry.lane.range()],
                    entity_count,
                )
                .map_err(|fault| FragmentError::Documentation { fault })?;
                documentation = Some(entry.lane);
            }
            Ok(SectionKind::LanguageExtensions) => {
                validate_known_length(&entry, 1)?;
                require_known(&entry)?;
                language_extensions = Some(entry.lane);
            }
            Ok(SectionKind::ExtensionPools) => {
                validate_known_length(&entry, 1)?;
                require_known(&entry)?;
                extension_pools = Some(entry.lane);
            }
            Err(kind) if entry.requirement == SectionRequirement::Required => {
                return Err(directory_fault(
                    entry.ordinal,
                    DirectoryFault::RequiredUnknown { kind },
                ));
            }
            Err(_) => {}
        }
        state = next_state;
    }
    if state.expected_offset != envelope.len() {
        return Err(FragmentError::Extent {
            required: state.expected_offset,
            actual: envelope.len(),
        });
    }
    let entities = entities.ok_or(FragmentError::MissingSection {
        section: SectionKind::EntityTypes,
    })?;
    let type_nodes = type_nodes.ok_or(FragmentError::MissingSection {
        section: SectionKind::TypeNodes,
    })?;
    let source_identity = source_identity.ok_or(FragmentError::MissingSection {
        section: SectionKind::SourceIdentity,
    })?;
    let atoms = atoms.ok_or(FragmentError::MissingSection {
        section: SectionKind::AtomRecords,
    })?;
    let atom_bytes = atom_bytes.ok_or(FragmentError::MissingSection {
        section: SectionKind::AtomBytes,
    })?;
    let recipe_fact = recipe_fact.ok_or(FragmentError::MissingSection {
        section: SectionKind::RecipeFact,
    })?;
    if language_extensions.is_some() != extension_pools.is_some() {
        return Err(FragmentError::ExtensionPoolPair {
            extensions: language_extensions.is_some(),
            pools: extension_pools.is_some(),
        });
    }
    let source = decode_source_identity(&envelope[source_identity.range()])
        .map_err(|fault| FragmentError::SourceIdentity { fault })?;
    let recipe = decode_recipe_fact(&envelope[recipe_fact.range()])
        .map_err(|fault| FragmentError::RecipeFact { fault })?;
    let expected_recipe = crate::vocabulary::CompileRecipeFact::derive(
        recipe.profile,
        recipe.stage,
        recipe.tool,
        source.identity,
        recipe.toolchain,
    );
    if recipe.identity != expected_recipe.identity {
        return Err(FragmentError::RecipeFact {
            fault: crate::ir::RecipeFactFault::IdentityRelation {
                expected: expected_recipe.identity,
                observed: recipe.identity,
            },
        });
    }
    if let (Some(extension_lane), Some(pool_lane)) = (language_extensions, extension_pools) {
        let type_count = type_fact_counts
            .map(|counts| counts.total())
            .transpose()
            .map_err(|fault| FragmentError::TypeFacts { fault })?
            .unwrap_or(0);
        let pools = crate::ir::reopen_extension_pools(
            schema,
            &envelope[pool_lane.range()],
            u32::from(atoms.count),
            type_count,
            u32::from(entities.count),
        )
        .map_err(|fault| FragmentError::ExtensionPools { fault })?;
        let bounds = crate::ir::ValidatedLanguageExtensionCommonBounds::from_validated(
            crate::ir::LanguageExtensionCommonBounds {
                atoms: u32::from(atoms.count),
                types: type_count,
                entities: u32::from(entities.count),
                type_lists: pools.type_list_count(),
                entity_lists: pools.entity_list_count(),
                atom_lists: pools.atom_list_count(),
                free_predicates: pools.free_predicate_list_count(),
                type_parameters: pools.type_parameter_list_bounds(),
            },
        );
        crate::ir::reopen_language_extension_section(
            &envelope[extension_lane.range()],
            crate::ir::SemanticImageAuthority::Language(recipe.profile),
            bounds,
        )
        .map_err(|fault| FragmentError::LanguageExtensions { fault })?;
    }
    Ok(FragmentLayout {
        schema,
        entities,
        type_nodes,
        atoms,
        atom_bytes,
        source_identity,
        recipe_fact,
        semantic_data,
        semantic_data_layout,
        occurrences,
        type_facts,
        type_fact_counts,
        documentation,
        language_extensions,
        extension_pools,
        source,
        recipe,
        output_len: envelope.len(),
        output_wire_len: declared_wire_length,
    })
}

fn require_known(entry: &ParsedDirectoryEntry) -> Result<(), FragmentError> {
    if entry.requirement != SectionRequirement::Required {
        return Err(directory_fault(
            entry.ordinal,
            DirectoryFault::Flags {
                kind: entry.kind,
                actual: u16::from(entry.requirement),
            },
        ));
    }
    Ok(())
}

fn validate_known_length(entry: &ParsedDirectoryEntry, width: usize) -> Result<(), FragmentError> {
    let count_index =
        usize::try_from(entry.lane.count).map_err(|source| FragmentError::WireWidth {
            field: WireField::SectionItemCount {
                ordinal: entry.ordinal,
            },
            actual: u32::from(entry.lane.count),
            source,
        })?;
    let expected = count_index.checked_mul(width).ok_or_else(|| {
        directory_fault(
            entry.ordinal,
            DirectoryFault::CountWidthOverflow {
                kind: entry.kind,
                count: u32::from(entry.lane.count),
                width,
            },
        )
    })?;
    let actual_index =
        usize::try_from(entry.lane.length).map_err(|source| FragmentError::WireWidth {
            field: WireField::SectionByteLength {
                ordinal: entry.ordinal,
            },
            actual: u32::from(entry.lane.length),
            source,
        })?;
    if expected != actual_index {
        return Err(directory_fault(
            entry.ordinal,
            DirectoryFault::ByteLength {
                kind: entry.kind,
                expected,
                actual: u32::from(entry.lane.length),
            },
        ));
    }
    Ok(())
}
