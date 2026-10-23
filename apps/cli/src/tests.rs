//! CLI adapter tests.
//!
//! Two halves, deliberately separate. The transport half checks that an
//! admitted request and its proof survive the wire unchanged — that contract
//! did not move and neither did its cases. The surface half checks the things
//! this rewrite is responsible for: that every registry row is reachable by
//! the words a person types, that an operand the engine cannot admit fails
//! with the operand named, and that `--format markdown` uses the same renderer
//! as the MCP text block, including CLI-specific continuation syntax.
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
            cargo_target: None,
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

    /// A unique socket path within the portable local-endpoint byte budget.
    #[cfg(not(target_os = "macos"))]
    pub(super) fn scratch(label: &str) -> PathBuf {
        let name = format!(
            "bcli-{label}-{}-{}.sock",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        );
        // TMPDIR is suitable for ordinary temporary files but may be too deep
        // for a named socket; prefer short conventional Unix roots first.
        let mut roots = Vec::new();
        #[cfg(unix)]
        roots.extend([PathBuf::from("/tmp"), PathBuf::from("/var/tmp")]);
        roots.push(std::env::temp_dir());
        roots
            .into_iter()
            .map(|root| root.join(&name))
            .find(|path| {
                path.parent().is_some_and(std::path::Path::is_dir)
                    && path.as_os_str().as_encoded_bytes().len() <= crate::MAX_ENDPOINT_PATH
            })
            .unwrap_or_else(|| panic!("no temporary directory can hold a portable local endpoint"))
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
            cargo_target: None,
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
            cargo_target: None,
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
            cargo_target: None,
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
    assert!(help.contains("nudox mcp-config"));
    assert!(help.contains("no config writes or service start"));
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
        fault.cause().sentence().contains("nudox show [COORDINATE]"),
        "{}",
        fault.cause().sentence()
    );
}

#[test]
fn a_missing_operand_through_a_cli_alias_uses_the_entered_installed_command() {
    let fault = invoke::parse(&words("document"), None).expect_err("missing coordinate");
    assert_eq!(fault.slug(), FaultSlug::Usage);
    assert_eq!(fault.operand().render(), "document");
    assert_eq!(
        fault.cause().sentence(),
        "usage: nudox document [COORDINATE] [--selected-id SELECTED_ID] [--cursor CURSOR]"
    );
    assert_eq!(render::exit_code(&fault), ExitCode::from(64));
}

#[test]
fn a_page_bound_outside_its_range_is_refused_with_the_value() {
    let invocation = invoke::parse(&words("search ferris --limit 900"), None).expect("parse");
    let fault = lower(&invocation, "/abs/project").expect_err("out of range");
    assert_eq!(fault.operand().render(), "limit");
    assert!(fault.cause().sentence().contains("`900`"));
}

#[test]
fn package_graph_help_and_cli_lowering_share_the_engine_page_bound() {
    let help = invoke::help_for(grammar_for("package-graph").expect("package graph grammar"));
    assert!(help.contains(&format!(
        "Range: 1 to {}; default 64.",
        backend_library::MAX_PACKAGE_GRAPH_PAGE_ROWS
    )));
    assert!(
        invoke::help_for(grammar_for("search").expect("search grammar"))
            .contains("Range: 1 to 200; default 25.")
    );
    for (bound, valid) in [(128, true), (129, false)] {
        let args = words(&format!(
            "package-graph pkg:cargo/serde@1.0.228 dependencies --limit {bound}"
        ));
        let (options, command) = options::split(&args).expect("global page option");
        let input = invoke::parse(&command, options.limit()).expect("CLI parse");
        assert_eq!(lower(&input, "/abs/project").is_ok(), valid);
    }
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
    assert!(help.contains("--cargo-target NAME"));
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
        cargo_target,
        execution_intent,
        ..
    } = lower(&invocation, "/unused").expect("lower background Add")
    else {
        panic!("add lowers to an index request");
    };
    assert_eq!(
        execution_intent,
        backend_library::CompileExecutionIntent::Background
    );
    assert_eq!(cargo_target, None);

    let target_invocation = invoke::parse(
        &words("add /abs/project --cargo-target integration-test"),
        None,
    )
    .expect("parse explicit Cargo target");
    let Request::IndexWithExecutionIntent { cargo_target, .. } =
        lower(&target_invocation, "/unused").expect("lower target Add")
    else {
        panic!("an explicit Cargo target uses the scoped index request");
    };
    assert_eq!(cargo_target.expect("selector").as_str(), "integration-test");

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
        &words(
            "index_start pkg:cargo/serde@1.0.228 --execution-intent background --cargo-target lib",
        ),
        None,
    )
    .expect("parse index_start");
    assert!(matches!(
        lower(&start, "/unused"),
        Ok(Request::Surface(command))
            if matches!(*command,
                SurfaceCommand::IndexStart {
                    cargo_target: Some(ref target),
                    package: backend_library::PackageReference::Purl(ref package),
                    execution_intent: backend_library::CompileExecutionIntent::Background,
                } if package.as_str() == "pkg:cargo/serde@1.0.228" && target.as_str() == "lib")
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
    assert!(
        help.contains("[TICKET]"),
        "ticket is optional when using --operation-key"
    );
    assert!(help.contains("quote it as one shell argument"));
    assert!(help.contains("--after-sequence SEQUENCE"));
    assert!(help.contains("--operation-key TEXT"));
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
    let key = backend_library::IndexOperationKey::from_bytes([0x76; 32]).expect("saved key");
    let unconfirmed = Fault::from_client_error(
        &ClientError::IndexOperationDeliveryUnknown {
            operation_key: key,
            cause: Box::new(ClientError::Disconnected(std::io::ErrorKind::WouldBlock)),
        },
        backend_present::Operand::Whole,
    );
    assert_eq!(
        render::exit_code(&unconfirmed),
        ExitCode::from(crate::index_wait::EXIT_WAIT_INCOMPLETE)
    );
    let wire = backend_present::fault_value(&unconfirmed);
    assert_eq!(
        wire["unconfirmed_index_delivery"],
        serde_json::to_value(key).expect("same key")
    );
    assert_eq!(
        wire["call"]["arguments"]["operation_key"],
        serde_json::to_value(key).expect("same key")
    );
    assert!(
        wire["detail"]
            .as_str()
            .expect("detail")
            .contains("may already be queued or accepted")
    );
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
fn admitted_durable_partial_matches_ticket_fault_and_cli_exit_without_inventing_source_facts() {
    use backend_library::{
        CompileExecutionIntent, IndexJobOutcome, IndexJobPartialPublication, IndexJobTerminal,
        IndexJobTicket, IndexOperationFailureReason, IndexOperationKey, IndexOperationObservation,
        IndexOperationProfileRefusal, IndexOperationPublicationReceipt,
        IndexOperationSemanticCoverage, IndexOperationSemanticProfileState,
        IndexOperationSemanticUnavailableReason, IndexOperationSourceCaptureReceipt,
        IndexOperationSourceProfile, IndexOperationState, IndexOperationStatus,
        IndexSourceCaptureSummary, PackageReference, ProductText, SemanticLanguageProfile,
        SurfaceReply,
    };
    // These are admitted serialization fixtures, not compiler authority.
    let package = PackageReference::parse("/abs/mixed-docs").expect("package");
    let key = IndexOperationKey::from_bytes([0x41; 32]).expect("operation key");
    let py = SemanticLanguageProfile::from_name("python").expect("Python");
    let ts = SemanticLanguageProfile::from_name("typescript").expect("TypeScript");
    let mut profiles = vec![
        IndexOperationSourceProfile {
            profile: py,
            source_version: [4; 32],
            input_digest: [5; 32],
            observation_sequence: 2,
            source_count: 236,
            state: IndexOperationSemanticProfileState::Published {
                generation: [6; 32],
                coverage: IndexOperationSemanticCoverage::Complete,
            },
        },
        IndexOperationSourceProfile {
            profile: ts,
            source_version: [4; 32],
            input_digest: [7; 32],
            observation_sequence: 3,
            source_count: 626,
            state: IndexOperationSemanticProfileState::Unavailable {
                reason: IndexOperationSemanticUnavailableReason::Rejected,
            },
        },
    ];
    profiles.sort_by_key(|profile| profile.profile);
    let view = root();
    let partial = IndexJobPartialPublication {
        package: package.clone(),
        receipt: IndexOperationPublicationReceipt::from_published_view(
            Some([8; 32]),
            [9; 32],
            [10; 32],
            3,
            &view,
            backend_library::Cursor::for_view_root(&view),
        )
        .expect("receipt"),
        source_capture: IndexSourceCaptureSummary {
            producer_package: PackageReference::parse("pkg:cargo/producer@1.0.0")
                .expect("producer"),
            request_identity: [1; 32],
            commit_identity: [2; 32],
            workspace_root: [3; 32],
            workspace_sequence: 2,
            profiles: profiles.into_boxed_slice(),
        },
        refused_profiles: vec![IndexOperationProfileRefusal {
            profile: ts,
            reason: IndexOperationSemanticUnavailableReason::Rejected,
            compiler_failure: None,
        }]
        .into_boxed_slice(),
    };
    let ticket = IndexJobTicket::new(std::num::NonZeroU64::MIN, [11; 16], package.clone());
    let capture = IndexOperationSourceCaptureReceipt::from_checked_parts(
        key,
        partial.source_capture.commit_identity,
        partial.source_capture.workspace_root,
        partial.source_capture.workspace_sequence,
        partial.source_capture.profiles.clone(),
    )
    .expect("capture");
    let status = IndexOperationStatus::new(
        key,
        package.clone(),
        CompileExecutionIntent::Interactive,
        IndexOperationState::PartiallyPublished {
            receipt: partial.receipt.clone(),
            refused_profiles: partial.refused_profiles.clone(),
        },
    )
    .with_source_capture(Some(capture));
    let operation = IndexOperationObservation::Known(status.clone());
    let cases = [
        (
            SurfaceCommand::IndexAwait {
                ticket: ticket.clone(),
            },
            SurfaceReply::IndexTerminal(IndexJobTerminal {
                ticket,
                outcome: IndexJobOutcome::PartiallyPublished(partial.clone()),
            }),
        ),
        (
            SurfaceCommand::IndexOperationStatus { operation_key: key },
            SurfaceReply::IndexOperationStatus(operation.clone()),
        ),
    ];
    let mut causes = Vec::new();
    for (command, reply) in cases {
        let request = CommandDto::new(901, Command::Surface(command.clone()));
        let bytes = serde_json::to_vec(&ReplyDto::new(901, CommandReply::Surface(reply)))
            .expect("wire encoding");
        let decoded = backend_library::decode_reply_body(&bytes).expect("actual wire decode");
        backend_library::admit_reply(&request, &decoded).expect("actual request-bound admission");
        let CommandReply::Surface(reply) = decoded.reply else {
            panic!("surface reply")
        };
        let answer = Answer::Product(Box::new(backend_present::product_view_for_command(
            &command, &reply,
        )));
        assert_eq!(
            render::answer_exit_code(&answer),
            ExitCode::from(render::EXIT_REFUSED)
        );
        let fault = answer.fault().expect("partial is a fault");
        assert_eq!(fault.slug(), FaultSlug::PartiallyPublished);
        causes.push(fault.cause().sentence().to_owned());
        let json: serde_json::Value = serde_json::from_str(&render::json(&answer)).expect("JSON");
        assert_eq!(json["fault"]["slug"], "partially-published");
        assert!(
            render::answer(&answer, &plain())
                .contains("refused profiles do not have current semantic coverage")
        );
        if matches!(command, SurfaceCommand::IndexOperationStatus { .. }) {
            assert_eq!(
                json["index_operation"],
                serde_json::to_value(&operation).expect("operation")
            );
            assert!(
                json["fault"]["partial_publication"].is_null(),
                "no fabricated legacy source summary"
            );
            assert_eq!(
                json["index_operation"]["detail"]["source_capture"],
                serde_json::to_value(&status.source_capture).expect("exact capture")
            );
        } else {
            assert_eq!(
                json["fault"]["partial_publication"],
                serde_json::to_value(&partial).expect("legacy receipt")
            );
        }
    }
    assert_eq!(
        causes[0], causes[1],
        "same exact profile facts imply the same refusal guidance"
    );
    for state in [
        IndexOperationState::Accepted,
        IndexOperationState::Failed {
            reason: IndexOperationFailureReason::Cancelled,
            detail: ProductText::from_static("cancelled before commit"),
            compiler_failure: None,
        },
    ] {
        let command = SurfaceCommand::IndexOperationStatus { operation_key: key };
        let reports_failure = matches!(&state, IndexOperationState::Failed { .. });
        let reply = SurfaceReply::IndexOperationStatus(IndexOperationObservation::Known(
            IndexOperationStatus::new(
                key,
                package.clone(),
                CompileExecutionIntent::Interactive,
                state,
            ),
        ));
        backend_library::admit_surface_reply(&command, &reply)
            .expect("a terminal operation state is still an admitted observation");
        let answer = Answer::Product(Box::new(backend_present::product_view_for_command(
            &command, &reply,
        )));
        if reports_failure {
            assert_eq!(
                answer.fault().expect("terminal failure is surfaced").slug(),
                FaultSlug::Rejected
            );
            assert_eq!(
                render::answer_exit_code(&answer),
                ExitCode::from(render::EXIT_REFUSED)
            );
        } else {
            assert!(answer.fault().is_none());
            assert_eq!(render::answer_exit_code(&answer), ExitCode::SUCCESS);
        }
    }
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
    assert!(rendered.ends_with("→ nudox search nothing"));
}

#[cfg(unix)]
#[test]
fn installed_fault_actions_round_trip_through_real_shell_argv_and_shared_cli_lowering() {
    use backend_present::{Affordance, Request, lower};
    let query = "error 'quoted' $(printf BAD)\nnext";
    for action in [
        Affordance::WaitForReadiness,
        Affordance::InspectOwner,
        Affordance::UseCommand {
            name: "status",
            args: Box::new([]),
        },
        Affordance::search(query),
    ] {
        let command = action.shell().expect("installed action");
        // Parse argv only; never execute nudox or start an owner.
        let parsed = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(format!("set -- {command}; printf '%s\\000' \"$@\""))
            .output()
            .expect("shell argv control");
        assert!(parsed.status.success(), "{command}");
        assert!(parsed.stderr.is_empty());
        let words = parsed
            .stdout
            .split(|byte| *byte == 0)
            .filter(|word| !word.is_empty())
            .map(|word| String::from_utf8(word.to_vec()).expect("UTF-8 argv"))
            .collect::<Vec<_>>();
        assert_eq!(words[0], "nudox");
        let invocation = invoke::parse(&words[1..], None).expect("actual CLI grammar");
        let request = lower(&invocation, "/original/project").expect("typed lowering");
        match action {
            Affordance::UseCommand { name: "search", .. } => {
                assert!(matches!(request, Request::Search { text, .. } if text == query));
            }
            _ => assert!(matches!(request, Request::Status)),
        }
    }
    let missing = invoke::parse(&["search".to_owned()], None).expect_err("missing query");
    assert!(
        missing
            .cause()
            .sentence()
            .starts_with("usage: nudox search")
    );
    assert!(invoke::help().starts_with("nudox —"));
    assert!(
        invoke::help_for(grammar_for("search").expect("search grammar"))
            .starts_with("nudox search")
    );
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
fn page_projection_cli_detail_controls_each_format_without_changing_show_source_defaults() {
    use backend_present::{
        Detail, Identity, Language, LineNumber, PackagePath, Page, Signature, Source, SourceSite,
        Truncation,
    };

    let coordinate = "/abs/app::src/entry.ts:42::entrySummary";
    let source = "CLI_SOURCE_SENTINEL";
    let signature = "function entrySummary(entry: Entry): string";
    let line = LineNumber::new(42).expect("one-based");
    let native = backend_library::symbol_key("cli-native-entry");
    let page = Page::new(
        Identity::parse_with_key(coordinate, backend_present::IdentityKey::Symbol(native)),
        Some(backend_library::DeclarationKind::Function),
        Source::Captured {
            site: SourceSite::new(PackagePath::new("src/entry.ts"), line),
            lines: Source::number_lines(source, line),
            truncation: Truncation::Complete,
        },
    )
    .with_signature(Signature::tokenize(signature, Language::TypeScript))
    .with_notes(vec![backend_present::Fault::usage(
        "relations",
        "CLI_NOTE_SENTINEL",
    )]);
    let answer = Answer::Page(Box::new(page));
    for format in ["human", "markdown", "json"] {
        for command in ["show", "source"] {
            let (summary, _) = options::split(&words(&format!(
                "--no-color --format {format} --detail summary {command} {coordinate}"
            )))
            .expect("explicit summary options");
            let rendered = render::try_answer_with_cursor(&answer, &summary, None)
                .expect("summary projection fits");
            assert!(rendered.contains(coordinate));
            assert!(rendered.contains(signature));
            assert!(rendered.contains("CLI_NOTE_SENTINEL"));
            assert!(!rendered.contains(source), "{format} summary leaked source");
            if format == "json" {
                let value: serde_json::Value = serde_json::from_str(&rendered).expect("JSON");
                assert!(value.get("source").is_none());
                assert!(value.get("source_fault").is_none());
                assert_eq!(
                    value["identity"]["semantic_data"]["value"],
                    serde_json::json!(native.as_bytes())
                );
            }
            for level in [None, Some("standard"), Some("full")] {
                let flag = level.map_or(String::new(), |level| format!("--detail {level}"));
                let (options, _) = options::split(&words(&format!(
                    "--no-color --format {format} {flag} {command} {coordinate}"
                )))
                .expect("source-bearing options");
                assert_eq!(
                    options.detail(),
                    if level == Some("full") {
                        Detail::Full
                    } else {
                        Detail::Standard
                    }
                );
                let rendered = render::try_answer_with_cursor(&answer, &options, None)
                    .expect("source projection fits");
                assert!(
                    rendered.contains(source),
                    "{format} {command} {level:?} lost source"
                );
                assert!(rendered.contains(signature));
                assert!(rendered.contains("CLI_NOTE_SENTINEL"));
                if format == "json" {
                    let value: serde_json::Value = serde_json::from_str(&rendered).expect("JSON");
                    assert_eq!(value["source"]["path"], "src/entry.ts");
                    assert_eq!(value["source"]["line"], 42);
                    assert_eq!(value["source"]["extent"], "complete");
                    assert_eq!(
                        value["identity"]["semantic_data"]["value"],
                        serde_json::json!(native.as_bytes())
                    );
                }
            }
        }
    }
    assert!(render::markdown_text(&answer).contains(source));
    assert!(invoke::help().contains("page text follows the same detail"));
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
    assert!(token.starts_with("pc4-"));
    assert!(payload.get("certificate").is_none());
    assert!(payload.get("query_proof").is_none());
    for format in ["human", "markdown"] {
        let options = options::split(&["--format".to_owned(), format.to_owned()])
            .expect("text options")
            .0;
        let rendered = process::render_admitted_answer(&session, &answer, &options)
            .expect("text graph response remains successful");
        assert!(
            rendered.contains("continue the same command with `--cursor`"),
            "{rendered}"
        );
        assert!(rendered.contains(token), "{rendered}");
        assert!(!rendered.contains("raise --limit"), "{rendered}");
        assert!(!rendered.contains("raise `limit`"), "{rendered}");
    }
    let owner =
        Library::from_view(root.clone(), Cursor::for_view_root(&root)).expect("reopened owner");
    let mut cold =
        backend_client::Session::from_transport("/private/graph-owner.sock", GraphTransport(owner));
    let next = cold
        .decode_page_continuation(token)
        .expect("strict cold graph token");
    let reply = cold
        .graph_page("pkg::Parent", 2, Some(next))
        .expect("exact second graph page");
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
    // Exercise the command people actually type, reopening its connected client
    // on every page. The owner, certificates and cursors remain real library DTOs.
    let mut token: Option<String> = None;
    let mut observed = Vec::new();
    let mut pages = 0;
    loop {
        let owner = Library::from_view(root.clone(), Cursor::for_view_root(&root))
            .expect("same immutable graph owner");
        let mut client = backend_client::Session::from_transport(
            "/private/graph-owner.sock",
            GraphTransport(owner),
        );
        let mut args = [
            "--json",
            "--detail",
            "full",
            "graph",
            "pkg::Parent",
            "--limit",
            "1",
        ]
        .map(str::to_owned)
        .to_vec();
        if let Some(cursor) = &token {
            args.extend(["--cursor".to_owned(), cursor.clone()]);
        }
        let (options, rest) = options::split(&args).expect("actual CLI options");
        let invocation = invoke::parse(&rest, options.limit()).expect("actual CLI graph grammar");
        let request = lower(&invocation, "/p").expect("actual bounded graph request");
        let answer = run::execute_with_output_admission(
            &mut client,
            &request,
            None,
            None,
            backend_present::Detail::Full,
            None,
            |_, _| Ok(()),
        )
        .expect("certified native CLI graph page");
        let Answer::Records(records) = &answer else {
            panic!("graph records");
        };
        assert!(records.records().len() <= 1);
        let rendered = process::render_admitted_answer(&client, &answer, &options)
            .expect("complete full CLI encoding");
        let payload: serde_json::Value = serde_json::from_str(&rendered).expect("actual CLI JSON");
        observed.extend(
            payload["records"]
                .as_array()
                .expect("records")
                .iter()
                .map(|row| {
                    row["identity"]["coordinate"]
                        .as_str()
                        .expect("coordinate")
                        .to_owned()
                }),
        );
        for format in ["human", "markdown"] {
            let options =
                options::split(&["--format", format, "--detail", "full"].map(str::to_owned))
                    .expect("plain graph options")
                    .0;
            let plain = process::render_admitted_answer(&client, &answer, &options)
                .expect("full plain graph page");
            assert_eq!(
                plain.contains("continue the same command with `--cursor`"),
                payload["more"] == true
            );
        }
        pages += 1;
        token = payload["nextCursor"].as_str().map(str::to_owned);
        if pages == 1 {
            let saved = token.as_ref().expect("first credit1 graph page continues");
            for (coordinate, credit) in [("pkg::Parent", "2"), ("pkg::Child0", "1")] {
                let owner = Library::from_view(root.clone(), Cursor::for_view_root(&root))
                    .expect("same owner for cursor opponent");
                let mut opponent = backend_client::Session::from_transport(
                    "/private/graph-owner.sock",
                    GraphTransport(owner),
                );
                let args = [
                    "graph",
                    coordinate,
                    "--limit",
                    credit,
                    "--cursor",
                    saved.as_str(),
                ]
                .map(str::to_owned);
                let (options, rest) = options::split(&args).expect("opponent option parse");
                let invocation = invoke::parse(&rest, options.limit()).expect("opponent grammar");
                let request = lower(&invocation, "/p").expect("opponent request");
                assert!(
                    run::execute_with_output_admission(
                        &mut opponent,
                        &request,
                        None,
                        None,
                        backend_present::Detail::Full,
                        None,
                        |_, _| Ok(())
                    )
                    .is_err(),
                    "changed credit or declaration cannot resume the original graph"
                );
            }
        }
        if payload["more"] == false {
            assert!(token.is_none());
            break;
        }
        assert!(token.is_some());
        assert!(pages <= 16, "bounded terminal chain");
    }
    let expected_coordinates = baseline
        .snapshot
        .root
        .rows()
        .iter()
        .map(|row| row.label.clone())
        .collect::<Vec<_>>();
    assert_eq!(observed, expected_coordinates);
    assert!(pages > 1);
}

#[test]
fn bounded_graph_cli_arguments_preserve_exact_selector_credit_and_cursor() {
    const AT: &str = "/p::semantic::row::Caller";
    let (options, rest) =
        options::split(&["--json", "graph", AT, "--limit", "1"].map(str::to_owned))
            .expect("real graph global options");
    let invocation = invoke::parse(&rest, options.limit()).expect("real graph grammar");
    assert!(
        matches!(lower(&invocation, "/p").expect("bounded graph"), Request::GraphPage { selector: backend_library::DeclarationSelector::Coordinate(value), limit: 1 } if value == AT)
    );
    let invocation = invoke::parse(
        &["graph", AT, "--limit", "1", "--cursor", "pc4-proof"].map(str::to_owned),
        None,
    )
    .expect("graph continuation grammar");
    assert!(
        matches!(lower(&invocation, "/p").expect("continued graph"), Request::ResumeQuery { query, cursor } if matches!(*query, Request::GraphPage { limit: 1, .. }) && cursor == "pc4-proof")
    );
    let invocation =
        invoke::parse(&["graph", AT].map(str::to_owned), None).expect("original graph grammar");
    assert!(matches!(
        lower(&invocation, "/p").expect("original graph"),
        Request::Neighbourhood {
            incoming: false,
            ..
        }
    ));
    let id = "62".repeat(32);
    let invocation = invoke::parse(
        &["graph", AT, "--selected-id", &id, "--limit", "1"].map(str::to_owned),
        None,
    )
    .expect("selected graph grammar");
    assert!(
        matches!(lower(&invocation, "/p").expect("selected bounded graph"), Request::GraphPage { selector: backend_library::DeclarationSelector::SelectedAt { coordinate, .. }, limit: 1 } if coordinate == AT)
    );
    for bound in ["0", "201"] {
        let invocation = invoke::parse(&["graph", AT, "--limit", bound].map(str::to_owned), None)
            .expect("option lexing");
        assert!(lower(&invocation, "/p").is_err());
    }
}

#[test]
fn bounded_outline_cli_arguments_lower_to_the_same_native_page_as_mcp() {
    use backend_present::{Request, lower};
    let (options, rest) = options::split(&words("--limit 5 outline /p")).expect("options");
    let invocation = invoke::parse(&rest, options.limit()).expect("shared page grammar");
    assert!(
        matches!(lower(&invocation, "/p").expect("native outline"), Request::OutlinePage { path, limit: 5 } if path == "/p")
    );
    let invocation = invoke::parse(&words("outline /p --limit 5 --cursor pc4-proof"), None)
        .expect("cursor option");
    assert!(
        matches!(lower(&invocation, "/p").expect("continued page"), Request::ResumeQuery { query, cursor } if matches!(*query, Request::OutlinePage { limit: 5, .. }) && cursor == "pc4-proof")
    );
    let invocation = invoke::parse(&words("outline /p"), None).expect("default grammar");
    assert!(matches!(
        lower(&invocation, "/p").expect("full outline"),
        Request::Outline(_)
    ));
    for path in ["/p", "."] {
        let invocation = invoke::parse(
            &["outline", path, "--cursor", "pc4-proof"].map(str::to_owned),
            None,
        )
        .expect("default outline continuation");
        assert!(matches!(
            lower(&invocation, "/p").expect("unchanged default credit"),
            Request::ResumeQuery { query, cursor }
                if matches!(query.as_ref(), Request::OutlinePage { path, limit }
                    if path == "/p" && *limit == QueryLimit::MAX)
                    && cursor == "pc4-proof"
        ));
    }
    let invocation = invoke::parse(&words("outline /p --cursor pc4-proof"), Some("7"))
        .expect("explicit global credit");
    assert!(matches!(
        lower(&invocation, "/p").expect("retain explicit global credit"),
        Request::ResumeQuery { query, .. }
            if matches!(*query, Request::OutlinePage { limit: 7, .. })
    ));
    let invocation = invoke::parse(&words("search Thing --cursor pc4-proof"), None)
        .expect("ordinary search continuation");
    assert!(matches!(
        lower(&invocation, "/p").expect("retain ordinary query default"),
        Request::ResumeQuery { query, .. }
            if matches!(*query, Request::Search { limit: backend_present::DEFAULT_LIMIT, .. })
    ));
    let invocation = invoke::parse(&words("outline /p --limit 0"), None).expect("option parse");
    assert!(lower(&invocation, "/p").is_err());
}

#[path = "tests/composite_show.rs"]
mod composite_show;

#[test]
fn cli_selected_id_is_copyable_full_width_and_optional_coordinate_is_an_assertion() {
    const PROJECT: &str = "/abs/app";
    const DECLARATION: &str = "/abs/app::box.ts:1::Box::constructor::item";
    use backend_library::{DeclarationSelector, SelectedSymbolId};
    let id = SelectedSymbolId::new([0x62; 32]).expect("nonzero row");
    for verb in ["show", "source", "graph", "related"] {
        for coordinate in [None, Some(DECLARATION)] {
            let mut args = vec![verb.to_owned(), "--selected-id".to_owned(), "62".repeat(32)];
            if let Some(coordinate) = coordinate {
                args.push(coordinate.to_owned());
            }
            let invocation = invoke::parse(&args, None).expect("actual CLI parser");
            let Request::Declaration { selector, .. } =
                lower(&invocation, PROJECT).expect("typed read")
            else {
                panic!("selected request")
            };
            assert_eq!(selector.selected_id(), Some(id));
            assert_eq!(selector.coordinate(), coordinate);
            assert!(matches!(
                selector,
                DeclarationSelector::Selected(_) | DeclarationSelector::SelectedAt { .. }
            ));
        }
        for invalid in [
            "62".repeat(8),
            "00".repeat(32),
            format!("0x{}", "62".repeat(32)),
            "gg".repeat(32),
        ] {
            let args = vec![verb.to_owned(), "--selected-id".to_owned(), invalid];
            let invocation = invoke::parse(&args, None).expect("grammar accepts operand text");
            assert!(lower(&invocation, PROJECT).is_err());
        }
    }
    assert!(invoke::parse(&words("references --selected-id 6262"), None).is_err());
}

#[test]
fn published_names_search_is_useful_partial_json_with_exact_identity_and_zero_exit() {
    let base = root();
    let coordinate = "/workspace/docs::models.py:994::Document";
    let symbol = backend_library::symbol_key("native Document search-stage identity");
    let row = backend_library::Row::in_package(
        backend_library::RowId::Symbol(symbol),
        base.basis(),
        package_key("/workspace/docs"),
        coordinate,
    )
    .with_kind(backend_library::DeclarationKind::Class)
    .with_source(backend_library::SourceLocation::new("models.py", 994).expect("authored site"));
    let view = ViewRoot::new_incomplete(
        base.recipe(),
        base.basis(),
        base.frontier(),
        vec![row],
        Vec::new(),
    )
    .expect("exact retained projection");
    let snapshot = ViewSnapshot {
        root: view,
        freshness: Freshness::Current,
        next: None,
        graph_relations: None,
        rich_graph: None,
    };
    let reply = ReplyDto::new(
        1,
        CommandReply::PublishedNameSearch(backend_library::PublishedNameSearchPage {
            snapshot,
            readiness: backend_library::PublishedSearchReadiness {
                full_search: backend_library::FullSearchReadiness::Preparing,
                semantic: backend_library::PublishedSemanticReadiness::Pending,
            },
        }),
    );
    let answer = backend_present::catalog_page_answer(
        &backend_present::Request::Search {
            text: "Document".to_owned(),
            limit: 10,
        },
        reply,
    )
    .expect("shared admitted route");
    // Summary is the ordinary CLI default; every detail must retain stage
    // scope as well as exact useful identities and independent readiness.
    for detail in [
        backend_present::Detail::Summary,
        backend_present::Detail::Standard,
        backend_present::Detail::Full,
    ] {
        let value: serde_json::Value =
            serde_json::from_str(&render::json_with_detail(&answer, detail))
                .expect("ordinary CLI JSON");
        assert_eq!(value["readiness"], "partial");
        assert_eq!(value["search_scope"], "published_names_only");
        assert_eq!(value["full_search"], "preparing");
        assert_eq!(value["semantic_preparation"]["state"], "pending");
        assert_eq!(value["records"].as_array().expect("useful names").len(), 1);
        assert_eq!(value["records"][0]["identity"]["coordinate"], coordinate);
        assert_eq!(
            value["records"][0]["identity"]["semantic_data"]["value"],
            serde_json::json!(*symbol.as_bytes())
        );
    }
    assert_eq!(render::answer_exit_code(&answer), ExitCode::SUCCESS);
    let text = render::markdown_text(&answer);
    assert!(text.contains("Document"));
    assert!(
        text.contains("published-names-only")
            && text.contains("preparing")
            && text.contains("pending")
    );
}
