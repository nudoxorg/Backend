//! Sends bounded collection, upsert, delete, and projection checks to one Qdrant endpoint.

use super::*;

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
