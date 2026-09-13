//! Negotiation and owner-held remote request admission.

use super::{
    CapabilityManifest, Daemon, DaemonError, DispatchPlan, DispatchTicket,
    JOURNAL_CANCEL_SEND_FAILED, NegotiatedCapabilities, PendingRemoteKey, QueueSized, Relation,
    RemoteDispatchContract, RemoteSession, ReplicationError, TransportLimits, TransportMessage,
    WireRecipeRequest, WorkspaceModel,
};

impl<M, V, A> Daemon<M, V, A>
where
    M: WorkspaceModel,
    M::Intent: QueueSized,
    V: crate::dispatch::OutputAdmissionValidator + crate::dispatch::SemanticCoverageValidator,
    A: backend_replication::AttestationVerifier + Send + Sync + 'static,
{
    /// Starts a daemon-owned remote attempt and returns its correlation key.
    ///
    /// The daemon retains the affine request envelope and routes the eventual
    /// result through the same admission and publication path as local work.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn dispatch_remote<R: Relation + Send>(
        &mut self,
        plan: DispatchPlan<R>,
        contract: RemoteDispatchContract,
        local_capabilities: &CapabilityManifest,
    ) -> Result<PendingRemoteKey, DaemonError> {
        self.dispatch_remote_pending(plan, contract, local_capabilities)
    }

    /// Sends a remote request while returning its typed affine ticket to the
    /// caller. The ticket owns the exact plan and contract and must be passed
    /// to [`Self::receive_remote_result`], [`Self::fallback_remote`], or
    /// [`Self::cancel_remote`]. A failed send returns
    /// [`DaemonError::RemoteSendPending`] with a daemon-owned pending key, so
    /// the reservation remains available for retry or fallback.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn dispatch_remote_ticket<R: Relation + Send>(
        &mut self,
        plan: DispatchPlan<R>,
        contract: RemoteDispatchContract,
        local_capabilities: &CapabilityManifest,
    ) -> Result<DispatchTicket<R>, DaemonError> {
        let ticket = self
            .dispatcher
            .dispatch_ticket(plan, contract)
            .map_err(DaemonError::dispatch)?;
        let request = ticket.wire_request().map_err(DaemonError::dispatch)?;
        let key = self.journal_intent(&ticket, &request)?;
        let requested_key = PendingRemoteKey::new(
            ticket.work_key().map_err(DaemonError::dispatch)?,
            ticket.attempt().map_err(DaemonError::dispatch)?,
        );
        if let Err(error) = self.send_remote_request(request, local_capabilities) {
            // Preserve the exact affine ticket so a transient transport
            // failure can be retried or handed to the owner-held fallback.
            // The compatibility ticket API has no separate start state, so
            // expose the retained owner key through a typed error.
            let (envelope, _request) = match self.dispatcher.begin_remote(ticket) {
                Ok(value) => value,
                Err(dispatch_error) => {
                    let _ = self.journal_cancel(key, JOURNAL_CANCEL_SEND_FAILED);
                    return Err(DaemonError::dispatch(dispatch_error));
                }
            };
            let retained_bytes = envelope.retained_size();
            let pending_key = match self.pending_remote.insert(
                requested_key,
                envelope,
                retained_bytes,
                self.config.replication.count,
                self.config.replication.bytes,
            ) {
                Ok(key) => key,
                Err(insert_error) => {
                    let _ = self.journal_cancel(key, JOURNAL_CANCEL_SEND_FAILED);
                    return Err(insert_error);
                }
            };
            return Err(DaemonError::RemoteSendPending {
                key: pending_key,
                error: error.to_string(),
            });
        }
        Ok(ticket)
    }

    fn send_remote_request(
        &mut self,
        request: WireRecipeRequest,
        local_capabilities: &CapabilityManifest,
    ) -> Result<(), DaemonError> {
        let negotiated = self.negotiate_remote(local_capabilities)?;
        request
            .validate(negotiated.limits)
            .map_err(DaemonError::Replication)?;
        if !request.resources.fits_within(negotiated.max_resources) {
            return Err(DaemonError::Replication(ReplicationError::ResourceLimit));
        }
        if !negotiated
            .recipes
            .iter()
            .any(|(recipe, _version)| *recipe == request.recipe)
        {
            return Err(DaemonError::Replication(ReplicationError::NoCommonRecipe));
        }
        let Some(remote) = self.remote.as_mut() else {
            return Err(DaemonError::RemoteUnavailable);
        };
        remote
            .send(
                TransportMessage::WireRecipeRequest(request),
                negotiated.limits,
            )
            .map_err(DaemonError::Replication)
    }

    /// Retries a request retained after a transport send failure or
    /// connection replacement. The pending envelope remains owned by the
    /// daemon on every error, so callers may choose local fallback instead.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn resend_remote_pending(
        &mut self,
        key: PendingRemoteKey,
        local_capabilities: &CapabilityManifest,
    ) -> Result<(), DaemonError> {
        let request = self
            .pending_remote
            .get(&key)
            .ok_or(DaemonError::RemoteResultUnmatched)?
            .request()
            .clone();
        match self.send_remote_request(request, local_capabilities) {
            Ok(()) => Ok(()),
            Err(error) => Err(DaemonError::RemoteSendPending {
                key,
                error: error.to_string(),
            }),
        }
    }

    /// Sends one negotiated replication control message before a recipe
    /// request.  Product compositions use this for authenticated root/CAS
    /// preludes; the daemon still owns negotiation, frame bounds, and the
    /// underlying transport so a control message cannot bypass its session.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn send_remote_control(
        &mut self,
        message: TransportMessage,
        local_capabilities: &CapabilityManifest,
    ) -> Result<NegotiatedCapabilities, DaemonError> {
        let negotiated = self.negotiate_remote(local_capabilities)?;
        message
            .validate(negotiated.limits)
            .map_err(DaemonError::Replication)?;
        let Some(remote) = self.remote.as_mut() else {
            return Err(DaemonError::RemoteUnavailable);
        };
        remote
            .send(message, negotiated.limits)
            .map_err(DaemonError::Replication)?;
        Ok(negotiated)
    }

    /// Receives one negotiated control response.  Recipe result frames remain
    /// on the pending-ticket path and are therefore never consumed here.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn receive_remote_control(&mut self) -> Result<TransportMessage, DaemonError> {
        if let Some(message) = self.drain_replication() {
            return Ok(message);
        }

        // A result for any pending attempt is routed to the inbox and the
        // transport keeps being polled. This avoids a result at the head of
        // the stream permanently hiding a later control response.
        loop {
            let Some(message) = self.recv_remote_message()? else {
                return Err(DaemonError::RemoteControlBuffered);
            };
            match message {
                TransportMessage::WireRecipeResult(result) => {
                    self.route_remote_message(TransportMessage::WireRecipeResult(result))?;
                }
                control => {
                    return Ok(control);
                }
            }
        }
    }

    fn negotiate_remote(
        &mut self,
        local_capabilities: &CapabilityManifest,
    ) -> Result<NegotiatedCapabilities, DaemonError> {
        let Some(connection) = self.remote.as_ref().map(|remote| remote.connection_id()) else {
            return Err(DaemonError::RemoteUnavailable);
        };
        self.observe_transport_generation(Some(connection));
        self.ensure_peer_capability_connection(Some(connection));
        let local_fingerprint =
            capability_fingerprint(local_capabilities, self.protocol.transport_limits);
        if let Some(session) = &self.remote_session
            && session.connection == connection
            && session.local_fingerprint == local_fingerprint
        {
            return Ok(session.negotiated.clone());
        }
        let negotiated = self
            .remote
            .as_mut()
            .ok_or(DaemonError::RemoteUnavailable)?
            .negotiate(local_capabilities, self.protocol.transport_limits)
            .map_err(DaemonError::Replication)?;
        self.remote_session = Some(RemoteSession {
            connection,
            local_fingerprint,
            negotiated: negotiated.clone(),
        });
        Ok(negotiated)
    }

    /// Sends a request and transfers ticket ownership into the daemon's
    /// relation-erased pending relation. The returned key carries the
    /// relation's owner generation, while its callback retains the typed
    /// ticket; a stale key cannot address a replacement attempt.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn dispatch_remote_pending<R: Relation + Send>(
        &mut self,
        plan: DispatchPlan<R>,
        contract: RemoteDispatchContract,
        local_capabilities: &CapabilityManifest,
    ) -> Result<PendingRemoteKey, DaemonError> {
        let (key, request) = self.retain_remote_pending(plan, contract)?;
        if let Err(error) = self.send_remote_request(request, local_capabilities) {
            // Keep the pending relation row and affine envelope live. The
            // owner can call `resend_remote_pending` after reconnect or
            // `fallback_remote_pending` without losing the reserved demand.
            return Err(DaemonError::RemoteSendPending {
                key,
                error: error.to_string(),
            });
        }
        Ok(key)
    }

    /// Transfers a scheduled plan into daemon-owned pending custody without
    /// sending it to a peer. This is the typed cutover seam for a local
    /// fallback selected before remote execution begins: the same affine
    /// ticket, journal identity, scheduler reservation, and publication path
    /// are retained, while no unpaired recipe request can escape.
    /// # Errors
    ///
    /// Returns an error when ticket, journal, or bounded pending-relation
    /// admission fails.
    pub fn prepare_remote_pending<R: Relation + Send>(
        &mut self,
        plan: DispatchPlan<R>,
        contract: RemoteDispatchContract,
    ) -> Result<PendingRemoteKey, DaemonError> {
        self.retain_remote_pending(plan, contract)
            .map(|(key, _request)| key)
    }

    fn retain_remote_pending<R: Relation + Send>(
        &mut self,
        plan: DispatchPlan<R>,
        contract: RemoteDispatchContract,
    ) -> Result<(PendingRemoteKey, WireRecipeRequest), DaemonError> {
        let ticket = self
            .dispatcher
            .dispatch_ticket(plan, contract)
            .map_err(DaemonError::dispatch)?;
        let journal_request = ticket.wire_request().map_err(DaemonError::dispatch)?;
        let journal_key = self.journal_intent(&ticket, &journal_request)?;
        let requested_key = PendingRemoteKey::new(
            ticket.work_key().map_err(DaemonError::dispatch)?,
            ticket.attempt().map_err(DaemonError::dispatch)?,
        );
        let (envelope, request) = match self.dispatcher.begin_remote(ticket) {
            Ok(value) => value,
            Err(error) => {
                let _ = self.journal_cancel(journal_key, JOURNAL_CANCEL_SEND_FAILED);
                return Err(DaemonError::dispatch(error));
            }
        };
        let retained_bytes = envelope.retained_size();
        let key = match self.pending_remote.insert(
            requested_key,
            envelope,
            retained_bytes,
            self.config.replication.count,
            self.config.replication.bytes,
        ) {
            Ok(key) => key,
            Err(error) => {
                let _ = self.journal_cancel(journal_key, JOURNAL_CANCEL_SEND_FAILED);
                return Err(error);
            }
        };
        Ok((key, request))
    }
}

pub(crate) fn capability_fingerprint(
    capabilities: &CapabilityManifest,
    limits: TransportLimits,
) -> [u8; 32] {
    // Keep this grammar domain-separated and self-delimiting.  The digest is
    // used as a retained peer capability identity, so truncating a list or
    // omitting resource limits would let a changed peer masquerade as the
    // previous negotiated session.
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"backend.engine.capability-manifest.v2\0");
    hasher.update(&capabilities.protocol.min.to_be_bytes());
    hasher.update(&capabilities.protocol.max.to_be_bytes());
    hasher.update(&capabilities.max_object.to_be_bytes());
    hasher.update(&capabilities.max_chunk.to_be_bytes());
    hasher.update(&capabilities.max_frame.to_be_bytes());
    hasher.update(&capabilities.max_ranges.to_be_bytes());
    hasher.update(
        &u64::try_from(capabilities.schemas.len())
            .unwrap_or(u64::MAX)
            .to_be_bytes(),
    );
    for schema in &capabilities.schemas {
        hasher.update(&[schema.domain]);
        hasher.update(&schema.type_id.to_be_bytes());
        hasher.update(&schema.versions.min.to_be_bytes());
        hasher.update(&schema.versions.max.to_be_bytes());
    }
    hasher.update(
        &u64::try_from(capabilities.recipes.len())
            .unwrap_or(u64::MAX)
            .to_be_bytes(),
    );
    for recipe in &capabilities.recipes {
        hasher.update(&recipe.recipe.as_bytes());
        let context = recipe.recipe.context();
        hasher.update(&[context.class(), context.domain(), context.version()]);
        hasher.update(&context.ty().to_be_bytes());
        hasher.update(&recipe.versions.min.to_be_bytes());
        hasher.update(&recipe.versions.max.to_be_bytes());
    }
    // TransportLimits is part of the negotiated envelope.  The manifest
    // fields above are the peer's advertised maxima; include the receiver's
    // actual protocol ceiling so changing decode limits cannot reuse a stale
    // session.
    hasher.update(&limits.max_frame.to_be_bytes());
    hasher.update(&limits.max_chunk.to_be_bytes());
    hasher.update(&limits.max_object.to_be_bytes());
    hasher.update(&limits.max_objects.to_be_bytes());
    hasher.update(&limits.max_ranges.to_be_bytes());
    hasher.update(&limits.max_capabilities.to_be_bytes());
    hasher.update(&limits.max_key_bytes.to_be_bytes());
    hasher.update(&limits.max_inputs.to_be_bytes());
    *hasher.finalize().as_bytes()
}
