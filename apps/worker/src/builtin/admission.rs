//! Owner-admitted recipe request and closure/CAS state.

use super::{
    AdmittedAuthority, AuthorityEpoch, AuthorityExpectation, AuthorityVersion, BuiltinInputSchema,
    BuiltinProfile, BuiltinSemanticAuthority, CompleteSemanticCoverage,
    ExecutionRequestExpectation, ExecutionScopeId, ExpectedIdentity, IdContext,
    ImmutableObjectSchema, InputCas, JobAdmission, JobCancellation, ObjectKey,
    ObjectSummaryExpectation, ObjectVersion, OutputEquivalence, ProductRelation, ReadManifestId,
    RecipeId, RevocationVersion, RootSummaryExpectation, TransportLimits, TransportMessage,
    UntrustedSemanticCoverageClaim, WireAuthority, WireAuthorityPolicy, WireIdentity,
    WireRecipeRequest, WorkerError, WorkerJob, WorkerJobBindings, WorkerProcessError,
    builtin_workspace_root, product_dependency_manifest, profile_ids, relation_state,
    schema_object_key_identity_claim, schema_object_version_identity_claim,
    semantic_publication_row_bytes,
};
use std::num::NonZeroU64;

const LEGACY_SCOPE_ONE: NonZeroU64 = NonZeroU64::MIN;

#[derive(Debug)]
pub(super) struct BuiltinAdmission {
    pub(super) profile: BuiltinProfile,
    pub(super) limits: TransportLimits,
    pub(super) recipe: RecipeId,
    pub(super) read_manifest: ReadManifestId,
    pub(super) authority: AuthorityVersion,
    pub(super) equivalence: OutputEquivalence,
    pub(super) input_basis: backend_engine::WorkspaceRoot,
    pub(super) authority_claim: WireAuthority,
    pub(super) authority_expectation: AuthorityExpectation,
    pub(super) pending_authority: Option<AdmittedAuthority>,
    pub(super) semantic_authority: BuiltinSemanticAuthority,
    pub(super) input_cas: InputCas,
    pub(super) pending_root: Option<(backend_engine::MerkleRoot, [u8; 32], u64)>,
    pub(super) pending_page: Option<backend_engine::ClosurePageRequest>,
    pub(super) pending_pages: std::collections::VecDeque<backend_engine::ClosurePageRequest>,
    pub(super) pending_need_cursor: Option<u32>,
    /// Relation identity admitted from the synthetic closure manifest. The
    /// companion `ProductInput` identity is checked and installed directly into
    /// the input CAS, so it never enters the Merkle page frontier.
    pub(super) manifest_relation: Option<[u8; 32]>,
    /// Retained relation-node proof used to re-admit the execution root.
    pub(super) relation_proof: Option<Vec<u8>>,
    /// Untrusted manifest bytes retained until relation and authority evidence
    /// have been rebound through the version admission seam.
    pub(super) workspace_manifest: Option<backend_engine::UntrustedWorkspaceManifest>,
    /// Checked workspace closure retained for the final durable publish.
    pub(super) admitted_workspace_manifest: Option<backend_engine::CheckedWorkspaceManifest>,
}

impl BuiltinAdmission {
    pub(super) fn new(
        profile: BuiltinProfile,
        limits: TransportLimits,
        cas_path: impl AsRef<std::path::Path>,
    ) -> Result<Self, WorkerProcessError> {
        let ids = profile_ids(&profile).map_err(WorkerProcessError::Profile)?;
        let recipe = ids.recipe;
        let read_manifest = ids.read_manifest;
        let authority = ids.authority;
        let equivalence = ids.equivalence;
        let relation = relation_state(false, authority)
            .map_err(|error| WorkerProcessError::Profile(error.to_string()))?;
        let input_basis = builtin_workspace_root(&relation, authority)?;
        let authority_claim = WireAuthority::from_typed(&authority, AuthorityEpoch(1));
        let authority_expectation =
            AuthorityExpectation::from_typed(&authority, AuthorityEpoch(1), RevocationVersion(1));
        Ok(Self {
            profile,
            limits,
            recipe,
            read_manifest,
            authority,
            equivalence,
            input_basis,
            authority_claim,
            authority_expectation,
            pending_authority: None,
            semantic_authority: BuiltinSemanticAuthority { ids },
            input_cas: InputCas::open(cas_path, limits)
                .map_err(|error| WorkerProcessError::Profile(format!("open input CAS: {error}")))?,
            pending_root: None,
            pending_page: None,
            pending_pages: std::collections::VecDeque::new(),
            pending_need_cursor: None,
            manifest_relation: None,
            relation_proof: None,
            workspace_manifest: None,
            admitted_workspace_manifest: None,
        })
    }

    #[allow(clippy::too_many_lines)]
    fn admit_request(
        &mut self,
        request: &WireRecipeRequest,
    ) -> Result<WorkerJob<ProductRelation>, WorkerError> {
        let recipe_claim = WireIdentity::from_typed(&self.recipe);
        let read_claim = WireIdentity::from_typed(&self.read_manifest);
        let expected_authority = WireAuthorityPolicy {
            id: self.authority_claim.id,
            minimum_epoch: AuthorityEpoch(1),
            revocation_version: RevocationVersion(1),
        };
        let (input_root, input_basis) = match self.profile {
            BuiltinProfile::Product => {
                let input = request
                    .inputs
                    .first()
                    .copied()
                    .ok_or(WorkerError::InputProof("recipe input claim"))?;
                if request.inputs.len() != 1
                    || input.context() != IdContext::schema::<BuiltinInputSchema>()
                {
                    return Err(WorkerError::InputProof("recipe input schema"));
                }
                let (_, bytes) = self
                    .input_cas
                    .get_claim(input)
                    .map_err(|_| WorkerError::InputProof("recipe input CAS read"))?
                    .ok_or(WorkerError::InputProof("recipe input CAS presence"))?;
                let root_bytes: [u8; 32] = bytes
                    .as_ref()
                    .try_into()
                    .map_err(|_| WorkerError::InputProof("recipe input root bytes"))?;
                let relation_claim = backend_engine::UntrustedId::<ProductRelation>::from_wire(
                    &root_bytes,
                    IdContext::relation::<ProductRelation>(),
                )
                .map_err(|_| WorkerError::InputProof("recipe relation identity"))?;
                let persisted_proof = self
                    .input_cas
                    .node_proof(root_bytes)
                    .map_err(WorkerError::Replication)?;
                let relation_proof = self
                    .relation_proof
                    .as_deref()
                    .or(persisted_proof.as_deref())
                    .ok_or(WorkerError::InputProof("recipe relation proof presence"))?;
                let input_root = backend_engine::PersistedTreeRoot::<ProductRelation>::admit(
                    relation_claim,
                    relation_proof,
                )
                .map_err(|_| WorkerError::InputProof("recipe relation root"))?
                .root();
                (
                    input_root,
                    self.input_cas
                        .admitted_workspace()
                        .ok_or(WorkerError::InputProof("recipe workspace presence"))?,
                )
            }
            BuiltinProfile::EchoFixture => {
                let relation = relation_state(false, self.authority)?;
                (relation.root(), self.input_basis)
            }
        };
        let identity = backend_engine::VersionedWorkIdentity::new(
            self.recipe,
            input_root,
            self.read_manifest,
            self.authority,
            self.equivalence,
        );
        let work_key = identity.work_key();
        let work_claim = WireIdentity::from_typed(&work_key);
        let (expected_inputs, expected_wire_inputs) = if self.profile == BuiltinProfile::Product {
            let (version, _) = self
                .input_cas
                .get_claim(request.inputs[0])
                .map_err(|_| WorkerError::InputMismatch)?
                .ok_or(WorkerError::InputMismatch)?;
            (
                vec![ExpectedIdentity::from_typed(&version)],
                vec![WireIdentity::from_typed(&version)],
            )
        } else {
            (Vec::new(), Vec::new())
        };
        let mismatch = if request.recipe != recipe_claim {
            Some("recipe")
        } else if request.read_manifest != read_claim {
            Some("read manifest")
        } else if request.work_key != work_claim {
            Some("work identity")
        } else if request.input_basis.as_bytes() != *input_basis.as_bytes() {
            Some("workspace")
        } else if request.inputs != expected_wire_inputs {
            Some("input identity")
        } else if request.scope != ExecutionScopeId::from_legacy_ordinal(LEGACY_SCOPE_ONE) {
            Some("scope")
        } else if request.authority != expected_authority {
            Some("authority")
        } else {
            None
        };
        if let Some(mismatch) = mismatch {
            return Err(WorkerError::InputProof(mismatch));
        }
        let identity = backend_engine::VersionedWorkIdentity::new(
            self.recipe,
            input_root,
            self.read_manifest,
            self.authority,
            self.equivalence,
        );
        let work_key = identity.work_key();
        let claim = UntrustedSemanticCoverageClaim::complete_claim(
            &identity,
            1,
            self.semantic_authority.ids.witness,
            AuthorityEpoch(1),
            RevocationVersion(1),
        );
        let semantic = if self.semantic_authority.ids.include_basis {
            let manifest = product_dependency_manifest(
                profile_ids(&BuiltinProfile::Product).map_err(|_| WorkerError::SemanticCoverage)?,
            )
            .map_err(|_| WorkerError::SemanticCoverage)?;
            CompleteSemanticCoverage::admit_with_manifest(
                &identity,
                claim,
                manifest,
                &self.semantic_authority,
            )
            .map_err(|_| WorkerError::SemanticCoverage)?
        } else {
            CompleteSemanticCoverage::admit(&identity, claim, &self.semantic_authority)
                .map_err(|_| WorkerError::SemanticCoverage)?
        };
        let expected = ExecutionRequestExpectation {
            attempt: request.attempt,
            recipe: ExpectedIdentity::from_typed(&self.recipe),
            work_key: ExpectedIdentity::from_typed(&work_key),
            inputs: expected_inputs,
            read_manifest: ExpectedIdentity::from_typed(&self.read_manifest),
            scope: ExecutionScopeId::from_legacy_ordinal(LEGACY_SCOPE_ONE),
            authority: self.authority_expectation,
            resources: request.resources,
            fence: request.fence,
            input_basis,
            cancellation: request.cancellation,
        };
        let admitted_inputs = if self.profile == BuiltinProfile::Product {
            let identity = request
                .inputs
                .first()
                .copied()
                .ok_or(WorkerError::InputMismatch)?;
            let (_, bytes) = self
                .input_cas
                .get_claim(identity)
                .map_err(|_| WorkerError::InputMismatch)?
                .ok_or(WorkerError::InputMismatch)?;
            vec![backend_engine::worker::AdmittedInput {
                identity,
                bytes: bytes.as_ref().into(),
            }]
            .into_boxed_slice()
        } else {
            Vec::new().into_boxed_slice()
        };
        let cancellation = JobCancellation::new();
        Ok(WorkerJob::with_cancellation(
            WorkerJobBindings {
                expected,
                input_basis,
                recipe: self.recipe,
                read_manifest: self.read_manifest,
                authority: self.authority,
                output_equivalence: self.equivalence,
                work_key,
                inputs: admitted_inputs,
                semantic,
                observed_revocation: RevocationVersion(1),
                resources: request.resources,
                relation: core::marker::PhantomData,
            },
            cancellation,
        ))
    }

    fn finish_closure(
        &mut self,
        root: backend_engine::MerkleRoot,
        correlation: u64,
    ) -> Result<TransportMessage, WorkerError> {
        if self.relation_proof.is_none() {
            return Err(WorkerError::InputProof("relation proof completion"));
        }
        if self
            .input_cas
            .input_claim(root)
            .map_err(WorkerError::Replication)?
            .is_none()
        {
            return Err(WorkerError::InputProof("input claim completion"));
        }
        self.input_cas
            .record_workspace_manifest(
                self.admitted_workspace_manifest
                    .as_ref()
                    .ok_or(WorkerError::InputProof("admitted workspace closure"))?,
            )
            .map_err(WorkerError::Replication)?;
        self.input_cas
            .record_root(root)
            .map_err(WorkerError::Replication)?;
        self.pending_root = None;
        Ok(TransportMessage::ClosureRootAck(
            backend_engine::ClosureRootAck {
                correlation,
                root,
                warm: true,
                missing: Vec::new(),
                next: None,
            },
        ))
    }
}

impl JobAdmission<ProductRelation> for BuiltinAdmission {
    fn admit(
        &mut self,
        request: &WireRecipeRequest,
    ) -> Result<WorkerJob<ProductRelation>, WorkerError> {
        self.admit_request(request)
    }

    #[allow(clippy::too_many_lines)]
    fn admit_control(
        &mut self,
        message: TransportMessage,
    ) -> Result<Option<TransportMessage>, WorkerError> {
        if let TransportMessage::Chunk(frame) = message {
            let root = self
                .pending_root
                .map(|pending| pending.0)
                .ok_or(WorkerError::InputProof("chunk closure root"))?;
            if !self
                .input_cas
                .declared_missing(
                    root,
                    schema_object_version_identity_claim::<ImmutableObjectSchema>(
                        frame.version.as_bytes(),
                    )
                    .map_err(|_| WorkerError::InputProof("chunk version identity"))?,
                )
                .map_err(WorkerError::Replication)?
            {
                return Err(WorkerError::InputProof("declared missing object"));
            }
            let frame_version = frame.version;
            self.input_cas
                .ingest_complete_frame(frame, self.authority_claim)
                .map_err(WorkerError::Replication)?;
            self.input_cas
                .mark_received(
                    root,
                    schema_object_version_identity_claim::<ImmutableObjectSchema>(
                        frame_version.as_bytes(),
                    )
                    .map_err(|_| WorkerError::InputProof("received object identity"))?,
                )
                .map_err(WorkerError::Replication)?;
            if let Some((root, _workspace, correlation)) = self.pending_root
                && self
                    .input_cas
                    .missing_complete(root)
                    .map_err(WorkerError::Replication)?
            {
                return self.finish_closure(root, correlation).map(Some);
            }
            return Ok(None);
        }
        if let TransportMessage::ClosureRootOffer(offer) = message {
            offer
                .validate(self.limits)
                .map_err(WorkerError::Replication)?;
            let admitted_authority = self
                .authority_expectation
                .admit_capability(offer.authority, RevocationVersion(1))
                .map_err(WorkerError::Replication)?;
            let manifest =
                backend_engine::WorkspaceManifest::decode_untrusted(&offer.workspace_manifest)
                    .map_err(|_| WorkerError::InputMismatch)?;
            // The decoded descriptor is still untrusted and intentionally has
            // no `root()` capability.  Compare the raw workspace claim only
            // after relation and authority evidence have been rebound through
            // `admit_checked` below.
            if manifest.relations().len() != 1 || !manifest.basis().is_empty() {
                return Err(WorkerError::InputMismatch);
            }
            let durable_root_matches = self.input_cas.durable_root_matches(offer.root);
            if (self.input_cas.admitted_root() == Some(offer.root) || durable_root_matches)
                && self.input_cas.admitted_workspace_claim() == Some(offer.workspace.as_bytes())
                && self
                    .input_cas
                    .workspace_manifest_matches(&offer.workspace_manifest)
                && self
                    .input_cas
                    .input_claim(offer.root)
                    .map_err(WorkerError::Replication)?
                    .is_some_and(|claim| self.input_cas.contains_claim(claim))
            {
                if durable_root_matches {
                    self.input_cas
                        .admit_durable_root(offer.root)
                        .map_err(WorkerError::Replication)?;
                }
                if self.input_cas.admitted_workspace().is_none() {
                    self.input_cas
                        .reopen_workspace(self.authority, &admitted_authority)
                        .map_err(WorkerError::Replication)?;
                }
                self.pending_authority = Some(admitted_authority);
                return Ok(Some(TransportMessage::ClosureRootAck(
                    backend_engine::ClosureRootAck {
                        correlation: offer.correlation,
                        root: offer.root,
                        warm: true,
                        missing: Vec::new(),
                        next: None,
                    },
                )));
            }
            // The offer is an authenticated namespace claim, not a hint for a
            // fixture relation.  The sender's page source is authoritative for
            // the actual root contents; we learn object identities only from
            // those pages and admit their bytes through the typed CAS.  This
            // keeps the worker independent of the sender's relation shape and
            // makes arbitrary schema-valid roots first-class.
            let workspace = offer.workspace.as_bytes();
            self.input_cas.clear_missing(offer.root);
            self.pending_root = Some((offer.root, workspace, offer.correlation));
            self.pending_need_cursor = None;
            self.pending_pages.clear();
            self.manifest_relation = None;
            self.relation_proof = None;
            self.admitted_workspace_manifest = None;
            self.workspace_manifest = Some(manifest);
            self.pending_authority = Some(admitted_authority);
            let request = backend_engine::ClosurePageRequest {
                correlation: offer.correlation,
                request: backend_engine::MerklePageRequest {
                    root: offer.root,
                    node: offer.root.digest(),
                    cursor: backend_engine::PageCursor::origin(),
                    max_items: u16::try_from(self.limits.max_objects.min(64))
                        .map_err(|_| WorkerError::InputMismatch)?,
                },
            };
            self.pending_page = Some(request);
            return Ok(Some(TransportMessage::ClosurePageRequest(request)));
        }
        if let TransportMessage::ClosureNeedRequest(request) = message {
            request
                .validate(self.limits)
                .map_err(WorkerError::Replication)?;
            let Some((root, _workspace, correlation)) = self.pending_root else {
                return Err(WorkerError::InputMismatch);
            };
            if request.correlation != correlation
                || request.root != root
                || self.pending_need_cursor != Some(request.cursor)
            {
                return Err(WorkerError::InputMismatch);
            }
            let (missing, next) = self
                .input_cas
                .missing_batch(root, request.cursor, self.limits.max_objects)
                .map_err(WorkerError::Replication)?;
            self.pending_need_cursor = next;
            return Ok(Some(TransportMessage::ClosureRootAck(
                backend_engine::ClosureRootAck {
                    correlation,
                    root,
                    warm: false,
                    missing,
                    next,
                },
            )));
        }
        let TransportMessage::WireRootSummary(summary) = message else {
            return Ok(None);
        };
        if self.profile == BuiltinProfile::Product {
            return Err(WorkerError::Capability);
        }
        // A durable workspace marker is an O(1) warm-root proof. It lets the
        // worker acknowledge a reconnect without rebuilding the relation or
        // scanning the closure index; the recipe's exact input lookup remains
        // the final typed admission gate.
        if let Some(workspace) = self.input_cas.admitted_workspace()
            && summary.workspace.admit(workspace).is_ok()
            && summary.schema == 1
        {
            self.authority_expectation
                .admit(summary.authority, RevocationVersion(1))
                .map_err(WorkerError::Replication)?;
            let mut warm = summary;
            warm.objects.clear();
            return Ok(Some(TransportMessage::WireRootSummary(warm)));
        }
        let (relation, workspace) = match self.profile {
            BuiltinProfile::EchoFixture => {
                let relation = relation_state(false, self.authority)?;
                let workspace = self.input_basis;
                summary
                    .workspace
                    .admit(workspace)
                    .map_err(WorkerError::Replication)?;
                (relation, workspace)
            }
            BuiltinProfile::Product => return Err(WorkerError::Capability),
        };
        let mut objects = relation
            .iter()
            .map(|(key, value)| {
                let row = semantic_publication_row_bytes(key, value);
                let object_key = ObjectKey::<ImmutableObjectSchema>::from_value(&row);
                let object_version = ObjectVersion::<ImmutableObjectSchema>::from_value(&row);
                ObjectSummaryExpectation {
                    key: object_key,
                    version: object_version,
                    len: row.len() as u64,
                }
            })
            .collect::<Vec<_>>();
        let input = relation.root().to_bytes();
        objects.push(ObjectSummaryExpectation {
            key: ObjectKey::<ImmutableObjectSchema>::from_value(&input),
            version: ObjectVersion::<ImmutableObjectSchema>::from_value(&input),
            len: input.len() as u64,
        });
        objects.sort_by_key(|object| object.key.to_bytes());
        let expected = RootSummaryExpectation {
            schema: 1,
            workspace,
            relations: Vec::new(),
            objects,
        };
        let admitted = summary
            .admit_with_authority(
                &expected,
                self.authority_expectation,
                RevocationVersion(1),
                self.limits,
            )
            .map_err(WorkerError::Replication)?;
        self.input_cas
            .record_workspace(admitted.workspace())
            .map_err(WorkerError::Replication)?;
        // The summary admission above authenticates the complete relation
        // closure.  Its object list is already bounded by the negotiated
        // summary limits, so it can be sent back as checked evidence.
        Ok(Some(TransportMessage::RootSummary(admitted)))
    }

    #[allow(clippy::too_many_lines)]
    fn admit_page_response(
        &mut self,
        response: backend_engine::ClosurePageResponse,
    ) -> Result<Option<TransportMessage>, WorkerError> {
        let request = self.pending_page.take().ok_or(WorkerError::InputMismatch)?;
        response
            .admit_against(request, self.limits)
            .map_err(WorkerError::Replication)?;
        self.admit_page_proof(&response)?;
        let continuation = response
            .page
            .next
            .map(|cursor| backend_engine::ClosurePageRequest {
                correlation: request.correlation,
                request: backend_engine::MerklePageRequest {
                    root: request.request.root,
                    node: request.request.node,
                    cursor,
                    max_items: request.request.max_items,
                },
            });
        match response.page.body {
            backend_engine::MerklePageBody::Branch(children) => {
                for child in children.into_iter().rev() {
                    if self.input_cas.contains_node(child.digest.as_bytes()) {
                        continue;
                    }
                    if self.pending_pages.len() >= self.limits.max_objects {
                        return Err(WorkerError::Replication(
                            backend_engine::ReplicationError::Backpressure,
                        ));
                    }
                    self.pending_pages
                        .push_front(backend_engine::ClosurePageRequest {
                            correlation: request.correlation,
                            request: backend_engine::MerklePageRequest {
                                root: request.request.root,
                                node: child.digest,
                                cursor: backend_engine::PageCursor::origin(),
                                max_items: request.request.max_items,
                            },
                        });
                }
                if let Some(next) = continuation {
                    if self.pending_pages.len() >= self.limits.max_objects {
                        return Err(WorkerError::Replication(
                            backend_engine::ReplicationError::Backpressure,
                        ));
                    }
                    self.pending_pages.push_front(next);
                }
                if let Some(next) = self.pending_pages.pop_front() {
                    self.pending_page = Some(next);
                    Ok(Some(TransportMessage::ClosurePageRequest(next)))
                } else {
                    self.pending_page = None;
                    let (missing, next) = self
                        .input_cas
                        .missing_batch(request.request.root, 0, self.limits.max_objects)
                        .map_err(WorkerError::Replication)?;
                    self.pending_need_cursor = next;
                    if missing.is_empty() && next.is_none() {
                        return self
                            .finish_closure(request.request.root, request.correlation)
                            .map(Some);
                    }
                    Ok(Some(TransportMessage::ClosureRootAck(
                        backend_engine::ClosureRootAck {
                            correlation: request.correlation,
                            root: request.request.root,
                            warm: false,
                            missing,
                            next,
                        },
                    )))
                }
            }
            backend_engine::MerklePageBody::Leaf(entries) => {
                if entries.len() > self.limits.max_objects {
                    return Err(WorkerError::Replication(
                        backend_engine::ReplicationError::CoverageLimit,
                    ));
                }
                for entry in entries {
                    schema_object_version_identity_claim::<ImmutableObjectSchema>(&entry.version)
                        .map_err(|_| WorkerError::InputMismatch)?;
                    schema_object_key_identity_claim::<ImmutableObjectSchema>(&entry.key_id)
                        .map_err(|_| WorkerError::InputMismatch)?;
                }
                if let Some(next) = continuation {
                    if self.pending_pages.len() >= self.limits.max_objects {
                        return Err(WorkerError::Replication(
                            backend_engine::ReplicationError::Backpressure,
                        ));
                    }
                    self.pending_pages.push_front(next);
                }
                if let Some(next) = self.pending_pages.pop_front() {
                    self.pending_page = Some(next);
                    return Ok(Some(TransportMessage::ClosurePageRequest(next)));
                }
                self.pending_page = None;
                let (missing, next) = self
                    .input_cas
                    .missing_batch(request.request.root, 0, self.limits.max_objects)
                    .map_err(WorkerError::Replication)?;
                self.pending_need_cursor = next;
                if missing.is_empty() && next.is_none() {
                    return self
                        .finish_closure(request.request.root, request.correlation)
                        .map(Some);
                }
                Ok(Some(TransportMessage::ClosureRootAck(
                    backend_engine::ClosureRootAck {
                        correlation: request.correlation,
                        root: request.request.root,
                        warm: false,
                        missing,
                        next,
                    },
                )))
            }
        }
    }
}
