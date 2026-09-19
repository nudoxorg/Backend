use super::cursor_wire::cursor_from_wire_against_owner;
use super::page_wire::{PageRequestWire, page_request_from_wire, page_request_to_wire};
use crate::{
    Cursor, GraphQueryControl, GraphQueryRequest, GraphValue, PageContinuation, WireCertificate,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct GraphQueryRequestWire {
    query: String,
    variables: BTreeMap<String, GraphValueWire>,
    page: PageRequestWire,
    cancelled: bool,
}

pub(crate) fn request_from_wire_against_owner(
    mut value: GraphQueryRequestWire,
    certificate: &WireCertificate,
    owner: Cursor,
) -> Result<GraphQueryRequest, String> {
    let variables = value
        .variables
        .into_iter()
        .map(|(name, value)| value_from_wire(value).map(|value| (name, value)))
        .collect::<Result<BTreeMap<_, _>, _>>()?;
    let continuation = value.page.continuation.take();
    let page = page_request_from_wire(value.page, certificate)?;
    let mut request = GraphQueryRequest::new(value.query, variables, page.basis(), page.limit())
        .map_err(|error| error.to_string())?;
    if let Some(value) = continuation {
        let cursor = cursor_from_wire_against_owner(&value, certificate, owner)?;
        if cursor.recipe() != request.recipe() {
            return Err("graph query cursor recipe does not match its request".to_owned());
        }
        request = request.with_continuation(PageContinuation::from_cursor(cursor));
    }
    if value.cancelled {
        request = request.cancelled();
    }
    Ok(request)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub(crate) enum GraphValueWire {
    Null,
    Boolean(bool),
    Signed(i64),
    Unsigned(u64),
    Float(u64),
    String(String),
    List(Vec<Self>),
}

pub(crate) fn request_to_wire(request: &GraphQueryRequest) -> GraphQueryRequestWire {
    GraphQueryRequestWire {
        query: request.query().to_owned(),
        variables: request
            .variables()
            .iter()
            .map(|(name, value)| (name.clone(), value_to_wire(value)))
            .collect(),
        page: page_request_to_wire(request.page()),
        cancelled: request.control() == GraphQueryControl::Cancel,
    }
}

pub(crate) fn request_from_wire(
    value: GraphQueryRequestWire,
    certificate: &WireCertificate,
) -> Result<GraphQueryRequest, String> {
    let variables = value
        .variables
        .into_iter()
        .map(|(name, value)| value_from_wire(value).map(|value| (name, value)))
        .collect::<Result<BTreeMap<_, _>, _>>()?;
    let page = page_request_from_wire(value.page, certificate)?;
    let mut request = GraphQueryRequest::new(value.query, variables, page.basis(), page.limit())
        .map_err(|error| error.to_string())?;
    if let Some(continuation) = page.continuation() {
        request = request.with_continuation(continuation);
    }
    if value.cancelled {
        request = request.cancelled();
    }
    Ok(request)
}

pub(crate) fn value_to_wire(value: &GraphValue) -> GraphValueWire {
    match value {
        GraphValue::Null => GraphValueWire::Null,
        GraphValue::Boolean(value) => GraphValueWire::Boolean(*value),
        GraphValue::Signed(value) => GraphValueWire::Signed(*value),
        GraphValue::Unsigned(value) => GraphValueWire::Unsigned(*value),
        GraphValue::Float(bits) => GraphValueWire::Float(*bits),
        GraphValue::String(value) => GraphValueWire::String(value.clone()),
        GraphValue::List(values) => {
            GraphValueWire::List(values.iter().map(value_to_wire).collect())
        }
    }
}

pub(crate) fn value_from_wire(value: GraphValueWire) -> Result<GraphValue, String> {
    Ok(match value {
        GraphValueWire::Null => GraphValue::Null,
        GraphValueWire::Boolean(value) => GraphValue::Boolean(value),
        GraphValueWire::Signed(value) => GraphValue::Signed(value),
        GraphValueWire::Unsigned(value) => GraphValue::Unsigned(value),
        GraphValueWire::Float(bits) => {
            GraphValue::finite_float(f64::from_bits(bits)).map_err(|error| error.to_string())?
        }
        GraphValueWire::String(value) => GraphValue::String(value),
        GraphValueWire::List(values) => GraphValue::List(
            values
                .into_iter()
                .map(value_from_wire)
                .collect::<Result<Vec<_>, _>>()?
                .into_boxed_slice(),
        ),
    })
}
