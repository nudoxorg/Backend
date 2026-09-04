//! Full entity-row reference projection.
//!
//! Canonical entity order remains owned by the core declaration-identity
//! index. This module supplies the full-only key and remapped pooled
//! coordinates that make a later entity wire row truthful without letting a
//! type/list insertion order leak through it.

use alloc::vec::Vec;

use crate::{ArenaRange, Ir};

use super::super::typed::TypedDependencyPlan;
use super::{FullEntityFault, FullEntityPlan, FullPlanError, TerminalPools};

impl FullEntityPlan {
    pub(super) fn build(
        ir: &Ir,
        typed: &TypedDependencyPlan<'_>,
        terminal: &TerminalPools,
    ) -> Result<Self, FullPlanError> {
        let canonical = typed.canonical();
        let entity_count = ir.entity_count();
        let entity_count_u32 = u32::try_from(entity_count)
            .map_err(|_| FullEntityFault::GeometryOverflow { rows: entity_count })?;
        let mut rows = Vec::with_capacity(entity_count);
        let mut key_bytes = Vec::new();
        let mut key_ranges = Vec::with_capacity(entity_count);
        for canonical_row in 0..entity_count {
            let entity = *canonical.core.entities.get(canonical_row).ok_or(
                FullEntityFault::MissingEntity {
                    entity: crate::EntityId::new(
                        u32::try_from(canonical_row)
                            .map_err(|_| FullEntityFault::GeometryOverflow { rows: entity_count })?,
                    ),
                    count: entity_count_u32,
                },
            )?;
            let facts = ir.semantic_entity(entity).ok_or(FullEntityFault::MissingEntity {
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
            let start = key_bytes.len();
            append_entity_key(&mut key_bytes, row);
            let end = key_bytes.len();
            key_ranges.push(ArenaRange {
                start: u32::try_from(start)
                    .map_err(|_| FullEntityFault::KeyLengthOverflow { entity })?,
                len: u32::try_from(end.checked_sub(start).ok_or(
                    FullEntityFault::KeyLengthOverflow { entity },
                )?)
                .map_err(|_| FullEntityFault::KeyLengthOverflow { entity })?,
            });
            rows.push(row);
        }
        Ok(Self { rows, key_bytes, key_ranges })
    }
}

fn append_entity_key(out: &mut Vec<u8>, row: FullEntityRow) {
    // The row is pinned to the already canonical exact declaration identity;
    // this full-only suffix records every entity-owned pooled coordinate.
    out.push(full_entity_row_tag());
    out.extend_from_slice(&row.canonical_entity.to_le_bytes());
    match row.semantic_type {
        Some(semantic_type) => {
            out.push(present_tag());
            out.extend_from_slice(&semantic_type.to_le_bytes());
        }
        None => {
            out.push(absent_tag());
            out.extend_from_slice(&0_u32.to_le_bytes());
        }
    }
    out.extend_from_slice(&row.members.to_le_bytes());
    out.extend_from_slice(&row.docs.to_le_bytes());
    out.extend_from_slice(&row.attributes.to_le_bytes());
}

const fn full_entity_row_tag() -> u8 { 0 }
const fn absent_tag() -> u8 { 0 }
const fn present_tag() -> u8 { 1 }
