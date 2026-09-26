//! Row, row-id, and row-state wire for reply and event payloads.
//!
//! A row carries its basis, identity claims, and document fragments. Callers
//! that already checked the basis use the against-basis decoder so a compact
//! delta does not repeat the source relation root.

use super::super::EmptyWire;
use super::super::WireCertificate;
use super::super::WireSchema;
use super::super::reply_admission::CoverageAdmission;
use super::super::reply_content::{
    FragmentWire, SourceAvailabilityWire, SourceExcerptWire, fragment_from_wire_with_capability,
    fragment_to_wire, source_availability_from_wire, source_availability_to_wire,
    source_excerpt_from_wire, source_excerpt_not_captured, source_excerpt_to_wire,
};
use super::{
    BasisWire, basis_from_wire, basis_from_wire_with_capability, basis_object, basis_to_wire,
};
use crate::canonical::{ObjectSchema, PackageSchema, SymbolSchema, encode_id};
use crate::{
    Basis, CoverageCapability, DeclarationKind, Row, RowId, RowIdentityPreimage, RowState,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RowWire {
    id: RowIdWire,
    basis: BasisWire,
    state: RowStateWire,
    label: String,
    score: Option<u32>,
    package: Option<String>,
    parent: Option<String>,
    document: Vec<FragmentWire>,
    signature: Option<String>,
    kind: Option<String>,
    source: SourceAvailabilityWire,
    #[serde(default = "source_excerpt_not_captured")]
    excerpt: SourceExcerptWire,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RowIdWire {
    pub(crate) kind: String,
    pub(crate) id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", deny_unknown_fields)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RowStateWire {
    Ready(EmptyWire),
    Loading(EmptyWire),
    Failed(EmptyWire),
}

pub(crate) fn row_to_wire(row: &Row) -> RowWire {
    RowWire {
        id: row_id_to_wire(row.id),
        basis: basis_to_wire(row.basis),
        state: row_state_to_wire(row.state),
        label: row.label.clone(),
        score: row.score,
        package: row.package.map(|value| encode_id(value.as_bytes())),
        parent: row.parent.map(|value| encode_id(value.as_bytes())),
        document: row.document.iter().map(fragment_to_wire).collect(),
        signature: row.signature.clone(),
        kind: row.kind.map(|kind| kind.name().to_owned()),
        source: source_availability_to_wire(&row.source),
        excerpt: source_excerpt_to_wire(&row.excerpt),
    }
}

pub(crate) fn row_from_wire(value: RowWire, certificate: &WireCertificate) -> Result<Row, String> {
    row_from_wire_with_capability(value, certificate, None)
}

pub(super) fn row_from_wire_with_capability(
    value: RowWire,
    certificate: &WireCertificate,
    capability: Option<&CoverageCapability>,
) -> Result<Row, String> {
    let document = value
        .document
        .into_iter()
        .map(|fragment| fragment_from_wire_with_capability(fragment, certificate, capability))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("snapshot row document: {error}"))?
        .into_boxed_slice();
    let id = capability
        .map_or_else(
            || row_id_from_wire(&value.id, certificate),
            |capability| row_id_from_wire_with_capability(&value.id, certificate, capability),
        )
        .map_err(|error| format!("identity: {error}"))?;
    let identity_preimage = row_identity_preimage_from_wire(&value.id, certificate)?;
    let basis = match capability {
        Some(capability) => basis_from_wire_with_capability(&value.basis, certificate, capability),
        None => basis_from_wire(&value.basis, certificate),
    }
    .map_err(|error| format!("basis: {error}"))?;
    let package = value
        .package
        .as_deref()
        .map(|id| match capability {
            Some(capability) => certificate.row_identity_or_key_or_producer::<PackageSchema>(
                WireSchema::Package,
                id,
                capability,
            ),
            None => certificate.row_identity_or_key_value::<PackageSchema>(WireSchema::Package, id),
        })
        .transpose()
        .map_err(|error| format!("package: {error}"))?;
    let parent = value
        .parent
        .as_deref()
        .map(|id| match capability {
            Some(capability) => certificate.row_identity_or_key_or_producer::<SymbolSchema>(
                WireSchema::Symbol,
                id,
                capability,
            ),
            None => certificate.row_identity_or_key_value::<SymbolSchema>(WireSchema::Symbol, id),
        })
        .transpose()
        .map_err(|error| format!("parent: {error}"))?;
    Ok(Row {
        id,
        identity_preimage,
        basis,
        state: row_state_from_wire(&value.state),
        label: value.label,
        score: value.score,
        package,
        parent,
        document,
        signature: value.signature,
        kind: value.kind.map(|kind| DeclarationKind::from_name(&kind)),
        source: source_availability_from_wire(value.source)?,
        excerpt: source_excerpt_from_wire(value.excerpt)?,
    })
}

pub(super) fn row_from_wire_with_admission<A: CoverageAdmission>(
    value: RowWire,
    certificate: &WireCertificate,
    admission: &A,
) -> Result<Row, String> {
    let capability = admission.admit(certificate, basis_object(&value.basis))?;
    row_from_wire_with_capability(value, certificate, capability.as_ref())
}

/// Decodes a row while pinning its source basis to a caller-owned checked
/// basis.  Compact journal events use this path so a row delta does not need
/// to repeat a canonical source relation root or its full source certificate.
/// Stable row/package/parent/link identities still require their individual
/// producer claims.
pub(crate) fn row_from_wire_against(
    value: RowWire,
    expected: Basis,
    certificate: &WireCertificate,
) -> Result<Row, String> {
    row_from_wire_against_admission(value, expected, certificate, None)
}

pub(crate) fn row_from_wire_against_with_capability(
    value: RowWire,
    expected: Basis,
    certificate: &WireCertificate,
    capability: &CoverageCapability,
) -> Result<Row, String> {
    row_from_wire_against_admission(value, expected, certificate, Some(capability))
}

fn row_from_wire_against_admission(
    value: RowWire,
    expected: Basis,
    certificate: &WireCertificate,
    capability: Option<&CoverageCapability>,
) -> Result<Row, String> {
    if value.basis != basis_to_wire(expected) {
        return Err("compact row basis does not match its checked source".to_owned());
    }
    let document = value
        .document
        .into_iter()
        .map(|fragment| fragment_from_wire_with_capability(fragment, certificate, capability))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("snapshot row document: {error}"))?
        .into_boxed_slice();
    let id = capability
        .map_or_else(
            || row_id_from_wire(&value.id, certificate),
            |capability| row_id_from_wire_with_capability(&value.id, certificate, capability),
        )
        .map_err(|error| format!("snapshot row identity: {error}"))?;
    let identity_preimage = row_identity_preimage_from_wire(&value.id, certificate)?;
    let package = value
        .package
        .as_deref()
        .map(|id| match capability {
            Some(capability) => certificate.row_identity_or_key_or_producer::<PackageSchema>(
                WireSchema::Package,
                id,
                capability,
            ),
            None => certificate.row_identity_or_key_value::<PackageSchema>(WireSchema::Package, id),
        })
        .transpose()
        .map_err(|error| format!("snapshot row package: {error}"))?;
    let parent = value
        .parent
        .as_deref()
        .map(|id| match capability {
            Some(capability) => certificate.row_identity_or_key_or_producer::<SymbolSchema>(
                WireSchema::Symbol,
                id,
                capability,
            ),
            None => certificate.row_identity_or_key_value::<SymbolSchema>(WireSchema::Symbol, id),
        })
        .transpose()
        .map_err(|error| format!("snapshot row parent: {error}"))?;
    Ok(Row {
        id,
        identity_preimage,
        basis: expected,
        state: row_state_from_wire(&value.state),
        label: value.label,
        score: value.score,
        package,
        parent,
        document,
        signature: value.signature,
        kind: value.kind.map(|kind| DeclarationKind::from_name(&kind)),
        source: source_availability_from_wire(value.source)?,
        excerpt: source_excerpt_from_wire(value.excerpt)?,
    })
}

fn row_id_from_wire_with_capability(
    value: &RowIdWire,
    certificate: &WireCertificate,
    capability: &CoverageCapability,
) -> Result<RowId, String> {
    match value.kind.as_str() {
        "package" => certificate
            .row_identity_or_key_or_producer::<PackageSchema>(
                WireSchema::Package,
                &value.id,
                capability,
            )
            .map(RowId::Package),
        "symbol" => certificate
            .row_identity_or_key_or_producer::<SymbolSchema>(
                WireSchema::Symbol,
                &value.id,
                capability,
            )
            .map(RowId::Symbol),
        "object" => certificate
            .version_value::<ObjectSchema>(WireSchema::Object, &value.id)
            .map(RowId::Object),
        _ => Err("unknown stable row identity kind".to_owned()),
    }
}

pub(crate) fn row_id_to_wire(id: RowId) -> RowIdWire {
    match id {
        RowId::Package(value) => RowIdWire {
            kind: "package".to_owned(),
            id: encode_id(value.as_bytes()),
        },
        RowId::Symbol(value) => RowIdWire {
            kind: "symbol".to_owned(),
            id: encode_id(value.as_bytes()),
        },
        RowId::Object(value) => RowIdWire {
            kind: "object".to_owned(),
            id: encode_id(value.as_bytes()),
        },
    }
}

pub(crate) fn row_id_from_wire(
    value: &RowIdWire,
    certificate: &WireCertificate,
) -> Result<RowId, String> {
    match value.kind.as_str() {
        "package" => Ok(RowId::Package(
            certificate
                .row_identity_or_key_value::<PackageSchema>(WireSchema::Package, &value.id)?,
        )),
        "symbol" => Ok(RowId::Symbol(
            certificate.row_identity_or_key_value::<SymbolSchema>(WireSchema::Symbol, &value.id)?,
        )),
        "object" => Ok(RowId::Object(
            certificate.version_value::<ObjectSchema>(WireSchema::Object, &value.id)?,
        )),
        _ => Err("unknown stable row identity kind".to_owned()),
    }
}

fn row_identity_preimage_from_wire(
    value: &RowIdWire,
    certificate: &WireCertificate,
) -> Result<Option<RowIdentityPreimage>, String> {
    let schema = match value.kind.as_str() {
        "package" => WireSchema::Package,
        "symbol" => WireSchema::Symbol,
        "object" => return Ok(None),
        _ => return Err("unknown stable row identity kind".to_owned()),
    };
    certificate
        .row_identity_preimage(schema, &value.id)
        .and_then(|preimage| {
            preimage
                .map(|value| RowIdentityPreimage::try_new(value).map_err(|error| error.to_string()))
                .transpose()
        })
}

pub(crate) fn row_state_to_wire(state: RowState) -> RowStateWire {
    match state {
        RowState::Ready => RowStateWire::Ready(EmptyWire {}),
        RowState::Loading => RowStateWire::Loading(EmptyWire {}),
        RowState::Failed => RowStateWire::Failed(EmptyWire {}),
    }
}

pub(crate) fn row_state_from_wire(state: &RowStateWire) -> RowState {
    match state {
        RowStateWire::Ready(_) => RowState::Ready,
        RowStateWire::Loading(_) => RowState::Loading,
        RowStateWire::Failed(_) => RowState::Failed,
    }
}
