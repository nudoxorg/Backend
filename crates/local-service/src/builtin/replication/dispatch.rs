//! Typed product dispatch admission and local fallback.
//!
//! This module is the execution boundary after the protocol and closure phases
//! have checked the registered profile, dependency manifest, placement, and
//! owner publication contract.

use super::{
    AuthorityClaim, AuthorityEpoch, BuiltinAuthorityVerifier, BuiltinModel, BuiltinProfile,
    BuiltinReplication, BuiltinSemanticRelation, BuiltinValidator, ClosurePlanRequest,
    ClosureRootOffer, CostSnapshot, DispatchPlan, EngineStatus, ExpectedInput, LocalCapability,
    LocalState, OutputVersion, PendingClosure, PendingDispatch, PendingProduct, PlacementClass,
    RebuildScope, RefreshChoice, RelationState, RemoteAuthorityPolicy, RemoteCapability,
    RemoteDispatchContract, RemotePlanRequest, RemoteState, ResourceEnvelope, ResourceVector,
    RevocationVersion, ScheduleRequest, TransportMessage, UntrustedSemanticCoverageClaim,
    VersionedWorkIdentity, WireAuthorityPolicy, WireIdentity, WireRecipeRequest,
    execution_resources, output_for_relation, output_for_snapshot, product_dependency_manifest,
    validate_canonical_output,
};
use std::num::NonZeroU64;

const LEGACY_SCOPE_ONE: NonZeroU64 = NonZeroU64::MIN;

#[path = "dispatch/admit.rs"]
mod admit;

impl BuiltinReplication {
    pub(super) fn dispatch_remote_plan(
        &mut self,
        daemon: &mut crate::Locald<BuiltinModel, BuiltinValidator, BuiltinAuthorityVerifier>,
        request: RemotePlanRequest,
    ) -> Result<EngineStatus, crate::protocol::ProtocolError> {
        if !self.pending.can_accept(request.identity.work_key()) {
            return Err(crate::protocol::ProtocolError::Backpressure);
        }
        let dispatched = daemon.engine_mut().daemon_mut().dispatch_remote_pending(
            request.plan,
            request.contract.clone(),
            &self.capabilities,
        );
        let key = match dispatched {
            Ok(key) => key,
            Err(error) => error
                .retained_remote_key()
                .ok_or_else(|| crate::protocol::ProtocolError::InvalidCommand(error.to_string()))?,
        };
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
        let workspace_manifest = backend_engine::semantic_execution_input_manifest_from_snapshot(
            &snapshot,
            ids.authority,
        )
        .map_err(crate::protocol::ProtocolError::InvalidCommand)?
        .encode();
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
        let input_version = backend_engine::semantic_input_version(input_value).to_bytes();
        self.pending_dispatch = Some(PendingDispatch {
            plan,
            contract,
            identity,
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

    pub(super) fn fallback_pending_dispatch(
        &mut self,
        daemon: &mut crate::Locald<BuiltinModel, BuiltinValidator, BuiltinAuthorityVerifier>,
        pending: PendingDispatch,
    ) -> Result<(), crate::protocol::ProtocolError> {
        let bytes = output_for_snapshot(pending.ids, pending.input_basis, &pending.snapshot)
            .map_err(|error| {
                crate::protocol::ProtocolError::InvalidCommand(format!(
                    "project fallback product source: {error}"
                ))
            })?;
        validate_canonical_output(
            OutputVersion::from_value(bytes.as_slice()),
            bytes.as_slice(),
        )
        .map_err(|error| {
            crate::protocol::ProtocolError::InvalidCommand(format!(
                "fallback output CAS admission failed: {error}"
            ))
        })?;
        let work_key = pending.identity.work_key();
        if !self.pending.can_accept(work_key) {
            return Err(crate::protocol::ProtocolError::Backpressure);
        }
        let key = daemon
            .engine_mut()
            .prepare_remote_pending(pending.plan, pending.contract)
            .map_err(|error| {
                crate::protocol::ProtocolError::InvalidCommand(format!(
                    "admit local fallback custody: {error}"
                ))
            })?;
        if !self.pending.insert(
            key,
            work_key,
            self.owner_deadline(),
            PendingProduct {
                ids: pending.ids,
                input_basis: pending.input_basis,
                snapshot: pending.snapshot,
            },
        ) {
            return Err(crate::protocol::ProtocolError::Backpressure);
        }
        match daemon
            .engine_mut()
            .fail_remote_pending(key, bytes.into(), Self::owner_now())
        {
            Ok(_) => {
                let _ = self.pending.remove(key);
                Ok(())
            }
            Err(error) => Err(crate::protocol::ProtocolError::InvalidCommand(format!(
                "publish local fallback: {error}"
            ))),
        }
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
