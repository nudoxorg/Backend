//! CLI adapter tests.
#![allow(clippy::expect_used, clippy::panic)]

use super::transport::{read_frame, write_frame};
use super::*;
use backend_library::{
    Basis, CommandReply, DTO_VERSION, Document, Fragment, Freshness, Frontier, Query, QueryLimit,
    Row, RowId, SourceExcerpt, SourceExcerptExtent, SourceLocation, SurfaceCommand, ViewRoot,
    ViewSnapshot, WireCertificate, WireClaim, WireSchema, branch_key, encode_id, log_key,
    object_version, package_key, symbol_key, view_key,
};
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
fn parser_requires_an_admitted_basis_and_rejects_unknown_options() {
    let basis = encode_id(backend_library::view_state_root(&[]).as_bytes());
    assert!(parse(&["search".to_owned(), "Thing".to_owned()]).is_err());
    assert!(
        parse(&[
            "search".to_owned(),
            "Thing".to_owned(),
            "--basis".to_owned(),
            basis,
            "--limit".to_owned(),
            "10".to_owned(),
        ])
        .is_err()
    );
    assert!(parse(&["health".to_owned(), "extra".to_owned()]).is_err());
    assert!(matches!(
        parse(&["show".to_owned(), "pkg::Thing".to_owned()]),
        Ok(Command::Show { .. })
    ));
    assert!(matches!(
        parse(&["resolve".to_owned(), "Thing".to_owned()]),
        Ok(Command::Resolve { text }) if text == "Thing"
    ));
    assert!(parse(&["search".to_owned(), "x".to_owned(), "--unknown".to_owned()]).is_err());
    assert!(
        parse(&[
            "outline".to_owned(),
            "pkg".to_owned(),
            "--limit".to_owned(),
            "10".to_owned(),
        ])
        .is_err()
    );
}

#[test]
fn parser_constructs_a_typed_package_diff_without_a_view_basis() {
    let command = parse(&[
        "diff".to_owned(),
        "pkg:cargo/example@1.0.0".to_owned(),
        "pkg:cargo/example@2.0.0".to_owned(),
    ])
    .expect("typed diff");
    assert!(matches!(
        command,
        Command::Surface(SurfaceCommand::Diff { from, to })
            if from.as_str() == "pkg:cargo/example@1.0.0"
                && to.as_str() == "pkg:cargo/example@2.0.0"
    ));
    assert!(parse(&["diff".to_owned(), "only-one".to_owned()]).is_err());
    assert!(
        parse(&[
            "diff".to_owned(),
            "pkg:cargo/example".to_owned(),
            "pkg:cargo/example@2.0.0".to_owned(),
        ])
        .is_err()
    );
}

#[test]
fn complete_surface_enum_is_reachable_through_one_typed_cli_bridge() {
    use backend_library::{
        PackageCoordinate, PackageReference, ProductText, ProjectName, ProjectSelector,
        SemanticGenerationId, SemanticLanguageProfile, TreeNodeId, TreeOpener, TreeSubject,
    };
    use std::num::NonZeroU64;

    let package = PackageReference::parse("pkg:cargo/example@1.0.0").expect("package");
    let coordinate = PackageCoordinate::parse("pkg:cargo/example@1.0.0").expect("coordinate");
    let text = || ProductText::new("example").expect("text");
    let project = || ProjectSelector::parse("project").expect("project");
    let node = TreeNodeId::new(NonZeroU64::MIN);
    let profile = serde_json::from_value::<SemanticLanguageProfile>(serde_json::json!([0, 3]))
        .expect("Rust 2024 profile");
    let commands = vec![
        SurfaceCommand::Read {
            locators: vec![text()].into_boxed_slice(),
        },
        SurfaceCommand::Diff {
            from: package.clone(),
            to: package.clone(),
        },
        SurfaceCommand::Explore {
            query: Some(text()),
            limit: 1,
        },
        SurfaceCommand::Package {
            package: package.clone(),
        },
        SurfaceCommand::Dependents {
            package: package.clone(),
        },
        SurfaceCommand::Owner { owner: text() },
        SurfaceCommand::IndexSearch {
            query: text(),
            limit: 1,
        },
        SurfaceCommand::PackageVersions {
            package: package.clone(),
        },
        SurfaceCommand::SemanticVersions {
            package: package.clone(),
        },
        SurfaceCommand::SelectSemanticVersion {
            package: package.clone(),
            coordinate,
            profile,
            generation: SemanticGenerationId::new([7; 32]),
        },
        SurfaceCommand::PackageProfile {
            package: package.clone(),
        },
        SurfaceCommand::Subscribe {
            package: package.clone(),
            project: Some(project()),
        },
        SurfaceCommand::Unsubscribe {
            package: package.clone(),
        },
        SurfaceCommand::Subscriptions,
        SurfaceCommand::Releases { mark_seen: true },
        SurfaceCommand::Projects,
        SurfaceCommand::ProjectCreate {
            name: ProjectName::new("project").expect("name"),
            lockfile: None,
        },
        SurfaceCommand::ProjectDelete { project: project() },
        SurfaceCommand::ProjectAdd {
            project: project(),
            package: package.clone(),
        },
        SurfaceCommand::ProjectRemove {
            project: project(),
            package: package.clone(),
        },
        SurfaceCommand::ProjectSync { project: project() },
        SurfaceCommand::Tree,
        SurfaceCommand::TreeOpen {
            subject: TreeSubject::Package(package),
            parent: Some(node),
            title: Some(text()),
            opener: TreeOpener::Cli,
        },
        SurfaceCommand::TreeClose { node, branch: true },
    ];
    assert_eq!(commands.len(), 24);
    for expected in commands {
        let encoded = serde_json::to_string(&expected).expect("surface JSON");
        let parsed = parse(&["surface".to_owned(), encoded]).expect("typed surface command");
        assert_eq!(parsed, Command::Surface(expected));
    }
}

#[test]
fn cli_preserves_typed_semantic_link_evidence_in_json_and_human_output() {
    let identity = backend_library::SemanticDeclarationIdentity {
        family: [1; 16],
        variant: [2; 16],
    };
    let reply = ReplyDto::new(
        7,
        CommandReply::Surface(backend_library::SurfaceReply::Diff(
            vec![backend_library::DiffRecord {
                label: backend_library::ProductText::new("example::call").expect("label"),
                change: backend_library::DeclarationChange::Changed,
                before: Some(identity),
                after: Some(identity),
                links: vec![backend_library::SemanticLinkDelta::EvidenceChanged {
                    from: identity,
                    target: backend_library::SemanticLinkTarget::Foreign {
                        declaration: [3; 16],
                        variant: None,
                    },
                    relation: backend_library::SemanticLinkKind::Calls,
                    before: backend_library::SemanticLinkEvidence {
                        confidence: backend_library::SemanticConfidence::Syntactic,
                        source: None,
                    },
                    after: backend_library::SemanticLinkEvidence {
                        confidence: backend_library::SemanticConfidence::Compiler,
                        source: None,
                    },
                }]
                .into_boxed_slice(),
            }]
            .into_boxed_slice(),
        )),
    );

    let json: serde_json::Value = serde_json::from_str(&run_json(&reply)).expect("CLI JSON");
    let link = &json["reply"]["data"]["data"][0]["links"][0];
    assert_eq!(link["change"], "evidence-changed", "{json}");
    assert_eq!(link["relation"], "calls");
    assert_eq!(link["before"]["confidence"], "syntactic");
    assert_eq!(link["after"]["confidence"], "compiler");

    let human = format_human(&reply);
    assert!(human.contains("\"evidence-changed\""));
    assert!(human.contains("\"compiler\""));
}

#[test]
fn parser_admits_a_basis_only_against_the_daemon_root() {
    let basis = backend_library::view_state_root(&[]);
    let encoded = encode_id(basis.as_bytes());
    let command = parse_with_basis(
        &[
            "search".to_owned(),
            "Thing".to_owned(),
            "--basis".to_owned(),
            encoded.clone(),
        ],
        Some(basis),
    )
    .expect("admitted basis");
    assert!(matches!(command, Command::Search(query) if query.basis() == basis));
    assert!(
        parse_with_basis(
            &[
                "search".to_owned(),
                "Thing".to_owned(),
                "--basis".to_owned(),
                encoded
            ],
            Some(backend_library::view_state_root(&[(
                "different".to_owned(),
                "root".to_owned()
            )])),
        )
        .is_err()
    );
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

#[test]
fn human_output_is_bounded_at_the_presentation_boundary() {
    let reply = ReplyDto::error(3, "x".repeat(MAX_FRAME));
    assert!(format_human(&reply).len() <= MAX_FRAME);
}

#[test]
fn human_add_output_reports_acceptance_without_claiming_completion() {
    let intent = backend_library::Intent::request_package(package_key("pkg")).id();
    let output = format_human(&ReplyDto::new(1, CommandReply::Added(intent)));
    assert!(output.contains("Index request accepted"));
    assert!(output.contains("Check `backend health`"));
    assert!(!output.contains("Indexed"));
}

#[test]
fn human_failure_keeps_the_typed_command_category() {
    let output = format_human(&ReplyDto::new(
        1,
        CommandReply::Failed(backend_library::CommandFailure::NotFound),
    ));
    assert_eq!(output, "Error [not_found]: library record not found\n");
}

#[test]
fn human_health_reports_unavailable_coverage_truthfully() {
    let library = backend_library::Library::new();
    let health = library.execute(Command::Health).expect("health");
    let output = format_human(&ReplyDto::new(1, health));
    assert!(output.starts_with("Unavailable ·"));
    assert!(output.contains("exact unavailable (no_index)"));
    assert!(output.contains("capability ·"));
    assert!(output.contains("unavailable/no_manifest"));
}

#[test]
fn empty_unavailable_snapshot_is_not_reported_as_an_empty_result() {
    let library = backend_library::Library::new();
    let CommandReply::Packages(snapshot) = library.execute(Command::Packages).expect("packages")
    else {
        panic!("packages reply");
    };
    let output = format_human(&ReplyDto::new(
        1,
        CommandReply::Search(ViewSnapshot {
            root: snapshot.root,
            freshness: Freshness::Current,
            next: None,
        }),
    ));
    assert!(output.starts_with("Results unavailable."));
    assert!(!output.contains("No matches"));
}

#[test]
fn human_rows_include_lifecycle_and_source_availability() {
    let basis_root = backend_library::view_state_root(&[]);
    let basis = Basis::new(basis_root, object_version(b"source"));
    let located = Row::new(RowId::Symbol(symbol_key("pkg::ready")), basis, "pkg::ready")
        .with_source(SourceLocation::new("src/lib.rs", 7).expect("source"));
    let missing = Row::new(
        RowId::Symbol(symbol_key("pkg::missing")),
        basis,
        "pkg::missing",
    );
    let view = ViewRoot::new_incomplete(
        view_key(b"rows"),
        basis,
        Frontier::new(basis.branch, basis.log, basis.schema, basis_root, 0),
        vec![located, missing],
        Vec::new(),
    )
    .expect("view");
    let output = format_human(&ReplyDto::new(
        1,
        CommandReply::Search(ViewSnapshot {
            root: view,
            freshness: Freshness::Current,
            next: None,
        }),
    ));
    assert!(output.contains("pkg::ready · ready"));
    assert!(output.contains("source · src/lib.rs:7"));
    assert!(output.contains("pkg::missing · ready"));
    assert!(output.contains("source · not captured"));
}

#[test]
fn human_document_exposes_source_location_and_bounded_excerpt() {
    let document = Document::new(
        symbol_key("pkg::thing"),
        backend_library::view_state_root(&[]),
        [Fragment::Text("documentation".to_owned())],
    )
    .with_location(backend_library::SourceAvailability::Captured(
        SourceLocation::new("src/lib.rs", 4).expect("source"),
    ))
    .with_excerpt(
        SourceExcerpt::captured("fn thing() {}", SourceExcerptExtent::Truncated).expect("excerpt"),
    );
    let output = format_human(&ReplyDto::new(1, CommandReply::Document(document)));
    assert!(output.contains("Source · src/lib.rs:4"));
    assert!(output.contains("Source excerpt · truncated"));
    assert!(output.contains("fn thing() {}"));
}

#[test]
fn human_projection_page_reports_its_terminal_state() {
    let page = backend_library::ProjectionPage {
        snapshot: ViewSnapshot {
            root: root(),
            freshness: Freshness::Current,
            next: None,
        },
        terminal: backend_library::PageTerminal::Complete,
    };
    let output = format_human(&ReplyDto::new(1, CommandReply::ProjectionPage(page)));
    assert!(output.ends_with("Page · complete\n"));
}

#[test]
fn revision_formatter_returns_its_output() {
    let library = backend_library::Library::new();
    let revision = library.execute(Command::Revision).expect("revision");
    let output = format_human(&ReplyDto::new(1, revision));
    assert!(output.starts_with("revision "));
    assert!(output.contains(" · sequence "));
}

#[test]
fn human_outline_emits_every_root_and_its_extent() {
    let basis = backend_library::view_state_root(&[]);
    let first = symbol_key("pkg::First");
    let second = symbol_key("pkg::Second");
    let outline = backend_library::Outline::new(
        package_key("pkg"),
        basis,
        backend_library::OutlineNode {
            symbol: first,
            children: Box::new([]),
        },
    )
    .with_additional_roots([backend_library::OutlineNode {
        symbol: second,
        children: Box::new([]),
    }])
    .with_extent(backend_library::OutlineExtent::Truncated);
    let output = format_human(&ReplyDto::new(1, CommandReply::Outline(outline)));
    assert!(output.contains(&encode_id(first.as_bytes())[..12]));
    assert!(output.contains(&encode_id(second.as_bytes())[..12]));
    assert!(output.ends_with("Outline · truncated\n"));
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
