//! Proof-bearing command replies and view subscription certificates.

use super::BuiltinModelError;
use backend_engine::{
    Command, CommandReply, PageTerminal, RowId, ViewRoot, WireCertificate, WireClaim,
};

#[path = "projection/certificate.rs"]
mod certificate;
use certificate::{
    add_certificate_claim, append_cursor_claim, append_required_row_once,
    append_row_once_if_present, identity_preimage, paged_recipe, required_row, view_certificate,
    view_commitment_certificate,
};
pub(crate) use certificate::{
    certificate_for_compact_event, certificate_for_snapshot_page, certificate_for_view,
};

pub(crate) fn reply_certificate(
    command: &Command,
    reply: &CommandReply,
    owner_root: &ViewRoot,
    owner_cursor: backend_engine::Cursor,
    base: Option<WireCertificate>,
) -> Result<Option<WireCertificate>, BuiltinModelError> {
    let certificate = match reply {
        CommandReply::Packages(snapshot) => Some(view_certificate(
            &snapshot.root,
            &identity_preimage(&[
                b"query",
                b"packages",
                snapshot.root.basis().root.as_bytes(),
                &[0],
            ]),
            Some(owner_root),
            base,
        )?),
        CommandReply::ProjectionPage(page) => Some(projection_page_certificate(
            command, page, owner_root, base,
        )?),
        CommandReply::Names(snapshot) | CommandReply::Search(snapshot) => {
            let text = match command {
                Command::Name(query) => query.text().as_bytes(),
                Command::Search(query) => query.text().as_bytes(),
                _ => b"",
            };
            Some(view_certificate(
                &snapshot.root,
                &identity_preimage(&[b"query", text, snapshot.root.basis().root.as_bytes(), &[0]]),
                Some(owner_root),
                base,
            )?)
        }
        CommandReply::Resolved(rows) => Some(view_commitment_certificate(
            owner_root,
            b"library-view-v1",
            None,
            rows,
            base,
        )?),
        CommandReply::Graph(snapshot) => Some(view_certificate(
            &snapshot.root,
            &identity_preimage(&[
                b"query",
                b"graph",
                snapshot.root.basis().root.as_bytes(),
                &[0],
            ]),
            Some(owner_root),
            base,
        )?),
        CommandReply::Health(_) => {
            return Err(BuiltinModelError(
                "local service refused a legacy materialized health reply".to_owned(),
            ));
        }
        CommandReply::Readiness(report) => Some(readiness_certificate(
            report,
            owner_root,
            owner_cursor,
            base,
        )?),
        CommandReply::Revision(_) => Some(certificate_for_snapshot_page(
            owner_root,
            owner_cursor,
            None,
            &[],
            base,
        )?),
        CommandReply::Document(document) | CommandReply::Page(document) => {
            Some(document_certificate(document, owner_root, base)?)
        }
        CommandReply::Outline(outline) => Some(outline_certificate(outline, owner_root, base)?),
        CommandReply::GraphQueryPage(page) => {
            Some(graph_query_certificate(command, page, owner_root, base)?)
        }
        CommandReply::Added(_)
        | CommandReply::Removed(_)
        | CommandReply::Error(_)
        | CommandReply::Failed(_)
        | CommandReply::Surface(_) => base,
    };
    Ok(certificate)
}

fn document_certificate(
    document: &backend_engine::Document,
    owner_root: &ViewRoot,
    base: Option<WireCertificate>,
) -> Result<WireCertificate, BuiltinModelError> {
    // A semantic declaration is addressed publicly by its copied coordinate,
    // while its row identity is compiler-owned. The command adapter may
    // therefore return a document whose requested symbol is absent as a row
    // ID even though the owner found the exact labelled row. Preserve the
    // ordinary row proof when present and rely on the admitted key claim for
    // the semantic alias.
    let mut rows = Vec::new();
    if owner_root.row(RowId::Symbol(document.symbol)).is_some() {
        rows.push(required_row(
            owner_root,
            RowId::Symbol(document.symbol),
            "document symbol",
        )?);
    }
    for fragment in &document.fragments {
        let backend_engine::Fragment::Link { target, .. } = fragment else {
            continue;
        };
        append_row_once_if_present(owner_root, RowId::Symbol(*target), &mut rows);
    }
    view_commitment_certificate(owner_root, b"library-view-v1", None, &rows, base)
}

fn outline_certificate(
    outline: &backend_engine::Outline,
    owner_root: &ViewRoot,
    base: Option<WireCertificate>,
) -> Result<WireCertificate, BuiltinModelError> {
    let mut rows = vec![required_row(
        owner_root,
        RowId::Package(outline.package),
        "outline package",
    )?];
    let mut pending = outline.roots().collect::<Vec<_>>();
    while let Some(node) = pending.pop() {
        append_required_row_once(
            owner_root,
            RowId::Symbol(node.symbol),
            "outline symbol",
            &mut rows,
        )?;
        pending.extend(node.children.iter());
    }
    view_commitment_certificate(owner_root, b"library-view-v1", None, &rows, base)
}

fn projection_page_certificate(
    command: &Command,
    page: &backend_engine::ProjectionPage,
    owner_root: &ViewRoot,
    base: Option<WireCertificate>,
) -> Result<WireCertificate, BuiltinModelError> {
    let recipe = paged_recipe(command)?;
    let mut certificate = view_certificate(
        &page.snapshot.root,
        &identity_preimage(&[
            b"query",
            &recipe,
            page.snapshot.root.basis().root.as_bytes(),
            &[0],
        ]),
        Some(owner_root),
        base,
    )?;
    if let PageTerminal::More(continuation) = page.terminal {
        append_cursor_claim(&mut certificate, continuation.cursor());
    }
    Ok(certificate)
}

fn readiness_certificate(
    report: &backend_engine::HealthReport,
    owner_root: &ViewRoot,
    owner_cursor: backend_engine::Cursor,
    base: Option<WireCertificate>,
) -> Result<WireCertificate, BuiltinModelError> {
    let revision = report.revision();
    if revision.root() != owner_root.root()
        || revision.cursor() != owner_cursor
        || report.basis() != owner_root.basis()
        || report.coverage() != owner_root.coverage()
        || report.row_count() != owner_root.row_count()
    {
        return Err(BuiltinModelError(
            "health report does not describe the selected owner revision".to_owned(),
        ));
    }
    let mut certificate =
        view_commitment_certificate(owner_root, b"library-view-v1", None, &[], base)?;
    append_cursor_claim(&mut certificate, owner_cursor);
    Ok(certificate)
}

fn graph_query_certificate(
    command: &Command,
    page: &backend_engine::GraphQueryPage,
    owner_root: &ViewRoot,
    base: Option<WireCertificate>,
) -> Result<WireCertificate, BuiltinModelError> {
    let Command::GraphQuery(request) = command else {
        return Err(BuiltinModelError(
            "structured graph-query page does not match its command".to_owned(),
        ));
    };
    if !page.revision.matches(owner_root.root()) || page.source != owner_root.basis().object {
        return Err(BuiltinModelError(
            "structured graph-query page does not match the selected owner revision".to_owned(),
        ));
    }
    let mut certificate = base.ok_or_else(|| {
        BuiltinModelError(
            "structured graph-query request omitted its revision certificate".to_owned(),
        )
    })?;
    if let PageTerminal::More(continuation) = page.terminal {
        let cursor = continuation.cursor();
        if cursor.recipe() != request.recipe() || cursor.root() != owner_root.root() {
            return Err(BuiltinModelError(
                "structured graph-query continuation does not match its request".to_owned(),
            ));
        }
        add_certificate_claim(
            &mut certificate,
            WireClaim::KeyBytes {
                schema: backend_engine::WireSchema::ViewRecipe,
                id: backend_engine::encode_id(request.recipe().as_bytes()),
                value: request.recipe_preimage(),
            },
        );
        certificate.claims = certificate
            .claims
            .iter()
            .filter(|claim| !matches!(claim, WireClaim::Cursor { .. }))
            .cloned()
            .collect::<Vec<_>>()
            .into_boxed_slice();
        append_cursor_claim(&mut certificate, cursor);
    }
    Ok(certificate)
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn commitment_certificate_size_is_independent_of_hidden_rows() {
        let (empty, _) = super::super::initial_view().expect("initial view");
        let capability = super::super::test_builtin_view_capability().expect("coverage");
        let empty_certificate =
            view_commitment_certificate(&empty, b"library-view-v1", None, &[], None)
                .expect("empty certificate");

        let mut large = empty;
        for index in 0..256 {
            let row = backend_engine::Row::new(
                RowId::Symbol(backend_engine::symbol_key(&format!("bounded::{index:04}"))),
                large.basis(),
                format!("bounded::{index:04}"),
            );
            let prepared = large
                .prepare(
                    backend_engine::ViewDelta::Upsert { row },
                    capability.clone(),
                )
                .expect("prepare row");
            (large, _) = large.commit(prepared).expect("commit row");
        }
        let large_certificate =
            view_commitment_certificate(&large, b"library-view-v1", None, &[], None)
                .expect("large certificate");

        assert_eq!(
            empty_certificate.claims.len(),
            large_certificate.claims.len()
        );
        assert!(large_certificate.claims.iter().any(|claim| matches!(
            claim,
            WireClaim::RootCommitment {
                schema: backend_engine::WireSchema::ViewRelation,
                id,
            } if id == &backend_engine::encode_id(large.root().as_bytes())
        )));
        assert!(
            !large_certificate
                .claims
                .iter()
                .any(|claim| matches!(claim, WireClaim::Root { .. }))
        );
    }
}
