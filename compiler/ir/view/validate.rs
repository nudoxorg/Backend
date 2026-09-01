//! Defines view validate behavior for `compiler-ir`, whose purpose is to encode, validate, map, and borrow canonical compiler IR fragments.
//! This module owns the view validate invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
use crate::{AtomId, EntityId, ProductChildRole, SemanticProductConstructor, TypeId};
use heart_identity::{ContentId, HASH_BYTES, IrFragmentDomain};

use crate::{
    view::{
        DirectoryFault, FragmentError, FragmentView, SemanticDataFault, WireField,
        directory::{DirectoryState, ParsedDirectoryEntry, directory_fault},
    },
    wire::{
        ATOM_RECORD_BYTES, ByteLength, DIRECTORY_ENTRY_LAYOUT, ENTITY_BYTES, FragmentLayout,
        HEADER_LAYOUT, RECIPE_FACT_BYTES, SEMANTIC_CHILD_BYTES, SEMANTIC_CONSTRUCTOR_BYTES,
        SEMANTIC_DATA_HEADER_BYTES, SEMANTIC_EXTERNAL_TAG, SEMANTIC_LIST_BYTES, SEMANTIC_LOCAL_TAG,
        SEMANTIC_PRODUCT_BYTES, SOURCE_IDENTITY_BYTES, SectionCount, SectionKind,
        SectionRequirement, TYPE_NODE_BYTES, atom_fault, decode_entity, decode_recipe_fact,
        decode_source_identity, decode_type_node, entity_fault, entity_name_fault, read_u16,
        read_u32, type_node_fault,
    },
};

impl<'fragment> FragmentView<'fragment> {
    pub fn validate(envelope: &'fragment [u8]) -> Result<Self, FragmentError> {
        let layout = validate_fragment_layout(envelope)?;
        Ok(Self::from_validated_layout(envelope, layout))
    }

    pub(crate) fn from_validated_layout(envelope: &'fragment [u8], layout: FragmentLayout) -> Self {
        let entity_lane = &envelope[layout.entities.range()];
        let atom_lane = &envelope[layout.atoms.range()];
        let occurrence_lane = layout.occurrences.map(|lane| &envelope[lane.range()]);
        let type_fact_lane = layout.type_facts.map(|lane| &envelope[lane.range()]);
        Self {
            envelope,
            entities: entity_lane,
            type_nodes: &envelope[layout.type_nodes.range()],
            atoms: atom_lane,
            atom_bytes: &envelope[layout.atom_bytes.range()],
            occurrence_lane,
            type_fact_lane,
            source: layout.source,
            recipe: layout.recipe,
            layout,
        }
    }

    /// Lazily decodes the validated occurrence fact plane, or `None` when
    /// the fragment commits no occurrence section.
    pub fn occurrences(&self) -> Option<crate::semantic_facts::OccurrenceCursor<'fragment>> {
        self.occurrence_lane
            .map(crate::semantic_facts::OccurrenceCursor::new)
    }

    /// The raw validated occurrence section payload, or `None` when the
    /// fragment commits no occurrence section. Hostile-mutation tests use
    /// the slice to address individual wire cells.
    pub fn occurrence_payload(&self) -> Option<&'fragment [u8]> {
        self.occurrence_lane
    }

    pub fn type_facts(&self) -> Option<crate::type_facts::TypeFactCursor<'fragment>> {
        self.type_fact_lane
            .map(crate::type_facts::TypeFactCursor::new)
    }

    pub fn type_fact_payload(&self) -> Option<&'fragment [u8]> {
        self.type_fact_lane
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
    if actual_magic != crate::FRAGMENT_MAGIC {
        return Err(FragmentError::Magic {
            actual: actual_magic,
        });
    }
    let schema = read_u16(envelope, HEADER_LAYOUT.schema);
    if schema != crate::FRAGMENT_SCHEMA {
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
    let mut occurrences = None;
    let mut type_facts = None;
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
                validate_semantic_data(&envelope[entry.lane.range()])?;
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
                crate::semantic_facts::validate_occurrence_payload(payload, entity_count).map_err(
                    |fault| FragmentError::Occurrences {
                        fault: crate::semantic_facts::occurrence_view_fault(fault),
                    },
                )?;
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
                crate::type_facts::validate_payload(&envelope[entry.lane.range()], entity_count)
                    .map_err(|fault| FragmentError::TypeFacts { fault })?;
                type_facts = Some(entry.lane);
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
    let source = decode_source_identity(&envelope[source_identity.range()])
        .map_err(|fault| FragmentError::SourceIdentity { fault })?;
    let recipe = decode_recipe_fact(&envelope[recipe_fact.range()])
        .map_err(|fault| FragmentError::RecipeFact { fault })?;
    let expected_recipe = compiler_vocabulary::CompileRecipeFact::derive(
        recipe.profile,
        recipe.stage,
        recipe.tool,
        source.identity,
        recipe.toolchain,
    );
    if recipe.identity != expected_recipe.identity {
        return Err(FragmentError::RecipeFact {
            fault: crate::RecipeFactFault::IdentityRelation {
                expected: expected_recipe.identity,
                observed: recipe.identity,
            },
        });
    }
    Ok(FragmentLayout {
        entities,
        type_nodes,
        atoms,
        atom_bytes,
        source_identity,
        recipe_fact,
        semantic_data,
        occurrences,
        type_facts,
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

/// Validates one embedded canonical semantic-data section completely before
/// any borrowed view exists.
///
/// The walk covers the exact wire grammar written by `write_semantic_data`:
/// five dense lane counts, one length-prefixed canonical atom stream, the
/// product and constructor lanes, the pooled-list table, and the fixed-width
/// role-bearing child lane. Every rejection retains the rejected operands.
fn validate_semantic_data(section: &[u8]) -> Result<(), FragmentError> {
    if section.len() < SEMANTIC_DATA_HEADER_BYTES {
        return Err(FragmentError::SemanticData {
            fault: SemanticDataFault::Header {
                required: SEMANTIC_DATA_HEADER_BYTES,
                actual: section.len(),
            },
        });
    }

    let atom_count = read_u32(section, 0);
    let product_count = read_u32(section, size_of::<u32>());
    let constructor_count = read_u32(section, size_of::<u32>() * 2);
    let list_count = read_u32(section, size_of::<u32>() * 3);
    let child_count = read_u32(section, size_of::<u32>() * 4);
    if constructor_count != product_count {
        return Err(FragmentError::SemanticData {
            fault: SemanticDataFault::ConstructorCount {
                product_count,
                constructor_count,
            },
        });
    }

    let mut cursor = SEMANTIC_DATA_HEADER_BYTES;
    for ordinal in 0..atom_count {
        let Some(length_bytes) = take(section, &mut cursor, size_of::<u32>()) else {
            return Err(semantic_trailing(section, cursor));
        };
        let length = read_u32(length_bytes, 0);
        let length_index = semantic_index(length, section, cursor, |length, available| {
            SemanticDataFault::AtomLength {
                ordinal,
                length,
                available,
            }
        })?;
        cursor += length_index;
    }

    let products_offset = cursor;
    let constructors_offset = advance_offset(
        products_offset,
        product_count,
        SEMANTIC_PRODUCT_BYTES,
        section,
    )?;
    let lists_offset = advance_offset(
        constructors_offset,
        constructor_count,
        SEMANTIC_CONSTRUCTOR_BYTES,
        section,
    )?;
    let children_offset = advance_offset(lists_offset, list_count, SEMANTIC_LIST_BYTES, section)?;
    let declared_end = advance_offset(children_offset, child_count, SEMANTIC_CHILD_BYTES, section)?;
    if declared_end != section.len() {
        return Err(FragmentError::SemanticData {
            fault: SemanticDataFault::Trailing {
                actual: section.len().saturating_sub(declared_end),
            },
        });
    }

    let product_record =
        |product: u32| record_at(section, products_offset, product, SEMANTIC_PRODUCT_BYTES);
    for list in 0..list_count {
        let Some(record) = record_at(section, lists_offset, list, SEMANTIC_LIST_BYTES) else {
            return Err(semantic_trailing(section, lists_offset));
        };
        let start = read_u32(record, 0);
        let length = read_u32(record, size_of::<u32>());
        let Some(end) = start.checked_add(length) else {
            return Err(FragmentError::SemanticData {
                fault: SemanticDataFault::ListExtent {
                    list,
                    start,
                    length,
                    child_count,
                },
            });
        };
        if end > child_count {
            return Err(FragmentError::SemanticData {
                fault: SemanticDataFault::ListExtent {
                    list,
                    start,
                    length,
                    child_count,
                },
            });
        }
    }
    for product in 0..product_count {
        let Some(record) = product_record(product) else {
            return Err(semantic_trailing(section, products_offset));
        };
        let head = read_u32(record, 0);
        if head >= atom_count {
            return Err(FragmentError::SemanticData {
                fault: SemanticDataFault::ProductHead {
                    product,
                    target: head,
                    atom_count,
                },
            });
        }
        let list = read_u32(record, size_of::<u32>());
        if list >= list_count {
            return Err(FragmentError::SemanticData {
                fault: SemanticDataFault::ProductList {
                    product,
                    target: list,
                    list_count,
                },
            });
        }
        let Some(constructor_record) = record_at(
            section,
            constructors_offset,
            product,
            SEMANTIC_CONSTRUCTOR_BYTES,
        ) else {
            return Err(semantic_trailing(section, constructors_offset));
        };
        let tag = read_u32(constructor_record, 0);
        let payload0 = read_u32(constructor_record, size_of::<u32>());
        let payload1 = read_u32(constructor_record, size_of::<u32>() * 2);
        let Some(list_record) = record_at(section, lists_offset, list, SEMANTIC_LIST_BYTES) else {
            return Err(semantic_trailing(section, lists_offset));
        };
        let span_start = read_u32(list_record, 0);
        let span_length = read_u32(list_record, size_of::<u32>());
        let constructor =
            SemanticProductConstructor::try_from_parts(tag, payload0, payload1, span_length)
                .map_err(|fault| FragmentError::SemanticData {
                    fault: SemanticDataFault::Constructor { product, fault },
                })?;
        for position in 0..span_length {
            let child = span_start.saturating_add(position);
            let Some(child_record) =
                record_at(section, children_offset, child, SEMANTIC_CHILD_BYTES)
            else {
                return Err(semantic_trailing(section, children_offset));
            };
            validate_child_role(child_record, child, &constructor, position)?;
        }
    }

    for child in 0..child_count {
        let Some(record) = record_at(section, children_offset, child, SEMANTIC_CHILD_BYTES) else {
            return Err(semantic_trailing(section, children_offset));
        };
        validate_child_target(record, child, product_count)?;
    }
    Ok(())
}

/// Proves one product-reachable child carries the closed role required by its
/// constructor position. Tag, target, and authority classes are proven for
/// every declared child by [`validate_child_target`].
fn validate_child_role(
    record: &[u8],
    child: u32,
    constructor: &SemanticProductConstructor,
    position: u32,
) -> Result<(), FragmentError> {
    let expected = constructor.expected_role(position);
    let actual =
        ProductChildRole::try_from(record[0]).map_err(|error| FragmentError::SemanticData {
            fault: SemanticDataFault::ChildRoleCode {
                child,
                actual: error.actual,
            },
        })?;
    if actual != expected {
        return Err(FragmentError::SemanticData {
            fault: SemanticDataFault::ChildRole {
                child,
                expected,
                actual,
            },
        });
    }
    Ok(())
}

fn validate_child_target(
    record: &[u8],
    child: u32,
    product_count: u32,
) -> Result<(), FragmentError> {
    let tag = record[size_of::<u8>()];
    let target = read_u32(record, size_of::<u8>() * 2);
    let mut authority = [0; HASH_BYTES];
    authority.copy_from_slice(&record[size_of::<u8>() * 2 + size_of::<u32>()..]);
    match tag {
        SEMANTIC_LOCAL_TAG => {
            if target >= product_count {
                return Err(FragmentError::SemanticData {
                    fault: SemanticDataFault::LocalChild {
                        child,
                        target,
                        product_count,
                    },
                });
            }
            if authority != [0; HASH_BYTES] {
                return Err(FragmentError::SemanticData {
                    fault: SemanticDataFault::LocalReserved {
                        child,
                        actual: authority,
                    },
                });
            }
        }
        SEMANTIC_EXTERNAL_TAG => {
            if let Err(error) = ContentId::<IrFragmentDomain>::try_from(authority) {
                let (expected, observed) = match error {
                    heart_identity::ContentIdDecodeError::Domain {
                        expected, observed, ..
                    } => (u8::from(expected), observed),
                    heart_identity::ContentIdDecodeError::Width { .. } => (0, 0),
                };
                return Err(FragmentError::SemanticData {
                    fault: SemanticDataFault::ExternalAuthority {
                        child,
                        expected,
                        observed,
                        raw: authority,
                    },
                });
            }
        }
        actual => {
            return Err(FragmentError::SemanticData {
                fault: SemanticDataFault::ChildTag { child, actual },
            });
        }
    }
    Ok(())
}

fn record_at(section: &[u8], base: usize, ordinal: u32, width: usize) -> Option<&[u8]> {
    let index = usize::try_from(ordinal).ok()?;
    let start = base.checked_add(index.checked_mul(width)?)?;
    section.get(start..start.checked_add(width)?)
}

fn take<'section>(
    section: &'section [u8],
    cursor: &mut usize,
    width: usize,
) -> Option<&'section [u8]> {
    let end = cursor.checked_add(width)?;
    let record = section.get(*cursor..end)?;
    *cursor = end;
    Some(record)
}

fn advance_offset(
    base: usize,
    count: u32,
    width: usize,
    section: &[u8],
) -> Result<usize, FragmentError> {
    let index = usize::try_from(count).map_err(|_| FragmentError::SemanticData {
        fault: SemanticDataFault::Trailing {
            actual: section.len(),
        },
    })?;
    let advanced = index
        .checked_mul(width)
        .and_then(|length| base.checked_add(length))
        .ok_or(FragmentError::SemanticData {
            fault: SemanticDataFault::Trailing {
                actual: section.len(),
            },
        })?;
    if advanced > section.len() {
        return Err(FragmentError::SemanticData {
            fault: SemanticDataFault::Trailing {
                actual: section.len().saturating_sub(base),
            },
        });
    }
    Ok(advanced)
}

fn semantic_index(
    length: u32,
    section: &[u8],
    cursor: usize,
    fault: impl Fn(u32, usize) -> SemanticDataFault,
) -> Result<usize, FragmentError> {
    let available = section.len().saturating_sub(cursor);
    let index = usize::try_from(length).map_err(|_| FragmentError::SemanticData {
        fault: fault(length, available),
    })?;
    if index > available {
        return Err(FragmentError::SemanticData {
            fault: fault(length, available),
        });
    }
    Ok(index)
}

fn semantic_trailing(section: &[u8], cursor: usize) -> FragmentError {
    FragmentError::SemanticData {
        fault: SemanticDataFault::Trailing {
            actual: section.len().saturating_sub(cursor),
        },
    }
}
