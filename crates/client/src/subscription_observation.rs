//! Proof-admitted state of one durable local publication lease. Socket work
//! belongs to the caller's worker; cancellation is checked between bounded
//! frames. No partial reset can replace the retained complete root.
//!
//! # Protocol limits
//!
//! Every number this module relies on (the lease term it asks for, its page
//! credit, the row and page caps and the reset allowance) comes from
//! [`crate::lease_contract`], the same module the owner imports its defaults
//! from. The owner either grants the term asked for or refuses the request, so
//! the term stored in a [`PublicationLease`] is the term the owner will honor.

use crate::lease_contract::{LeaseMs, PUBLICATION_CREDIT, PUBLICATION_LEASE};
use crate::reset_budget::{ResetBudget, ResetFault};
use crate::subscription::{
    snapshot_page_from_bytes_with_verifier, subscription_read_from_bytes_against,
};
use crate::subscription_local::ConnectionId;
use crate::{ClientError, LocalSubscriptionTransport};
use backend_library::{Cursor, CursorEvent, CursorRead, SnapshotHydrator, ViewRoot};
use backend_replication::{AuthenticatedLocalPeer, LocalSubscriptionId, LocalSubscriptionResponse};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Read and write limit for the terminal cancel on an exact socket. A cancel
/// that cannot finish this fast is abandoned to the owner's lease expiry.
const TERMINAL_CANCEL_IO: Duration = Duration::from_millis(50);

/// Exact producer state retained across socket reconnects. The lease is an
/// owner-issued identity, never reconstructed from a path or a clock.
pub struct PublicationLease {
    lease: LocalSubscriptionId,
    cursor: Cursor,
    root: Arc<ViewRoot>,
    /// The term the owner granted, which governs how often to renew.
    term: LeaseMs,
    /// The connection this lease was last used on: the only socket a terminal
    /// cancel may use.
    held_on: ConnectionId,
}

impl PublicationLease {
    /// Complete root whose producer proofs have been admitted.
    #[must_use]
    pub fn root(&self) -> Arc<ViewRoot> {
        Arc::clone(&self.root)
    }

    /// Exact admitted producer cursor.
    #[must_use]
    pub const fn cursor(&self) -> Cursor {
        self.cursor
    }

    /// How long a quiet holder may wait between renewals: half the term the
    /// owner granted, so one lost renewal still leaves a second chance.
    #[must_use]
    pub const fn renew_after(&self) -> Duration {
        self.term.renewal_interval()
    }
}

impl LocalSubscriptionTransport {
    /// Acquires a lease from a previously admitted exact root/cursor.
    ///
    /// If the reset that follows cannot be admitted, the lease is released on
    /// the socket it was opened on (see [`Self::cancel_publications_current`])
    /// before the error is returned; the owner reclaims it on expiry if that
    /// cancel cannot be delivered.
    /// # Errors
    /// Returns a transport, proof, cancellation, or bounded-reset error.
    pub fn acquire_publications(
        &mut self,
        root: Arc<ViewRoot>,
        cursor: Cursor,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<PublicationLease, ClientError> {
        check(cancelled)?;
        if root.root() != cursor.root() || !root.is_coherent() || root.capability().is_none() {
            return Err(protocol("publication base root/cursor mismatch"));
        }
        let response =
            self.open_lease(Some(cursor), PUBLICATION_CREDIT, PUBLICATION_LEASE.get())?;
        let lease = response.lease();
        let mut state = PublicationLease {
            lease,
            cursor,
            root,
            term: PUBLICATION_LEASE,
            held_on: self.connection(),
        };
        if let Err(error) = self.admit_publications(&mut state, response, cancelled) {
            let _ = self.cancel_publications_current(&state);
            return Err(error);
        }
        Ok(state)
    }

    /// Requests a bounded suffix, also extending the retained lease. The
    /// same operation resumes after reconnect; a lost reply is never guessed.
    /// # Errors
    /// A rejected/expired lease leaves the last admitted state unchanged.
    pub fn resume_publications(
        &mut self,
        state: &mut PublicationLease,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<(), ClientError> {
        check(cancelled)?;
        let response = self.resume_lease(
            state.lease,
            state.cursor,
            PUBLICATION_CREDIT,
            PUBLICATION_LEASE.get(),
        )?;
        self.admit_publications(state, response, cancelled)
    }

    /// Fences a quiet lease to its exact admitted cursor before an idle pause.
    /// # Errors
    /// Returns an error if the owner no longer retains this exact lease.
    pub fn renew_publications(&mut self, state: &PublicationLease) -> Result<(), ClientError> {
        match self.renew_lease(
            state.lease,
            state.cursor,
            PUBLICATION_CREDIT,
            PUBLICATION_LEASE.get(),
        )? {
            LocalSubscriptionResponse::Renewed {
                cursor, lease_ms, ..
            } if cursor.as_ref() == state.cursor.encode_control().as_ref() => {
                granted_term(lease_ms).map(drop)
            }
            _ => Err(protocol(
                "publication renewal did not fence the admitted cursor",
            )),
        }
    }

    /// Releases owner-side retention. Socket I/O remains subject to this
    /// transport's timeout; a caller closing a cancelled socket can instead
    /// drop it and let the finite producer lease expire.
    /// # Errors
    /// Returns a transport error, or a protocol error when the owner did not
    /// acknowledge the cancellation.
    pub fn cancel_publications(&mut self, state: &PublicationLease) -> Result<(), ClientError> {
        match self.cancel_lease(state.lease)? {
            LocalSubscriptionResponse::Cancelled { .. } => Ok(()),
            _ => Err(protocol("publication cancellation was not acknowledged")),
        }
    }

    /// Terminal best-effort release on the socket the lease was last used on,
    /// without opening a replacement socket.
    ///
    /// Read and write each have a 50 ms socket timeout; this does not promise
    /// preemption of native syscalls, scheduling, or decoding. The transport
    /// cannot be used afterwards, whatever the outcome: its socket now carries
    /// those short deadlines and may hold a half-read frame. A lease held on a
    /// different socket is refused without sending anything.
    /// # Errors
    /// Returns an I/O or protocol error when this exact socket cannot release
    /// the lease; the caller must not assume successful producer cleanup.
    pub fn cancel_publications_current(
        &mut self,
        state: &PublicationLease,
    ) -> Result<(), ClientError> {
        self.cancel_publications_current_within(state, TERMINAL_CANCEL_IO)
    }

    /// [`Self::cancel_publications_current`] with the socket deadline named,
    /// so a test can give the owner thread scheduling slack that the
    /// production 50 ms deliberately does not allow.
    pub(crate) fn cancel_publications_current_within(
        &mut self,
        state: &PublicationLease,
        io: Duration,
    ) -> Result<(), ClientError> {
        if state.held_on != self.connection() {
            return Err(protocol("publication lease is not held on this socket"));
        }
        self.cancel_lease_current(state.lease, io)
    }

    fn admit_publications(
        &mut self,
        state: &mut PublicationLease,
        mut response: LocalSubscriptionResponse,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<(), ClientError> {
        let previous = state.cursor;
        let mut reset = ResetHydration::begin(previous, self.now())?;
        let (root, cursor) = loop {
            check(cancelled)?;
            reset.check(self.now())?;
            if response.lease() != state.lease {
                return Err(protocol("publication response changed lease identity"));
            }
            state.held_on = self.connection();
            match response {
                LocalSubscriptionResponse::Opened {
                    cursor, lease_ms, ..
                }
                | LocalSubscriptionResponse::Resumed {
                    cursor, lease_ms, ..
                } => {
                    if reset.in_progress() || cursor.as_ref() != previous.encode_control().as_ref()
                    {
                        return Err(protocol("publication acknowledgement changed its cursor"));
                    }
                    state.term = granted_term(lease_ms)?;
                    break (Arc::clone(&state.root), previous);
                }
                LocalSubscriptionResponse::Batch {
                    previous: predecessor,
                    cursor: target,
                    payload,
                    ..
                } => {
                    if reset.in_progress()
                        || predecessor.as_ref() != previous.encode_control().as_ref()
                    {
                        return Err(protocol("publication batch predecessor mismatch"));
                    }
                    break admit_batch(state, previous, &target, &payload)?;
                }
                LocalSubscriptionResponse::SnapshotPage {
                    page,
                    next,
                    payload,
                    ..
                } => {
                    let peer = self.authenticated_peer().ok_or_else(|| {
                        protocol("publication reset requires authenticated producer")
                    })?;
                    match reset.admit(peer, &page, next, &payload, self.now())? {
                        ResetStep::Complete { root, cursor } => break (root, cursor),
                        ResetStep::Continue(continuation) => {
                            check(cancelled)?;
                            response =
                                self.snapshot_page(state.lease, continuation, PUBLICATION_CREDIT)?;
                        }
                    }
                }
                _ => return Err(protocol("unexpected publication lifecycle response")),
            }
        };
        check(cancelled)?;
        reset.check(self.now())?;
        // Quiet Resume has already fenced this exact durable cursor; it
        // does not need another frame or a duplicate root publication.
        if cursor == previous {
            return Ok(());
        }
        match self.ack_lease(state.lease, cursor)? {
            LocalSubscriptionResponse::Acked {
                cursor: admitted, ..
            } if admitted.as_ref() == cursor.encode_control().as_ref() => {}
            _ => return Err(protocol("publication acknowledgement cursor mismatch")),
        }
        check(cancelled)?;
        reset.check(self.now())?;
        state.held_on = self.connection();
        state.cursor = cursor;
        state.root = root;
        Ok(())
    }
}

/// Admits one certified event batch against the lease's retained root.
fn admit_batch(
    state: &PublicationLease,
    previous: Cursor,
    target: &[u8],
    payload: &[u8],
) -> Result<(Arc<ViewRoot>, Cursor), ClientError> {
    let read = subscription_read_from_bytes_against(
        payload,
        previous,
        &state.root,
        state.root.capability(),
    )?;
    let CursorRead::Events { cursor, events } = read else {
        return Err(protocol("publication batch carried a reset"));
    };
    if events.len() > PUBLICATION_CREDIT || target != cursor.encode_control().as_ref() {
        return Err(protocol("publication batch credit/cursor mismatch"));
    }
    let mut root = Arc::clone(&state.root);
    for event in events {
        if let CursorEvent::View { delta } = event {
            root = Arc::new(delta.target_view().clone());
        }
    }
    if root.root() != cursor.root() {
        return Err(protocol("publication batch target mismatch"));
    }
    Ok((root, cursor))
}

/// What admitting one reset page leads to.
enum ResetStep {
    /// More pages follow: ask the owner for this exact continuation.
    Continue(Box<[u8]>),
    /// The last page arrived and the root recomputed from every page matches
    /// its certified commitment.
    Complete { root: Arc<ViewRoot>, cursor: Cursor },
}

/// The pages of one reset collected so far, under that reset's budget.
///
/// Nothing here can replace the lease's admitted root: a reset only yields
/// one when its final page has been certified.
struct ResetHydration {
    previous: Cursor,
    budget: ResetBudget,
    hydrator: Option<SnapshotHydrator>,
    /// The only continuation the next page may answer; empty before the first.
    expected: Box<[u8]>,
}

impl ResetHydration {
    fn begin(previous: Cursor, now: Instant) -> Result<Self, ResetFault> {
        Ok(Self {
            previous,
            budget: ResetBudget::begin(now)?,
            hydrator: None,
            expected: Box::new([]),
        })
    }

    /// Whether a reset has delivered a page and is awaiting more.
    const fn in_progress(&self) -> bool {
        self.hydrator.is_some()
    }

    fn check(&self, now: Instant) -> Result<(), ResetFault> {
        self.budget.check(now)
    }

    /// Admits one page. Strict by construction: it must answer the exact
    /// continuation asked for, authenticate its own continuation, fit the
    /// budget the first descriptor fixed, and extend the hydrator in order.
    fn admit(
        &mut self,
        peer: &AuthenticatedLocalPeer,
        page: &[u8],
        next: Option<Box<[u8]>>,
        payload: &[u8],
        now: Instant,
    ) -> Result<ResetStep, ClientError> {
        if page != self.expected.as_ref() {
            return Err(protocol("publication reset page mismatch"));
        }
        let claim = snapshot_page_from_bytes_with_verifier(payload, self.previous, None, peer)?;
        self.budget
            .admit_page(claim.descriptor().row_count(), now)?;
        let admitted_next = claim.next_token().map_err(ClientError::Protocol)?;
        if next.as_deref() == Some(page) {
            return Err(ResetFault::RepeatedContinuation.into());
        }
        if admitted_next.as_deref() != next.as_deref() {
            return Err(protocol(
                "publication reset continuation is not authenticated",
            ));
        }
        let hydrator = match self.hydrator.take() {
            Some(mut current) => {
                current.push_page(claim).map_err(ClientError::Protocol)?;
                current
            }
            None => SnapshotHydrator::start(self.previous, claim).map_err(ClientError::Protocol)?,
        };
        if hydrator.is_complete() {
            let CursorRead::Reset { cursor, root, .. } =
                hydrator.finish().map_err(ClientError::Protocol)?
            else {
                return Err(protocol("publication hydration did not reset"));
            };
            return Ok(ResetStep::Complete {
                root: Arc::from(root),
                cursor,
            });
        }
        let continuation =
            next.ok_or_else(|| protocol("publication reset omitted continuation"))?;
        self.expected = continuation.clone();
        self.hydrator = Some(hydrator);
        Ok(ResetStep::Continue(continuation))
    }
}

/// The term the owner reports for a lease. It never exceeds the term asked
/// for: the owner refuses a longer one rather than shortening it, so anything
/// longer is a protocol violation, and zero is not a term.
fn granted_term(lease_ms: u64) -> Result<LeaseMs, ClientError> {
    LeaseMs::new(lease_ms)
        .filter(|granted| *granted <= PUBLICATION_LEASE)
        .ok_or_else(|| protocol("publication lease term exceeds the term requested"))
}

fn protocol(message: &str) -> ClientError {
    ClientError::Protocol(message.to_owned())
}

fn check(cancelled: &dyn Fn() -> bool) -> Result<(), ClientError> {
    if cancelled() {
        Err(protocol("publication observation withdrawn"))
    } else {
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic, clippy::too_many_lines)]
mod tests {
    use super::*;
    use crate::test_socket::local_pair;
    use backend_library::{
        AuthorityScopeClaim, Basis, CoverageCapability, Frontier, ProducerObservationClaims,
        ProducerObservationVerifier, ScopeRoot, UntrustedProducerObservation, admit_complete_scope,
        admit_producer_observation, object_version, view_key, view_state_root,
    };
    use backend_replication::{
        LocalControlRequest, LocalControlResponse, LocalStream, LocalSubscriptionOperation,
        decode_request, encode_response, read_frame, write_frame,
    };
    use std::thread::JoinHandle;
    use std::time::Instant;

    const CREDIT: usize = PUBLICATION_CREDIT;
    const LEASE_MS: u64 = 10_000;

    struct FixtureVerifier;
    impl ProducerObservationVerifier for FixtureVerifier {
        type Error = &'static str;
        fn verify(
            &self,
            observation: &UntrustedProducerObservation,
        ) -> Result<ProducerObservationClaims, Self::Error> {
            if observation.evidence() != b"publication-fixture" {
                return Err("invalid fixture");
            }
            Ok(ProducerObservationClaims::new(
                observation.producer_identity(),
                observation.scope_root(),
                observation.context(),
                *blake3::hash(observation.evidence()).as_bytes(),
            ))
        }
    }
    fn root() -> Arc<ViewRoot> {
        let basis = Basis::new(view_state_root(&[]), object_version(b"publication-source"));
        let scope = ScopeRoot::from_bytes(basis.object.to_bytes());
        let observation = admit_producer_observation(
            UntrustedProducerObservation::new(
                [7; 32],
                scope,
                [9; 32],
                b"publication-fixture".to_vec(),
            ),
            &FixtureVerifier,
        )
        .expect("observation");
        let capability = CoverageCapability::from_authorized_with_evidence(
            admit_complete_scope(
                AuthorityScopeClaim::from_object_version(basis.object),
                observation,
            )
            .expect("source scope"),
            b"publication-fixture".to_vec(),
        )
        .expect("coverage");
        Arc::new(
            ViewRoot::empty_checked(
                view_key(b"publication-view"),
                basis,
                Frontier::new(basis.branch, basis.log, basis.schema, basis.root, 0),
                capability,
            )
            .expect("root"),
        )
    }
    fn pair(
        serve: impl FnOnce(&mut LocalStream) + Send + 'static,
    ) -> (LocalSubscriptionTransport, JoinHandle<()>) {
        let (client, mut server) = local_pair();
        server
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("server bound");
        server
            .set_write_timeout(Some(Duration::from_secs(2)))
            .expect("server bound");
        let join = std::thread::spawn(move || serve(&mut server));
        (LocalSubscriptionTransport::from_stream(client), join)
    }
    fn request(stream: &mut LocalStream) -> (u64, LocalSubscriptionOperation) {
        let body = read_frame(stream, crate::limits()).expect("request frame");
        let LocalControlRequest::Subscription(request) =
            decode_request(&body, crate::limits()).expect("request")
        else {
            panic!("wrong lane")
        };
        (request.request_id, request.operation)
    }
    fn reply(stream: &mut LocalStream, response: LocalControlResponse) {
        let body = encode_response(&response, crate::limits()).expect("response");
        write_frame(stream, &body, crate::limits()).expect("response frame");
    }
    /// The state a successful acquisition on `transport` would have produced.
    fn lease_on(
        transport: &LocalSubscriptionTransport,
        lease: LocalSubscriptionId,
        root: &Arc<ViewRoot>,
        cursor: Cursor,
    ) -> PublicationLease {
        PublicationLease {
            lease,
            cursor,
            root: Arc::clone(root),
            term: PUBLICATION_LEASE,
            held_on: transport.connection(),
        }
    }
    fn cancelled_reply(request_id: u64, lease: LocalSubscriptionId) -> LocalControlResponse {
        LocalControlResponse::Subscription(LocalSubscriptionResponse::Cancelled {
            request_id,
            lease,
        })
    }
    fn opened_reply(
        request_id: u64,
        lease: LocalSubscriptionId,
        cursor: Cursor,
        lease_ms: u64,
    ) -> LocalControlResponse {
        LocalControlResponse::Subscription(LocalSubscriptionResponse::Opened {
            request_id,
            lease,
            cursor: cursor.encode_control(),
            credit: CREDIT,
            lease_ms,
        })
    }
    /// A first reset page this unauthenticated fixture cannot admit.
    fn unadmittable_page(request_id: u64, lease: LocalSubscriptionId) -> LocalControlResponse {
        LocalControlResponse::Subscription(LocalSubscriptionResponse::SnapshotPage {
            request_id,
            lease,
            page: Box::new([]),
            next: Some(Box::new([1])),
            credit: CREDIT,
            payload: Box::new([0]),
        })
    }

    #[test]
    fn quiet_acquire_reconnect_resume_and_renew_keep_the_exact_shared_root() {
        let root = root();
        let cursor = Cursor::for_view_root_at(&root, 0);
        let lease = LocalSubscriptionId::from_bytes([3; 16]);
        let (mut transport, owner) = pair(move |stream| {
            let (request_id, operation) = request(stream);
            assert!(
                matches!(operation, LocalSubscriptionOperation::Open { cursor: sent, credit: CREDIT, lease_ms: LEASE_MS } if sent.as_ref() == cursor.encode_control().as_ref())
            );
            reply(stream, opened_reply(request_id, lease, cursor, LEASE_MS));
        });
        let mut state = transport
            .acquire_publications(Arc::clone(&root), cursor, &|| false)
            .expect("acquire");
        assert_eq!(state.renew_after(), Duration::from_millis(5_000));
        owner.join().expect("owner");
        drop(transport);
        let (mut transport, owner) = pair(move |stream| {
            let (request_id, operation) = request(stream);
            assert!(
                matches!(operation, LocalSubscriptionOperation::Resume { lease: sent, cursor: sent_cursor, .. } if sent == lease && sent_cursor.as_ref() == cursor.encode_control().as_ref())
            );
            reply(
                stream,
                LocalControlResponse::Subscription(LocalSubscriptionResponse::Resumed {
                    request_id,
                    lease,
                    cursor: cursor.encode_control(),
                    credit: CREDIT,
                    lease_ms: LEASE_MS,
                }),
            );
            let (request_id, operation) = request(stream);
            assert!(
                matches!(operation, LocalSubscriptionOperation::Renew { lease: sent, .. } if sent == lease)
            );
            reply(
                stream,
                LocalControlResponse::Subscription(LocalSubscriptionResponse::Renewed {
                    request_id,
                    lease,
                    cursor: cursor.encode_control(),
                    credit: CREDIT,
                    lease_ms: LEASE_MS,
                }),
            );
            let (request_id, operation) = request(stream);
            assert!(
                matches!(operation, LocalSubscriptionOperation::Cancel { lease: sent } if sent == lease)
            );
            reply(stream, cancelled_reply(request_id, lease));
        });
        transport
            .resume_publications(&mut state, &|| false)
            .expect("resume");
        assert!(Arc::ptr_eq(&state.root(), &root));
        assert_eq!(state.cursor(), cursor);
        transport.renew_publications(&state).expect("renew");
        transport.cancel_publications(&state).expect("cancel");
        owner.join().expect("owner");
    }

    #[test]
    fn expired_or_dropped_lease_never_advances_the_admitted_state() {
        let root = root();
        let cursor = Cursor::for_view_root_at(&root, 0);
        for message in ["subscription lease expired", "unknown subscription lease"] {
            let lease = LocalSubscriptionId::from_bytes([3; 16]);
            let (mut transport, owner) = pair(move |stream| {
                let (request_id, _) = request(stream);
                reply(
                    stream,
                    LocalControlResponse::Rejected {
                        request_id,
                        message: message.into(),
                    },
                );
            });
            let mut state = lease_on(&transport, lease, &root, cursor);
            assert!(
                transport
                    .resume_publications(&mut state, &|| false)
                    .is_err()
            );
            assert_eq!(state.cursor(), cursor);
            assert!(Arc::ptr_eq(&state.root(), &root));
            owner.join().expect("owner");
            // Reacquisition uses the admitted cursor, never the uncertain
            // server cursor from a failed/lost reply.
            let fresh = LocalSubscriptionId::from_bytes([4; 16]);
            let (mut transport, owner) = pair(move |stream| {
                let (request_id, operation) = request(stream);
                assert!(
                    matches!(operation, LocalSubscriptionOperation::Open { cursor: sent, .. } if sent.as_ref() == cursor.encode_control().as_ref())
                );
                reply(stream, opened_reply(request_id, fresh, cursor, LEASE_MS));
            });
            let fresh = transport
                .acquire_publications(state.root(), state.cursor(), &|| false)
                .expect("reacquire");
            assert_eq!(fresh.cursor(), cursor);
            owner.join().expect("owner");
        }
    }

    #[test]
    fn incomplete_or_misbound_publications_cannot_replace_a_complete_root() {
        let root = root();
        let cursor = Cursor::for_view_root_at(&root, 0);
        let lease = LocalSubscriptionId::from_bytes([3; 16]);
        let wrong = LocalSubscriptionId::from_bytes([4; 16]);
        let responses = [
            LocalSubscriptionResponse::Resumed {
                request_id: 1,
                lease: wrong,
                cursor: cursor.encode_control(),
                credit: CREDIT,
                lease_ms: LEASE_MS,
            },
            LocalSubscriptionResponse::Batch {
                request_id: 1,
                lease,
                previous: Cursor::new().encode_control(),
                cursor: cursor.encode_control(),
                credit: CREDIT,
                payload: Box::new([]),
            },
            // A frame received on an unauthenticated fixture transport
            // cannot admit even a first reset page, much less partial Ready.
            LocalSubscriptionResponse::SnapshotPage {
                request_id: 1,
                lease,
                page: Box::new([]),
                next: Some(Box::new([1])),
                credit: CREDIT,
                payload: Box::new([]),
            },
        ];
        for response in responses {
            let (mut transport, owner) = pair(|_| {});
            let mut state = lease_on(&transport, lease, &root, cursor);
            assert!(
                transport
                    .admit_publications(&mut state, response, &|| false)
                    .is_err()
            );
            assert_eq!(state.cursor(), cursor);
            assert!(Arc::ptr_eq(&state.root(), &root));
            owner.join().expect("fixture");
        }
    }

    #[test]
    fn cancellation_interrupts_the_exact_pending_lease_socket() {
        use std::sync::{
            atomic::{AtomicBool, Ordering},
            mpsc,
        };
        let root = root();
        let cursor = Cursor::for_view_root_at(&root, 0);
        let (begun, received) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let (mut transport, owner) = pair(move |stream| {
            let _ = request(stream);
            begun.send(()).expect("pending");
            released
                .recv_timeout(Duration::from_secs(2))
                .expect("release fixture");
        });
        let interrupt = transport.interrupt_handle().expect("exact socket");
        let cancelled = Arc::new(AtomicBool::new(false));
        let worker_cancel = Arc::clone(&cancelled);
        let worker = std::thread::spawn(move || {
            transport.acquire_publications(root, cursor, &|| worker_cancel.load(Ordering::Acquire))
        });
        received
            .recv_timeout(Duration::from_secs(2))
            .expect("read pending");
        let started = Instant::now();
        cancelled.store(true, Ordering::Release);
        interrupt.interrupt();
        assert!(worker.join().expect("worker").is_err());
        assert!(started.elapsed() < Duration::from_secs(1));
        release.send(()).expect("release");
        owner.join().expect("owner");
    }

    #[test]
    fn terminal_cancel_uses_the_exact_existing_socket() {
        let root = root();
        let cursor = Cursor::for_view_root_at(&root, 0);
        let lease = LocalSubscriptionId::from_bytes([3; 16]);
        let (mut transport, owner) = pair(move |stream| {
            let (request_id, operation) = request(stream);
            assert!(
                matches!(operation, LocalSubscriptionOperation::Cancel { lease: sent } if sent == lease)
            );
            reply(stream, cancelled_reply(request_id, lease));
        });
        let state = lease_on(&transport, lease, &root, cursor);
        transport
            .cancel_publications_current_within(&state, Duration::from_secs(5))
            .expect("terminal cancel");
        owner.join().expect("owner");
        // A socket already interrupted by cancellation fails promptly; it
        // must never dial an endpoint to perform terminal cleanup.
        let (mut transport, owner) = pair(|_| {});
        let state = lease_on(&transport, lease, &root, cursor);
        transport.interrupt_handle().expect("socket").interrupt();
        assert!(transport.cancel_publications_current(&state).is_err());
        owner.join().expect("owner");
    }

    #[test]
    fn terminal_cancel_is_bounded_by_its_short_deadline_and_leaves_the_transport_unusable() {
        let root = root();
        let cursor = Cursor::for_view_root_at(&root, 0);
        let lease = LocalSubscriptionId::from_bytes([3; 16]);
        let (mut transport, owner) = pair(move |stream| {
            let _ = request(stream);
            // Never answers within any reasonable deadline.
            std::thread::sleep(Duration::from_millis(1_500));
        });
        let state = lease_on(&transport, lease, &root, cursor);
        let started = Instant::now();
        assert!(transport.cancel_publications_current(&state).is_err());
        assert!(
            started.elapsed() < Duration::from_millis(1_000),
            "the cancel waited {:?} on a silent owner",
            started.elapsed()
        );
        // The socket now carries 50 ms deadlines and may hold half a frame:
        // nothing else may be sent on it.
        assert!(matches!(
            transport.cancel_lease(lease),
            Err(ClientError::Io(message)) if message.contains("released")
        ));
        assert!(transport.cancel_publications_current(&state).is_err());
        owner.join().expect("owner");
    }

    #[test]
    fn terminal_cancel_never_takes_the_connection_rotation_path() {
        let root = root();
        let cursor = Cursor::for_view_root_at(&root, 0);
        let lease = LocalSubscriptionId::from_bytes([3; 16]);
        let (mut transport, owner) = pair(move |stream| {
            let (request_id, operation) = request(stream);
            assert!(
                matches!(operation, LocalSubscriptionOperation::Cancel { lease: sent } if sent == lease)
            );
            reply(stream, cancelled_reply(request_id, lease));
        });
        transport.exhaust_connection_budget_for_test();
        // An ordinary request would now have to rotate the connection, which a
        // stream-backed transport cannot do.
        assert!(
            transport
                .renew_lease(lease, cursor, CREDIT, LEASE_MS)
                .is_err()
        );
        let state = lease_on(&transport, lease, &root, cursor);
        transport
            .cancel_publications_current_within(&state, Duration::from_secs(5))
            .expect("the terminal cancel does not rotate");
        owner.join().expect("owner");
    }

    #[test]
    fn a_lease_held_on_another_socket_is_never_cancelled_on_this_one() {
        let root = root();
        let cursor = Cursor::for_view_root_at(&root, 0);
        let lease = LocalSubscriptionId::from_bytes([3; 16]);
        let (mut here, here_owner) = pair(|stream| {
            // Nothing may arrive: the client drops the socket instead.
            assert!(read_frame(stream, crate::limits()).is_err());
        });
        let (elsewhere, elsewhere_owner) = pair(|_| {});
        let foreign = lease_on(&elsewhere, lease, &root, cursor);
        assert_eq!(
            here.cancel_publications_current(&foreign),
            Err(protocol("publication lease is not held on this socket"))
        );
        drop(here);
        here_owner
            .join()
            .expect("no frame crossed the wrong socket");
        drop(elsewhere);
        elsewhere_owner.join().expect("owner");
    }

    #[test]
    fn a_failed_acquisition_releases_its_lease_on_the_socket_it_was_opened_on() {
        let root = root();
        let cursor = Cursor::for_view_root_at(&root, 0);
        let lease = LocalSubscriptionId::from_bytes([5; 16]);
        let (mut transport, owner) = pair(move |stream| {
            let (request_id, _) = request(stream);
            reply(stream, unadmittable_page(request_id, lease));
            let (request_id, operation) = request(stream);
            assert!(
                matches!(operation, LocalSubscriptionOperation::Cancel { lease: sent } if sent == lease),
                "the abandoned reset was not released: {operation:?}"
            );
            reply(stream, cancelled_reply(request_id, lease));
        });
        let error = transport
            .acquire_publications(root, cursor, &|| false)
            .err()
            .expect("the page cannot be admitted");
        assert_eq!(
            error,
            protocol("publication reset requires authenticated producer"),
            "the original failure, not the cleanup, is reported"
        );
        owner.join().expect("the owner saw the cancel");
    }

    #[test]
    fn a_failed_cleanup_never_hides_the_original_acquisition_failure() {
        let root = root();
        let cursor = Cursor::for_view_root_at(&root, 0);
        let lease = LocalSubscriptionId::from_bytes([5; 16]);
        let (mut transport, owner) = pair(move |stream| {
            let (request_id, _) = request(stream);
            reply(stream, unadmittable_page(request_id, lease));
            // The owner dies before the cancel can be acknowledged.
        });
        let error = transport
            .acquire_publications(root, cursor, &|| false)
            .err()
            .expect("the page cannot be admitted");
        assert_eq!(
            error,
            protocol("publication reset requires authenticated producer")
        );
        owner.join().expect("owner");
    }

    #[test]
    fn a_lease_term_longer_than_the_one_requested_is_a_protocol_violation() {
        let root = root();
        let cursor = Cursor::for_view_root_at(&root, 0);
        let lease = LocalSubscriptionId::from_bytes([3; 16]);
        let (mut transport, owner) = pair(move |stream| {
            let (request_id, _) = request(stream);
            reply(
                stream,
                opened_reply(request_id, lease, cursor, LEASE_MS + 1),
            );
            // The failed acquisition cancels; the owner just closes.
        });
        assert_eq!(
            transport
                .acquire_publications(root, cursor, &|| false)
                .err(),
            Some(protocol(
                "publication lease term exceeds the term requested"
            ))
        );
        owner.join().expect("owner");
    }

    #[test]
    fn the_renewal_cadence_follows_the_term_the_owner_reports() {
        let root = root();
        let cursor = Cursor::for_view_root_at(&root, 0);
        let lease = LocalSubscriptionId::from_bytes([3; 16]);
        let (mut transport, owner) = pair(move |stream| {
            let (request_id, _) = request(stream);
            reply(stream, opened_reply(request_id, lease, cursor, 3_000));
            let (request_id, _) = request(stream);
            reply(
                stream,
                LocalControlResponse::Subscription(LocalSubscriptionResponse::Resumed {
                    request_id,
                    lease,
                    cursor: cursor.encode_control(),
                    credit: CREDIT,
                    lease_ms: 4_000,
                }),
            );
        });
        let mut state = transport
            .acquire_publications(root, cursor, &|| false)
            .expect("a shorter grant is honored");
        assert_eq!(state.renew_after(), Duration::from_millis(1_500));
        transport
            .resume_publications(&mut state, &|| false)
            .expect("resume");
        assert_eq!(state.renew_after(), Duration::from_millis(2_000));
        owner.join().expect("owner");
    }
}
