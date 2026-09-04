//! Coordinate-free declaration identity and payload materialization.
//!
//! Stable declaration identity is deliberately smaller than semantic payload:
//! a declaration's scope, language profile, and closed parentage state are
//! always present, while every row also has a coordinate-free structural
//! variant. Type and member content remain in the payload plane, so ordinary
//! edits do not remint declaration families.

use compiler_ir::{
    CorePayloadHash, DeclarationFamilyId, DeclarationIdentity, DeclarationKey,
    DeclarationParentage, EntityId, EntityKind, EntityVersion, NominalRef,
    ScopedDeclarationKey, SemanticTypeRecord, VariantFingerprint,
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
    let signature_seeds = declaration_signature_seeds(facts)?;
    let locators = lexical_locators(facts, &signature_seeds)?;
    let variants = variant_fingerprints(facts, &locators)?;
    let member_payloads = member_payloads(facts, &locators, &variants)?;
    let identities = declaration_identities(facts, scope, profile, &variants)?;
    let mut payloads = vec![None; count].into_boxed_slice();
    let mut type_shapes = vec![None; facts.len + facts.anonymous_rows + facts.computed_rows]
        .into_boxed_slice();
    let mut type_visiting = vec![false; type_shapes.len()].into_boxed_slice();
    (0..count).map(|ordinal| {
        Ok(EntityVersion {
            family: identities.get(ordinal).ok_or_else(|| dangling_entity(ordinal))?.family,
            variant: *variants.get(ordinal).ok_or_else(|| dangling_entity(ordinal))?,
            core_payload: payload_for(
                facts, ordinal, &locators, &member_payloads, &mut payloads,
                &mut type_shapes, &mut type_visiting,
            )?,
        })
    }).collect::<Result<Vec<_>, compiler_ir::BuildError>>().map(Vec::into_boxed_slice)
}

/// Mints every family with an explicit post-order parent stack. Bound rows
/// contain the already-minted exact parent instance; no family calculation
/// can recurse through 16k source declarations on the process stack.
fn declaration_identities(
    facts: &FactSet<'_>,
    scope: DeclarationScope<'_>,
    profile: LanguageProfile,
    variants: &[VariantFingerprint],
) -> Result<Box<[DeclarationIdentity]>, compiler_ir::BuildError> {
    let mut values = vec![None; facts.len].into_boxed_slice();
    let mut state = vec![0_u8; facts.len].into_boxed_slice();
    for root in 0..facts.len {
        if state[root] == 2 { continue; }
        let mut stack = Vec::new();
        stack.push((root, false));
        while let Some((ordinal, exit)) = stack.pop() {
            if exit {
                let raw_parentage = *facts.provenance.parentage().get(ordinal)
                    .ok_or_else(|| dangling_entity(ordinal))?;
                let parentage = match raw_parentage {
                    ParentageState::Root => DeclarationParentage::Root,
                    ParentageState::Unavailable => DeclarationParentage::Unavailable,
                    ParentageState::UnrepresentedAuthorityOwner { identity } => DeclarationParentage::Unrepresented(identity),
                    ParentageState::Bound { parent } => {
                        let parent = usize::try_from(parent.raw).map_err(|_| dangling_entity(ordinal))?;
                        DeclarationParentage::Bound(values.get(parent).and_then(|value| *value)
                            .ok_or_else(|| dangling_entity(parent))?)
                    }
                };
                let key = DeclarationKey::new(scope.lineage(), scope.path(), facts.kinds[ordinal], facts.names[ordinal])
                    .map_err(|cause| compiler_ir::BuildError::DeclarationKey {
                        entity: EntityId::new(u32::try_from(ordinal).unwrap_or(u32::MAX)), cause,
                    })?;
                let scoped = ScopedDeclarationKey::new(key, profile, parentage);
                let length = scoped.family_preimage_len().map_err(|cause| compiler_ir::BuildError::ScopedDeclarationPreimage {
                    entity: EntityId::new(u32::try_from(ordinal).unwrap_or(u32::MAX)), cause,
                })?;
                let mut preimage = vec![0_u8; length];
                let family = scoped.family_id(&mut preimage).map_err(|cause| compiler_ir::BuildError::ScopedDeclarationPreimage {
                    entity: EntityId::new(u32::try_from(ordinal).unwrap_or(u32::MAX)), cause,
                })?;
                let mut bytes = [0_u8; 16];
                bytes.copy_from_slice(&family.as_ref()[..16]);
                values[ordinal] = Some(DeclarationIdentity {
                    family: DeclarationFamilyId::from_raw(bytes),
                    variant: *variants.get(ordinal).ok_or_else(|| dangling_entity(ordinal))?,
                });
                state[ordinal] = 2;
                continue;
            }
            match state[ordinal] {
                2 => continue,
                1 => return Err(compiler_ir::BuildError::ParentCycle {
                    entity: EntityId::new(u32::try_from(ordinal).unwrap_or(u32::MAX)),
                }),
                _ => {}
            }
            state[ordinal] = 1;
            stack.push((ordinal, true));
            if let ParentageState::Bound { parent } = *facts.provenance.parentage().get(ordinal)
                .ok_or_else(|| dangling_entity(ordinal))? {
                let parent = usize::try_from(parent.raw).map_err(|_| dangling_entity(ordinal))?;
                if parent >= facts.len { return Err(dangling_entity(parent)); }
                if state[parent] != 2 { stack.push((parent, false)); }
            }
        }
    }
    values.into_iter().collect::<Option<Vec<_>>>().map(Vec::into_boxed_slice)
        .ok_or_else(|| dangling_entity(facts.len))
}

/// Builds ordered local-member payloads with their semantic roles. A sorted
/// multiset would erase reordering and role edits;
/// child direct bases retain their own declaration/type semantics without
/// making a parent's payload part of a child's identity.
fn member_payloads(
    facts: &FactSet<'_>,
    locators: &[CorePayloadHash],
    variants: &[VariantFingerprint],
) -> Result<Box<[CorePayloadHash]>, compiler_ir::BuildError> {
    let empty = CorePayloadHash::from_canonical_bytes(b"compiler.local-members.v2\0");
    let mut summaries = vec![empty; facts.len].into_boxed_slice();
    for parent in 0..facts.len {
        let start = usize::try_from(facts.child_starts[parent]).map_err(|_| dangling_entity(parent))?;
        let count = usize::from(facts.child_counts[parent]);
        let end = start.checked_add(count).ok_or_else(|| dangling_entity(parent))?;
        let targets = facts.child_targets.get(start..end).ok_or_else(|| dangling_entity(parent))?;
        let roles = facts.child_roles.get(start..end).ok_or_else(|| dangling_entity(parent))?;
        let count = u32::try_from(targets.len()).map_err(|_| dangling_entity(parent))?;
        let mut preimage = Vec::new();
        preimage.extend_from_slice(b"compiler.local-members.v2");
        preimage.extend_from_slice(&count.to_le_bytes());
        for (target, role) in targets.iter().zip(roles) {
            let child = usize::try_from(*target).map_err(|_| dangling_entity(parent))?;
            let ParentageState::Bound { parent: bound } = *facts.provenance.parentage().get(child)
                .ok_or_else(|| dangling_entity(child))? else {
                return Err(dangling_entity(child));
            };
            if bound.raw as usize != parent {
                return Err(dangling_entity(child));
            }
            preimage.push(u8::from(*role));
            preimage.extend_from_slice(
                locators.get(child).ok_or_else(|| dangling_entity(child))?.as_ref(),
            );
            preimage.extend_from_slice(
                variants.get(child).ok_or_else(|| dangling_entity(child))?.as_bytes(),
            );
        }
        summaries[parent] = CorePayloadHash::from_canonical_bytes(&preimage);
    }
    Ok(summaries)
}

/// Computes a coordinate-free structural variant for every admitted row.
/// Families never receive a skeleton: the variant is an exact local-instance
/// discriminator for both singleton and overloaded declaration families.
fn variant_fingerprints(
    facts: &FactSet<'_>,
    locators: &[CorePayloadHash],
) -> Result<Box<[VariantFingerprint]>, compiler_ir::BuildError> {
    let mut shapes = vec![None; facts.len].into_boxed_slice();
    let mut type_shapes = vec![None; facts.len + facts.anonymous_rows + facts.computed_rows]
        .into_boxed_slice();
    let mut type_visiting = vec![false; type_shapes.len()].into_boxed_slice();
    (0..facts.len)
        .map(|ordinal| {
            let shape = collision_shape_for(
                facts, ordinal, locators, &mut shapes, &mut type_shapes, &mut type_visiting,
            )?;
            Ok(VariantFingerprint::from_raw(*shape.as_bytes()))
        })
        .collect::<Result<Vec<_>, compiler_ir::BuildError>>()
        .map(Vec::into_boxed_slice)
}

/// Builds lexical declaration locators with an explicit enter/exit stack.
/// Raw parent coordinates guide only traversal; emitted bytes contain the
/// closed parent state and already-minted locator, never that coordinate.
fn lexical_locators(
    facts: &FactSet<'_>,
    signature_seeds: &[CorePayloadHash],
) -> Result<Box<[CorePayloadHash]>, compiler_ir::BuildError> {
    let mut values = vec![None; facts.len].into_boxed_slice();
    let mut state = vec![0_u8; facts.len].into_boxed_slice();
    for root in 0..facts.len {
        if state[root] == 2 { continue; }
        let mut stack = Vec::new();
        stack.push((root, false));
        while let Some((ordinal, exit)) = stack.pop() {
            if exit {
                let parentage = *facts.provenance.parentage().get(ordinal)
                    .ok_or_else(|| dangling_entity(ordinal))?;
                let mut bytes = Vec::with_capacity(64 + facts.names[ordinal].len());
                bytes.extend_from_slice(b"compiler.declaration-lexical-locator.v1");
                match parentage {
                    ParentageState::Root => bytes.push(0),
                    ParentageState::Bound { parent } => {
                        bytes.push(1);
                        let parent = usize::try_from(parent.raw).map_err(|_| dangling_entity(ordinal))?;
                        bytes.extend_from_slice(values.get(parent).and_then(|value| *value)
                            .ok_or_else(|| dangling_entity(parent))?.as_ref());
                        bytes.extend_from_slice(signature_seeds.get(parent)
                            .ok_or_else(|| dangling_entity(parent))?.as_ref());
                    }
                    ParentageState::UnrepresentedAuthorityOwner { identity } => {
                        bytes.push(2); bytes.extend_from_slice(&identity);
                    }
                    ParentageState::Unavailable => bytes.push(3),
                }
                bytes.extend_from_slice(&u16::from(facts.kinds[ordinal]).to_le_bytes());
                let length = u32::try_from(facts.names[ordinal].len()).map_err(|_| dangling_entity(ordinal))?;
                bytes.extend_from_slice(&length.to_le_bytes());
                bytes.extend_from_slice(facts.names[ordinal]);
                values[ordinal] = Some(CorePayloadHash::from_canonical_bytes(&bytes));
                state[ordinal] = 2;
                continue;
            }
            match state[ordinal] {
                2 => continue,
                1 => return Err(compiler_ir::BuildError::ParentCycle { entity: EntityId::new(u32::try_from(ordinal).unwrap_or(u32::MAX)) }),
                _ => {}
            }
            state[ordinal] = 1;
            stack.push((ordinal, true));
            if let ParentageState::Bound { parent } = *facts.provenance.parentage().get(ordinal)
                .ok_or_else(|| dangling_entity(ordinal))? {
                let parent = usize::try_from(parent.raw).map_err(|_| dangling_entity(ordinal))?;
                if parent >= facts.len { return Err(dangling_entity(parent)); }
                if state[parent] != 2 { stack.push((parent, false)); }
            }
        }
    }
    values.into_iter().collect::<Option<Vec<_>>>().map(Vec::into_boxed_slice)
        .ok_or_else(|| dangling_entity(facts.len))
}

/// A member-insensitive declaration signature seed for lexical nesting. It
/// owns only direct kind/name/type scalar evidence; ordered local members
/// remain exclusively in the core payload plane.
fn declaration_signature_seeds(
    facts: &FactSet<'_>,
) -> Result<Box<[CorePayloadHash]>, compiler_ir::BuildError> {
    (0..facts.len).map(|ordinal| {
        let record = facts.type_records[ordinal];
        let mut bytes = Vec::with_capacity(48 + facts.names[ordinal].len());
        bytes.extend_from_slice(b"compiler.declaration-signature-seed.v1");
        bytes.extend_from_slice(&u16::from(facts.kinds[ordinal]).to_le_bytes());
        bytes.extend_from_slice(&u32::try_from(facts.names[ordinal].len())
            .map_err(|_| dangling_entity(ordinal))?.to_le_bytes());
        bytes.extend_from_slice(facts.names[ordinal]);
        bytes.push(u8::from(record.tag));
        append_tag_owned_scalars(&mut bytes, record);
        let start = usize::try_from(facts.type_child_starts[ordinal])
            .map_err(|_| dangling_entity(ordinal))?;
        let count = usize::from(facts.type_child_counts[ordinal]);
        let end = start.checked_add(count).ok_or_else(|| dangling_entity(ordinal))?;
        let targets = facts.type_child_targets.get(start..end).ok_or_else(|| dangling_entity(ordinal))?;
        let names = facts.type_child_names.get(start..end).ok_or_else(|| dangling_entity(ordinal))?;
        let flags = facts.type_child_flags.get(start..end).ok_or_else(|| dangling_entity(ordinal))?;
        bytes.extend_from_slice(&u32::try_from(targets.len()).map_err(|_| dangling_entity(ordinal))?.to_le_bytes());
        for ((target, name), flags) in targets.iter().zip(names).zip(flags) {
            match name { Some(name) => { bytes.push(1); bytes.extend_from_slice(&u32::try_from(name.len()).map_err(|_| dangling_entity(ordinal))?.to_le_bytes()); bytes.extend_from_slice(name); }, None => bytes.push(0) }
            bytes.push(*flags);
            append_direct_target_signature(&mut bytes, facts, *target)?;
        }
        Ok(CorePayloadHash::from_canonical_bytes(&bytes))
    }).collect::<Result<Vec<_>, compiler_ir::BuildError>>().map(Vec::into_boxed_slice)
}

fn append_direct_target_signature(
    out: &mut Vec<u8>,
    facts: &FactSet<'_>,
    target: u32,
) -> Result<(), compiler_ir::BuildError> {
    if target == STAGED_TEXT_CHILD { out.push(2); return Ok(()); }
    if target < facts.len as u32 {
        let ordinal = usize::try_from(target).map_err(|_| dangling_entity(0))?;
        let record = *facts.type_records.get(ordinal).ok_or_else(|| dangling_entity(ordinal))?;
        out.push(0);
        out.extend_from_slice(&u16::from(facts.kinds[ordinal]).to_le_bytes());
        out.extend_from_slice(&u32::try_from(facts.names[ordinal].len()).map_err(|_| dangling_entity(ordinal))?.to_le_bytes());
        out.extend_from_slice(facts.names[ordinal]);
        out.push(u8::from(record.tag));
        append_tag_owned_scalars(out, record);
        return Ok(());
    }
    let (record, _, _, _) = staged_type_row(facts, target)?;
    out.push(1);
    out.push(u8::from(record.tag));
    append_tag_owned_scalars(out, record);
    Ok(())
}

/// Direct, coordinate-free material captured when a fact is admitted.  This
/// is a payload basis, not a declaration stable-key input.  Local targets are
/// framed but resolved by [`payload_for`] so a payload captures semantic type
/// and member edits without making those edits identity inputs.
pub(super) fn fact_payload_basis(fact: &SemanticFact<'_>) -> CorePayloadHash {
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
        append_tag_owned_scalars(out, value);
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
    CorePayloadHash::from_canonical_bytes(&preimage)
}

/// Minimal structural shape used only to distinguish a proved same-scope
/// sibling collision.  Unlike a payload, it deliberately excludes member
/// topology, source/provenance, docs, visibility, extensions, and occurrence
/// planes, so editing those facts never remints a collided declaration.
fn collision_shape_for(
    facts: &FactSet<'_>,
    ordinal: usize,
    locators: &[CorePayloadHash],
    shapes: &mut [Option<CorePayloadHash>],
    type_shapes: &mut [Option<CorePayloadHash>],
    type_visiting: &mut [bool],
) -> Result<CorePayloadHash, compiler_ir::BuildError> {
    let Some(slot) = shapes.get(ordinal) else {
        return Err(dangling_entity(ordinal));
    };
    if let Some(shape) = *slot {
        return Ok(shape);
    }
    let mut preimage = Vec::with_capacity(48 + MAX_CHILD_SHAPE_BYTES);
    preimage.extend_from_slice(b"compiler.declaration-variant.v2");
    preimage.extend_from_slice(&u16::from(facts.kinds[ordinal]).to_le_bytes());
    let name_len = u32::try_from(facts.names[ordinal].len()).map_err(|_| dangling_entity(ordinal))?;
    preimage.extend_from_slice(&name_len.to_le_bytes());
    preimage.extend_from_slice(facts.names[ordinal]);
    let record = facts.type_records[ordinal];
    preimage.push(u8::from(record.tag));
    append_tag_owned_scalars(&mut preimage, record);
    for text in [record.text, record.text2] {
        match text {
            Some(text) => {
                preimage.push(1);
                preimage.extend_from_slice(&u32::try_from(text.len()).map_err(|_| dangling_entity(ordinal))?.to_le_bytes());
                preimage.extend_from_slice(text);
            }
            None => preimage.push(0),
        }
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
            locators,
            type_shapes,
            type_visiting,
        )?;
    }
    if let Some(nominal) = facts.type_records[ordinal].nominal {
        append_nominal_payload(
            &mut preimage,
            facts,
            nominal,
            locators,
            type_shapes,
            type_visiting,
        )?;
    }
    let shape = CorePayloadHash::from_canonical_bytes(&preimage);
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
    locators: &[CorePayloadHash],
    member_payloads: &[CorePayloadHash],
    payloads: &mut [Option<CorePayloadHash>],
    type_shapes: &mut [Option<CorePayloadHash>],
    type_visiting: &mut [bool],
) -> Result<CorePayloadHash, compiler_ir::BuildError> {
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
            locators,
            type_shapes,
            type_visiting,
        )?;
    }
    if let Some(nominal) = facts.type_records[ordinal].nominal {
        append_nominal_payload(
            &mut preimage,
            facts,
            nominal,
            locators,
            type_shapes,
            type_visiting,
        )?;
    }
    let payload = CorePayloadHash::from_canonical_bytes(&preimage);
    payloads[ordinal] = Some(payload);
    Ok(payload)
}

const MAX_CHILD_SHAPE_BYTES: usize = 64 * 16;

fn append_nominal_payload(
    preimage: &mut Vec<u8>,
    facts: &FactSet<'_>,
    nominal: NominalRef,
    locators: &[CorePayloadHash],
    type_shapes: &mut [Option<CorePayloadHash>],
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
    }
}

fn append_type_target_payload(
    preimage: &mut Vec<u8>,
    facts: &FactSet<'_>,
    target: u32,
    name: Option<&[u8]>,
    flags: u8,
    locators: &[CorePayloadHash],
    type_shapes: &mut [Option<CorePayloadHash>],
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
        let locator = locators.get(target as usize)
            .ok_or_else(|| dangling_entity(target as usize))?;
        preimage.extend_from_slice(locator.as_ref());
        return Ok(());
    }
    preimage.push(1);
    preimage.extend_from_slice(
        type_shape(facts, target, locators, type_shapes, type_visiting)?.as_ref(),
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
) -> Result<CorePayloadHash, compiler_ir::BuildError> {
    let slot = staged_type_slot(facts, row).ok_or_else(|| compiler_ir::BuildError::Dangling {
        space: compiler_ir::SemanticSpace::Type,
        raw: row,
    })?;
    if let Some(shape) = cache[slot] {
        return Ok(shape);
    }
    let mut stack = Vec::new();
    stack.push((row, false));
    while let Some((current, exit)) = stack.pop() {
        let current_slot = staged_type_slot(facts, current).ok_or_else(|| dangling_type(current))?;
        if cache[current_slot].is_some() { continue; }
        if !exit {
            if visiting[current_slot] { continue; }
            visiting[current_slot] = true;
            stack.push((current, true));
            let (_, targets, _, _) = staged_type_row(facts, current)?;
            for target in targets.iter().rev().copied() {
                if target >= facts.len as u32 && target != STAGED_TEXT_CHILD {
                    let nested = staged_type_slot(facts, target).ok_or_else(|| dangling_type(target))?;
                    if visiting[nested] {
                        return Err(compiler_ir::BuildError::RecursiveType { raw: target });
                    }
                    if cache[nested].is_none() { stack.push((target, false)); }
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
                Some(text) => { preimage.push(1); preimage.extend_from_slice(&u32::try_from(text.len()).map_err(|_| dangling_type(current))?.to_le_bytes()); preimage.extend_from_slice(text); }
                None => preimage.push(0),
            }
        }
        match record.nominal {
            None => preimage.push(0),
            Some(NominalRef::Local(local)) => { preimage.push(1); preimage.push(0); preimage.extend_from_slice(locators.get(local.raw as usize).ok_or_else(|| dangling_entity(local.raw as usize))?.as_ref()); }
            Some(NominalRef::External(external)) => { preimage.push(1); preimage.push(1); preimage.extend_from_slice(external.fragment.as_ref()); preimage.extend_from_slice(&external.ordinal.to_le_bytes()); }
        }
        preimage.extend_from_slice(&u32::try_from(targets.len()).map_err(|_| dangling_type(current))?.to_le_bytes());
        for ((target, name), flag) in targets.iter().zip(names).zip(flags) {
            match name { Some(name) => { preimage.push(1); preimage.extend_from_slice(&u32::try_from(name.len()).map_err(|_| dangling_type(current))?.to_le_bytes()); preimage.extend_from_slice(name); }, None => preimage.push(0) }
            preimage.push(*flag);
            if *target == STAGED_TEXT_CHILD { preimage.push(2); continue; }
            if *target < facts.len as u32 { preimage.push(0); preimage.extend_from_slice(locators.get(*target as usize).ok_or_else(|| dangling_entity(*target as usize))?.as_ref()); continue; }
            preimage.push(1);
            let nested = staged_type_slot(facts, *target).ok_or_else(|| dangling_type(*target))?;
            preimage.extend_from_slice(cache[nested]
                .ok_or(compiler_ir::BuildError::RecursiveType { raw: *target })?.as_ref());
        }
        cache[current_slot] = Some(CorePayloadHash::from_canonical_bytes(&preimage));
        visiting[current_slot] = false;
    }
    cache[slot].ok_or_else(|| dangling_type(row))
}

/// Only tag-owned scalar cells may enter an identity preimage directly.
/// Structural target/list coordinates are represented by ordered child or
/// nominal frames below; they are never hashed as staging coordinates.
fn append_tag_owned_scalars(out: &mut Vec<u8>, record: SemanticTypeRecord<'_>) {
    use compiler_ir::SemanticTypeTag as Tag;
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
