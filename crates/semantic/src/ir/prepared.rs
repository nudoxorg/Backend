//! Defines prepared behavior for `backend-semantic::ir`, whose purpose is to encode, validate, map, and borrow canonical compiler IR fragments.
//! This module owns the prepared invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
use core::num::TryFromIntError;

use crate::ir::{AtomId, EntityId, ProductRef, TypeId};
use thiserror::Error;

use crate::ir::{
    AtomInput, CanonicalDataError, EntityRecord, EntityRecordFault, RecipeFact, SourceIdentity,
    TypeNode, TypeNodeFault,
    canonical_data::CanonicalDataGraph,
    docs_facts::DocumentationLane,
    extension_pools::ExtensionPoolsLane,
    semantic_extension_section::{
        ExtensionSectionInput, encode_fragment_extension_section, fragment_extension_section_len,
    },
    wire::{
        ATOM_RECORD_BYTES, ByteLength, ByteOffset, DIRECTORY_ENTRY_LAYOUT, ENTITY_BYTES,
        FragmentLayout, HEADER_LAYOUT, ItemCount, LaneLayout, RECIPE_FACT_BYTES,
        SEMANTIC_CHILD_BYTES, SEMANTIC_CONSTRUCTOR_BYTES, SEMANTIC_DATA_HEADER_BYTES,
        SEMANTIC_EXTERNAL_TAG, SEMANTIC_LIST_BYTES, SEMANTIC_LOCAL_TAG, SEMANTIC_PRODUCT_BYTES,
        SOURCE_IDENTITY_BYTES, SectionCount, SectionKind, SectionRequirement, TYPE_NODE_BYTES,
        entity_fault, entity_name_fault, type_node_fault, write_atom_record, write_entity,
        write_recipe_fact, write_source_identity, write_type_node, write_u16, write_u32,
    },
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LayoutStep {
    Directory,
    EntityLane,
    TypeNodeLane,
    AtomRecordLane,
    AtomByteLane,
    SourceIdentityLane,
    RecipeFactLane,
    SemanticData,
    Occurrences,
    TypeFacts,
    Documentation,
    LanguageExtensions,
    ExtensionPools,
}

#[derive(Debug, Eq, Error, PartialEq)]
pub enum PrepareError {
    #[error("{lane:?} count {actual} exceeds the fragment count width")]
    Count {
        lane: LayoutStep,
        actual: usize,
        #[source]
        source: TryFromIntError,
    },
    #[error("atom byte pool overflowed while adding atom {ordinal:?}")]
    AtomBytePoolOverflow { ordinal: AtomId },
    #[error(
        "fragment layout overflow at {step:?} for {entity_count} entities and {type_node_count} type nodes"
    )]
    LayoutOverflow {
        step: LayoutStep,
        entity_count: u32,
        type_node_count: u32,
    },
    #[error("{step:?} item count {actual} exceeds the native address width")]
    NativeCount {
        step: LayoutStep,
        actual: u32,
        #[source]
        source: TryFromIntError,
    },
    #[error("fragment output length {actual} exceeds the wire byte-coordinate width")]
    OutputLength {
        actual: usize,
        #[source]
        source: TryFromIntError,
    },
    #[error("entity {ordinal:?} is invalid: {fault}")]
    Entity {
        ordinal: EntityId,
        #[source]
        fault: EntityRecordFault,
    },
    #[error("type node {ordinal:?} is invalid: {fault}")]
    TypeNode {
        ordinal: TypeId,
        #[source]
        fault: TypeNodeFault,
    },
    #[error(
        "semantic graph with {atoms} atoms, {products} products, and {children} children overflows the semantic payload layout"
    )]
    SemanticDataOverflow {
        atoms: u32,
        products: u32,
        children: u32,
    },
    #[error("semantic graph carries {roots} declaration roots for {entities} fragment entities")]
    SemanticEntityRoots { roots: u32, entities: u32 },
    #[error("semantic atom {ordinal:?} has {actual} bytes, exceeding the wire length width")]
    SemanticAtomLength {
        ordinal: AtomId,
        actual: usize,
        #[source]
        source: TryFromIntError,
    },
    /// Canonical semantic-data preparation failed before the fragment layout
    /// could be derived. The exact admission, validation, budget, or capacity
    /// rejection is retained as the source.
    #[error("semantic data canonicalization failed")]
    SemanticData {
        #[source]
        cause: CanonicalDataError,
    },
    #[error("occurrence lane admission rejected a reference fact: {fault}")]
    OccurrenceLane {
        ordinal: u32,
        #[source]
        fault: crate::ir::view::OccurrenceFault,
    },
    #[error("type-fact lane admission rejected: {fault}")]
    TypeFacts {
        #[source]
        fault: crate::ir::TypeFactFault,
    },
    #[error("documentation lane admission rejected: {fault}")]
    Documentation {
        #[source]
        fault: crate::ir::DocFactFault,
    },
    #[error("extension-pool admission rejected: {fault}")]
    ExtensionPools {
        #[source]
        fault: crate::ir::ExtensionPoolFault,
    },
    #[error("the language-extension section and its pooled lanes must be committed together")]
    ExtensionPoolsMismatch,
}

#[derive(Debug, Error)]
pub enum WriteError {
    #[error("fragment output needs {required} bytes but only {available} are available")]
    OutputTooSmall { required: usize, available: usize },
    #[error("language-extension section encoding failed: {fault}")]
    ExtensionSection {
        #[source]
        fault: crate::ir::semantic_extension_section::LanguageExtensionEncodeError,
    },
    #[error("prepared atom {ordinal:?} no longer fits the compact byte width")]
    AtomLength {
        ordinal: AtomId,
        actual: usize,
        #[source]
        source: TryFromIntError,
    },
    #[error("prepared atom {ordinal:?} no longer fits its prepared byte region")]
    AtomExtent { ordinal: AtomId },
    #[error("semantic atom {ordinal:?} no longer fits the semantic wire length width")]
    SemanticAtomLength {
        ordinal: AtomId,
        actual: usize,
        #[source]
        source: TryFromIntError,
    },
}

pub struct PreparedFragment<'facts> {
    source: SourceIdentity,
    recipe: RecipeFact,
    entities: &'facts [EntityRecord],
    type_nodes: &'facts [TypeNode],
    atoms: &'facts [AtomInput<'facts>],
    semantic_data: Option<&'facts CanonicalDataGraph<'facts, 'facts>>,
    occurrences: Option<&'facts crate::ir::semantic_facts::OccurrenceLane<'facts>>,
    type_facts: Option<&'facts crate::ir::type_facts::TypeFactLane<'facts>>,
    docs: Option<&'facts DocumentationLane<'facts>>,
    extensions: Option<&'facts ExtensionSectionInput<'facts>>,
    pools: Option<&'facts ExtensionPoolsLane<'facts>>,
    layout: FragmentLayout,
}

/// Optional semantic planes committed beside the fragment's required lanes.
#[derive(Clone, Copy, Default)]
pub struct FragmentSemantics<'facts> {
    pub data: Option<&'facts CanonicalDataGraph<'facts, 'facts>>,
    pub occurrences: Option<&'facts crate::ir::semantic_facts::OccurrenceLane<'facts>>,
    pub type_facts: Option<&'facts crate::ir::type_facts::TypeFactLane<'facts>>,
    pub docs: Option<&'facts DocumentationLane<'facts>>,
    pub extensions: Option<&'facts ExtensionSectionInput<'facts>>,
    pub pools: Option<&'facts ExtensionPoolsLane<'facts>>,
}

#[derive(Clone, Copy)]
struct FragmentInput<'facts> {
    source: SourceIdentity,
    recipe: RecipeFact,
    entities: &'facts [EntityRecord],
    type_nodes: &'facts [TypeNode],
    atoms: &'facts [AtomInput<'facts>],
}

impl<'facts> PreparedFragment<'facts> {
    pub fn prepare(
        source: SourceIdentity,
        recipe: RecipeFact,
        entities: &'facts [EntityRecord],
        type_nodes: &'facts [TypeNode],
        atoms: &'facts [AtomInput<'facts>],
    ) -> Result<Self, PrepareError> {
        Self::prepare_inner(
            FragmentInput {
                source,
                recipe,
                entities,
                type_nodes,
                atoms,
            },
            FragmentSemantics::default(),
        )
    }

    /// Prepare one fragment whose envelope embeds one canonical semantic-data
    /// graph beside the entity, type, atom, source, and recipe lanes.
    ///
    /// The graph borrows caller-owned canonicalization output; no separate
    /// data artifact or copied semantic owner is retained by the prepared
    /// value. The fragment therefore commits the complete semantic structure
    /// under the same wire identity.
    pub fn prepare_with_data(
        source: SourceIdentity,
        recipe: RecipeFact,
        entities: &'facts [EntityRecord],
        type_nodes: &'facts [TypeNode],
        atoms: &'facts [AtomInput<'facts>],
        semantic_data: &'facts CanonicalDataGraph<'facts, 'facts>,
    ) -> Result<Self, PrepareError> {
        Self::prepare_with_semantics(
            source,
            recipe,
            entities,
            type_nodes,
            atoms,
            FragmentSemantics {
                data: Some(semantic_data),
                ..FragmentSemantics::default()
            },
        )
    }

    /// Prepare one fragment that additionally embeds the occurrence fact
    /// plane: admitted reference facts owned by the entity lane, written in
    /// lane order, and revalidated identically on reopen.
    pub fn prepare_with_occurrences(
        source: SourceIdentity,
        recipe: RecipeFact,
        entities: &'facts [EntityRecord],
        type_nodes: &'facts [TypeNode],
        atoms: &'facts [AtomInput<'facts>],
        semantic_data: Option<&'facts CanonicalDataGraph<'facts, 'facts>>,
        occurrences: &'facts crate::ir::semantic_facts::OccurrenceLane<'facts>,
    ) -> Result<Self, PrepareError> {
        Self::prepare_with_semantics(
            source,
            recipe,
            entities,
            type_nodes,
            atoms,
            FragmentSemantics {
                data: semantic_data,
                occurrences: Some(occurrences),
                type_facts: None,
                docs: None,
                extensions: None,
                pools: None,
            },
        )
    }

    /// Prepares one fragment from its required lanes and a closed set of
    /// optional semantic planes.
    pub fn prepare_with_semantics(
        source: SourceIdentity,
        recipe: RecipeFact,
        entities: &'facts [EntityRecord],
        type_nodes: &'facts [TypeNode],
        atoms: &'facts [AtomInput<'facts>],
        semantics: FragmentSemantics<'facts>,
    ) -> Result<Self, PrepareError> {
        Self::prepare_inner(
            FragmentInput {
                source,
                recipe,
                entities,
                type_nodes,
                atoms,
            },
            semantics,
        )
    }

    fn prepare_inner(
        input: FragmentInput<'facts>,
        semantics: FragmentSemantics<'facts>,
    ) -> Result<Self, PrepareError> {
        let FragmentInput {
            source,
            recipe,
            entities,
            type_nodes,
            atoms,
        } = input;
        let FragmentSemantics {
            data: semantic_data,
            occurrences,
            type_facts,
            docs,
            extensions,
            pools,
        } = semantics;
        if extensions.is_some() != pools.is_some() {
            return Err(PrepareError::ExtensionPoolsMismatch);
        }
        let entity_count = count(LayoutStep::EntityLane, entities.len())?;
        let type_node_count = count(LayoutStep::TypeNodeLane, type_nodes.len())?;
        let atom_count = count(LayoutStep::AtomRecordLane, atoms.len())?;
        let atom_byte_count = atom_byte_count(atoms)?;
        if let Some(data) = semantic_data {
            let roots = u32::try_from(data.source_product_roots().len()).map_err(|source| {
                PrepareError::Count {
                    lane: LayoutStep::SemanticData,
                    actual: data.source_product_roots().len(),
                    source,
                }
            })?;
            if roots != u32::from(entity_count) {
                return Err(PrepareError::SemanticEntityRoots {
                    roots,
                    entities: u32::from(entity_count),
                });
            }
        }
        if let Some(lane) = occurrences {
            lane.admit(u32::from(entity_count))
                .map_err(|fault| PrepareError::OccurrenceLane {
                    ordinal: 0,
                    fault: crate::ir::semantic_facts::occurrence_view_fault(fault),
                })?;
        }
        let type_fact_counts = if let Some(lane) = type_facts {
            lane.admit_schema(
                u32::from(entity_count),
                lane.children,
                crate::ir::FRAGMENT_SCHEMA,
            )
            .map_err(|fault| PrepareError::TypeFacts { fault })?;
            Some(
                lane.counts()
                    .map_err(|fault| PrepareError::TypeFacts { fault })?,
            )
        } else {
            None
        };
        if let Some(lane) = docs {
            lane.admit(u32::from(entity_count))
                .map_err(|fault| PrepareError::Documentation { fault })?;
        }
        if extensions.is_some() && type_facts.is_none() {
            return Err(PrepareError::ExtensionPoolsMismatch);
        }
        if let Some(lane) = pools {
            let type_count = type_fact_counts
                .map(|counts| counts.total())
                .transpose()
                .map_err(|fault| PrepareError::TypeFacts { fault })?
                .unwrap_or(0);
            lane.admit(u32::from(atom_count), type_count, u32::from(entity_count))
                .map_err(|fault| PrepareError::ExtensionPools { fault })?;
        }
        let layout = layout(
            FragmentFacts { source, recipe },
            FragmentCounts {
                entities: entity_count,
                type_nodes: type_node_count,
                atoms: atom_count,
                atom_bytes: atom_byte_count,
            },
            semantics,
        )?;

        for (ordinal, entity) in (0..u32::from(entity_count)).zip(entities) {
            if let Some(fault) = entity_fault(entity.semantic_type, type_node_count) {
                return Err(PrepareError::Entity {
                    ordinal: EntityId::new(ordinal),
                    fault: fault.into(),
                });
            }
            if let Some(fault) = entity_name_fault(entity.name, atom_count) {
                return Err(PrepareError::Entity {
                    ordinal: EntityId::new(ordinal),
                    fault: fault.into(),
                });
            }
        }
        for (ordinal, node) in (0..u32::from(type_node_count)).zip(type_nodes.iter().copied()) {
            if let Some(fault) = type_node_fault(node, type_node_count) {
                return Err(PrepareError::TypeNode {
                    ordinal: TypeId::new(ordinal),
                    fault,
                });
            }
        }

        Ok(Self {
            source,
            recipe,
            entities,
            type_nodes,
            atoms,
            semantic_data,
            occurrences,
            type_facts,
            docs,
            extensions,
            pools,
            layout,
        })
    }

    #[must_use]
    pub const fn required_capacity(&self) -> usize {
        self.layout.output_len
    }

    pub fn write_into<'output>(
        &self,
        output: &'output mut [u8],
    ) -> Result<&'output [u8], WriteError> {
        if output.len() < self.layout.output_len {
            return Err(WriteError::OutputTooSmall {
                required: self.layout.output_len,
                available: output.len(),
            });
        }

        let written = &mut output[..self.layout.output_len];
        written[HEADER_LAYOUT.magic..HEADER_LAYOUT.schema].copy_from_slice(&crate::ir::FRAGMENT_MAGIC);
        write_u16(written, HEADER_LAYOUT.schema, crate::ir::FRAGMENT_SCHEMA);
        write_u16(
            written,
            HEADER_LAYOUT.section_count,
            u16::from(SectionCount::from(
                6_u16
                    + u16::from(self.semantic_data.is_some())
                    + u16::from(self.occurrences.is_some())
                    + u16::from(self.type_facts.is_some())
                    + u16::from(self.docs.is_some())
                    + u16::from(self.extensions.is_some())
                    + u16::from(self.pools.is_some()),
            )),
        );
        write_u32(
            written,
            HEADER_LAYOUT.declared_length,
            u32::from(self.layout.output_wire_len),
        );
        write_directory_entry(written, 0, SectionKind::EntityTypes, self.layout.entities);
        write_directory_entry(written, 1, SectionKind::TypeNodes, self.layout.type_nodes);
        write_directory_entry(written, 2, SectionKind::AtomRecords, self.layout.atoms);
        write_directory_entry(written, 3, SectionKind::AtomBytes, self.layout.atom_bytes);
        write_directory_entry(
            written,
            4,
            SectionKind::SourceIdentity,
            self.layout.source_identity,
        );
        write_directory_entry(written, 5, SectionKind::RecipeFact, self.layout.recipe_fact);
        let mut ordinal: usize = 6;
        if let (Some(semantic_data), Some(data_layout)) =
            (self.semantic_data, self.layout.semantic_data)
        {
            write_directory_entry(written, ordinal, SectionKind::SemanticData, data_layout);
            write_semantic_data(written, data_layout, semantic_data)?;
            ordinal += 1;
        }
        if let (Some(lane), Some(occurrence_layout)) = (self.occurrences, self.layout.occurrences) {
            write_directory_entry(
                written,
                ordinal,
                SectionKind::Occurrences,
                occurrence_layout,
            );
            write_occurrence_payload(written, occurrence_layout, lane);
            ordinal += 1;
        }
        if let (Some(lane), Some(type_layout)) = (self.type_facts, self.layout.type_facts) {
            write_directory_entry(written, ordinal, SectionKind::TypeFacts, type_layout);
            lane.write_payload(&mut written[type_layout.range()]);
            ordinal += 1;
        }
        if let (Some(lane), Some(doc_layout)) = (self.docs, self.layout.documentation) {
            write_directory_entry(written, ordinal, SectionKind::Documentation, doc_layout);
            lane.write_payload(&mut written[doc_layout.range()]);
            ordinal += 1;
        }
        if let (Some(input), Some(extension_layout)) =
            (self.extensions, self.layout.language_extensions)
        {
            write_directory_entry(
                written,
                ordinal,
                SectionKind::LanguageExtensions,
                extension_layout,
            );
            let section = &mut written[extension_layout.range()];
            encode_fragment_extension_section(*input, section)
                .map_err(|fault| WriteError::ExtensionSection { fault })?;
            ordinal += 1;
        }
        if let (Some(lane), Some(pool_layout)) = (self.pools, self.layout.extension_pools) {
            write_directory_entry(written, ordinal, SectionKind::ExtensionPools, pool_layout);
            lane.write_payload(&mut written[pool_layout.range()]);
        }

        let mut entity_cursor = self.layout.entities.start_index;
        for entity in self.entities {
            write_entity(
                &mut written[entity_cursor..entity_cursor + ENTITY_BYTES],
                *entity,
            );
            entity_cursor += ENTITY_BYTES;
        }
        let mut type_cursor = self.layout.type_nodes.start_index;
        for node in self.type_nodes {
            write_type_node(
                &mut written[type_cursor..type_cursor + TYPE_NODE_BYTES],
                *node,
            );
            type_cursor += TYPE_NODE_BYTES;
        }
        write_atoms(written, self.layout, self.atoms)?;
        write_source_identity(
            &mut written[self.layout.source_identity.range()],
            self.source,
        );
        write_recipe_fact(&mut written[self.layout.recipe_fact.range()], self.recipe);
        Ok(written)
    }
}

fn count(step: LayoutStep, actual: usize) -> Result<ItemCount, PrepareError> {
    ItemCount::try_from(actual).map_err(|source| PrepareError::Count {
        lane: step,
        actual,
        source,
    })
}

fn atom_byte_count(atoms: &[AtomInput<'_>]) -> Result<ItemCount, PrepareError> {
    let mut total = 0_usize;
    for (ordinal, atom) in (0..u32::MAX).zip(atoms) {
        total = total
            .checked_add(atom.bytes.len())
            .ok_or(PrepareError::AtomBytePoolOverflow {
                ordinal: AtomId::new(ordinal),
            })?;
    }
    count(LayoutStep::AtomByteLane, total)
}

fn layout(
    facts: FragmentFacts,
    counts: FragmentCounts,
    semantics: FragmentSemantics<'_>,
) -> Result<FragmentLayout, PrepareError> {
    let FragmentCounts {
        entities: entity_count,
        type_nodes: type_node_count,
        atoms: atom_count,
        atom_bytes: atom_byte_count,
    } = counts;
    let FragmentSemantics {
        data: semantic_data,
        occurrences,
        type_facts,
        docs,
        extensions,
        pools,
    } = semantics;
    let section_count = SectionCount::from(
        6_u16
            + u16::from(semantic_data.is_some())
            + u16::from(occurrences.is_some())
            + u16::from(type_facts.is_some())
            + u16::from(docs.is_some())
            + u16::from(extensions.is_some())
            + u16::from(pools.is_some()),
    );
    let mut cursor = LayoutCursor::new(entity_count, type_node_count, section_count)?;
    let entities = cursor.lane(LayoutStep::EntityLane, entity_count, ENTITY_BYTES)?;
    let type_nodes = cursor.lane(LayoutStep::TypeNodeLane, type_node_count, TYPE_NODE_BYTES)?;
    let atoms = cursor.lane(LayoutStep::AtomRecordLane, atom_count, ATOM_RECORD_BYTES)?;
    let atom_bytes = cursor.lane(LayoutStep::AtomByteLane, atom_byte_count, 1)?;
    let source_identity = cursor.lane(
        LayoutStep::SourceIdentityLane,
        ItemCount::from(1),
        SOURCE_IDENTITY_BYTES,
    )?;
    let recipe_fact = cursor.lane(
        LayoutStep::RecipeFactLane,
        ItemCount::from(1),
        RECIPE_FACT_BYTES,
    )?;
    let semantic_lane = semantic_data
        .map(|data| semantic_data_lane(&mut cursor, data))
        .transpose()?;
    let occurrence_lane = occurrences
        .map(|lane| occurrence_lane(&mut cursor, lane))
        .transpose()?;
    let type_fact_lane = type_facts
        .map(|lane| type_fact_lane(&mut cursor, lane))
        .transpose()?;
    let type_fact_counts = type_facts
        .map(|lane| lane.counts())
        .transpose()
        .map_err(|fault| PrepareError::TypeFacts { fault })?;
    let documentation_lane = docs
        .map(|lane| byte_lane(&mut cursor, LayoutStep::Documentation, lane.payload_len()))
        .transpose()?;
    let language_extension_lane = extensions
        .map(|input| {
            byte_lane(
                &mut cursor,
                LayoutStep::LanguageExtensions,
                fragment_extension_section_len(*input).map_err(|_| {
                    PrepareError::LayoutOverflow {
                        step: LayoutStep::LanguageExtensions,
                        entity_count: 0,
                        type_node_count: 0,
                    }
                })?,
            )
        })
        .transpose()?;
    let extension_pool_lane = pools
        .map(|lane| byte_lane(&mut cursor, LayoutStep::ExtensionPools, lane.payload_len()))
        .transpose()?;
    cursor.finish(
        facts,
        FragmentLanes {
            entities,
            type_nodes,
            atoms,
            atom_bytes,
            source_identity,
            recipe_fact,
            semantic_data: semantic_lane,
            occurrences: occurrence_lane,
            type_facts: type_fact_lane,
            type_fact_counts,
            documentation: documentation_lane,
            language_extensions: language_extension_lane,
            extension_pools: extension_pool_lane,
        },
    )
}

fn byte_lane(
    cursor: &mut LayoutCursor,
    step: LayoutStep,
    payload: usize,
) -> Result<LaneLayout, PrepareError> {
    let count = ItemCount::try_from(payload).map_err(|_| PrepareError::LayoutOverflow {
        step,
        entity_count: 0,
        type_node_count: 0,
    })?;
    cursor.lane(step, count, 1)
}

#[derive(Clone, Copy)]
struct FragmentFacts {
    source: SourceIdentity,
    recipe: RecipeFact,
}

#[derive(Clone, Copy)]
struct FragmentCounts {
    entities: ItemCount,
    type_nodes: ItemCount,
    atoms: ItemCount,
    atom_bytes: ItemCount,
}

#[derive(Clone, Copy)]
struct FragmentLanes {
    entities: LaneLayout,
    type_nodes: LaneLayout,
    atoms: LaneLayout,
    atom_bytes: LaneLayout,
    source_identity: LaneLayout,
    recipe_fact: LaneLayout,
    semantic_data: Option<LaneLayout>,
    occurrences: Option<LaneLayout>,
    type_facts: Option<LaneLayout>,
    type_fact_counts: Option<crate::ir::TypeFactCounts>,
    documentation: Option<LaneLayout>,
    language_extensions: Option<LaneLayout>,
    extension_pools: Option<LaneLayout>,
}

struct LayoutCursor {
    next_index: usize,
    entity_count: ItemCount,
    type_node_count: ItemCount,
}

impl LayoutCursor {
    fn new(
        entity_count: ItemCount,
        type_node_count: ItemCount,
        section_count: SectionCount,
    ) -> Result<Self, PrepareError> {
        let mut cursor = Self {
            next_index: HEADER_LAYOUT.encoded_len,
            entity_count,
            type_node_count,
        };
        let directory_bytes = usize::from(section_count)
            .checked_mul(DIRECTORY_ENTRY_LAYOUT.encoded_len)
            .ok_or_else(|| cursor.overflow(LayoutStep::Directory))?;
        cursor.advance(LayoutStep::Directory, directory_bytes)?;
        Ok(cursor)
    }

    fn lane(
        &mut self,
        step: LayoutStep,
        count: ItemCount,
        item_width: usize,
    ) -> Result<LaneLayout, PrepareError> {
        let native_count = usize::try_from(count).map_err(|source| PrepareError::NativeCount {
            step,
            actual: u32::from(count),
            source,
        })?;
        let length = native_count
            .checked_mul(item_width)
            .ok_or_else(|| self.overflow(step))?;
        let start_index = self.next_index;
        self.advance(step, length)?;
        let start =
            ByteOffset::try_from(start_index).map_err(|source| PrepareError::OutputLength {
                actual: start_index,
                source,
            })?;
        let length = ByteLength::try_from(length).map_err(|source| PrepareError::OutputLength {
            actual: length,
            source,
        })?;
        Ok(LaneLayout {
            count,
            start,
            length,
            start_index,
            end_index: self.next_index,
        })
    }

    fn advance(&mut self, step: LayoutStep, length: usize) -> Result<(), PrepareError> {
        self.next_index = self
            .next_index
            .checked_add(length)
            .ok_or_else(|| self.overflow(step))?;
        Ok(())
    }

    fn finish(
        self,
        facts: FragmentFacts,
        lanes: FragmentLanes,
    ) -> Result<FragmentLayout, PrepareError> {
        let output_wire_len =
            ByteLength::try_from(self.next_index).map_err(|source| PrepareError::OutputLength {
                actual: self.next_index,
                source,
            })?;
        Ok(FragmentLayout {
            schema: crate::ir::FRAGMENT_SCHEMA,
            entities: lanes.entities,
            type_nodes: lanes.type_nodes,
            atoms: lanes.atoms,
            atom_bytes: lanes.atom_bytes,
            source_identity: lanes.source_identity,
            recipe_fact: lanes.recipe_fact,
            semantic_data: lanes.semantic_data,
            // Prepared fragments use this layout only for writing; reopen
            // derives the proof-carrying semantic-data offsets from bytes.
            semantic_data_layout: None,
            occurrences: lanes.occurrences,
            type_facts: lanes.type_facts,
            type_fact_counts: lanes.type_fact_counts,
            documentation: lanes.documentation,
            language_extensions: lanes.language_extensions,
            extension_pools: lanes.extension_pools,
            source: facts.source,
            recipe: facts.recipe,
            output_len: self.next_index,
            output_wire_len,
        })
    }

    fn overflow(&self, step: LayoutStep) -> PrepareError {
        PrepareError::LayoutOverflow {
            step,
            entity_count: u32::from(self.entity_count),
            type_node_count: u32::from(self.type_node_count),
        }
    }
}

fn write_atoms(
    output: &mut [u8],
    layout: FragmentLayout,
    atoms: &[AtomInput<'_>],
) -> Result<(), WriteError> {
    let mut record_cursor = layout.atoms.start_index;
    let mut bytes_cursor = layout.atom_bytes.start_index;
    let mut relative_start = 0_u32;
    for (ordinal, atom) in (0..u32::MAX).zip(atoms) {
        let atom_length =
            u32::try_from(atom.bytes.len()).map_err(|source| WriteError::AtomLength {
                ordinal: AtomId::new(ordinal),
                actual: atom.bytes.len(),
                source,
            })?;
        let bytes_end = bytes_cursor
            .checked_add(atom.bytes.len())
            .filter(|end| *end <= layout.atom_bytes.end_index)
            .ok_or(WriteError::AtomExtent {
                ordinal: AtomId::new(ordinal),
            })?;
        write_atom_record(
            &mut output[record_cursor..record_cursor + ATOM_RECORD_BYTES],
            relative_start,
            atom_length,
        );
        output[bytes_cursor..bytes_end].copy_from_slice(atom.bytes);
        record_cursor += ATOM_RECORD_BYTES;
        bytes_cursor = bytes_end;
        relative_start = relative_start
            .checked_add(atom_length)
            .ok_or(WriteError::AtomExtent {
                ordinal: AtomId::new(ordinal),
            })?;
    }
    Ok(())
}

fn write_directory_entry(output: &mut [u8], ordinal: usize, kind: SectionKind, lane: LaneLayout) {
    let start = HEADER_LAYOUT.encoded_len + ordinal * DIRECTORY_ENTRY_LAYOUT.encoded_len;
    write_u16(output, start + DIRECTORY_ENTRY_LAYOUT.kind, u16::from(kind));
    write_u16(
        output,
        start + DIRECTORY_ENTRY_LAYOUT.requirement,
        u16::from(SectionRequirement::Required),
    );
    write_u32(
        output,
        start + DIRECTORY_ENTRY_LAYOUT.item_count,
        u32::from(lane.count),
    );
    write_u32(
        output,
        start + DIRECTORY_ENTRY_LAYOUT.byte_offset,
        u32::from(lane.start),
    );
    write_u32(
        output,
        start + DIRECTORY_ENTRY_LAYOUT.byte_length,
        u32::from(lane.length),
    );
}

fn occurrence_lane(
    cursor: &mut LayoutCursor,
    lane: &crate::ir::semantic_facts::OccurrenceLane<'_>,
) -> Result<LaneLayout, PrepareError> {
    // One payload cell per byte, matching the fixed-width byte-lane grammar:
    // the directory proves `count * 1 == byte_length` and the payload header
    // declares the record count.
    let payload = lane.payload_len();
    let Ok(count) = ItemCount::try_from(payload) else {
        return Err(PrepareError::LayoutOverflow {
            step: LayoutStep::Occurrences,
            entity_count: 0,
            type_node_count: 0,
        });
    };
    cursor.lane(LayoutStep::Occurrences, count, 1)
}

fn type_fact_lane(
    cursor: &mut LayoutCursor,
    lane: &crate::ir::type_facts::TypeFactLane<'_>,
) -> Result<LaneLayout, PrepareError> {
    let payload = lane.payload_len();
    let count = ItemCount::try_from(payload).map_err(|_| PrepareError::LayoutOverflow {
        step: LayoutStep::TypeFacts,
        entity_count: 0,
        type_node_count: 0,
    })?;
    cursor.lane(LayoutStep::TypeFacts, count, 1)
}

fn write_occurrence_payload(
    output: &mut [u8],
    lane: LaneLayout,
    facts: &crate::ir::semantic_facts::OccurrenceLane<'_>,
) {
    let section = &mut output[lane.range()];
    facts.write_payload(section);
}

fn semantic_data_lane(
    cursor: &mut LayoutCursor,
    data: &CanonicalDataGraph<'_, '_>,
) -> Result<LaneLayout, PrepareError> {
    let metrics = data.metrics();
    let overflow = || PrepareError::SemanticDataOverflow {
        atoms: metrics.canonical_atom_count,
        products: metrics.canonical_product_count,
        children: metrics.canonical_child_count,
    };
    let mut payload = SEMANTIC_DATA_HEADER_BYTES;
    for (ordinal, atom) in (0..u32::MAX).zip(data.atoms()) {
        u32::try_from(atom.bytes.len()).map_err(|source| PrepareError::SemanticAtomLength {
            ordinal: AtomId::new(ordinal),
            actual: atom.bytes.len(),
            source,
        })?;
        payload = payload
            .checked_add(size_of::<u32>())
            .and_then(|payload| payload.checked_add(atom.bytes.len()))
            .ok_or_else(overflow)?;
    }
    let Ok(product_count) = usize::try_from(metrics.canonical_product_count) else {
        return Err(overflow());
    };
    let product_bytes = product_count
        .checked_mul(SEMANTIC_PRODUCT_BYTES)
        .ok_or_else(overflow)?;
    let Ok(constructor_count) = usize::try_from(metrics.canonical_constructor_count) else {
        return Err(overflow());
    };
    let constructor_bytes = constructor_count
        .checked_mul(SEMANTIC_CONSTRUCTOR_BYTES)
        .ok_or_else(overflow)?;
    let Ok(list_count) = usize::try_from(metrics.canonical_list_count) else {
        return Err(overflow());
    };
    let list_bytes = list_count
        .checked_mul(SEMANTIC_LIST_BYTES)
        .ok_or_else(overflow)?;
    let Ok(child_count) = usize::try_from(metrics.canonical_child_count) else {
        return Err(overflow());
    };
    let child_bytes = child_count
        .checked_mul(SEMANTIC_CHILD_BYTES)
        .ok_or_else(overflow)?;
    let root_bytes = data
        .source_product_roots()
        .len()
        .checked_mul(size_of::<u32>())
        .ok_or_else(overflow)?;
    payload = payload
        .checked_add(product_bytes)
        .and_then(|payload| payload.checked_add(constructor_bytes))
        .and_then(|payload| payload.checked_add(list_bytes))
        .and_then(|payload| payload.checked_add(child_bytes))
        .and_then(|payload| payload.checked_add(root_bytes))
        .ok_or_else(overflow)?;
    // One payload cell per byte, matching the fixed-width byte-lane grammar:
    // the directory proves `count * 1 == byte_length`.
    let Ok(count) = ItemCount::try_from(payload) else {
        return Err(overflow());
    };
    cursor.lane(LayoutStep::SemanticData, count, 1)
}

fn write_semantic_data(
    output: &mut [u8],
    lane: LaneLayout,
    data: &CanonicalDataGraph<'_, '_>,
) -> Result<(), WriteError> {
    let section = &mut output[lane.range()];
    let metrics = data.metrics();
    write_u32(section, 0, metrics.canonical_atom_count);
    write_u32(section, size_of::<u32>(), metrics.canonical_product_count);
    write_u32(
        section,
        size_of::<u32>() * 2,
        metrics.canonical_constructor_count,
    );
    write_u32(section, size_of::<u32>() * 3, metrics.canonical_list_count);
    write_u32(section, size_of::<u32>() * 4, metrics.canonical_child_count);
    write_u32(
        section,
        size_of::<u32>() * 5,
        u32::try_from(data.source_product_roots().len()).unwrap_or(u32::MAX),
    );

    let mut cursor = SEMANTIC_DATA_HEADER_BYTES;
    for (ordinal, atom) in (0..u32::MAX).zip(data.atoms()) {
        let length =
            u32::try_from(atom.bytes.len()).map_err(|source| WriteError::SemanticAtomLength {
                ordinal: AtomId::new(ordinal),
                actual: atom.bytes.len(),
                source,
            })?;
        write_u32(section, cursor, length);
        cursor += size_of::<u32>();
        section[cursor..cursor + atom.bytes.len()].copy_from_slice(atom.bytes);
        cursor += atom.bytes.len();
    }
    for product in data.products() {
        write_u32(section, cursor, product.head.raw);
        write_u32(section, cursor + size_of::<u32>(), product.children.raw);
        cursor += SEMANTIC_PRODUCT_BYTES;
    }
    for constructor in data.constructors() {
        write_u32(section, cursor, u32::from(constructor.tag));
        write_u32(section, cursor + size_of::<u32>(), constructor.payload0);
        write_u32(section, cursor + size_of::<u32>() * 2, constructor.payload1);
        cursor += SEMANTIC_CONSTRUCTOR_BYTES;
    }
    for span in data.lists() {
        write_u32(section, cursor, span.start);
        write_u32(section, cursor + size_of::<u32>(), span.length);
        cursor += SEMANTIC_LIST_BYTES;
    }
    for child in data.children() {
        section[cursor] = u8::from(child.role);
        match child.target {
            ProductRef::Local(target) => {
                section[cursor + size_of::<u8>()] = SEMANTIC_LOCAL_TAG;
                write_u32(section, cursor + size_of::<u8>() * 2, target.raw);
                section[cursor + size_of::<u8>() * 2 + size_of::<u32>()
                    ..cursor + SEMANTIC_CHILD_BYTES]
                    .fill(0);
            }
            ProductRef::External(target) => {
                section[cursor + size_of::<u8>()] = SEMANTIC_EXTERNAL_TAG;
                write_u32(section, cursor + size_of::<u8>() * 2, target.ordinal);
                section[cursor + size_of::<u8>() * 2 + size_of::<u32>()
                    ..cursor + SEMANTIC_CHILD_BYTES]
                    .copy_from_slice(target.fragment.as_ref());
            }
        }
        cursor += SEMANTIC_CHILD_BYTES;
    }
    for root in data.source_product_roots() {
        write_u32(section, cursor, *root);
        cursor += size_of::<u32>();
    }
    Ok(())
}
