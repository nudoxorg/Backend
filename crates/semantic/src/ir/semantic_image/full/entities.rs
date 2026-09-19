//! Full entity-row reference projection.
//!
//! Canonical entity order remains owned by the core declaration-identity
//! index. This module supplies the full-only key and remapped pooled
//! coordinates that make a later entity wire row truthful without letting a
//! type/list insertion order leak through it.

use alloc::vec::Vec;

use crate::ir::Ir;

use super::super::typed::TypedDependencyPlan;
use super::{FullEntityFault, FullEntityPlan, FullEntityRow, FullPlanError, TerminalPools};

impl FullEntityPlan {
    pub(crate) fn build(
        ir: &Ir,
        typed: &TypedDependencyPlan<'_>,
        terminal: &TerminalPools,
    ) -> Result<Self, FullPlanError> {
        let canonical = typed.canonical();
        let entity_count = ir.entity_count();
        let entity_count_u32 = u32::try_from(entity_count)
            .map_err(|_| FullEntityFault::GeometryOverflow { rows: entity_count })?;
        let mut rows = Vec::with_capacity(entity_count);
        for canonical_row in 0..entity_count {
            let entity =
                *canonical.core.entities.get(canonical_row).ok_or(
                    FullEntityFault::MissingEntity {
                        entity: crate::ir::EntityId::new(u32::try_from(canonical_row).map_err(
                            |_| FullEntityFault::GeometryOverflow { rows: entity_count },
                        )?),
                        count: entity_count_u32,
                    },
                )?;
            let facts = ir
                .semantic_entity(entity)
                .ok_or(FullEntityFault::MissingEntity {
                    entity,
                    count: entity_count_u32,
                })?;
            let semantic_type = facts
                .semantic_type
                .map(|id| typed.canonical_type(id))
                .transpose()?;
            let members = terminal.member(facts.members)?;
            let docs = terminal.docs(facts.docs)?;
            let attributes = typed.canonical_atom_list(facts.attributes)?;
            let row = FullEntityRow {
                entity,
                canonical_entity: canonical.entity(entity)?,
                semantic_type,
                members,
                docs,
                attributes,
            };
            rows.push(row);
        }
        Ok(Self { rows })
    }
}
