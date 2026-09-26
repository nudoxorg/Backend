use super::super::{
    BuiltinAuthorityVerifier, BuiltinIntent, BuiltinModel, BuiltinModelError,
    BuiltinSemanticChange, BuiltinSemanticRelation, BuiltinSourceChange, BuiltinValidator,
    BuiltinWorkspaceRelation, Command, CommandReply, ProductSourceRecord, RegistryGateway,
    WireCertificate, WireClaim, WorkspaceModel, activate_semantic_publication, ingest, projection,
    publish_builtin_view,
};
use super::snapshot::{
    SemanticDeclaration, SemanticLinkSummary, SemanticPackageSnapshot, semantic_confidence,
    semantic_declaration_identity, semantic_link_evidence, semantic_link_kind,
    semantic_link_target, semantic_package_snapshot,
};
use backend_engine::application::LocalCompilerClient;
use backend_semantic::ir::StableLinkKey;
use std::collections::BTreeMap;

mod semantic;

use semantic::diff_semantic_snapshots;

pub(super) fn execute_semantic_diff(
    daemon: &crate::Locald<BuiltinModel, BuiltinValidator, BuiltinAuthorityVerifier>,
    compiler: &LocalCompilerClient,
    from: &backend_engine::PackageReference,
    to: &backend_engine::PackageReference,
) -> Result<Box<[backend_engine::DiffRecord]>, BuiltinModelError> {
    let before = semantic_package_snapshot(daemon, compiler, from)?;
    let after = semantic_package_snapshot(daemon, compiler, to)?;
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

#[cfg(test)]
mod semantic_diff_tests {
    use super::super::snapshot::{
        SemanticDeclaration, SemanticLinkSummary, SemanticPackageSnapshot,
    };
    use super::*;
    use backend_semantic::ir::{
        CorePayloadHash, DeclarationFamilyId, DeclarationIdentity, EntityVersion,
        VariantFingerprint,
    };

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
