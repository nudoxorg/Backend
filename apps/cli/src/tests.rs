//! CLI adapter tests.
//!
//! Two halves, deliberately separate. The transport half checks that an
//! admitted request and its proof survive the wire unchanged — that contract
//! did not move and neither did its cases. The surface half checks the things
//! this rewrite is responsible for: that every registry row is reachable by
//! the words a person types, that an operand the engine cannot admit fails
//! with the operand named, and that `--format markdown` is the same bytes the
//! MCP text block carries.
#![allow(clippy::expect_used, clippy::panic)]

use super::transport::{read_frame, write_frame};
use super::*;
use backend_library::{
    Basis, COMMANDS, CommandReply, DTO_VERSION, Freshness, Frontier, Query, QueryLimit,
    SurfaceCommand, ViewRoot, ViewSnapshot, WireCertificate, WireClaim, WireSchema, branch_key,
    encode_id, log_key, object_version, package_key, view_key,
};
use backend_present::{FaultSlug, grammar_for};
use std::process::ExitCode;
use backend_replication::{LocalControlLimits, frame as canonical_frame};


struct Fake;
impl LocalEngine for Fake {
    fn execute(&mut self, request: CommandDto) -> ReplyDto {
        ReplyDto::new(request.request_id, CommandReply::Error("fake".to_owned()))
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

fn root() -> ViewRoot {
    let basis_root = backend_library::view_state_root(&[]);
    let basis = Basis::new(basis_root, object_version(b"source"));
    ViewRoot::new_incomplete(
        view_key(b"view"),
        basis,
        Frontier::new(
            branch_key("main"),
            log_key("library"),
            backend_library::canonical::PROTOCOL_SCHEMA,
            basis_root,
            0,
        ),
        Vec::new(),
        Vec::new(),
    )
    .expect("incomplete root")
}
#[test]
fn injected_transport_preserves_identity_and_freshness() {
    let request = CommandDto::new(
        7,
        Command::Search(Query::new(
            "Thing",
            backend_library::view_state_root(&[]),
            QueryLimit::default(),
        )),
    );
    let snapshot = ViewSnapshot {
        root: root(),
        freshness: Freshness::Current,
        next: None,
    };
    let mut transport = Checked {
        reply: Some(ReplyDto::new(7, CommandReply::Search(snapshot))),
    };
    let reply = execute_dto_with_transport(&mut transport, &request).expect("reply");
    assert_eq!(reply.request_id, 7);
}

#[test]
fn mismatched_basis_and_unknown_freshness_are_rejected() {
    let expected = backend_library::view_state_root(&[]);
    let other = backend_library::view_state_root(&[("other".to_owned(), "root".to_owned())]);
    let request = CommandDto::new(
        1,
        Command::Search(Query::new("x", expected, QueryLimit::default())),
    );
    let basis = Basis::new(other, object_version(b"source"));
    let snapshot = ViewSnapshot {
        root: ViewRoot::new_incomplete(
            view_key(b"view"),
            basis,
            Frontier::new(basis.branch, basis.log, basis.schema, other, 0),
            Vec::new(),
            Vec::new(),
        )
        .expect("incomplete root"),
        freshness: Freshness::Unknown,
        next: None,
    };
    let result = admit_reply(&request, ReplyDto::new(1, CommandReply::Search(snapshot)));
    assert!(matches!(
        result,
        Err(ClientError::Protocol(message)) if message.contains("revision")
    ));
}

#[test]
fn replies_must_match_the_requested_command_shape() {
    let request = CommandDto::new(1, Command::Health);
    let snapshot = ViewSnapshot {
        root: root(),
        freshness: Freshness::Current,
        next: None,
    };
    assert!(matches!(
        admit_reply(&request, ReplyDto::new(1, CommandReply::Packages(snapshot))),
        Err(ClientError::Protocol(message)) if message.contains("reply kind")
    ));
}

#[test]
fn health_cannot_be_claimed_without_a_producer_certificate() {
    let request = CommandDto::new(1, Command::Health);
    let result = admit_reply(&request, ReplyDto::new(1, CommandReply::Health(root())));
    assert!(matches!(
        result,
        Err(ClientError::Protocol(message)) if message.contains("certificate")
    ));
}

#[test]
fn compatibility_engine_has_no_unavailable_fallback() {
    let mut fake = Fake;
    let reply = execute(&mut fake, Command::Packages);
    assert_eq!(reply.request_id, 1);
    assert!(run_json(&reply).contains(&format!("\"version\":{DTO_VERSION}")));
}

#[test]
fn cli_frames_reject_truncation_and_oversize() {
    assert!(unframe(&[0, 0, 0]).is_err());
    assert!(frame(&vec![0; MAX_FRAME + 1]).is_err());
    let oversized_length = u32::try_from(MAX_FRAME + 1)
        .expect("test length")
        .to_be_bytes();
    assert!(matches!(
        unframe(&oversized_length),
        Err(ClientError::Transport(_))
    ));
    let request = CommandDto::new(4, Command::Health);
    let encoded = encode_request(&request).expect("request");
    assert_eq!(decode_request(&encoded).expect("decoded request"), request);
    assert!(decode_reply(&encoded).is_err());
}

#[test]
fn cli_outer_frame_matches_the_canonical_local_codec() {
    let body = b"cli-wire";
    let limits = LocalControlLimits {
        max_frame: MAX_FRAME,
        ..LocalControlLimits::default()
    };
    assert_eq!(
        frame(body).expect("cli frame"),
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
fn cli_admission_bounds_requests_and_replies() {
    let oversized = CommandDto::new(
        1,
        Command::Search(Query::new(
            "x".repeat(MAX_TEXT + 1),
            backend_library::view_state_root(&[]),
            QueryLimit::default(),
        )),
    );
    let mut transport = Checked {
        reply: Some(ReplyDto::error(1, "should not execute")),
    };
    assert!(execute_dto_with_transport(&mut transport, &oversized).is_err());

    let mut transport = Checked {
        reply: Some(ReplyDto::error(2, "x".repeat(MAX_FRAME))),
    };
    assert!(
        execute_dto_with_transport(&mut transport, &CommandDto::new(2, Command::Health),).is_err()
    );
    assert!(run_json(&ReplyDto::error(3, "x".repeat(MAX_FRAME))).len() <= MAX_FRAME);
}
#[cfg(all(unix, not(target_os = "macos")))]
#[test]
fn unix_transport_executes_one_correlated_request() {
    use std::os::unix::net::UnixListener;
    let path = std::env::temp_dir().join(format!(
        "backend-cli-test-{}-{}.sock",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    let listener = UnixListener::bind(&path).expect("listener");
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
        write_frame(&mut server, &body).expect("reply frame");
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
    let reply_certificate = WireCertificate::new().with_claim(WireClaim::Intent {
        id: encode_id(intent.as_bytes()),
        token: "request_package".to_owned(),
        payload: package.as_bytes().to_vec().into_boxed_slice(),
    });
    let (mut server, client) = UnixStream::pair().expect("pair");
    let server_thread = std::thread::spawn(move || {
        let body = read_frame(&mut server).expect("request body");
        let request: CommandDto = serde_json::from_slice(&body).expect("certified request");
        assert!(matches!(request.command, Command::Add { .. }));
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
        CommandDto::new(15, Command::Add { package }).with_certificate(request_certificate);
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
        16,
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

// ---------------------------------------------------------------------------
// surface: the argument grammar, the lowering, and the renderings
// ---------------------------------------------------------------------------

fn words(line: &str) -> Vec<String> {
    line.split_whitespace().map(ToOwned::to_owned).collect()
}

fn plain() -> Options {
    Options::fallback()
}

#[test]
fn every_registry_row_is_reachable_by_the_words_a_person_types() {
    for spec in COMMANDS {
        let grammar = grammar_for(spec.name)
            .unwrap_or_else(|| panic!("`{}` has no CLI grammar", spec.name));
        assert_eq!(grammar.name(), spec.name);
        let help = invoke::help();
        assert!(
            help.contains(spec.name),
            "`{}` is missing from the generated help",
            spec.name
        );
    }
    assert!(invoke::registry_is_covered());
}

#[test]
fn help_is_grouped_by_domain_and_names_every_domain() {
    let help = invoke::help();
    for domain in invoke::help_domains() {
        assert!(
            help.contains(backend_present::domain_name(domain)),
            "help omits the {domain:?} domain"
        );
    }
    assert!(help.contains("surface <JSON>"), "the escape hatch stays");
    assert!(help.contains("--format human|markdown|json"));
}

#[test]
fn an_unknown_command_names_the_nearest_one_it_knows() {
    let fault = invoke::parse(&words("serch ferris"), None).expect_err("unknown verb");
    assert_eq!(fault.slug(), FaultSlug::Usage);
    assert_eq!(fault.operand().render(), "serch");
    assert!(
        fault.cause().sentence().contains("did you mean `search`"),
        "{}",
        fault.cause().sentence()
    );
}

#[test]
fn an_unknown_option_names_the_options_the_command_takes() {
    let fault = invoke::parse(&words("search ferris --deep"), None).expect_err("unknown option");
    assert_eq!(fault.operand().render(), "--deep");
    assert!(fault.cause().sentence().contains("--limit"));
}

#[test]
fn a_missing_operand_prints_the_exact_usage_line() {
    let fault = invoke::parse(&words("show"), None).expect_err("missing coordinate");
    assert_eq!(fault.slug(), FaultSlug::Usage);
    assert!(
        fault.cause().sentence().contains("backend show <COORDINATE>"),
        "{}",
        fault.cause().sentence()
    );
}

#[test]
fn a_page_bound_outside_its_range_is_refused_with_the_value() {
    let invocation = invoke::parse(&words("search ferris --limit 900"), None).expect("parse");
    let fault = lower(&invocation, "/abs/project").expect_err("out of range");
    assert_eq!(fault.operand().render(), "limit");
    assert!(fault.cause().sentence().contains("`900`"));
}

#[test]
fn the_global_limit_reaches_the_commands_that_page() {
    let invocation = invoke::parse(&words("search ferris"), Some("3")).expect("parse");
    let Request::Search { limit, .. } = lower(&invocation, "/abs/project").expect("lower") else {
        panic!("search lowers to a search request");
    };
    assert_eq!(limit, 3);
}

#[test]
fn typed_rows_lower_without_the_json_escape_hatch() {
    type Shape = fn(&Request) -> bool;
    let cases: [(&str, Shape); 6] = [
        ("packages", |request| matches!(request, Request::Shelf)),
        ("health", |request| matches!(request, Request::Status)),
        ("show /p::src/lib.rs:1::f", |request| {
            matches!(request, Request::Page(_))
        }),
        ("outline /p", |request| matches!(request, Request::Outline(_))),
        ("related /p::src/lib.rs:1::f", |request| {
            matches!(request, Request::Neighbourhood { incoming: true, .. })
        }),
        ("resolve ferris", |request| {
            matches!(request, Request::Resolve { .. })
        }),
    ];
    for (line, matches) in cases {
        let invocation = invoke::parse(&words(line), None).expect("parse");
        let request = lower(&invocation, "/abs/project").expect("lower");
        assert!(matches(&request), "`{line}` lowered to {request:?}");
    }
}

#[test]
fn every_durable_row_lowers_into_the_one_surface_contract() {
    let cases = [
        "read one two",
        "diff pkg:cargo/a@1.0.0 pkg:cargo/a@2.0.0",
        "explore serde",
        "package pkg:cargo/a@1.0.0",
        "dependents pkg:cargo/a@1.0.0",
        "owner dtolnay",
        "index-search serde",
        "package-versions pkg:cargo/a@1.0.0",
        "semantic-versions pkg:cargo/a@1.0.0",
        "package-profile pkg:cargo/a@1.0.0",
        "subscribe pkg:cargo/a@1.0.0",
        "unsubscribe pkg:cargo/a@1.0.0",
        "subscriptions",
        "releases",
        "projects",
        "project-create work",
        "project-delete work",
        "project-add work pkg:cargo/a@1.0.0",
        "project-remove work pkg:cargo/a@1.0.0",
        "project-sync work",
        "tree",
        "tree-open search ferris",
        "tree-close 1",
    ];
    for line in cases {
        let invocation = invoke::parse(&words(line), None).expect("parse");
        let request = lower(&invocation, "/abs/project")
            .unwrap_or_else(|fault| panic!("`{line}`: {}", fault.cause().sentence()));
        assert!(
            matches!(request, Request::Surface(_)),
            "`{line}` did not lower into the surface contract"
        );
    }
}

#[test]
fn a_malformed_package_reference_names_the_operand_it_refused() {
    let invocation = invoke::parse(&words("package pkg:not-a-purl"), None).expect("parse");
    let fault = lower(&invocation, "/abs/project").expect_err("bad purl");
    assert_eq!(fault.operand().render(), "pkg:not-a-purl");
    assert_eq!(fault.slug(), FaultSlug::Usage);
}

#[test]
fn an_unknown_semantic_profile_lists_the_closed_set() {
    let line = "select-semantic-version pkg:cargo/a@1.0.0 pkg:cargo/a@1.0.0 kotlin \
                0000000000000000000000000000000000000000000000000000000000000000";
    let invocation = invoke::parse(&words(line), None).expect("parse");
    let fault = lower(&invocation, "/abs/project").expect_err("unknown profile");
    assert_eq!(fault.operand().render(), "profile");
    for expected in ["rust", "typescript", "python", "go", "java", "csharp", "c", "cpp"] {
        assert!(
            fault.cause().sentence().contains(expected),
            "the closed profile set omits {expected}"
        );
    }
}

#[test]
fn the_json_escape_hatch_still_reaches_the_whole_surface() {
    let encoded = serde_json::to_string(&SurfaceCommand::Subscriptions).expect("encode");
    let request = lower_surface_json(&encoded).expect("escape hatch");
    assert!(matches!(request, Request::Surface(_)));
    let fault = lower_surface_json("{\"operation\":\"nope\"}").expect_err("unknown operation");
    assert_eq!(fault.slug(), FaultSlug::Usage);
}

#[test]
fn a_fault_chooses_the_exit_code_a_script_can_branch_on() {
    let usage = invoke::parse(&words("show"), None).expect_err("usage");
    assert_eq!(render::exit_code(&usage), ExitCode::from(64));
    let refused = Fault::from_command_failure(
        &backend_library::CommandFailure::NotFound,
        backend_present::Operand::Whole,
    );
    assert_eq!(render::exit_code(&refused), ExitCode::from(2));
    let endpoint = Fault::from_client_error(
        &ClientError::Io("no such file".to_owned()),
        backend_present::Operand::Path("/tmp/x".to_owned()),
    );
    assert_eq!(render::exit_code(&endpoint), ExitCode::from(1));
}

#[test]
fn a_rendered_fault_carries_the_operand_and_the_next_command() {
    let fault = Fault::from_command_failure(
        &backend_library::CommandFailure::NotFound,
        backend_present::Operand::Coordinate(backend_present::Coordinate::new(
            "/abs/p::src/lib.rs:999::nothing",
        )),
    )
    .with_affordance(backend_present::Affordance::search("nothing"));
    let rendered = render::fault(&fault, &plain());
    assert!(rendered.starts_with("✗ not-found /abs/p::src/lib.rs:999::nothing"));
    assert!(rendered.ends_with("→ backend search nothing"));
}

#[test]
fn markdown_output_is_the_shared_renderer_and_json_is_the_typed_dto() {
    let snapshot = ViewSnapshot {
        root: root(),
        freshness: Freshness::Current,
        next: None,
    };
    let list = backend_present::record_list("ferris", &snapshot);
    let answer = Answer::Records(Box::new(list.clone()));
    assert_eq!(
        render::markdown_text(&answer),
        backend_present::markdown::records(&list, None),
        "the CLI markdown rendering must be the shared one"
    );
    let json = render::json(&answer);
    let value: serde_json::Value = serde_json::from_str(&json).expect("typed DTO");
    assert_eq!(value["kind"], "records");
    assert_eq!(value["query"], "ferris");
    assert!(value["coverage"].is_array());
    assert!(
        value.get("certificate").is_none(),
        "the product JSON must not leak wire proof"
    );
}
