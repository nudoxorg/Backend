use super::super::{BuiltinAuthorityVerifier, BuiltinModel, BuiltinModelError, BuiltinValidator};
use super::snapshot::{
    SemanticDeclaration, SemanticLinkSummary, SemanticPackageSnapshot,
    semantic_declaration_identity, semantic_link_kind, semantic_link_target,
    semantic_package_snapshot,
};
use backend_engine::application::LocalCompilerClient;
use std::collections::BTreeMap;

pub(super) fn execute_semantic_diff(
    daemon: &crate::Locald<BuiltinModel, BuiltinValidator, BuiltinAuthorityVerifier>,
    compiler: &LocalCompilerClient,
    generations: &mut super::super::generation_residence::SemanticGenerationResidence,
    image_rows: &mut super::super::view_build::ImageRowResidence,
    from: &backend_engine::PackageReference,
    to: &backend_engine::PackageReference,
) -> Result<Box<[backend_engine::DiffRecord]>, BuiltinModelError> {
    let before = semantic_package_snapshot(daemon, compiler, from, generations, image_rows)?;
    let after = semantic_package_snapshot(daemon, compiler, to, generations, image_rows)?;
    if let (Some(before), Some(after)) = (before, after) {
        return diff_semantic_snapshots(&before, &after);
    }
    structural_package_diff(daemon, from, to)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct StructuralDeclarationSummary {
    identity: backend_semantic::ir::DeclarationIdentity,
    fingerprint: [u8; 32],
}

fn structural_declaration_name(label: &str) -> Option<&str> {
    label.rsplit("::").next().filter(|name| !name.is_empty())
}

fn structural_declaration_fingerprint(row: &backend_engine::Row) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    if let Some(signature) = &row.signature {
        hasher.update(signature.as_bytes());
    }
    for fragment in row.document.iter() {
        match fragment {
            backend_library::Fragment::Text(text) => {
                hasher.update(text.as_bytes());
            }
            backend_library::Fragment::Code(text) => {
                hasher.update(text.as_bytes());
            }
            backend_library::Fragment::Link { label, .. } => {
                hasher.update(label.as_bytes());
            }
            backend_library::Fragment::Break => {
                hasher.update(b"\n");
            }
        }
    }
    *hasher.finalize().as_bytes()
}

fn structural_declaration_identity(
    name: &str,
    fingerprint: [u8; 32],
) -> backend_semantic::ir::DeclarationIdentity {
    let family = blake3::hash(name.as_bytes());
    backend_semantic::ir::DeclarationIdentity {
        family: backend_semantic::ir::DeclarationFamilyId::from_raw({
            let mut bytes = [0_u8; 16];
            bytes.copy_from_slice(&family.as_bytes()[..16]);
            bytes
        }),
        variant: backend_semantic::ir::VariantFingerprint::from_raw({
            let mut bytes = [0_u8; 16];
            bytes.copy_from_slice(&fingerprint[..16]);
            bytes
        }),
    }
}

fn structural_package_declarations(
    daemon: &crate::Locald<BuiltinModel, BuiltinValidator, BuiltinAuthorityVerifier>,
    package: &backend_engine::PackageReference,
) -> Result<BTreeMap<String, StructuralDeclarationSummary>, BuiltinModelError> {
    let view = daemon.engine().daemon().library().view();
    let package_key = backend_engine::package_key(package.as_str());
    if view
        .row_ref(backend_engine::RowId::Package(package_key))
        .filter(|row| row.label == package.as_str())
        .is_none()
    {
        return Err(BuiltinModelError("diff package is not indexed".to_owned()));
    }
    let mut declarations = BTreeMap::new();
    let mut cursor = backend_engine::ViewPageCursor::first(view);
    loop {
        let page = view
            .page(cursor, backend_engine::MAX_SNAPSHOT_PAGE_ROWS)
            .map_err(|error| BuiltinModelError(format!("page structural diff view: {error:?}")))?;
        for row in page.rows() {
            if row.package != Some(package_key) || row.kind.is_none() {
                continue;
            }
            let name = structural_declaration_name(&row.label)
                .ok_or_else(|| {
                    BuiltinModelError(
                        "structural diff declaration coordinate omitted a name".to_owned(),
                    )
                })?
                .to_owned();
            let fingerprint = structural_declaration_fingerprint(row);
            let summary = StructuralDeclarationSummary {
                identity: structural_declaration_identity(&name, fingerprint),
                fingerprint,
            };
            if declarations.insert(name, summary).is_some() {
                return Err(BuiltinModelError(
                    "structural diff package contains a duplicate declaration name".to_owned(),
                ));
            }
        }
        let Some(next) = page.next() else {
            break;
        };
        cursor = next;
    }
    Ok(declarations)
}

fn structural_package_diff(
    daemon: &crate::Locald<BuiltinModel, BuiltinValidator, BuiltinAuthorityVerifier>,
    from: &backend_engine::PackageReference,
    to: &backend_engine::PackageReference,
) -> Result<Box<[backend_engine::DiffRecord]>, BuiltinModelError> {
    let before = structural_package_declarations(daemon, from)?;
    let after = structural_package_declarations(daemon, to)?;
    let mut rows = Vec::new();
    let mut remaining = before;
    for (name, after_summary) in after {
        match remaining.remove(&name) {
            None => push_diff(
                &mut rows,
                &name,
                backend_engine::DeclarationChange::Added,
                None,
                Some(after_summary.identity),
            )?,
            Some(before_summary) if before_summary.fingerprint != after_summary.fingerprint => {
                push_diff(
                    &mut rows,
                    &name,
                    backend_engine::DeclarationChange::Changed,
                    Some(before_summary.identity),
                    Some(after_summary.identity),
                )?;
            }
            Some(_) => {}
        }
    }
    for (name, before_summary) in remaining {
        push_diff(
            &mut rows,
            &name,
            backend_engine::DeclarationChange::Removed,
            Some(before_summary.identity),
            None,
        )?;
    }
    rows.sort_by(|left, right| left.label.as_str().cmp(right.label.as_str()));
    Ok(rows.into_boxed_slice())
}

fn diff_semantic_snapshots(
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
                    let end = semantic_link_key_end(&before.links, left);
                    let older = &before.links[left..end];
                    left = end;
                    (older, &[][..])
                }
                std::cmp::Ordering::Greater => {
                    let end = semantic_link_key_end(&after.links, right);
                    let newer = &after.links[right..end];
                    right = end;
                    (&[][..], newer)
                }
                std::cmp::Ordering::Equal => {
                    let before_end = semantic_link_key_end(&before.links, left);
                    let after_end = semantic_link_key_end(&after.links, right);
                    let older = &before.links[left..before_end];
                    let newer = &after.links[right..after_end];
                    left = before_end;
                    right = after_end;
                    (older, newer)
                }
            },
            (Some(_), None) => {
                let end = semantic_link_key_end(&before.links, left);
                let older = &before.links[left..end];
                left = end;
                (older, &[][..])
            }
            (None, Some(_)) => {
                let end = semantic_link_key_end(&after.links, right);
                let newer = &after.links[right..end];
                right = end;
                (&[][..], newer)
            }
            (None, None) => break,
        };
        diff_semantic_link_group(older, newer, declaration_rows, &mut total, &mut by_source)?;
    }
    Ok((by_source, total))
}

fn semantic_link_key_end(links: &[SemanticLinkSummary], start: usize) -> usize {
    let key = links[start].key;
    start + links[start..].partition_point(|link| link.key == key)
}

fn semantic_link_site_end(links: &[SemanticLinkSummary], start: usize) -> usize {
    let source = links[start].evidence.source.as_ref();
    start
        + links[start..].partition_point(|link| {
            super::snapshot::semantic_link_site_order(link.evidence.source.as_ref(), source)
                == std::cmp::Ordering::Equal
        })
}

fn semantic_link_confidence_end(links: &[SemanticLinkSummary], start: usize) -> usize {
    let confidence = links[start].evidence.confidence;
    start + links[start..].partition_point(|link| link.evidence.confidence == confidence)
}

fn diff_semantic_link_group(
    older: &[SemanticLinkSummary],
    newer: &[SemanticLinkSummary],
    declaration_rows: usize,
    total: &mut usize,
    by_source: &mut SemanticLinkDeltas,
) -> Result<(), BuiltinModelError> {
    if older.is_empty() || newer.is_empty() {
        let (old, new) = if older.is_empty() {
            (None, Some(newer))
        } else {
            (Some(older), None)
        };
        let group = old.or(new).ok_or_else(|| {
            BuiltinModelError("semantic graph diff lost both relation groups".to_owned())
        })?;
        for summary in group {
            append_semantic_link_delta(
                old.map(|_| summary),
                new.map(|_| summary),
                declaration_rows,
                total,
                by_source,
            )?;
        }
        return Ok(());
    }

    let mut left = 0;
    let mut right = 0;
    while left < older.len() || right < newer.len() {
        let ordering = match (older.get(left), newer.get(right)) {
            (Some(old), Some(new)) => super::snapshot::semantic_link_site_order(
                old.evidence.source.as_ref(),
                new.evidence.source.as_ref(),
            ),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => break,
        };
        match ordering {
            std::cmp::Ordering::Less => {
                let end = semantic_link_site_end(older, left);
                diff_semantic_link_group(
                    &older[left..end],
                    &[],
                    declaration_rows,
                    total,
                    by_source,
                )?;
                left = end;
            }
            std::cmp::Ordering::Greater => {
                let end = semantic_link_site_end(newer, right);
                diff_semantic_link_group(
                    &[],
                    &newer[right..end],
                    declaration_rows,
                    total,
                    by_source,
                )?;
                right = end;
            }
            std::cmp::Ordering::Equal => {
                let old_end = semantic_link_site_end(older, left);
                let new_end = semantic_link_site_end(newer, right);
                diff_semantic_link_site(
                    &older[left..old_end],
                    &newer[right..new_end],
                    declaration_rows,
                    total,
                    by_source,
                )?;
                left = old_end;
                right = new_end;
            }
        }
    }
    Ok(())
}

fn diff_semantic_link_site(
    older: &[SemanticLinkSummary],
    newer: &[SemanticLinkSummary],
    declaration_rows: usize,
    total: &mut usize,
    by_source: &mut SemanticLinkDeltas,
) -> Result<(), BuiltinModelError> {
    let (removed, added, removed_summary, added_summary) =
        unmatched_semantic_link_evidence(older, newer);
    if removed == 1 && added == 1 {
        return append_semantic_link_delta(
            removed_summary,
            added_summary,
            declaration_rows,
            total,
            by_source,
        );
    }
    append_unmatched_semantic_link_evidence(older, newer, declaration_rows, total, by_source)
}

fn unmatched_semantic_link_evidence<'summary>(
    older: &'summary [SemanticLinkSummary],
    newer: &'summary [SemanticLinkSummary],
) -> (
    usize,
    usize,
    Option<&'summary SemanticLinkSummary>,
    Option<&'summary SemanticLinkSummary>,
) {
    let mut left = 0;
    let mut right = 0;
    let mut removed = 0;
    let mut added = 0;
    let mut removed_summary = None;
    let mut added_summary = None;
    while left < older.len() || right < newer.len() {
        match (older.get(left), newer.get(right)) {
            (Some(old), Some(new)) => match old.evidence.confidence.cmp(&new.evidence.confidence) {
                std::cmp::Ordering::Less => {
                    let end = semantic_link_confidence_end(older, left);
                    let count = end - left;
                    if count == 1 {
                        removed_summary = Some(&older[left]);
                    }
                    removed += count;
                    left = end;
                }
                std::cmp::Ordering::Greater => {
                    let end = semantic_link_confidence_end(newer, right);
                    let count = end - right;
                    if count == 1 {
                        added_summary = Some(&newer[right]);
                    }
                    added += count;
                    right = end;
                }
                std::cmp::Ordering::Equal => {
                    let old_end = semantic_link_confidence_end(older, left);
                    let new_end = semantic_link_confidence_end(newer, right);
                    let old_count = old_end - left;
                    let new_count = new_end - right;
                    if old_count > new_count {
                        if old_count - new_count == 1 {
                            removed_summary = Some(&older[left + new_count]);
                        }
                        removed += old_count - new_count;
                    } else if new_count > old_count {
                        if new_count - old_count == 1 {
                            added_summary = Some(&newer[right + old_count]);
                        }
                        added += new_count - old_count;
                    }
                    left = old_end;
                    right = new_end;
                }
            },
            (Some(_), None) => {
                let end = semantic_link_confidence_end(older, left);
                let count = end - left;
                if count == 1 {
                    removed_summary = Some(&older[left]);
                }
                removed += count;
                left = end;
            }
            (None, Some(_)) => {
                let end = semantic_link_confidence_end(newer, right);
                let count = end - right;
                if count == 1 {
                    added_summary = Some(&newer[right]);
                }
                added += count;
                right = end;
            }
            (None, None) => break,
        }
    }
    (removed, added, removed_summary, added_summary)
}

fn append_unmatched_semantic_link_evidence(
    older: &[SemanticLinkSummary],
    newer: &[SemanticLinkSummary],
    declaration_rows: usize,
    total: &mut usize,
    by_source: &mut SemanticLinkDeltas,
) -> Result<(), BuiltinModelError> {
    let mut left = 0;
    let mut right = 0;
    while left < older.len() || right < newer.len() {
        match (older.get(left), newer.get(right)) {
            (Some(old), Some(new)) => match old.evidence.confidence.cmp(&new.evidence.confidence) {
                std::cmp::Ordering::Less => {
                    let end = semantic_link_confidence_end(older, left);
                    for summary in &older[left..end] {
                        append_semantic_link_delta(
                            Some(summary),
                            None,
                            declaration_rows,
                            total,
                            by_source,
                        )?;
                    }
                    left = end;
                }
                std::cmp::Ordering::Greater => {
                    let end = semantic_link_confidence_end(newer, right);
                    for summary in &newer[right..end] {
                        append_semantic_link_delta(
                            None,
                            Some(summary),
                            declaration_rows,
                            total,
                            by_source,
                        )?;
                    }
                    right = end;
                }
                std::cmp::Ordering::Equal => {
                    let old_end = semantic_link_confidence_end(older, left);
                    let new_end = semantic_link_confidence_end(newer, right);
                    let common = (old_end - left).min(new_end - right);
                    for summary in &older[left + common..old_end] {
                        append_semantic_link_delta(
                            Some(summary),
                            None,
                            declaration_rows,
                            total,
                            by_source,
                        )?;
                    }
                    for summary in &newer[right + common..new_end] {
                        append_semantic_link_delta(
                            None,
                            Some(summary),
                            declaration_rows,
                            total,
                            by_source,
                        )?;
                    }
                    left = old_end;
                    right = new_end;
                }
            },
            (Some(_), None) => {
                let end = semantic_link_confidence_end(older, left);
                for summary in &older[left..end] {
                    append_semantic_link_delta(
                        Some(summary),
                        None,
                        declaration_rows,
                        total,
                        by_source,
                    )?;
                }
                left = end;
            }
            (None, Some(_)) => {
                let end = semantic_link_confidence_end(newer, right);
                for summary in &newer[right..end] {
                    append_semantic_link_delta(
                        None,
                        Some(summary),
                        declaration_rows,
                        total,
                        by_source,
                    )?;
                }
                right = end;
            }
            (None, None) => break,
        }
    }
    Ok(())
}

fn append_semantic_link_delta(
    older: Option<&SemanticLinkSummary>,
    newer: Option<&SemanticLinkSummary>,
    declaration_rows: usize,
    total: &mut usize,
    by_source: &mut SemanticLinkDeltas,
) -> Result<(), BuiltinModelError> {
    *total = total.checked_add(1).ok_or_else(|| {
        BuiltinModelError("semantic graph diff result count overflowed".to_owned())
    })?;
    ensure_semantic_diff_bound(declaration_rows, *total)?;
    let (source, delta) = semantic_link_delta(older, newer)?;
    by_source.entry(source).or_default().push(delta);
    Ok(())
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

fn push_diff(
    rows: &mut Vec<backend_engine::DiffRecord>,
    label: &str,
    change: backend_engine::DeclarationChange,
    before: Option<backend_semantic::ir::DeclarationIdentity>,
    after: Option<backend_semantic::ir::DeclarationIdentity>,
) -> Result<(), BuiltinModelError> {
    if rows.len() == backend_engine::MAX_PRODUCT_ROWS {
        return Err(BuiltinModelError(
            "semantic diff exceeds the bounded result contract".to_owned(),
        ));
    }
    rows.push(backend_engine::DiffRecord {
        label: backend_engine::ProductText::new(label)
            .map_err(|error| BuiltinModelError(error.to_string()))?,
        change,
        before: before.map(semantic_declaration_identity),
        after: after.map(semantic_declaration_identity),
        links: Box::new([]),
    });
    Ok(())
}

const fn declaration_change_order(change: backend_engine::DeclarationChange) -> u8 {
    match change {
        backend_engine::DeclarationChange::Added => 0,
        backend_engine::DeclarationChange::Removed => 1,
        backend_engine::DeclarationChange::Changed => 2,
        backend_engine::DeclarationChange::Indeterminate => 3,
    }
}
#[cfg(test)]
mod semantic_diff_tests {
    use super::super::snapshot::{
        SemanticDeclaration, SemanticLinkSummary, SemanticPackageSnapshot,
    };
    use super::*;
    use backend_semantic::ir::{
        BorrowedTree, Confidence, CorePayloadHash, DeclarationFamilyId, DeclarationIdentity,
        EntityAuthorityFacts, EntityVersion, ExternalTarget, FactAvailability,
        ForeignDeclarationId, ForeignExternalTarget, ForeignTargetOrigin, IrBuilder, ItemKind,
        LinkKind, LinkTarget, OccurrenceAuthorityFacts, ParentageAuthority,
        SemanticCoreReader as _, SemanticReader as _, SemanticSnapshot, SemanticStableLinks,
        SourceSpan, StableLinkKey, TreeEntityId, TreeItemInput, TreeLinkInput, TreeLinkTarget,
        VariantAvailability, VariantFingerprint, Visibility, encode_full_semantic_image,
        full_semantic_image_len,
    };

    #[derive(Clone, Copy)]
    struct ForeignLinkSite {
        target_path: &'static str,
        target_display: &'static str,
        start: u32,
        end: u32,
        confidence: Confidence,
    }

    fn declaration(
        family: u16,
        variant: u16,
        payload: u8,
        label: &'static str,
    ) -> (DeclarationIdentity, SemanticDeclaration<'static>) {
        let mut family_bytes = [0; 16];
        family_bytes[..2].copy_from_slice(&family.to_be_bytes());
        let mut variant_bytes = [0; 16];
        variant_bytes[..2].copy_from_slice(&variant.to_be_bytes());
        let version = EntityVersion {
            family: DeclarationFamilyId::from_raw(family_bytes),
            variant: VariantFingerprint::from_raw(variant_bytes),
            core_payload: CorePayloadHash::from_raw([payload; 16]),
        };
        (
            version.identity(),
            SemanticDeclaration {
                label,
                version,
                parent: None,
            },
        )
    }

    fn snapshot(
        declarations: impl IntoIterator<Item = (DeclarationIdentity, SemanticDeclaration<'static>)>,
    ) -> SemanticPackageSnapshot<'static> {
        let mut declarations = declarations.into_iter().collect::<Vec<_>>();
        declarations.sort_unstable_by_key(|(identity, _)| *identity);
        SemanticPackageSnapshot {
            declarations,
            links: Vec::new(),
        }
    }

    fn admitted_foreign_link_image(
        sites: &[ForeignLinkSite],
    ) -> Result<backend_library::interface::SemanticImageSnapshot, BuiltinModelError> {
        let mut builder = IrBuilder::new();
        let caller_version = declaration(7, 1, 1, "caller").1.version;
        let source_path = builder
            .intern_atom(b"src/httpie/client.py")
            .map_err(|error| BuiltinModelError(error.to_string()))?;
        let ecosystem = builder
            .intern_atom(b"python")
            .map_err(|error| BuiltinModelError(error.to_string()))?;
        let namespace = builder
            .intern_atom(b"httpie")
            .map_err(|error| BuiltinModelError(error.to_string()))?;
        let mut links = Vec::new();
        links
            .try_reserve_exact(sites.len())
            .map_err(|error| BuiltinModelError(format!("reserve fixture link inputs: {error}")))?;
        for site in sites {
            let path = builder
                .intern_atom(site.target_path.as_bytes())
                .map_err(|error| BuiltinModelError(error.to_string()))?;
            let display = builder
                .intern_atom(site.target_display.as_bytes())
                .map_err(|error| BuiltinModelError(error.to_string()))?;
            let target = builder
                .intern_external(ExternalTarget::Foreign(ForeignExternalTarget {
                    identity: backend_semantic::ir::ExternalDeclarationIdentity {
                        foreign: ForeignDeclarationId::from_raw([0x65; 16]),
                        variant: VariantAvailability::Unavailable,
                    },
                    origin: ForeignTargetOrigin::Namespace {
                        ecosystem,
                        namespace,
                    },
                    path,
                    display,
                    kind: Some(ItemKind::Function),
                }))
                .map_err(|error| BuiltinModelError(error.to_string()))?;
            let source = SourceSpan::new(source_path, site.start, site.end).ok_or_else(|| {
                BuiltinModelError("fixture source span has an invalid range".to_owned())
            })?;
            links.push(TreeLinkInput {
                from: TreeEntityId::new(0),
                target: TreeLinkTarget::External(target),
                kind: LinkKind::Calls,
                confidence: site.confidence,
                authority: OccurrenceAuthorityFacts {
                    source: FactAvailability::Captured,
                },
                source: Some(source),
            });
        }
        let items = [TreeItemInput {
            name: b"caller",
            anonymous_callable_anchor: None,
            kind: ItemKind::Function,
            visibility: Visibility::Public,
            authority: EntityAuthorityFacts {
                parentage: ParentageAuthority::Root,
                visibility: FactAvailability::Captured,
                ..EntityAuthorityFacts::default()
            },
            parent: None,
            semantic_type: None,
            members: &[],
            docs: &[],
            attributes: &[],
            source: None,
            extension: None,
        }];
        builder
            .add_borrowed_tree(BorrowedTree {
                versions: &[caller_version],
                items: &items,
                links: &links,
            })
            .map_err(|error| BuiltinModelError(error.to_string()))?;
        let ir = builder
            .finish()
            .map_err(|error| BuiltinModelError(error.to_string()))?;
        let mut bytes = vec![
            0;
            full_semantic_image_len(&ir)
                .map_err(|error| BuiltinModelError(error.to_string()))?
        ];
        encode_full_semantic_image(&ir, &mut bytes)
            .map_err(|error| BuiltinModelError(error.to_string()))?;
        let authority = backend_library::interface::SemanticImageAuthority {
            identity: backend_version::ArtifactId::<
                backend_version::IrSemanticImageEncoding,
                backend_version::IrSemanticImageDomain,
            >::from_encoded_bytes(&bytes),
            byte_len: u32::try_from(bytes.len())
                .map_err(|error| BuiltinModelError(error.to_string()))?,
        };
        backend_library::interface::SemanticImageSnapshot::try_from_reopened(authority, &bytes)
            .map_err(|error| BuiltinModelError(format!("admit fixture semantic image: {error:?}")))
    }

    fn package_snapshot_from_image(
        admitted: &backend_library::interface::SemanticImageSnapshot,
    ) -> Result<SemanticPackageSnapshot<'static>, BuiltinModelError> {
        let image = admitted.reopen().map_err(|error| {
            BuiltinModelError(format!("reopen admitted fixture image: {error}"))
        })?;
        let mut declarations = Vec::new();
        for entity in image.canonical_entities() {
            let identity = entity.version.identity();
            let parent = entity
                .parent
                .and_then(|parent| image.entity(parent))
                .map(|parent| parent.version.identity());
            declarations.push((
                identity,
                SemanticDeclaration {
                    label: "caller",
                    version: entity.version,
                    parent,
                },
            ));
        }
        let mut links = Vec::new();
        let semantic_snapshot = SemanticSnapshot {
            generation: backend_semantic::ir::GenerationId::from_canonical_bytes(admitted.as_ref()),
            reader: &image,
        };
        super::super::snapshot::append_semantic_image_links(
            semantic_snapshot,
            &mut links,
            super::super::snapshot::MAX_DIFF_LINKS,
        )?;
        super::super::snapshot::finish_semantic_snapshot(true, declarations, links)?.ok_or_else(
            || BuiltinModelError("admitted fixture publication was not retained".to_owned()),
        )
    }

    fn foreign_site(path_start: u32, alias: &'static str) -> ForeignLinkSite {
        ForeignLinkSite {
            target_path: alias,
            target_display: alias,
            start: path_start,
            end: path_start + 6,
            confidence: Confidence::Compiler,
        }
    }

    #[test]
    fn singleton_variant_churn_is_one_change() -> Result<(), BuiltinModelError> {
        let before = snapshot([declaration(1, 1, 1, "old")]);
        let after = snapshot([declaration(1, 2, 2, "new")]);
        let rows = diff_semantic_snapshots(&before, &after)?;
        assert!(matches!(
            rows.as_ref(),
            [backend_engine::DiffRecord {
                change: backend_engine::DeclarationChange::Changed,
                ..
            }]
        ));
        assert_eq!(rows[0].label.as_str(), "new");
        Ok(())
    }

    #[test]
    fn ambiguous_overload_churn_never_claims_removal() -> Result<(), BuiltinModelError> {
        let before = snapshot([
            declaration(1, 1, 1, "before-a"),
            declaration(1, 2, 1, "before-b"),
        ]);
        let after = snapshot([
            declaration(1, 3, 2, "after-a"),
            declaration(1, 4, 2, "after-b"),
        ]);
        let rows = diff_semantic_snapshots(&before, &after)?;
        assert_eq!(rows.len(), 4);
        assert_eq!(
            rows.iter()
                .filter(|row| row.change == backend_engine::DeclarationChange::Indeterminate)
                .count(),
            2
        );
        assert!(
            !rows
                .iter()
                .any(|row| row.change == backend_engine::DeclarationChange::Removed)
        );
        Ok(())
    }

    #[test]
    fn exact_identity_payload_change_is_reported() -> Result<(), BuiltinModelError> {
        let before = snapshot([declaration(1, 1, 1, "before")]);
        let after = snapshot([declaration(1, 1, 2, "after")]);
        let rows = diff_semantic_snapshots(&before, &after)?;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].change, backend_engine::DeclarationChange::Changed);
        Ok(())
    }

    #[test]
    fn link_only_change_retains_typed_endpoint_and_both_evidence_rows()
    -> Result<(), BuiltinModelError> {
        let source = declaration(1, 1, 1, "source");
        let target = declaration(2, 1, 1, "target");
        let key = StableLinkKey {
            from: source.0,
            target: backend_semantic::ir::DeclarationLinkTarget::Local(target.0),
            kind: backend_semantic::ir::LinkKind::Calls,
        };
        let source_path = backend_engine::ProductText::new("src/lib.rs")
            .map_err(|error| BuiltinModelError(error.to_string()))?;
        let before_evidence = backend_engine::SemanticLinkEvidence {
            confidence: backend_engine::SemanticConfidence::Syntactic,
            source: Some(backend_engine::SemanticSourceSpan {
                file: source_path.clone(),
                start: 4,
                end: 10,
            }),
        };
        let after_evidence = backend_engine::SemanticLinkEvidence {
            confidence: backend_engine::SemanticConfidence::Compiler,
            source: Some(backend_engine::SemanticSourceSpan {
                file: source_path,
                start: 4,
                end: 10,
            }),
        };
        let mut before = snapshot([source, target]);
        before.links.push(SemanticLinkSummary {
            key,
            evidence: before_evidence.clone(),
        });
        let source = declaration(1, 1, 1, "source");
        let target = declaration(2, 1, 1, "target");
        let mut after = snapshot([source, target]);
        after.links.push(SemanticLinkSummary {
            key,
            evidence: after_evidence.clone(),
        });

        let rows = diff_semantic_snapshots(&before, &after)?;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].label.as_str(), "source");
        assert_eq!(rows[0].change, backend_engine::DeclarationChange::Changed);
        assert_eq!(
            rows[0].before,
            Some(semantic_declaration_identity(key.from))
        );
        assert_eq!(rows[0].after, Some(semantic_declaration_identity(key.from)));
        assert!(matches!(
            rows[0].links.as_ref(),
            [backend_engine::SemanticLinkDelta::EvidenceChanged {
                target: backend_engine::SemanticLinkTarget::Local { .. },
                relation: backend_engine::SemanticLinkKind::Calls,
                before,
                after,
                ..
            }] if before == &before_evidence && after == &after_evidence
        ));
        Ok(())
    }

    #[test]
    fn admitted_foreign_aliases_preserve_two_sites_and_reverse_input_order()
    -> Result<(), BuiltinModelError> {
        let first = foreign_site(10, "httpie.core.request");
        let second = foreign_site(24, "httpie.client.request");
        let forward_image = admitted_foreign_link_image(&[first, second])?;
        let reversed_image = admitted_foreign_link_image(&[second, first])?;
        assert_eq!(forward_image.as_ref(), reversed_image.as_ref());

        // These are two real canonical FullLinks rows. Their foreign descriptors
        // differ in raw identity, path, and display while the stable foreign
        // declaration identity deliberately collapses them to one relation key.
        let view = forward_image.reopen().map_err(|error| {
            BuiltinModelError(format!("reopen admitted fixture image: {error}"))
        })?;
        let raw_links = view.canonical_links().collect::<Vec<_>>();
        assert_eq!(raw_links.len(), 2);
        let external_ids = raw_links
            .iter()
            .map(|(_, link)| match link.target {
                LinkTarget::External(id) => Ok(id),
                LinkTarget::Local(_) => Err(BuiltinModelError(
                    "foreign-link fixture unexpectedly has a local target".to_owned(),
                )),
            })
            .collect::<Result<Vec<_>, _>>()?;
        assert_ne!(external_ids[0], external_ids[1]);
        let external_descriptors = external_ids
            .iter()
            .map(|id| match view.external(*id) {
                Some(ExternalTarget::Foreign(target)) => Ok(target),
                _ => Err(BuiltinModelError(
                    "foreign-link fixture descriptor was not retained".to_owned(),
                )),
            })
            .collect::<Result<Vec<_>, _>>()?;
        assert_eq!(
            external_descriptors[0].identity,
            external_descriptors[1].identity
        );
        let raw_aliases = external_descriptors
            .iter()
            .map(|target| {
                Ok((
                    view.atom(target.path).ok_or_else(|| {
                        BuiltinModelError("fixture target path atom is absent".to_owned())
                    })?,
                    view.atom(target.display).ok_or_else(|| {
                        BuiltinModelError("fixture target display atom is absent".to_owned())
                    })?,
                ))
            })
            .collect::<Result<Vec<_>, BuiltinModelError>>()?;
        assert_ne!(raw_aliases[0].0, raw_aliases[1].0);
        assert_ne!(raw_aliases[0].1, raw_aliases[1].1);
        let semantic_snapshot = SemanticSnapshot {
            generation: backend_semantic::ir::GenerationId::from_canonical_bytes(
                forward_image.as_ref(),
            ),
            reader: &view,
        };
        let stable_links = SemanticStableLinks::new(semantic_snapshot).collect::<Vec<_>>();
        assert_eq!(stable_links.len(), 2);
        assert_eq!(stable_links[0].key, stable_links[1].key);

        let before = package_snapshot_from_image(&forward_image)?;
        let after = package_snapshot_from_image(&reversed_image)?;
        assert_eq!(before.links.len(), 2);
        assert_eq!(before.links[0].key, before.links[1].key);
        assert_eq!(
            before.links[0].key.kind,
            backend_semantic::ir::LinkKind::Calls
        );
        assert!(matches!(
            before.links[0].key.target,
            backend_semantic::ir::DeclarationLinkTarget::Foreign(identity)
                if identity.foreign == ForeignDeclarationId::from_raw([0x65; 16])
                    && identity.variant == VariantAvailability::Unavailable
        ));
        assert_eq!(
            before
                .links
                .iter()
                .map(|link| link.evidence.clone())
                .collect::<Vec<_>>(),
            [
                backend_engine::SemanticLinkEvidence {
                    confidence: backend_engine::SemanticConfidence::Compiler,
                    source: Some(backend_engine::SemanticSourceSpan {
                        file: backend_engine::ProductText::new("src/httpie/client.py")
                            .map_err(|error| BuiltinModelError(error.to_string()))?,
                        start: 10,
                        end: 16,
                    }),
                },
                backend_engine::SemanticLinkEvidence {
                    confidence: backend_engine::SemanticConfidence::Compiler,
                    source: Some(backend_engine::SemanticSourceSpan {
                        file: backend_engine::ProductText::new("src/httpie/client.py")
                            .map_err(|error| BuiltinModelError(error.to_string()))?,
                        start: 24,
                        end: 30,
                    }),
                },
            ]
        );
        assert!(diff_semantic_snapshots(&before, &after)?.is_empty());
        Ok(())
    }

    #[test]
    fn admitted_same_site_confidence_change_retains_both_observations()
    -> Result<(), BuiltinModelError> {
        let mut before_site = foreign_site(10, "httpie.core.request");
        before_site.confidence = Confidence::Syntactic;
        let before_image = admitted_foreign_link_image(&[before_site])?;
        let after_image = admitted_foreign_link_image(&[foreign_site(10, "httpie.core.request")])?;
        let before = package_snapshot_from_image(&before_image)?;
        let after = package_snapshot_from_image(&after_image)?;

        let rows = diff_semantic_snapshots(&before, &after)?;
        assert_eq!(rows.len(), 1);
        assert!(matches!(
            rows[0].links.as_ref(),
            [backend_engine::SemanticLinkDelta::EvidenceChanged { before, after, .. }]
                if before.confidence == backend_engine::SemanticConfidence::Syntactic
                    && after.confidence == backend_engine::SemanticConfidence::Compiler
                    && before.source == after.source
        ));
        Ok(())
    }

    #[test]
    fn admitted_changed_source_site_is_added_and_removed_without_guessing()
    -> Result<(), BuiltinModelError> {
        let before_image = admitted_foreign_link_image(&[foreign_site(10, "httpie.core.request")])?;
        let after_image = admitted_foreign_link_image(&[foreign_site(20, "httpie.core.request")])?;
        let before = package_snapshot_from_image(&before_image)?;
        let after = package_snapshot_from_image(&after_image)?;

        let rows = diff_semantic_snapshots(&before, &after)?;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].links.len(), 2);
        assert_eq!(
            rows[0]
                .links
                .iter()
                .filter(|link| matches!(link, backend_engine::SemanticLinkDelta::Removed { .. }))
                .count(),
            1
        );
        assert_eq!(
            rows[0]
                .links
                .iter()
                .filter(|link| matches!(link, backend_engine::SemanticLinkDelta::Added { .. }))
                .count(),
            1
        );
        assert!(rows[0].links.iter().all(|link| !matches!(
            link,
            backend_engine::SemanticLinkDelta::EvidenceChanged { .. }
        )));
        Ok(())
    }

    #[test]
    fn admitted_repeated_site_add_remove_and_exact_duplicate_multiplicity_are_preserved()
    -> Result<(), BuiltinModelError> {
        let first = foreign_site(10, "httpie.core.request");
        let second = foreign_site(24, "httpie.client.request");
        let before_image = admitted_foreign_link_image(&[first])?;
        let repeated_image = admitted_foreign_link_image(&[first, second])?;
        let before = package_snapshot_from_image(&before_image)?;
        let repeated = package_snapshot_from_image(&repeated_image)?;

        let added = diff_semantic_snapshots(&before, &repeated)?;
        assert_eq!(added.len(), 1);
        assert!(matches!(
            added[0].links.as_ref(),
            [backend_engine::SemanticLinkDelta::Added { evidence, .. }]
                if evidence.source.as_ref().is_some_and(|source| source.start == 24 && source.end == 30)
        ));
        let removed = diff_semantic_snapshots(&repeated, &before)?;
        assert_eq!(removed.len(), 1);
        assert!(matches!(
            removed[0].links.as_ref(),
            [backend_engine::SemanticLinkDelta::Removed { evidence, .. }]
                if evidence.source.as_ref().is_some_and(|source| source.start == 24 && source.end == 30)
        ));

        let duplicate = foreign_site(10, "httpie.client.request");
        let repeated_evidence_image = admitted_foreign_link_image(&[first, duplicate])?;
        let one_evidence = package_snapshot_from_image(&before_image)?;
        let two_equal_evidence = package_snapshot_from_image(&repeated_evidence_image)?;
        assert_eq!(two_equal_evidence.links.len(), 2);
        assert_eq!(
            two_equal_evidence.links[0].evidence,
            two_equal_evidence.links[1].evidence
        );
        assert!(diff_semantic_snapshots(&two_equal_evidence, &two_equal_evidence)?.is_empty());
        let one_removed = diff_semantic_snapshots(&two_equal_evidence, &one_evidence)?;
        assert_eq!(one_removed.len(), 1);
        assert!(matches!(
            one_removed[0].links.as_ref(),
            [backend_engine::SemanticLinkDelta::Removed { .. }]
        ));
        Ok(())
    }

    #[test]
    fn admitted_link_projection_refuses_its_budget_without_hiding_partial_evidence()
    -> Result<(), BuiltinModelError> {
        let image = admitted_foreign_link_image(&[
            foreign_site(10, "httpie.core.request"),
            foreign_site(24, "httpie.client.request"),
        ])?;
        let view = image.reopen().map_err(|error| {
            BuiltinModelError(format!("reopen admitted fixture image: {error}"))
        })?;
        let semantic_snapshot = SemanticSnapshot {
            generation: backend_semantic::ir::GenerationId::from_canonical_bytes(image.as_ref()),
            reader: &view,
        };
        let mut links = Vec::new();
        let error =
            super::super::snapshot::append_semantic_image_links(semantic_snapshot, &mut links, 1)
                .expect_err("second distinct stable-link occurrence exceeds the test budget");
        assert!(error.0.contains("memory budget"));
        assert_eq!(links.len(), 1);
        Ok(())
    }

    #[test]
    fn finish_keeps_duplicate_declaration_identity_rejection() {
        let repeated = declaration(1, 1, 1, "caller");
        let result = super::super::snapshot::finish_semantic_snapshot(
            true,
            vec![repeated, repeated],
            Vec::new(),
        );
        let error = match result {
            Ok(_) => panic!("duplicate declaration identity must remain invalid"),
            Err(error) => error,
        };
        assert!(error.0.contains("duplicate declaration identity"));
    }

    #[test]
    fn absent_source_evidence_remains_distinct_from_a_captured_site()
    -> Result<(), BuiltinModelError> {
        let source = declaration(1, 1, 1, "source");
        let key = StableLinkKey {
            from: source.0,
            target: backend_semantic::ir::DeclarationLinkTarget::Foreign(
                backend_semantic::ir::ExternalDeclarationIdentity {
                    foreign: ForeignDeclarationId::from_raw([0x65; 16]),
                    variant: VariantAvailability::Unavailable,
                },
            ),
            kind: LinkKind::Calls,
        };
        let mut package = snapshot([source]);
        package.links = vec![
            SemanticLinkSummary {
                key,
                evidence: backend_engine::SemanticLinkEvidence {
                    confidence: backend_engine::SemanticConfidence::Compiler,
                    source: Some(backend_engine::SemanticSourceSpan {
                        file: backend_engine::ProductText::new("src/caller.py")
                            .map_err(|error| BuiltinModelError(error.to_string()))?,
                        start: 4,
                        end: 9,
                    }),
                },
            },
            SemanticLinkSummary {
                key,
                evidence: backend_engine::SemanticLinkEvidence {
                    confidence: backend_engine::SemanticConfidence::Compiler,
                    source: None,
                },
            },
        ];
        let package = super::super::snapshot::finish_semantic_snapshot(
            true,
            package.declarations,
            package.links,
        )?
        .ok_or_else(|| BuiltinModelError("fixture publication was not retained".to_owned()))?;
        assert!(package.links[0].evidence.source.is_none());
        assert!(package.links[1].evidence.source.is_some());
        Ok(())
    }

    #[test]
    fn overload_merge_retains_exact_matches_and_marks_only_churn_ambiguous()
    -> Result<(), BuiltinModelError> {
        let before = snapshot([
            declaration(1, 1, 1, "stable"),
            declaration(1, 2, 1, "removed"),
        ]);
        let after = snapshot([
            declaration(1, 1, 1, "stable"),
            declaration(1, 3, 1, "added"),
        ]);
        let rows = diff_semantic_snapshots(&before, &after)?;
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().any(|row| {
            row.label.as_str() == "removed"
                && row.change == backend_engine::DeclarationChange::Indeterminate
        }));
        assert!(rows.iter().any(|row| {
            row.label.as_str() == "added" && row.change == backend_engine::DeclarationChange::Added
        }));
        Ok(())
    }

    #[test]
    fn result_bound_is_enforced_during_the_linear_merge() {
        let before = snapshot([]);
        let after = snapshot(
            (0..=backend_engine::MAX_PRODUCT_ROWS)
                .map(|index| declaration(index as u16, 1, 1, "added")),
        );
        assert!(diff_semantic_snapshots(&before, &after).is_err());
    }
}
