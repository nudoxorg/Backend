//! Production CLI plan/driver/session/journal with admitted owner DTOs.
//! These controls do not acquire a native compiler or start a local daemon.
use super::*;
use backend_client::{ClientError, CommandTransport};
use backend_library::{
    Command, CommandDto, CommandReply, IndexJobStage, IndexJobTicket, IndexOperationFailureReason,
    IndexOperationObservation, IndexOperationState, IndexOperationStatus, ProductText, ReplyDto,
    SurfaceCommand, SurfaceReply,
};
use std::sync::{Arc, Mutex};

#[derive(Clone, Copy)]
enum Script {
    ProfileProgress,
    ProfileEof,
    LaterFailure,
    Disconnected,
    Active,
    ImmediateFailure,
    WaitDeadline,
    Cancelled,
}
struct Owner {
    script: Script,
    status: Option<IndexOperationStatus>,
    commands: Arc<Mutex<Vec<SurfaceCommand>>>,
}
impl CommandTransport for Owner {
    fn request(&mut self, request: CommandDto) -> Result<ReplyDto, ClientError> {
        let Command::Surface(command) = request.command else {
            panic!("no unrelated probes");
        };
        self.commands
            .lock()
            .expect("commands")
            .push(command.clone());
        let observation = match command {
            SurfaceCommand::IndexOperationStart {
                cargo_target: None,
                operation_key,
                package,
                execution_intent,
            } => {
                assert!(self.status.is_none(), "one durable start only");
                let state = if matches!(self.script, Script::ImmediateFailure) {
                    failed()
                } else if matches!(self.script, Script::LaterFailure) {
                    IndexOperationState::Accepted
                } else {
                    IndexOperationState::Active {
                        ticket: IndexJobTicket::new(
                            std::num::NonZeroU64::MIN,
                            [7; 16],
                            package.clone(),
                        ),
                        stage: IndexJobStage::Compiling,
                    }
                };
                self.status = Some(IndexOperationStatus::new(
                    operation_key,
                    package,
                    execution_intent,
                    state,
                ));
                SurfaceReply::IndexOperationStarted(IndexOperationObservation::Known(
                    self.status.clone().expect("accepted"),
                ))
            }
            SurfaceCommand::IndexOperationStatus { operation_key } => {
                let status = self
                    .status
                    .as_mut()
                    .expect("durable start precedes observation");
                assert_eq!(status.operation_key, operation_key);
                if matches!(self.script, Script::Disconnected) {
                    return Err(ClientError::Disconnected(
                        std::io::ErrorKind::ConnectionReset,
                    ));
                }
                if matches!(self.script, Script::WaitDeadline) {
                    return Err(ClientError::RemoteDeadlineExceeded);
                }
                if matches!(self.script, Script::Cancelled) {
                    return Err(ClientError::RequestCancelled(
                        backend_client::RequestCancellationReason::Explicit,
                    ));
                }
                status.state = match status.state {
                    IndexOperationState::Accepted => IndexOperationState::Active {
                        ticket: IndexJobTicket::new(
                            std::num::NonZeroU64::MIN,
                            [7; 16],
                            status.package.clone(),
                        ),
                        stage: IndexJobStage::Compiling,
                    },
                    _ if matches!(self.script, Script::ProfileProgress | Script::ProfileEof) => {
                        status.state.clone()
                    }
                    _ => failed(),
                };
                SurfaceReply::IndexOperationStatus(IndexOperationObservation::Known(status.clone()))
            }
            SurfaceCommand::IndexProgress {
                ticket,
                after_sequence,
            } => {
                assert!(matches!(self.status.as_ref().map(|status| &status.state),
                    Some(IndexOperationState::Active { ticket: active, .. }) if active == &ticket));
                if matches!(self.script, Script::ProfileEof) {
                    return Err(ClientError::Disconnected(std::io::ErrorKind::UnexpectedEof));
                }
                let events = if matches!(self.script, Script::ProfileProgress) && after_sequence < 3
                {
                    vec![backend_library::IndexJobProgressEvent {
                        ticket: ticket.clone(),
                        sequence: 3,
                        kind: backend_library::IndexJobProgressKind::ProfileStarted {
                            profile: backend_library::SemanticLanguageProfile::from_name(
                                "typescript",
                            )
                            .expect("native profile"),
                            ordinal: 2,
                            total: 3,
                        },
                    }]
                    .into_boxed_slice()
                } else {
                    Box::new([])
                };
                let next_sequence = events.last().map_or(after_sequence, |event| event.sequence);
                SurfaceReply::IndexProgress(backend_library::IndexJobObservation::Pending(
                    backend_library::IndexProgressPage {
                        ticket,
                        stage: IndexJobStage::Compiling,
                        events,
                        next_sequence,
                        truncated: false,
                        has_more: false,
                    },
                ))
            }
            other => panic!("unexpected command: {other:?}"),
        };
        Ok(ReplyDto::new(
            request.request_id,
            CommandReply::Surface(observation),
        ))
    }
}
fn failed() -> IndexOperationState {
    IndexOperationState::Failed {
        reason: IndexOperationFailureReason::Refused,
        detail: ProductText::from_static("exact compiler refusal after asynchronous acceptance"),
        compiler_failure: None,
    }
}
struct Fixture {
    root: PathBuf,
    paths: backend_runtime::WorkspacePaths,
    session: Session,
    commands: Arc<Mutex<Vec<SurfaceCommand>>>,
}
impl Fixture {
    fn new(script: Script) -> Self {
        use std::os::unix::fs::DirBuilderExt as _;
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = PathBuf::from("/tmp").join(format!(
            "niw-{}-{nonce:x}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&root)
            .expect("private caller root");
        let project = root.join("project");
        std::fs::create_dir(&project).expect("real project");
        let paths = backend_runtime::WorkspacePaths::discover(
            Some(project),
            Some(root.join("state")),
            Some(root.join("owner.sock")),
        )
        .expect("exact attachment");
        paths
            .initialize()
            .expect("real caller authority/journal root");
        let commands = Arc::new(Mutex::new(Vec::new()));
        let session = Session::from_transport(
            paths.endpoint(),
            Owner {
                script,
                status: None,
                commands: commands.clone(),
            },
        );
        Self {
            root,
            paths,
            session,
            commands,
        }
    }
    fn run(&mut self, args: &[&str]) -> (Answer, ExitCode) {
        let words = args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>();
        let planned = plan(
            &words,
            &Options::fallback(),
            self.paths.project().to_str().expect("project"),
        )
        .expect("production grammar/plan");
        execute_plan(&mut self.session, &planned, &self.paths).expect("production CLI driver")
    }
    fn claims(&self) -> Vec<serde_json::Value> {
        std::fs::read_dir(self.paths.data().join("client-index-operations-v1"))
            .expect("caller journal")
            .filter_map(|entry| {
                let path = entry.expect("entry").path();
                (path
                    .extension()
                    .is_some_and(|extension| extension == "json"))
                .then(|| {
                    serde_json::from_slice(&std::fs::read(path).expect("claim bytes"))
                        .expect("durable exact claim")
                })
            })
            .collect()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn cli_wait_uses_shared_grammar_without_advertising_product_wait_arguments() {
    for spelling in ["add", "index"] {
        let planned = plan(
            &[spelling, ".", "--wait", "--wait-timeout", "30"].map(str::to_owned),
            &Options::fallback(),
            "/abs/project",
        )
        .expect("CLI wait");
        assert!(planned.wait.is_some());
        assert!(matches!(planned.request, Request::Index(path) if path == "."));
    }
    for args in [
        vec!["add", "--wait-timeout", "30"],
        vec!["add", "--wait", "--wait-timeout", "901"],
        vec!["add", "--wait", "--wait-timeout", "-1"],
        vec!["search", "name", "--wait"],
    ] {
        assert!(
            plan(
                &args.into_iter().map(str::to_owned).collect::<Vec<_>>(),
                &Options::fallback(),
                "/abs/project"
            )
            .is_err()
        );
    }
    let grammar = grammar_for("add").expect("shared grammar");
    assert!(
        !grammar
            .options()
            .iter()
            .any(|option| option.name().starts_with("wait"))
    );
    assert!(
        backend_present::Invocation::from_json(
            grammar,
            serde_json::json!({"path":"/abs/project", "wait":true})
                .as_object()
                .expect("arguments")
        )
        .is_err()
    );
    let help = invoke::help_for(grammar);
    assert!(help.contains("--wait"));
    assert!(help.contains("--wait-timeout"));
    assert!(help.contains("--wait-timeout SECONDS"));
    assert!(help.contains("terminal receipt"));
}

#[test]
fn cli_wait_later_failure_retains_exact_operation_and_returns_nonzero_terminal() {
    let mut fixture = Fixture::new(Script::LaterFailure);
    let (answer, exit) = fixture.run(&["add", "--wait"]);
    assert_eq!(exit, ExitCode::from(render::EXIT_REFUSED));
    let Answer::Product(product) = &answer else {
        panic!("complete product reply");
    };
    let Some(IndexOperationObservation::Known(status)) = product.index_operation() else {
        panic!("original durable receipt");
    };
    assert_eq!(status.state, failed());
    assert_eq!(
        product.fault().expect("visible failure").operand().render(),
        status.operation_key.to_hex()
    );
    let commands = fixture.commands.lock().expect("commands");
    assert_eq!(
        commands
            .iter()
            .filter(|command| !matches!(command, SurfaceCommand::IndexProgress { .. }))
            .count(),
        3
    );
    assert_eq!(
        commands
            .iter()
            .filter(|command| matches!(command, SurfaceCommand::IndexProgress { .. }))
            .count(),
        1
    );
    assert!(
        matches!(&commands[0], SurfaceCommand::IndexOperationStart { operation_key, .. } if *operation_key == status.operation_key)
    );
    assert!(commands[1..].iter().all(|command| match command {
        SurfaceCommand::IndexOperationStatus { operation_key } =>
            *operation_key == status.operation_key,
        SurfaceCommand::IndexProgress { ticket, .. } => ticket.package() == &status.package,
        _ => false,
    }));
    assert!(
        fixture.claims().is_empty(),
        "only terminal acknowledgement removes the exact claim"
    );
    let json = render_admitted_answer(
        &fixture.session,
        &answer,
        &Options::plain(options::Format::Json),
    )
    .expect("bounded CLI JSON");
    let json: serde_json::Value = serde_json::from_str(&json).expect("complete envelope");
    assert_eq!(
        json["index_operation"]["detail"],
        serde_json::to_value(status).expect("exact failure DTO")
    );
    assert_eq!(json["fault"]["slug"], "rejected");
    assert_eq!(json["fault"]["cause"], "refused");
    assert!(json.get("index_poll").is_none());
}

#[test]
fn cli_async_default_and_zero_wait_keep_the_saved_identity_for_resumption() {
    for args in [vec!["add"], vec!["add", "--wait", "--wait-timeout", "0"]] {
        let mut fixture = Fixture::new(Script::Active);
        let (answer, exit) = fixture.run(&args);
        assert_eq!(
            exit,
            if args.len() == 1 {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(crate::index_wait::EXIT_WAIT_INCOMPLETE)
            }
        );
        let Answer::Product(product) = &answer else {
            panic!("product");
        };
        let Some(IndexOperationObservation::Known(status)) = product.index_operation() else {
            panic!("accepted identity");
        };
        assert!(matches!(status.state, IndexOperationState::Active { .. }));
        assert_eq!(
            fixture.commands.lock().expect("commands").len(),
            1,
            "no inferred wait/second mutation"
        );
        let claims = fixture.claims();
        assert_eq!(claims.len(), 1);
        assert_eq!(
            claims[0]["operation_key"],
            serde_json::to_value(status.operation_key).expect("same key")
        );
        if args.len() != 1 {
            let expected_poll = format!("nudox index_progress --operation-key {}", status.operation_key);
            assert!(product.fault().is_none(), "pending durable work is not an endpoint failure");
            assert_eq!(
                product
                    .index_poll_affordance()
                    .expect("same operation poll")
                    .shell()
                    .as_deref(),
                Some(expected_poll.as_str())
            );
            let (resumed, code) = fixture.run(&[
                "index_progress",
                "--operation-key",
                &status.operation_key.to_hex(),
            ]);
            assert_eq!(code, ExitCode::from(render::EXIT_REFUSED));
            assert!(resumed.fault().is_some());
            assert!(fixture.claims().is_empty());
        }
    }
}

#[test]
fn cli_wait_deadline_and_cancellation_report_pending_work_without_endpoint_fault() {
    for (script, expected_reason) in [
        (Script::WaitDeadline, "The CLI wait deadline expired"),
        (Script::Cancelled, "The CLI wait was cancelled (request explicitly cancelled by client)"),
    ] {
        let mut fixture = Fixture::new(script);
        let (answer, exit) = fixture.run(&["add", "--wait", "--wait-timeout", "900"]);
        assert_eq!(exit, ExitCode::from(crate::index_wait::EXIT_WAIT_INCOMPLETE));
        let Answer::Product(product) = &answer else {
            panic!("retained product observation");
        };
        let Some(IndexOperationObservation::Known(status)) = product.index_operation() else {
            panic!("exact durable operation status");
        };
        assert!(matches!(status.state, IndexOperationState::Active { .. }));
        assert!(product.fault().is_none(), "caller wait stop is not an endpoint/index failure");
        assert!(product.cli_context().is_none(),
            "this plan fixture does not run the executable's CLI-context attachment");
        let note = product.note().expect("reason and recovery instruction");
        assert!(note.contains(expected_reason));
        assert!(note.contains("last admitted owner status was active"));
        assert!(note.contains("did not cancel owner work"));
        assert!(note.contains("Do not resubmit `add`"));
        let poll = format!("nudox index_progress --operation-key {}", status.operation_key);
        assert!(note.contains(&poll));
        assert_eq!(product.index_poll_affordance().and_then(|poll| poll.shell()), Some(poll.clone()));
        assert_eq!(fixture.commands.lock().expect("commands").len(), 2);
        assert_eq!(fixture.claims().len(), 1, "a nonterminal wait keeps the durable claim");
        let rendered = render_admitted_answer(
            &fixture.session,
            &answer,
            &Options::plain(options::Format::Human),
        )
        .expect("bounded human response");
        assert!(rendered.contains(expected_reason));
        assert!(rendered.contains(&poll));
        assert!(rendered.contains("Do not resubmit `add`"));
        let json_options = Options::plain(options::Format::Json);
        let json = render_admitted_answer(&fixture.session, &answer, &json_options)
            .expect("bounded typed response");
        let json: serde_json::Value = serde_json::from_str(&json).expect("JSON answer");
        assert!(json["fault"].is_null());
        assert!(
            json["index_poll"].get("shell").is_none(),
            "plain fixture has no observed installed-CLI context for a shell command"
        );
        assert_eq!(
            json["index_poll"]["call"],
            serde_json::json!({
                "name": "backend.index_progress",
                "arguments": {
                    "operation_key": status.operation_key.to_hex(),
                    "detail": json_options.detail().name(),
                },
            }),
            "the exact saved operation remains copyable without inventing CLI authority"
        );
        assert!(json["note"].as_str().is_some_and(|note| note.contains(expected_reason)));
    }
}

#[test]
fn cli_disconnected_wait_preserves_acceptance_and_immediate_failure_is_terminal() {
    let mut fixture = Fixture::new(Script::Disconnected);
    let (answer, exit) = fixture.run(&["add", "--wait"]);
    assert_eq!(
        exit,
        ExitCode::from(crate::index_wait::EXIT_WAIT_INCOMPLETE)
    );
    let Answer::Product(product) = answer else {
        panic!("retained observation");
    };
    let Some(IndexOperationObservation::Known(status)) = product.index_operation() else {
        panic!("original acceptance");
    };
    assert!(matches!(status.state, IndexOperationState::Active { .. }));
    assert_eq!(
        fixture.claims()[0]["operation_key"],
        serde_json::to_value(status.operation_key).expect("retained key")
    );
    assert_eq!(fixture.commands.lock().expect("commands").len(), 2);
    assert_eq!(
        product
            .fault()
            .expect("wait disconnect")
            .affordance()
            .shell()
            .expect("resume"),
        format!(
            "nudox index_progress --operation-key {}",
            status.operation_key
        )
    );
    let mut terminal = Fixture::new(Script::ImmediateFailure);
    let (answer, code) = terminal.run(&["add", "--wait", "--wait-timeout", "0"]);
    assert_eq!(code, ExitCode::from(render::EXIT_REFUSED));
    assert_eq!(
        answer
            .fault()
            .expect("real failure remains visible")
            .cause()
            .slug(),
        CauseSlug::Refused
    );
    assert_eq!(terminal.commands.lock().expect("commands").len(), 1);
    assert!(terminal.claims().is_empty());
}

#[test]
fn cli_explicit_operation_progress_reads_real_ticket_milestone_without_a_second_start() {
    let mut fixture = Fixture::new(Script::ProfileProgress);
    let (accepted, _) = fixture.run(&["add"]);
    let Answer::Product(view) = accepted else {
        panic!("durable acceptance")
    };
    let Some(IndexOperationObservation::Known(status)) = view.index_operation() else {
        panic!("exact key")
    };
    let key = status.operation_key.to_hex();
    let (answer, exit) = fixture.run(&["index_progress", "--operation-key", &key]);
    assert_eq!(exit, ExitCode::SUCCESS);
    let Answer::Product(view) = answer else {
        panic!("composed progress")
    };
    assert_eq!(
        view.index_operation(),
        Some(&IndexOperationObservation::Known(status.clone()))
    );
    assert!(
        matches!(view.index_job(), Some(backend_present::IndexJobProjection::Progress(backend_library::IndexJobObservation::Pending(page))) if page.next_sequence == 3 && page.events.len() == 1)
    );
    let commands = fixture.commands.lock().expect("commands");
    assert!(
        matches!(commands.as_slice(), [SurfaceCommand::IndexOperationStart { .. }, SurfaceCommand::IndexOperationStatus { operation_key }, SurfaceCommand::IndexProgress { ticket, after_sequence: 0 }]
        if *operation_key == status.operation_key && ticket.package() == &status.package)
    );
    assert_eq!(fixture.claims().len(), 1);
    assert!(
        fixture.session.request_context().is_none(),
        "original session binding restored"
    );
}

#[test]
fn cli_ticket_progress_eof_preserves_key_journal_and_original_session_scope() {
    let mut fixture = Fixture::new(Script::ProfileEof);
    let (accepted, _) = fixture.run(&["add"]);
    let Answer::Product(view) = accepted else {
        panic!("durable acceptance")
    };
    let Some(IndexOperationObservation::Known(status)) = view.index_operation() else {
        panic!("exact key")
    };
    let key = status.operation_key.to_hex();
    let args = vec![
        "index_progress".to_owned(),
        "--operation-key".to_owned(),
        key.clone(),
    ];
    let planned = plan(
        &args,
        &Options::fallback(),
        fixture.paths.project().to_str().expect("project"),
    )
    .expect("real CLI grammar");
    let fault = execute_plan(&mut fixture.session, &planned, &fixture.paths)
        .expect_err("secondary read EOF is unproven progress");
    assert_eq!(fault.operand(), &backend_present::Operand::Text(key));
    assert_eq!(fixture.claims().len(), 1);
    assert_eq!(
        fixture
            .commands
            .lock()
            .expect("commands")
            .iter()
            .filter(|command| matches!(command, SurfaceCommand::IndexOperationStart { .. }))
            .count(),
        1
    );
    assert!(
        fixture.session.request_context().is_none(),
        "binding restored even on secondary EOF"
    );
}
