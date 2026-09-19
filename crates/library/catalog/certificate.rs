//! Producer certificate construction for the built-in catalog source.

use crate::{
    Cursor, ViewRoot, WireCertificate, WireClaim, WireSchema, canonical,
    empty_view_relation_preimage, encode_id, object_version, view_key, view_state_root,
};

/// The default library projection owns the canonical preimage for its source
/// object and view recipe. Emit that evidence on the health DTO so a process
/// that only has the serialized reply can independently admit the root. Views
/// supplied by an external producer use the explicit certified DTO APIs and
/// therefore return no guessed certificate here.
pub(super) fn producer_certificate_for_health(
    root: &ViewRoot,
    cursor: Cursor,
) -> Option<WireCertificate> {
    if root.recipe() != view_key(b"library-view-v1")
        || root.basis().object != object_version(b"library-source-v1")
        || root.basis().root != view_state_root(&[])
        || root.capability().is_none()
    {
        return None;
    }
    let relation = root.canonical_relation_bytes().ok()?;
    let basis_relation = empty_view_relation_preimage().into_boxed_slice();
    let mut certificate = WireCertificate::new().with_claim(WireClaim::KeyBytes {
        schema: WireSchema::ViewRecipe,
        id: encode_id(root.recipe().as_bytes()),
        value: b"library-view-v1".to_vec().into_boxed_slice(),
    });
    certificate = certificate.with_claim(WireClaim::Version {
        schema: WireSchema::ViewVersion,
        id: encode_id(root.version().as_bytes()),
        value: canonical::view_version_preimage(
            root.recipe(),
            root.basis(),
            root.frontier(),
            root.root(),
            root.coverage(),
        )
        .into_boxed_slice(),
    });
    certificate = certificate.with_claim(WireClaim::Root {
        schema: WireSchema::ViewRelation,
        id: encode_id(root.root().as_bytes()),
        canonical: relation.into_boxed_slice(),
    });
    if root.root() != root.basis().root {
        certificate = certificate.with_claim(WireClaim::Root {
            schema: WireSchema::ViewRelation,
            id: encode_id(root.basis().root.as_bytes()),
            canonical: basis_relation,
        });
    }
    certificate = certificate.with_claim(WireClaim::Version {
        schema: WireSchema::Object,
        id: encode_id(root.basis().object.as_bytes()),
        value: b"library-source-v1".to_vec().into_boxed_slice(),
    });
    let capability = root.capability()?;
    certificate = certificate.with_claim(WireClaim::Coverage {
        scope: encode_id(root.basis().object.as_bytes()),
        observed: encode_id(root.basis().object.as_bytes()),
        producer: encode_id(&capability.producer_identity()),
        context: encode_id(&capability.context()),
        evidence: capability.evidence().to_vec().into_boxed_slice(),
    });
    certificate = certificate.with_claim(WireClaim::Key {
        schema: WireSchema::Branch,
        id: encode_id(root.basis().branch.as_bytes()),
        value: "main".to_owned(),
    });
    Some(
        certificate
            .with_claim(WireClaim::Key {
                schema: WireSchema::Log,
                id: encode_id(root.basis().log.as_bytes()),
                value: "library".to_owned(),
            })
            .with_claim(WireClaim::Cursor {
                recipe: encode_id(cursor.recipe().as_bytes()),
                version: encode_id(cursor.version().as_bytes()),
                branch: encode_id(cursor.branch().as_bytes()),
                log: encode_id(cursor.log().as_bytes()),
                schema: cursor.schema(),
                root: encode_id(cursor.root().as_bytes()),
                sequence: cursor.sequence(),
            }),
    )
}

/// Emits the constant-size proof needed to admit the current root as an
/// authenticated producer reference. Unlike a health snapshot, this proof
/// contains no relation preimage or row identity claims.
pub(super) fn producer_certificate_for_revision(
    root: &ViewRoot,
    cursor: Cursor,
) -> Option<WireCertificate> {
    if root.recipe() != view_key(b"library-view-v1")
        || root.basis().object != object_version(b"library-source-v1")
        || root.capability().is_none()
    {
        return None;
    }
    let capability = root.capability()?;
    Some(
        WireCertificate::new()
            .with_claim(WireClaim::KeyBytes {
                schema: WireSchema::ViewRecipe,
                id: encode_id(root.recipe().as_bytes()),
                value: b"library-view-v1".to_vec().into_boxed_slice(),
            })
            .with_claim(WireClaim::Version {
                schema: WireSchema::ViewVersion,
                id: encode_id(root.version().as_bytes()),
                value: canonical::view_version_preimage(
                    root.recipe(),
                    root.basis(),
                    root.frontier(),
                    root.root(),
                    root.coverage(),
                )
                .into_boxed_slice(),
            })
            .with_claim(WireClaim::RootCommitment {
                schema: WireSchema::ViewRelation,
                id: encode_id(root.root().as_bytes()),
            })
            .with_claim(WireClaim::Version {
                schema: WireSchema::Object,
                id: encode_id(root.basis().object.as_bytes()),
                value: b"library-source-v1".to_vec().into_boxed_slice(),
            })
            .with_claim(WireClaim::Coverage {
                scope: encode_id(root.basis().object.as_bytes()),
                observed: encode_id(root.basis().object.as_bytes()),
                producer: encode_id(&capability.producer_identity()),
                context: encode_id(&capability.context()),
                evidence: capability.evidence().to_vec().into_boxed_slice(),
            })
            .with_claim(WireClaim::Key {
                schema: WireSchema::Branch,
                id: encode_id(root.basis().branch.as_bytes()),
                value: "main".to_owned(),
            })
            .with_claim(WireClaim::Key {
                schema: WireSchema::Log,
                id: encode_id(root.basis().log.as_bytes()),
                value: "library".to_owned(),
            })
            .with_claim(WireClaim::Cursor {
                recipe: encode_id(cursor.recipe().as_bytes()),
                version: encode_id(cursor.version().as_bytes()),
                branch: encode_id(cursor.branch().as_bytes()),
                log: encode_id(cursor.log().as_bytes()),
                schema: cursor.schema(),
                root: encode_id(cursor.root().as_bytes()),
                sequence: cursor.sequence(),
            }),
    )
}

/// Emits the constant-size claims needed for a row-free readiness report.
pub(super) fn producer_certificate_for_readiness(
    root: &ViewRoot,
    cursor: Cursor,
) -> Option<WireCertificate> {
    producer_certificate_for_revision(root, cursor).map(|certificate| {
        certificate.with_claim(WireClaim::RootCommitment {
            schema: WireSchema::ViewRelation,
            id: encode_id(root.basis().root.as_bytes()),
        })
    })
}
