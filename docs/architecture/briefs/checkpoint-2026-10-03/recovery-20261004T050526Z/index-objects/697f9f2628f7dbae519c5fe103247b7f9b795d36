//! Proof-admitted state of one durable local publication lease. Socket work
//! belongs to the caller's worker; cancellation is checked between bounded
//! frames. No partial reset can replace the retained complete root.

use crate::subscription::{
    snapshot_page_from_bytes_with_verifier, subscription_read_from_bytes_against,
};
use crate::{ClientError, LocalSubscriptionTransport};
use backend_library::{Cursor, CursorEvent, CursorRead, SnapshotHydrator, ViewRoot};
use backend_replication::{LocalSubscriptionId, LocalSubscriptionResponse};
use std::sync::Arc;
use std::time::{Duration, Instant};

const CREDIT: usize = 64;
const LEASE_MS: u64 = 10_000;
const MAX_RESET_ROWS: u64 = 131_072;
const RESET_TIME: Duration = Duration::from_secs(10);

/// Exact producer state retained across socket reconnects. The lease is an
/// owner-issued identity, never reconstructed from a path or a clock.
pub struct PublicationLease {
    lease: LocalSubscriptionId,
    cursor: Cursor,
    root: Arc<ViewRoot>,
}

impl PublicationLease {
    /// Complete root whose producer proofs have been admitted.
    pub fn root(&self) -> Arc<ViewRoot> {
        Arc::clone(&self.root)
    }
    /// Exact admitted producer cursor.
    pub const fn cursor(&self) -> Cursor {
        self.cursor
    }
}

impl LocalSubscriptionTransport {
    /// Acquires a lease from a previously admitted exact root/cursor.
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
        let response = self.open_lease(Some(cursor), CREDIT, LEASE_MS)?;
        let lease = response.lease();
        let mut state = PublicationLease {
            lease,
            cursor,
            root,
        };
        self.admit_publications(&mut state, response, cancelled)?;
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
        let response = self.resume_lease(state.lease, state.cursor, CREDIT, LEASE_MS)?;
        self.admit_publications(state, response, cancelled)
    }

    /// Fences a quiet lease to its exact admitted cursor before an idle pause.
    /// # Errors
    /// Returns an error if the owner no longer retains this exact lease.
    pub fn renew_publications(&mut self, state: &PublicationLease) -> Result<(), ClientError> {
        match self.renew_lease(state.lease, state.cursor, CREDIT, LEASE_MS)? {
            LocalSubscriptionResponse::Renewed { cursor, .. }
                if cursor.as_ref() == state.cursor.encode_control().as_ref() =>
            {
                Ok(())
            }
            _ => Err(protocol(
                "publication renewal did not fence the admitted cursor",
            )),
        }
    }

    /// Releases owner-side retention. Socket I/O remains subject to this
    /// transport's timeout; a caller closing a cancelled socket can instead
    /// drop it and let the finite producer lease expire.
    pub fn cancel_publications(&mut self, state: &PublicationLease) -> Result<(), ClientError> {
        match self.cancel_lease(state.lease)? {
            LocalSubscriptionResponse::Cancelled { .. } => Ok(()),
            _ => Err(protocol("publication cancellation was not acknowledged")),
        }
    }

    fn admit_publications(
        &mut self,
        state: &mut PublicationLease,
        mut response: LocalSubscriptionResponse,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<(), ClientError> {
        let deadline = Instant::now() + RESET_TIME;
        let previous = state.cursor;
        let mut hydrator: Option<SnapshotHydrator> = None;
        let mut expected_page: Box<[u8]> = Box::new([]);
        let (root, cursor) = loop {
            check(cancelled)?;
            if Instant::now() >= deadline {
                return Err(protocol("publication reset exceeded its time budget"));
            }
            if response.lease() != state.lease {
                return Err(protocol("publication response changed lease identity"));
            }
            match response {
                LocalSubscriptionResponse::Opened { cursor, .. }
                | LocalSubscriptionResponse::Resumed { cursor, .. } => {
                    if hydrator.is_some() || cursor.as_ref() != previous.encode_control().as_ref() {
                        return Err(protocol("publication acknowledgement changed its cursor"));
                    }
                    break (Arc::clone(&state.root), previous);
                }
                LocalSubscriptionResponse::Batch {
                    previous: predecessor,
                    cursor: target,
                    payload,
                    ..
                } => {
                    if hydrator.is_some()
                        || predecessor.as_ref() != previous.encode_control().as_ref()
                    {
                        return Err(protocol("publication batch predecessor mismatch"));
                    }
                    let read = subscription_read_from_bytes_against(
                        &payload,
                        previous,
                        &state.root,
                        state.root.capability(),
                    )?;
                    let CursorRead::Events { cursor, events } = read else {
                        return Err(protocol("publication batch carried a reset"));
                    };
                    if events.len() > CREDIT || target.as_ref() != cursor.encode_control().as_ref()
                    {
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
                    break (root, cursor);
                }
                LocalSubscriptionResponse::SnapshotPage {
                    page,
                    next,
                    payload,
                    ..
                } => {
                    if page != expected_page {
                        return Err(protocol("publication reset page mismatch"));
                    }
                    let peer = self.authenticated_peer().ok_or_else(|| {
                        protocol("publication reset requires authenticated producer")
                    })?;
                    let claim =
                        snapshot_page_from_bytes_with_verifier(&payload, previous, None, peer)?;
                    if claim.descriptor().row_count() > MAX_RESET_ROWS {
                        return Err(protocol("publication reset exceeds its row budget"));
                    }
                    let admitted_next = claim.next_token().map_err(ClientError::Protocol)?;
                    if admitted_next.as_deref() != next.as_deref() {
                        return Err(protocol(
                            "publication reset continuation is not authenticated",
                        ));
                    }
                    let current = match hydrator.take() {
                        Some(mut current) => {
                            current.push_page(claim).map_err(ClientError::Protocol)?;
                            current
                        }
                        None => SnapshotHydrator::start(previous, claim)
                            .map_err(ClientError::Protocol)?,
                    };
                    if current.is_complete() {
                        let CursorRead::Reset { cursor, root, .. } =
                            current.finish().map_err(ClientError::Protocol)?
                        else {
                            return Err(protocol("publication hydration did not reset"));
                        };
                        break (Arc::from(root), cursor);
                    }
                    expected_page =
                        next.ok_or_else(|| protocol("publication reset omitted continuation"))?;
                    hydrator = Some(current);
                    check(cancelled)?;
                    response = self.snapshot_page(state.lease, expected_page.clone(), CREDIT)?;
                }
                _ => return Err(protocol("unexpected publication lifecycle response")),
            }
        };
        check(cancelled)?;
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
        state.cursor = cursor;
        state.root = root;
        Ok(())
    }
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

#[cfg(all(test, unix))]
#[allow(clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use backend_library::{
        AuthorityScopeClaim, Basis, CoverageCapability, Frontier, ProducerObservationClaims,
        ProducerObservationVerifier, ScopeRoot, UntrustedProducerObservation, admit_complete_scope,
        admit_producer_observation, object_version, view_key, view_state_root,
    };
    use backend_replication::{
        LocalControlRequest, LocalControlResponse, LocalSubscriptionOperation, decode_request,
        encode_response, read_frame, write_frame,
    };
    use std::os::unix::net::UnixStream;
    use std::thread::JoinHandle;

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
        serve: impl FnOnce(&mut UnixStream) + Send + 'static,
    ) -> (LocalSubscriptionTransport, JoinHandle<()>) {
        let (client, mut server) = UnixStream::pair().expect("pair");
        server
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("server bound");
        server
            .set_write_timeout(Some(Duration::from_secs(2)))
            .expect("server bound");
        let join = std::thread::spawn(move || serve(&mut server));
        (LocalSubscriptionTransport::from_stream(client), join)
    }
    fn request(stream: &mut UnixStream) -> (u64, LocalSubscriptionOperation) {
        let body = read_frame(stream, crate::limits()).expect("request frame");
        let LocalControlRequest::Subscription(request) =
            decode_request(&body, crate::limits()).expect("request")
        else {
            panic!("wrong lane")
        };
        (request.request_id, request.operation)
    }
    fn reply(stream: &mut UnixStream, response: LocalControlResponse) {
        let body = encode_response(&response, crate::limits()).expect("response");
        write_frame(stream, &body, crate::limits()).expect("response frame");
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
            reply(
                stream,
                LocalControlResponse::Subscription(LocalSubscriptionResponse::Opened {
                    request_id,
                    lease,
                    cursor: cursor.encode_control(),
                    credit: CREDIT,
                    lease_ms: LEASE_MS,
                }),
            );
        });
        let mut state = transport
            .acquire_publications(Arc::clone(&root), cursor, &|| false)
            .expect("acquire");
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
            reply(
                stream,
                LocalControlResponse::Subscription(LocalSubscriptionResponse::Cancelled {
                    request_id,
                    lease,
                }),
            );
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
            let mut state = PublicationLease {
                lease,
                root: Arc::clone(&root),
                cursor,
            };
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
                reply(
                    stream,
                    LocalControlResponse::Subscription(LocalSubscriptionResponse::Opened {
                        request_id,
                        lease: fresh,
                        cursor: cursor.encode_control(),
                        credit: CREDIT,
                        lease_ms: LEASE_MS,
                    }),
                );
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
            let mut state = PublicationLease {
                lease,
                cursor,
                root: Arc::clone(&root),
            };
            let (mut transport, owner) = pair(|_| {});
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
}
