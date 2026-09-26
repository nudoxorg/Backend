//! Admits one owner dispatch transition for a checked wire recipe request.

use super::*;

impl BuiltinReplication {
    #[expect(
        clippy::too_many_lines,
        reason = "dispatch admission is one checked owner transition"
    )]
    pub(in crate::builtin::replication) fn dispatch(
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
        let semantic_snapshot = (profile == BuiltinProfile::Product)
            .then(|| backend_engine::ProductSemanticPublicationSnapshot::from_workspace(&snapshot))
            .transpose()
            .map_err(|error| {
                crate::protocol::ProtocolError::InvalidCommand(format!(
                    "open selected semantic publication plane: {error}"
                ))
            })?;
        if profile == BuiltinProfile::Product
            && semantic_snapshot
                .as_ref()
                .is_none_or(|source| source.authority_bytes() != ids.authority.as_bytes())
        {
            return Err(crate::protocol::ProtocolError::InvalidCommand(
                "selected semantic publication authority does not match the recipe authority"
                    .to_owned(),
            ));
        }
        let relation = match profile {
            BuiltinProfile::Product => None,
            BuiltinProfile::EchoFixture => Some(
                backend_engine::semantic_publication_fixture_with_authority(false, ids.authority)
                    .map_err(crate::protocol::ProtocolError::InvalidCommand)?,
            ),
        };
        let relation_root = semantic_snapshot
            .as_ref()
            .map(backend_engine::ProductSemanticPublicationSnapshot::relation_root)
            .or_else(|| relation.as_ref().map(RelationState::root))
            .ok_or(crate::protocol::ProtocolError::InvalidControl(
                "missing selected semantic publication relation",
            ))?;
        let input_basis = match profile {
            BuiltinProfile::Product => {
                let source = semantic_snapshot.as_ref().ok_or(
                    crate::protocol::ProtocolError::InvalidControl(
                        "missing semantic publication plane",
                    ),
                )?;
                backend_engine::semantic_execution_input_basis_from_snapshot(source, ids.authority)
                    .map_err(crate::protocol::ProtocolError::InvalidCommand)?
            }
            BuiltinProfile::EchoFixture => backend_engine::semantic_execution_input_basis(
                relation
                    .as_ref()
                    .ok_or(crate::protocol::ProtocolError::InvalidControl(
                        "missing echo semantic relation",
                    ))?,
                ids.authority,
            )
            .map_err(crate::protocol::ProtocolError::InvalidCommand)?,
        };
        let refresh = self
            .refresh_choice(semantic_snapshot.as_ref(), relation_root)
            .map_err(crate::protocol::ProtocolError::InvalidCommand)?;
        let placement = if profile == BuiltinProfile::Product {
            let retention = semantic_snapshot
                .as_ref()
                .map(backend_engine::ProductSemanticPublicationSnapshot::retention_facts)
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
        let expected_input = if ids.include_basis {
            let source = semantic_snapshot.as_ref().ok_or(
                crate::protocol::ProtocolError::InvalidControl(
                    "missing semantic publication plane",
                ),
            )?;
            vec![ExpectedInput::from_typed(
                &backend_engine::semantic_input_version(source.input()),
            )]
        } else {
            Vec::new()
        };
        let resources = execution_resources(ids);
        let expected_authority = WireAuthorityPolicy {
            id: authority_claim,
            minimum_epoch: AuthorityEpoch(1),
            revocation_version: RevocationVersion(1),
        };
        let mismatch = if request.recipe != recipe_claim {
            Some("recipe")
        } else if request.work_key != work_claim {
            Some("work identity")
        } else if request.read_manifest != read_claim {
            Some("read manifest")
        } else if request.scope
            != backend_engine::ExecutionScopeId::from_legacy_ordinal(LEGACY_SCOPE_ONE)
        {
            Some("scope")
        } else if request.inputs.len() != expected_input.len()
            || request
                .inputs
                .iter()
                .zip(&expected_input)
                .any(|(actual, expected)| actual != &expected.wire)
        {
            Some("input identity")
        } else if request.input_basis.as_bytes() != *input_basis.as_bytes() {
            Some("selected workspace")
        } else if request.authority != expected_authority {
            Some("authority fence")
        } else if request.resources != resources {
            Some("resource envelope")
        } else {
            None
        };
        if let Some(mismatch) = mismatch {
            return Err(crate::protocol::ProtocolError::InvalidCommand(format!(
                "builtin request {mismatch} does not match the registered recipe contract"
            )));
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
        let fallback_at = now
            .saturating_add(u64::try_from(self.worker_timeout.as_millis()).unwrap_or(u64::MAX))
            .max(now.saturating_add(1));
        let local = LocalCapability::admit(
            &identity,
            LocalState::Ready,
            now,
            expires_at,
            &|candidate: &VersionedWorkIdentity<BuiltinSemanticRelation>, state| {
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
            &|candidate: &VersionedWorkIdentity<BuiltinSemanticRelation>,
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
                .with_fallback(Some(fallback_at));
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
            let bytes = match (profile, semantic_snapshot.as_ref(), relation.as_ref()) {
                (BuiltinProfile::Product, Some(snapshot), _) => {
                    output_for_snapshot(ids, input_basis, snapshot).map_err(|error| {
                        crate::protocol::ProtocolError::InvalidCommand(format!(
                            "project local semantic publications: {error}"
                        ))
                    })?
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
        let semantic_snapshot =
            semantic_snapshot.ok_or(crate::protocol::ProtocolError::InvalidControl(
                "remote product dispatch requires checked semantic publications",
            ))?;
        let expected_root = crate::reconcile::ProductPageSource::from_workspace(
            input_basis,
            semantic_snapshot.relation().clone(),
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
                    snapshot: semantic_snapshot,
                },
            )
        } else {
            self.begin_closure(
                daemon,
                ClosurePlanRequest {
                    plan,
                    contract,
                    identity,
                    ids,
                    input_basis,
                    snapshot: semantic_snapshot,
                    expected_root,
                },
            )
        };
        if result.is_ok() {
            self.commit_refresh_observation(refresh);
        }
        result
    }
}
