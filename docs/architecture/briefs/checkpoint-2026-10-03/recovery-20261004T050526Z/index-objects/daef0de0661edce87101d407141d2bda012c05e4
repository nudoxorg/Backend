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
    use backend_engine::{Row, RowId, ViewPageCursor, ViewRoot, symbol_key};

    let head = crate::builtin::genesis().expect("built-in workspace genesis");
    let (initial, _, capability) =
        crate::builtin::test_view_generation(&head).expect("built-in owner view fixture");
    let basis = initial.basis();
    let root = ViewRoot::new_checked(
        initial.recipe(),
        basis,
        initial.frontier(),
        ["first", "second", "third"]
            .into_iter()
            .map(|label| Row::new(RowId::Symbol(symbol_key(label)), basis, label))
            .collect(),
        initial.coverage().to_vec(),
        capability,
    )
    .expect("bounded reset root");
    let next = root
        .page(ViewPageCursor::first(&root), 1)
        .expect("first bounded page")
        .next()
        .expect("multipage continuation");
    DurableSnapshot {
        cursor: backend_engine::Cursor::for_view_root(&root),
        root: Box::new(root),
        reason: backend_engine::CursorResetReason::Gap,
        next: SnapshotContinuation {
            cursor: next,
            token: Box::from(b"exact-page-token".as_slice()),
        },
        expires_at: Instant::now() + Duration::from_secs(90 * 60),
        pages_remaining: NonZeroU16::new(2047).expect("nonempty page budget"),
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
    assert!(
        leases
            .get(&lease)
            .is_some_and(|state| state.snapshot.is_some())
    );
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

    let prepared = prepare_snapshot_page(
        &state,
        b"exact-page-token",
        1,
        || now + Duration::from_secs(1),
        |_| Ok(Box::from(b"encoded-page".as_slice())),
    )
    .expect("exact advancing page");
    assert!(prepared.continuation.is_some());
    assert!(prepared.expires_at > original_expiry);
    let snapshot = state.snapshot.as_ref().expect("retained reset root");
    let requested = pending_page_cursor(&state, b"exact-page-token").expect("continuation");
    let page = snapshot
        .root
        .page(requested, 1)
        .expect("advancing second page");
    assert!(page_advances(&page, requested));
    assert!(!page_advances(
        &page,
        backend_engine::ViewPageCursor::first(&snapshot.root)
    ));
}

#[test]
fn page_replay_and_encoding_failure_preserve_the_pending_continuation() {
    let now = Instant::now();
    let original_expiry = now + Duration::from_secs(10);
    let state = DurableLease {
        cursor: Box::from(b"reset-target".as_slice()),
        credit: 1,
        duration: Duration::from_secs(10),
        expires_at: original_expiry,
        snapshot: Some(pending_reset_snapshot()),
    };

    let replay = prepare_snapshot_page(
        &state,
        b"replayed-page-token",
        1,
        || now,
        |_| panic!("a replay must be rejected before encoding"),
    );
    assert!(matches!(
        replay,
        Err(ProtocolError::InvalidControl(
            "snapshot page continuation mismatch or replay"
        ))
    ));
    assert_eq!(state.expires_at, original_expiry);
    assert!(state.snapshot.is_some());

    let encoding = prepare_snapshot_page(
        &state,
        b"exact-page-token",
        1,
        || now,
        |_| Err(ProtocolError::InvalidControl("snapshot page encoding")),
    );
    assert!(matches!(
        encoding,
        Err(ProtocolError::InvalidControl("snapshot page encoding"))
    ));
    assert_eq!(state.expires_at, original_expiry);
    assert!(state.snapshot.is_some(), "failed encoding dropped the root");
    assert_eq!(
        state
            .snapshot
            .as_ref()
            .map(|snapshot| snapshot.next.token.as_ref()),
        Some(b"exact-page-token".as_slice())
    );
}

#[test]
fn page_finishing_after_lease_expiry_cannot_revive_a_queued_reset() {
    let now = Instant::now();
    let expires_at = now + Duration::from_secs(10);
    let state = DurableLease {
        cursor: Box::from(b"reset-target".as_slice()),
        credit: 1,
        duration: Duration::from_secs(10),
        expires_at,
        snapshot: Some(pending_reset_snapshot()),
    };
    let mut readings = [expires_at - Duration::from_nanos(1), expires_at].into_iter();
    let result = prepare_snapshot_page(
        &state,
        b"exact-page-token",
        1,
        || readings.next().expect("two clock reads"),
        |_| Ok(Box::from(b"encoded-page".as_slice())),
    );
    assert!(matches!(
        result,
        Err(ProtocolError::InvalidControl("subscription lease expired"))
    ));
    assert_eq!(state.expires_at, expires_at);
    assert!(state.snapshot.is_some());
}

#[test]
fn page_finishing_after_absolute_reset_deadline_cannot_keep_the_root() {
    let now = Instant::now();
    let reset_expires_at = now + Duration::from_secs(10);
    let mut snapshot = pending_reset_snapshot();
    snapshot.expires_at = reset_expires_at;
    let state = DurableLease {
        cursor: Box::from(b"reset-target".as_slice()),
        credit: 1,
        duration: Duration::from_secs(30),
        expires_at: now + Duration::from_secs(30),
        snapshot: Some(snapshot),
    };
    let mut readings = [reset_expires_at - Duration::from_nanos(1), reset_expires_at].into_iter();
    let result = prepare_snapshot_page(
        &state,
        b"exact-page-token",
        1,
        || readings.next().expect("two clock reads"),
        |_| Ok(Box::from(b"encoded-page".as_slice())),
    );
    assert_eq!(result.err(), Some(ProtocolError::ResetDeadlineExceeded));
    assert!(state.snapshot.is_some());
}

#[test]
fn page_budget_rejects_a_nonfinal_page_without_consuming_its_root() {
    let now = Instant::now();
    let mut snapshot = pending_reset_snapshot();
    snapshot.pages_remaining = NonZeroU16::new(1).expect("one remaining page");
    let state = DurableLease {
        cursor: Box::from(b"reset-target".as_slice()),
        credit: 1,
        duration: Duration::from_secs(10),
        expires_at: now + Duration::from_secs(10),
        snapshot: Some(snapshot),
    };
    let result = prepare_snapshot_page(
        &state,
        b"exact-page-token",
        1,
        || now,
        |_| Ok(Box::from(b"encoded-page".as_slice())),
    );
    assert_eq!(result.err(), Some(ProtocolError::ResetPageBudgetExhausted));
    assert!(state.snapshot.is_some());
}

#[test]
fn final_page_consumes_the_last_budget_and_releases_the_reset_root() {
    let now = Instant::now();
    let mut snapshot = pending_reset_snapshot();
    let final_cursor = snapshot
        .root
        .page(snapshot.next.cursor, 1)
        .expect("penultimate page")
        .next()
        .expect("third row remains");
    snapshot.next = SnapshotContinuation {
        cursor: final_cursor,
        token: Box::from(b"final-page-token".as_slice()),
    };
    snapshot.pages_remaining = NonZeroU16::new(1).expect("last page");
    let mut state = DurableLease {
        cursor: Box::from(b"reset-target".as_slice()),
        credit: 1,
        duration: Duration::from_secs(10),
        expires_at: now + Duration::from_secs(10),
        snapshot: Some(snapshot),
    };
    let prepared = prepare_snapshot_page(
        &state,
        b"final-page-token",
        1,
        || now,
        |_| Ok(Box::from(b"encoded-final-page".as_slice())),
    )
    .expect("final page fits the remaining budget");
    assert!(prepared.continuation.is_none());
    assert!(prepared.pages_remaining.is_none());
    let (next, payload, deadline) =
        commit_snapshot_page(&mut state, prepared, 1).expect("commit completed reset");
    assert!(next.is_none());
    assert_eq!(payload.as_ref(), b"encoded-final-page");
    assert_eq!(deadline, state.expires_at);
    assert!(
        state.snapshot.is_none(),
        "completed reset root was retained"
    );
}

#[test]
fn rejected_credit_addition_leaves_the_live_lease_unchanged() {
    let max = backend_engine::MAX_SUBSCRIPTION_EVENTS;
    let mut state = DurableLease {
        cursor: Box::from(b"cursor".as_slice()),
        credit: max - 1,
        duration: Duration::from_secs(10),
        expires_at: Instant::now() + Duration::from_secs(10),
        snapshot: None,
    };
    assert_eq!(
        apply_subscription_credit(&mut state, 2),
        Err(ProtocolError::InvalidControl("subscription credit bounds"))
    );
    assert_eq!(state.credit, max - 1);
    apply_subscription_credit(&mut state, 1).expect("last valid credit");
    assert_eq!(state.credit, max);
    assert_eq!(
        increased_subscription_credit(usize::MAX, 1),
        Err(ProtocolError::InvalidControl(
            "subscription credit overflow"
        ))
    );
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
    let now = Instant::now();
    let limits =
        SubscriptionLeaseLimits::new(1, Duration::from_secs(30), 1, Duration::from_secs(30))
            .expect("finite reset budget");
    assert!(matches!(
        retained_reset_snapshot(
            *snapshot.root,
            snapshot.cursor,
            snapshot.reason,
            Some(snapshot.next),
            now,
            now,
            limits,
        ),
        Err(ProtocolError::ResetPageBudgetExhausted)
    ));
}

#[test]
fn active_lease_capacity_is_enforced_after_due_expiry_is_swept() {
    let now = Instant::now();
    let limits =
        SubscriptionLeaseLimits::new(1, Duration::from_secs(30), 1, Duration::from_secs(30))
            .expect("one lease capacity");
    let first = LocalSubscriptionId::from_bytes([0x61; 16]);
    let second = LocalSubscriptionId::from_bytes([0x62; 16]);
    let expires_at = now + Duration::from_secs(10);
    let mut leases = BTreeMap::from([(
        first,
        DurableLease {
            cursor: Box::from(b"cursor".as_slice()),
            credit: 1,
            duration: Duration::from_secs(10),
            expires_at,
            snapshot: None,
        },
    )]);
    let mut next_expiry = Some(expires_at);
    assert_eq!(
        admit_subscription_open(&mut leases, &mut next_expiry, limits, now),
        Err(ProtocolError::Backpressure)
    );
    assert_eq!(leases.len(), 1);
    assert_eq!(
        admit_subscription_open(&mut leases, &mut next_expiry, limits, expires_at),
        Ok(())
    );
    assert!(leases.is_empty());
    assert_eq!(next_expiry, None);
    leases.insert(
        second,
        DurableLease {
            cursor: Box::from(b"cursor-2".as_slice()),
            credit: 1,
            duration: Duration::from_secs(10),
            expires_at: expires_at + Duration::from_secs(10),
            snapshot: None,
        },
    );
    assert_eq!(leases.len(), 1);
    note_lease_expiry(
        &mut next_expiry,
        leases.get(&second).expect("new lease").expires_at,
    );
    assert_eq!(
        admit_subscription_open(&mut leases, &mut next_expiry, limits, expires_at),
        Err(ProtocolError::Backpressure)
    );
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
