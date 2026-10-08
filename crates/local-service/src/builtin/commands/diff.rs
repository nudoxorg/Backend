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

/// Package-relative declaration coordinates used by the structural fallback.
/// It preserves repeated signature/documentation variants but cannot prove
/// identity across file moves or parent-only changes.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct StructuralCoordinate<'row> {
    path: &'row str,
    kind: backend_engine::DeclarationKind,
    // File-module rows are addressed by their exact source path; ordinary
    // declarations and semantic rows retain their typed name.
    name: Option<&'row str>,
    semantic_family: Option<backend_semantic::ir::DeclarationFamilyId>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct StructuralOccurrence {
    identity: backend_semantic::ir::DeclarationIdentity,
    count: usize,
}

type StructuralVariantMap = BTreeMap<[u8; 32], StructuralOccurrence>;
type StructuralDeclarations<'row> = BTreeMap<StructuralCoordinate<'row>, StructuralVariantMap>;

struct AdmittedStructuralRow<'row> {
    row: &'row backend_engine::Row,
    coordinate: StructuralCoordinate<'row>,
    semantic_variant: Option<backend_semantic::ir::VariantFingerprint>,
}

fn admitted_structural_row<'row>(
    row: &'row backend_engine::Row,
    package_label: &str,
    package_key: backend_engine::PackageKey,
) -> Result<Option<AdmittedStructuralRow<'row>>, BuiltinModelError> {
    if row.package != Some(package_key) {
        return Ok(None);
    }
    let Some(kind) = row.kind else {
        return Ok(None);
    };
    let invalid = || {
        BuiltinModelError(
            "structural diff declaration failed canonical identity admission".to_owned(),
        )
    };
    if row.label.contains('\0')
        || !row.label.starts_with(package_label)
        || row
            .label
            .strip_prefix(package_label)
            .is_none_or(|suffix| !suffix.starts_with("::"))
    {
        return Err(invalid());
    }
    let (path, name, semantic_identity) = match row.identity_preimage() {
        None => {
            if row.id != backend_engine::RowId::Symbol(backend_engine::symbol_key(&row.label)) {
                return Err(invalid());
            }
            let (path, name) = structural_coordinate_parts(row, package_label)?;
            (path, name, None)
        }
        Some(preimage) if preimage.as_str().contains('\0') => {
            let preimage = preimage.as_str();
            let mut fields = preimage.split('\0');
            let coordinate = fields.next().ok_or_else(invalid)?;
            let encoded_kind = fields.next().ok_or_else(invalid)?;
            let signature = fields.next().ok_or_else(invalid)?;
            let occurrence = fields.next().ok_or_else(invalid)?;
            let occurrence_number = occurrence.parse::<u32>().map_err(|_| invalid())?;
            if fields.next().is_some()
                || coordinate != row.label
                || encoded_kind != kind.name()
                || row.signature.as_deref() != Some(signature)
                || occurrence != occurrence_number.to_string()
                || row.id != backend_engine::RowId::Symbol(backend_engine::symbol_key(preimage))
            {
                return Err(invalid());
            }
            let (path, name) = structural_coordinate_parts(row, package_label)?;
            (path, name, None)
        }
        Some(preimage) => {
            let (path, name, identity) =
                semantic_coordinate_parts(row, package_label, package_key, preimage.as_str())?;
            (path, Some(name), Some(identity))
        }
    };
    let (semantic_family, semantic_variant) = semantic_identity
        .map(|identity| (Some(identity.family), Some(identity.variant)))
        .unwrap_or((None, None));
    Ok(Some(AdmittedStructuralRow {
        row,
        coordinate: StructuralCoordinate {
            path,
            kind,
            name,
            semantic_family,
        },
        semantic_variant,
    }))
}

fn checked_source_path(row: &backend_engine::Row) -> Result<&str, BuiltinModelError> {
    let path = row.source.file_path().ok_or_else(|| {
        BuiltinModelError("structural diff declaration has no source path".to_owned())
    })?;
    if path.is_empty()
        || path == "<unknown>"
        || path.len() > backend_engine::SourceLocation::MAX_PATH_BYTES
        || path.starts_with('/')
        || path.contains(['\\', '\0'])
        || path
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
        || (path.len() >= 2 && path.as_bytes()[1] == b':')
    {
        return Err(BuiltinModelError(
            "structural diff declaration has an invalid package-relative source path".to_owned(),
        ));
    }
    Ok(path)
}

fn structural_coordinate_parts<'row>(
    row: &'row backend_engine::Row,
    package_label: &str,
) -> Result<(&'row str, Option<&'row str>), BuiltinModelError> {
    let invalid = || {
        BuiltinModelError(
            "structural diff declaration coordinate disagrees with its source".to_owned(),
        )
    };
    let path = checked_source_path(row)?;
    let location = match &row.source {
        backend_library::SourceAvailability::Captured(location) => Some(location),
        // A stale file retains its admitted package-relative path but no
        // current line. The label still carries a canonical old line; line is
        // deliberately excluded from the structural comparison key.
        backend_library::SourceAvailability::StaleFile { .. } => None,
        backend_library::SourceAvailability::NotCaptured
        | backend_library::SourceAvailability::NotHydrated
        | backend_library::SourceAvailability::Unconfigured => return Err(invalid()),
    };
    let root = row.label.strip_prefix(package_label).ok_or_else(invalid)?;
    let root = root.strip_prefix("::").ok_or_else(invalid)?;
    if row.kind == Some(backend_engine::DeclarationKind::Module) && root == path {
        if location.is_some_and(|location| location.start_line() != 1) {
            return Err(invalid());
        }
        return Ok((path, None));
    }
    let file = root.strip_prefix(path).ok_or_else(invalid)?;
    let line_and_name = file.strip_prefix(':').ok_or_else(invalid)?;
    let (line, name) = line_and_name.split_once("::").ok_or_else(invalid)?;
    let line_number = line
        .parse::<u32>()
        .ok()
        .filter(|line_number| *line_number > 0)
        .ok_or_else(invalid)?;
    if line != line_number.to_string()
        || location.is_some_and(|location| line_number != location.start_line())
        || name.is_empty()
        || name.len() > backend_library::MAX_PRODUCT_TEXT_BYTES
        || name.contains('\0')
    {
        return Err(invalid());
    }
    Ok((path, Some(name)))
}

fn semantic_coordinate_parts<'row>(
    row: &'row backend_engine::Row,
    package_label: &str,
    package_key: backend_engine::PackageKey,
    preimage: &str,
) -> Result<
    (
        &'row str,
        &'row str,
        backend_semantic::ir::DeclarationIdentity,
    ),
    BuiltinModelError,
> {
    let invalid = || {
        BuiltinModelError(
            "structural diff semantic declaration failed canonical identity admission".to_owned(),
        )
    };
    let package_prefix = format!("{}::", backend_engine::encode_id(package_key.as_bytes()));
    let identity_hex = preimage.strip_prefix(&package_prefix).ok_or_else(invalid)?;
    let bytes = backend_library::decode_id(identity_hex).map_err(|_| invalid())?;
    if bytes.len() != 32
        || backend_engine::encode_id(&bytes) != identity_hex
        || row.id != backend_engine::RowId::Symbol(backend_engine::symbol_key(preimage))
    {
        return Err(invalid());
    }
    let mut family = [0_u8; 16];
    family.copy_from_slice(&bytes[..16]);
    let mut variant = [0_u8; 16];
    variant.copy_from_slice(&bytes[16..]);
    let identity = backend_semantic::ir::DeclarationIdentity {
        family: backend_semantic::ir::DeclarationFamilyId::from_raw(family),
        variant: backend_semantic::ir::VariantFingerprint::from_raw(variant),
    };
    let label_suffix = row
        .label
        .strip_prefix(package_label)
        .and_then(|suffix| suffix.strip_prefix("::semantic::"))
        .ok_or_else(invalid)?;
    let (label_identity, name) = label_suffix.split_once("::").ok_or_else(invalid)?;
    if label_identity != identity_hex
        || name.is_empty()
        || name.len() > backend_library::MAX_PRODUCT_TEXT_BYTES
        || name.contains('\0')
    {
        return Err(invalid());
    }
    Ok((checked_source_path(row)?, name, identity))
}

fn structural_family_identity(
    coordinate: StructuralCoordinate<'_>,
) -> backend_semantic::ir::DeclarationFamilyId {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"nudox.structural-diff.family.v1\0");
    hash_structural_bytes(&mut hasher, coordinate.path.as_bytes());
    hasher.update(&[coordinate.kind.wire_tag()]);
    match coordinate.name {
        None => {
            hasher.update(&[0]);
        }
        Some(name) => {
            hasher.update(&[1]);
            hash_structural_bytes(&mut hasher, name.as_bytes());
        }
    }
    match coordinate.semantic_family {
        None => {
            hasher.update(&[0]);
        }
        Some(family) => {
            hasher.update(&[1]);
            hasher.update(family.as_bytes());
        }
    };
    let digest = hasher.finalize();
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest.as_bytes()[..16]);
    backend_semantic::ir::DeclarationFamilyId::from_raw(bytes)
}

fn structural_declaration_fingerprint(admitted: &AdmittedStructuralRow<'_>) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"nudox.structural-diff.variant.v1\0");
    match admitted.row.signature.as_deref() {
        None => {
            hasher.update(&[0]);
        }
        Some(signature) => {
            hasher.update(&[1]);
            hash_structural_bytes(&mut hasher, signature.as_bytes());
        }
    };
    match admitted.semantic_variant {
        None => {
            hasher.update(&[0]);
        }
        Some(variant) => {
            hasher.update(&[1]);
            hasher.update(variant.as_bytes());
        }
    };
    hasher.update(&(admitted.row.document.len() as u64).to_le_bytes());
    for fragment in admitted.row.document.iter() {
        match fragment {
            backend_library::Fragment::Text(text) => {
                hasher.update(&[0]);
                hash_structural_bytes(&mut hasher, text.as_bytes());
            }
            backend_library::Fragment::Code(text) => {
                hasher.update(&[1]);
                hash_structural_bytes(&mut hasher, text.as_bytes());
            }
            backend_library::Fragment::Link { label, .. } => {
                hasher.update(&[2]);
                hash_structural_bytes(&mut hasher, label.as_bytes());
                // Raw link targets may be rooted or foreign. This facade
                // preserves their display text but does not claim target
                // identity when the semantic diff is unavailable.
            }
            backend_library::Fragment::Break => {
                hasher.update(&[3]);
            }
        };
    }
    *hasher.finalize().as_bytes()
}

fn hash_structural_bytes(hasher: &mut blake3::Hasher, bytes: &[u8]) {
    hasher.update(&(bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
}

fn structural_identity(
    coordinate: StructuralCoordinate<'_>,
    fingerprint: [u8; 32],
) -> backend_semantic::ir::DeclarationIdentity {
    let mut variant = [0_u8; 16];
    variant.copy_from_slice(&fingerprint[..16]);
    backend_semantic::ir::DeclarationIdentity {
        family: structural_family_identity(coordinate),
        variant: backend_semantic::ir::VariantFingerprint::from_raw(variant),
    }
}

fn append_structural_row<'row>(
    row: &'row backend_engine::Row,
    package_label: &str,
    package_key: backend_engine::PackageKey,
    declarations: &mut StructuralDeclarations<'row>,
) -> Result<(), BuiltinModelError> {
    let Some(admitted) = admitted_structural_row(row, package_label, package_key)? else {
        return Ok(());
    };
    let fingerprint = structural_declaration_fingerprint(&admitted);
    let identity = structural_identity(admitted.coordinate, fingerprint);
    let count = &mut declarations
        .entry(admitted.coordinate)
        .or_default()
        .entry(fingerprint)
        .or_insert(StructuralOccurrence { identity, count: 0 })
        .count;
    *count = count.checked_add(1).ok_or_else(|| {
        BuiltinModelError("structural diff occurrence count overflowed".to_owned())
    })?;
    Ok(())
}

fn structural_declarations_from_rows<'row>(
    rows: impl IntoIterator<Item = &'row backend_engine::Row>,
    package_label: &str,
    package_key: backend_engine::PackageKey,
) -> Result<StructuralDeclarations<'row>, BuiltinModelError> {
    let mut declarations = BTreeMap::new();
    for row in rows {
        append_structural_row(row, package_label, package_key, &mut declarations)?;
    }
    Ok(declarations)
}

fn structural_package_declarations<'view>(
    daemon: &'view crate::Locald<BuiltinModel, BuiltinValidator, BuiltinAuthorityVerifier>,
    package: &backend_engine::PackageReference,
) -> Result<StructuralDeclarations<'view>, BuiltinModelError> {
    let view = daemon.engine().daemon().library().view();
    let package_key = backend_engine::package_key(package.as_str());
    if view
        .row_ref(backend_engine::RowId::Package(package_key))
        .filter(|row| {
            row.label == package.as_str() && row.id == backend_engine::RowId::Package(package_key)
        })
        .is_none()
    {
        return Err(BuiltinModelError("diff package is not indexed".to_owned()));
    }
    structural_declarations_from_rows(view.row_refs(), package.as_str(), package_key)
}

fn structural_package_diff(
    daemon: &crate::Locald<BuiltinModel, BuiltinValidator, BuiltinAuthorityVerifier>,
    from: &backend_engine::PackageReference,
    to: &backend_engine::PackageReference,
) -> Result<Box<[backend_engine::DiffRecord]>, BuiltinModelError> {
    let before = structural_package_declarations(daemon, from)?;
    let after = structural_package_declarations(daemon, to)?;
    diff_structural_declarations(&before, &after)
}

fn diff_structural_declarations(
    before: &StructuralDeclarations<'_>,
    after: &StructuralDeclarations<'_>,
) -> Result<Box<[backend_engine::DiffRecord]>, BuiltinModelError> {
    let mut rows = Vec::new();
    let mut left = before.iter().peekable();
    let mut right = after.iter().peekable();
    while left.peek().is_some() || right.peek().is_some() {
        match (left.peek(), right.peek()) {
            (Some((old_coordinate, old_variants)), Some((new_coordinate, new_variants))) => {
                match old_coordinate.cmp(new_coordinate) {
                    std::cmp::Ordering::Less => {
                        diff_structural_group(
                            **old_coordinate,
                            Some(*old_variants),
                            None,
                            &mut rows,
                        )?;
                        left.next();
                    }
                    std::cmp::Ordering::Greater => {
                        diff_structural_group(
                            **new_coordinate,
                            None,
                            Some(*new_variants),
                            &mut rows,
                        )?;
                        right.next();
                    }
                    std::cmp::Ordering::Equal => {
                        diff_structural_group(
                            **old_coordinate,
                            Some(*old_variants),
                            Some(*new_variants),
                            &mut rows,
                        )?;
                        left.next();
                        right.next();
                    }
                }
            }
            (Some((coordinate, variants)), None) => {
                diff_structural_group(**coordinate, Some(*variants), None, &mut rows)?;
                left.next();
            }
            (None, Some((coordinate, variants))) => {
                diff_structural_group(**coordinate, None, Some(*variants), &mut rows)?;
                right.next();
            }
            (None, None) => break,
        }
    }
    rows.sort_by(|left, right| {
        left.label.cmp(&right.label).then_with(|| {
            declaration_change_order(left.change).cmp(&declaration_change_order(right.change))
        })
    });
    Ok(rows.into_boxed_slice())
}

fn structural_coordinate_label(coordinate: StructuralCoordinate<'_>) -> String {
    match coordinate.name {
        Some(name) => format!("{}::{}", coordinate.path, name),
        None => coordinate.path.to_owned(),
    }
}

fn diff_structural_group(
    coordinate: StructuralCoordinate<'_>,
    older: Option<&StructuralVariantMap>,
    newer: Option<&StructuralVariantMap>,
    rows: &mut Vec<backend_engine::DiffRecord>,
) -> Result<(), BuiltinModelError> {
    let (removed, added, removed_identity, added_identity) =
        structural_residual_counts(older, newer)?;
    if removed == 0 && added == 0 {
        return Ok(());
    }
    let label = structural_coordinate_label(coordinate);
    if removed == 1 && added == 1 {
        return push_diff(
            rows,
            &label,
            backend_engine::DeclarationChange::Changed,
            removed_identity,
            added_identity,
        );
    }
    append_structural_residuals(older, newer, &label, rows)
}

fn structural_residual_counts(
    older: Option<&StructuralVariantMap>,
    newer: Option<&StructuralVariantMap>,
) -> Result<
    (
        usize,
        usize,
        Option<backend_semantic::ir::DeclarationIdentity>,
        Option<backend_semantic::ir::DeclarationIdentity>,
    ),
    BuiltinModelError,
> {
    let mut removed = 0_usize;
    let mut added = 0_usize;
    let mut removed_identity = None;
    let mut added_identity = None;
    walk_structural_variant_deltas(older, newer, |old, new| {
        let old_count = old.map_or(0, |summary| summary.count);
        let new_count = new.map_or(0, |summary| summary.count);
        if old_count > new_count {
            removed = removed.checked_add(old_count - new_count).ok_or_else(|| {
                BuiltinModelError("structural diff occurrence count overflowed".to_owned())
            })?;
            removed_identity = old.map(|summary| summary.identity);
        } else if new_count > old_count {
            added = added.checked_add(new_count - old_count).ok_or_else(|| {
                BuiltinModelError("structural diff occurrence count overflowed".to_owned())
            })?;
            added_identity = new.map(|summary| summary.identity);
        }
        Ok(())
    })?;
    Ok((removed, added, removed_identity, added_identity))
}

fn append_structural_residuals(
    older: Option<&StructuralVariantMap>,
    newer: Option<&StructuralVariantMap>,
    label: &str,
    rows: &mut Vec<backend_engine::DiffRecord>,
) -> Result<(), BuiltinModelError> {
    walk_structural_variant_deltas(older, newer, |old, new| {
        let old_count = old.map_or(0, |summary| summary.count);
        let new_count = new.map_or(0, |summary| summary.count);
        if old_count > new_count {
            for _ in 0..old_count - new_count {
                push_diff(
                    rows,
                    label,
                    backend_engine::DeclarationChange::Removed,
                    old.map(|summary| summary.identity),
                    None,
                )?;
            }
        } else if new_count > old_count {
            for _ in 0..new_count - old_count {
                push_diff(
                    rows,
                    label,
                    backend_engine::DeclarationChange::Added,
                    None,
                    new.map(|summary| summary.identity),
                )?;
            }
        }
        Ok(())
    })
}

fn walk_structural_variant_deltas(
    older: Option<&StructuralVariantMap>,
    newer: Option<&StructuralVariantMap>,
    mut visit: impl FnMut(
        Option<&StructuralOccurrence>,
        Option<&StructuralOccurrence>,
    ) -> Result<(), BuiltinModelError>,
) -> Result<(), BuiltinModelError> {
    let mut left = older
        .into_iter()
        .flat_map(|variants| variants.iter())
        .peekable();
    let mut right = newer
        .into_iter()
        .flat_map(|variants| variants.iter())
        .peekable();
    while left.peek().is_some() || right.peek().is_some() {
        match (left.peek().copied(), right.peek().copied()) {
            (Some((old_fingerprint, old)), Some((new_fingerprint, new))) => {
                match old_fingerprint.cmp(new_fingerprint) {
                    std::cmp::Ordering::Less => {
                        visit(Some(old), None)?;
                        left.next();
                    }
                    std::cmp::Ordering::Greater => {
                        visit(None, Some(new))?;
                        right.next();
                    }
                    std::cmp::Ordering::Equal => {
                        visit(Some(old), Some(new))?;
                        left.next();
                        right.next();
                    }
                }
            }
            (Some((_, old)), None) => {
                visit(Some(old), None)?;
                left.next();
            }
            (None, Some((_, new))) => {
                visit(None, Some(new))?;
                right.next();
            }
            (None, None) => break,
        }
    }
    Ok(())
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

    fn structural_fixture_row(
        basis: backend_engine::Basis,
        package_label: &str,
        path: &str,
        line: u32,
        name: &str,
        kind: backend_engine::DeclarationKind,
        signature: Option<&str>,
        documentation: &str,
        occurrence: Option<u32>,
    ) -> backend_engine::Row {
        let package = backend_engine::package_key(package_label);
        let coordinate = format!("{package_label}::{path}:{line}::{name}");
        let (id, preimage) = match occurrence {
            None => (backend_engine::symbol_key(&coordinate), None),
            Some(occurrence) => {
                let preimage = format!(
                    "{coordinate}\0{}\0{}\0{occurrence}",
                    kind.name(),
                    signature.unwrap_or_default()
                );
                (backend_engine::symbol_key(&preimage), Some(preimage))
            }
        };
        let mut row = backend_engine::Row::in_package(
            backend_engine::RowId::Symbol(id),
            basis,
            package,
            coordinate,
        )
        .with_kind(kind)
        .with_source(backend_engine::SourceLocation::new(path, line).expect("source location"))
        .with_document(vec![backend_library::Fragment::Text(
            documentation.to_owned(),
        )]);
        if let Some(signature) = signature {
            row = row.with_signature(signature);
        }
        if let Some(preimage) = preimage {
            row = row
                .try_with_identity_preimage(&preimage)
                .expect("identity preimage");
        }
        row
    }

    fn semantic_fixture_row(
        basis: backend_engine::Basis,
        package_label: &str,
        path: &str,
        line: u32,
        name: &str,
        kind: backend_engine::DeclarationKind,
        family: [u8; 16],
        variant: [u8; 16],
    ) -> backend_engine::Row {
        let package = backend_engine::package_key(package_label);
        let mut identity_bytes = [0_u8; 32];
        identity_bytes[..16].copy_from_slice(&family);
        identity_bytes[16..].copy_from_slice(&variant);
        let identity_hex = backend_engine::encode_id(&identity_bytes);
        let preimage = format!(
            "{}::{identity_hex}",
            backend_engine::encode_id(package.as_bytes())
        );
        let label = format!("{package_label}::semantic::{identity_hex}::{name}");
        backend_engine::Row::in_package(
            backend_engine::RowId::Symbol(backend_engine::symbol_key(&preimage)),
            basis,
            package,
            label,
        )
        .with_kind(kind)
        .with_source(backend_engine::SourceLocation::new(path, line).expect("source location"))
        .with_signature(format!("{name}()"))
        .with_document(vec![backend_library::Fragment::Text(
            "semantic docs".to_owned(),
        )])
        .try_with_identity_preimage(&preimage)
        .expect("semantic preimage")
    }

    fn structural_fixture_declarations<'row>(
        rows: &'row [backend_engine::Row],
        package_label: &str,
    ) -> Result<StructuralDeclarations<'row>, BuiltinModelError> {
        let package = backend_engine::package_key(package_label);
        structural_declarations_from_rows(rows.iter(), package_label, package)
    }

    fn structural_fixture_basis() -> backend_engine::Basis {
        crate::builtin::initial_view()
            .expect("initial view")
            .0
            .basis()
    }

    #[test]
    fn structural_multiset_is_root_and_line_independent_and_keeps_homonyms() {
        let basis = structural_fixture_basis();
        let root_a = "/private/project-a";
        let root_b = "/private/project-b";
        let package_a = [
            structural_fixture_row(
                basis,
                root_a,
                "src/first.ts",
                10,
                "status",
                backend_engine::DeclarationKind::Property,
                Some("status: string"),
                "current status",
                None,
            ),
            structural_fixture_row(
                basis,
                root_a,
                "src/second.ts",
                24,
                "status",
                backend_engine::DeclarationKind::Property,
                Some("status: number"),
                "another status",
                None,
            ),
        ];
        let package_b = [
            structural_fixture_row(
                basis,
                root_b,
                "src/first.ts",
                88,
                "status",
                backend_engine::DeclarationKind::Property,
                Some("status: string"),
                "current status",
                None,
            ),
            structural_fixture_row(
                basis,
                root_b,
                "src/second.ts",
                24,
                "status",
                backend_engine::DeclarationKind::Property,
                Some("status: number"),
                "another status",
                None,
            ),
        ];
        let before = structural_fixture_declarations(&package_a, root_a).expect("before");
        let after = structural_fixture_declarations(&package_b, root_b).expect("after");
        assert_eq!(
            before.len(),
            2,
            "same terminal name in two files is two families"
        );
        assert!(
            diff_structural_declarations(&before, &after)
                .expect("cross-root and moved-line diff")
                .is_empty()
        );

        let homonyms = structural_fixture_declarations(&package_a, root_a).expect("homonyms");
        assert!(
            diff_structural_declarations(&homonyms, &homonyms)
                .expect("same-snapshot homonym diff")
                .is_empty()
        );
    }

    #[test]
    fn structural_file_module_uses_exact_package_relative_path_coordinate() {
        let basis = structural_fixture_basis();
        let package = "/private/module-project";
        let path = "src/frontend/useFindReplace.ts";
        let label = format!("{package}::{path}");
        let row = backend_engine::Row::in_package(
            backend_engine::RowId::Symbol(backend_engine::symbol_key(&label)),
            basis,
            backend_engine::package_key(package),
            label.clone(),
        )
        .with_kind(backend_engine::DeclarationKind::Module)
        .with_source(
            backend_engine::SourceLocation::new(path, 1).expect("file module source location"),
        );
        let admitted = admitted_structural_row(&row, package, backend_engine::package_key(package))
            .expect("file module identity admission")
            .expect("package row is a declaration");
        assert_eq!(admitted.coordinate.path, path);
        assert_eq!(admitted.coordinate.name, None);
        assert_eq!(structural_coordinate_label(admitted.coordinate), path);

        let wrong_line = row.clone().with_source(
            backend_engine::SourceLocation::new(path, 2).expect("non-module-start source location"),
        );
        assert!(
            admitted_structural_row(&wrong_line, package, backend_engine::package_key(package),)
                .is_err()
        );

        let wrong_kind = row
            .clone()
            .with_kind(backend_engine::DeclarationKind::Function);
        assert!(
            admitted_structural_row(&wrong_kind, package, backend_engine::package_key(package),)
                .is_err()
        );

        let mut foreign_path_prefix = row;
        foreign_path_prefix.label = format!("{label}-foreign");
        foreign_path_prefix.id =
            backend_engine::RowId::Symbol(backend_engine::symbol_key(&foreign_path_prefix.label));
        assert!(
            admitted_structural_row(
                &foreign_path_prefix,
                package,
                backend_engine::package_key(package),
            )
            .is_err()
        );
    }

    #[test]
    fn structural_coordinate_uses_typed_path_when_source_line_is_stale() {
        let basis = structural_fixture_basis();
        let package = "/private/stale-project";
        let mut stale = structural_fixture_row(
            basis,
            package,
            "src/api.ts",
            14,
            "run",
            backend_engine::DeclarationKind::Function,
            Some("run()"),
            "docs",
            None,
        );
        stale.source = backend_library::SourceAvailability::stale_file("src/api.ts")
            .expect("bounded stale package-relative path");
        let admitted =
            admitted_structural_row(&stale, package, backend_engine::package_key(package))
                .expect("stale source keeps path evidence")
                .expect("stale declaration is admitted");
        assert_eq!(admitted.coordinate.path, "src/api.ts");
        assert_eq!(admitted.coordinate.name, Some("run"));

        for source in [
            backend_library::SourceAvailability::NotCaptured,
            backend_library::SourceAvailability::NotHydrated,
            backend_library::SourceAvailability::Unconfigured,
        ] {
            let mut unavailable = stale.clone();
            unavailable.source = source;
            assert!(
                admitted_structural_row(
                    &unavailable,
                    package,
                    backend_engine::package_key(package),
                )
                .is_err()
            );
        }
    }

    #[test]
    fn structural_duplicate_overloads_preserve_counts_and_only_pair_single_residuals() {
        let basis = structural_fixture_basis();
        let package = "/private/overload-project";
        let rows = [
            structural_fixture_row(
                basis,
                package,
                "src/api.ts",
                6,
                "send",
                backend_engine::DeclarationKind::Function,
                Some("send(number)"),
                "send a value",
                Some(0),
            ),
            structural_fixture_row(
                basis,
                package,
                "src/api.ts",
                6,
                "send",
                backend_engine::DeclarationKind::Function,
                Some("send(number)"),
                "send a value",
                Some(1),
            ),
            structural_fixture_row(
                basis,
                package,
                "src/api.ts",
                6,
                "send",
                backend_engine::DeclarationKind::Function,
                Some("send(date)"),
                "send a value",
                Some(0),
            ),
        ];
        let after_rows = [
            rows[0].clone(),
            rows[2].clone(),
            structural_fixture_row(
                basis,
                package,
                "src/api.ts",
                6,
                "send",
                backend_engine::DeclarationKind::Function,
                Some("send(boolean)"),
                "send a value",
                Some(0),
            ),
        ];
        let before = structural_fixture_declarations(&rows, package).expect("before overloads");
        let after = structural_fixture_declarations(&after_rows, package).expect("after overloads");
        assert!(
            diff_structural_declarations(&before, &before)
                .expect("repeated same-snapshot diff")
                .is_empty()
        );
        let reversed_before_rows = rows.iter().rev().cloned().collect::<Vec<_>>();
        let reversed_before = structural_fixture_declarations(&reversed_before_rows, package)
            .expect("reverse before");
        assert!(
            diff_structural_declarations(&before, &reversed_before)
                .expect("input-order-independent self-diff")
                .is_empty()
        );
        let changed = diff_structural_declarations(&before, &after).expect("one residual pair");
        assert_eq!(changed.len(), 1);
        assert_eq!(
            changed[0].change,
            backend_engine::DeclarationChange::Changed
        );

        let reversed_rows = after_rows.into_iter().rev().collect::<Vec<_>>();
        let reversed =
            structural_fixture_declarations(&reversed_rows, package).expect("reversed overloads");
        assert_eq!(
            diff_structural_declarations(&before, &reversed).expect("order stable"),
            changed
        );

        let ambiguous_before_rows = [rows[0].clone(), rows[2].clone()];
        let ambiguous_before = structural_fixture_declarations(&ambiguous_before_rows, package)
            .expect("two overloads");
        let ambiguous_after_rows = [
            structural_fixture_row(
                basis,
                package,
                "src/api.ts",
                6,
                "send",
                backend_engine::DeclarationKind::Function,
                Some("send(string)"),
                "send a value",
                Some(0),
            ),
            structural_fixture_row(
                basis,
                package,
                "src/api.ts",
                6,
                "send",
                backend_engine::DeclarationKind::Function,
                Some("send(boolean)"),
                "send a value",
                Some(0),
            ),
        ];
        let ambiguous_after = structural_fixture_declarations(&ambiguous_after_rows, package)
            .expect("ambiguous variants");
        let ambiguous = diff_structural_declarations(&ambiguous_before, &ambiguous_after)
            .expect("ambiguous residuals");
        assert_eq!(ambiguous.len(), 4);
        assert_eq!(
            ambiguous
                .iter()
                .filter(|row| row.change == backend_engine::DeclarationChange::Changed)
                .count(),
            0
        );
    }

    #[test]
    fn structural_signature_document_and_occurrence_changes_keep_multiset_evidence() {
        let basis = structural_fixture_basis();
        let package = "/private/evidence-project";
        let before_rows = [
            structural_fixture_row(
                basis,
                package,
                "src/api.ts",
                12,
                "run",
                backend_engine::DeclarationKind::Function,
                Some("run()"),
                "old docs",
                Some(0),
            ),
            structural_fixture_row(
                basis,
                package,
                "src/api.ts",
                12,
                "run",
                backend_engine::DeclarationKind::Function,
                Some("run()"),
                "old docs",
                Some(1),
            ),
        ];
        let after_rows = [structural_fixture_row(
            basis,
            package,
            "src/api.ts",
            12,
            "run",
            backend_engine::DeclarationKind::Function,
            Some("run()"),
            "old docs",
            Some(0),
        )];
        let before = structural_fixture_declarations(&before_rows, package).expect("before");
        let after = structural_fixture_declarations(&after_rows, package).expect("after");
        let result = diff_structural_declarations(&before, &after).expect("duplicate removal");
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].change, backend_engine::DeclarationChange::Removed);

        let added_rows = [
            before_rows[0].clone(),
            before_rows[1].clone(),
            structural_fixture_row(
                basis,
                package,
                "src/api.ts",
                12,
                "run",
                backend_engine::DeclarationKind::Function,
                Some("run()"),
                "old docs",
                Some(2),
            ),
        ];
        let added = structural_fixture_declarations(&added_rows, package).expect("added repeat");
        let original =
            structural_fixture_declarations(&before_rows, package).expect("original repeats");
        let growth =
            diff_structural_declarations(&original, &added).expect("one repeated addition");
        assert_eq!(growth.len(), 1);
        assert_eq!(growth[0].change, backend_engine::DeclarationChange::Added);

        let old_docs_rows = [structural_fixture_row(
            basis,
            package,
            "src/api.ts",
            12,
            "documented",
            backend_engine::DeclarationKind::Function,
            Some("documented()"),
            "old docs",
            None,
        )];
        let old_docs = structural_fixture_declarations(&old_docs_rows, package).expect("old docs");
        let new_docs_rows = [structural_fixture_row(
            basis,
            package,
            "src/api.ts",
            12,
            "documented",
            backend_engine::DeclarationKind::Function,
            Some("documented()"),
            "new docs",
            None,
        )];
        let new_docs = structural_fixture_declarations(&new_docs_rows, package).expect("new docs");
        let documentation_change =
            diff_structural_declarations(&old_docs, &new_docs).expect("documentation change");
        assert_eq!(documentation_change.len(), 1);
        assert_eq!(
            documentation_change[0].change,
            backend_engine::DeclarationChange::Changed
        );

        let old_signature_rows = [structural_fixture_row(
            basis,
            package,
            "src/api.ts",
            12,
            "optional",
            backend_engine::DeclarationKind::Function,
            None,
            "same docs",
            None,
        )];
        let old_signature = structural_fixture_declarations(&old_signature_rows, package)
            .expect("absent signature");
        let new_signature_rows = [structural_fixture_row(
            basis,
            package,
            "src/api.ts",
            12,
            "optional",
            backend_engine::DeclarationKind::Function,
            Some(""),
            "same docs",
            None,
        )];
        let new_signature =
            structural_fixture_declarations(&new_signature_rows, package).expect("empty signature");
        let signature_change = diff_structural_declarations(&old_signature, &new_signature)
            .expect("None differs from Some");
        assert_eq!(signature_change.len(), 1);
        assert_eq!(
            signature_change[0].change,
            backend_engine::DeclarationChange::Changed
        );

        let moved_path_rows = [structural_fixture_row(
            basis,
            package,
            "src/new-api.ts",
            12,
            "optional",
            backend_engine::DeclarationKind::Function,
            None,
            "same docs",
            None,
        )];
        let moved_path =
            structural_fixture_declarations(&moved_path_rows, package).expect("moved path");
        let move_change = diff_structural_declarations(&old_signature, &moved_path)
            .expect("path move is remove/add");
        assert_eq!(move_change.len(), 2);
        assert!(
            move_change
                .iter()
                .any(|row| row.change == backend_engine::DeclarationChange::Added)
        );
        assert!(
            move_change
                .iter()
                .any(|row| row.change == backend_engine::DeclarationChange::Removed)
        );
    }

    #[test]
    fn semantic_fallback_rows_keep_typed_family_and_variant_multisets() {
        let basis = structural_fixture_basis();
        let root_a = "/private/semantic-a";
        let root_b = "/private/semantic-b";
        let family_a = [0x31; 16];
        let family_b = [0x32; 16];
        let first = semantic_fixture_row(
            basis,
            root_a,
            "src/module.py",
            8,
            "load",
            backend_engine::DeclarationKind::Function,
            family_a,
            [0x41; 16],
        );
        let moved_same = semantic_fixture_row(
            basis,
            root_b,
            "src/module.py",
            80,
            "load",
            backend_engine::DeclarationKind::Function,
            family_a,
            [0x41; 16],
        );
        let before = structural_fixture_declarations(std::slice::from_ref(&first), root_a)
            .expect("first family");
        let same = structural_fixture_declarations(std::slice::from_ref(&moved_same), root_b)
            .expect("same family");
        assert!(
            diff_structural_declarations(&before, &same)
                .expect("root and line independent semantic row")
                .is_empty()
        );

        let changed_variant_rows = [semantic_fixture_row(
            basis,
            root_b,
            "src/module.py",
            80,
            "load",
            backend_engine::DeclarationKind::Function,
            family_a,
            [0x42; 16],
        )];
        let changed_variant = structural_fixture_declarations(&changed_variant_rows, root_b)
            .expect("changed semantic variant");
        let changed = diff_structural_declarations(&before, &changed_variant)
            .expect("semantic variant change");
        assert_eq!(changed.len(), 1);
        assert_eq!(
            changed[0].change,
            backend_engine::DeclarationChange::Changed
        );

        let homonymous_family_rows = [semantic_fixture_row(
            basis,
            root_a,
            "src/module.py",
            8,
            "load",
            backend_engine::DeclarationKind::Function,
            family_b,
            [0x41; 16],
        )];
        let homonymous_family = structural_fixture_declarations(&homonymous_family_rows, root_a)
            .expect("second semantic family");
        let distinct = diff_structural_declarations(&before, &homonymous_family)
            .expect("different semantic families");
        assert_eq!(distinct.len(), 2);
        assert!(
            distinct
                .iter()
                .any(|row| row.change == backend_engine::DeclarationChange::Added)
        );
        assert!(
            distinct
                .iter()
                .any(|row| row.change == backend_engine::DeclarationChange::Removed)
        );
    }

    #[test]
    fn structural_foreign_prefix_malformed_coordinates_and_duplicate_signature_are_rejected() {
        let basis = structural_fixture_basis();
        let package = "/private/admission-project";
        let mut foreign = structural_fixture_row(
            basis,
            package,
            "src/api.ts",
            3,
            "run",
            backend_engine::DeclarationKind::Function,
            Some("run()"),
            "docs",
            None,
        );
        foreign.label = format!("{package}-foreign::src/api.ts:3::run");
        foreign.id = backend_engine::RowId::Symbol(backend_engine::symbol_key(&foreign.label));
        assert!(
            admitted_structural_row(&foreign, package, backend_engine::package_key(package))
                .is_err()
        );

        let mut malformed = structural_fixture_row(
            basis,
            package,
            "src/api.ts",
            3,
            "run",
            backend_engine::DeclarationKind::Function,
            Some("run()"),
            "docs",
            None,
        );
        malformed.label = format!("{package}::src/other.ts:3::run");
        malformed.id = backend_engine::RowId::Symbol(backend_engine::symbol_key(&malformed.label));
        assert!(
            admitted_structural_row(&malformed, package, backend_engine::package_key(package))
                .is_err()
        );

        // The production builder always supplies a signature string before it
        // creates a duplicate-coordinate preimage. A None signature in that
        // preimage is therefore malformed; None vs Some("") remains distinct
        // for the ordinary non-preimage fallback rows above.
        let duplicate_without_signature = structural_fixture_row(
            basis,
            package,
            "src/api.ts",
            3,
            "run",
            backend_engine::DeclarationKind::Function,
            None,
            "docs",
            Some(0),
        );
        assert!(
            admitted_structural_row(
                &duplicate_without_signature,
                package,
                backend_engine::package_key(package)
            )
            .is_err()
        );
    }

    #[test]
    fn dump_admitted_docs_structural_collision() -> Result<(), String> {
        let Some(workspace) = std::env::var_os("NUDOX_DOCS_DIAG_WORKSPACE") else {
            return Ok(());
        };
        let workspace = std::path::PathBuf::from(workspace);
        let package_label = std::env::var("NUDOX_DOCS_DIAG_PACKAGE")
            .map_err(|error| format!("NUDOX_DOCS_DIAG_PACKAGE: {error}"))?;
        let profile = crate::builtin::profile_descriptor(crate::builtin::BuiltinProfile::Product)?;
        let authority_secret =
            backend_engine::read_authority_secret(&workspace.join("authority.secret"))
                .map_err(|error| error.to_string())?;
        let dispatcher = crate::builtin::builtin_dispatcher(
            Some(authority_secret),
            std::sync::Arc::clone(&profile),
            60_000,
        )?;
        let mut daemon = crate::Locald::open_with_dispatcher_and_registry(
            &workspace,
            crate::builtin::BuiltinModel,
            crate::builtin::genesis().map_err(|error| error.to_string())?,
            dispatcher,
            backend_engine::DaemonConfig::default(),
            crate::builtin::product_relation_registry().map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        let owner = daemon.engine().daemon().owner();
        let snapshot = owner.snapshot();
        let capability = crate::builtin::builtin_view_capability_for_workspace(&snapshot)
            .map_err(|error| error.to_string())?;
        let workspace_root = owner.head().root();
        let journal =
            crate::builtin::view_journal::ViewJournal::open(workspace.join("view.journal"))?;
        let recovered = journal
            .load_for_workspace(workspace_root, &capability)?
            .ok_or_else(|| {
                "production view journal had no matching admitted snapshot".to_owned()
            })?;
        let admission = crate::builtin::BuiltinViewAdmission {
            workspace_root,
            source_root: recovered.view.basis().root,
        };
        daemon
            .engine_mut()
            .daemon_mut()
            .set_view_persistence(Box::new(journal));
        daemon
            .engine_mut()
            .daemon_mut()
            .restore_view(
                recovered.view,
                recovered.cursor,
                &admission,
                recovered.events,
                recovered.base_sequence,
            )
            .map_err(|error| error.to_string())?;
        let view = daemon.engine().daemon().library().view();
        let package = backend_engine::package_key(&package_label);
        let package_row = view
            .row_ref(backend_engine::RowId::Package(package))
            .filter(|row| row.label == package_label)
            .ok_or_else(|| format!("admitted package row absent: {package_label}"))?;
        eprintln!(
            "DOCS_DIAG root={} package_label={:?} package_row_id={:?} workspace_root={:?} view_root={:?} rows={}",
            workspace.display(),
            package_row.label,
            package_row.id,
            workspace_root,
            view.root(),
            view.row_count(),
        );

        #[derive(Clone, Debug)]
        struct RowEvidence {
            label: String,
            id: backend_engine::RowId,
            preimage: Option<String>,
            kind: String,
            source: Option<(String, u32)>,
        }
        let capture = |row: &backend_engine::Row| RowEvidence {
            label: row.label.clone(),
            id: row.id,
            preimage: row
                .identity_preimage()
                .map(|preimage| preimage.as_str().to_owned()),
            kind: format!("{:?}", row.kind),
            source: row
                .source
                .captured()
                .map(|location| (location.path().to_owned(), location.start_line())),
        };
        let expected_sites = [
            ("src/yhub-server/src/backend.ts", 150_u32),
            ("src/yhub-server/__tests__/metrics.spec.ts", 241_u32),
        ];
        let mut docs_rows = Vec::new();
        let mut cursor = backend_engine::ViewPageCursor::first(&view);
        loop {
            let page = view
                .page(cursor, backend_engine::MAX_SNAPSHOT_PAGE_ROWS)
                .map_err(|error| format!("page admitted Docs view: {error:?}"))?;
            for row in page.rows() {
                if row.package != Some(package) || row.kind.is_none() {
                    continue;
                }
                let name = row
                    .label
                    .rsplit("::")
                    .next()
                    .filter(|name| !name.is_empty())
                    .ok_or_else(|| "declaration row had no terminal name".to_owned())?
                    .to_owned();
                let current = capture(row);
                if name == "status"
                    && current.source.as_ref().is_some_and(|source| {
                        expected_sites
                            .iter()
                            .any(|expected| *expected == (source.0.as_str(), source.1))
                    })
                {
                    docs_rows.push(current);
                }
            }
            let Some(next) = page.next() else {
                break;
            };
            cursor = next;
        }
        let mut before = None;
        let mut after = None;
        for row in docs_rows {
            match row
                .source
                .as_ref()
                .map(|source| (source.0.as_str(), source.1))
            {
                Some(("src/yhub-server/src/backend.ts", 150)) => before = Some(row),
                Some(("src/yhub-server/__tests__/metrics.spec.ts", 241)) => after = Some(row),
                _ => {}
            }
        }
        let before = before.ok_or_else(|| {
            "admitted Docs image omitted the expected backend.ts status row".to_owned()
        })?;
        let after = after.ok_or_else(|| {
            "admitted Docs image omitted the expected metrics.spec.ts status row".to_owned()
        })?;
        if before.id == after.id
            || before.preimage.is_some()
            || after.preimage.is_some()
            || before.kind != "Some(Property)"
            || after.kind != "Some(Property)"
        {
            return Err(
                "admitted Docs status homonyms did not match canonical row controls".to_owned(),
            );
        }
        eprintln!("DOCS_DIAG duplicate_terminal_name=\"status\"");
        eprintln!("DOCS_DIAG before={before:#?}");
        eprintln!("DOCS_DIAG after={after:#?}");
        for row in view
            .row_refs()
            .filter(|row| row.package == Some(package) && row.kind.is_some())
        {
            if let Err(error) = admitted_structural_row(row, &package_label, package) {
                eprintln!(
                    "DOCS_DIAG first_rejected_row label={:?} id={:?} preimage={:?} kind={:?} source={:?} error={error}",
                    row.label,
                    row.id,
                    row.identity_preimage().map(|preimage| preimage.as_str()),
                    row.kind,
                    row.source,
                );
                break;
            }
        }
        let reference = backend_engine::PackageReference::parse(package_label)
            .map_err(|error| error.to_string())?;
        let rows = structural_package_diff(&daemon, &reference, &reference)
            .map_err(|error| format!("production structural self-diff: {error}"))?;
        if !rows.is_empty() {
            return Err(format!(
                "production structural self-diff returned {} rows",
                rows.len()
            ));
        }
        eprintln!("DOCS_DIAG same_root_self_diff=empty");
        Ok(())
    }
}
