//! View and row certificates for local command replies.
//!
//! Reply dispatch stays with the projection module. This module builds the
//! wire claims for a view root, a paged snapshot, and a compact journal event.

use super::BuiltinModelError;
use backend_engine::{Command, RowId, ViewRoot, WireCertificate, WireClaim};

/// Binds one owner cursor into a certificate.
pub(super) fn append_cursor_claim(
    certificate: &mut WireCertificate,
    cursor: backend_engine::Cursor,
) {
    add_certificate_claim(
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

/// Names the recipe for one paged command.
pub(super) fn paged_recipe(command: &Command) -> Result<Vec<u8>, BuiltinModelError> {
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

/// Length-prefixes the parts of one certificate recipe.
pub(super) fn identity_preimage(parts: &[&[u8]]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for part in parts {
        bytes.extend_from_slice(&(part.len() as u64).to_be_bytes());
        bytes.extend_from_slice(part);
    }
    bytes
}

/// Appends one claim, replacing an earlier claim of the same shape.
pub(super) fn add_certificate_claim(certificate: &mut WireCertificate, claim: WireClaim) {
    *certificate = std::mem::take(certificate).with_claim_once(claim);
}

/// Certifies a view root and every visible row.
pub(super) fn view_certificate(
    root: &ViewRoot,
    recipe_preimage: &[u8],
    basis_root: Option<&ViewRoot>,
    base: Option<WireCertificate>,
) -> Result<WireCertificate, BuiltinModelError> {
    let rows = root.rows();
    view_certificate_with_rows(root, recipe_preimage, basis_root, rows, false, base)
}

/// Certifies a view root by commitment, omitting hidden row bodies.
pub(super) fn view_commitment_certificate(
    root: &ViewRoot,
    recipe_preimage: &[u8],
    basis_root: Option<&ViewRoot>,
    rows: &[backend_engine::Row],
    base: Option<WireCertificate>,
) -> Result<WireCertificate, BuiltinModelError> {
    view_certificate_with_rows(root, recipe_preimage, basis_root, rows, true, base)
}

fn view_certificate_with_rows(
    root: &ViewRoot,
    recipe_preimage: &[u8],
    basis_root: Option<&ViewRoot>,
    rows: &[backend_engine::Row],
    commitment_only: bool,
    base: Option<WireCertificate>,
) -> Result<WireCertificate, BuiltinModelError> {
    let mut certificate = base.unwrap_or_default();
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
            value: super::super::VIEW_SOURCE_VALUE.to_vec().into_boxed_slice(),
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
    Ok(certificate)
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
    let mut certificate = base.unwrap_or_default();
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
            value: super::super::VIEW_SOURCE_VALUE.to_vec().into_boxed_slice(),
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
    Ok(certificate)
}

fn add_row_id_commitment(certificate: &mut WireCertificate, id: RowId) {
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
    let mut certificate = WireCertificate::new().with_claim(WireClaim::Delta {
        schema: backend_engine::WireSchema::ViewRelation,
        id: backend_engine::encode_id(delta.id().as_bytes()),
        base: backend_engine::encode_id(delta.base_root().as_bytes()),
        target: backend_engine::encode_id(delta.target_root().as_bytes()),
        changes: delta.canonical_changes().to_vec().into_boxed_slice(),
    });
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
    Ok(certificate)
}

/// Returns one view row that the certificate names, or fails closed.
pub(super) fn required_row(
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

/// Appends one required row when the certificate does not already name it.
pub(super) fn append_required_row_once(
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

/// Appends one row when the view still contains it.
pub(super) fn append_row_once_if_present(
    root: &ViewRoot,
    id: RowId,
    rows: &mut Vec<backend_engine::Row>,
) {
    if rows.iter().any(|row| row.id == id) {
        return;
    }
    if let Some(row) = root.row(id) {
        rows.push(row);
    }
}

fn add_row_certificate_claims_for_rows(
    certificate: &mut WireCertificate,
    root: &ViewRoot,
    rows: &[backend_engine::Row],
) {
    for row in rows {
        match row.id {
            RowId::Package(package) => {
                add_package_row_claim(certificate, package, row);
            }
            RowId::Symbol(symbol) => {
                add_symbol_row_claim(certificate, symbol, row);
            }
            RowId::Object(_) => {}
        }
        if let Some(package) = row.package
            && let Some(package_row) = root.row_ref(RowId::Package(package))
        {
            add_package_row_claim(certificate, package, package_row);
        }
        if let Some(parent) = row.parent {
            if let Some(parent_row) = root.row_ref(RowId::Symbol(parent)) {
                add_symbol_row_claim(certificate, parent, parent_row);
            } else {
                add_row_id_commitment(certificate, RowId::Symbol(parent));
            }
        }
        for fragment in &row.document {
            let backend_engine::Fragment::Link { target, .. } = fragment else {
                continue;
            };
            if root.row(RowId::Symbol(*target)).is_some() {
                if let Some(target_row) = root.row_ref(RowId::Symbol(*target)) {
                    add_symbol_row_claim(certificate, *target, target_row);
                } else {
                    add_row_id_commitment(certificate, RowId::Symbol(*target));
                }
            }
        }
    }
}

fn add_package_row_claim(
    certificate: &mut WireCertificate,
    package: backend_engine::PackageKey,
    row: &backend_engine::Row,
) {
    if let Some(preimage) = row.identity_preimage() {
        add_row_identity_claim(
            certificate,
            backend_engine::WireSchema::Package,
            package.as_bytes(),
            preimage,
        );
    } else {
        add_package_claim(certificate, package, &row.label);
    }
}

fn add_symbol_row_claim(
    certificate: &mut WireCertificate,
    symbol: backend_engine::SymbolKey,
    row: &backend_engine::Row,
) {
    if let Some(preimage) = row.identity_preimage() {
        add_row_identity_claim(
            certificate,
            backend_engine::WireSchema::Symbol,
            symbol.as_bytes(),
            preimage,
        );
    } else {
        add_symbol_claim(certificate, symbol, &row.label);
    }
}

fn add_row_identity_claim(
    certificate: &mut WireCertificate,
    schema: backend_engine::WireSchema,
    id: &[u8; 32],
    preimage: &backend_engine::RowIdentityPreimage,
) {
    add_certificate_claim(
        certificate,
        WireClaim::RowIdentity {
            schema,
            id: backend_engine::encode_id(id),
            preimage: preimage.as_str().to_owned(),
        },
    );
}

fn add_package_claim(
    certificate: &mut WireCertificate,
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
    certificate: &mut WireCertificate,
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
