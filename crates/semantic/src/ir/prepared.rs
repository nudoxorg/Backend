//! Defines prepared behavior for `backend-semantic::ir`, whose purpose is to encode, validate, map, and borrow canonical compiler IR fragments.
//! This module owns the prepared invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
use core::num::TryFromIntError;

use crate::ir::{AtomId, EntityId, TypeId};
use thiserror::Error;

use crate::ir::{
    AtomInput, CanonicalDataError, EntityRecord, EntityRecordFault, RecipeFact, SourceIdentity,
    TypeNode, TypeNodeFault,
    canonical_data::CanonicalDataGraph,
    docs_facts::DocumentationLane,
    extension_pools::ExtensionPoolsLane,
    semantic_extension_section::{ExtensionSectionInput, encode_fragment_extension_section},
    wire::{
        ENTITY_BYTES, FragmentLayout, HEADER_LAYOUT, SectionCount, SectionKind, TYPE_NODE_BYTES,
        entity_fault, entity_name_fault, type_node_fault, write_entity, write_recipe_fact,
        write_source_identity, write_type_node, write_u16, write_u32,
    },
};

mod layout;

use layout::{
    FragmentCounts, FragmentFacts, atom_byte_count, count, write_atoms, write_directory_entry,
    write_occurrence_payload, write_semantic_data,
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
        let layout = layout::layout(
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
