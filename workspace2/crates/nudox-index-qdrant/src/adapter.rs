//! Qdrant connection, collection lifecycle, and adapter-level orchestration.

use nudox_index_graph_vector::VectorAuthority;
use serde::Serialize;

use super::{
    config,
    contract::{
        CollectionField, CollectionValue, PayloadIndexKind, QdrantError, RequestPhase, RetryPolicy,
    },
    limits::{CREATE_COLLECTION_PATH, CREATE_PAYLOAD_INDEX_PATH, REQUEST_TIMEOUT},
    transport::{self, Method},
    wire,
};

const PAYLOAD_INDEXES: &[wire::PayloadIndexDescriptor] = &[
    wire::PayloadIndexDescriptor::new(
        super::contract::PayloadField::Snapshot,
        PayloadIndexKind::Keyword,
    ),
    wire::PayloadIndexDescriptor::new(
        super::contract::PayloadField::Model,
        PayloadIndexKind::Keyword,
    ),
    wire::PayloadIndexDescriptor::new(
        super::contract::PayloadField::Segment,
        PayloadIndexKind::Keyword,
    ),
    wire::PayloadIndexDescriptor::new(
        super::contract::PayloadField::Metric,
        PayloadIndexKind::Keyword,
    ),
    wire::PayloadIndexDescriptor::new(
        super::contract::PayloadField::Partition,
        PayloadIndexKind::Integer,
    ),
];

/// A named blocking Qdrant adapter owning endpoint, collection, authority, and connection pool.
pub struct QdrantBlockingAdapter {
    pub(super) agent: ureq::Agent,
    pub(super) endpoint: String,
    pub(super) collection: String,
    pub(super) authority: VectorAuthority,
    pub(super) retry: RetryPolicy,
}

impl QdrantBlockingAdapter {
    /// Creates a blocking adapter over a caller-provided HTTP(S) endpoint.
    pub fn new(
        endpoint: &str,
        collection: &str,
        authority: VectorAuthority,
    ) -> Result<Self, QdrantError> {
        Self::with_retry(
            endpoint,
            collection,
            authority,
            RetryPolicy::default_policy(),
        )
    }

    /// Creates an adapter with an explicit bounded retry policy.
    pub fn with_retry(
        endpoint: &str,
        collection: &str,
        authority: VectorAuthority,
        retry: RetryPolicy,
    ) -> Result<Self, QdrantError> {
        let endpoint = config::validate_endpoint(endpoint)?;
        let collection = config::validate_collection(collection)?;
        config::validate_dimension(usize::from(authority.dimension))?;
        let config = ureq::Agent::config_builder()
            .timeout_global(Some(REQUEST_TIMEOUT))
            .http_status_as_error(false)
            .build();
        Ok(Self {
            agent: config.new_agent(),
            endpoint,
            collection,
            authority,
            retry,
        })
    }

    /// Returns the adapter's immutable endpoint, collection, authority, and retry facts.
    #[must_use]
    pub fn config(&self) -> super::contract::QdrantAdapterConfig<'_> {
        super::contract::QdrantAdapterConfig {
            endpoint: &self.endpoint,
            collection: &self.collection,
            authority: self.authority,
            retry: self.retry,
        }
    }

    /// Creates the collection when absent and verifies its dimension and metric when present.
    pub fn ensure_collection(&self) -> Result<(), QdrantError> {
        let observed =
            self.request_json(RequestPhase::ReadCollection, Method::Get, &self.url(""), ())?;
        if observed.status == 404 {
            let created = self.request_json(
                RequestPhase::CreateCollection,
                Method::Put,
                &self.url(CREATE_COLLECTION_PATH),
                wire::CollectionRequest::new(self.authority),
            )?;
            if !transport::is_success(created.status) && created.status != 409 {
                return Err(transport::status_error(
                    RequestPhase::CreateCollection,
                    created,
                ));
            }
            if transport::is_success(created.status) {
                wire::parse_boolean_ack(RequestPhase::CreateCollection, &created.body)?;
            }
        } else if !transport::is_success(observed.status) {
            return Err(transport::status_error(
                RequestPhase::ReadCollection,
                observed,
            ));
        }
        self.verify_collection()?;
        self.ensure_payload_indexes()?;
        self.verify_payload_indexes()
    }

    /// Deletes the caller-selected collection and verifies a successful service response.
    pub fn delete_collection(&self) -> Result<(), QdrantError> {
        let response = self.request_json(
            RequestPhase::DeleteCollection,
            Method::Delete,
            &self.url(""),
            (),
        )?;
        if !transport::is_success(response.status) {
            return Err(transport::status_error(
                RequestPhase::DeleteCollection,
                response,
            ));
        }
        wire::parse_boolean_ack(RequestPhase::DeleteCollection, &response.body)
    }

    pub(super) fn url(&self, suffix: &str) -> String {
        format!(
            "{}/collections/{}{}",
            self.endpoint, self.collection, suffix
        )
    }

    pub(super) fn request_json<T: Serialize>(
        &self,
        phase: RequestPhase,
        method: Method,
        url: &str,
        body: T,
    ) -> Result<transport::ResponseEnvelope, QdrantError> {
        transport::request_json(&self.agent, self.retry, phase, method, url, body)
    }

    fn verify_collection(&self) -> Result<(), QdrantError> {
        let response =
            self.request_json(RequestPhase::ReadCollection, Method::Get, &self.url(""), ())?;
        if !transport::is_success(response.status) {
            return Err(transport::status_error(
                RequestPhase::ReadCollection,
                response,
            ));
        }
        let metadata = wire::collection_metadata(RequestPhase::ReadCollection, &response.body)?;
        let expected_size = u64::from(self.authority.dimension);
        if metadata.dimension != expected_size {
            return Err(QdrantError::CollectionMismatch {
                phase: RequestPhase::ReadCollection,
                field: CollectionField::VectorDimension,
                expected: CollectionValue::Dimension(expected_size),
                observed: CollectionValue::Dimension(metadata.dimension),
            });
        }
        if metadata.metric != self.authority.metric.into() {
            return Err(QdrantError::CollectionMismatch {
                phase: RequestPhase::ReadCollection,
                field: CollectionField::VectorMetric,
                expected: CollectionValue::Metric(self.authority.metric),
                observed: CollectionValue::Metric(metadata.metric.into()),
            });
        }
        if metadata.write_consistency_factor < metadata.replication_factor {
            return Err(QdrantError::CollectionMismatch {
                phase: RequestPhase::ReadCollection,
                field: CollectionField::WriteConsistency,
                expected: CollectionValue::Consistency(metadata.replication_factor),
                observed: CollectionValue::Consistency(metadata.write_consistency_factor),
            });
        }
        Ok(())
    }

    fn ensure_payload_indexes(&self) -> Result<(), QdrantError> {
        for &descriptor in PAYLOAD_INDEXES {
            let response = self.request_json(
                RequestPhase::CreatePayloadIndex,
                Method::Put,
                &self.url(CREATE_PAYLOAD_INDEX_PATH),
                wire::PayloadIndexRequest::new(descriptor.field, descriptor.schema),
            )?;
            if !transport::is_success(response.status) && response.status != 409 {
                return Err(transport::status_error(
                    RequestPhase::CreatePayloadIndex,
                    response,
                ));
            }
            if transport::is_success(response.status) {
                wire::parse_completed_ack(RequestPhase::CreatePayloadIndex, &response.body)?;
            }
        }
        Ok(())
    }

    fn verify_payload_indexes(&self) -> Result<(), QdrantError> {
        let response =
            self.request_json(RequestPhase::ReadCollection, Method::Get, &self.url(""), ())?;
        if !transport::is_success(response.status) {
            return Err(transport::status_error(
                RequestPhase::ReadCollection,
                response,
            ));
        }
        wire::verify_payload_indexes(
            RequestPhase::ReadCollection,
            &response.body,
            PAYLOAD_INDEXES,
        )
    }
}
