//! MCP adapter tests.
#![allow(clippy::expect_used)]

use super::transport::{read_frame, write_frame};
use super::*;
use backend_library::{
    Basis, CommandReply, Freshness, Frontier, Query, QueryLimit, ViewRoot, ViewSnapshot,
    WireCertificate, WireClaim, WireSchema, encode_id, intent_id, object_version, package_key,
    symbol_key, view_key, view_state_root,
};
use backend_replication::{LocalControlLimits, frame as canonical_frame};

struct Fake {
    calls: usize,
    reply: Option<ReplyDto>,
}

impl LocalEngine for Fake {
    fn execute(&mut self, request: CommandDto) -> ReplyDto {
        self.calls += 1;
        self.reply
            .take()
            .unwrap_or_else(|| ReplyDto::error(request.request_id, "empty fake"))
    }
}

struct Checked {
    reply: Option<ReplyDto>,
}

impl CommandTransport for Checked {
    fn request(&mut self, request: CommandDto) -> Result<ReplyDto, ClientError> {
        admit_reply(&request, self.reply.take().expect("reply"))
    }
}

impl CertifiedCommandTransport for Checked {
    fn request_with_certificate(
        &mut self,
        request: CommandDto,
        _capability: Option<CoverageCapability>,
    ) -> Result<ReplyDto, ClientError> {
        self.request(request)
    }
}

#[test]
fn in_process_client_does_not_cache_replies() {
    let mut client = Client::new(Fake {
        calls: 0,
        reply: Some(ReplyDto::error(77, "one")),
    });
    let intent = intent_id("intent", b"payload");
    assert_eq!(
        client.call_intent(intent, 77, Command::Health).request_id,
        77
    );
    assert_eq!(client.into_inner().calls, 1);
}

#[test]
fn embedded_client_rejects_an_intent_for_another_package_before_execution() {
    let package = package_key("pkg");
    let wrong = intent_id("request_package", b"other");
    let mut client = Client::new(Fake {
        calls: 0,
        reply: Some(ReplyDto::new(
            78,
            CommandReply::Added(backend_library::Intent::request_package(package).id()),
        )),
    });
    let reply = client.call_intent(wrong, 78, Command::Add { package });
    assert!(
        matches!(reply.reply, CommandReply::Error(message) if message.contains("does not match"))
    );
    assert_eq!(client.into_inner().calls, 0);
}

#[test]
fn frames_are_bounded_and_round_trip() {
    let body = b"{}";
    assert_eq!(
        unframe(&frame(body).expect("frame")).expect("unframe"),
        body
    );
    assert!(frame(&vec![0; MAX_FRAME + 1]).is_err());
    assert!(unframe(&[0, 0, 0, 2, b'{']).is_err());
    assert!(unframe(&[0, 0, 0, 1, b'{', b'}']).is_err());
}

#[test]
fn mcp_outer_frame_matches_the_canonical_local_codec() {
    let body = b"mcp-wire";
    let limits = LocalControlLimits {
        max_frame: MAX_FRAME,
        ..LocalControlLimits::default()
    };
    assert_eq!(
        frame(body).expect("mcp frame"),
        canonical_frame(body, limits).expect("canonical frame")
    );
    assert!(unframe(&[0, 0, 0, 1, b'x', b'y']).is_err());
}

#[test]
fn request_encoding_rejects_a_certificate_for_another_package() {
    let package = package_key("pkg");
    let request = CommandDto::new(8, Command::Add { package }).with_certificate(
        WireCertificate::new().with_claim(WireClaim::Key {
            schema: WireSchema::Package,
            id: encode_id(package.as_bytes()),
            value: "other".to_owned(),
        }),
    );
    assert!(encode_request(&request).is_err());
}

#[test]
fn injected_transport_checks_basis_and_freshness() {
    let request = CommandDto::new(
        9,
        Command::Search(Query::new(
            "Thing",
            view_state_root(&[]),
            QueryLimit::default(),
        )),
    );
    let basis = Basis::new(view_state_root(&[]), object_version(b"source"));
    let snapshot = ViewSnapshot {
        root: ViewRoot::new_incomplete(
            view_key(b"view"),
            basis,
            Frontier::new(basis.branch, basis.log, basis.schema, basis.root, 0),
            Vec::new(),
            Vec::new(),
        )
        .expect("incomplete root"),
        freshness: Freshness::Current,
        next: None,
        graph_relations: None,
        rich_graph: None,
    };
    let mut transport = Checked {
        reply: Some(ReplyDto::new(9, CommandReply::Search(snapshot))),
    };
    let reply = transport.request(request).expect("reply");
    assert_eq!(reply.request_id, 9);
}

#[test]
fn graph_requests_retain_their_revision_for_endpoint_admission() {
    let basis = view_state_root(&[]);
    let command = Command::Graph(backend_library::GraphNeighborhoodQuery::new(
        symbol_key("pkg::Thing"),
        basis,
    ));
    assert_eq!(client::request_basis(&command), Some(basis.into()));
}

#[test]
fn replies_must_match_the_requested_command_shape() {
    let request = CommandDto::new(1, Command::Health);
    let basis = Basis::new(view_state_root(&[]), object_version(b"source"));
    let snapshot = ViewSnapshot {
        root: ViewRoot::new_incomplete(
            view_key(b"view"),
            basis,
            Frontier::new(basis.branch, basis.log, basis.schema, basis.root, 0),
            Vec::new(),
            Vec::new(),
        )
        .expect("incomplete root"),
        freshness: Freshness::Current,
        next: None,
        graph_relations: None,
        rich_graph: None,
    };
    assert!(matches!(
        admit_reply(&request, ReplyDto::new(1, CommandReply::Packages(snapshot))),
        Err(ClientError::Protocol(message)) if message.contains("reply kind")
    ));
}

#[test]
fn health_cannot_be_claimed_without_a_producer_certificate() {
    let request = CommandDto::new(1, Command::Health);
    let result = admit_reply(
        &request,
        ReplyDto::new(
            1,
            CommandReply::Health(
                ViewRoot::new_incomplete(
                    view_key(b"health"),
                    Basis::new(view_state_root(&[]), object_version(b"source")),
                    Frontier::new(
                        backend_library::branch_key("main"),
                        backend_library::log_key("library"),
                        backend_library::canonical::PROTOCOL_SCHEMA,
                        view_state_root(&[]),
                        0,
                    ),
                    Vec::new(),
                    Vec::new(),
                )
                .expect("incomplete health root"),
            ),
        ),
    );
    assert!(matches!(
        result,
        Err(ClientError::Protocol(message)) if message.contains("certificate")
    ));
}

#[test]
fn malformed_request_gets_a_framed_reply() {
    let mut fake = Checked { reply: None };
    let response = serve_frame(&[0, 0, 0, 1, b'{'], &mut fake);
    assert!(unframe(&response).is_ok());
    let body = unframe(&response).expect("body");
    let reply: ReplyDto = serde_json::from_slice(body).expect("reply");
    assert_eq!(reply.request_id, 0);
    assert!(matches!(reply.reply, CommandReply::Error(_)));
}

#[test]
fn endpoint_error_keeps_a_valid_request_id() {
    let request = CommandDto::new(41, Command::Health);
    let body = serde_json::to_vec(&request).expect("request");
    let input = frame(&body).expect("frame");
    let response = error_frame_for_input(&input, "offline");
    let reply: ReplyDto = serde_json::from_slice(unframe(&response).expect("body")).expect("reply");
    assert_eq!(reply.request_id, 41);
}

#[test]
fn endpoint_error_extracts_correlation_without_admitting_claims() {
    let body = br#"{"version":1,"request_id":55,"command":{"kind":"unknown"}}"#;
    let input = frame(body).expect("frame");
    let response = error_frame_for_input(&input, "offline");
    let reply = decode_reply(&response).expect("reply");
    assert_eq!(reply.request_id, 55);
}

#[test]
fn framed_dispatch_preserves_cli_reply_shape() {
    let request = CommandDto::new(3, Command::Health);
    let body = serde_json::to_vec(&request).expect("request");
    let input = frame(&body).expect("frame");
    let mut fake = Checked {
        reply: Some(ReplyDto::new(3, CommandReply::Error("ok".to_owned()))),
    };
    let output = dispatch_frame_with_transport(&mut fake, &input).expect("dispatch");
    let decoded = decode_reply(&output).expect("reply");
    assert_eq!(decoded.request_id, 3);
    assert!(matches!(decoded.reply, CommandReply::Error(_)));
}

#[test]
fn certified_framed_dispatch_admits_identity_before_presentation() {
    let package = package_key("pkg");
    let intent = backend_library::Intent::request_package(package).id();
    let request = CommandDto::new(5, Command::Add { package }).with_certificate(
        WireCertificate::new().with_claim(WireClaim::Key {
            schema: WireSchema::Package,
            id: encode_id(package.as_bytes()),
            value: "pkg".to_owned(),
        }),
    );
    let request_frame = frame(&serde_json::to_vec(&request).expect("request")).expect("frame");
    let reply = ReplyDto::new(5, CommandReply::Added(intent)).with_certificate(
        WireCertificate::new().with_claim(WireClaim::Intent {
            id: encode_id(intent.as_bytes()),
            token: "request_package".to_owned(),
            payload: package.as_bytes().to_vec().into_boxed_slice(),
        }),
    );
    let mut transport = Checked { reply: Some(reply) };
    let output =
        dispatch_frame_with_transport_with_certificate(&mut transport, &request_frame, None)
            .expect("certified dispatch");
    let decoded = decode_reply_with_certificate(&output, None).expect("certified reply");
    assert_eq!(decoded.reply, CommandReply::Added(intent));
}

#[test]
fn mcp_admission_bounds_requests_and_replies() {
    let oversized = CommandDto::new(
        1,
        Command::Search(Query::new(
            "x".repeat(MAX_TEXT + 1),
            view_state_root(&[]),
            QueryLimit::default(),
        )),
    );
    let mut engine = Fake {
        calls: 0,
        reply: Some(ReplyDto::error(1, "should not execute")),
    };
    let mut transport = InProcessTransport::new(&mut engine);
    assert!(transport.request(oversized).is_err());
    let mut transport = Checked {
        reply: Some(ReplyDto::error(2, "x".repeat(MAX_FRAME))),
    };
    assert!(
        transport
            .request(CommandDto::new(2, Command::Health))
            .is_err()
    );
}

#[cfg(all(unix, not(target_os = "macos")))]
#[test]
fn unix_transport_executes_one_correlated_request() {
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::UnixListener;
    let path = std::env::temp_dir().join(format!(
        "backend-mcp-test-{}-{}.sock",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    let listener = UnixListener::bind(&path).expect("listener");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
        .expect("socket permissions");
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
        .expect("socket permissions");
    let (accepted_tx, accepted_rx) = std::sync::mpsc::channel();
    let server_thread = std::thread::spawn(move || {
        let (mut server, _) = listener.accept().expect("accept");
        accepted_tx.send(()).expect("accepted signal");
        let body = read_frame(&mut server).expect("request body");
        let request: CommandDto = serde_json::from_slice(&body).expect("request");
        let reply = ReplyDto::error(request.request_id, "offline");
        let body = serde_json::to_vec(&reply).expect("reply");
        write_frame(&mut server, &body).expect("reply write");
    });
    let client_stream = std::os::unix::net::UnixStream::connect(&path).expect("connect probe");
    accepted_rx.recv().expect("accepted");
    let mut transport =
        UnixCommandTransport::from_authenticated_stream(client_stream, &path).expect("connect");
    let reply = transport
        .request(CommandDto::new(12, Command::Health))
        .expect("transport reply");
    assert_eq!(reply.request_id, 12);
    assert!(matches!(reply.reply, CommandReply::Error(_)));
    server_thread.join().expect("server");
    std::fs::remove_file(path).expect("remove socket");
}

#[cfg(unix)]
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one case per forged-identity variant keeps the proof readable"
)]
fn unix_transport_consumes_producer_certified_success_without_expected_cache() {
    use std::os::unix::net::UnixStream;

    let package = package_key("pkg");
    let request_certificate = WireCertificate::new().with_claim(WireClaim::Key {
        schema: WireSchema::Package,
        id: encode_id(package.as_bytes()),
        value: "pkg".to_owned(),
    });
    let intent = backend_library::Intent::request_package(package).id();
    let (mut server, client) = UnixStream::pair().expect("pair");
    let server_thread = std::thread::spawn(move || {
        let body = read_frame(&mut server).expect("request body");
        let request: CommandDto = serde_json::from_slice(&body).expect("certified request");
        assert!(matches!(request.command, Command::Add { .. }));
        let reply_certificate = WireCertificate::new().with_claim(WireClaim::Intent {
            id: encode_id(intent.as_bytes()),
            token: "request_package".to_owned(),
            payload: package_key("pkg").as_bytes().to_vec().into_boxed_slice(),
        });
        let reply = ReplyDto::new(request.request_id, CommandReply::Added(intent))
            .with_certificate(reply_certificate);
        write_frame(
            &mut server,
            &serde_json::to_vec(&reply).expect("reply body"),
        )
        .expect("reply frame");
    });
    let mut transport = UnixCommandTransport::from_stream(client);
    let request =
        CommandDto::new(17, Command::Add { package }).with_certificate(request_certificate);
    let reply = transport
        .request_with_certificate(request, None)
        .expect("certified success");
    assert_eq!(reply.reply, CommandReply::Added(intent));
    server_thread.join().expect("server");

    let (mut server, client) = UnixStream::pair().expect("pair");
    let server_thread = std::thread::spawn(move || {
        let body = read_frame(&mut server).expect("request body");
        let request: CommandDto = serde_json::from_slice(&body).expect("certified request");
        let reply_certificate = WireCertificate::new().with_claim(WireClaim::Intent {
            id: encode_id(intent.as_bytes()),
            token: "request_package".to_owned(),
            payload: package_key("pkg").as_bytes().to_vec().into_boxed_slice(),
        });
        let reply = ReplyDto::new(request.request_id, CommandReply::Added(intent))
            .with_certificate(reply_certificate);
        let mut value = serde_json::to_value(&reply).expect("reply value");
        let forged = backend_library::Intent::remove_package(package_key("pkg")).id();
        value["reply"]["data"]["id"] = serde_json::json!(encode_id(forged.as_bytes()));
        value["certificate"]["claims"][0]["data"]["id"] =
            serde_json::json!(encode_id(forged.as_bytes()));
        value["certificate"]["claims"][0]["data"]["token"] = serde_json::json!("remove_package");
        write_frame(
            &mut server,
            &serde_json::to_vec(&value).expect("forged reply body"),
        )
        .expect("reply frame");
    });
    let mut transport = UnixCommandTransport::from_stream(client);
    let request = CommandDto::new(
        18,
        Command::Add {
            package: package_key("pkg"),
        },
    )
    .with_certificate(WireCertificate::new().with_claim(WireClaim::Key {
        schema: WireSchema::Package,
        id: encode_id(package_key("pkg").as_bytes()),
        value: "pkg".to_owned(),
    }));
    assert!(transport.request(request).is_err());
    server_thread.join().expect("server");
}

#[cfg(unix)]
#[test]
fn unix_transport_rejects_digest_only_identity_success() {
    use std::os::unix::net::UnixStream;

    let package = package_key("pkg");
    let (mut server, client) = UnixStream::pair().expect("pair");
    let server_thread = std::thread::spawn(move || {
        let body = read_frame(&mut server).expect("request body");
        let request: CommandDto = serde_json::from_slice(&body).expect("request");
        let intent = backend_library::Intent::request_package(package).id();
        let reply = ReplyDto::new(request.request_id, CommandReply::Added(intent));
        write_frame(
            &mut server,
            &serde_json::to_vec(&reply).expect("digest-only reply body"),
        )
        .expect("reply frame");
    });
    let request = CommandDto::new(19, Command::Add { package }).with_certificate(
        WireCertificate::new().with_claim(WireClaim::Key {
            schema: WireSchema::Package,
            id: encode_id(package_key("pkg").as_bytes()),
            value: "pkg".to_owned(),
        }),
    );
    let mut transport = UnixCommandTransport::from_stream(client);
    assert!(transport.request(request).is_err());
    server_thread.join().expect("server");
}

#[cfg(unix)]
#[test]
fn unix_endpoint_path_is_bounded() {
    assert!(matches!(
        UnixCommandTransport::connect("x".repeat(MAX_ENDPOINT_PATH + 1)),
        Err(ClientError::Transport(_))
    ));
}
