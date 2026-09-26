//! Semantic package diff over declarations and graph links.

use std::collections::BTreeMap;

use super::{
    BuiltinModelError, SemanticDeclaration, SemanticLinkSummary, SemanticPackageSnapshot,
    push_diff, semantic_declaration_identity, semantic_link_kind, semantic_link_target,
};

pub(super) fn diff_semantic_snapshots(
    before: &SemanticPackageSnapshot<'_>,
    after: &SemanticPackageSnapshot<'_>,
) -> Result<Box<[backend_engine::DiffRecord]>, BuiltinModelError> {
    let mut rows = Vec::new();
    let mut before_at = 0;
    let mut after_at = 0;
    while before_at < before.declarations.len() || after_at < after.declarations.len() {
        let family = match (
            before.declarations.get(before_at),
            after.declarations.get(after_at),
        ) {
            (Some((left, _)), Some((right, _))) => left.family.min(right.family),
            (Some((left, _)), None) => left.family,
            (None, Some((right, _))) => right.family,
            (None, None) => break,
        };
        let before_end = family_end(&before.declarations, before_at, family);
        let after_end = family_end(&after.declarations, after_at, family);
        diff_semantic_family(
            &before.declarations[before_at..before_end],
            &after.declarations[after_at..after_end],
            &mut rows,
        )?;
        before_at = before_end;
        after_at = after_end;
    }
    diff_semantic_links(before, after, &mut rows)?;
    rows.sort_by(|left, right| {
        left.label.cmp(&right.label).then_with(|| {
            declaration_change_order(left.change).cmp(&declaration_change_order(right.change))
        })
    });
    Ok(rows.into_boxed_slice())
}

fn family_end(
    declarations: &[(
        backend_semantic::ir::DeclarationIdentity,
        SemanticDeclaration<'_>,
    )],
    start: usize,
    family: backend_semantic::ir::DeclarationFamilyId,
) -> usize {
    start + declarations[start..].partition_point(|(identity, _)| identity.family == family)
}

fn diff_semantic_family(
    before: &[(
        backend_semantic::ir::DeclarationIdentity,
        SemanticDeclaration<'_>,
    )],
    after: &[(
        backend_semantic::ir::DeclarationIdentity,
        SemanticDeclaration<'_>,
    )],
    rows: &mut Vec<backend_engine::DiffRecord>,
) -> Result<(), BuiltinModelError> {
    if let ([(before_id, before)], [(after_id, after)]) = (before, after) {
        if before_id != after_id
            || before.version.core_payload != after.version.core_payload
            || before.parent != after.parent
        {
            push_diff(
                rows,
                after.label,
                backend_engine::DeclarationChange::Changed,
                Some(*before_id),
                Some(*after_id),
            )?;
        }
        return Ok(());
    }

    let (_, added) = unmatched_counts(before, after);
    let mut left = 0;
    let mut right = 0;
    while left < before.len() || right < after.len() {
        match (before.get(left), after.get(right)) {
            (Some((before_id, before)), Some((after_id, after))) => match before_id.cmp(after_id) {
                std::cmp::Ordering::Less => {
                    let change = if added == 0 {
                        backend_engine::DeclarationChange::Removed
                    } else {
                        backend_engine::DeclarationChange::Indeterminate
                    };
                    push_diff(rows, before.label, change, Some(*before_id), None)?;
                    left += 1;
                }
                std::cmp::Ordering::Greater => {
                    push_diff(
                        rows,
                        after.label,
                        backend_engine::DeclarationChange::Added,
                        None,
                        Some(*after_id),
                    )?;
                    right += 1;
                }
                std::cmp::Ordering::Equal => {
                    if before.version.core_payload != after.version.core_payload
                        || before.parent != after.parent
                    {
                        push_diff(
                            rows,
                            after.label,
                            backend_engine::DeclarationChange::Changed,
                            Some(*before_id),
                            Some(*after_id),
                        )?;
                    }
                    left += 1;
                    right += 1;
                }
            },
            (Some((before_id, before)), None) => {
                push_diff(
                    rows,
                    before.label,
                    backend_engine::DeclarationChange::Removed,
                    Some(*before_id),
                    None,
                )?;
                left += 1;
            }
            (None, Some((after_id, after))) => {
                push_diff(
                    rows,
                    after.label,
                    backend_engine::DeclarationChange::Added,
                    None,
                    Some(*after_id),
                )?;
                right += 1;
            }
            (None, None) => break,
        }
    }
    Ok(())
}

fn unmatched_counts(
    before: &[(
        backend_semantic::ir::DeclarationIdentity,
        SemanticDeclaration<'_>,
    )],
    after: &[(
        backend_semantic::ir::DeclarationIdentity,
        SemanticDeclaration<'_>,
    )],
) -> (usize, usize) {
    let mut left = 0;
    let mut right = 0;
    let mut removed = 0;
    let mut added = 0;
    while left < before.len() && right < after.len() {
        match before[left].0.cmp(&after[right].0) {
            std::cmp::Ordering::Less => {
                removed += 1;
                left += 1;
            }
            std::cmp::Ordering::Greater => {
                added += 1;
                right += 1;
            }
            std::cmp::Ordering::Equal => {
                left += 1;
                right += 1;
            }
        }
    }
    (removed + before.len() - left, added + after.len() - right)
}

type SemanticLinkDeltas =
    BTreeMap<backend_semantic::ir::DeclarationIdentity, Vec<backend_engine::SemanticLinkDelta>>;

fn diff_semantic_links(
    before: &SemanticPackageSnapshot<'_>,
    after: &SemanticPackageSnapshot<'_>,
    rows: &mut Vec<backend_engine::DiffRecord>,
) -> Result<(), BuiltinModelError> {
    let (by_source, link_count) = collect_semantic_link_deltas(before, after, rows.len())?;
    attach_semantic_link_deltas(before, after, rows, by_source, link_count)
}

fn collect_semantic_link_deltas(
    before: &SemanticPackageSnapshot<'_>,
    after: &SemanticPackageSnapshot<'_>,
    declaration_rows: usize,
) -> Result<(SemanticLinkDeltas, usize), BuiltinModelError> {
    let mut by_source = SemanticLinkDeltas::new();
    let mut left = 0;
    let mut right = 0;
    let mut total = 0_usize;
    while left < before.links.len() || right < after.links.len() {
        let (older, newer) = match (before.links.get(left), after.links.get(right)) {
            (Some(older), Some(newer)) => match older.key.cmp(&newer.key) {
                std::cmp::Ordering::Less => {
                    left += 1;
                    (Some(older), None)
                }
                std::cmp::Ordering::Greater => {
                    right += 1;
                    (None, Some(newer))
                }
                std::cmp::Ordering::Equal => {
                    left += 1;
                    right += 1;
                    if older.evidence == newer.evidence {
                        continue;
                    }
                    (Some(older), Some(newer))
                }
            },
            (Some(older), None) => {
                left += 1;
                (Some(older), None)
            }
            (None, Some(newer)) => {
                right += 1;
                (None, Some(newer))
            }
            (None, None) => break,
        };
        total = total.checked_add(1).ok_or_else(|| {
            BuiltinModelError("semantic graph diff result count overflowed".to_owned())
        })?;
        ensure_semantic_diff_bound(declaration_rows, total)?;
        let (source, delta) = semantic_link_delta(older, newer)?;
        by_source.entry(source).or_default().push(delta);
    }
    Ok((by_source, total))
}

fn semantic_link_delta(
    older: Option<&SemanticLinkSummary>,
    newer: Option<&SemanticLinkSummary>,
) -> Result<
    (
        backend_semantic::ir::DeclarationIdentity,
        backend_engine::SemanticLinkDelta,
    ),
    BuiltinModelError,
> {
    let summary = newer.or(older).ok_or_else(|| {
        BuiltinModelError("semantic graph diff lost both relation sides".to_owned())
    })?;
    let from = semantic_declaration_identity(summary.key.from);
    let target = semantic_link_target(summary.key.target);
    let relation = semantic_link_kind(summary.key.kind);
    let delta = match (older, newer) {
        (Some(older), None) => backend_engine::SemanticLinkDelta::Removed {
            from,
            target,
            relation,
            evidence: older.evidence.clone(),
        },
        (None, Some(newer)) => backend_engine::SemanticLinkDelta::Added {
            from,
            target,
            relation,
            evidence: newer.evidence.clone(),
        },
        (Some(older), Some(newer)) => backend_engine::SemanticLinkDelta::EvidenceChanged {
            from,
            target,
            relation,
            before: older.evidence.clone(),
            after: newer.evidence.clone(),
        },
        (None, None) => {
            return Err(BuiltinModelError(
                "semantic graph diff lost both relation sides".to_owned(),
            ));
        }
    };
    Ok((summary.key.from, delta))
}

fn attach_semantic_link_deltas(
    before: &SemanticPackageSnapshot<'_>,
    after: &SemanticPackageSnapshot<'_>,
    rows: &mut Vec<backend_engine::DiffRecord>,
    by_source: SemanticLinkDeltas,
    link_count: usize,
) -> Result<(), BuiltinModelError> {
    for (source, links) in by_source {
        let index = semantic_diff_row(before, after, rows, source, link_count)?;
        let row = rows.get_mut(index).ok_or_else(|| {
            BuiltinModelError("semantic graph diff row index is invalid".to_owned())
        })?;
        let mut combined = Vec::from(std::mem::take(&mut row.links));
        combined.extend(links);
        row.links = combined.into_boxed_slice();
    }
    Ok(())
}

fn semantic_diff_row(
    before: &SemanticPackageSnapshot<'_>,
    after: &SemanticPackageSnapshot<'_>,
    rows: &mut Vec<backend_engine::DiffRecord>,
    source: backend_semantic::ir::DeclarationIdentity,
    link_count: usize,
) -> Result<usize, BuiltinModelError> {
    let identity = semantic_declaration_identity(source);
    if let Some(index) = rows
        .iter()
        .position(|row| row.before == Some(identity) || row.after == Some(identity))
    {
        return Ok(index);
    }
    let older = declaration(before, source);
    let newer = declaration(after, source);
    let label = newer.or(older).ok_or_else(|| {
        BuiltinModelError(
            "semantic graph relation source is absent from both package snapshots".to_owned(),
        )
    })?;
    push_diff(
        rows,
        label.label,
        backend_engine::DeclarationChange::Changed,
        older.map(|_| source),
        newer.map(|_| source),
    )?;
    ensure_semantic_diff_bound(rows.len(), link_count)?;
    Ok(rows.len() - 1)
}

fn ensure_semantic_diff_bound(
    declaration_rows: usize,
    link_count: usize,
) -> Result<(), BuiltinModelError> {
    if declaration_rows
        .checked_add(link_count)
        .is_none_or(|facts| facts > backend_engine::MAX_PRODUCT_ROWS)
    {
        return Err(BuiltinModelError(
            "semantic declaration and graph diff exceeds the bounded result contract".to_owned(),
        ));
    }
    Ok(())
}

fn declaration<'snapshot, 'view>(
    snapshot: &'snapshot SemanticPackageSnapshot<'view>,
    identity: backend_semantic::ir::DeclarationIdentity,
) -> Option<&'snapshot SemanticDeclaration<'view>> {
    snapshot
        .declarations
        .binary_search_by_key(&identity, |(identity, _)| *identity)
        .ok()
        .map(|index| &snapshot.declarations[index].1)
}

const fn declaration_change_order(change: backend_engine::DeclarationChange) -> u8 {
    match change {
        backend_engine::DeclarationChange::Added => 0,
        backend_engine::DeclarationChange::Removed => 1,
        backend_engine::DeclarationChange::Changed => 2,
        backend_engine::DeclarationChange::Indeterminate => 3,
    }
}
