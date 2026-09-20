//! Proof-bearing command replies and view subscription certificates.

use super::BuiltinModelError;
use backend_engine::{Command, CommandReply, RowId, ViewRoot, WireCertificate, WireClaim};

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
        CommandReply::Resolved(rows) => {
            // `resolve` lowers to the names arrangement and returns rows
            // directly, so it has no snapshot envelope from which a receiver
            // could recover the source basis. Seed the certificate from the
            // owner root, which carries the exact basis, row, and fragment
            // identities used to construct these rows.
            let _ = rows;
            Some(view_certificate(
                owner_root,
                b"library-view-v1",
                None,
                base,
            )?)
        }
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
        CommandReply::Health(root) => Some(
            view_certificate(root, b"library-view-v1", None, base)?.with_claim(WireClaim::Cursor {
                recipe: backend_engine::encode_id(owner_cursor.recipe().as_bytes()),
                version: backend_engine::encode_id(owner_cursor.version().as_bytes()),
                branch: backend_engine::encode_id(owner_cursor.branch().as_bytes()),
                log: backend_engine::encode_id(owner_cursor.log().as_bytes()),
                schema: owner_cursor.schema(),
                root: backend_engine::encode_id(owner_cursor.root().as_bytes()),
                sequence: owner_cursor.sequence(),
            }),
        ),
        CommandReply::Revision(_) => Some(certificate_for_snapshot_page(
            owner_root,
            owner_cursor,
            None,
            &[],
            base,
        )?),
        CommandReply::Document(_) | CommandReply::Page(_) | CommandReply::Outline(_) => {
            // Show/document/outline replies can arrive without a prior health
            // request. Retain the producer's owner-root claims so a standalone
            // client can admit their source basis and nested stable IDs.
            Some(view_certificate(
                owner_root,
                b"library-view-v1",
                None,
                base,
            )?)
        }
        CommandReply::Added(_) | CommandReply::Removed(_) | CommandReply::Error(_) => base,
    };
    Ok(certificate)
}

fn identity_preimage(parts: &[&[u8]]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for part in parts {
        bytes.extend_from_slice(&(part.len() as u64).to_be_bytes());
        bytes.extend_from_slice(part);
    }
    bytes
}

fn add_certificate_claim(certificate: &mut WireCertificate, claim: WireClaim) {
    *certificate = std::mem::take(certificate).with_claim_once(claim);
}

fn view_certificate(
    root: &ViewRoot,
    recipe_preimage: &[u8],
    basis_root: Option<&ViewRoot>,
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
    add_row_certificate_claims_for_rows(&mut certificate, basis_root.unwrap_or(root), root.rows());
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

fn add_row_certificate_claims_for_rows(
    certificate: &mut WireCertificate,
    root: &ViewRoot,
    rows: &[backend_engine::Row],
) {
    for row in rows {
        match row.id {
            RowId::Package(package) => add_certificate_claim(
                certificate,
                WireClaim::Key {
                    schema: backend_engine::WireSchema::Package,
                    id: backend_engine::encode_id(package.as_bytes()),
                    value: row.label.clone(),
                },
            ),
            RowId::Symbol(symbol) => {
                add_certificate_claim(
                    certificate,
                    WireClaim::Key {
                        schema: backend_engine::WireSchema::Symbol,
                        id: backend_engine::encode_id(symbol.as_bytes()),
                        value: row.label.clone(),
                    },
                );
                add_row_id_commitment(certificate, row.id);
            }
            RowId::Object(_) => {}
        }
        if let Some(package) = row.package
            && let Some(label) = root.row_label(RowId::Package(package))
        {
            add_certificate_claim(
                certificate,
                WireClaim::Key {
                    schema: backend_engine::WireSchema::Package,
                    id: backend_engine::encode_id(package.as_bytes()),
                    value: label,
                },
            );
        }
        if let Some(parent) = row.parent {
            if let Some(label) = root.row_label(RowId::Symbol(parent)) {
                add_certificate_claim(
                    certificate,
                    WireClaim::Key {
                        schema: backend_engine::WireSchema::Symbol,
                        id: backend_engine::encode_id(parent.as_bytes()),
                        value: label,
                    },
                );
            }
            add_certificate_claim(
                certificate,
                WireClaim::KeyCommitment {
                    schema: backend_engine::WireSchema::Symbol,
                    id: backend_engine::encode_id(parent.as_bytes()),
                },
            );
        }
        for fragment in row.document.iter() {
            let backend_engine::Fragment::Link { target, .. } = fragment else {
                continue;
            };
            if root.row(RowId::Symbol(*target)).is_some() {
                if let Some(label) = root.row_label(RowId::Symbol(*target)) {
                    add_certificate_claim(
                        certificate,
                        WireClaim::Key {
                            schema: backend_engine::WireSchema::Symbol,
                            id: backend_engine::encode_id(target.as_bytes()),
                            value: label,
                        },
                    );
                }
                add_certificate_claim(
                    certificate,
                    WireClaim::KeyCommitment {
                        schema: backend_engine::WireSchema::Symbol,
                        id: backend_engine::encode_id(target.as_bytes()),
                    },
                );
            }
        }
    }
}
