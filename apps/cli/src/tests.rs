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
use backend_replication::{LocalControlLimits, frame as canonical_frame};
use std::process::ExitCode;

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
    let produced = backend_library::Library::new().execute_dto(request.clone());
    let mut transport = Checked {
        reply: Some(produced),
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
        graph_relations: None,
        rich_graph: None,
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
    let request = CommandDto::new(
        8,
        Command::Add {
            package,
            execution_intent: Default::default(),
        },
    )
    .with_certificate(WireCertificate::new().with_claim(WireClaim::Key {
        schema: WireSchema::Package,
        id: encode_id(package.as_bytes()),
        value: "other".to_owned(),
    }));
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
/// Local-endpoint fixtures that behave identically on Unix sockets and on the
/// Windows `AF_UNIX` sockets `backend_platform::local` provides, so no transport
/// test needs a platform branch of its own.
///
/// A connected pair is an unnamed socketpair on Unix, which has no `sun_path`
/// to overrun under a deep temporary directory. A named endpoint is only
/// needed by the tests that dial one by path, and those do not run on macOS.
#[cfg(any(unix, windows))]
mod endpoint {
    #[cfg(not(target_os = "macos"))]
    use backend_platform::local::LocalListener;
    use backend_platform::local::LocalStream;
    #[cfg(not(target_os = "macos"))]
    use std::path::{Path, PathBuf};
    #[cfg(not(target_os = "macos"))]
    use std::sync::atomic::{AtomicU64, Ordering};

    #[cfg(not(target_os = "macos"))]
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);

    /// A path no other test in this process, and no earlier process, is using.
    #[cfg(not(target_os = "macos"))]
    pub(super) fn scratch(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "bcli-{label}-{}-{}.sock",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ))
    }

    /// Binds an endpoint whose permissions satisfy peer authentication: the
    /// current user alone may use it.
    #[cfg(not(target_os = "macos"))]
    pub(super) fn bind_private(path: &Path) -> LocalListener {
        let listener = LocalListener::bind(path).expect("bind local endpoint");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
                .expect("restrict endpoint to its owner");
        }
        #[cfg(windows)]
        backend_platform::win32::security::restrict_to_current_user(path)
            .expect("restrict endpoint to its owner");
        listener
    }

    /// A connected `(server, client)` pair that has not been authenticated.
    #[cfg(unix)]
    pub(super) fn pair() -> (LocalStream, LocalStream) {
        LocalStream::pair().expect("create a socketpair")
    }

    /// A connected `(server, client)` pair that has not been authenticated.
    #[cfg(windows)]
    pub(super) fn pair() -> (LocalStream, LocalStream) {
        let path = scratch("pair");
        let listener = LocalListener::bind(&path).expect("bind pair endpoint");
        let client = LocalStream::connect(&path).expect("connect pair endpoint");
        let (server, _) = listener.accept().expect("accept pair endpoint");
        drop(listener);
        let _ = std::fs::remove_file(&path);
        (server, client)
    }
}

#[cfg(all(any(unix, windows), not(target_os = "macos")))]
#[test]
fn unix_transport_executes_one_correlated_request() {
    let path = endpoint::scratch("correlated");
    let listener = endpoint::bind_private(&path);
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
    // `connect` opens the endpoint and authenticates its owner as one step.
    let mut transport = UnixCommandTransport::connect(&path).expect("connect");
    accepted_rx.recv().expect("accepted");
    let reply = transport
        .request(CommandDto::new(12, Command::Health))
        .expect("transport reply");
    assert_eq!(reply.request_id, 12);
    assert!(matches!(reply.reply, CommandReply::Error(_)));
    server_thread.join().expect("server");
    std::fs::remove_file(path).expect("remove socket");
}

#[cfg(any(unix, windows))]
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one case per forged-identity variant keeps the proof readable"
)]
fn unix_transport_consumes_producer_certified_success_without_expected_cache() {
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
    let (mut server, client) = endpoint::pair();
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
    let request = CommandDto::new(
        15,
        Command::Add {
            package,
            execution_intent: Default::default(),
        },
    )
    .with_certificate(request_certificate);
    let reply = transport
        .request_with_certificate(request, None)
        .expect("certified success");
    assert_eq!(reply.reply, CommandReply::Added(intent));
    server_thread.join().expect("server");

    let (mut server, client) = endpoint::pair();
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
            execution_intent: Default::default(),
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

#[cfg(any(unix, windows))]
#[test]
fn unix_transport_rejects_digest_only_identity_success() {
    let package = package_key("pkg");
    let (mut server, client) = endpoint::pair();
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
    let request = CommandDto::new(
        19,
        Command::Add {
            package,
            execution_intent: Default::default(),
        },
    )
    .with_certificate(WireCertificate::new().with_claim(WireClaim::Key {
        schema: WireSchema::Package,
        id: encode_id(package_key("pkg").as_bytes()),
        value: "pkg".to_owned(),
    }));
    let mut transport = UnixCommandTransport::from_stream(client);
    assert!(transport.request(request).is_err());
    server_thread.join().expect("server");
}

#[cfg(any(unix, windows))]
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
        let grammar =
            grammar_for(spec.name).unwrap_or_else(|| panic!("`{}` has no CLI grammar", spec.name));
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
    assert!(help.contains("--passive"));
    assert!(help.contains("nudox add ."));
    assert!(help.contains("nudox search \"error handling\""));
    assert!(help.contains("claude mcp add --scope user --transport stdio nudox"));
    assert!(help.contains("--project '${CLAUDE_PROJECT_DIR:-.}'"));
}

#[test]
fn a_global_page_bound_is_not_silently_ignored_by_unpaged_commands() {
    for command in [
        "packages --limit 0",
        "packages --limit 20",
        "health --limit 5",
    ] {
        let fault = options::split(&words(command)).expect_err("reject unused global page bound");
        assert_eq!(fault.slug(), FaultSlug::Usage);
        assert_eq!(fault.operand().render(), "--limit");
        assert!(
            fault
                .cause()
                .sentence()
                .contains("does not take a page bound")
        );
    }
    let (options, _) = options::split(&words("search requests --limit 3"))
        .expect("paged commands keep their global bound");
    assert_eq!(options.limit(), Some("3"));
}

#[test]
fn passive_connection_mode_is_limited_to_health_and_status() {
    let (options, command_words) =
        options::split(&words("--passive status")).expect("status accepts a passive connection");
    assert!(options.passive());
    assert_eq!(command_words, vec!["status".to_owned()]);

    let error = options::split(&words("--passive search symbols"))
        .expect_err("passive mode must not alter ordinary commands");
    assert_eq!(error.slug(), FaultSlug::Usage);
    assert_eq!(error.operand().render(), "--passive");
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
        fault
            .cause()
            .sentence()
            .contains("backend show <COORDINATE>"),
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
fn add_defaults_to_interactive_and_advertises_the_background_option() {
    let invocation = invoke::parse(&words("add /abs/project"), None).expect("parse Add");
    let Request::Index(path) = lower(&invocation, "/unused").expect("lower Add") else {
        panic!("default add lowers to the interactive index request");
    };
    assert_eq!(path, "/abs/project");

    let help = invoke::help_for(grammar_for("add").expect("Add grammar"));
    assert!(help.contains("use `.` for the current directory"));
    assert!(help.contains("--execution-intent INTENT"));
    assert!(help.contains("bounded remote calibration"));
}

#[test]
fn installed_quick_start_words_lower_to_the_same_add_and_search_requests() {
    let add = invoke::parse(&words("add ."), None).expect("parse the quick-start add");
    assert!(matches!(
        lower(&add, "/abs/project").expect("lower add"),
        Request::Index(path) if path == "."
    ));

    let index_alias = invoke::parse(&words("index ."), None).expect("parse the index alias");
    assert!(matches!(
        lower(&index_alias, "/abs/project").expect("lower index alias"),
        Request::Index(path) if path == "."
    ));

    let search = invoke::parse(&["search".to_owned(), "error handling".to_owned()], None)
        .expect("parse the positional search query");
    assert!(matches!(
        lower(&search, "/abs/project").expect("lower search"),
        Request::Search { text, .. } if text == "error handling"
    ));
}

#[test]
fn add_accepts_background_and_rejects_unknown_execution_intents() {
    let invocation = invoke::parse(
        &words("add /abs/project --execution-intent background"),
        None,
    )
    .expect("parse background Add");
    let Request::IndexWithExecutionIntent {
        execution_intent, ..
    } = lower(&invocation, "/unused").expect("lower background Add")
    else {
        panic!("add lowers to an index request");
    };
    assert_eq!(
        execution_intent,
        backend_library::CompileExecutionIntent::Background
    );

    let invocation = invoke::parse(&words("add /abs/project --execution-intent remote"), None)
        .expect("the grammar parses the option value before semantic validation");
    let fault = lower(&invocation, "/unused").expect_err("unknown intent");
    assert_eq!(fault.operand().render(), "execution-intent");
    assert!(
        fault
            .cause()
            .sentence()
            .contains("choose interactive or background")
    );
}

#[test]
fn owner_index_job_commands_accept_exact_cli_ticket_operands() {
    let ticket = backend_library::IndexJobTicket::new(
        std::num::NonZeroU64::new(29).expect("nonzero job id"),
        [0x2a; 16],
        backend_library::PackageReference::parse("pkg:cargo/serde@1.0.228")
            .expect("pinned package"),
    );
    let ticket_json = serde_json::to_string(&ticket).expect("exact owner ticket JSON");

    let start = invoke::parse(
        &words("index_start pkg:cargo/serde@1.0.228 --execution-intent background"),
        None,
    )
    .expect("parse index_start");
    assert!(matches!(
        lower(&start, "/unused"),
        Ok(Request::Surface(command))
            if matches!(*command,
                SurfaceCommand::IndexStart {
                    package: backend_library::PackageReference::Purl(ref package),
                    execution_intent: backend_library::CompileExecutionIntent::Background,
                } if package.as_str() == "pkg:cargo/serde@1.0.228")
    ));

    for (line, expected_id) in [
        (
            format!("index_progress {ticket_json} --after-sequence 17"),
            17,
        ),
        (format!("index_await {ticket_json}"), 0),
        (format!("index_cancel {ticket_json}"), 0),
    ] {
        let invocation = invoke::parse(&words(&line), None).expect("parse exact ticket operand");
        let request = lower(&invocation, "/unused").expect("lower exact ticket operand");
        match (invocation.grammar().name(), request) {
            ("index_progress", Request::Surface(command)) => assert!(matches!(
                *command,
                SurfaceCommand::IndexProgress {
                    ticket: ref observed,
                    after_sequence,
                } if observed == &ticket && after_sequence == expected_id
            )),
            ("index_await", Request::Surface(command)) => assert!(matches!(
                *command,
                SurfaceCommand::IndexAwait { ticket: ref observed }
                    if observed == &ticket
            )),
            ("index_cancel", Request::Surface(command)) => assert!(matches!(
                *command,
                SurfaceCommand::IndexCancel { ticket: ref observed }
                    if observed == &ticket
            )),
            (name, _) => panic!("{name} did not lower to its typed job surface command"),
        }
    }

    let help = invoke::help_for(grammar_for("index_progress").expect("progress grammar"));
    assert!(help.contains("<TICKET>"));
    assert!(help.contains("quote it as one shell argument"));
    assert!(help.contains("--after-sequence SEQUENCE"));
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
        ("outline /p", |request| {
            matches!(request, Request::Outline(_))
        }),
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

// Authenticate only this closed projection fixture. This capability is not a
// compiler fact or a semantic-shape certificate.
fn fixture_projection_capability(basis: Basis) -> backend_library::CoverageCapability {
    use backend_library::{
        AuthorityScopeClaim, CoverageCapability, ProducerObservationClaims,
        ProducerObservationVerifier, ScopeRoot, UntrustedProducerObservation, admit_complete_scope,
        admit_producer_observation,
    };
    struct ProjectionVerifier;
    impl ProducerObservationVerifier for ProjectionVerifier {
        type Error = &'static str;
        fn verify(
            &self,
            observation: &UntrustedProducerObservation,
        ) -> Result<ProducerObservationClaims, Self::Error> {
            if observation.producer_identity() != [7; 32]
                || observation.context() != [8; 32]
                || observation.evidence() != [9; 32]
            {
                return Err("foreign test projection producer");
            }
            Ok(ProducerObservationClaims::new(
                observation.producer_identity(),
                observation.scope_root(),
                observation.context(),
                *blake3::hash(observation.evidence()).as_bytes(),
            ))
        }
    }
    let observation = admit_producer_observation(
        UntrustedProducerObservation::new(
            [7; 32],
            ScopeRoot::from_bytes(basis.object.to_bytes()),
            [8; 32],
            vec![9; 32],
        ),
        &ProjectionVerifier,
    )
    .expect("closed projection observation");
    CoverageCapability::from_authorized_with_evidence(
        admit_complete_scope(
            AuthorityScopeClaim::from_object_version(basis.object),
            observation,
        )
        .expect("projection source scope"),
        vec![9; 32],
    )
    .expect("projection capability")
}

#[test]
fn cli_resolve_selector_round_trips_into_the_shared_shape_request() {
    use backend_library::{Coverage, Cursor, Library, Reason, Row, RowId, symbol_key};
    struct Owner(Library);
    impl CommandTransport for Owner {
        fn request(&mut self, request: CommandDto) -> Result<ReplyDto, ClientError> {
            let mut reply = self.0.execute_dto(request.clone());
            if let (Command::Name(query), CommandReply::Names(page)) =
                (&request.command, &reply.reply)
            {
                // Match the real producer query proof: revision scope plus the
                // exact bounded page preimages and retained symbol commitments.
                let recipe = backend_library::QueryPageRecipe::names(self.0.revision_root(), query);
                let mut proof = self
                    .0
                    .execute_dto(CommandDto::new(1, Command::Revision))
                    .certificate()
                    .expect("owned revision proof")
                    .clone();
                let root = &page.root;
                for claim in [
                    WireClaim::KeyBytes {
                        schema: WireSchema::ViewRecipe,
                        id: encode_id(root.recipe().as_bytes()),
                        value: recipe.canonical_preimage().into(),
                    },
                    WireClaim::Version {
                        schema: WireSchema::ViewVersion,
                        id: encode_id(root.version().as_bytes()),
                        value: backend_library::view_version_preimage(
                            root.recipe(),
                            root.basis(),
                            root.frontier(),
                            root.root(),
                            root.coverage(),
                        )
                        .into_boxed_slice(),
                    },
                    WireClaim::Root {
                        schema: WireSchema::ViewRelation,
                        id: encode_id(root.root().as_bytes()),
                        canonical: root
                            .canonical_relation_bytes()
                            .expect("owned page relation")
                            .into_boxed_slice(),
                    },
                ] {
                    proof = proof.with_claim_once(claim);
                }
                for row in root.rows() {
                    if let RowId::Symbol(symbol) = row.id {
                        proof = proof.with_claim_once(WireClaim::KeyCommitment {
                            schema: WireSchema::Symbol,
                            id: encode_id(symbol.as_bytes()),
                        });
                    }
                }
                reply = reply.with_certificate(proof);
            }
            let bytes = serde_json::to_vec(&reply).expect("producer wire");
            let reply = ReplyDto::decode_with_certificate(&bytes, self.0.view().capability())
                .map_err(ClientError::Protocol)?;
            admit_reply(&request, reply)
        }
    }
    let key = symbol_key("retained-selected-row");
    let basis = Basis::new(
        backend_library::view_state_root(&[]),
        object_version(b"library-source-v1"),
    );
    let root = ViewRoot::new_checked(
        view_key(b"library-view-v1"),
        basis,
        Frontier::new(basis.branch, basis.log, basis.schema, basis.root, 0),
        vec![Row::new(
            RowId::Symbol(key),
            basis,
            "/abs/p::src/lib.rs:1::ferris",
        )],
        vec![
            Coverage::Complete,
            Coverage::Unavailable {
                lane: backend_library::Lane::Semantic,
                reason: Reason::Unconfigured,
            },
        ],
        fixture_projection_capability(basis),
    )
    .expect("checked projection fixture; semantic lane stays unconfigured");
    let owner = Library::from_view(root.clone(), Cursor::for_view_root(&root)).expect("owner");
    let mut session =
        backend_client::Session::from_transport("/private/selector.sock", Owner(owner));
    let invocation = invoke::parse(&words("resolve ferris"), None).expect("CLI parse");
    let request = lower(&invocation, "/abs/p").expect("CLI lower");
    let answer = run::execute(&mut session, &request).expect("actual admitted resolve");
    let value: serde_json::Value =
        serde_json::from_str(&render::json(&answer)).expect("public CLI JSON");
    let selector = &value["records"][0]["identity"]["semantic_data"];
    assert_eq!(selector["kind"], "selected-symbol-id");
    assert_eq!(selector["value"], serde_json::json!(key.as_bytes()));
    let mut operands: serde_json::Value = serde_json::from_str(include_str!(
        "../../../crates/library/fixtures/semantic-shape-read.json"
    ))
    .expect("source operand fixture; no compiler authority claim");
    operands["symbols"] = serde_json::json!([selector["value"]]);
    let words = vec![
        "semantic-shapes".to_owned(),
        serde_json::to_string(&operands).expect("operand JSON"),
    ];
    let invocation = invoke::parse(&words, None).expect("public shape CLI parse");
    let Request::Surface(surface) = lower(&invocation, "/abs/p").expect("shared shape grammar")
    else {
        panic!("shape request must retain its typed surface route");
    };
    let SurfaceCommand::SemanticShapes { request } = *surface else {
        panic!("shape request must retain its typed semantic-shapes command");
    };
    assert_eq!(request.symbols(), &[*key.as_bytes()]);
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
    for expected in [
        "rust",
        "typescript",
        "python",
        "go",
        "java",
        "csharp",
        "c",
        "cpp",
    ] {
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
fn an_admitted_dependency_refusal_keeps_json_evidence_and_a_nonzero_exit() {
    let package =
        backend_library::PackageReference::parse("pkg:npm/react@19.1.0").expect("package");
    let view = backend_present::product_view_for_command(
        &backend_library::SurfaceCommand::Dependencies {
            package: package.clone(),
        },
        &backend_library::SurfaceReply::Dependencies(
            backend_library::DependencyFacts::Unavailable(
                backend_library::ProductText::from_static("package is not recorded"),
            ),
        ),
    );
    let answer = Answer::Product(Box::new(view));
    assert_eq!(
        render::answer_exit_code(&answer),
        ExitCode::from(render::EXIT_REFUSED)
    );
    let encoded: serde_json::Value =
        serde_json::from_str(&render::json(&answer)).expect("complete JSON");
    assert_eq!(encoded["answer"], "product");
    assert_eq!(encoded["fault"]["operand"], package.as_str());
    assert_eq!(
        encoded["fault"]["call"]["arguments"]["package"],
        package.as_str()
    );
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
        graph_relations: None,
        rich_graph: None,
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
    assert_eq!(value["answer"], "records");
    assert_eq!(value["query"], "ferris");
    assert!(value["coverage"].is_array());
    assert!(
        value.get("certificate").is_none(),
        "the product JSON must not leak wire proof"
    );
}

#[test]
fn an_oversized_json_page_is_a_nonzero_typed_transport_refusal() {
    let snapshot = ViewSnapshot {
        root: root(),
        freshness: Freshness::Current,
        next: None,
        graph_relations: None,
        rich_graph: None,
    };
    let answer = Answer::Records(Box::new(backend_present::record_list(
        &"q".repeat(backend_present::DEFAULT_RESPONSE_BUDGET_BYTES + 1),
        &snapshot,
    )));
    let session = backend_client::Session::from_transport(
        "/private/oversized-answer.sock",
        Checked { reply: None },
    );
    let options = options::split(&["--json".to_owned()])
        .expect("JSON options")
        .0;

    let fault = process::render_admitted_answer(&session, &answer, &options)
        .expect_err("an incomplete JSON DTO cannot be reported as success");
    assert_eq!(fault.slug(), FaultSlug::Transport);
    assert_eq!(fault.cause().slug(), backend_present::CauseSlug::Oversized);
    assert_eq!(render::exit_code(&fault), ExitCode::from(render::EXIT_IO));

    let rendered: serde_json::Value =
        serde_json::from_str(&render::answer(&answer, &options)).expect("bounded fault JSON");
    assert_eq!(rendered["answer"], "fault");
    assert_eq!(rendered["cause"], "oversized");
}

#[test]
fn cli_named_versions_preserves_the_captured_semver_operand_at_every_detail() {
    let packet: serde_json::Value = serde_json::from_str(include_str!(
        "../../../crates/present/fixtures/semantic-versions-semver-public.json"
    ))
    .expect("complete captured public source packet");
    let reply: backend_library::SurfaceReply = serde_json::from_value(packet["surface"].clone())
        .expect("complete source DTO; no wire certificate is fabricated");
    let view = backend_present::product_view(&reply);
    let answer = Answer::Product(Box::new(view));
    for detail in [
        backend_present::Detail::Summary,
        backend_present::Detail::Standard,
        backend_present::Detail::Full,
    ] {
        let rendered = render::json_with_detail(&answer, detail);
        let value: serde_json::Value = serde_json::from_str(&rendered).expect("public CLI JSON");
        assert_eq!(value["answer"], "product");
        assert_eq!(value["semantic_data"]["value"], packet["surface"]["data"]);
        assert!(
            value["records"][0]["history_status"]["proof"]
                .get("images")
                .is_none()
        );
        let shared = backend_present::encode_answer(
            &answer,
            detail,
            None,
            backend_present::DEFAULT_RESPONSE_BUDGET_BYTES,
        )
        .expect("shared CLI/MCP projection");
        assert_eq!(rendered.as_bytes(), shared.bytes.as_ref());
    }
    assert_eq!(
        render::markdown_text(&answer),
        backend_present::bounded_text(&backend_present::markdown::answer(&answer))
    );
}

#[cfg(any(unix, windows))]
#[test]
fn cli_exports_plain_graph_continuation_without_leaking_the_wire_proof() {
    use backend_library::{Coverage, Cursor, Library, Row, RowId, symbol_key, view_state_root};
    struct GraphTransport(Library);
    impl CommandTransport for GraphTransport {
        fn request(&mut self, request: CommandDto) -> Result<ReplyDto, ClientError> {
            let mut reply = self.0.execute_dto(request.clone());
            if let (Command::GraphPage { symbol, .. }, CommandReply::ProjectionPage(page)) =
                (&request.command, &reply.reply)
            {
                let recipe =
                    backend_library::QueryPageRecipe::graph(self.0.revision_root(), *symbol);
                let mut proof = self
                    .0
                    .execute_dto(CommandDto::new(1, Command::Revision))
                    .certificate()
                    .expect("owner scope")
                    .clone();
                // The selected graph operand and exact canonical predecessor
                // come from this fixture producer's admitted request and page.
                if let Some(request_proof) = request.certificate() {
                    for claim in &request_proof.claims {
                        proof = proof.with_claim_once(claim.clone());
                    }
                }
                let root = &page.snapshot.root;
                for claim in [
                    WireClaim::KeyBytes {
                        schema: WireSchema::ViewRecipe,
                        id: encode_id(root.recipe().as_bytes()),
                        value: recipe.canonical_preimage().into(),
                    },
                    WireClaim::Version {
                        schema: WireSchema::ViewVersion,
                        id: encode_id(root.version().as_bytes()),
                        value: backend_library::view_version_preimage(
                            root.recipe(),
                            root.basis(),
                            root.frontier(),
                            root.root(),
                            root.coverage(),
                        )
                        .into_boxed_slice(),
                    },
                    WireClaim::Root {
                        schema: WireSchema::ViewRelation,
                        id: encode_id(root.root().as_bytes()),
                        canonical: root
                            .canonical_relation_bytes()
                            .expect("exact relation")
                            .into_boxed_slice(),
                    },
                ] {
                    proof = proof.with_claim_once(claim);
                }
                for row in root.rows() {
                    if let RowId::Symbol(symbol) = row.id {
                        proof = proof.with_claim_once(WireClaim::KeyCommitment {
                            schema: WireSchema::Symbol,
                            id: encode_id(symbol.as_bytes()),
                        });
                    }
                    if let Some(package) = row.package {
                        proof = proof.with_claim_once(WireClaim::KeyCommitment {
                            schema: WireSchema::Package,
                            id: encode_id(package.as_bytes()),
                        });
                    }
                    if let Some(parent) = row.parent {
                        proof = proof.with_claim_once(WireClaim::KeyCommitment {
                            schema: WireSchema::Symbol,
                            id: encode_id(parent.as_bytes()),
                        });
                    }
                }
                reply = reply.with_certificate(proof);
            }
            let bytes = serde_json::to_vec(&reply).expect("producer wire");
            let reply = ReplyDto::decode_with_certificate(&bytes, self.0.view().capability())
                .map_err(ClientError::Protocol)?;
            admit_reply(&request, reply)
        }
    }
    let object = object_version(b"library-source-v1");
    let basis = Basis::new(view_state_root(&[]), object);
    let capability = fixture_projection_capability(basis);
    let package = package_key("pkg");
    let parent = symbol_key("pkg::Parent");
    let mut rows = vec![Row::in_package(
        RowId::Symbol(parent),
        basis,
        package,
        "pkg::Parent",
    )];
    for i in 0..5 {
        let mut child = Row::in_package(
            RowId::Symbol(symbol_key(&format!("pkg::Child{i}"))),
            basis,
            package,
            format!("pkg::Child{i}"),
        );
        child.parent = Some(parent);
        rows.push(child);
    }
    let root = ViewRoot::new_checked(
        view_key(b"library-view-v1"),
        basis,
        Frontier::new(basis.branch, basis.log, basis.schema, basis.root, 0),
        rows,
        vec![Coverage::Complete],
        capability,
    )
    .expect("graph root");
    let owner =
        Library::from_view(root.clone(), Cursor::for_view_root(&root)).expect("graph owner");
    let mut session =
        backend_client::Session::from_transport("/private/graph-owner.sock", GraphTransport(owner));
    let baseline = session
        .graph_page("pkg::Parent", 200, None)
        .expect("ordinary graph order");
    let CommandReply::ProjectionPage(baseline) = baseline.reply else {
        panic!("graph baseline");
    };
    let expected = baseline
        .snapshot
        .root
        .rows()
        .iter()
        .map(|row| row.id)
        .collect::<Vec<_>>();
    let reply = session
        .graph_page("pkg::Parent", 2, None)
        .expect("real bounded graph page");
    let CommandReply::ProjectionPage(page) = reply.reply else {
        panic!("graph page reply");
    };
    assert_eq!(page.snapshot.root.rows().len(), 2);
    let continuation = page
        .snapshot
        .next
        .expect("five children exceed graph page credit");
    let answer = backend_present::Answer::Records(Box::new(backend_present::record_list(
        "pkg::Parent",
        &page.snapshot,
    )));
    assert!(answer.continuation().is_some());
    assert!(session.has_portable_query_continuation(
        backend_library::PageContinuation::from_cursor(continuation)
    ));
    let options = options::split(&["--json".to_owned()])
        .expect("JSON options")
        .0;
    let rendered = process::render_admitted_answer(&session, &answer, &options)
        .expect("graph response remains successful");
    let payload: serde_json::Value = serde_json::from_str(&rendered).expect("rendered graph JSON");
    assert_eq!(payload["more"], true);
    assert_eq!(payload["records"].as_array().expect("records").len(), 2);
    let token = payload["nextCursor"]
        .as_str()
        .expect("portable graph cursor");
    assert!(token.starts_with("pc3."));
    assert!(payload.get("certificate").is_none());
    assert!(payload.get("query_proof").is_none());
    let owner =
        Library::from_view(root.clone(), Cursor::for_view_root(&root)).expect("reopened owner");
    let mut cold =
        backend_client::Session::from_transport("/private/graph-owner.sock", GraphTransport(owner));
    let next = cold
        .decode_page_continuation(token)
        .expect("strict cold graph token");
    let reply = cold.continue_page(next).expect("exact second graph page");
    let CommandReply::ProjectionPage(next) = reply.reply else {
        panic!("graph page");
    };
    assert_eq!(
        next.snapshot
            .root
            .rows()
            .iter()
            .map(|row| row.id)
            .collect::<Vec<_>>(),
        expected[2..4]
    );
}
