//! Bounded blocking Qdrant transport behind the extension's provider contract.

use std::{
    collections::{HashMap, HashSet},
    io::Read,
    num::NonZeroU8,
    str::FromStr,
    sync::Arc,
    time::Duration,
};

use backend_version::CoverageWitness;
use serde::{Deserialize, Serialize};

use crate::{
    AnnCursor, AnnPage, AnnSource, Binding, CandidateId, DocumentVector, EmbeddingEncoding,
    EmbeddingRecipe, Recipe, SchemaVersion, SearchQuality, VectorSearchRequest,
};
use backend_version::WorkspaceRoot;

/// Qdrant API key whose debug representation never exposes credential bytes.
#[derive(Clone, Eq, PartialEq)]
pub struct ApiKey(Arc<str>);

impl ApiKey {
    /// Admits a non-empty API key.
    ///
    /// # Errors
    /// Returns [`HttpProviderError::InvalidConfiguration`] for an empty value.
    pub fn new(value: impl Into<String>) -> Result<Self, HttpProviderError> {
        let value = value.into();
        if value.is_empty() || ureq::http::HeaderValue::from_str(&value).is_err() {
            return Err(HttpProviderError::InvalidConfiguration);
        }
        Ok(Self(Arc::from(value)))
    }
}

impl std::fmt::Debug for ApiKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ApiKey([REDACTED])")
    }
}

/// Complete bounded HTTP and response policy.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QdrantHttpConfig {
    /// HTTPS endpoint without a collection path. Plain HTTP is admitted only
    /// for a loopback Qdrant process.
    pub endpoint: String,
    /// Qdrant collection path component.
    pub collection: String,
    /// Optional API key sent using Qdrant's `api-key` header.
    pub api_key: Option<ApiKey>,
    /// TCP/TLS connection deadline.
    pub connect_deadline: Duration,
    /// Response-header and response-body inactivity deadline.
    pub read_deadline: Duration,
    /// Positive attempt bound for idempotent operations.
    pub attempts: NonZeroU8,
    /// Maximum decoded response body size.
    pub max_response_bytes: usize,
    /// Maximum encoded request body size. This is independent of point count
    /// because vector dimensionality and JSON float expansion also consume
    /// transport memory.
    pub max_request_bytes: usize,
    /// Maximum points in one mutation or response page.
    pub max_batch_points: usize,
}

impl QdrantHttpConfig {
    /// Validates transport configuration before constructing an agent.
    ///
    /// # Errors
    /// Returns [`HttpProviderError::InvalidConfiguration`] for an unsafe endpoint, collection,
    /// zero deadline, or zero resource bound.
    pub fn validate(mut self) -> Result<Self, HttpProviderError> {
        self.endpoint = self.endpoint.trim_end_matches('/').to_owned();
        let valid_endpoint = endpoint_is_admitted(&self.endpoint);
        let valid_collection = !self.collection.is_empty()
            && self
                .collection
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'));
        if !valid_endpoint
            || !valid_collection
            || self.connect_deadline.is_zero()
            || self.read_deadline.is_zero()
            || self.max_response_bytes == 0
            || self.max_request_bytes == 0
            || self.max_batch_points == 0
        {
            return Err(HttpProviderError::InvalidConfiguration);
        }
        Ok(self)
    }
}

fn endpoint_is_admitted(endpoint: &str) -> bool {
    let Ok(uri) = ureq::http::Uri::from_str(endpoint) else {
        return false;
    };
    let Some(scheme) = uri.scheme_str() else {
        return false;
    };
    let Some(authority) = uri.authority() else {
        return false;
    };
    if authority.as_str().contains('@')
        || authority.host().is_empty()
        || uri.query().is_some()
        || endpoint.contains('#')
    {
        return false;
    }
    match scheme {
        "https" => true,
        "http" => endpoint_host_is_loopback(authority.host()),
        _ => false,
    }
}

fn endpoint_host_is_loopback(host: &str) -> bool {
    let host = host
        .strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
        .unwrap_or(host);
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|address| address.is_loopback())
}

/// Verified mutation count after Qdrant acknowledges every bounded batch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct QdrantMutationReceipt {
    /// Number of points submitted or deleted.
    pub points: usize,
    /// Number of bounded service operations completed.
    pub batches: usize,
}

/// Stable residence of one document row inside a workspace and embedding recipe.
///
/// The view frontier, authority, read manifest, and candidate id travel in the
/// point payload. A fence move can restamp that payload without uploading the
/// vector again. Two workspaces, or two recipes, never share a residence.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PointResidence([u8; 16]);

impl PointResidence {
    /// Derives the residence of one stable row key.
    ///
    /// # Errors
    /// Returns [`HttpProviderError::BindingMismatch`] for an empty row key and
    /// [`HttpProviderError::SizeLimit`] when a component cannot be length-prefixed.
    pub fn for_row(
        workspace: WorkspaceRoot,
        recipe: Recipe,
        row_key: &str,
    ) -> Result<Self, HttpProviderError> {
        if row_key.is_empty() {
            return Err(HttpProviderError::BindingMismatch);
        }
        let row_len = u64::try_from(row_key.len()).map_err(|_| HttpProviderError::SizeLimit)?;
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"backend.qdrant.point-residence.v1\0");
        hash_component(&mut hasher, workspace.as_bytes())?;
        hash_component(&mut hasher, recipe.as_bytes())?;
        hasher.update(&row_len.to_be_bytes());
        hasher.update(row_key.as_bytes());
        let digest = hasher.finalize();
        let mut bytes = [0_u8; 16];
        bytes.copy_from_slice(&digest.as_bytes()[..16]);
        Ok(Self(bytes))
    }

    fn parse(value: &str) -> Option<Self> {
        if value.len() != 32 {
            return None;
        }
        let mut bytes = [0_u8; 16];
        for (index, chunk) in value.as_bytes().chunks(2).enumerate() {
            let text = std::str::from_utf8(chunk).ok()?;
            bytes[index] = u8::from_str_radix(text, 16).ok()?;
        }
        Some(Self(bytes))
    }
}

fn hash_component(hasher: &mut blake3::Hasher, bytes: &[u8]) -> Result<(), HttpProviderError> {
    let len = u64::try_from(bytes.len()).map_err(|_| HttpProviderError::SizeLimit)?;
    hasher.update(&len.to_be_bytes());
    hasher.update(bytes);
    Ok(())
}

/// Whether a resident point may replace stored coordinates.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CoordinateWrite {
    /// The stored vector must already equal these coordinates.
    ///
    /// A different coordinate key is treated as corruption and refuses the
    /// batch before any write. A matching key restamps payload only.
    Hold,
    /// These coordinates are the new document embedding.
    ///
    /// A different coordinate key replaces that one vector. A matching key
    /// still restamps payload only.
    Replace,
}

/// One document vector addressed by its stable residence.
#[derive(Clone, Copy, Debug)]
pub struct ResidentDocument<'a> {
    /// Row residence. Independent of the view fence and candidate id.
    pub residence: PointResidence,
    /// Whether a coordinate-key mismatch may replace the stored vector.
    pub write: CoordinateWrite,
    /// Logical candidate and coordinates to publish under the current binding.
    pub document: &'a DocumentVector,
}

/// What a resident upsert changed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResidentMutationReceipt {
    /// Points whose vectors were uploaded.
    pub vectors: usize,
    /// Points whose payload was restamped without a vector upload.
    pub payloads: usize,
    /// Points already storing this binding, candidate, and coordinate key.
    pub unchanged: usize,
    /// Bounded service operations completed.
    pub batches: usize,
}

/// Qdrant client before actual projection coverage has been observed.
#[derive(Clone)]
pub struct QdrantHttpClient {
    transport: Transport,
    recipe: EmbeddingRecipe,
}

/// ANN source that exists only after Qdrant's collection and exact projection count are verified.
#[derive(Clone)]
pub struct QdrantHttpSource {
    client: QdrantHttpClient,
    binding: Binding,
    coverage: CoverageWitness,
}

impl QdrantHttpClient {
    /// Creates a bounded client for one complete embedding recipe.
    ///
    /// # Errors
    /// Returns a typed configuration or unsupported-encoding error.
    pub fn new(
        config: QdrantHttpConfig,
        recipe: EmbeddingRecipe,
    ) -> Result<Self, HttpProviderError> {
        if recipe.encoding != EmbeddingEncoding::Float32 {
            return Err(HttpProviderError::UnsupportedEncoding);
        }
        let config = config.validate()?;
        let agent_config = ureq::Agent::config_builder()
            .timeout_connect(Some(config.connect_deadline))
            .timeout_recv_response(Some(config.read_deadline))
            .timeout_recv_body(Some(config.read_deadline))
            // A configured Qdrant authority is stable. Following redirects
            // would allow an HTTP response to choose where API credentials
            // and idempotent mutation bodies are sent.
            .max_redirects(0)
            .http_status_as_error(false)
            .build();
        Ok(Self {
            transport: Transport {
                agent: agent_config.new_agent(),
                config,
            },
            recipe,
        })
    }

    /// Creates or validates the collection's exact dimension and distance metric.
    ///
    /// # Errors
    /// Returns a typed transport, response, or collection mismatch error.
    pub fn ensure_collection(&self) -> Result<(), HttpProviderError> {
        let url = self.transport.collection_url("");
        let observed = self.transport.request(Method::Get, &url, None::<&()>)?;
        if observed.status == 404 {
            let body = CollectionCreate {
                vectors: VectorConfig {
                    size: self.recipe.dimensions.get(),
                    distance: Distance::from(self.recipe.metric),
                },
            };
            let created = self.transport.request(Method::Put, &url, Some(&body))?;
            require_success(created)?;
        } else {
            require_success(observed)?;
        }
        let response = require_success(self.transport.request(Method::Get, &url, None::<&()>)?)?;
        let metadata: CollectionResponse = decode(&response.body)?;
        let vectors = metadata.result.config.params.vectors;
        if vectors.size != self.recipe.dimensions.get()
            || vectors.distance != Distance::from(self.recipe.metric)
        {
            return Err(HttpProviderError::CollectionMismatch);
        }
        Ok(())
    }

    /// Upserts document vectors in independently bounded, idempotent batches.
    ///
    /// # Errors
    /// Returns a typed binding, transport, response, or size error.
    pub fn upsert(
        &self,
        binding: Binding,
        documents: &[DocumentVector],
    ) -> Result<QdrantMutationReceipt, HttpProviderError> {
        if binding.recipe != self.recipe.version()
            || documents
                .iter()
                .any(|document| document.recipe() != self.recipe)
        {
            return Err(HttpProviderError::BindingMismatch);
        }
        if documents.is_empty() {
            return Ok(QdrantMutationReceipt {
                points: 0,
                batches: 0,
            });
        }
        let mut points = 0;
        let mut batches = 0;
        for batch in documents.chunks(self.transport.config.max_batch_points) {
            let points_in_batch: Vec<UpsertPoint<'_>> = batch
                .iter()
                .map(|document| {
                    let values = document.point().values();
                    UpsertPoint {
                        id: PhysicalPointId::for_candidate(binding, document.point().id()),
                        vector: values,
                        payload: PointPayload::for_candidate(
                            binding,
                            document.point().id(),
                            values,
                        ),
                    }
                })
                .collect();
            let observed = self.retrieve_coordinate_keys(binding, &points_in_batch)?;
            let mut due_points = Vec::new();
            for point in &points_in_batch {
                match coordinate_disposition(
                    &point.payload.coordinate_key,
                    observed.get(&point.id).map(String::as_str),
                ) {
                    CoordinateDisposition::Due => due_points.push(UpsertPoint {
                        id: point.id.clone(),
                        vector: point.vector,
                        payload: point.payload.clone(),
                    }),
                    CoordinateDisposition::Unchanged => {}
                    CoordinateDisposition::Conflict => {
                        return Err(HttpProviderError::ImmutableVector);
                    }
                }
            }
            if due_points.is_empty() {
                continue;
            }
            let submitted = due_points.len();
            let body = UpsertRequest { points: due_points };
            let response = require_success(
                self.transport.request(
                    Method::Put,
                    &self
                        .transport
                        .collection_url("/points?wait=true&ordering=strong"),
                    Some(&body),
                )?,
            )?;
            completed(&response.body)?;
            points += submitted;
            batches += 1;
        }
        Ok(QdrantMutationReceipt { points, batches })
    }

    fn retrieve_coordinate_keys(
        &self,
        binding: Binding,
        points: &[UpsertPoint<'_>],
    ) -> Result<HashMap<PhysicalPointId, String>, HttpProviderError> {
        let body = RetrieveRequest {
            ids: points.iter().map(|point| point.id.clone()).collect(),
            with_payload: true,
            with_vector: false,
        };
        let response = require_success(self.transport.request(
            Method::Post,
            &self.transport.collection_url("/points"),
            Some(&body),
        )?)?;
        let decoded: RetrieveResponse = decode(&response.body)?;
        let expected_ids: HashSet<_> = points.iter().map(|point| point.id.clone()).collect();
        let mut observed = HashMap::new();
        for point in decoded.result {
            if !expected_ids.contains(&point.id) {
                return Err(HttpProviderError::BindingMismatch);
            };
            let Some(payload) = point.payload else {
                return Err(HttpProviderError::BindingMismatch);
            };
            let Some(candidate) = payload.candidate() else {
                return Err(HttpProviderError::BindingMismatch);
            };
            if !payload.matches_binding(binding)
                || point.id != PhysicalPointId::for_candidate(binding, candidate)
            {
                return Err(HttpProviderError::BindingMismatch);
            }
            observed.insert(point.id, payload.coordinate_key);
        }
        Ok(observed)
    }

    /// Deletes logical candidate IDs in independently bounded, idempotent batches.
    ///
    /// # Errors
    /// Returns a typed binding, transport, response, or size error.
    pub fn delete(
        &self,
        binding: Binding,
        ids: &[CandidateId],
    ) -> Result<QdrantMutationReceipt, HttpProviderError> {
        if binding.recipe != self.recipe.version() || ids.iter().any(|id| !id.is_valid()) {
            return Err(HttpProviderError::BindingMismatch);
        }
        let mut batches = 0;
        for batch in ids.chunks(self.transport.config.max_batch_points) {
            let body = DeleteRequest {
                points: batch
                    .iter()
                    .copied()
                    .map(|id| PhysicalPointId::for_candidate(binding, id))
                    .collect(),
            };
            let response = require_success(
                self.transport.request(
                    Method::Post,
                    &self
                        .transport
                        .collection_url("/points/delete?wait=true&ordering=strong"),
                    Some(&body),
                )?,
            )?;
            completed(&response.body)?;
            batches += 1;
        }
        Ok(QdrantMutationReceipt {
            points: ids.len(),
            batches,
        })
    }

    /// Publishes document vectors under a stable residence.
    ///
    /// Missing points are written with their vectors. A point that already
    /// stores the same coordinate key keeps that vector and receives a payload
    /// update when the binding or candidate moved. [`CoordinateWrite::Hold`]
    /// refuses the whole call before any write when a stored coordinate key
    /// disagrees. [`CoordinateWrite::Replace`] uploads only the vectors whose
    /// keys changed.
    ///
    /// # Errors
    /// Returns a typed binding, transport, response, or size error. A held
    /// coordinate mismatch returns [`HttpProviderError::ImmutableVector`]
    /// and leaves the collection unchanged by this call.
    pub fn upsert_resident(
        &self,
        binding: Binding,
        documents: &[ResidentDocument<'_>],
    ) -> Result<ResidentMutationReceipt, HttpProviderError> {
        if binding.recipe != self.recipe.version()
            || documents
                .iter()
                .any(|document| document.document.recipe() != self.recipe)
        {
            return Err(HttpProviderError::BindingMismatch);
        }
        if documents.is_empty() {
            return Ok(ResidentMutationReceipt {
                vectors: 0,
                payloads: 0,
                unchanged: 0,
                batches: 0,
            });
        }
        let mut seen = HashSet::with_capacity(documents.len());
        let mut probes = Vec::with_capacity(documents.len());
        for document in documents {
            if !seen.insert(document.residence) {
                return Err(HttpProviderError::BindingMismatch);
            }
            let values = document.document.point().values();
            probes.push(ResidenceProbe {
                id: PhysicalPointId::for_residence(
                    binding.workspace,
                    binding.recipe,
                    document.residence,
                ),
                residence: document.residence,
                write: document.write,
                candidate: document.document.point().id(),
                vector: values,
                payload: PointPayload::for_residence(
                    binding,
                    document.residence,
                    document.document.point().id(),
                    values,
                ),
            });
        }
        let observed = self.retrieve_residences(&probes)?;
        let mut write_vectors = Vec::new();
        let mut write_payloads = Vec::new();
        let mut unchanged = 0_usize;
        for probe in &probes {
            match observed.get(&probe.id) {
                None => write_vectors.push(probe),
                Some(stored) => {
                    let Some(stored_residence) = PointResidence::parse(&stored.residence) else {
                        return Err(HttpProviderError::BindingMismatch);
                    };
                    if stored_residence != probe.residence
                        || stored.workspace != hex(binding.workspace.as_bytes())
                        || stored.recipe != hex(binding.recipe.as_bytes())
                    {
                        return Err(HttpProviderError::BindingMismatch);
                    }
                    let observed_key = (!stored.coordinate_key.is_empty())
                        .then_some(stored.coordinate_key.as_str());
                    match coordinate_disposition(&probe.payload.coordinate_key, observed_key) {
                        CoordinateDisposition::Due => write_vectors.push(probe),
                        CoordinateDisposition::Unchanged
                            if stored.matches_projection(binding, probe.candidate) =>
                        {
                            unchanged += 1;
                        }
                        CoordinateDisposition::Unchanged => write_payloads.push(probe),
                        CoordinateDisposition::Conflict if probe.write == CoordinateWrite::Hold => {
                            return Err(HttpProviderError::ImmutableVector);
                        }
                        CoordinateDisposition::Conflict => write_vectors.push(probe),
                    }
                }
            }
        }
        let mut batches = 0_usize;
        let vectors = write_vectors.len();
        for batch in write_vectors.chunks(self.transport.config.max_batch_points) {
            let points = batch
                .iter()
                .map(|probe| UpsertPoint {
                    id: probe.id.clone(),
                    vector: probe.vector,
                    payload: probe.payload.clone(),
                })
                .collect::<Vec<_>>();
            let body = UpsertRequest { points };
            let response = require_success(
                self.transport.request(
                    Method::Put,
                    &self
                        .transport
                        .collection_url("/points?wait=true&ordering=strong"),
                    Some(&body),
                )?,
            )?;
            completed(&response.body)?;
            batches += 1;
        }
        for batch in write_payloads.chunks(self.transport.config.max_batch_points) {
            let body = PayloadBatchRequest {
                operations: batch
                    .iter()
                    .map(|probe| PayloadOperation {
                        set_payload: SetPayload {
                            payload: probe.payload.clone(),
                            points: vec![probe.id.clone()],
                        },
                    })
                    .collect(),
            };
            let response = require_success(
                self.transport.request(
                    Method::Post,
                    &self
                        .transport
                        .collection_url("/points/batch?wait=true&ordering=strong"),
                    Some(&body),
                )?,
            )?;
            completed(&response.body)?;
            batches += 1;
        }
        Ok(ResidentMutationReceipt {
            vectors,
            payloads: write_payloads.len(),
            unchanged,
            batches,
        })
    }

    fn retrieve_residences(
        &self,
        probes: &[ResidenceProbe<'_>],
    ) -> Result<HashMap<PhysicalPointId, PointPayload>, HttpProviderError> {
        let mut observed = HashMap::new();
        for batch in probes.chunks(self.transport.config.max_batch_points) {
            let body = RetrieveRequest {
                ids: batch.iter().map(|probe| probe.id.clone()).collect(),
                with_payload: true,
                with_vector: false,
            };
            let response = require_success(self.transport.request(
                Method::Post,
                &self.transport.collection_url("/points"),
                Some(&body),
            )?)?;
            let decoded: RetrieveResponse = decode(&response.body)?;
            let expected_ids: HashSet<_> = batch.iter().map(|probe| probe.id.clone()).collect();
            for point in decoded.result {
                let Some(payload) = point.payload else {
                    return Err(HttpProviderError::BindingMismatch);
                };
                if !expected_ids.contains(&point.id) {
                    return Err(HttpProviderError::BindingMismatch);
                }
                if observed.insert(point.id, payload).is_some() {
                    return Err(HttpProviderError::BindingMismatch);
                }
            }
        }
        Ok(observed)
    }

    /// Deletes stable residences in independently bounded, idempotent batches.
    ///
    /// The physical id does not include the view fence, so a caller must pass
    /// only residences that are no longer live. Deleting a residence that was
    /// just rebound removes the current vector.
    ///
    /// # Errors
    /// Returns a typed binding, transport, response, or size error.
    pub fn delete_residences(
        &self,
        workspace: WorkspaceRoot,
        recipe: Recipe,
        residences: &[PointResidence],
    ) -> Result<QdrantMutationReceipt, HttpProviderError> {
        if recipe != self.recipe.version() {
            return Err(HttpProviderError::BindingMismatch);
        }
        let mut batches = 0;
        for batch in residences.chunks(self.transport.config.max_batch_points) {
            let body = DeleteRequest {
                points: batch
                    .iter()
                    .copied()
                    .map(|residence| PhysicalPointId::for_residence(workspace, recipe, residence))
                    .collect(),
            };
            let response = require_success(
                self.transport.request(
                    Method::Post,
                    &self
                        .transport
                        .collection_url("/points/delete?wait=true&ordering=strong"),
                    Some(&body),
                )?,
            )?;
            completed(&response.body)?;
            batches += 1;
        }
        Ok(QdrantMutationReceipt {
            points: residences.len(),
            batches,
        })
    }

    /// Observes the exact row count at `consistency=all` before minting an ANN source.
    ///
    /// # Errors
    /// Returns a typed error unless binding, authorized coverage, and actual row count agree.
    pub fn verify_projection(
        self,
        binding: Binding,
        coverage: CoverageWitness,
        expected_points: usize,
    ) -> Result<QdrantHttpSource, HttpProviderError> {
        if binding.recipe != self.recipe.version()
            || !matches!(coverage, CoverageWitness::Complete(_))
        {
            return Err(HttpProviderError::BindingMismatch);
        }
        let body = CountRequest {
            exact: true,
            filter: BindingFilter::new(binding),
        };
        let response = require_success(
            self.transport.request(
                Method::Post,
                &self
                    .transport
                    .collection_url("/points/count?consistency=all"),
                Some(&body),
            )?,
        )?;
        let observed: CountResponse = decode(&response.body)?;
        let expected = u64::try_from(expected_points).map_err(|_| HttpProviderError::SizeLimit)?;
        if observed.result.count != expected {
            return Err(HttpProviderError::IncompleteProjection {
                expected,
                observed: observed.result.count,
            });
        }
        Ok(QdrantHttpSource {
            client: self,
            binding,
            coverage,
        })
    }
}

impl AnnSource for QdrantHttpSource {
    type Error = HttpProviderError;

    fn fetch(&self, request: &VectorSearchRequest) -> Result<AnnPage, Self::Error> {
        if request.binding.base != self.binding
            || request.limit == 0
            || request.limit > self.client.transport.config.max_batch_points
            || request
                .cursor
                .is_some_and(|cursor| cursor.binding() != request.binding)
        {
            return Err(HttpProviderError::BindingMismatch);
        }
        let offset = request.cursor.map_or(0, AnnCursor::offset);
        let body = QueryRequest {
            query: request.query.values(),
            filter: BindingFilter::new(self.binding),
            params: SearchParams { exact: true },
            limit: request.limit,
            offset,
            with_payload: true,
            with_vector: false,
        };
        let response = require_success(
            self.client.transport.request(
                Method::Post,
                &self
                    .client
                    .transport
                    .collection_url("/points/query?consistency=all"),
                Some(&body),
            )?,
        )?;
        let decoded: QueryResponse = decode(&response.body)?;
        if decoded.result.points.len() > request.limit
            || decoded.result.points.len() > self.client.transport.config.max_batch_points
        {
            return Err(HttpProviderError::SizeLimit);
        }
        let mut ids = Vec::with_capacity(decoded.result.points.len());
        for point in decoded.result.points {
            let Some(candidate) = point.payload.candidate() else {
                return Err(HttpProviderError::BindingMismatch);
            };
            if !point.payload.matches_binding(self.binding)
                || point.id != point.payload.physical_id(self.binding, candidate)
            {
                return Err(HttpProviderError::BindingMismatch);
            }
            ids.push(candidate);
        }
        let next_offset = offset
            .checked_add(ids.len())
            .ok_or(HttpProviderError::SizeLimit)?;
        let next =
            (ids.len() == request.limit).then(|| AnnCursor::new(request.binding, next_offset));
        Ok(AnnPage {
            schema: SchemaVersion::CURRENT,
            binding: request.binding,
            ids,
            next,
            coverage: self.coverage,
            quality: SearchQuality::Exact,
        })
    }
}

#[derive(Clone)]
struct Transport {
    agent: ureq::Agent,
    config: QdrantHttpConfig,
}

impl Transport {
    fn collection_url(&self, suffix: &str) -> String {
        format!(
            "{}/collections/{}{}",
            self.config.endpoint, self.config.collection, suffix
        )
    }

    fn request<T: Serialize>(
        &self,
        method: Method,
        url: &str,
        body: Option<&T>,
    ) -> Result<Response, HttpProviderError> {
        let encoded = body.map(serde_json::to_vec).transpose()?;
        if encoded
            .as_ref()
            .is_some_and(|body| body.len() > self.config.max_request_bytes)
        {
            return Err(HttpProviderError::RequestTooLarge);
        }
        let maximum_attempts = self.config.attempts.get();
        let mut attempt = 1_u8;
        loop {
            let result = self.send(method, url, encoded.as_deref());
            match result {
                Ok(response) => match self.read(response) {
                    Ok(response) => {
                        if !retryable_status(response.status) || attempt == maximum_attempts {
                            return Ok(response);
                        }
                    }
                    Err(error) if attempt == maximum_attempts => return Err(error),
                    Err(_) => {}
                },
                Err(error) if attempt == maximum_attempts => return Err(error),
                Err(_) => {}
            }
            attempt = attempt.saturating_add(1);
        }
    }

    fn send(
        &self,
        method: Method,
        url: &str,
        body: Option<&[u8]>,
    ) -> Result<ureq::http::Response<ureq::Body>, HttpProviderError> {
        macro_rules! authenticate {
            ($request:expr) => {
                match &self.config.api_key {
                    Some(key) => $request.header("api-key", key.0.as_ref()),
                    None => $request,
                }
            };
        }
        let response = match method {
            Method::Get => authenticate!(self.agent.get(url)).call(),
            Method::Put => authenticate!(self.agent.put(url))
                .content_type("application/json")
                .send(body.unwrap_or_default()),
            Method::Post => authenticate!(self.agent.post(url))
                .content_type("application/json")
                .send(body.unwrap_or_default()),
        }?;
        Ok(response)
    }

    fn read(
        &self,
        mut response: ureq::http::Response<ureq::Body>,
    ) -> Result<Response, HttpProviderError> {
        let status = response.status().as_u16();
        let mut body = String::new();
        let limit = u64::try_from(self.config.max_response_bytes)
            .map_err(|_| HttpProviderError::SizeLimit)?;
        response
            .body_mut()
            .as_reader()
            .take(limit.saturating_add(1))
            .read_to_string(&mut body)?;
        if body.len() > self.config.max_response_bytes {
            return Err(HttpProviderError::ResponseTooLarge);
        }
        Ok(Response { status, body })
    }
}

#[derive(Clone, Copy)]
enum Method {
    Get,
    Put,
    Post,
}

#[derive(Debug)]
struct Response {
    status: u16,
    body: String,
}

fn require_success(response: Response) -> Result<Response, HttpProviderError> {
    if (200..300).contains(&response.status) {
        Ok(response)
    } else {
        Err(HttpProviderError::HttpStatus(response.status))
    }
}

fn retryable_status(status: u16) -> bool {
    matches!(status, 408 | 425 | 429) || status >= 500
}

fn decode<'body, T: Deserialize<'body>>(body: &'body str) -> Result<T, HttpProviderError> {
    serde_json::from_str(body).map_err(HttpProviderError::Decode)
}

fn completed(body: &str) -> Result<(), HttpProviderError> {
    let response: OperationResponse = decode(body)?;
    if response.result.status == "completed" {
        Ok(())
    } else {
        Err(HttpProviderError::RejectedOperation)
    }
}

/// Bounded HTTP/provider failure with no credential-bearing fields.
#[derive(Debug, thiserror::Error)]
pub enum HttpProviderError {
    /// Endpoint, collection, deadline, credential, or bound is invalid.
    #[error("invalid Qdrant HTTP configuration")]
    InvalidConfiguration,
    /// The configured coordinate representation is not accepted by this provider.
    #[error("Qdrant HTTP provider currently requires float32 coordinates")]
    UnsupportedEncoding,
    /// Collection dimension or distance disagrees with the embedding recipe.
    #[error("Qdrant collection schema disagrees with the embedding recipe")]
    CollectionMismatch,
    /// Request, document, payload, cursor, or projection binding disagrees.
    #[error("Qdrant request binding mismatch")]
    BindingMismatch,
    /// An existing point already stores different coordinates.
    #[error("Qdrant point already stores a different vector")]
    ImmutableVector,
    /// Actual remote point count does not cover the publication.
    #[error("Qdrant projection contains {observed} points; expected {expected}")]
    IncompleteProjection {
        /// Exact row count committed by the publication.
        expected: u64,
        /// Exact row count observed from Qdrant.
        observed: u64,
    },
    /// Request or response exceeded a configured bound.
    #[error("Qdrant operation exceeded a configured size bound")]
    SizeLimit,
    /// Decoded body exceeded the configured byte bound.
    #[error("Qdrant response exceeded the configured byte bound")]
    ResponseTooLarge,
    /// Encoded request body exceeded the configured byte bound.
    #[error("Qdrant request exceeded the configured byte bound")]
    RequestTooLarge,
    /// Service returned a terminal non-success status.
    #[error("Qdrant returned HTTP {0}")]
    HttpStatus(u16),
    /// A successful operation was not completed.
    #[error("Qdrant did not complete the requested operation")]
    RejectedOperation,
    /// JSON request encoding failed.
    #[error("Qdrant request encoding failed")]
    Encode(#[from] serde_json::Error),
    /// Successful response JSON was malformed.
    #[error("Qdrant response decoding failed")]
    Decode(serde_json::Error),
    /// HTTP transport failed after bounded retries.
    #[error("Qdrant transport failed")]
    Transport(#[from] ureq::Error),
    /// Bounded response read failed.
    #[error("Qdrant response read failed")]
    Read(#[from] std::io::Error),
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
enum Distance {
    Euclid,
    Cosine,
    Dot,
}

impl From<crate::Metric> for Distance {
    fn from(metric: crate::Metric) -> Self {
        match metric {
            crate::Metric::EuclideanSquared => Self::Euclid,
            crate::Metric::CosineDistance => Self::Cosine,
            crate::Metric::NegativeDot => Self::Dot,
        }
    }
}

#[derive(Serialize)]
struct CollectionCreate {
    vectors: VectorConfig,
}
#[derive(Serialize)]
struct VectorConfig {
    size: u32,
    distance: Distance,
}
#[derive(Deserialize)]
struct CollectionResponse {
    result: CollectionResult,
}
#[derive(Deserialize)]
struct CollectionResult {
    config: CollectionConfig,
}
#[derive(Deserialize)]
struct CollectionConfig {
    params: CollectionParams,
}
#[derive(Deserialize)]
struct CollectionParams {
    vectors: CollectionVectors,
}
#[derive(Deserialize)]
struct CollectionVectors {
    size: u32,
    distance: Distance,
}

#[derive(Serialize)]
struct UpsertRequest<'a> {
    points: Vec<UpsertPoint<'a>>,
}
#[derive(Serialize)]
struct UpsertPoint<'a> {
    id: PhysicalPointId,
    vector: &'a [f32],
    payload: PointPayload,
}
#[derive(Serialize)]
struct DeleteRequest {
    points: Vec<PhysicalPointId>,
}
#[derive(Serialize)]
struct RetrieveRequest {
    ids: Vec<PhysicalPointId>,
    with_payload: bool,
    with_vector: bool,
}
#[derive(Deserialize)]
struct RetrieveResponse {
    result: Vec<RetrievedPoint>,
}
#[derive(Deserialize)]
struct RetrievedPoint {
    id: PhysicalPointId,
    payload: Option<PointPayload>,
}
#[derive(Deserialize)]
struct OperationResponse {
    result: OperationResult,
}
#[derive(Deserialize)]
struct OperationResult {
    status: String,
}
#[derive(Serialize)]
struct CountRequest {
    exact: bool,
    filter: BindingFilter,
}
#[derive(Deserialize)]
struct CountResponse {
    result: CountResult,
}
#[derive(Deserialize)]
struct CountResult {
    count: u64,
}

#[derive(Serialize)]
struct QueryRequest<'a> {
    query: &'a [f32],
    filter: BindingFilter,
    params: SearchParams,
    limit: usize,
    offset: usize,
    with_payload: bool,
    with_vector: bool,
}
#[derive(Serialize)]
struct SearchParams {
    exact: bool,
}
#[derive(Deserialize)]
struct QueryResponse {
    result: QueryResult,
}
#[derive(Deserialize)]
struct QueryResult {
    points: Vec<QueryPoint>,
}
#[derive(Deserialize)]
struct QueryPoint {
    id: PhysicalPointId,
    payload: PointPayload,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct PointPayload {
    workspace: String,
    root: String,
    recipe: String,
    authority: String,
    read_manifest: String,
    frontier: String,
    candidate: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    coordinate_key: String,
    /// Stable row residence. Empty for a binding-scoped point.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    residence: String,
}

impl PointPayload {
    fn for_candidate(binding: Binding, candidate: CandidateId, values: &[f32]) -> Self {
        Self {
            workspace: hex(binding.workspace.as_bytes()),
            root: hex(binding.root.as_bytes()),
            recipe: hex(binding.recipe.as_bytes()),
            authority: hex(binding.authority.as_bytes()),
            read_manifest: hex(binding.read_manifest.as_bytes()),
            frontier: hex(binding.frontier.as_bytes()),
            candidate: format!("{:016x}", candidate.0),
            coordinate_key: coordinate_key(values),
            residence: String::new(),
        }
    }

    fn for_residence(
        binding: Binding,
        residence: PointResidence,
        candidate: CandidateId,
        values: &[f32],
    ) -> Self {
        let mut payload = Self::for_candidate(binding, candidate, values);
        payload.residence = hex(&residence.0);
        payload
    }

    fn matches_projection(&self, binding: Binding, candidate: CandidateId) -> bool {
        self.matches_binding(binding) && self.candidate() == Some(candidate)
    }

    fn physical_id(&self, binding: Binding, candidate: CandidateId) -> PhysicalPointId {
        match PointResidence::parse(&self.residence) {
            Some(residence) => {
                PhysicalPointId::for_residence(binding.workspace, binding.recipe, residence)
            }
            None => PhysicalPointId::for_candidate(binding, candidate),
        }
    }

    fn matches_binding(&self, binding: Binding) -> bool {
        self.workspace == hex(binding.workspace.as_bytes())
            && self.root == hex(binding.root.as_bytes())
            && self.recipe == hex(binding.recipe.as_bytes())
            && self.authority == hex(binding.authority.as_bytes())
            && self.read_manifest == hex(binding.read_manifest.as_bytes())
            && self.frontier == hex(binding.frontier.as_bytes())
    }

    fn candidate(&self) -> Option<CandidateId> {
        if self.candidate.len() != 16
            || !self
                .candidate
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return None;
        }
        let value = u64::from_str_radix(&self.candidate, 16).ok()?;
        CandidateId::new(value).ok()
    }
}

#[derive(Serialize)]
struct BindingFilter {
    must: Vec<FilterCondition>,
}
#[derive(Serialize)]
struct FilterCondition {
    key: &'static str,
    r#match: FilterMatch,
}
#[derive(Serialize)]
struct FilterMatch {
    value: String,
}

impl BindingFilter {
    fn new(binding: Binding) -> Self {
        Self {
            must: vec![
                condition("workspace", hex(binding.workspace.as_bytes())),
                condition("root", hex(binding.root.as_bytes())),
                condition("recipe", hex(binding.recipe.as_bytes())),
                condition("authority", hex(binding.authority.as_bytes())),
                condition("read_manifest", hex(binding.read_manifest.as_bytes())),
                condition("frontier", hex(binding.frontier.as_bytes())),
            ],
        }
    }
}

/// Qdrant's collection-global point identity, isolated by the complete logical binding.
///
/// The logical candidate remains the extension's compact `u64` relation key. It must not be
/// used directly as Qdrant's physical ID because separate workspace generations share one
/// collection and could otherwise overwrite each other before payload validation.
#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(transparent)]
struct PhysicalPointId(String);

impl PhysicalPointId {
    fn for_candidate(binding: Binding, candidate: CandidateId) -> Self {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"backend.qdrant.physical-point.v1\0");
        hasher.update(binding.workspace.as_bytes());
        hasher.update(binding.root.as_bytes());
        hasher.update(binding.recipe.as_bytes());
        hasher.update(binding.authority.as_bytes());
        hasher.update(binding.read_manifest.as_bytes());
        hasher.update(binding.frontier.as_bytes());
        hasher.update(&candidate.0.to_be_bytes());
        let digest = hasher.finalize();
        let bytes = &digest.as_bytes()[..16];
        Self(format!(
            "{}-{}-{}-{}-{}",
            hex(&bytes[..4]),
            hex(&bytes[4..6]),
            hex(&bytes[6..8]),
            hex(&bytes[8..10]),
            hex(&bytes[10..16]),
        ))
    }

    fn for_residence(workspace: WorkspaceRoot, recipe: Recipe, residence: PointResidence) -> Self {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"backend.qdrant.physical-residence.v1\0");
        hasher.update(workspace.as_bytes());
        hasher.update(recipe.as_bytes());
        hasher.update(&residence.0);
        let digest = hasher.finalize();
        let bytes = &digest.as_bytes()[..16];
        Self(format!(
            "{}-{}-{}-{}-{}",
            hex(&bytes[..4]),
            hex(&bytes[4..6]),
            hex(&bytes[6..8]),
            hex(&bytes[8..10]),
            hex(&bytes[10..16]),
        ))
    }
}

struct ResidenceProbe<'a> {
    id: PhysicalPointId,
    residence: PointResidence,
    write: CoordinateWrite,
    candidate: CandidateId,
    vector: &'a [f32],
    payload: PointPayload,
}

#[derive(Serialize)]
struct PayloadBatchRequest {
    operations: Vec<PayloadOperation>,
}

#[derive(Serialize)]
struct PayloadOperation {
    set_payload: SetPayload,
}

#[derive(Serialize)]
struct SetPayload {
    payload: PointPayload,
    points: Vec<PhysicalPointId>,
}

fn condition(key: &'static str, value: String) -> FilterCondition {
    FilterCondition {
        key,
        r#match: FilterMatch { value },
    }
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut result = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        result.push(char::from(DIGITS[usize::from(byte >> 4)]));
        result.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    result
}

fn coordinate_key(values: &[f32]) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"backend.qdrant.coordinate-key.v1\0");
    let len = u32::try_from(values.len()).unwrap_or(u32::MAX);
    hasher.update(&len.to_le_bytes());
    for value in values {
        hasher.update(&value.to_le_bytes());
    }
    hex(hasher.finalize().as_bytes())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CoordinateDisposition {
    /// Absent, or present without a coordinate key (legacy point). Write it.
    Due,
    /// Present and the coordinate key matches. Do not PUT.
    Unchanged,
    /// Present with a different coordinate key. Vectors are immutable.
    Conflict,
}

fn coordinate_disposition(expected: &str, observed: Option<&str>) -> CoordinateDisposition {
    match observed {
        None | Some("") => CoordinateDisposition::Due,
        Some(key) if key == expected => CoordinateDisposition::Unchanged,
        Some(_) => CoordinateDisposition::Conflict,
    }
}

#[cfg(test)]
mod tests;
