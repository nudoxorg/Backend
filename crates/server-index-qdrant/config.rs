//! Defines config behavior for `server-index-qdrant`, whose purpose is to adapt typed vector authorities and queries to the Qdrant service.
//! This module owns the config invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Local configuration validation before any remote effect.

use super::{
    contract::{QdrantConfigError, RejectedCollectionName, RejectedEndpoint},
    limits::MAX_VECTOR_DIMENSION,
};

pub(super) fn validate_endpoint(endpoint: &str) -> Result<String, QdrantConfigError> {
    let trimmed = endpoint.trim_end_matches('/');
    if trimmed.is_empty() {
        return Err(QdrantConfigError::EmptyEndpoint);
    }
    if trimmed.chars().any(char::is_whitespace) || trimmed.contains(['?', '#']) {
        return Err(QdrantConfigError::InvalidEndpoint {
            observed: RejectedEndpoint(endpoint.to_owned()),
        });
    }
    if !trimmed.starts_with("http://") && !trimmed.starts_with("https://") {
        return Err(QdrantConfigError::UnsupportedScheme {
            observed: RejectedEndpoint(endpoint.to_owned()),
        });
    }
    if trimmed.ends_with("://") {
        return Err(QdrantConfigError::InvalidEndpoint {
            observed: RejectedEndpoint(endpoint.to_owned()),
        });
    }
    Ok(trimmed.to_owned())
}

pub(super) fn validate_collection(collection: &str) -> Result<String, QdrantConfigError> {
    if collection.is_empty() {
        return Err(QdrantConfigError::EmptyCollection);
    }
    if !collection
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
    {
        return Err(QdrantConfigError::InvalidCollection {
            observed: RejectedCollectionName(collection.to_owned()),
        });
    }
    Ok(collection.to_owned())
}

pub(super) fn validate_dimension(dimension: usize) -> Result<(), QdrantConfigError> {
    if dimension == 0 || dimension > MAX_VECTOR_DIMENSION {
        return Err(QdrantConfigError::InvalidDimension {
            maximum: MAX_VECTOR_DIMENSION,
            observed: dimension,
        });
    }
    Ok(())
}
