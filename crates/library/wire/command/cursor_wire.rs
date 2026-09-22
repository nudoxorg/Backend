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
    owner: Cursor,
) -> Result<Cursor, String> {
    let admitted = cursor(value, certificate, |certificate, encoded| {
        certificate
            .root_commitment_bytes::<crate::ViewRelation>(WireSchema::ViewRelation, encoded)?;
        if encoded != encode_id(owner.root().as_bytes()) {
            return Err("cursor root does not match the owner root".to_owned());
        }
        Ok(owner.root())
    })?;
    if !admitted.matches_owner(owner) {
        return Err("cursor does not match the owner context".to_owned());
    }
    certificate.cursor_claim(admitted.with_query_offset(0))?;
    Ok(admitted)
}

fn cursor(
    value: &CursorWire,
    certificate: &WireCertificate,
    root: impl FnOnce(&WireCertificate, &str) -> Result<crate::ViewStateRoot, String>,
) -> Result<Cursor, String> {
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
    .with_query_offset(value.query_offset))
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
