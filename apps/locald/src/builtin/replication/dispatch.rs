//! Typed product dispatch admission and local fallback.
//!
//! This module is the execution boundary after the protocol and closure phases
//! have checked the registered profile, dependency manifest, placement, and
//! owner publication contract.

use super::{
    AuthorityClaim, AuthorityEpoch, BuiltinAuthorityVerifier, BuiltinModel, BuiltinProfile,
    BuiltinReplication, BuiltinValidator, ClosurePlanRequest, ClosureRootOffer, CostSnapshot,
    DispatchPlan, EngineStatus, ExpectedInput, LocalCapability, LocalState, OutputVersion,
    PendingClosure, PendingDispatch, PendingProduct, PlacementClass, ProductRelation,
    ProductSourceSnapshot, RebuildScope, RefreshChoice, RelationState, RemoteAuthorityPolicy,
    RemoteCapability, RemoteDispatchContract, RemotePlanRequest, RemoteState, ResourceEnvelope,
    ResourceVector, RevocationVersion, ScheduleRequest, TransportMessage,
    UntrustedSemanticCoverageClaim, VersionedWorkIdentity, WireAuthorityPolicy, WireIdentity,
    WireRecipeRequest, execution_input_basis, execution_resources, output_for_relation,
    output_for_snapshot, product_dependency_manifest, product_input_version,
    product_source_fixture_with_authority, validate_canonical_output,
};

impl BuiltinReplication {
    #[expect(
        clippy::too_many_lines,
        reason = "dispatch admission is one checked owner transition"
    )]
    pub(super) fn dispatch(
        &mut self,
        daemon: &mut crate::Locald<BuiltinModel, BuiltinValidator, BuiltinAuthorityVerifier>,
        request: &WireRecipeRequest,
    ) -> Result<EngineStatus, crate::protocol::ProtocolError> {
        request
            .validate(self.limits)
            .map_err(|error| crate::protocol::ProtocolError::InvalidCommand(error.to_string()))?;
        if request.recipe != self.profile.recipe_claim {
            return Err(crate::protocol::ProtocolError::InvalidCommand(
                "builtin request names a recipe outside the selected profile".to_owned(),
            ));
        }
        let profile = self.profile.kind;
        let ids = self.profile.ids;
        let snapshot = daemon.engine().daemon().owner().snapshot();
        let source_snapshot = (profile == BuiltinProfile::Product)
            .then(|| ProductSourceSnapshot::from_workspace(&snapshot))
            .transpose()
            .map_err(|error| {
                crate::protocol::ProtocolError::InvalidCommand(format!(
                    "open selected product source: {error}"
                ))
            })?;
        if profile == BuiltinProfile::Product
            && source_snapshot
                .as_ref()
                .is_none_or(|source| source.authority_bytes() != ids.authority.as_bytes())
        {
            return Err(crate::protocol::ProtocolError::InvalidCommand(
                "selected workspace authority does not match the product recipe authority"
                    .to_owned(),
            ));
        }
        let relation = match profile {
            BuiltinProfile::Product => None,
            BuiltinProfile::EchoFixture => Some(
                product_source_fixture_with_authority(false, ids.authority)
                    .map_err(crate::protocol::ProtocolError::InvalidCommand)?,
            ),
        };
        let relation_root = source_snapshot
            .as_ref()
            .map(ProductSourceSnapshot::relation_root)
            .or_else(|| relation.as_ref().map(RelationState::root))
            .ok_or(crate::protocol::ProtocolError::InvalidControl(
                "missing selected product relation",
            ))?;
        let input_basis = match profile {
            BuiltinProfile::Product => source_snapshot
                .as_ref()
                .ok_or(crate::protocol::ProtocolError::InvalidControl(
                    "missing product source",
                ))
                .map(ProductSourceSnapshot::workspace_root)
                .map_err(|error| {
                    crate::protocol::ProtocolError::InvalidCommand(error.to_string())
                })?,
            BuiltinProfile::EchoFixture => execution_input_basis(
                relation
                    .as_ref()
                    .ok_or(crate::protocol::ProtocolError::InvalidControl(
                        "missing echo relation",
                    ))?,
                ids.authority,
            )
            .map_err(crate::protocol::ProtocolError::InvalidCommand)?,
        };
        let refresh = self
            .refresh_choice(source_snapshot.as_ref(), relation_root)
            .map_err(crate::protocol::ProtocolError::InvalidCommand)?;
        let placement = if profile == BuiltinProfile::Product {
            let retention = source_snapshot
                .as_ref()
                .map(ProductSourceSnapshot::retention_facts)
                .unwrap_or_default();
            Self::placement_for_refresh(refresh, retention)
        } else {
            // Echo is an explicit compatibility fixture and never enters the
            // Product closure/remote route.
            PlacementClass::LocalPreferred
        };
        let identity = VersionedWorkIdentity::new(
            ids.recipe,
            relation_root,
            ids.read_manifest,
            ids.authority,
            ids.equivalence,
        );
        let recipe_claim = WireIdentity::from_typed(&identity.recipe);
        let read_claim = WireIdentity::from_typed(&identity.read_manifest);
        let work_claim = WireIdentity::from_typed(&identity.work_key());
        let authority_claim = WireIdentity::from_typed(&identity.authority);
        let expected_input =
            if ids.include_basis {
                let source = source_snapshot.as_ref().ok_or(
                    crate::protocol::ProtocolError::InvalidControl("missing product source"),
                )?;
                vec![ExpectedInput::from_typed(&product_input_version(
                    &source.input(),
                ))]
            } else {
                Vec::new()
            };
        let resources = execution_resources(ids);
        if request.recipe != recipe_claim
            || request.work_key != work_claim
            || request.read_manifest != read_claim
            || request.scope != 1
            || request.inputs
                != expected_input
                    .iter()
                    .map(|input| input.wire)
                    .collect::<Vec<_>>()
            || request.input_basis.as_bytes() != *input_basis.as_bytes()
            || request.authority
                != (WireAuthorityPolicy {
                    id: authority_claim,
                    minimum_epoch: AuthorityEpoch(1),
                    revocation_version: RevocationVersion(1),
                })
            || request.resources != resources
        {
            return Err(crate::protocol::ProtocolError::InvalidCommand(
                "builtin request does not match the registered recipe contract".to_owned(),
            ));
        }

        if (placement != PlacementClass::LocalPreferred
            || (self.worker_endpoint.is_some()
                && matches!(
                    refresh.choice,
                    RefreshChoice::Rebuild(RebuildScope::Product)
                )))
            && self.recovery_fallback.is_none()
        {
            // Reconnect attempts remain bounded for the current transport
            // generation. A request cannot reset the counter while the peer
            // is offline, so request floods cannot create an unbounded
            // connector stream. A successful handshake starts the next
            // generation and resets the counter in `poll_reconnect`.
            self.ensure_transport(daemon);
        }

        let negotiated = self
            .capabilities
            .negotiate(&self.capabilities, self.limits)
            .map_err(|error| crate::protocol::ProtocolError::InvalidCommand(error.to_string()))?;
        let semantic_claim = UntrustedSemanticCoverageClaim::complete_claim(
            &identity,
            1,
            ids.witness,
            AuthorityEpoch(1),
            RevocationVersion(1),
        );
        let semantic = if ids.include_basis {
            let manifest = product_dependency_manifest(ids)
                .map_err(crate::protocol::ProtocolError::InvalidCommand)?;
            daemon
                .engine()
                .daemon()
                .dispatcher()
                .admit_semantic_with_manifest(&identity, semantic_claim, manifest)
        } else {
            daemon
                .engine()
                .daemon()
                .dispatcher()
                .admit_semantic(&identity, semantic_claim)
        }
        .map_err(|error| crate::protocol::ProtocolError::InvalidCommand(error.to_string()))?;
        let now = Self::owner_now();
        let expires_at = now.saturating_add(30_000).max(now.saturating_add(1));
        let local = LocalCapability::admit(
            &identity,
            LocalState::Ready,
            now,
            expires_at,
            &|candidate: &VersionedWorkIdentity<ProductRelation>, state| {
                if *candidate == identity && state == LocalState::Ready {
                    Ok(())
                } else {
                    Err(backend_engine::ObservationError::Rejected)
                }
            },
        )
        .map_err(|error| crate::protocol::ProtocolError::InvalidCommand(error.to_string()))?;
        let remote = RemoteCapability::admit(
            &identity,
            &negotiated,
            now,
            expires_at,
            &|candidate: &VersionedWorkIdentity<ProductRelation>,
              capabilities: &backend_engine::NegotiatedCapabilities| {
                if *candidate == identity
                    && capabilities
                        .recipes
                        .iter()
                        .any(|(recipe, _)| *recipe == recipe_claim)
                {
                    Ok(
                        if self.recovery_fallback.is_some() || !self.transport_ready {
                            RemoteState::Unavailable
                        } else if self.remote_root.is_some() {
                            RemoteState::Warm
                        } else {
                            RemoteState::Available
                        },
                    )
                } else {
                    Err(backend_engine::ObservationError::Rejected)
                }
            },
        )
        .map_err(|error| crate::protocol::ProtocolError::InvalidCommand(error.to_string()))?;
        // A request starts with an explicit cold snapshot. Dispatcher planning
        // immediately replaces it with the owner-maintained model, so a
        // caller cannot steer this Product route with invented costs.
        let costs = CostSnapshot::cold_start(&identity, now)
            .map_err(|error| crate::protocol::ProtocolError::InvalidCommand(error.to_string()))?;
        let schedule_request =
            ScheduleRequest::new(identity, 1, now, placement, local, remote, costs)
                .with_charge(resources.output_bytes, resource_vector(resources))
                .pure(true)
                .with_fallback(Some(expires_at));
        let contract = RemoteDispatchContract {
            input_basis,
            inputs: expected_input,
            semantic: semantic.clone(),
            resources,
            authority_epoch: AuthorityEpoch(1),
            revocation_version: RevocationVersion(1),
            authority_policy: RemoteAuthorityPolicy::Signed,
            limits: self.limits,
            expected_receipt: None,
        };
        let plan = daemon
            .engine_mut()
            .plan_with_durable_reuse(schedule_request, semantic.clone(), now)
            .map_err(|error| crate::protocol::ProtocolError::InvalidCommand(error.to_string()))?;
        if let DispatchPlan::Reused(output) = &plan {
            self.complete_recovered_output(
                daemon,
                &backend_engine::DispatchCompletion::Reused(output.clone()),
            )?;
            self.commit_refresh_observation(refresh);
            return Ok(EngineStatus::Accepted);
        }
        if matches!(
            &plan,
            DispatchPlan::Scheduled(schedule)
                if schedule.decision() == backend_engine::PlacementDecision::Local
        ) {
            let bytes = match (profile, source_snapshot.as_ref(), relation.as_ref()) {
                (BuiltinProfile::Product, Some(snapshot), _) => {
                    output_for_snapshot(ids, input_basis, snapshot)
                }
                (_, _, Some(relation)) => output_for_relation(ids, input_basis, relation),
                _ => {
                    return Err(crate::protocol::ProtocolError::InvalidControl(
                        "missing local product source",
                    ));
                }
            };
            validate_canonical_output(
                OutputVersion::from_value(bytes.as_slice()),
                bytes.as_slice(),
            )
            .map_err(|error| {
                crate::protocol::ProtocolError::InvalidCommand(format!(
                    "local output CAS admission failed: {error}"
                ))
            })?;
            let completion = daemon
                .engine_mut()
                .complete_local(plan, bytes.as_slice(), semantic, now)
                .map_err(|error| {
                    crate::protocol::ProtocolError::InvalidCommand(error.to_string())
                })?;
            self.complete_recovered_output(daemon, &completion)?;
            self.commit_refresh_observation(refresh);
            return Ok(EngineStatus::Accepted);
        }
        if matches!(&plan, DispatchPlan::Waiting(_)) {
            return Err(crate::protocol::ProtocolError::InvalidControl(
                "remote dispatch cannot consume a waiting follower",
            ));
        }

        // Remote execution is the product closure path. It must retain the
        // owner-admitted lazy source all the way through page production.
        let source_snapshot =
            source_snapshot.ok_or(crate::protocol::ProtocolError::InvalidControl(
                "remote product dispatch requires a checked source snapshot",
            ))?;
        let expected_root = crate::reconcile::ProductPageSource::from_workspace(
            input_basis,
            source_snapshot.relation().clone(),
        )
        .map_err(|error| {
            crate::protocol::ProtocolError::InvalidCommand(format!(
                "construct expected closure root: {error}"
            ))
        })?
        .root();
        let result = if self.remote_root == Some(expected_root) && self.pending_closure.is_none() {
            self.dispatch_remote_plan(
                daemon,
                RemotePlanRequest {
                    plan,
                    contract,
                    identity,
                    ids,
                    input_basis,
                    snapshot: source_snapshot,
                },
            )
        } else {
            self.begin_closure(
                daemon,
                ClosurePlanRequest {
                    plan,
                    contract,
                    identity,
                    semantic,
                    ids,
                    input_basis,
                    snapshot: source_snapshot,
                    expected_root,
                },
            )
        };
        if result.is_ok() {
            self.commit_refresh_observation(refresh);
        }
        result
    }

    pub(super) fn dispatch_remote_plan(
        &mut self,
        daemon: &mut crate::Locald<BuiltinModel, BuiltinValidator, BuiltinAuthorityVerifier>,
        request: RemotePlanRequest,
    ) -> Result<EngineStatus, crate::protocol::ProtocolError> {
        if !self.pending.can_accept(request.identity.work_key()) {
            return Err(crate::protocol::ProtocolError::Backpressure);
        }
        let key = daemon
            .engine_mut()
            .daemon_mut()
            .dispatch_remote_pending(request.plan, request.contract.clone(), &self.capabilities)
            .map_err(|error| crate::protocol::ProtocolError::InvalidCommand(error.to_string()))?;
        let deadline = self.owner_deadline();
        let inserted = self.pending.insert(
            key,
            request.identity.work_key(),
            deadline,
            PendingProduct {
                ids: request.ids,
                input_basis: request.input_basis,
                snapshot: request.snapshot,
            },
        );
        if !inserted {
            return Err(crate::protocol::ProtocolError::Backpressure);
        }
        Ok(EngineStatus::Queued {
            bytes: usize::try_from(request.contract.resources.output_bytes)
                .map_or(usize::MAX, |value| value),
        })
    }

    pub(super) fn begin_closure(
        &mut self,
        daemon: &mut crate::Locald<BuiltinModel, BuiltinValidator, BuiltinAuthorityVerifier>,
        request: ClosurePlanRequest,
    ) -> Result<EngineStatus, crate::protocol::ProtocolError> {
        let ClosurePlanRequest {
            plan,
            contract,
            identity,
            semantic,
            ids,
            input_basis,
            snapshot,
            expected_root,
        } = request;
        if self.pending_dispatch.is_some() || self.pending_closure.is_some() {
            return Err(crate::protocol::ProtocolError::Backpressure);
        }
        let authority = AuthorityClaim::from_typed(&ids.authority, AuthorityEpoch(1));
        let source = crate::reconcile::ProductPageSource::from_workspace(
            input_basis,
            snapshot.relation().clone(),
        )
        .map_err(|error| crate::protocol::ProtocolError::InvalidCommand(error.to_string()))?;
        if source.root() != expected_root {
            return Err(crate::protocol::ProtocolError::InvalidControl(
                "closure source root changed during admission",
            ));
        }
        let correlation = self.next_correlation;
        self.next_correlation = self.next_correlation.checked_add(1).ok_or(
            crate::protocol::ProtocolError::InvalidControl("closure correlation space exhausted"),
        )?;
        let workspace_manifest = snapshot.manifest_bytes().to_vec();
        let offer = ClosureRootOffer {
            correlation,
            workspace: backend_engine::WorkspaceRootClaim::from_bytes(*input_basis.as_bytes()),
            root: expected_root,
            authority,
            workspace_manifest,
        };
        daemon
            .engine_mut()
            .send_remote_control(
                TransportMessage::ClosureRootOffer(offer),
                &self.capabilities,
            )
            .map_err(|error| crate::protocol::ProtocolError::InvalidCommand(error.to_string()))?;
        let input_value = snapshot.input();
        let input_version = product_input_version(&input_value).to_bytes();
        self.pending_dispatch = Some(PendingDispatch {
            plan,
            contract,
            identity,
            semantic,
            ids,
            input_basis,
            snapshot,
        });
        self.pending_closure = Some(PendingClosure {
            correlation,
            expected_root,
            source: Some(source),
            input_value,
            input_version,
            authority,
            frames: None,
            next_frame: 0,
            input: None,
            summary_admitted: false,
            awaiting_complete: false,
            awaiting_need: false,
            need_cursor: None,
            next_transfer: 1,
            needed_versions: None,
            deadline: self.owner_deadline(),
        });
        Ok(EngineStatus::Queued { bytes: 0 })
    }

    pub(super) fn complete_pending_locally(
        daemon: &mut crate::Locald<BuiltinModel, BuiltinValidator, BuiltinAuthorityVerifier>,
        pending: PendingDispatch,
    ) -> Result<(), crate::protocol::ProtocolError> {
        let bytes = output_for_snapshot(pending.ids, pending.input_basis, &pending.snapshot);
        validate_canonical_output(
            OutputVersion::from_value(bytes.as_slice()),
            bytes.as_slice(),
        )
        .map_err(|error| {
            crate::protocol::ProtocolError::InvalidCommand(format!(
                "fallback output CAS admission failed: {error}"
            ))
        })?;
        daemon
            .engine_mut()
            .complete_local_shared(
                pending.plan,
                bytes.into(),
                pending.semantic,
                Self::owner_now(),
            )
            .map(|_| ())
            .map_err(|error| {
                crate::protocol::ProtocolError::InvalidCommand(format!(
                    "complete local fallback: {error}"
                ))
            })
    }
}

fn resource_vector(resources: ResourceEnvelope) -> ResourceVector {
    ResourceVector {
        cpu_millis: resources.cpu_millis,
        memory_bytes: resources.memory_bytes,
        network_bytes: resources.network_bytes,
        storage_bytes: resources.storage_bytes,
    }
}
