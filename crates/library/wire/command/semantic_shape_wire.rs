use super::{
    SymbolAddressWire, revision_from_wire, symbol_address_from_wire, symbol_address_to_wire,
};
use crate::{SemanticShapeBudget, SemanticShapeRequest, WireCertificate, encode_id};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SemanticShapeRequestWire {
    basis: String,
    source: crate::SemanticVersionRecord,
    symbols: Vec<SymbolAddressWire>,
    max_nodes: u16,
    max_bytes: u32,
}

pub(crate) fn request_to_wire(request: &SemanticShapeRequest) -> SemanticShapeRequestWire {
    SemanticShapeRequestWire {
        basis: encode_id(request.basis().as_bytes()),
        source: request.source().clone(),
        symbols: request
            .symbols()
            .iter()
            .copied()
            .map(symbol_address_to_wire)
            .collect(),
        max_nodes: request.budget().max_nodes(),
        max_bytes: request.budget().max_bytes(),
    }
}

pub(crate) fn request_from_wire(
    value: SemanticShapeRequestWire,
    certificate: &WireCertificate,
) -> Result<SemanticShapeRequest, String> {
    if value.symbols.is_empty() || value.symbols.len() > crate::MAX_SEMANTIC_SHAPE_BATCH {
        return Err(crate::SemanticShapeError::BatchBound.to_string());
    }
    let basis = revision_from_wire(certificate, &value.basis)?;
    let budget = SemanticShapeBudget::new(value.max_nodes, value.max_bytes)
        .map_err(|error| error.to_string())?;
    let symbols = value
        .symbols
        .iter()
        .map(|symbol| symbol_address_from_wire(symbol, certificate))
        .collect::<Result<Vec<_>, _>>()?
        .into_boxed_slice();
    SemanticShapeRequest::from_admitted_parts(basis, value.source, symbols, budget)
        .map_err(|error| error.to_string())
}
