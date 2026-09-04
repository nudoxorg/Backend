//! Coordinate-free declaration identity and payload materialization.
//!
//! Stable declaration identity is deliberately smaller than semantic payload:
//! a declaration's scope, language profile, and closed parentage state are
//! always present, while a structural signature enters only to distinguish a
//! genuine same-scope/kind/name sibling collision.  Type and member content
//! remain in the payload plane, so ordinary edits do not remint identities.

use core::cmp::Ordering;

use compiler_ir::{
    DeclarationKey, DeclarationParentage, Disambiguator, EntityId, EntityKind, EntityVersion,
    NominalRef, PayloadHash, ScopedDeclarationKey, SemanticTypeRecord, StableEntityId,
};
use compiler_vocabulary::LanguageProfile;

use super::{
    FactSet, SemanticFact, ANONYMOUS_ROW_BASE, COMPUTED_ROW_BASE, STAGED_TEXT_CHILD,
};
use crate::types::{DeclarationScope, ParentageState};

/// Builds every admitted entity version.  Siblings are sorted and grouped
/// exactly once by their authority parent-state, kind, and name.  That is
/// O(F log F) grouping, followed by linear stable-parent resolution and
/// payload shaping; neither an ordinal nor an insertion-neighbor enters a
/// minted key.
pub(super) fn versions(
    facts: &FactSet<'_>,
    scope: DeclarationScope<'_>,
    profile: LanguageProfile,
) -> Result<Box<[EntityVersion]>, compiler_ir::BuildError> {
    let count = facts.len;
    let member_payloads = member_payloads(facts)?;
    let collisions = collision_signatures(facts)?;
    let mut payloads = vec![None; count].into_boxed_slice();
    let mut type_shapes = vec![None; facts.len + facts.anonymous_rows + facts.computed_rows]
        .into_boxed_slice();
    let mut type_visiting = vec![false; type_shapes.len()].into_boxed_slice();
    let mut resolved = vec![None; count].into_boxed_slice();
    let mut visiting = vec![false; count].into_boxed_slice();
    for ordinal in 0..count {
        let _ = resolve_version(
            facts,
            ordinal,
            scope,
            profile,
            &collisions,
            &member_payloads,
            &mut payloads,
            &mut type_shapes,
            &mut type_visiting,
            &mut resolved,
            &mut visiting,
        )?;
    }
    resolved
        .iter()
        .copied()
        .collect::<Option<Vec<_>>>()
        .map(Vec::into_boxed_slice)
        .ok_or(compiler_ir::BuildError::Dangling {
            space: compiler_ir::SemanticSpace::Entity,
            raw: u32::try_from(count).unwrap_or(u32::MAX),
        })
}

#[allow(clippy::too_many_arguments)]
fn resolve_version(
    facts: &FactSet<'_>,
    ordinal: usize,
    scope: DeclarationScope<'_>,
    profile: LanguageProfile,
    collisions: &[Option<PayloadHash>],
    member_payloads: &[PayloadHash],
    payloads: &mut [Option<PayloadHash>],
    type_shapes: &mut [Option<PayloadHash>],
    type_visiting: &mut [bool],
    resolved: &mut [Option<EntityVersion>],
    visiting: &mut [bool],
) -> Result<EntityVersion, compiler_ir::BuildError> {
    let Some(slot) = resolved.get(ordinal) else {
        return Err(dangling_entity(ordinal));
    };
    if let Some(version) = *slot {
        return Ok(version);
    }
    if visiting.get(ordinal).copied().unwrap_or(false) {
        return Err(compiler_ir::BuildError::ParentCycle {
            entity: EntityId::new(u32::try_from(ordinal).unwrap_or(u32::MAX)),
        });
    }
    visiting[ordinal] = true;
    let parentage = facts
        .provenance
        .parentage()
        .get(ordinal)
        .copied()
        .ok_or_else(|| dangling_entity(ordinal))?;
    let parentage = match parentage {
        ParentageState::Root => DeclarationParentage::Root,
        ParentageState::Unavailable => DeclarationParentage::Unavailable,
        ParentageState::UnrepresentedAuthorityOwner { identity } => {
            DeclarationParentage::Unrepresented(identity)
        }
        ParentageState::Bound { parent } => {
            let parent = parent.raw as usize;
            if parent >= facts.len {
                return Err(dangling_entity(parent));
            }
            DeclarationParentage::Bound(resolve_version(
                    facts,
                    parent,
                    scope,
                    profile,
                    collisions,
                    member_payloads,
                    payloads,
                    type_shapes,
                    type_visiting,
                    resolved,
                    visiting,
                )?
                .stable,
            )
        }
    };
    let payload = payload_for(
        facts,
        ordinal,
        member_payloads,
        payloads,
        type_shapes,
        type_visiting,
    )?;
    let declaration = DeclarationKey::new(
        scope.lineage(),
        scope.path(),
        facts.kinds[ordinal],
        facts.names[ordinal],
    )
    .map_err(|cause| compiler_ir::BuildError::DeclarationKey {
        entity: EntityId::new(u32::try_from(ordinal).unwrap_or(u32::MAX)),
        cause,
    })?;
    let scoped = ScopedDeclarationKey::new(declaration, profile, parentage);
    let disambiguator = match collisions.get(ordinal).copied().flatten() {
        Some(collision) => Disambiguator::Skeleton(collision.as_ref()),
        None => Disambiguator::None,
    };
    let mut preimage = vec![0_u8; scoped.preimage_len(disambiguator)];
    let stable = scoped
        .stable_id(disambiguator, &mut preimage)
        .map_err(|cause| compiler_ir::BuildError::ScopedDeclarationPreimage {
            entity: EntityId::new(u32::try_from(ordinal).unwrap_or(u32::MAX)),
            cause,
        })?;
    let mut stable_bytes = [0_u8; 16];
    stable_bytes.copy_from_slice(&stable.as_ref()[..16]);
    let version = EntityVersion {
        stable: StableEntityId::from_raw(stable_bytes),
        payload,
    };
    visiting[ordinal] = false;
    resolved[ordinal] = Some(version);
    Ok(version)
}

/// Groups Bound topology rows once, then assigns every parent one
/// coordinate-free local-member payload summary.  The group sort is the only
/// topology aggregation: `payload_for` merely reads its aligned summary and
/// never scans children.  Child direct bases retain their kind/name/type and
/// constructor facts without borrowing a parent's payload or row coordinate.
fn member_payloads(
    facts: &FactSet<'_>,
) -> Result<Box<[PayloadHash]>, compiler_ir::BuildError> {
    let empty = PayloadHash::from_canonical_bytes(b"compiler.local-members.v1\0");
    let mut summaries = vec![empty; facts.len].into_boxed_slice();
    let mut members = Vec::new();
    for (child, parentage) in facts.provenance.parentage().iter().take(facts.len).enumerate() {
        let ParentageState::Bound { parent } = *parentage else {
            continue;
        };
        let parent = parent.raw as usize;
        if parent >= facts.len {
            return Err(dangling_entity(parent));
        }
        let basis = *facts
            .key_digests
            .get(child)
            .ok_or_else(|| dangling_entity(child))?;
        members.push((parent, basis));
    }
    members.sort_unstable_by(|(left_parent, left_basis), (right_parent, right_basis)| {
        left_parent
            .cmp(right_parent)
            .then_with(|| left_basis.as_ref().cmp(right_basis.as_ref()))
    });
    let mut start = 0;
    while start < members.len() {
        let parent = members[start].0;
        let mut end = start + 1;
        while end < members.len() && members[end].0 == parent {
            end += 1;
        }
        let count = u64::try_from(end - start).map_err(|_| dangling_entity(parent))?;
        let mut preimage = Vec::with_capacity(32 + (end - start) * 16);
        preimage.extend_from_slice(b"compiler.local-members.v1");
        preimage.extend_from_slice(&count.to_le_bytes());
        for (_, basis) in &members[start..end] {
            preimage.extend_from_slice(basis.as_ref());
        }
        summaries[parent] = PayloadHash::from_canonical_bytes(&preimage);
        start = end;
    }
    Ok(summaries)
}

/// Separates real overload/implementation collisions from ordinary unique
/// declarations.  The group locator uses staging parent coordinates only to
/// find authority siblings; they are never copied into an identity preimage.
fn collision_signatures(
    facts: &FactSet<'_>,
) -> Result<Box<[Option<PayloadHash>]>, compiler_ir::BuildError> {
    let mut ordinals = (0..facts.len).collect::<Vec<_>>();
    ordinals.sort_unstable_by(|left, right| sibling_order(facts, *left, *right));
    let mut signatures = vec![None; facts.len].into_boxed_slice();
    let mut shapes = vec![None; facts.len].into_boxed_slice();
    let mut type_shapes = vec![None; facts.len + facts.anonymous_rows + facts.computed_rows]
        .into_boxed_slice();
    let mut type_visiting = vec![false; type_shapes.len()].into_boxed_slice();
    let mut start = 0;
    while start < ordinals.len() {
        let mut end = start + 1;
        while end < ordinals.len()
            && same_sibling_key(facts, ordinals[start], ordinals[end])
        {
            end += 1;
        }
        if end - start > 1 {
            let mut group = Vec::with_capacity(end - start);
            for ordinal in &ordinals[start..end] {
                group.push((
                    collision_shape_for(
                        facts,
                        *ordinal,
                        &mut shapes,
                        &mut type_shapes,
                        &mut type_visiting,
                    )?,
                    *ordinal,
                ));
            }
            group.sort_unstable_by(|(left, _), (right, _)| left.as_ref().cmp(right.as_ref()));
            let mut duplicate_start = 0;
            while duplicate_start < group.len() {
                let mut duplicate_end = duplicate_start + 1;
                while duplicate_end < group.len()
                    && group[duplicate_start].0 == group[duplicate_end].0
                {
                    duplicate_end += 1;
                }
                if duplicate_end - duplicate_start > 1 {
                    let ordinal = group[duplicate_start].1;
                    return Err(compiler_ir::BuildError::IndistinguishableDeclarationSiblings {
                        entity: EntityId::new(u32::try_from(ordinal).unwrap_or(u32::MAX)),
                        kind: facts.kinds[ordinal],
                        name: PayloadHash::from_canonical_bytes(facts.names[ordinal]),
                        skeleton: group[duplicate_start].0,
                        count: u32::try_from(duplicate_end - duplicate_start).unwrap_or(u32::MAX),
                    });
                }
                duplicate_start = duplicate_end;
            }
            for (signature, ordinal) in group {
                signatures[ordinal] = Some(signature);
            }
        }
        start = end;
    }
    Ok(signatures)
}

fn sibling_order(facts: &FactSet<'_>, left: usize, right: usize) -> Ordering {
    sibling_parentage(facts, left)
        .cmp(&sibling_parentage(facts, right))
        .then_with(|| u16::from(facts.kinds[left]).cmp(&u16::from(facts.kinds[right])))
        .then_with(|| facts.names[left].cmp(facts.names[right]))
}

fn same_sibling_key(facts: &FactSet<'_>, left: usize, right: usize) -> bool {
    sibling_parentage(facts, left) == sibling_parentage(facts, right)
        && facts.kinds[left] == facts.kinds[right]
        && facts.names[left] == facts.names[right]
}

/// Fixed sort/group representation only.  It contains no semantic identity
/// input: a bound parent's staging ordinal is legal here because grouping is
/// ephemeral and proves the two rows share one authority parent.
#[derive(Clone, Copy, Eq, PartialEq, Ord, PartialOrd)]
enum SiblingParentage {
    Root,
    Bound(u32),
    Unrepresented([u8; 16]),
    Unavailable,
}

fn sibling_parentage(facts: &FactSet<'_>, ordinal: usize) -> SiblingParentage {
    match facts.provenance.parentage()[ordinal] {
        ParentageState::Root => SiblingParentage::Root,
        ParentageState::Bound { parent } => SiblingParentage::Bound(parent.raw),
        ParentageState::UnrepresentedAuthorityOwner { identity } => {
            SiblingParentage::Unrepresented(identity)
        }
        ParentageState::Unavailable => SiblingParentage::Unavailable,
    }
}

/// Direct, coordinate-free material captured when a fact is admitted.  This
/// is a payload basis, not a declaration stable-key input.  Local targets are
/// framed but resolved by [`payload_for`] so a payload captures semantic type
/// and member edits without making those edits identity inputs.
pub(super) fn fact_payload_basis(fact: &SemanticFact<'_>) -> PayloadHash {
    fn bytes(out: &mut Vec<u8>, value: &[u8]) {
        out.extend_from_slice(&(value.len() as u64).to_le_bytes());
        out.extend_from_slice(value);
    }
    fn optional_bytes(out: &mut Vec<u8>, value: Option<&[u8]>) {
        match value {
            Some(value) => {
                out.push(1);
                bytes(out, value);
            }
            None => out.push(0),
        }
    }
    fn record(out: &mut Vec<u8>, value: SemanticTypeRecord<'_>) {
        out.push(u8::from(value.tag));
        out.extend_from_slice(&value.payload0.to_le_bytes());
        out.extend_from_slice(&value.payload1.to_le_bytes());
        optional_bytes(out, value.text);
        optional_bytes(out, value.text2);
        match value.nominal {
            None => out.push(0),
            Some(NominalRef::Local(_)) => out.push(1),
            Some(NominalRef::External(target)) => {
                out.push(2);
                out.extend_from_slice(target.fragment.as_ref());
                out.extend_from_slice(&target.ordinal.to_le_bytes());
            }
        }
    }

    let mut preimage = Vec::with_capacity(
        64 + fact.name.len()
            + fact.type_children[..usize::from(fact.type_child_count)]
                .iter()
                .map(|child| child.name.map_or(0, <[u8]>::len))
                .sum::<usize>(),
    );
    preimage.extend_from_slice(b"compiler.declaration-payload-basis.v1");
    preimage.extend_from_slice(&u16::from(fact.kind).to_le_bytes());
    bytes(&mut preimage, fact.name);
    preimage.push(u32::from(fact.constructor.tag) as u8);
    preimage.extend_from_slice(&fact.constructor.payload0.to_le_bytes());
    preimage.extend_from_slice(&fact.constructor.payload1.to_le_bytes());
    record(&mut preimage, fact.type_record);
    preimage.push(fact.type_child_count);
    for child in fact.type_children.iter().take(usize::from(fact.type_child_count)) {
        optional_bytes(&mut preimage, child.name);
        preimage.push(child.flags);
    }
    preimage.push(fact.child_count);
    for child in fact.children.iter().take(usize::from(fact.child_count)) {
        preimage.push(u8::from(child.role));
    }
    PayloadHash::from_canonical_bytes(&preimage)
}

/// Minimal structural shape used only to distinguish a proved same-scope
/// sibling collision.  Unlike a payload, it deliberately excludes member
/// topology, source/provenance, docs, visibility, extensions, and occurrence
/// planes, so editing those facts never remints a collided declaration.
fn collision_shape_for(
    facts: &FactSet<'_>,
    ordinal: usize,
    shapes: &mut [Option<PayloadHash>],
    type_shapes: &mut [Option<PayloadHash>],
    type_visiting: &mut [bool],
) -> Result<PayloadHash, compiler_ir::BuildError> {
    let Some(slot) = shapes.get(ordinal) else {
        return Err(dangling_entity(ordinal));
    };
    if let Some(shape) = *slot {
        return Ok(shape);
    }
    let basis = *facts
        .key_digests
        .get(ordinal)
        .ok_or_else(|| dangling_entity(ordinal))?;
    let mut preimage = Vec::with_capacity(48 + MAX_CHILD_SHAPE_BYTES);
    preimage.extend_from_slice(b"compiler.declaration-collision-shape.v1");
    preimage.extend_from_slice(basis.as_ref());
    let child_start = facts.child_starts[ordinal] as usize;
    let child_count = usize::from(facts.child_counts[ordinal]);
    preimage.extend_from_slice(&(child_count as u64).to_le_bytes());
    for child in &facts.child_targets[child_start..child_start + child_count] {
        preimage.extend_from_slice(
            facts
                .key_digests
                .get(*child as usize)
                .ok_or_else(|| dangling_entity(*child as usize))?
                .as_ref(),
        );
    }
    let type_child_start = facts.type_child_starts[ordinal] as usize;
    let type_child_count = usize::from(facts.type_child_counts[ordinal]);
    preimage.extend_from_slice(&(type_child_count as u64).to_le_bytes());
    for ((target, name), flags) in facts.type_child_targets
        [type_child_start..type_child_start + type_child_count]
        .iter()
        .zip(&facts.type_child_names[type_child_start..type_child_start + type_child_count])
        .zip(&facts.type_child_flags[type_child_start..type_child_start + type_child_count])
    {
        append_type_target_payload(
            &mut preimage,
            facts,
            *target,
            *name,
            *flags,
            type_shapes,
            type_visiting,
        )?;
    }
    if let Some(nominal) = facts.type_records[ordinal].nominal {
        append_nominal_payload(
            &mut preimage,
            facts,
            nominal,
            type_shapes,
            type_visiting,
        )?;
    }
    let shape = PayloadHash::from_canonical_bytes(&preimage);
    shapes[ordinal] = Some(shape);
    Ok(shape)
}

/// Full payload hash for one declaration.  It includes its direct basis,
/// product-child/type target shapes, and the once-grouped local-member basis
/// set. Documentation, visibility, extensions, source spans, opaque
/// parentage, and occurrences remain outside the payload until those planes
/// have a durable payload contract.
fn payload_for(
    facts: &FactSet<'_>,
    ordinal: usize,
    member_payloads: &[PayloadHash],
    payloads: &mut [Option<PayloadHash>],
    type_shapes: &mut [Option<PayloadHash>],
    type_visiting: &mut [bool],
) -> Result<PayloadHash, compiler_ir::BuildError> {
    let Some(slot) = payloads.get(ordinal) else {
        return Err(dangling_entity(ordinal));
    };
    if let Some(payload) = *slot {
        return Ok(payload);
    }
    let basis = *facts
        .key_digests
        .get(ordinal)
        .ok_or_else(|| dangling_entity(ordinal))?;
    let mut preimage = Vec::with_capacity(48 + (MAX_CHILD_SHAPE_BYTES * 2));
    preimage.extend_from_slice(b"compiler.declaration-payload.v3");
    preimage.extend_from_slice(basis.as_ref());
    preimage.extend_from_slice(
        member_payloads
            .get(ordinal)
            .ok_or_else(|| dangling_entity(ordinal))?
            .as_ref(),
    );
    let child_start = facts.child_starts[ordinal] as usize;
    let child_count = usize::from(facts.child_counts[ordinal]);
    preimage.extend_from_slice(&(child_count as u64).to_le_bytes());
    for child in &facts.child_targets[child_start..child_start + child_count] {
        preimage.extend_from_slice(
            facts
                .key_digests
                .get(*child as usize)
                .ok_or_else(|| dangling_entity(*child as usize))?
                .as_ref(),
        );
    }
    let type_child_start = facts.type_child_starts[ordinal] as usize;
    let type_child_count = usize::from(facts.type_child_counts[ordinal]);
    preimage.extend_from_slice(&(type_child_count as u64).to_le_bytes());
    for ((target, name), flags) in facts.type_child_targets
        [type_child_start..type_child_start + type_child_count]
        .iter()
        .zip(&facts.type_child_names[type_child_start..type_child_start + type_child_count])
        .zip(&facts.type_child_flags[type_child_start..type_child_start + type_child_count])
    {
        append_type_target_payload(
            &mut preimage,
            facts,
            *target,
            *name,
            *flags,
            type_shapes,
            type_visiting,
        )?;
    }
    if let Some(nominal) = facts.type_records[ordinal].nominal {
        append_nominal_payload(
            &mut preimage,
            facts,
            nominal,
            type_shapes,
            type_visiting,
        )?;
    }
    let payload = PayloadHash::from_canonical_bytes(&preimage);
    payloads[ordinal] = Some(payload);
    Ok(payload)
}

const MAX_CHILD_SHAPE_BYTES: usize = 64 * 16;

fn append_nominal_payload(
    preimage: &mut Vec<u8>,
    facts: &FactSet<'_>,
    nominal: NominalRef,
    type_shapes: &mut [Option<PayloadHash>],
    type_visiting: &mut [bool],
) -> Result<(), compiler_ir::BuildError> {
    match nominal {
        NominalRef::Local(target) => {
            preimage.push(0);
            append_type_target_payload(
                preimage,
                facts,
                target.raw,
                None,
                0,
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
    }
}

fn append_type_target_payload(
    preimage: &mut Vec<u8>,
    facts: &FactSet<'_>,
    target: u32,
    name: Option<&[u8]>,
    flags: u8,
    type_shapes: &mut [Option<PayloadHash>],
    type_visiting: &mut [bool],
) -> Result<(), compiler_ir::BuildError> {
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
        preimage.extend_from_slice(
            facts
                .key_digests
                .get(target as usize)
                .ok_or_else(|| dangling_entity(target as usize))?
                .as_ref(),
        );
        return Ok(());
    }
    preimage.push(1);
    preimage.extend_from_slice(
        type_shape(facts, target, type_shapes, type_visiting)?.as_ref(),
    );
    Ok(())
}

/// Coordinate-free shape for an anonymous/computed type.  This is structural
/// only: ownership rows and staged offsets never enter the payload or an
/// overload signature.  A compound cycle gets one fixed marker rather than
/// a recursion-dependent coordinate.
fn type_shape(
    facts: &FactSet<'_>,
    row: u32,
    cache: &mut [Option<PayloadHash>],
    visiting: &mut [bool],
) -> Result<PayloadHash, compiler_ir::BuildError> {
    let slot = staged_type_slot(facts, row).ok_or_else(|| compiler_ir::BuildError::Dangling {
        space: compiler_ir::SemanticSpace::Type,
        raw: row,
    })?;
    if let Some(shape) = cache[slot] {
        return Ok(shape);
    }
    if visiting[slot] {
        return Ok(PayloadHash::from_canonical_bytes(b"compiler.type-cycle.v2"));
    }
    visiting[slot] = true;
    let (record, targets, names, flags) = staged_type_row(facts, row)?;
    let mut preimage = Vec::with_capacity(64 + targets.len() * 24);
    preimage.extend_from_slice(b"compiler.staged-type-payload.v2");
    append_record_payload(&mut preimage, facts, record, cache, visiting)?;
    preimage.extend_from_slice(&(targets.len() as u64).to_le_bytes());
    for ((target, name), flags) in targets.iter().zip(names).zip(flags) {
        append_type_target_payload(&mut preimage, facts, *target, *name, *flags, cache, visiting)?;
    }
    let shape = PayloadHash::from_canonical_bytes(&preimage);
    visiting[slot] = false;
    cache[slot] = Some(shape);
    Ok(shape)
}

fn append_record_payload(
    preimage: &mut Vec<u8>,
    facts: &FactSet<'_>,
    record: SemanticTypeRecord<'_>,
    type_shapes: &mut [Option<PayloadHash>],
    type_visiting: &mut [bool],
) -> Result<(), compiler_ir::BuildError> {
    preimage.push(u8::from(record.tag));
    preimage.extend_from_slice(&record.payload0.to_le_bytes());
    preimage.extend_from_slice(&record.payload1.to_le_bytes());
    for text in [record.text, record.text2] {
        match text {
            Some(text) => {
                preimage.push(1);
                preimage.extend_from_slice(&(text.len() as u64).to_le_bytes());
                preimage.extend_from_slice(text);
            }
            None => preimage.push(0),
        }
    }
    match record.nominal {
        None => preimage.push(0),
        Some(nominal) => {
            preimage.push(1);
            append_nominal_payload(preimage, facts, nominal, type_shapes, type_visiting)?;
        }
    }
    Ok(())
}

fn staged_type_slot(facts: &FactSet<'_>, row: u32) -> Option<usize> {
    if row < ANONYMOUS_ROW_BASE {
        return (row as usize < facts.len).then_some(row as usize);
    }
    if row >= COMPUTED_ROW_BASE {
        let computed = row - COMPUTED_ROW_BASE;
        return (computed as usize < facts.computed_rows)
            .then_some(facts.len + facts.anonymous_rows + computed as usize);
    }
    let anonymous = row - ANONYMOUS_ROW_BASE;
    (anonymous as usize < facts.anonymous_rows).then_some(facts.len + anonymous as usize)
}

fn staged_type_row<'facts, 'source>(
    facts: &'facts FactSet<'source>,
    row: u32,
) -> Result<
    (
        SemanticTypeRecord<'source>,
        &'facts [u32],
        &'facts [Option<&'source [u8]>],
        &'facts [u8],
    ),
    compiler_ir::BuildError,
> {
    if row < ANONYMOUS_ROW_BASE {
        let ordinal = row as usize;
        let start = *facts
            .type_child_starts
            .get(ordinal)
            .ok_or_else(|| dangling_type(row))? as usize;
        let count = usize::from(*facts.type_child_counts.get(ordinal).ok_or_else(|| dangling_type(row))?);
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

fn dangling_entity(ordinal: usize) -> compiler_ir::BuildError {
    compiler_ir::BuildError::Dangling {
        space: compiler_ir::SemanticSpace::Entity,
        raw: u32::try_from(ordinal).unwrap_or(u32::MAX),
    }
}

fn dangling_type(row: u32) -> compiler_ir::BuildError {
    compiler_ir::BuildError::Dangling {
        space: compiler_ir::SemanticSpace::Type,
        raw: row,
    }
}
