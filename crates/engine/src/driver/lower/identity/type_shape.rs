//! Coordinate-free type-row shapes.
//!
//! Anonymous and computed rows hash by their lattice cells and ordered
//! children. Staged offsets and ownership rows stay out of the bytes, and a
//! structural cycle is rejected with its exact staged row.

use super::{ANONYMOUS_ROW_BASE, COMPUTED_ROW_BASE, FactSet, STAGED_TEXT_CHILD};
use backend_semantic::ir::{CorePayloadHash, NominalRef, SemanticTypeRecord};

pub(super) const MAX_CHILD_SHAPE_BYTES: usize = 64 * 16;

pub(super) fn append_nominal_payload(
    preimage: &mut Vec<u8>,
    facts: &FactSet<'_>,
    nominal: NominalRef,
    locators: &[CorePayloadHash],
    type_shapes: &mut [Option<CorePayloadHash>],
    type_visiting: &mut [bool],
) -> Result<(), backend_semantic::ir::BuildError> {
    match nominal {
        NominalRef::Local(target) => {
            preimage.push(0);
            append_type_target_payload(
                preimage,
                facts,
                target.raw,
                None,
                0,
                locators,
                type_shapes,
                type_visiting,
            )
        }
        NominalRef::External(target) => {
            preimage.push(1);
            preimage.extend_from_slice(target.fragment.as_ref());
            preimage.extend_from_slice(&target.ordinal.to_le_bytes());
            Ok(())
        }
        NominalRef::Stable(stable) => {
            preimage.push(2);
            preimage.extend_from_slice(stable.fragment.as_ref());
            preimage.extend_from_slice(stable.declaration.family.as_bytes());
            preimage.extend_from_slice(stable.declaration.variant.as_bytes());
            Ok(())
        }
    }
}

pub(super) fn append_type_target_payload(
    preimage: &mut Vec<u8>,
    facts: &FactSet<'_>,
    target: u32,
    name: Option<&[u8]>,
    flags: u8,
    locators: &[CorePayloadHash],
    type_shapes: &mut [Option<CorePayloadHash>],
    type_visiting: &mut [bool],
) -> Result<(), backend_semantic::ir::BuildError> {
    match name {
        Some(name) => {
            preimage.push(1);
            preimage.extend_from_slice(&(name.len() as u64).to_le_bytes());
            preimage.extend_from_slice(name);
        }
        None => preimage.push(0),
    }
    preimage.push(flags);
    if target == STAGED_TEXT_CHILD {
        preimage.extend_from_slice(b"compiler.template-text-child.v1");
        return Ok(());
    }
    if target < facts.len as u32 {
        preimage.push(0);
        let locator = locators
            .get(target as usize)
            .ok_or_else(|| dangling_entity(target as usize))?;
        preimage.extend_from_slice(locator.as_bytes());
        return Ok(());
    }
    preimage.push(1);
    preimage.extend_from_slice(
        type_shape(facts, target, locators, type_shapes, type_visiting)?.as_bytes(),
    );
    Ok(())
}

/// Coordinate-free shape for an anonymous/computed type.  This is structural
/// only: ownership rows and staged offsets never enter the payload or an
/// overload signature. A structural cycle is rejected with its exact staged
/// row rather than being assigned a traversal-dependent marker.
fn type_shape(
    facts: &FactSet<'_>,
    row: u32,
    locators: &[CorePayloadHash],
    cache: &mut [Option<CorePayloadHash>],
    visiting: &mut [bool],
) -> Result<CorePayloadHash, backend_semantic::ir::BuildError> {
    let slot =
        staged_type_slot(facts, row).ok_or_else(|| backend_semantic::ir::BuildError::Dangling {
            space: backend_semantic::ir::SemanticSpace::Type,
            raw: row,
        })?;
    if let Some(shape) = cache[slot] {
        return Ok(shape);
    }
    let mut stack = Vec::new();
    stack.push((row, false));
    while let Some((current, exit)) = stack.pop() {
        let current_slot =
            staged_type_slot(facts, current).ok_or_else(|| dangling_type(current))?;
        if cache[current_slot].is_some() {
            continue;
        }
        if !exit {
            if visiting[current_slot] {
                continue;
            }
            visiting[current_slot] = true;
            stack.push((current, true));
            let (_, targets, _, _) = staged_type_row(facts, current)?;
            for target in targets.iter().rev().copied() {
                if target >= facts.len as u32 && target != STAGED_TEXT_CHILD {
                    let nested =
                        staged_type_slot(facts, target).ok_or_else(|| dangling_type(target))?;
                    if visiting[nested] {
                        return Err(backend_semantic::ir::BuildError::RecursiveType {
                            raw: target,
                        });
                    }
                    if cache[nested].is_none() {
                        stack.push((target, false));
                    }
                }
            }
            continue;
        }
        let (record, targets, names, flags) = staged_type_row(facts, current)?;
        let mut preimage = Vec::with_capacity(64 + targets.len() * 24);
        preimage.extend_from_slice(b"compiler.staged-type-payload.v3");
        preimage.push(u8::from(record.tag));
        append_tag_owned_scalars(&mut preimage, record);
        for text in [record.text, record.text2] {
            match text {
                Some(text) => {
                    preimage.push(1);
                    preimage.extend_from_slice(
                        &u32::try_from(text.len())
                            .map_err(|_| dangling_type(current))?
                            .to_le_bytes(),
                    );
                    preimage.extend_from_slice(text);
                }
                None => preimage.push(0),
            }
        }
        match record.nominal {
            None => preimage.push(0),
            Some(NominalRef::Local(local)) => {
                preimage.push(1);
                preimage.push(0);
                preimage.extend_from_slice(
                    locators
                        .get(local.raw as usize)
                        .ok_or_else(|| dangling_entity(local.raw as usize))?
                        .as_bytes(),
                );
            }
            Some(NominalRef::External(external)) => {
                preimage.push(1);
                preimage.push(1);
                preimage.extend_from_slice(external.fragment.as_ref());
                preimage.extend_from_slice(&external.ordinal.to_le_bytes());
            }
            Some(NominalRef::Stable(stable)) => {
                preimage.push(1);
                preimage.push(2);
                preimage.extend_from_slice(stable.fragment.as_ref());
                preimage.extend_from_slice(stable.declaration.family.as_bytes());
                preimage.extend_from_slice(stable.declaration.variant.as_bytes());
            }
        }
        preimage.extend_from_slice(
            &u32::try_from(targets.len())
                .map_err(|_| dangling_type(current))?
                .to_le_bytes(),
        );
        for ((target, name), flag) in targets.iter().zip(names).zip(flags) {
            match name {
                Some(name) => {
                    preimage.push(1);
                    preimage.extend_from_slice(
                        &u32::try_from(name.len())
                            .map_err(|_| dangling_type(current))?
                            .to_le_bytes(),
                    );
                    preimage.extend_from_slice(name);
                }
                None => preimage.push(0),
            }
            preimage.push(*flag);
            if *target == STAGED_TEXT_CHILD {
                preimage.push(2);
                continue;
            }
            if (*target as usize) < facts.len {
                preimage.push(0);
                preimage.extend_from_slice(
                    locators
                        .get(*target as usize)
                        .ok_or_else(|| dangling_entity(*target as usize))?
                        .as_bytes(),
                );
                continue;
            }
            preimage.push(1);
            let nested = staged_type_slot(facts, *target).ok_or_else(|| dangling_type(*target))?;
            preimage.extend_from_slice(
                cache[nested]
                    .ok_or(backend_semantic::ir::BuildError::RecursiveType { raw: *target })?
                    .as_bytes(),
            );
        }
        cache[current_slot] = Some(CorePayloadHash::from_canonical_bytes(&preimage));
        visiting[current_slot] = false;
    }
    cache[slot].ok_or_else(|| dangling_type(row))
}

/// Only tag-owned scalar cells may enter an identity preimage directly.
/// Structural target/list coordinates are represented by ordered child or
/// nominal frames below; they are never hashed as staging coordinates.
pub(super) fn append_tag_owned_scalars(out: &mut Vec<u8>, record: SemanticTypeRecord<'_>) {
    use backend_semantic::ir::SemanticTypeTag as Tag;
    match record.tag {
        Tag::Primitive
        | Tag::Array
        | Tag::ArrayRectangular
        | Tag::ArrayFixed
        | Tag::FunctionPointer
        | Tag::Channel
        | Tag::Annotated
        | Tag::AnonymousRecord
        | Tag::Wildcard
        | Tag::Mapped
        | Tag::CQualified
        | Tag::Unknown => {
            out.push(1);
            out.extend_from_slice(&record.payload0.to_le_bytes());
            out.extend_from_slice(&record.payload1.to_le_bytes());
        }
        _ => out.push(0),
    }
}

pub(super) fn staged_type_slot(facts: &FactSet<'_>, row: u32) -> Option<usize> {
    if row < ANONYMOUS_ROW_BASE {
        return ((row as usize) < facts.len).then_some(row as usize);
    }
    if row >= COMPUTED_ROW_BASE {
        let computed = row - COMPUTED_ROW_BASE;
        return ((computed as usize) < facts.computed_rows)
            .then_some(facts.len + facts.anonymous_rows + computed as usize);
    }
    let anonymous = row - ANONYMOUS_ROW_BASE;
    ((anonymous as usize) < facts.anonymous_rows).then_some(facts.len + anonymous as usize)
}

pub(super) fn staged_type_row<'facts, 'source>(
    facts: &'facts FactSet<'source>,
    row: u32,
) -> Result<
    (
        SemanticTypeRecord<'source>,
        &'facts [u32],
        &'facts [Option<&'source [u8]>],
        &'facts [u8],
    ),
    backend_semantic::ir::BuildError,
> {
    if row < ANONYMOUS_ROW_BASE {
        let ordinal = row as usize;
        let start = *facts
            .type_child_starts
            .get(ordinal)
            .ok_or_else(|| dangling_type(row))? as usize;
        let count = usize::from(
            *facts
                .type_child_counts
                .get(ordinal)
                .ok_or_else(|| dangling_type(row))?,
        );
        return Ok((
            facts.type_records[ordinal],
            &facts.type_child_targets[start..start + count],
            &facts.type_child_names[start..start + count],
            &facts.type_child_flags[start..start + count],
        ));
    }
    if row < COMPUTED_ROW_BASE {
        let ordinal = (row - ANONYMOUS_ROW_BASE) as usize;
        if ordinal >= facts.anonymous_rows {
            return Err(dangling_type(row));
        }
        let start = facts.anonymous_child_starts[ordinal] as usize;
        let count = usize::from(facts.anonymous_child_counts[ordinal]);
        return Ok((
            facts.anonymous_records[ordinal],
            &facts.anonymous_child_targets[start..start + count],
            &facts.anonymous_child_names[start..start + count],
            &facts.anonymous_child_flags[start..start + count],
        ));
    }
    let ordinal = (row - COMPUTED_ROW_BASE) as usize;
    if ordinal >= facts.computed_rows {
        return Err(dangling_type(row));
    }
    let start = facts.computed_child_starts[ordinal] as usize;
    let count = usize::from(facts.computed_child_counts[ordinal]);
    Ok((
        facts.computed_records[ordinal],
        &facts.computed_child_targets[start..start + count],
        &facts.computed_child_names[start..start + count],
        &facts.computed_child_flags[start..start + count],
    ))
}

pub(super) fn dangling_entity(ordinal: usize) -> backend_semantic::ir::BuildError {
    backend_semantic::ir::BuildError::Dangling {
        space: backend_semantic::ir::SemanticSpace::Entity,
        raw: u32::try_from(ordinal).unwrap_or(u32::MAX),
    }
}

pub(super) fn dangling_type(row: u32) -> backend_semantic::ir::BuildError {
    backend_semantic::ir::BuildError::Dangling {
        space: backend_semantic::ir::SemanticSpace::Type,
        raw: row,
    }
}
