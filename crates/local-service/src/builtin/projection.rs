//! Proof-bearing command replies and view subscription certificates.

use super::BuiltinModelError;
use backend_engine::{
    Command, CommandReply, PageTerminal, RowId, ViewRoot, WireCertificate, WireClaim,
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
        add_finished_claim(
            &mut certificate,
            WireClaim::KeyBytes {
                schema: backend_engine::WireSchema::ViewRecipe,
                id: backend_engine::encode_id(request.recipe().as_bytes()),
                value: request.recipe_preimage(),
            },
        );
        certificate = WireCertificate::from_claims(
            certificate
                .claims
                .iter()
                .filter(|claim| !matches!(claim, WireClaim::Cursor { .. }))
                .cloned()
                .collect::<Vec<_>>()
                .into_boxed_slice(),
        );
        append_cursor_claim(&mut certificate, cursor);
    }
    Ok(certificate)
}

fn append_cursor_claim(certificate: &mut WireCertificate, cursor: backend_engine::Cursor) {
    add_finished_claim(
        certificate,
        WireClaim::Cursor {
            recipe: backend_engine::encode_id(cursor.recipe().as_bytes()),
            version: backend_engine::encode_id(cursor.version().as_bytes()),
            branch: backend_engine::encode_id(cursor.branch().as_bytes()),
            log: backend_engine::encode_id(cursor.log().as_bytes()),
            schema: cursor.schema(),
            root: backend_engine::encode_id(cursor.root().as_bytes()),
            sequence: cursor.sequence(),
        },
    );
}

fn paged_recipe(command: &Command) -> Result<Vec<u8>, BuiltinModelError> {
    match command {
        Command::PackagePage(_) => Ok(b"packages-page".to_vec()),
        Command::OutlinePage { package, .. } => {
            let mut recipe = b"outline-page".to_vec();
            recipe.extend_from_slice(package.as_bytes());
            Ok(recipe)
        }
        Command::GraphPage { symbol, .. } => {
            let mut recipe = b"graph-page".to_vec();
            recipe.extend_from_slice(&symbol.claimed_bytes());
            Ok(recipe)
        }
        _ => Err(BuiltinModelError(
            "projection page reply does not match a paged command".to_owned(),
        )),
    }
}

fn identity_preimage(parts: &[&[u8]]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for part in parts {
        bytes.extend_from_slice(&(part.len() as u64).to_be_bytes());
        bytes.extend_from_slice(part);
    }
    bytes
}

struct ClaimBuilder {
    claims: Vec<WireClaim>,
    index: std::collections::HashMap<u64, Vec<usize>>,
}

impl ClaimBuilder {
    fn from_claims(claims: Vec<WireClaim>) -> Self {
        let mut index: std::collections::HashMap<u64, Vec<usize>> =
            std::collections::HashMap::new();
        for (slot, claim) in claims.iter().enumerate() {
            index
                .entry(claim_fingerprint(claim))
                .or_default()
                .push(slot);
        }
        Self { claims, index }
    }

    fn push_once(&mut self, claim: WireClaim) {
        let fingerprint = claim_fingerprint(&claim);
        if let Some(slots) = self.index.get(&fingerprint)
            && slots.iter().any(|&slot| {
                self.claims
                    .get(slot)
                    .is_some_and(|existing| existing == &claim)
            })
        {
            return;
        }
        self.index
            .entry(fingerprint)
            .or_default()
            .push(self.claims.len());
        self.claims.push(claim);
    }

    fn finish(self) -> WireCertificate {
        WireCertificate::from_claims(self.claims.into_boxed_slice())
    }
}

fn claim_fingerprint(claim: &WireClaim) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    claim.hash(&mut hasher);
    hasher.finish()
}

fn add_finished_claim(certificate: &mut WireCertificate, claim: WireClaim) {
    let mut builder = ClaimBuilder::from_claims(std::mem::take(certificate).claims.into_vec());
    builder.push_once(claim);
    *certificate = builder.finish();
}

fn add_certificate_claim(certificate: &mut ClaimBuilder, claim: WireClaim) {
    certificate.push_once(claim);
}

fn view_certificate(
    root: &ViewRoot,
    recipe_preimage: &[u8],
    basis_root: Option<&ViewRoot>,
    base: Option<WireCertificate>,
) -> Result<WireCertificate, BuiltinModelError> {
    view_certificate_with_rows(
        root,
        recipe_preimage,
        basis_root,
        root.row_refs(),
        false,
        base,
    )
}

fn view_commitment_certificate<'a>(
    root: &ViewRoot,
    recipe_preimage: &[u8],
    basis_root: Option<&ViewRoot>,
    rows: impl IntoIterator<Item = &'a backend_engine::Row>,
    base: Option<WireCertificate>,
) -> Result<WireCertificate, BuiltinModelError> {
    view_certificate_with_rows(root, recipe_preimage, basis_root, rows, true, base)
}

fn view_certificate_with_rows<'a>(
    root: &ViewRoot,
    recipe_preimage: &[u8],
    basis_root: Option<&ViewRoot>,
    rows: impl IntoIterator<Item = &'a backend_engine::Row>,
    commitment_only: bool,
    base: Option<WireCertificate>,
) -> Result<WireCertificate, BuiltinModelError> {
    let mut certificate = ClaimBuilder::from_claims(
        base.map(|certificate| certificate.claims.into_vec())
            .unwrap_or_default(),
    );
    add_certificate_claim(
        &mut certificate,
        WireClaim::KeyBytes {
            schema: backend_engine::WireSchema::ViewRecipe,
            id: backend_engine::encode_id(root.recipe().as_bytes()),
            value: recipe_preimage.to_vec().into_boxed_slice(),
        },
    );
    add_certificate_claim(
        &mut certificate,
        WireClaim::Version {
            schema: backend_engine::WireSchema::ViewVersion,
            id: backend_engine::encode_id(root.version().as_bytes()),
            value: backend_engine::view_version_preimage(
                root.recipe(),
                root.basis(),
                root.frontier(),
                root.root(),
                root.coverage(),
            )
            .into_boxed_slice(),
        },
    );
    if commitment_only {
        add_certificate_claim(
            &mut certificate,
            WireClaim::RootCommitment {
                schema: backend_engine::WireSchema::ViewRelation,
                id: backend_engine::encode_id(root.root().as_bytes()),
            },
        );
    } else {
        let relation = root
            .canonical_relation_bytes()
            .map_err(|error| BuiltinModelError(format!("{error:?}")))?;
        add_certificate_claim(
            &mut certificate,
            WireClaim::Root {
                schema: backend_engine::WireSchema::ViewRelation,
                id: backend_engine::encode_id(root.root().as_bytes()),
                canonical: relation.into_boxed_slice(),
            },
        );
    }
    if root.basis().root != root.root() {
        if basis_root.is_some_and(|source| source.root() != root.basis().root) {
            return Err(BuiltinModelError(
                "view certificate source revision does not match its producer".to_owned(),
            ));
        }
        add_certificate_claim(
            &mut certificate,
            WireClaim::RootCommitment {
                schema: backend_engine::WireSchema::ViewRelation,
                id: backend_engine::encode_id(root.basis().root.as_bytes()),
            },
        );
    }
    add_certificate_claim(
        &mut certificate,
        WireClaim::Version {
            schema: backend_engine::WireSchema::Object,
            id: backend_engine::encode_id(root.basis().object.as_bytes()),
            value: super::VIEW_SOURCE_VALUE.to_vec().into_boxed_slice(),
        },
    );
    // Complete view roots carry an explicit producer coverage witness.  The
    // receiver uses this marker together with the object preimage above to
    // admit the capability; a digest-only `Complete` lane is never enough to
    // make a health or reset claim authoritative.
    let source_object = backend_engine::encode_id(root.basis().object.as_bytes());
    if let Some(producer) = root.capability() {
        add_certificate_claim(
            &mut certificate,
            WireClaim::Coverage {
                scope: source_object.clone(),
                observed: source_object,
                producer: backend_engine::encode_id(&producer.producer_identity()),
                context: backend_engine::encode_id(&producer.context()),
                evidence: producer.evidence().to_vec().into_boxed_slice(),
            },
        );
    }
    add_certificate_claim(
        &mut certificate,
        WireClaim::Key {
            schema: backend_engine::WireSchema::Branch,
            id: backend_engine::encode_id(root.basis().branch.as_bytes()),
            value: "main".to_owned(),
        },
    );
    add_certificate_claim(
        &mut certificate,
        WireClaim::Key {
            schema: backend_engine::WireSchema::Log,
            id: backend_engine::encode_id(root.basis().log.as_bytes()),
            value: "library".to_owned(),
        },
    );
    add_row_certificate_claims_for_rows(&mut certificate, basis_root.unwrap_or(root), rows);
    Ok(certificate.finish())
}

/// Builds the bounded certificate carried by one reset hydration page.
///
/// The visible relation is represented by a deferred commitment.  The
/// producer therefore emits no canonical relation node and never calls
/// `ViewRoot::rows`; only the page rows and their package/parent references
/// receive key claims.  The receiver recomputes the complete relation after
/// all pages arrive and admits that root against the commitment.
pub(crate) fn certificate_for_snapshot_page(
    root: &ViewRoot,
    cursor: backend_engine::Cursor,
    after: Option<RowId>,
    rows: &[backend_engine::Row],
    base: Option<WireCertificate>,
) -> Result<WireCertificate, BuiltinModelError> {
    let recipe = backend_engine::view_key(b"library-view-v1");
    if root.recipe() != recipe {
        return Err(BuiltinModelError(
            "builtin snapshot root has an unknown recipe identity".to_owned(),
        ));
    }
    let mut certificate = ClaimBuilder::from_claims(
        base.map(|certificate| certificate.claims.into_vec())
            .unwrap_or_default(),
    );
    add_certificate_claim(
        &mut certificate,
        WireClaim::KeyBytes {
            schema: backend_engine::WireSchema::ViewRecipe,
            id: backend_engine::encode_id(root.recipe().as_bytes()),
            value: b"library-view-v1".to_vec().into_boxed_slice(),
        },
    );
    add_certificate_claim(
        &mut certificate,
        WireClaim::Version {
            schema: backend_engine::WireSchema::ViewVersion,
            id: backend_engine::encode_id(root.version().as_bytes()),
            value: backend_engine::view_version_preimage(
                root.recipe(),
                root.basis(),
                root.frontier(),
                root.root(),
                root.coverage(),
            )
            .into_boxed_slice(),
        },
    );
    add_certificate_claim(
        &mut certificate,
        WireClaim::RootCommitment {
            schema: backend_engine::WireSchema::ViewRelation,
            id: backend_engine::encode_id(root.root().as_bytes()),
        },
    );
    add_certificate_claim(
        &mut certificate,
        WireClaim::RootCommitment {
            schema: backend_engine::WireSchema::ViewRelation,
            id: backend_engine::encode_id(root.basis().root.as_bytes()),
        },
    );
    add_certificate_claim(
        &mut certificate,
        WireClaim::Version {
            schema: backend_engine::WireSchema::Object,
            id: backend_engine::encode_id(root.basis().object.as_bytes()),
            value: super::VIEW_SOURCE_VALUE.to_vec().into_boxed_slice(),
        },
    );
    let source_object = backend_engine::encode_id(root.basis().object.as_bytes());
    if let Some(producer) = root.capability() {
        add_certificate_claim(
            &mut certificate,
            WireClaim::Coverage {
                scope: source_object.clone(),
                observed: source_object,
                producer: backend_engine::encode_id(&producer.producer_identity()),
                context: backend_engine::encode_id(&producer.context()),
                evidence: producer.evidence().to_vec().into_boxed_slice(),
            },
        );
    }
    add_certificate_claim(
        &mut certificate,
        WireClaim::Key {
            schema: backend_engine::WireSchema::Branch,
            id: backend_engine::encode_id(root.basis().branch.as_bytes()),
            value: "main".to_owned(),
        },
    );
    add_certificate_claim(
        &mut certificate,
        WireClaim::Key {
            schema: backend_engine::WireSchema::Log,
            id: backend_engine::encode_id(root.basis().log.as_bytes()),
            value: "library".to_owned(),
        },
    );
    add_row_certificate_claims_for_rows(&mut certificate, root, rows);
    if let Some(after) = after {
        add_row_id_commitment(&mut certificate, after);
    }
    add_certificate_claim(
        &mut certificate,
        WireClaim::Cursor {
            recipe: backend_engine::encode_id(cursor.recipe().as_bytes()),
            version: backend_engine::encode_id(cursor.version().as_bytes()),
            branch: backend_engine::encode_id(cursor.branch().as_bytes()),
            log: backend_engine::encode_id(cursor.log().as_bytes()),
            schema: cursor.schema(),
            root: backend_engine::encode_id(cursor.root().as_bytes()),
            sequence: cursor.sequence(),
        },
    );
    Ok(certificate.finish())
}

fn add_row_id_commitment(certificate: &mut ClaimBuilder, id: RowId) {
    match id {
        RowId::Package(package) => add_certificate_claim(
            certificate,
            WireClaim::KeyCommitment {
                schema: backend_engine::WireSchema::Package,
                id: backend_engine::encode_id(package.as_bytes()),
            },
        ),
        RowId::Symbol(symbol) => add_certificate_claim(
            certificate,
            WireClaim::KeyCommitment {
                schema: backend_engine::WireSchema::Symbol,
                id: backend_engine::encode_id(symbol.as_bytes()),
            },
        ),
        RowId::Object(_) => {}
    }
}

pub(crate) fn certificate_for_view(
    root: &ViewRoot,
    base: Option<WireCertificate>,
) -> Result<WireCertificate, BuiltinModelError> {
    let recipe = backend_engine::view_key(b"library-view-v1");
    if root.recipe() != recipe {
        return Err(BuiltinModelError(
            "builtin subscription root has an unknown recipe identity".to_owned(),
        ));
    }
    view_certificate(root, b"library-view-v1", None, base)
}

/// Builds the bounded certificate used by the local durable journal.
///
/// Standalone event DTOs retain complete base/target snapshots because they
/// may arrive without a prior root.  The journal already has the checked base
/// root, so it needs only the transition claim and the row identities present
/// in this delta.  Keeping this certificate separate makes a one-row journal
/// append independent of the visible view size.
pub(crate) fn certificate_for_compact_event(
    delta: &backend_engine::CommittedViewDelta,
) -> Result<WireCertificate, BuiltinModelError> {
    if matches!(delta.delta(), backend_engine::ViewDelta::Reset { .. }) {
        return Err(BuiltinModelError(
            "reset transitions are persisted as a bounded snapshot".to_owned(),
        ));
    }
    let mut certificate = ClaimBuilder::from_claims(vec![WireClaim::Delta {
        schema: backend_engine::WireSchema::ViewRelation,
        id: backend_engine::encode_id(delta.id().as_bytes()),
        base: backend_engine::encode_id(delta.base_root().as_bytes()),
        target: backend_engine::encode_id(delta.target_root().as_bytes()),
        changes: delta.canonical_changes().to_vec().into_boxed_slice(),
    }]);
    match delta.delta() {
        backend_engine::ViewDelta::Upsert { row } => {
            add_row_certificate_claims_for_rows(
                &mut certificate,
                delta.target_view(),
                std::slice::from_ref(row),
            );
        }
        backend_engine::ViewDelta::Remove { id } => {
            if let Some(row) = delta.base_view().row(*id) {
                add_row_certificate_claims_for_rows(
                    &mut certificate,
                    delta.base_view(),
                    std::slice::from_ref(&row),
                );
            }
        }
        backend_engine::ViewDelta::Patch { changes } => {
            for change in changes.iter() {
                match change {
                    backend_engine::RowChange::Upsert(row) => {
                        add_row_certificate_claims_for_rows(
                            &mut certificate,
                            delta.target_view(),
                            std::slice::from_ref(row.as_ref()),
                        );
                    }
                    backend_engine::RowChange::Remove(id) => {
                        if let Some(row) = delta.base_view().row(*id) {
                            add_row_certificate_claims_for_rows(
                                &mut certificate,
                                delta.base_view(),
                                std::slice::from_ref(&row),
                            );
                        }
                    }
                }
            }
        }
        backend_engine::ViewDelta::Coverage { .. } => {}
        backend_engine::ViewDelta::Reset { .. } => {
            return Err(BuiltinModelError(
                "reset transitions are persisted as a bounded snapshot".to_owned(),
            ));
        }
    }
    Ok(certificate.finish())
}

fn required_row(
    root: &ViewRoot,
    id: RowId,
    context: &str,
) -> Result<backend_engine::Row, BuiltinModelError> {
    root.row(id).ok_or_else(|| {
        BuiltinModelError(format!(
            "{context} is not present in the reply source revision"
        ))
    })
}

fn append_required_row_once(
    root: &ViewRoot,
    id: RowId,
    context: &str,
    rows: &mut Vec<backend_engine::Row>,
) -> Result<(), BuiltinModelError> {
    if rows.iter().any(|row| row.id == id) {
        return Ok(());
    }
    rows.push(required_row(root, id, context)?);
    Ok(())
}

fn append_row_once_if_present(root: &ViewRoot, id: RowId, rows: &mut Vec<backend_engine::Row>) {
    if rows.iter().any(|row| row.id == id) {
        return;
    }
    if let Some(row) = root.row(id) {
        rows.push(row);
    }
}

struct RowClaimSeed {
    id: RowId,
    label: String,
    preimage: Option<String>,
    package: Option<backend_engine::PackageKey>,
    parent: Option<backend_engine::SymbolKey>,
    links: Vec<backend_engine::SymbolKey>,
}

/// Row-body clones performed by [`counted_row_clone`], since the last reset.
/// Thread-local so concurrent tests never cross-pollinate counts.
/// `#[cfg(test)]`-only: `row_claim_seed` below never calls
/// `counted_row_clone` in production, so this exists purely for tests to
/// measure the cost of the naive `.cloned()` alternative with the same
/// counting primitive, and to catch a reintroduced clone if one is ever
/// routed here.
#[cfg(test)]
thread_local! {
    static ROW_BODY_CLONES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
fn counted_row_clone(row: &backend_engine::Row) -> backend_engine::Row {
    ROW_BODY_CLONES.with(|count| count.set(count.get() + 1));
    row.clone()
}

fn row_claim_seed(row: &backend_engine::Row) -> RowClaimSeed {
    RowClaimSeed {
        id: row.id,
        label: row.label.clone(),
        preimage: row
            .identity_preimage()
            .map(|preimage| preimage.as_str().to_owned()),
        package: row.package,
        parent: row.parent,
        links: row
            .document
            .iter()
            .filter_map(|fragment| match fragment {
                backend_engine::Fragment::Link { target, .. } => Some(*target),
                _ => None,
            })
            .collect(),
    }
}

fn add_row_certificate_claims_for_rows<'a>(
    certificate: &mut ClaimBuilder,
    root: &ViewRoot,
    rows: impl IntoIterator<Item = &'a backend_engine::Row>,
) {
    let seeds = rows.into_iter().map(row_claim_seed).collect::<Vec<_>>();
    // The root does not change while claims are collected. The first lookup of a
    // package or symbol is the claim every later reference would repeat.
    let mut seen_packages = std::collections::HashSet::new();
    let mut seen_symbols = std::collections::HashSet::new();
    for seed in &seeds {
        match seed.id {
            RowId::Package(package) => {
                add_package_row_claim(certificate, package, &seed.label, seed.preimage.as_deref());
            }
            RowId::Symbol(symbol) => {
                add_symbol_row_claim(certificate, symbol, &seed.label, seed.preimage.as_deref());
            }
            RowId::Object(_) => {}
        }
        if let Some(package) = seed.package
            && seen_packages.insert(package)
            && let Some(package_row) = root.row_ref(RowId::Package(package))
        {
            add_package_row_claim(
                certificate,
                package,
                &package_row.label,
                package_row
                    .identity_preimage()
                    .map(backend_engine::RowIdentityPreimage::as_str),
            );
        }
        if let Some(parent) = seed.parent
            && !seen_symbols.contains(&parent)
        {
            if let Some(parent_row) = root.row_ref(RowId::Symbol(parent)) {
                seen_symbols.insert(parent);
                add_symbol_row_claim(
                    certificate,
                    parent,
                    &parent_row.label,
                    parent_row
                        .identity_preimage()
                        .map(backend_engine::RowIdentityPreimage::as_str),
                );
            } else {
                seen_symbols.insert(parent);
                add_row_id_commitment(certificate, RowId::Symbol(parent));
            }
        }
        for target in &seed.links {
            if seen_symbols.contains(target) {
                continue;
            }
            if let Some(target_row) = root.row_ref(RowId::Symbol(*target)) {
                seen_symbols.insert(*target);
                add_symbol_row_claim(
                    certificate,
                    *target,
                    &target_row.label,
                    target_row
                        .identity_preimage()
                        .map(backend_engine::RowIdentityPreimage::as_str),
                );
            }
        }
    }
}

fn add_package_row_claim(
    certificate: &mut ClaimBuilder,
    package: backend_engine::PackageKey,
    label: &str,
    preimage: Option<&str>,
) {
    if let Some(preimage) = preimage {
        add_row_identity_claim(
            certificate,
            backend_engine::WireSchema::Package,
            package.as_bytes(),
            preimage,
        );
    } else {
        add_package_claim(certificate, package, label);
    }
}

fn add_symbol_row_claim(
    certificate: &mut ClaimBuilder,
    symbol: backend_engine::SymbolKey,
    label: &str,
    preimage: Option<&str>,
) {
    if let Some(preimage) = preimage {
        add_row_identity_claim(
            certificate,
            backend_engine::WireSchema::Symbol,
            symbol.as_bytes(),
            preimage,
        );
    } else {
        add_symbol_claim(certificate, symbol, label);
    }
}

fn add_row_identity_claim(
    certificate: &mut ClaimBuilder,
    schema: backend_engine::WireSchema,
    id: &[u8; 32],
    preimage: &str,
) {
    add_certificate_claim(
        certificate,
        WireClaim::RowIdentity {
            schema,
            id: backend_engine::encode_id(id),
            preimage: preimage.to_owned(),
        },
    );
}

fn add_package_claim(
    certificate: &mut ClaimBuilder,
    package: backend_engine::PackageKey,
    label: &str,
) {
    if backend_engine::package_key(label) == package {
        add_certificate_claim(
            certificate,
            WireClaim::Key {
                schema: backend_engine::WireSchema::Package,
                id: backend_engine::encode_id(package.as_bytes()),
                value: label.to_owned(),
            },
        );
    }
    add_row_id_commitment(certificate, RowId::Package(package));
}

fn add_symbol_claim(
    certificate: &mut ClaimBuilder,
    symbol: backend_engine::SymbolKey,
    label: &str,
) {
    if backend_engine::symbol_key(label) == symbol {
        add_certificate_claim(
            certificate,
            WireClaim::Key {
                schema: backend_engine::WireSchema::Symbol,
                id: backend_engine::encode_id(symbol.as_bytes()),
                value: label.to_owned(),
            },
        );
    }
    add_row_id_commitment(certificate, RowId::Symbol(symbol));
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

    fn checked_root(rows: Vec<backend_engine::Row>) -> ViewRoot {
        let (template, _) = super::super::initial_view().expect("initial view");
        let capability = super::super::test_builtin_view_capability().expect("coverage");
        ViewRoot::new_checked(
            template.recipe(),
            template.basis(),
            template.frontier(),
            rows,
            vec![backend_engine::ViewCoverage::Complete],
            capability,
        )
        .expect("checked root")
    }

    fn row_claims<'a>(
        root: &ViewRoot,
        rows: impl IntoIterator<Item = &'a backend_engine::Row>,
    ) -> Vec<WireClaim> {
        let mut certificate = ClaimBuilder::from_claims(Vec::new());
        add_row_certificate_claims_for_rows(&mut certificate, root, rows);
        certificate.finish().claims.to_vec()
    }

    fn claim_mentions(claims: &[WireClaim], id: &[u8; 32]) -> bool {
        let encoded = backend_engine::encode_id(id);
        claims.iter().any(|claim| match claim {
            WireClaim::Key { id, .. }
            | WireClaim::KeyCommitment { id, .. }
            | WireClaim::RowIdentity { id, .. } => id == &encoded,
            _ => false,
        })
    }

    #[test]
    fn borrowed_row_claims_match_owned_rows_including_links() {
        let (template, _) = super::super::initial_view().expect("initial view");
        let basis = template.basis();
        let package_label = "certificate-pkg";
        let package = backend_engine::package_key(package_label);
        let parent_label = "parent";
        let parent = backend_engine::symbol_key(parent_label);
        let child_label = "child";
        let child = backend_engine::symbol_key(child_label);
        let witness = "occurrence\0child";
        let witness_key = backend_engine::symbol_key(witness);
        let missing_parent = backend_engine::symbol_key("missing-parent");
        let missing_link = backend_engine::symbol_key("missing-link");
        let root = checked_root(vec![
            backend_engine::Row::new(RowId::Package(package), basis, package_label),
            backend_engine::Row::in_package(RowId::Symbol(parent), basis, package, parent_label),
            backend_engine::Row::in_package(RowId::Symbol(child), basis, package, child_label)
                .with_parent(parent)
                .with_document(vec![
                    backend_engine::Fragment::Text("x".repeat(256)),
                    backend_engine::Fragment::Link {
                        label: "parent".to_owned(),
                        target: parent,
                    },
                    backend_engine::Fragment::Link {
                        label: "gone".to_owned(),
                        target: missing_link,
                    },
                ]),
            backend_engine::Row::in_package(
                RowId::Symbol(backend_engine::symbol_key("orphan")),
                basis,
                package,
                "orphan",
            )
            .with_parent(missing_parent),
            backend_engine::Row::new(RowId::Symbol(witness_key), basis, "shown-label")
                .try_with_identity_preimage(witness)
                .expect("witness"),
        ]);

        let borrowed = view_certificate(&root, b"library-view-v1", None, None).expect("borrowed");
        let owned =
            view_certificate_with_rows(&root, b"library-view-v1", None, root.rows(), false, None)
                .expect("owned");
        assert_eq!(borrowed.claims, owned.claims);
        assert_eq!(
            row_claims(&root, root.row_refs()),
            row_claims(&root, root.rows())
        );
        assert!(claim_mentions(&borrowed.claims, package.as_bytes()));
        assert!(claim_mentions(&borrowed.claims, parent.as_bytes()));
        assert!(claim_mentions(&borrowed.claims, child.as_bytes()));
        assert!(claim_mentions(&borrowed.claims, missing_parent.as_bytes()));
        assert!(!claim_mentions(&borrowed.claims, missing_link.as_bytes()));
        assert!(borrowed.claims.iter().any(|claim| matches!(
            claim,
            WireClaim::RowIdentity { preimage, .. } if preimage == witness
        )));
        assert!(borrowed.claims.iter().any(|claim| matches!(
            claim,
            WireClaim::KeyCommitment {
                schema: backend_engine::WireSchema::Symbol,
                id,
            } if id == &backend_engine::encode_id(missing_parent.as_bytes())
        )));
    }

    #[test]
    fn shared_reference_claims_match_separate_admission() {
        let (template, _) = super::super::initial_view().expect("initial view");
        let basis = template.basis();
        let package_label = "certificate-pkg";
        let package = backend_engine::package_key(package_label);
        let parent = backend_engine::symbol_key("parent");
        let child = backend_engine::symbol_key("child");
        let peer = backend_engine::symbol_key("peer");
        let linked = |symbol, label: &str| {
            backend_engine::Row::in_package(RowId::Symbol(symbol), basis, package, label)
                .with_parent(parent)
                .with_document(vec![
                    backend_engine::Fragment::Text("x".repeat(64)),
                    backend_engine::Fragment::Link {
                        label: "parent".to_owned(),
                        target: parent,
                    },
                ])
        };
        let root = checked_root(vec![
            backend_engine::Row::new(RowId::Package(package), basis, package_label),
            backend_engine::Row::in_package(RowId::Symbol(parent), basis, package, "parent"),
            linked(child, "child"),
            linked(peer, "peer"),
        ]);
        let child_row = root.row_ref(RowId::Symbol(child)).expect("child row");
        let peer_row = root.row_ref(RowId::Symbol(peer)).expect("peer row");
        let mut separate = WireCertificate::from_claims(
            row_claims(&root, std::slice::from_ref(child_row)).into_boxed_slice(),
        );
        for claim in row_claims(&root, std::slice::from_ref(peer_row)) {
            separate = separate.with_claim_once(claim);
        }
        assert_eq!(
            row_claims(&root, [child_row, peer_row]),
            separate.claims.to_vec()
        );
    }

    #[test]
    fn missing_link_does_not_hide_a_later_parent_commitment() {
        let (template, _) = super::super::initial_view().expect("initial view");
        let basis = template.basis();
        let package = backend_engine::package_key("certificate-pkg");
        let missing = backend_engine::symbol_key("missing-link");
        let linker = backend_engine::symbol_key("linker");
        let child = backend_engine::symbol_key("child");
        let root = checked_root(vec![
            backend_engine::Row::in_package(RowId::Symbol(linker), basis, package, "linker")
                .with_document(vec![backend_engine::Fragment::Link {
                    label: "gone".to_owned(),
                    target: missing,
                }]),
            backend_engine::Row::in_package(RowId::Symbol(child), basis, package, "child")
                .with_parent(missing),
        ]);
        let linker_row = root.row_ref(RowId::Symbol(linker)).expect("linker");
        let child_row = root.row_ref(RowId::Symbol(child)).expect("child");
        let claims = row_claims(&root, [linker_row, child_row]);
        assert!(claims.iter().any(|claim| matches!(
            claim,
            WireClaim::KeyCommitment {
                schema: backend_engine::WireSchema::Symbol,
                id,
            } if id == &backend_engine::encode_id(missing.as_bytes())
        )));
        let mut separate = WireCertificate::from_claims(
            row_claims(&root, std::slice::from_ref(linker_row)).into_boxed_slice(),
        );
        for claim in row_claims(&root, std::slice::from_ref(child_row)) {
            separate = separate.with_claim_once(claim);
        }
        assert_eq!(claims, separate.claims.to_vec());
    }

    #[test]
    fn borrowed_row_claims_skip_cloning_heavy_documents() {
        const ROWS: usize = 4096;
        let (template, _) = super::super::initial_view().expect("initial view");
        let basis = template.basis();
        let package_label = "heavy-pkg";
        let package = backend_engine::package_key(package_label);
        let anchor = backend_engine::symbol_key("anchor");
        let mut rows = Vec::with_capacity(ROWS + 2);
        rows.push(backend_engine::Row::new(
            RowId::Package(package),
            basis,
            package_label,
        ));
        rows.push(
            backend_engine::Row::in_package(RowId::Symbol(anchor), basis, package, "anchor")
                .with_document(vec![backend_engine::Fragment::Text("x".repeat(256))]),
        );
        for index in 0..ROWS {
            let label = format!("symbol-{index:04}");
            rows.push(
                backend_engine::Row::in_package(
                    RowId::Symbol(backend_engine::symbol_key(&label)),
                    basis,
                    package,
                    label,
                )
                .with_parent(anchor)
                .with_document(vec![
                    backend_engine::Fragment::Text("x".repeat(256)),
                    backend_engine::Fragment::Link {
                        label: "anchor".to_owned(),
                        target: anchor,
                    },
                ]),
            );
        }
        let root = checked_root(rows);
        let owned = root.row_refs().cloned().collect::<Vec<_>>();
        let owned_claims = row_claims(&root, &owned);
        let borrowed_claims = row_claims(&root, root.row_refs());
        assert_eq!(owned_claims, borrowed_claims);

        // The real property: producing certificate claims from borrowed rows
        // never clones a row body, no matter how many rows sit behind the
        // iterator, while materializing an owned `Vec<Row>` first always
        // clones every one, `document` and all. Counting real clones instead
        // of wall-clock nanoseconds keeps this deterministic at any
        // optimization level or CPU load.
        ROW_BODY_CLONES.with(|count| count.set(0));
        let seeds = root.row_refs().map(row_claim_seed).collect::<Vec<_>>();
        let seed_clones = ROW_BODY_CLONES.with(|count| count.get());
        assert_eq!(seeds.len(), ROWS + 2);
        assert_eq!(
            seed_clones,
            0,
            "row_claim_seed cloned {seed_clones} row bodies while seeding {} rows",
            seeds.len()
        );

        ROW_BODY_CLONES.with(|count| count.set(0));
        let cloned = root.row_refs().map(counted_row_clone).collect::<Vec<_>>();
        let clone_clones = ROW_BODY_CLONES.with(|count| count.get());
        assert_eq!(cloned.len(), ROWS + 2);
        assert_eq!(
            clone_clones,
            cloned.len(),
            "collecting an owned Vec<Row> is expected to clone every row body once"
        );

        // `ClaimBuilder::push_once` and `WireCertificate::with_claim_once`
        // must agree on which claims are duplicates, not just how fast
        // unique claims append: push every claim twice and check the
        // certificate keeps exactly one copy of each, the same dedup
        // `with_claim_once`'s identity scan already guarantees.
        let distinct = (0..256)
            .map(|index| WireClaim::Key {
                schema: backend_engine::WireSchema::Symbol,
                id: format!("symbol-{index:04}"),
                value: format!("symbol-{index:04}"),
            })
            .collect::<Vec<_>>();
        let mut once_certificate = WireCertificate::new();
        for claim in distinct.iter().cloned().chain(distinct.iter().cloned()) {
            once_certificate = once_certificate.with_claim_once(claim);
        }
        let mut builder = ClaimBuilder::from_claims(Vec::new());
        for claim in distinct.iter().cloned().chain(distinct.iter().cloned()) {
            builder.push_once(claim);
        }
        let builder_certificate = builder.finish();
        assert_eq!(once_certificate.claims.len(), distinct.len());
        assert_eq!(builder_certificate.claims.len(), distinct.len());
        assert_eq!(once_certificate, builder_certificate);
    }
}
