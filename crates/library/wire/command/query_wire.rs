//! Command query envelopes for document, outline, graph, name, and search.

use super::{
    CursorWire, ReadManifestWire, WireCertificate, WireSchema, cursor_from_wire, cursor_to_wire,
    read_manifest_from_wire, read_manifest_to_wire,
};
use crate::canonical::{PackageSchema, SymbolSchema, decode_id, encode_id};
use crate::{
    DocumentQuery, GraphNeighborhoodQuery, NameQuery, OutlineQuery, Query, QueryLimit,
    SymbolAddress,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DocumentQueryWire {
    symbol: SymbolAddressWire,
    basis: String,
    source: Option<super::super::BasisWire>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct OutlineQueryWire {
    package: String,
    basis: String,
    source: Option<super::super::BasisWire>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct GraphNeighborhoodQueryWire {
    symbol: SymbolAddressWire,
    basis: String,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SymbolAddressKindWire {
    Canonical,
    Selected,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SymbolAddressWire {
    pub(crate) kind: SymbolAddressKindWire,
    pub(crate) id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct NameQueryWire {
    text: String,
    limit: u16,
    basis: String,
    cursor: Option<CursorWire>,
    read_manifest: Option<ReadManifestWire>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct QueryWire {
    text: String,
    limit: u16,
    basis: String,
    cursor: Option<CursorWire>,
    read_manifest: Option<ReadManifestWire>,
}

pub(super) fn document_query_to_wire(query: &DocumentQuery) -> DocumentQueryWire {
    DocumentQueryWire {
        symbol: symbol_address_to_wire(query.symbol()),
        basis: encode_id(query.basis().as_bytes()),
        source: query.source_basis().map(super::super::basis_to_wire),
    }
}

pub(super) fn document_query_from_wire(
    value: &DocumentQueryWire,
    certificate: &WireCertificate,
) -> Result<DocumentQuery, String> {
    let basis = super::revision_from_wire(certificate, &value.basis)?;
    let source = value
        .source
        .as_ref()
        .map(|source| super::super::basis_from_wire(source, certificate))
        .transpose()?;
    if source.is_some_and(|source| !basis.matches(source.root)) {
        return Err("document query source basis does not match its root".to_owned());
    }
    Ok(DocumentQuery {
        symbol: symbol_address_from_wire(&value.symbol, certificate)?,
        basis,
        source,
    })
}

pub(super) fn outline_query_to_wire(query: &OutlineQuery) -> OutlineQueryWire {
    OutlineQueryWire {
        package: encode_id(query.package().as_bytes()),
        basis: encode_id(query.basis().as_bytes()),
        source: query.source_basis().map(super::super::basis_to_wire),
    }
}

pub(super) fn outline_query_from_wire(
    value: &OutlineQueryWire,
    certificate: &WireCertificate,
) -> Result<OutlineQuery, String> {
    let basis = super::revision_from_wire(certificate, &value.basis)?;
    let source = value
        .source
        .as_ref()
        .map(|source| super::super::basis_from_wire(source, certificate))
        .transpose()?;
    if source.is_some_and(|source| !basis.matches(source.root)) {
        return Err("outline query source basis does not match its root".to_owned());
    }
    Ok(OutlineQuery {
        package: certificate.key_value::<PackageSchema>(WireSchema::Package, &value.package)?,
        basis,
        source,
    })
}

pub(super) fn graph_query_to_wire(query: &GraphNeighborhoodQuery) -> GraphNeighborhoodQueryWire {
    GraphNeighborhoodQueryWire {
        symbol: symbol_address_to_wire(query.symbol()),
        basis: encode_id(query.basis().as_bytes()),
    }
}

pub(super) fn graph_query_from_wire(
    value: &GraphNeighborhoodQueryWire,
    certificate: &WireCertificate,
) -> Result<GraphNeighborhoodQuery, String> {
    Ok(GraphNeighborhoodQuery {
        symbol: symbol_address_from_wire(&value.symbol, certificate)?,
        basis: super::revision_from_wire(certificate, &value.basis)?,
    })
}

pub(super) fn symbol_address_to_wire(address: SymbolAddress) -> SymbolAddressWire {
    SymbolAddressWire {
        kind: if address.is_selected() {
            SymbolAddressKindWire::Selected
        } else {
            SymbolAddressKindWire::Canonical
        },
        id: encode_id(&address.claimed_bytes()),
    }
}

pub(super) fn symbol_address_from_wire(
    value: &SymbolAddressWire,
    certificate: &WireCertificate,
) -> Result<SymbolAddress, String> {
    match value.kind {
        SymbolAddressKindWire::Canonical => certificate
            .key_value::<SymbolSchema>(WireSchema::Symbol, &value.id)
            .map(SymbolAddress::canonical),
        SymbolAddressKindWire::Selected => {
            certificate.key_commitment(WireSchema::Symbol, &value.id)?;
            decode_id(&value.id)
                .map(SymbolAddress::from_selected_bytes)
                .map_err(|error| error.to_string())
        }
    }
}

pub(super) fn name_query_to_wire(query: &NameQuery) -> NameQueryWire {
    NameQueryWire {
        text: query.text().to_owned(),
        limit: query.limit().get(),
        basis: encode_id(query.basis().as_bytes()),
        cursor: query.cursor().map(cursor_to_wire),
        read_manifest: query.read_manifest().map(read_manifest_to_wire),
    }
}

pub(super) fn name_query_from_wire(
    value: NameQueryWire,
    certificate: &WireCertificate,
) -> Result<NameQuery, String> {
    let limit = QueryLimit::new(value.limit).ok_or_else(|| "invalid query limit".to_owned())?;
    Ok(NameQuery {
        text: value.text,
        limit,
        basis: super::revision_from_wire(certificate, &value.basis)?,
        cursor: value
            .cursor
            .as_ref()
            .map(|cursor| cursor_from_wire(cursor, certificate))
            .transpose()?,
        read_manifest: value
            .read_manifest
            .as_ref()
            .map(read_manifest_from_wire)
            .transpose()?,
    })
}

pub(super) fn query_to_wire(query: &Query) -> QueryWire {
    QueryWire {
        text: query.text().to_owned(),
        limit: query.limit().get(),
        basis: encode_id(query.basis().as_bytes()),
        cursor: query.cursor().map(cursor_to_wire),
        read_manifest: query.read_manifest().map(read_manifest_to_wire),
    }
}

pub(super) fn query_from_wire(
    value: QueryWire,
    certificate: &WireCertificate,
) -> Result<Query, String> {
    let limit = QueryLimit::new(value.limit).ok_or_else(|| "invalid query limit".to_owned())?;
    Ok(Query {
        text: value.text,
        limit,
        basis: super::revision_from_wire(certificate, &value.basis)?,
        cursor: value
            .cursor
            .as_ref()
            .map(|cursor| cursor_from_wire(cursor, certificate))
            .transpose()?,
        read_manifest: value
            .read_manifest
            .as_ref()
            .map(read_manifest_from_wire)
            .transpose()?,
    })
}
