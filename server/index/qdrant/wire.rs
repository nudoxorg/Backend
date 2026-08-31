//! Defines wire behavior for `server-index-qdrant`, whose purpose is to adapt typed vector authorities and queries to the Qdrant service.
//! This module owns the wire invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Qdrant's typed JSON boundary, split by request and response ownership.

mod request;
mod response;

pub(super) use request::{
    CollectionRequest, DeleteRequest, PayloadIndexDescriptor, PayloadIndexRequest, QueryRequest,
    RetrieveRequest, UpsertRequest,
};
pub(super) use response::{
    collection_metadata, decode_identity, decode_vector, parse_boolean_ack, parse_completed_ack,
    parse_query_candidates, retrieve_points, verify_payload_indexes,
};

#[cfg(test)]
mod tests;
