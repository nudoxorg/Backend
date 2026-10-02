//! Local daemon service framing tests.

#![allow(clippy::expect_used, clippy::panic)]

use super::*;
use crate::protocol::{EngineRequest, EngineStatus, FrameLimits, frame, unframe};
use std::io::Cursor;

#[derive(Debug, Default)]
struct FakeOwner {
    commands: usize,
    engines: usize,
    closed: bool,
}

impl OwnerService for FakeOwner {
    fn command(&mut self, body: &[u8]) -> Result<Vec<u8>, ProtocolError> {
        self.commands += 1;
        Ok(body.to_vec())
    }

    fn engine(
        &mut self,
        _request_id: u64,
        _request: EngineRequest,
    ) -> Result<EngineStatus, ProtocolError> {
        self.engines += 1;
        Ok(EngineStatus::Accepted)
    }

    fn serve_one(&mut self) -> bool {
        false
    }

    fn close(&mut self) {
        self.closed = true;
    }
}

#[test]
fn one_owner_handles_command_and_control_payloads() {
    let limits = FrameLimits {
        max_frame: 1024,
        max_cursor: 128,
        max_frames_per_connection: 4,
        transport: backend_engine::TransportLimits {
            max_frame: 1024,
            max_chunk: 1024,
            ..backend_engine::TransportLimits::default()
        },
    };
    let mut service = LocaldService::new(FakeOwner::default(), limits).expect("service");
    let command = service.handle_payload(b"{\"version\":1}").expect("command");
    assert_eq!(command, b"{\"version\":1}");
    let control = crate::protocol::encode_engine_request(
        4,
        &EngineRequest::Subscribe {
            cursor: Box::from(b"c".as_slice()),
            credit: 1,
        },
        limits,
    )
    .expect("control");
    let response = service.handle_payload(&control).expect("response");
    assert!(matches!(
        unframe(&frame(&response, limits).expect("frame"), limits).expect("unframe"),
        payload if !payload.is_empty()
    ));
    assert_eq!(service.owner().commands, 1);
    assert_eq!(service.owner().engines, 1);
}

#[test]
fn stream_service_preserves_outer_frame_boundaries() {
    let limits = FrameLimits {
        max_frame: 1024,
        max_cursor: 128,
        max_frames_per_connection: 2,
        transport: backend_engine::TransportLimits {
            max_frame: 1024,
            max_chunk: 1024,
            ..backend_engine::TransportLimits::default()
        },
    };
    let input = frame(b"abc", limits).expect("frame");
    let mut stream = Cursor::new(input);
    let mut service = LocaldService::new(FakeOwner::default(), limits).expect("service");
    assert_eq!(service.serve_stream(&mut stream).expect("serve"), 1);
    assert_eq!(service.owner().commands, 1);
}

#[test]
fn malformed_command_errors_preserve_the_shared_reply_version_and_correlation() {
    let limits = FrameLimits {
        max_frame: 1024,
        max_cursor: 128,
        max_frames_per_connection: 2,
        transport: backend_engine::TransportLimits {
            max_frame: 1024,
            max_chunk: 1024,
            ..backend_engine::TransportLimits::default()
        },
    };
    let correlation = RequestCorrelation::from_payload(
        br#"{"version":2,"request_id":47,"command":{"kind":"unknown"}}"#,
    );
    assert_eq!(correlation, RequestCorrelation::Command(47));
    let payload = error_payload(
        correlation,
        &ProtocolError::InvalidCommand("malformed command".to_owned()),
        limits,
    );
    let reply = backend_engine::decode_reply_dto(&payload).expect("shared command error reply");
    assert_eq!(reply.version(), backend_engine::DTO_VERSION);
    assert_eq!(reply.request_id, 47);
    assert!(matches!(
        reply.reply,
        backend_engine::CommandReply::Error(_)
    ));
}

#[test]
fn ingest_execution_errors_are_not_labeled_as_invalid_command_frames() {
    let limits = FrameLimits {
        max_frame: 1024,
        max_cursor: 128,
        max_frames_per_connection: 2,
        transport: backend_engine::TransportLimits {
            max_frame: 1024,
            max_chunk: 1024,
            ..backend_engine::TransportLimits::default()
        },
    };
    let payload = error_payload(
        RequestCorrelation::Command(48),
        &ProtocolError::CommandExecution(
            "read source src/oversized.rs: typed TooLarge diagnostic".to_owned(),
        ),
        limits,
    );
    let reply = backend_engine::decode_reply_dto(&payload).expect("shared command error reply");
    let backend_engine::CommandReply::Error(message) = reply.reply else {
        panic!("execution failures must use the typed command error reply");
    };
    assert!(message.contains("command execution failed"));
    assert!(message.contains("typed TooLarge diagnostic"));
    assert!(!message.contains("invalid command frame"));
}

#[test]
fn owner_restart_rejects_old_subscription_lease_and_reacquires_distinct_identity() {
    let cursor = b"same-cursor-after-restart";
    let mut previous_identity = OwnerLeaseIdentity {
        boot_nonce: Some(OwnerBootNonce([0x11; 32])),
        next_nonce: 0,
    };
    let mut previous_leases = BTreeMap::new();
    let previous = previous_identity
        .allocate(17, cursor, &previous_leases)
        .expect("first owner lease");
    previous_leases.insert(
        previous,
        DurableLease {
            cursor: cursor.to_vec().into_boxed_slice(),
            credit: 1,
            duration: Duration::from_secs(30),
            expires_at: Instant::now() + Duration::from_secs(30),
            snapshot: None,
        },
    );
    assert!(retained_lease(&previous_leases, previous).is_ok());

    // A new process can serve the same endpoint path and receive the same
    // request ID and cursor. Its active map starts empty, and its OS-minted
    // boot namespace must make the first new lease distinct from the old one.
    let mut restarted_identity = OwnerLeaseIdentity {
        boot_nonce: Some(OwnerBootNonce([0x22; 32])),
        next_nonce: 0,
    };
    let mut restarted_leases = BTreeMap::new();
    assert!(matches!(
        retained_lease(&restarted_leases, previous),
        Err(ProtocolError::InvalidControl("unknown subscription lease"))
    ));
    let reacquired = restarted_identity
        .allocate(17, cursor, &restarted_leases)
        .expect("new owner lease");
    assert_ne!(previous, reacquired);
    restarted_leases.insert(
        reacquired,
        DurableLease {
            cursor: cursor.to_vec().into_boxed_slice(),
            credit: 1,
            duration: Duration::from_secs(30),
            expires_at: Instant::now() + Duration::from_secs(30),
            snapshot: None,
        },
    );
    assert!(retained_lease(&restarted_leases, reacquired).is_ok());
    assert!(matches!(
        retained_lease(&restarted_leases, previous),
        Err(ProtocolError::InvalidControl("unknown subscription lease"))
    ));
    assert_ne!(
        restarted_identity
            .allocate(17, cursor, &restarted_leases)
            .expect("next owner lease"),
        reacquired
    );
}

#[test]
fn subscription_lease_counter_never_wraps_to_reissue_an_old_identity() {
    let mut identity = OwnerLeaseIdentity {
        boot_nonce: Some(OwnerBootNonce([0x33; 32])),
        next_nonce: u64::MAX,
    };
    assert_eq!(
        identity.allocate(17, b"cursor", &BTreeMap::new()),
        Err(ProtocolError::LeaseIdsExhausted)
    );
}

fn pending_reset_snapshot() -> DurableSnapshot {
    use backend_engine::{
        Basis, Frontier, Lane, Reason, Row, RowId, ViewCoverage, ViewPageCursor, ViewRoot,
        object_version, symbol_key, view_key, view_state_root,
    };

    let source = view_state_root(&[]);
    let basis = Basis::new(source, object_version(b"retained-reset-root"));
    let root = ViewRoot::new_incomplete(
        view_key(b"retained-reset-root"),
        basis,
        Frontier::new(basis.branch, basis.log, basis.schema, source, 0),
        ["first", "second"]
            .into_iter()
            .map(|label| Row::new(RowId::Symbol(symbol_key(label)), basis, label))
            .collect(),
        vec![ViewCoverage::Unavailable {
            lane: Lane::Exact,
            reason: Reason::Unconfigured,
        }],
    )
    .expect("two-row reset root");
    let next = root
        .page(ViewPageCursor::first(&root), 1)
        .expect("first bounded page")
        .next()
        .expect("multipage continuation");
    DurableSnapshot {
        cursor: backend_engine::Cursor::for_view_root(&root),
        root: Box::new(root),
        reason: backend_engine::CursorResetReason::Gap,
        next: Some(next),
        next_token: Some(Box::from(b"exact-page-token".as_slice())),
        expires_at: Instant::now() + Duration::from_secs(90 * 60),
        pages_remaining: 2047,
    }
}

#[test]
fn idle_expiry_sweep_releases_abandoned_multipage_root_and_rejects_resume() {
    let now = Instant::now();
    let expires_at = now + Duration::from_secs(10);
    let lease = LocalSubscriptionId::from_bytes([0x44; 16]);
    let mut leases = BTreeMap::from([(
        lease,
        DurableLease {
            cursor: Box::from(b"reset-target".as_slice()),
            credit: 1,
            duration: Duration::from_secs(10),
            expires_at,
            snapshot: Some(pending_reset_snapshot()),
        },
    )]);
    let mut next_expiry = Some(expires_at);
    assert!(leases.get(&lease).is_some_and(|state| {
        state
            .snapshot
            .as_ref()
            .is_some_and(|snapshot| snapshot.next.is_some())
    }));
    assert_eq!(
        sweep_expired_leases(
            &mut leases,
            &mut next_expiry,
            expires_at - Duration::from_nanos(1)
        ),
        0
    );
    // No client request is made: this is the same bounded maintenance call
    // used by OwnerService::serve_one on the listener's idle poll.
    assert_eq!(
        sweep_expired_leases(&mut leases, &mut next_expiry, expires_at),
        1
    );
    assert!(
        leases.is_empty(),
        "snapshot Box<ViewRoot> left owner retention"
    );
    assert_eq!(next_expiry, None);
    assert!(matches!(
        retained_lease(&leases, lease),
        Err(ProtocolError::InvalidControl("unknown subscription lease"))
    ));
}

#[test]
fn exact_advancing_page_can_extend_lease_but_stale_page_cannot() {
    let now = Instant::now();
    let duration = Duration::from_secs(10);
    let original_expiry = now + duration;
    let state = DurableLease {
        cursor: Box::from(b"reset-target".as_slice()),
        credit: 1,
        duration,
        expires_at: original_expiry,
        snapshot: Some(pending_reset_snapshot()),
    };
    assert!(matches!(
        pending_page_cursor(&state, b"stale-page-token"),
        Err(ProtocolError::InvalidControl(
            "snapshot page continuation mismatch or replay"
        ))
    ));
    assert_eq!(state.expires_at, original_expiry);

    let requested =
        pending_page_cursor(&state, b"exact-page-token").expect("exact pending continuation");
    let page = state
        .snapshot
        .as_ref()
        .expect("retained reset root")
        .root
        .page(requested, 1)
        .expect("advancing second page");
    assert!(page_advances(&page, requested));
    assert!(
        lease_deadline(now + Duration::from_secs(8), state.duration).expect("progress deadline")
            > original_expiry
    );
    assert!(!page_advances(
        &page,
        backend_engine::ViewPageCursor::first(&state.snapshot.as_ref().expect("root").root)
    ));
}

#[test]
fn renewed_lease_survives_stale_minimum_hint_then_expires_at_new_deadline() {
    let now = Instant::now();
    let old_expiry = now + Duration::from_secs(10);
    let new_expiry = now + Duration::from_secs(20);
    let lease = LocalSubscriptionId::from_bytes([0x55; 16]);
    let mut leases = BTreeMap::from([(
        lease,
        DurableLease {
            cursor: Box::from(b"cursor".as_slice()),
            credit: 1,
            duration: Duration::from_secs(10),
            expires_at: old_expiry,
            snapshot: None,
        },
    )]);
    let mut next_expiry = Some(old_expiry);
    leases.get_mut(&lease).expect("active lease").expires_at = new_expiry;
    note_lease_expiry(&mut next_expiry, new_expiry);
    assert_eq!(next_expiry, Some(old_expiry));
    assert_eq!(
        sweep_expired_leases(&mut leases, &mut next_expiry, old_expiry),
        0
    );
    assert!(retained_lease(&leases, lease).is_ok());
    assert_eq!(next_expiry, Some(new_expiry));
    assert_eq!(
        sweep_expired_leases(&mut leases, &mut next_expiry, new_expiry),
        1
    );
}

#[test]
fn reset_deadline_reclaims_root_even_when_normal_lease_is_renewed() {
    let now = Instant::now();
    let reset_deadline = now + Duration::from_secs(10);
    let lease = LocalSubscriptionId::from_bytes([0x57; 16]);
    let mut snapshot = pending_reset_snapshot();
    snapshot.expires_at = reset_deadline;
    let mut leases = BTreeMap::from([(
        lease,
        DurableLease {
            cursor: Box::from(b"reset-target".as_slice()),
            credit: 1,
            duration: Duration::from_secs(30),
            expires_at: now + Duration::from_secs(30),
            snapshot: Some(snapshot),
        },
    )]);
    let mut next_expiry = Some(retention_deadline(
        leases.get(&lease).expect("retained root"),
    ));
    assert_eq!(next_expiry, Some(reset_deadline));
    assert_eq!(
        sweep_expired_leases(&mut leases, &mut next_expiry, reset_deadline),
        1
    );
    assert!(leases.is_empty());
    assert_eq!(next_expiry, None);
}

#[test]
fn initial_reset_page_cannot_retain_a_continuation_beyond_page_budget() {
    let snapshot = pending_reset_snapshot();
    let limits =
        SubscriptionLeaseLimits::new(1, Duration::from_secs(30), 1, Duration::from_secs(30))
            .expect("finite reset budget");
    assert!(matches!(
        retained_reset_snapshot(
            *snapshot.root,
            snapshot.cursor,
            snapshot.reason,
            snapshot.next,
            snapshot.next_token,
            Instant::now(),
            limits,
        ),
        Err(ProtocolError::ResetPageBudgetExhausted)
    ));
}

#[test]
fn shutdown_clears_all_retained_leases_and_deadline_hint() {
    let lease = LocalSubscriptionId::from_bytes([0x66; 16]);
    let expires_at = Instant::now() + Duration::from_secs(30);
    let mut leases = BTreeMap::from([(
        lease,
        DurableLease {
            cursor: Box::from(b"cursor".as_slice()),
            credit: 1,
            duration: Duration::from_secs(30),
            expires_at,
            snapshot: Some(pending_reset_snapshot()),
        },
    )]);
    let mut next_expiry = Some(expires_at);
    release_all_leases(&mut leases, &mut next_expiry);
    assert!(leases.is_empty());
    assert_eq!(next_expiry, None);
}

#[test]
fn subscription_retention_configuration_stays_finite() {
    assert!(SubscriptionLeaseLimits::default().duration(10_000).is_ok());
    assert_eq!(
        SubscriptionLeaseLimits::new(0, Duration::from_secs(1), 1, Duration::from_secs(1)),
        Err(ProtocolError::InvalidLimits)
    );
    assert_eq!(
        SubscriptionLeaseLimits::new(1025, Duration::from_secs(1), 1, Duration::from_secs(1)),
        Err(ProtocolError::InvalidLimits)
    );
    assert_eq!(
        SubscriptionLeaseLimits::new(
            1,
            Duration::from_secs(60 * 60 + 1),
            1,
            Duration::from_secs(1)
        ),
        Err(ProtocolError::InvalidLimits)
    );
    assert_eq!(
        SubscriptionLeaseLimits::new(1, Duration::from_secs(1), 4097, Duration::from_secs(1)),
        Err(ProtocolError::InvalidLimits)
    );
    assert_eq!(
        SubscriptionLeaseLimits::new(
            1,
            Duration::from_secs(1),
            1,
            Duration::from_secs(4 * 60 * 60 + 1)
        ),
        Err(ProtocolError::InvalidLimits)
    );
    assert!(
        SubscriptionLeaseLimits::new(
            1,
            Duration::from_secs(60 * 60),
            2048,
            Duration::from_secs(90 * 60)
        )
        .is_ok()
    );
}
