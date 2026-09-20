//! Closure and input transfer phase of the owner remote state machine.

use super::{
    BuiltinAuthorityVerifier, BuiltinModel, BuiltinReplication, BuiltinValidator, ProductRelation,
    TransportMessage, WireIdentity,
};
use crate::reconcile::{product_frames_for_version_from_source, product_input_frame};
use backend_engine::ImmutableObjectSchema;

impl BuiltinReplication {
    fn respond_to_page_request(
        &self,
        daemon: &mut crate::Locald<BuiltinModel, BuiltinValidator, BuiltinAuthorityVerifier>,
        closure: &mut super::PendingClosure,
        request: backend_engine::ClosurePageRequest,
    ) -> Result<(), crate::protocol::ProtocolError> {
        request.validate(self.limits).map_err(|error| {
            crate::protocol::ProtocolError::InvalidCommand(format!(
                "validate worker closure page request: {error}"
            ))
        })?;
        if request.correlation != closure.correlation {
            return Err(crate::protocol::ProtocolError::InvalidCommand(
                "worker page request belongs to an earlier closure generation".to_owned(),
            ));
        }
        let source =
            closure
                .source
                .as_mut()
                .ok_or(crate::protocol::ProtocolError::InvalidControl(
                    "missing closure source",
                ))?;
        let page =
            backend_engine::MerklePageSource::page(source, request.request).map_err(|error| {
                crate::protocol::ProtocolError::InvalidCommand(format!(
                    "materialize authenticated closure page: {error}"
                ))
            })?;
        let proof = source.proof(page.node).map_err(|error| {
            crate::protocol::ProtocolError::InvalidCommand(format!(
                "load authenticated closure page proof: {error}"
            ))
        })?;
        daemon
            .engine_mut()
            .send_remote_control(
                TransportMessage::ClosurePageResponse(backend_engine::ClosurePageResponse {
                    correlation: request.correlation,
                    proof,
                    page,
                }),
                &self.capabilities,
            )
            .map_err(|error| {
                crate::protocol::ProtocolError::InvalidCommand(format!(
                    "send authenticated closure page: {error}"
                ))
            })?;
        Ok(())
    }

    #[expect(
        clippy::too_many_lines,
        reason = "closure polling is one owner-affine protocol state machine"
    )]
    pub(super) fn poll_closure(
        &mut self,
        daemon: &mut crate::Locald<BuiltinModel, BuiltinValidator, BuiltinAuthorityVerifier>,
    ) -> Result<bool, crate::protocol::ProtocolError> {
        let Some(mut closure) = self.pending_closure.take() else {
            return Ok(false);
        };
        if closure.deadline <= BuiltinReplication::owner_instant() {
            let pending = self.pending_dispatch.take();
            self.remote_root = None;
            if let Some(pending) = pending {
                self.fallback_pending_dispatch(daemon, pending)?;
            }
            return Ok(true);
        }
        if closure.awaiting_need {
            let response = match daemon.engine_mut().daemon_mut().receive_remote_control() {
                Ok(response) => response,
                Err(
                    backend_engine::DaemonError::RemoteControlBuffered
                    | backend_engine::DaemonError::RemoteResultUnmatched,
                ) => {
                    self.pending_closure = Some(closure);
                    return Ok(false);
                }
                Err(error) => {
                    self.remote_root = None;
                    return Err(crate::protocol::ProtocolError::InvalidCommand(format!(
                        "receive closure need batch: {error}"
                    )));
                }
            };
            if let TransportMessage::ClosurePageRequest(request) = response {
                self.respond_to_page_request(daemon, &mut closure, request)?;
                self.pending_closure = Some(closure);
                return Ok(true);
            }
            let TransportMessage::ClosureRootAck(ack) = response else {
                self.pending_closure = Some(closure);
                return Ok(false);
            };
            ack.validate(self.limits).map_err(|error| {
                crate::protocol::ProtocolError::InvalidCommand(error.to_string())
            })?;
            if ack.correlation != closure.correlation || ack.root != closure.expected_root {
                return Err(crate::protocol::ProtocolError::InvalidCommand(
                    "worker need acknowledgement did not match the offered root".to_owned(),
                ));
            }
            closure.needed_versions = Some(checked_missing_versions(
                closure.source.as_mut(),
                &ack.missing,
            )?);
            closure.need_cursor = ack.next;
            closure.awaiting_need = false;
            closure.frames = None;
            closure.next_frame = 0;
        }
        if closure.awaiting_complete {
            let response = match daemon.engine_mut().daemon_mut().receive_remote_control() {
                Ok(response) => response,
                Err(
                    backend_engine::DaemonError::RemoteControlBuffered
                    | backend_engine::DaemonError::RemoteResultUnmatched,
                ) => {
                    self.pending_closure = Some(closure);
                    return Ok(false);
                }
                Err(error) => {
                    self.remote_root = None;
                    return Err(crate::protocol::ProtocolError::InvalidCommand(format!(
                        "receive closure completion: {error}"
                    )));
                }
            };
            if let TransportMessage::ClosurePageRequest(request) = response {
                self.respond_to_page_request(daemon, &mut closure, request)?;
                self.pending_closure = Some(closure);
                return Ok(true);
            }
            let TransportMessage::ClosureRootAck(ack) = response else {
                self.pending_closure = Some(closure);
                return Ok(false);
            };
            ack.validate(self.limits).map_err(|error| {
                crate::protocol::ProtocolError::InvalidCommand(error.to_string())
            })?;
            if ack.correlation != closure.correlation
                || !ack.warm
                || ack.root != closure.expected_root
            {
                return Err(crate::protocol::ProtocolError::InvalidCommand(
                    "worker did not durably complete the offered closure".to_owned(),
                ));
            }
            let pending = self.pending_dispatch.take().ok_or(
                crate::protocol::ProtocolError::InvalidControl("missing dispatch"),
            )?;
            self.remote_root = Some(closure.expected_root);
            self.dispatch_remote_plan(daemon, pending.into())?;
            return Ok(true);
        }
        if !closure.summary_admitted {
            let response = match daemon.engine_mut().daemon_mut().receive_remote_control() {
                Ok(response) => response,
                Err(
                    backend_engine::DaemonError::RemoteControlBuffered
                    | backend_engine::DaemonError::RemoteResultUnmatched,
                ) => {
                    self.pending_closure = Some(closure);
                    return Ok(false);
                }
                Err(error) => {
                    self.remote_root = None;
                    return Err(crate::protocol::ProtocolError::InvalidCommand(format!(
                        "receive closure summary: {error}"
                    )));
                }
            };
            if let TransportMessage::ClosurePageRequest(request) = response {
                self.respond_to_page_request(daemon, &mut closure, request)?;
                self.pending_closure = Some(closure);
                return Ok(true);
            }
            let TransportMessage::ClosureRootAck(ack) = response else {
                self.pending_closure = Some(closure);
                return Ok(false);
            };
            ack.validate(self.limits).map_err(|error| {
                crate::protocol::ProtocolError::InvalidCommand(error.to_string())
            })?;
            if ack.correlation != closure.correlation || ack.root != closure.expected_root {
                return Err(crate::protocol::ProtocolError::InvalidCommand(
                    "worker closure acknowledgement did not match the offered root".to_owned(),
                ));
            }
            closure.summary_admitted = true;
            closure.needed_versions = Some(checked_missing_versions(
                closure.source.as_mut(),
                &ack.missing,
            )?);
            let input_version = closure.input_version;
            if ack.warm {
                closure.needed_versions = Some(std::collections::BTreeSet::new());
                let pending = self.pending_dispatch.take().ok_or({
                    crate::protocol::ProtocolError::InvalidControl("missing dispatch")
                })?;
                self.remote_root = Some(closure.expected_root);
                self.dispatch_remote_plan(daemon, pending.into())?;
                return Ok(true);
            }
            if let Some(needed) = closure.needed_versions.as_mut() {
                // The relation input has one dedicated final transfer. Merkle
                // node preimages already carry the canonical rows, so the
                // object plane contains only this fixed-size descriptor.
                needed.remove(&input_version);
            }
            closure.frames = None;
            closure.input = None;
        }
        if closure.frames.is_none()
            && let Some(version) = closure
                .needed_versions
                .as_ref()
                .and_then(|versions| versions.iter().next().copied())
        {
            let source = closure.source.as_mut().ok_or({
                crate::protocol::ProtocolError::InvalidControl("missing closure source")
            })?;
            closure.frames = Some(
                product_frames_for_version_from_source(
                    source,
                    version,
                    closure.authority,
                    self.limits,
                    closure.next_transfer,
                )
                .map_err(|error| {
                    crate::protocol::ProtocolError::InvalidCommand(error.to_string())
                })?,
            );
            if let Some(needed) = closure.needed_versions.as_mut() {
                needed.remove(&version);
            }
            closure.next_transfer = closure
                .frames
                .as_ref()
                .and_then(|frames| frames.last())
                .map_or(closure.next_transfer, |frame| {
                    frame.transfer.get().saturating_add(1)
                });
        }
        let frame_count = closure.frames.as_ref().map_or(0, Vec::len);
        if closure.next_frame < frame_count {
            let frame = closure
                .frames
                .as_ref()
                .and_then(|frames| frames.get(closure.next_frame))
                .cloned()
                .ok_or(crate::protocol::ProtocolError::InvalidControl(
                    "closure frame",
                ))?;
            match daemon
                .engine_mut()
                .send_remote_control(TransportMessage::Chunk(frame), &self.capabilities)
            {
                Ok(_) => {}
                Err(
                    backend_engine::DaemonError::Replication(
                        backend_engine::ReplicationError::Backpressure,
                    )
                    | backend_engine::DaemonError::Backpressure,
                ) => {
                    self.pending_closure = Some(closure);
                    return Ok(false);
                }
                Err(error) => {
                    self.remote_root = None;
                    return Err(crate::protocol::ProtocolError::InvalidCommand(format!(
                        "send closure object transfer: {error}"
                    )));
                }
            }
            closure.next_frame = closure.next_frame.saturating_add(1);
            self.pending_closure = Some(closure);
            return Ok(true);
        }
        if closure.frames.take().is_some() {
            // The frame vector is the affine send cursor for exactly one
            // immutable object. Retire it as soon as its final chunk has been
            // queued so the following poll can materialize the next missing
            // version. Keeping an exhausted vector here would pin the prior
            // object's bytes and prevent the closure frontier from advancing.
            closure.next_frame = 0;
            self.pending_closure = Some(closure);
            return Ok(true);
        }
        if let Some(cursor) = closure.need_cursor.take() {
            let request = backend_engine::ClosureNeedRequest {
                correlation: closure.correlation,
                root: closure.expected_root,
                cursor,
            };
            match daemon.engine_mut().send_remote_control(
                TransportMessage::ClosureNeedRequest(request),
                &self.capabilities,
            ) {
                Ok(_) => {}
                Err(
                    backend_engine::DaemonError::Replication(
                        backend_engine::ReplicationError::Backpressure,
                    )
                    | backend_engine::DaemonError::Backpressure,
                ) => {
                    closure.need_cursor = Some(cursor);
                    self.pending_closure = Some(closure);
                    return Ok(false);
                }
                Err(error) => {
                    self.remote_root = None;
                    return Err(crate::protocol::ProtocolError::InvalidCommand(format!(
                        "request closure need batch: {error}"
                    )));
                }
            }
            closure.awaiting_need = true;
            self.pending_closure = Some(closure);
            return Ok(true);
        }
        if closure.input.is_none()
            && closure
                .needed_versions
                .as_ref()
                .is_none_or(std::collections::BTreeSet::is_empty)
        {
            closure.input = Some(
                product_input_frame(
                    closure.input_value,
                    closure.authority,
                    self.limits,
                    closure.next_transfer,
                )
                .map_err(|error| {
                    crate::protocol::ProtocolError::InvalidCommand(error.to_string())
                })?,
            );
        }
        if let Some(input) = closure.input.take() {
            match daemon
                .engine_mut()
                .send_remote_control(TransportMessage::Chunk(input.clone()), &self.capabilities)
            {
                Ok(_) => {}
                Err(
                    backend_engine::DaemonError::Replication(
                        backend_engine::ReplicationError::Backpressure,
                    )
                    | backend_engine::DaemonError::Backpressure,
                ) => {
                    closure.input = Some(input);
                    self.pending_closure = Some(closure);
                    return Ok(false);
                }
                Err(error) => {
                    self.remote_root = None;
                    return Err(crate::protocol::ProtocolError::InvalidCommand(format!(
                        "send recipe input transfer: {error}"
                    )));
                }
            }
            self.pending_closure = Some(closure);
            if let Some(closure) = self.pending_closure.as_mut() {
                closure.awaiting_complete = true;
            }
            return Ok(true);
        }
        closure.awaiting_complete = true;
        self.pending_closure = Some(closure);
        Ok(true)
    }
}

fn checked_missing_versions(
    source: Option<&mut crate::reconcile::ProductPageSource<ProductRelation>>,
    missing: &[WireIdentity],
) -> Result<std::collections::BTreeSet<[u8; 32]>, crate::protocol::ProtocolError> {
    let source = source.ok_or(crate::protocol::ProtocolError::InvalidControl(
        "missing closure source",
    ))?;
    missing
        .iter()
        .map(|identity| {
            let version = identity.as_bytes();
            let object_claim = backend_engine::schema_object_version_identity_claim::<
                ImmutableObjectSchema,
            >(&version)
            .map_err(|_| {
                crate::protocol::ProtocolError::InvalidCommand(
                    "worker requested a malformed object identity".to_owned(),
                )
            })?;
            if object_claim.context() != identity.context() {
                return Err(crate::protocol::ProtocolError::InvalidCommand(
                    "worker requested an object under the wrong schema context".to_owned(),
                ));
            }
            if source.object_bytes(version).is_none() {
                return Err(crate::protocol::ProtocolError::InvalidCommand(
                    "worker requested an object outside the admitted closure".to_owned(),
                ));
            }
            Ok(version)
        })
        .collect()
}
