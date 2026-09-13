//! Desktop immutable-root reducer and admission.

use crate::{
    CertifiedSubscriptionTransport, ClientError, LocalEngine, MAX_EVENTS, SubscriptionRequest,
    SubscriptionTransport,
};
use backend_library::{
    Coverage, CoverageCapability, Cursor, CursorRead, CursorSub, ProducerObservationVerifier,
    SnapshotHydrator, SnapshotPageClaim, ViewProjection, ViewProjectionError, ViewRoot,
    ViewStateRoot,
};
use backend_replication::{LocalSubscriptionResponse, ReplicationError};

/// Desktop reducer state for one immutable view root and bounded subscription.
#[derive(Debug)]
pub struct Model {
    /// Current coherent view root.
    pub(crate) root: ViewRoot,
    sub: CursorSub,
    expected_basis: ViewStateRoot,
    snapshot: Option<SnapshotHydrator>,
    snapshot_next_token: Option<Box<[u8]>>,
    /// Number of explicit reset/rejection events observed.
    pub reset_count: u32,
}

impl Model {
    /// Creates a desktop model pinned to the supplied source root.
    #[must_use]
    pub fn new(root: ViewRoot, root_id: ViewStateRoot) -> Self {
        let valid = ViewProjection::from_root_against(root.clone(), root_id).is_ok();
        Self {
            sub: CursorSub::for_view(&root, MAX_EVENTS),
            root,
            expected_basis: root_id,
            snapshot: None,
            snapshot_next_token: None,
            reset_count: u32::from(!valid),
        }
    }

    /// Creates a desktop model and rejects an incoherent or misbased root.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError::IncoherentRoot`], [`ClientError::BasisMismatch`],
    /// or [`ClientError::Protocol`] when the supplied root fails admission.
    pub fn try_new(root: ViewRoot, root_id: ViewStateRoot) -> Result<Self, ClientError> {
        let cursor = CursorSub::for_view(&root, MAX_EVENTS).cursor();
        ViewProjection::admit_against(root.clone(), cursor, root_id)
            .map_err(|error| map_projection_error(error, root_id))?;
        Ok(Self::new(root, root_id))
    }

    /// Creates a desktop model at an already admitted stream cursor.
    ///
    /// This constructor is used when a process restores a view root and its
    /// persisted locald cursor separately.  Deriving a cursor from the root
    /// would lose intent-only stream positions, so the caller must provide the
    /// exact cursor returned by locald.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] when the cursor, view root, and expected source
    /// root do not form one complete admitted projection.
    pub fn try_new_at(
        root: ViewRoot,
        cursor: Cursor,
        root_id: ViewStateRoot,
    ) -> Result<Self, ClientError> {
        ViewProjection::admit_against(root.clone(), cursor, root_id)
            .map_err(|error| map_projection_error(error, root_id))?;
        Ok(Self {
            sub: CursorSub::from_cursor(cursor, MAX_EVENTS),
            root,
            expected_basis: root_id,
            snapshot: None,
            snapshot_next_token: None,
            reset_count: 0,
        })
    }

    /// Returns the cursor currently held by the reducer.
    #[must_use]
    pub const fn cursor(&self) -> Cursor {
        self.sub.cursor()
    }

    /// Returns the currently admitted immutable view root.
    #[must_use]
    pub const fn root(&self) -> &ViewRoot {
        &self.root
    }

    /// Returns the exact source basis required by future roots.
    #[must_use]
    pub const fn expected_basis(&self) -> ViewStateRoot {
        self.expected_basis
    }

    /// Returns the credit that will be advertised on the next poll.
    #[must_use]
    pub fn credit(&self) -> usize {
        self.sub.max().min(MAX_EVENTS)
    }

    /// Admits one bounded reset page and retains its hydrator until the
    /// descriptor's complete row set has arrived.
    ///
    /// Returns `Ok(false)` while another page is required and `Ok(true)` when
    /// the checked replacement root was installed.  Every page is admitted
    /// through the shared library hydrator, so the desktop does not parse
    /// reset descriptors or reconstruct row identities itself.
    /// # Errors
    ///
    /// Returns an error when the transport payload or checked state is invalid.
    pub fn reduce_snapshot_page(&mut self, page: SnapshotPageClaim) -> Result<bool, ClientError> {
        let next_token = page.next_token().map_err(ClientError::Protocol)?;
        let read = if let Some(mut hydrator) = self.snapshot.take() {
            hydrator.push_page(page).map_err(ClientError::Protocol)?;
            if !hydrator.is_complete() {
                self.snapshot = Some(hydrator);
                self.snapshot_next_token = next_token;
                return Ok(false);
            }
            self.snapshot_next_token = None;
            hydrator.finish().map_err(ClientError::Protocol)?
        } else {
            let hydrator =
                SnapshotHydrator::start(self.cursor(), page).map_err(ClientError::Protocol)?;
            if !hydrator.is_complete() {
                self.snapshot = Some(hydrator);
                self.snapshot_next_token = next_token;
                return Ok(false);
            }
            self.snapshot_next_token = None;
            hydrator.finish().map_err(ClientError::Protocol)?
        };
        self.reduce_checked(read).map(|()| true)
    }

    /// Returns the row anchor required for the next reset page, if hydration
    /// is waiting on one.
    #[must_use]
    pub fn snapshot_next_after(&self) -> Option<backend_library::RowId> {
        self.snapshot
            .as_ref()
            .and_then(SnapshotHydrator::next_after_anchor)
    }

    /// Returns whether a reset descriptor is still being hydrated.
    #[must_use]
    pub const fn snapshot_pending(&self) -> bool {
        self.snapshot.is_some()
    }

    /// Discards a partial snapshot after its lease or authenticated page chain
    /// fails. The last coherent visible root remains installed.
    pub fn abort_snapshot(&mut self) {
        self.snapshot = None;
        self.snapshot_next_token = None;
    }

    /// Returns the producer-authenticated continuation for the next reset
    /// page. Callers copy it back verbatim; they never derive page identity
    /// from a row or offset.
    #[must_use]
    pub fn snapshot_next_token(&self) -> Option<&[u8]> {
        self.snapshot_next_token.as_deref()
    }

    /// Admits one producer response from a durable locald lease.
    ///
    /// The raw cursor and continuation fields in the local control envelope
    /// are checked against the shared typed payload before any reducer state
    /// changes.  This keeps lease correlation, page replay protection, and
    /// event admission in one frontend adapter instead of letting each UI
    /// caller reconstruct a parallel state machine.
    /// # Errors
    ///
    /// Returns an error when the transport payload or checked state is invalid.
    pub fn reduce_lease_response(
        &mut self,
        response: LocalSubscriptionResponse,
        capability: Option<CoverageCapability>,
    ) -> Result<(), ClientError> {
        match response {
            LocalSubscriptionResponse::Opened { cursor, .. }
            | LocalSubscriptionResponse::Resumed { cursor, .. } => {
                let observed = decode_control_cursor(&cursor, &self.root)?;
                if observed.sequence() < self.cursor().sequence() {
                    return Err(ClientError::CursorMismatch);
                }
                // The owner may have advanced through intent-only records
                // while this process was disconnected.  Those records do
                // not change the retained view root, so reconcile the exact
                // producer cursor instead of deriving a sequence locally.
                ViewProjection::admit_against(self.root.clone(), observed, self.expected_basis)
                    .map_err(|error| map_projection_error(error, self.expected_basis))?;
                self.sub = CursorSub::from_cursor(observed, MAX_EVENTS);
                Ok(())
            }
            LocalSubscriptionResponse::Batch {
                previous,
                cursor,
                payload,
                ..
            } => {
                let expected = self.cursor();
                let previous = decode_control_cursor(&previous, &self.root)?;
                if previous != expected {
                    return Err(ClientError::CursorMismatch);
                }
                let read = crate::subscription::subscription_read_from_bytes(
                    &payload, expected, capability,
                )?;
                let observed = read_cursor(&read);
                Cursor::decode_control_against(&cursor, observed).map_err(ClientError::Protocol)?;
                self.reduce_checked(read)
            }
            LocalSubscriptionResponse::ResetWithRoot {
                cursor, payload, ..
            } => {
                let expected = self.cursor();
                let read = crate::subscription::subscription_read_from_bytes(
                    &payload, expected, capability,
                )?;
                let observed = read_cursor(&read);
                if observed != decode_reset_cursor(&cursor, &read)? {
                    return Err(ClientError::CursorMismatch);
                }
                self.reduce_checked(read)
            }
            LocalSubscriptionResponse::SnapshotPage {
                page,
                next,
                payload,
                ..
            } => {
                let expected_page = self.snapshot_next_token.as_deref().unwrap_or_default();
                if page.as_ref() != expected_page {
                    return Err(ClientError::Protocol(
                        "snapshot page continuation does not match the reducer".to_owned(),
                    ));
                }
                let claim = crate::subscription::snapshot_page_from_bytes_with_capability(
                    &payload,
                    self.cursor(),
                    None,
                    capability,
                )?;
                let expected_next = claim.next_token().map_err(ClientError::Protocol)?;
                if expected_next.as_deref() != next.as_deref() {
                    return Err(ClientError::Protocol(
                        "snapshot page response continuation is not authenticated".to_owned(),
                    ));
                }
                self.reduce_snapshot_page(claim)?;
                Ok(())
            }
            LocalSubscriptionResponse::Acked { cursor, .. }
            | LocalSubscriptionResponse::Renewed { cursor, .. } => {
                let observed = decode_control_cursor(&cursor, &self.root)?;
                if observed != self.cursor() {
                    return Err(ClientError::CursorMismatch);
                }
                Ok(())
            }
            LocalSubscriptionResponse::Cancelled { .. } => Ok(()),
        }
    }

    /// Admits one durable producer response through an authenticated process
    /// boundary. Snapshot pages use the verifier to mint their opaque coverage
    /// capability; other response shapes retain the regular typed reducer.
    ///
    /// # Errors
    ///
    /// Returns an error when the response identity, proof, cursor, or snapshot
    /// page does not match the currently admitted subscription state.
    pub fn reduce_lease_response_with_verifier<V: ProducerObservationVerifier>(
        &mut self,
        response: LocalSubscriptionResponse,
        verifier: &V,
    ) -> Result<(), ClientError> {
        if let LocalSubscriptionResponse::SnapshotPage {
            page,
            next,
            payload,
            ..
        } = response
        {
            let expected_page = self.snapshot_next_token.as_deref().unwrap_or_default();
            if page.as_ref() != expected_page {
                return Err(ClientError::Protocol(
                    "snapshot page continuation does not match the reducer".to_owned(),
                ));
            }
            let claim = crate::subscription::snapshot_page_from_bytes_with_verifier(
                &payload,
                self.cursor(),
                None,
                verifier,
            )?;
            let expected_next = claim.next_token().map_err(ClientError::Protocol)?;
            if expected_next.as_deref() != next.as_deref() {
                return Err(ClientError::Protocol(
                    "snapshot page response continuation is not authenticated".to_owned(),
                ));
            }
            self.reduce_snapshot_page(claim)?;
            return Ok(());
        }
        self.reduce_lease_response(response, self.root.capability())
    }

    /// Reduces one read after checking exact cursor, root, basis, and bounds.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] when the event batch exceeds credit, fails to
    /// chain from the current cursor, or contains a mismatched root/basis.
    pub fn reduce_checked(&mut self, read: CursorRead) -> Result<(), ClientError> {
        match read {
            CursorRead::Events { cursor, events } => {
                if events.len() > self.credit() {
                    return Err(ClientError::Transport(ReplicationError::MessageTooLarge));
                }
                let previous = self.sub.cursor();
                let projection =
                    ViewProjection::admit_against(self.root.clone(), previous, self.expected_basis)
                        .map_err(|error| map_projection_error(error, self.expected_basis))?;
                let next = projection
                    .apply_events(cursor, &events)
                    .map_err(|error| map_projection_error(error, self.expected_basis))?;
                self.root = next.root().clone();
                self.expected_basis = next.basis();
                self.sub = CursorSub::from_cursor(cursor, MAX_EVENTS);
                self.snapshot = None;
                self.snapshot_next_token = None;
                Ok(())
            }
            CursorRead::Reset {
                cursor,
                root,
                reason: _,
            } => {
                if cursor.sequence() <= self.sub.cursor().sequence() {
                    return Err(ClientError::CursorMismatch);
                }
                let projection = ViewProjection::admit(*root, cursor)
                    .map_err(|error| map_projection_error(error, self.expected_basis))?;
                if !projection
                    .root()
                    .coverage()
                    .iter()
                    .copied()
                    .any(Coverage::is_complete)
                {
                    return Err(ClientError::Protocol(
                        "subscription root requires producer-admitted complete coverage".to_owned(),
                    ));
                }
                self.root = projection.root().clone();
                self.expected_basis = projection.basis();
                self.sub = CursorSub::from_cursor(cursor, MAX_EVENTS);
                self.snapshot = None;
                self.snapshot_next_token = None;
                self.reset_count = self.reset_count.saturating_add(1);
                Ok(())
            }
        }
    }

    /// Reduces one read and records malformed/gapped reads as reset events.
    pub fn reduce(&mut self, read: CursorRead) {
        if self.reduce_checked(read).is_err() {
            self.reset_count = self.reset_count.saturating_add(1);
        }
    }

    /// Polls one bounded batch from an embedded subscription owner.
    pub fn poll_engine(&mut self, engine: &mut impl LocalEngine) {
        let read = engine.poll(&mut self.sub);
        self.reduce(read);
    }

    /// Polls the configured transport with explicit bounded credit.
    ///
    /// # Errors
    ///
    /// Returns the transport or event-admission error produced while polling.
    pub fn poll_transport(
        &mut self,
        transport: &mut impl SubscriptionTransport,
    ) -> Result<(), ClientError> {
        let request = SubscriptionRequest::new(self.cursor(), self.credit())?;
        let read = transport.subscribe(request)?;
        self.reduce_checked(read)
    }

    /// Polls one bounded batch through a producer-certificate-aware transport.
    ///
    /// The optional capability is supplied by the trusted composition root;
    /// the desktop never derives complete source coverage from a view root or
    /// from a digest-only cursor claim.
    ///
    /// # Errors
    ///
    /// Returns the transport, certificate, coverage, or event-admission error
    /// produced while polling.
    pub fn poll_transport_with_certificate(
        &mut self,
        transport: &mut impl CertifiedSubscriptionTransport,
        capability: Option<CoverageCapability>,
    ) -> Result<(), ClientError> {
        let request = SubscriptionRequest::new(self.cursor(), self.credit())?;
        let read = transport.subscribe_with_certificate_against(request, &self.root, capability)?;
        self.reduce_checked(read)
    }
}

fn decode_control_cursor(bytes: &[u8], root: &ViewRoot) -> Result<Cursor, ClientError> {
    Cursor::decode_control_for_root(bytes, root).map_err(ClientError::Protocol)
}

fn read_cursor(read: &CursorRead) -> Cursor {
    match read {
        CursorRead::Events { cursor, .. } | CursorRead::Reset { cursor, .. } => *cursor,
    }
}

fn decode_reset_cursor(bytes: &[u8], read: &CursorRead) -> Result<Cursor, ClientError> {
    let CursorRead::Reset { root, .. } = read else {
        return Err(ClientError::Protocol(
            "reset response omitted its replacement root".to_owned(),
        ));
    };
    Cursor::decode_control_for_root(bytes, root).map_err(ClientError::Protocol)
}

/// Applies one bounded batch from an injected transport result.
pub fn poll(model: &mut Model, read: CursorRead) {
    model.reduce(read);
}

fn map_projection_error(error: ViewProjectionError, expected: ViewStateRoot) -> ClientError {
    match error {
        ViewProjectionError::Incoherent => ClientError::IncoherentRoot,
        ViewProjectionError::UnsupportedSchema => {
            ClientError::Protocol("unsupported view schema".to_owned())
        }
        ViewProjectionError::CursorMismatch => ClientError::CursorMismatch,
        ViewProjectionError::FreshnessMismatch => {
            ClientError::Protocol("subscription snapshot freshness was not admitted".to_owned())
        }
        ViewProjectionError::BasisMismatch { observed, .. } => {
            ClientError::BasisMismatch { expected, observed }
        }
        ViewProjectionError::MissingCoverage => ClientError::Protocol(
            "subscription root requires producer-admitted complete coverage".to_owned(),
        ),
    }
}
