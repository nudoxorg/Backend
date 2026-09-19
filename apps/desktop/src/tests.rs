//! Desktop adapter tests.
#![allow(clippy::expect_used)]

use super::subscription::{
    cursor_from_value, cursor_value, subscription_read_from_value, subscription_request_value,
};
#[cfg(unix)]
use super::transport::{encode_control_request, read_frame, write_frame};
use super::*;
use backend_library::{
    AuthorityScopeClaim, Basis, CoverageCapability, Cursor, CursorEvent, CursorRead,
    CursorResetReason, Frontier, ProducerObservationVerifier, Row, RowId, ScopeRoot,
    SemanticObject, UntrustedProducerObservation, ViewDelta, ViewRoot, WireCertificate, WireClaim,
    WireSchema, admit_complete_scope, admit_producer_observation, empty_view_relation_preimage,
    encode_id, intent_id, object_version, symbol_key, view_key,
};
use backend_replication::{LocalSubscriptionId, LocalSubscriptionResponse};
use serde_json::{Value, json};

#[cfg(unix)]
use backend_replication::{
    LocalControlLimits, LocalControlRequest, LocalControlResponse,
    encode_request as encode_local_request, encode_response as encode_local_response,
};

fn capability(object: SemanticObject) -> CoverageCapability {
    let declared = AuthorityScopeClaim::from_object_version(object);
    let scope = ScopeRoot::from_bytes(object.to_bytes());
    let observation = admit_producer_observation(
        UntrustedProducerObservation::new(
            *scope.as_bytes(),
            scope,
            *scope.as_bytes(),
            scope.as_bytes().to_vec(),
        ),
        &TestCoverageVerifier,
    )
    .expect("producer observation");
    CoverageCapability::from_authorized_with_evidence(
        admit_complete_scope(declared, observation).expect("producer coverage"),
        scope.as_bytes().to_vec(),
    )
    .expect("coverage evidence")
}

struct TestCoverageVerifier;

impl ProducerObservationVerifier for TestCoverageVerifier {
    type Error = &'static str;

    fn verify(&self, observation: &UntrustedProducerObservation) -> Result<(), Self::Error> {
        if observation.producer_identity() == *observation.scope_root().as_bytes()
            && observation.context() == *observation.scope_root().as_bytes()
            && observation.evidence() == observation.scope_root().as_bytes()
        {
            Ok(())
        } else {
            Err("invalid test producer observation")
        }
    }
}

fn root() -> ViewRoot {
    let source_root = backend_library::view_state_root(&[]);
    let basis = Basis::new(source_root, object_version(b"source"));
    ViewRoot::empty_checked(
        view_key(b"view"),
        basis,
        Frontier::new(basis.branch, basis.log, basis.schema, source_root, 0),
        capability(basis.object),
    )
    .expect("empty root")
}

struct Fake {
    read: Option<CursorRead>,
    request: Option<SubscriptionRequest>,
}

impl SubscriptionTransport for Fake {
    fn subscribe(&mut self, request: SubscriptionRequest) -> Result<CursorRead, ClientError> {
        self.request = Some(request);
        self.read
            .take()
            .ok_or_else(|| ClientError::Protocol("missing read".to_owned()))
    }
}

#[test]
fn model_rejects_wrong_basis_at_construction() {
    let value = root();
    let other = backend_library::view_state_root(&[("other".to_owned(), "root".to_owned())]);
    assert!(matches!(
        Model::try_new(value, other),
        Err(ClientError::BasisMismatch { .. })
    ));
}

#[test]
fn desktop_updates_rows_and_tracks_exact_cursor() {
    let source_root = backend_library::view_state_root(&[]);
    let basis = Basis::new(source_root, object_version(b"source"));
    let frontier = Frontier::new(basis.branch, basis.log, basis.schema, source_root, 0);
    let base =
        ViewRoot::empty_checked(view_key(b"view"), basis, frontier, capability(basis.object))
            .expect("empty root");
    let row = Row::new(RowId::Symbol(symbol_key("pkg::Thing")), basis, "Thing");
    let prepared = base
        .prepare(ViewDelta::Upsert { row }, capability(basis.object))
        .expect("prepare");
    let (next, delta) = base.clone().commit(prepared).expect("commit");
    let cursor = Cursor::for_view(
        ViewRoot::recipe(&next),
        ViewRoot::version(&next),
        Frontier::new(
            ViewRoot::frontier(&next).branch,
            ViewRoot::frontier(&next).log,
            ViewRoot::frontier(&next).schema,
            ViewRoot::root(&next),
            1,
        ),
    );
    let mut model = Model::try_new(base, source_root).expect("model");
    let mut fake = Fake {
        read: Some(CursorRead::Events {
            cursor,
            events: vec![CursorEvent::View {
                delta: Box::new(delta),
            }]
            .into_boxed_slice(),
        }),
        request: None,
    };
    model.poll_transport(&mut fake).expect("poll");
    assert_eq!(ViewRoot::root(&model.root), ViewRoot::root(&next));
    assert_eq!(model.cursor(), cursor);
    assert_eq!(fake.request.expect("request").credit, MAX_EVENTS);
}

#[test]
fn desktop_restores_the_owner_cursor_without_deriving_stream_progress() {
    let view = root();
    let cursor = Cursor::for_view(
        view.recipe(),
        view.version(),
        Frontier::new(
            view.frontier().branch,
            view.frontier().log,
            view.frontier().schema,
            view.root(),
            7,
        ),
    );
    let mut model = Model::try_new_at(view.clone(), cursor, view.basis().root).expect("model");
    assert_eq!(model.cursor(), cursor);

    let next = Cursor::for_view(
        view.recipe(),
        view.version(),
        Frontier::new(
            view.frontier().branch,
            view.frontier().log,
            view.frontier().schema,
            view.root(),
            8,
        ),
    );
    model
        .reduce_checked(CursorRead::Events {
            cursor: next,
            events: vec![CursorEvent::Intent {
                id: intent_id("restored", b"cursor"),
            }]
            .into_boxed_slice(),
        })
        .expect("intent suffix");
    assert_eq!(model.cursor(), next);
    assert_eq!(model.root().root(), view.root());
}

#[test]
fn durable_open_reconciles_owner_intent_progress() {
    let view = root();
    let expected_root = view.root();
    let owner_cursor = Cursor::for_view_root_at(&view, 7);
    let mut model = Model::try_new(view, backend_library::view_state_root(&[])).expect("model");
    model
        .reduce_lease_response(
            LocalSubscriptionResponse::Opened {
                request_id: 1,
                lease: LocalSubscriptionId::from_bytes([7; 16]),
                cursor: owner_cursor.encode_control(),
                credit: MAX_EVENTS,
                lease_ms: 30_000,
            },
            None,
        )
        .expect("owner cursor");
    assert_eq!(model.cursor(), owner_cursor);
    assert_eq!(model.root().root(), expected_root);
}

#[test]
fn gap_reset_is_explicit_and_bounded() {
    let base = root();
    let cursor = Cursor::for_view(
        ViewRoot::recipe(&base),
        ViewRoot::version(&base),
        Frontier::new(
            ViewRoot::frontier(&base).branch,
            ViewRoot::frontier(&base).log,
            ViewRoot::frontier(&base).schema,
            ViewRoot::root(&base),
            5,
        ),
    );
    let mut model = Model::try_new(base.clone(), ViewRoot::basis(&base).root).expect("model");
    let mut fake = Fake {
        read: Some(CursorRead::Reset {
            cursor,
            root: Box::new(base),
            reason: CursorResetReason::Gap,
        }),
        request: None,
    };
    model.poll_transport(&mut fake).expect("reset");
    assert_eq!(model.reset_count, 1);
    assert_eq!(model.cursor().sequence(), 5);
}

#[test]
fn reset_rejects_an_identity_only_incomplete_root() {
    let base = root();
    let incomplete = ViewRoot::new_incomplete(
        ViewRoot::recipe(&base),
        ViewRoot::basis(&base),
        ViewRoot::frontier(&base),
        Vec::new(),
        Vec::new(),
    )
    .expect("incomplete replacement root");
    let cursor = Cursor::for_view(
        ViewRoot::recipe(&incomplete),
        ViewRoot::version(&incomplete),
        Frontier::new(
            ViewRoot::frontier(&incomplete).branch,
            ViewRoot::frontier(&incomplete).log,
            ViewRoot::frontier(&incomplete).schema,
            ViewRoot::root(&incomplete),
            ViewRoot::frontier(&incomplete).sequence + 1,
        ),
    );
    let mut model = Model::try_new(base, backend_library::view_state_root(&[])).expect("model");
    let result = model.reduce_checked(CursorRead::Reset {
        cursor,
        root: Box::new(incomplete),
        reason: CursorResetReason::Gap,
    });
    assert!(matches!(
        result,
        Err(ClientError::Protocol(message)) if message.contains("complete coverage")
    ));
}

#[test]
fn oversized_credit_and_reply_are_rejected() {
    assert!(SubscriptionRequest::new(Cursor::new(), MAX_EVENTS + 1).is_err());
    let mut model = Model::try_new(root(), backend_library::view_state_root(&[])).expect("model");
    let oversized = CursorRead::Events {
        cursor: model.cursor(),
        events: vec![
            CursorEvent::Intent {
                id: intent_id("x", b"x"),
            };
            MAX_EVENTS + 1
        ]
        .into_boxed_slice(),
    };
    assert!(model.reduce_checked(oversized).is_err());
}

#[test]
fn request_wire_has_version_and_credit() {
    let value =
        subscription_request_value(SubscriptionRequest::new(Cursor::new(), 4).expect("request"));
    assert_eq!(
        value.get("version").and_then(Value::as_u64),
        Some(u64::from(backend_library::protocol_version()))
    );
    assert_eq!(value.get("credit").and_then(Value::as_u64), Some(4));
}

#[test]
fn subscription_wire_rejects_unknown_fields_before_reduction() {
    let cursor = Cursor::new();
    let mut forged = cursor_value(cursor);
    forged["extra"] = json!(true);
    assert!(cursor_from_value(&forged, cursor).is_err());
    let reply = json!({
        "version": backend_library::protocol_version(),
        "kind": "events",
        "cursor": cursor_value(cursor),
        "events": [],
        "extra": true,
    });
    assert!(subscription_read_from_value(&reply, cursor).is_err());
    let identity_free = json!({
        "version": backend_library::protocol_version(),
        "kind": "events",
        "cursor": cursor_value(cursor),
        "events": [],
        "certificate": null,
    });
    assert!(subscription_read_from_value(&identity_free, cursor).is_ok());
}

#[test]
fn cursor_wire_requires_proof_against_the_requested_cursor() {
    let cursor = Cursor::new();
    assert_eq!(
        cursor_from_value(&cursor_value(cursor), cursor).expect("cursor"),
        cursor
    );
    let mut forged = cursor_value(cursor);
    forged["schema"] = json!(u64::from(cursor.schema()) + 1);
    assert!(cursor_from_value(&forged, cursor).is_err());
}

#[cfg(unix)]
#[test]
fn unix_endpoint_path_is_bounded() {
    assert!(matches!(
        UnixSubscriptionTransport::connect("x".repeat(MAX_ENDPOINT_PATH + 1)),
        Err(ClientError::Transport(_))
    ));
}

#[cfg(unix)]
#[test]
fn desktop_control_request_and_response_match_shared_wire_codec() {
    let request = SubscriptionRequest::new(Cursor::new(), MAX_EVENTS).expect("request");
    let desktop = encode_control_request(7, request).expect("desktop request");
    let shared = encode_local_request(
        &LocalControlRequest::Subscribe {
            request_id: 7,
            cursor: request
                .cursor
                .encode_control()
                .into_vec()
                .into_boxed_slice(),
            credit: request.credit,
        },
        LocalControlLimits::default(),
    )
    .expect("shared request");
    assert_eq!(desktop, shared);

    let payload = br"{}";
    let desktop = control_success(11, payload);
    let shared = encode_local_response(
        &LocalControlResponse::AcceptedPayload {
            request_id: 11,
            payload: payload.to_vec().into_boxed_slice(),
        },
        LocalControlLimits::default(),
    )
    .expect("shared response");
    assert_eq!(desktop, shared);
}

#[cfg(unix)]
#[test]
fn unix_subscription_admits_empty_success_and_rejects_mutated_cursor() {
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::UnixListener;

    let path = std::env::temp_dir().join(format!(
        "backend-desktop-test-{}-{}.sock",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    let listener = UnixListener::bind(&path).expect("listener");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
        .expect("private listener permissions");
    let expected_cursor = root_cursor();
    let server_thread = std::thread::spawn(move || {
        let (mut server, _) = listener.accept().expect("accept");
        let body = read_frame(&mut server).expect("request");
        assert_eq!(&body[..4], b"LDC2");
        assert_eq!(body[4], 1);
        assert_eq!(body[5], 3);
        let request_id = u64::from_be_bytes(body[6..14].try_into().expect("request id"));
        let cursor_length = usize::try_from(u32::from_be_bytes(
            body[14..18].try_into().expect("cursor length"),
        ))
        .expect("cursor length");
        assert_eq!(body.len(), 26 + cursor_length);
        assert_eq!(
            u64::from_be_bytes(body[18..26].try_into().expect("credit")),
            MAX_EVENTS as u64
        );
        let payload = serde_json::to_vec(&json!({
            "version": backend_library::protocol_version(),
            "kind": "events",
            "cursor": cursor_value(expected_cursor),
            "events": [],
        }))
        .expect("reply payload");
        let reply = control_success(request_id, &payload);
        write_frame(&mut server, &reply).expect("reply");
    });
    let mut model = Model::try_new(root(), backend_library::view_state_root(&[])).expect("model");
    let mut transport = UnixSubscriptionTransport::connect(&path).expect("connect");
    model.poll_transport(&mut transport).expect("empty success");
    assert_eq!(model.cursor().sequence(), 0);
    server_thread.join().expect("server");
    std::fs::remove_file(&path).expect("remove socket");

    let listener = UnixListener::bind(&path).expect("listener");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
        .expect("private listener permissions");
    let expected_cursor = model.cursor();
    let server_thread = std::thread::spawn(move || {
        let (mut server, _) = listener.accept().expect("accept");
        let _body = read_frame(&mut server).expect("request");
        let mut cursor = cursor_value(expected_cursor);
        cursor["root"] = json!(encode_id(
            backend_library::view_state_root(&[("forged".to_owned(), "root".to_owned())])
                .as_bytes()
        ));
        let reply = json!({
            "version": backend_library::protocol_version(),
            "kind": "events",
            "cursor": cursor,
            "events": [],
        });
        let body = serde_json::to_vec(&reply).expect("forged reply");
        write_frame(&mut server, &body).expect("reply");
    });
    let mut transport = UnixSubscriptionTransport::connect(&path).expect("connect");
    let request = SubscriptionRequest::new(Cursor::new(), MAX_EVENTS).expect("request");
    assert!(transport.subscribe(request).is_err());
    server_thread.join().expect("server");
    std::fs::remove_file(path).expect("remove socket");
}

#[cfg(unix)]
#[test]
fn unix_subscription_rejects_ack_without_a_read_payload() {
    use std::os::unix::net::UnixStream;

    let (mut server, client) = UnixStream::pair().expect("pair");
    let server_thread = std::thread::spawn(move || {
        let request = read_frame(&mut server).expect("request");
        let request_id = u64::from_be_bytes(request[6..14].try_into().expect("request id"));
        let mut reply = Vec::from(*b"LDC2");
        reply.extend_from_slice(&[1, 0]);
        reply.extend_from_slice(&request_id.to_be_bytes());
        reply.extend_from_slice(&0_u32.to_be_bytes());
        write_frame(&mut server, &reply).expect("reply");
    });
    let mut transport = UnixSubscriptionTransport::from_stream(client);
    assert!(
        transport
            .subscribe(SubscriptionRequest::new(Cursor::new(), MAX_EVENTS).expect("request"))
            .is_err()
    );
    server_thread.join().expect("server");
}

#[cfg(unix)]
#[test]
fn unix_subscription_rotates_before_the_listener_frame_limit() {
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::UnixListener;

    let path = std::path::PathBuf::from("/tmp").join(format!(
        "bdr-{}-{}.sock",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    let listener = UnixListener::bind(&path).expect("listener");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
        .expect("private listener permissions");
    let expected_cursor = root_cursor();
    let server = std::thread::spawn(move || {
        for expected_frames in [240_usize, 1] {
            let (mut stream, _) = listener.accept().expect("accept");
            for _ in 0..expected_frames {
                let body = read_frame(&mut stream).expect("request");
                let request_id = u64::from_be_bytes(body[6..14].try_into().expect("request id"));
                let payload = serde_json::to_vec(&json!({
                    "version": backend_library::protocol_version(),
                    "kind": "events",
                    "cursor": cursor_value(expected_cursor),
                    "events": [],
                }))
                .expect("payload");
                write_frame(&mut stream, &control_success(request_id, &payload)).expect("reply");
            }
        }
    });
    let mut transport = UnixSubscriptionTransport::connect(&path).expect("connect");
    for _ in 0..241 {
        transport
            .subscribe(SubscriptionRequest::new(expected_cursor, MAX_EVENTS).expect("subscription"))
            .expect("bounded connection rotation");
    }
    server.join().expect("server");
    std::fs::remove_file(path).expect("remove socket");
}

#[cfg(unix)]
fn root_cursor() -> Cursor {
    CursorSub::for_view(&root(), MAX_EVENTS).cursor()
}

#[cfg(unix)]
fn control_success(request_id: u64, payload: &[u8]) -> Vec<u8> {
    encode_local_response(
        &LocalControlResponse::AcceptedPayload {
            request_id,
            payload: payload.to_vec().into_boxed_slice(),
        },
        LocalControlLimits::default(),
    )
    .expect("control response")
}

fn empty_relation_canonical_bytes() -> Box<[u8]> {
    empty_view_relation_preimage().into_boxed_slice()
}

fn cursor_certificate(cursor: Cursor, intent: backend_library::IntentId) -> WireCertificate {
    WireCertificate::new()
        .with_claim(WireClaim::Key {
            schema: WireSchema::ViewRecipe,
            id: encode_id(cursor.recipe().as_bytes()),
            value: "default-view".to_owned(),
        })
        .with_claim(WireClaim::Version {
            schema: WireSchema::ViewVersion,
            id: encode_id(cursor.version().as_bytes()),
            value: cursor.root().as_bytes().to_vec().into_boxed_slice(),
        })
        .with_claim(WireClaim::Key {
            schema: WireSchema::Branch,
            id: encode_id(cursor.branch().as_bytes()),
            value: "main".to_owned(),
        })
        .with_claim(WireClaim::Key {
            schema: WireSchema::Log,
            id: encode_id(cursor.log().as_bytes()),
            value: "library".to_owned(),
        })
        .with_claim(WireClaim::Root {
            schema: WireSchema::ViewRelation,
            id: encode_id(cursor.root().as_bytes()),
            canonical: empty_relation_canonical_bytes(),
        })
        .with_claim(WireClaim::Intent {
            id: encode_id(intent.as_bytes()),
            token: "event".to_owned(),
            payload: b"payload".to_vec().into_boxed_slice(),
        })
}

#[cfg(unix)]
#[test]
fn unix_subscription_consumes_certified_event_and_rejects_event_mutation() {
    use std::os::unix::net::UnixStream;

    let previous = Cursor::new();
    let next = Cursor::for_view(
        previous.recipe(),
        previous.version(),
        Frontier::new(
            previous.branch(),
            previous.log(),
            previous.schema(),
            previous.root(),
            1,
        ),
    );
    let intent = intent_id("event", b"payload");
    let event = EventDto::new(next, CursorEvent::Intent { id: intent })
        .with_certificate(cursor_certificate(next, intent));
    let (mut server, client) = UnixStream::pair().expect("pair");
    let event_value = serde_json::to_value(&event).expect("event");
    let server_thread = std::thread::spawn(move || {
        let body = read_frame(&mut server).expect("request");
        assert_eq!(&body[..4], b"LDC2");
        let request_id = u64::from_be_bytes(body[6..14].try_into().expect("request id"));
        let reply = json!({
            "version": backend_library::protocol_version(),
            "kind": "events",
            "cursor": cursor_value(next),
            "events": [event_value],
        });
        let payload = serde_json::to_vec(&reply).expect("reply payload");
        write_frame(&mut server, &control_success(request_id, &payload)).expect("reply frame");
    });
    let mut transport = UnixSubscriptionTransport::from_stream(client);
    let read = transport
        .subscribe_with_certificate(
            SubscriptionRequest::new(previous, MAX_EVENTS).expect("request"),
            None,
        )
        .expect("certified event");
    assert!(
        matches!(read, CursorRead::Events { cursor, events } if cursor == next && events.len() == 1)
    );
    server_thread.join().expect("server");

    let (mut server, client) = UnixStream::pair().expect("pair");
    let event = EventDto::new(next, CursorEvent::Intent { id: intent })
        .with_certificate(cursor_certificate(next, intent));
    let mut event_value = serde_json::to_value(&event).expect("event");
    event_value["event"]["data"]["id"] = json!(encode_id(intent_id("forged", b"event").as_bytes()));
    let server_thread = std::thread::spawn(move || {
        let body = read_frame(&mut server).expect("request");
        assert_eq!(&body[..4], b"LDC2");
        let request_id = u64::from_be_bytes(body[6..14].try_into().expect("request id"));
        let reply = json!({
            "version": backend_library::protocol_version(),
            "kind": "events",
            "cursor": cursor_value(next),
            "events": [event_value],
        });
        let payload = serde_json::to_vec(&reply).expect("reply payload");
        write_frame(&mut server, &control_success(request_id, &payload)).expect("reply frame");
    });
    let mut transport = UnixSubscriptionTransport::from_stream(client);
    assert!(
        transport
            .subscribe(SubscriptionRequest::new(previous, MAX_EVENTS).expect("request"))
            .is_err()
    );
    server_thread.join().expect("server");
}

#[cfg(unix)]
#[test]
fn unix_subscription_rejects_digest_only_identity_event() {
    use std::os::unix::net::UnixStream;

    let previous = Cursor::new();
    let next = Cursor::for_view(
        previous.recipe(),
        previous.version(),
        Frontier::new(
            previous.branch(),
            previous.log(),
            previous.schema(),
            previous.root(),
            1,
        ),
    );
    let intent = intent_id("event", b"payload");
    let event = EventDto::new(next, CursorEvent::Intent { id: intent });
    let (mut server, client) = UnixStream::pair().expect("pair");
    let event_value = serde_json::to_value(&event).expect("event");
    let server_thread = std::thread::spawn(move || {
        let body = read_frame(&mut server).expect("request");
        assert_eq!(&body[..4], b"LDC2");
        let request_id = u64::from_be_bytes(body[6..14].try_into().expect("request id"));
        let reply = json!({
            "version": backend_library::protocol_version(),
            "kind": "events",
            "cursor": cursor_value(next),
            "events": [event_value],
        });
        let payload = serde_json::to_vec(&reply).expect("reply payload");
        write_frame(&mut server, &control_success(request_id, &payload)).expect("reply frame");
    });
    let mut transport = UnixSubscriptionTransport::from_stream(client);
    assert!(
        transport
            .subscribe(SubscriptionRequest::new(previous, MAX_EVENTS).expect("request"))
            .is_err()
    );
    server_thread.join().expect("server");
}
