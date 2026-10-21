//! Cursor and frontier wire projections shared by commands, replies, and subscriptions.

use super::{CursorWire, FrontierWire};
use crate::canonical::{BranchSchema, LogSchema, ViewRecipeSchema, ViewVersionSchema, encode_id};
use crate::{CoverageCapability, Cursor, Frontier, WireCertificate, WireSchema};

pub(crate) fn cursor_to_wire(cursor: Cursor) -> CursorWire {
    CursorWire {
        recipe: encode_id(cursor.recipe().as_bytes()),
        version: encode_id(cursor.version().as_bytes()),
        branch: encode_id(cursor.branch().as_bytes()),
        log: encode_id(cursor.log().as_bytes()),
        schema: cursor.schema(),
        root: encode_id(cursor.root().as_bytes()),
        sequence: cursor.sequence(),
        query_offset: cursor.query_offset(),
        query_window: cursor
            .query_position()
            .predecessor()
            .and_then(super::window_to_wire),
    }
}

pub(crate) fn cursor_from_wire(
    value: &CursorWire,
    certificate: &WireCertificate,
) -> Result<Cursor, String> {
    cursor(value, certificate, |certificate, encoded| {
        certificate.root_value::<crate::ViewRelation>(WireSchema::ViewRelation, encoded)
    })
}

pub(crate) fn cursor_from_wire_with_capability(
    value: &CursorWire,
    certificate: &WireCertificate,
    capability: &CoverageCapability,
) -> Result<Cursor, String> {
    cursor(value, certificate, |certificate, encoded| {
        certificate.producer_root_value::<crate::ViewRelation>(
            WireSchema::ViewRelation,
            encoded,
            capability,
        )
    })
}

pub(crate) fn cursor_from_wire_against_owner(
    value: &CursorWire,
    certificate: &WireCertificate,
    projection: &crate::graph_query::GraphQueryProjection<'_>,
) -> Result<Cursor, String> {
    if value.query_window.is_some() {
        return Err("structured graph query does not admit catalog query positions".to_owned());
    }
    let owner = projection.owner();
    certificate.cursor_claim(owner)?;
    let admitted = cursor(value, certificate, |certificate, encoded| {
        certificate
            .root_commitment_bytes::<crate::ViewRelation>(WireSchema::ViewRelation, encoded)?;
        if encoded != encode_id(owner.root().as_bytes()) {
            return Err("cursor root does not match the owner root".to_owned());
        }
        Ok(owner.root())
    })?;
    projection
        .admit(admitted)
        .map_err(|error| error.to_string())?;
    Ok(admitted)
}

/// Decodes a catalog-page cursor's digest-only predecessor root. Its root and
/// version are not authoritative until the catalog query reconstructs the
/// exact predecessor page from the admitted owner. Canonical recipe, version,
/// branch and log preimages are still required from the certificate.
pub(crate) fn cursor_from_wire_against_catalog_owner(
    value: &CursorWire,
    certificate: &WireCertificate,
    owner: Cursor,
) -> Result<Cursor, String> {
    // A digest-only RootCommitment is not a StateRoot proof. Keep portable
    // catalog continuations on the strict path until the page root can remain
    // untrusted through owner-side predecessor-page reconstruction.
    let admitted = cursor_from_wire(value, certificate)?;
    if admitted.branch() != owner.branch()
        || admitted.log() != owner.log()
        || admitted.schema() != owner.schema()
        || admitted.sequence() > owner.sequence()
    {
        return Err("catalog query cursor does not match the owner frontier".to_owned());
    }
    Ok(admitted)
}

fn cursor(
    value: &CursorWire,
    certificate: &WireCertificate,
    root: impl FnOnce(&WireCertificate, &str) -> Result<crate::ViewStateRoot, String>,
) -> Result<Cursor, String> {
    let position = match value.query_window.as_ref() {
        None => crate::QueryPosition::legacy(value.query_offset),
        Some(raw) => {
            let issued = crate::QueryLimit::new(raw.issued)
                .ok_or_else(|| "invalid query position ceiling".to_owned())?;
            let window = super::window_from_wire(Some(raw), issued)?;
            crate::QueryPosition::windowed(value.query_offset, window)
                .ok_or_else(|| "invalid query predecessor position".to_owned())?
        }
    };
    Ok(Cursor::for_view(
        certificate.key_bytes::<ViewRecipeSchema>(WireSchema::ViewRecipe, &value.recipe)?,
        certificate.version_value::<ViewVersionSchema>(WireSchema::ViewVersion, &value.version)?,
        Frontier::new(
            certificate.key_value::<BranchSchema>(WireSchema::Branch, &value.branch)?,
            certificate.key_value::<LogSchema>(WireSchema::Log, &value.log)?,
            value.schema,
            root(certificate, &value.root)?,
            value.sequence,
        ),
    )
    .with_query_position(position))
}

pub(crate) fn frontier_to_wire(frontier: Frontier) -> FrontierWire {
    FrontierWire {
        branch: encode_id(frontier.branch.as_bytes()),
        log: encode_id(frontier.log.as_bytes()),
        schema: frontier.schema,
        root: encode_id(frontier.root.as_bytes()),
        sequence: frontier.sequence,
    }
}

pub(crate) fn frontier_from_wire(
    frontier: &FrontierWire,
    certificate: &WireCertificate,
) -> Result<Frontier, String> {
    frontier_with_root(frontier, certificate, |certificate, encoded| {
        certificate.root_value::<crate::ViewRelation>(WireSchema::ViewRelation, encoded)
    })
}

pub(crate) fn frontier_from_wire_with_capability(
    frontier: &FrontierWire,
    certificate: &WireCertificate,
    capability: &CoverageCapability,
) -> Result<Frontier, String> {
    frontier_with_root(frontier, certificate, |certificate, encoded| {
        certificate
            .producer_root_value::<crate::ViewRelation>(
                WireSchema::ViewRelation,
                encoded,
                capability,
            )
            .or_else(|_| {
                certificate.root_value::<crate::ViewRelation>(WireSchema::ViewRelation, encoded)
            })
    })
}

fn frontier_with_root(
    frontier: &FrontierWire,
    certificate: &WireCertificate,
    root: impl FnOnce(&WireCertificate, &str) -> Result<crate::ViewStateRoot, String>,
) -> Result<Frontier, String> {
    Ok(Frontier::new(
        certificate.key_value::<BranchSchema>(WireSchema::Branch, &frontier.branch)?,
        certificate.key_value::<LogSchema>(WireSchema::Log, &frontier.log)?,
        frontier.schema,
        root(certificate, &frontier.root)?,
        frontier.sequence,
    ))
}
