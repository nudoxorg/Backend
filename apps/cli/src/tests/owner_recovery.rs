//! Production replacement policy and caller journal. Scripted owners below
//! prove dispatch/identity, not native publication. The socket control uses
//! the actual private endpoint/authentication/frame path for replacement.
use super::*;
use backend_client::CommandTransport;
use backend_library::{
    Command, CommandDto, CommandFailure, CommandReply, CompileExecutionIntent,
    IndexOperationObservation, IndexOperationState, IndexOperationStatus, PackageReference,
    ReplyDto,
};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::io::ErrorKind;
use std::rc::Rc;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};

struct Transport(Box<dyn FnMut(CommandDto) -> Result<ReplyDto, ClientError> + Send>);
impl CommandTransport for Transport {
    fn request(&mut self, request: CommandDto) -> Result<ReplyDto, ClientError> {
        (self.0)(request)
    }
}
fn session(
    endpoint: &std::path::Path,
    f: impl FnMut(CommandDto) -> Result<ReplyDto, ClientError> + Send + 'static,
) -> Session {
    Session::from_transport(endpoint, Transport(Box::new(f)))
}
fn reset() -> ClientError {
    ClientError::Disconnected(ErrorKind::ConnectionReset)
}

struct Fixture {
    root: std::path::PathBuf,
    paths: WorkspacePaths,
    package: PackageReference,
}
impl Fixture {
    fn new() -> Self {
        use std::os::unix::fs::DirBuilderExt as _;
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::path::PathBuf::from(format!(
            "/tmp/nor-{}-{nonce:x}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&root)
            .expect("private boundary");
        let project = root.join("project");
        std::fs::create_dir(&project).expect("real project operand");
        let paths = WorkspacePaths::discover(
            Some(project.clone()),
            Some(root.join("state")),
            Some(root.join("owner.sock")),
        )
        .expect("captured attachment");
        paths.initialize().expect("private authority and state");
        let package = PackageReference::parse(project.to_str().expect("UTF8")).expect("package");
        Self {
            root,
            paths,
            package,
        }
    }
    fn claims(&self) -> Vec<serde_json::Value> {
        let directory = self.paths.data().join("client-index-operations-v1");
        if !directory.exists() {
            return Vec::new();
        }
        std::fs::read_dir(directory)
            .expect("journal")
            .map(|entry| entry.expect("entry").path())
            .filter(|path| {
                path.extension()
                    .is_some_and(|extension| extension == "json")
            })
            .map(|path| {
                serde_json::from_slice(&std::fs::read(path).expect("claim"))
                    .expect("typed claim JSON")
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
fn owner_replacement_authenticates_private_endpoint_before_repeating_health() {
    use backend_local_service::{EmbeddedLocalService, ProcessConfig};
    use std::os::unix::fs::PermissionsExt as _;
    let fixture = Fixture::new();
    let config = ProcessConfig::parse([
        "--workspace".to_owned(),
        fixture.paths.data().to_string_lossy().into_owned(),
        "--endpoint".to_owned(),
        fixture.paths.endpoint().to_string_lossy().into_owned(),
        "--profile".to_owned(),
        "builtin-echo".to_owned(),
        "--idle-timeout-ms".to_owned(),
        "0".to_owned(),
    ])
    .expect("parse real builtin echo owner config");
    let owner = EmbeddedLocalService::start(config).expect("start admitted local owner");
    std::fs::set_permissions(
        fixture.paths.endpoint(),
        std::fs::Permissions::from_mode(0o600),
    )
    .expect("admitted endpoint mode");
    let initial = Arc::new(AtomicUsize::new(0));
    let seen = initial.clone();
    let mut original = session(fixture.paths.endpoint(), move |request| {
        assert_eq!(request.command, Command::Health);
        seen.fetch_add(1, Ordering::Relaxed);
        Err(reset())
    });
    let mut recovery = OwnerRecovery::new(&fixture.paths, true, RequestContext::new());
    let report = recovery
        .read(&mut original, RecoverableRead::Health, Session::health)
        .expect("authenticated health recovery");
    assert_eq!(initial.load(Ordering::Relaxed), 1);
    assert_eq!(report.row_count(), 0);
    assert_eq!(owner.close().expect("stop admitted owner").frames, 1);
    assert!(recovery.used);
}

#[test]
fn ambiguous_journal_start_reconciles_original_tuple_without_new_add_identity() {
    for state in [
        Some(IndexOperationState::Accepted),
        Some(IndexOperationState::Failed {
            reason: backend_library::IndexOperationFailureReason::Refused,
            detail: backend_library::ProductText::from_static("original compiler refusal"),
            compiler_failure: None,
        }),
        None,
    ] {
        let fixture = Fixture::new();
        let commands = Arc::new(Mutex::new(Vec::<SurfaceCommand>::new()));
        let start = Arc::new(Mutex::new(None::<SurfaceCommand>));
        let old_commands = commands.clone();
        let old_start = start.clone();
        let slow_send = state.is_none();
        let mut original = session(fixture.paths.endpoint(), move |request| {
            let Command::Surface(command @ SurfaceCommand::IndexOperationStart { .. }) =
                request.command
            else {
                panic!("only the saved start is submitted");
            };
            old_commands.lock().expect("commands").push(command.clone());
            *old_start.lock().expect("saved identity") = Some(command);
            if slow_send {
                std::thread::sleep(std::time::Duration::from_secs(23));
            }
            Err(reset()) // Delivery was ambiguous; no outer Add retry.
        });
        let connects = Arc::new(AtomicUsize::new(0));
        let observed_connects = connects.clone();
        let next_commands = commands.clone();
        let next_start = start.clone();
        let endpoint = fixture.paths.endpoint().to_path_buf();
        let expected_state = state.clone();
        let expired_startup = RequestContext::new();
        assert_eq!(
            expired_startup.read_deadline(std::time::Duration::ZERO),
            Err(ClientError::RemoteDeadlineExceeded)
        );
        let startup = expired_startup.clone();
        let mut recovery =
            OwnerRecovery::from_connector(fixture.paths.clone(), expired_startup, move |context| {
                assert!(
                    !context.same_request(&startup),
                    "expired startup cannot become the mutation reconciliation scope"
                );
                let remaining = context
                    .remaining_read_budget()
                    .expect("retained original total")
                    .expect("bounded start before send");
                if slow_send {
                    assert!(
                        remaining
                            <= backend_client::MAX_INDEX_OPERATION_WAIT
                                - std::time::Duration::from_secs(23),
                        "reconnect must not mint a fresh900s after the actual23s send"
                    );
                    assert!(
                        remaining > backend_runtime::COMMAND_STARTUP_TIMEOUT,
                        "start scope uses mutation total rather than expired/short startup budget"
                    );
                }
                observed_connects.fetch_add(1, Ordering::Relaxed);
                let commands = next_commands.clone();
                let start = next_start.clone();
                let state = state.clone();
                let mut replacement = session(&endpoint, move |request| {
                    let Command::Surface(command) = request.command else {
                        panic!("no unrelated command");
                    };
                    commands.lock().expect("commands").push(command.clone());
                    let saved = start
                        .lock()
                        .expect("saved start")
                        .clone()
                        .expect("original identity before replacement");
                    let SurfaceCommand::IndexOperationStart {
                        cargo_target: None,
                        operation_key,
                        package,
                        execution_intent,
                    } = &saved
                    else {
                        panic!("typed start");
                    };
                    let reply = match command {
                        SurfaceCommand::IndexOperationStatus {
                            operation_key: observed,
                        } => {
                            assert_eq!(observed, *operation_key);
                            SurfaceReply::IndexOperationStatus(match &state {
                                Some(state) => {
                                    IndexOperationObservation::Known(IndexOperationStatus::new(
                                        *operation_key,
                                        package.clone(),
                                        *execution_intent,
                                        state.clone(),
                                    ))
                                }
                                None => IndexOperationObservation::Unknown {
                                    operation_key: *operation_key,
                                },
                            })
                        }
                        SurfaceCommand::IndexOperationStart { .. } => {
                            assert!(state.is_none(), "known evidence never resubmits Start");
                            assert_eq!(
                                command, saved,
                                "unknown repeats every original field under the same key"
                            );
                            SurfaceReply::IndexOperationStarted(IndexOperationObservation::Known(
                                IndexOperationStatus::new(
                                    *operation_key,
                                    package.clone(),
                                    *execution_intent,
                                    IndexOperationState::Accepted,
                                ),
                            ))
                        }
                        other => panic!("unrelated command {other:?}"),
                    };
                    Ok(ReplyDto::new(
                        request.request_id,
                        CommandReply::Surface(reply),
                    ))
                });
                replacement
                    .set_request_context(Some(context))
                    .expect("replacement binds same request scope");
                Ok(replacement)
            });
        let answer = crate::run::execute_with_recovery(
            &mut original,
            &backend_present::Request::Index(
                fixture.paths.project().to_string_lossy().into_owned(),
            ),
            &fixture.paths,
            &mut recovery,
        )
        .expect("production CLI driver reconciles this same invocation");
        let backend_present::Answer::Product(product) = answer else {
            panic!("durable product answer");
        };
        let Some(IndexOperationObservation::Known(status)) = product.index_operation() else {
            panic!("exact admitted result");
        };
        let saved = start.lock().expect("start").clone().expect("original");
        let SurfaceCommand::IndexOperationStart {
            cargo_target: None,
            operation_key,
            package,
            execution_intent,
        } = saved
        else {
            panic!("typed");
        };
        assert_eq!(
            (
                status.operation_key,
                status.package.clone(),
                status.execution_intent
            ),
            (operation_key, package.clone(), execution_intent)
        );
        assert_eq!(
            status.request_digest,
            backend_library::index_operation_request_digest(&package, execution_intent)
        );
        assert_eq!(
            status.state,
            expected_state
                .clone()
                .unwrap_or(IndexOperationState::Accepted)
        );
        assert_eq!(connects.load(Ordering::Relaxed), 1);
        assert_eq!(
            commands.lock().expect("commands").len(),
            if expected_state.is_none() { 3 } else { 2 }
        );
        let claims = fixture.claims();
        if matches!(status.state, IndexOperationState::Failed { .. }) {
            assert!(claims.is_empty(), "terminal exact claim acknowledged");
        } else {
            assert_eq!(claims.len(), 1);
            assert_eq!(
                claims[0]["operation_key"],
                serde_json::to_value(operation_key).expect("key")
            );
        }
    }
}

#[test]
fn owner_replacement_excludes_pinned_timeout_cancel_protocol_and_nonindex_mutation() {
    let fixture = Fixture::new();
    for error in [
        ClientError::Disconnected(ErrorKind::TimedOut),
        ClientError::Disconnected(ErrorKind::WouldBlock),
        ClientError::RemoteDeadlineExceeded,
        ClientError::QueryPreparationDisconnected {
            basis: backend_library::view_state_root(&[]).into(),
            kind: ErrorKind::ConnectionReset,
        },
        ClientError::RequestCancelled(backend_client::RequestCancellationReason::Explicit),
        ClientError::Protocol("mismatched owner proof".to_owned()),
        ClientError::Io("authority refused".to_owned()),
    ] {
        let expected = error.clone();
        let mut original = session(std::path::Path::new("/unused"), move |_| Err(error.clone()));
        let mut recovery =
            OwnerRecovery::from_connector(fixture.paths.clone(), RequestContext::new(), |_| {
                panic!("excluded error must not authenticate/replay")
            });
        assert_eq!(
            recovery.read(&mut original, RecoverableRead::Health, Session::health),
            Err(expected)
        );
        assert!(!recovery.used);
    }
    let mut original = session(std::path::Path::new("/unused"), |_| Err(reset()));
    let mut recovery =
        OwnerRecovery::from_connector(fixture.paths.clone(), RequestContext::new(), |_| {
            panic!("non-index mutation must not reconnect")
        });
    let package = PackageReference::parse("/actual/project").expect("package");
    let command = SurfaceCommand::IndexStart {
        cargo_target: None,
        package,
        execution_intent: CompileExecutionIntent::Interactive,
    };
    assert_eq!(
        recovery.surface(&mut original, command, None),
        Err(ClientError::MutationOutcomeUnknown(
            ErrorKind::ConnectionReset
        ))
    );
    assert!(!recovery.used);

    for reason in [
        backend_client::RequestCancellationReason::Explicit,
        backend_client::RequestCancellationReason::ClientDisconnected,
        backend_client::RequestCancellationReason::Shutdown,
    ] {
        let context = RequestContext::new();
        context.cancel(reason);
        let mut original = session(fixture.paths.endpoint(), |_| {
            panic!("cancel before original exchange")
        });
        let mut recovery =
            OwnerRecovery::from_connector(fixture.paths.clone(), RequestContext::new(), |_| {
                panic!("cancel cannot reconnect")
            });
        let key = backend_library::IndexOperationKey::from_bytes([29; 32]).expect("key");
        assert_eq!(
            recovery.surface(
                &mut original,
                SurfaceCommand::IndexOperationStatus { operation_key: key },
                Some(&context)
            ),
            Err(ClientError::RequestCancelled(reason))
        );
        assert!(!recovery.used);
    }
}

#[test]
fn journal_start_capability_refuses_foreign_connection_attachment_before_send() {
    let fixture = Fixture::new();
    let foreign = Fixture::new();
    let mut original = session(fixture.paths.endpoint(), |_| {
        panic!("foreign recovery attachment cannot send the saved start")
    });
    let mut recovery =
        OwnerRecovery::from_connector(foreign.paths.clone(), RequestContext::new(), |_| {
            panic!("foreign recovery attachment cannot authenticate a replacement")
        });
    let error = backend_client::index_with_journal_recovery(
        &fixture.paths,
        fixture.package.clone(),
        CompileExecutionIntent::Interactive,
        |dispatch| recovery.journal(&mut original, dispatch, None),
    )
    .expect_err("held request and caller connection attachment must match");
    assert!(matches!(error, ClientError::Io(_)));
    assert!(
        error
            .to_string()
            .contains("held start does not match the replacement connection attachment")
    );
    assert!(!recovery.used);
    let claims = fixture.claims();
    assert_eq!(
        claims.len(),
        1,
        "original claim remains saved without a send"
    );
    assert_eq!(
        claims[0]["package"],
        serde_json::to_value(fixture.package.clone()).expect("original package")
    );
    assert!(
        foreign.claims().is_empty(),
        "foreign attachment is not mutated"
    );
}

#[test]
fn journal_start_retains_admitted_command_refusal_without_delivery_uncertainty() {
    let fixture = Fixture::new();
    let sends = Arc::new(AtomicUsize::new(0));
    let sent = sends.clone();
    let mut original = session(fixture.paths.endpoint(), move |request| {
        assert!(matches!(
            request.command,
            Command::Surface(SurfaceCommand::IndexOperationStart { .. })
        ));
        sent.fetch_add(1, Ordering::Relaxed);
        Ok(ReplyDto::new(
            request.request_id,
            CommandReply::Failed(CommandFailure::MutationRequiresOwner),
        ))
    });
    let mut recovery =
        OwnerRecovery::from_connector(fixture.paths.clone(), RequestContext::new(), |_| {
            panic!("refusal cannot reconnect")
        });
    let error = backend_client::index_with_journal_recovery(
        &fixture.paths,
        fixture.package.clone(),
        CompileExecutionIntent::Interactive,
        |dispatch| recovery.journal(&mut original, dispatch, None),
    )
    .expect_err("actual command rejection is retained");
    assert_eq!(
        error,
        ClientError::CommandFailed(CommandFailure::MutationRequiresOwner),
        "the typed admitted command cause must remain exact",
    );
    assert_eq!(sends.load(Ordering::Relaxed), 1);
    assert!(!recovery.used);
    assert_eq!(
        fixture.claims().len(),
        1,
        "no durable terminal receipt was invented"
    );
}

#[test]
fn journal_start_legacy_error_keeps_delivery_uncertainty_and_original_claim() {
    let fixture = Fixture::new();
    let sends = Arc::new(AtomicUsize::new(0));
    let sent = sends.clone();
    let mut original = session(fixture.paths.endpoint(), move |request| {
        assert!(matches!(
            request.command,
            Command::Surface(SurfaceCommand::IndexOperationStart { .. })
        ));
        sent.fetch_add(1, Ordering::Relaxed);
        Ok(ReplyDto::error(request.request_id, "legacy failure"))
    });
    let mut recovery =
        OwnerRecovery::from_connector(fixture.paths.clone(), RequestContext::new(), |_| {
            panic!("a legacy protocol failure cannot authenticate a replacement")
        });
    let error = backend_client::index_with_journal_recovery(
        &fixture.paths,
        fixture.package.clone(),
        CompileExecutionIntent::Interactive,
        |dispatch| recovery.journal(&mut original, dispatch, None),
    )
    .expect_err("an untyped legacy error cannot attest a refused mutation");
    let claims = fixture.claims();
    assert_eq!(
        claims.len(),
        1,
        "the original nonterminal claim is retained"
    );
    let ClientError::IndexOperationDeliveryUnknown {
        operation_key,
        cause,
    } = error
    else {
        panic!("an untyped failure cannot become an admitted command refusal");
    };
    assert_eq!(
        serde_json::to_value(operation_key).expect("operation key"),
        claims[0]["operation_key"],
        "uncertainty retains the exact saved request identity",
    );
    assert_eq!(*cause, ClientError::Protocol("legacy failure".to_owned()));
    assert_eq!(sends.load(Ordering::Relaxed), 1);
    assert!(!recovery.used);
}

#[test]
fn replacement_failure_keeps_saved_claim_and_read_recovery_keeps_original_deadline() {
    let fixture = Fixture::new();
    let commands = Arc::new(AtomicUsize::new(0));
    let submitted = commands.clone();
    let mut original = session(fixture.paths.endpoint(), move |_| {
        submitted.fetch_add(1, Ordering::Relaxed);
        Err(reset())
    });
    let mut recovery =
        OwnerRecovery::from_connector(fixture.paths.clone(), RequestContext::new(), |_| {
            Err(ClientError::Io("replacement authority refused".to_owned()))
        });
    let error = backend_client::index_with_journal_recovery(
        &fixture.paths,
        fixture.package.clone(),
        CompileExecutionIntent::Interactive,
        |dispatch| recovery.journal(&mut original, dispatch, None),
    )
    .expect_err("replacement refused without admitting an outcome");
    assert_eq!(commands.load(Ordering::Relaxed), 1);
    assert_eq!(
        fixture.claims().len(),
        1,
        "no new key/claim removal after failed authentication"
    );
    let ClientError::IndexOperationDeliveryUnknown {
        operation_key,
        ref cause,
    } = error
    else {
        panic!("unknown delivery retains original key and exact cause");
    };
    assert_eq!(
        cause.as_ref(),
        &ClientError::Io("replacement authority refused".to_owned())
    );
    assert_eq!(
        fixture.claims()[0]["operation_key"],
        serde_json::to_value(operation_key).expect("key")
    );
    let fault = backend_present::Fault::from_client_error(&error, backend_present::Operand::Whole);
    assert_eq!(
        fault.affordance(),
        &backend_present::Affordance::PollIndexOperation { operation_key }
    );
    assert!(
        fault
            .cause()
            .sentence()
            .contains("replacement authority refused")
    );
    assert_eq!(
        fault
            .affordance()
            .tool_call()
            .expect("exact copied progress")["arguments"]["operation_key"],
        serde_json::to_value(operation_key).expect("key")
    );
    assert!(recovery.used);

    let wait = RequestContext::new();
    let deadline = wait
        .read_deadline(std::time::Duration::from_secs(2))
        .expect("original wait deadline");
    let captured = wait.clone();
    let connects = Arc::new(AtomicUsize::new(0));
    let connected = connects.clone();
    let mut original = session(fixture.paths.endpoint(), |_| Err(reset()));
    let mut recovery =
        OwnerRecovery::from_connector(fixture.paths.clone(), RequestContext::new(), move |scope| {
            assert!(scope.same_request(&captured));
            assert_eq!(
                scope
                    .read_deadline(std::time::Duration::from_secs(90))
                    .expect("same deadline"),
                deadline
            );
            connected.fetch_add(1, Ordering::Relaxed);
            Ok(session(std::path::Path::new("/replacement"), |_| {
                Err(reset())
            }))
        });
    let key = backend_library::IndexOperationKey::from_bytes([19; 32]).expect("key");
    let command = SurfaceCommand::IndexOperationStatus { operation_key: key };
    assert_eq!(
        recovery.surface(&mut original, command.clone(), Some(&wait)),
        Err(ClientError::RemoteDeadlineExceeded)
    );
    let attempts = connects.load(Ordering::Relaxed);
    assert!(
        attempts > 2,
        "saved-key reads tolerate repeated peer retirement"
    );
    assert_eq!(
        recovery.surface(&mut original, command, Some(&wait)),
        Err(ClientError::RemoteDeadlineExceeded)
    );
    assert_eq!(
        connects.load(Ordering::Relaxed),
        attempts,
        "expired scope cannot reconnect"
    );
    assert!(std::time::Instant::now() >= deadline);
    assert_eq!(
        wait.read_deadline(std::time::Duration::from_secs(900)),
        Err(ClientError::RemoteDeadlineExceeded),
        "reconnection never renews the expired original deadline"
    );
    wait.finish();

    let fixture = Fixture::new();
    let commands = Arc::new(AtomicUsize::new(0));
    let sent = commands.clone();
    let sent_scope = Arc::new(Mutex::new(None::<RequestContext>));
    let transport_scope = sent_scope.clone();
    let mut original = session(fixture.paths.endpoint(), move |request| {
        assert!(matches!(
            request.command,
            Command::Surface(SurfaceCommand::IndexOperationStart { .. })
        ));
        sent.fetch_add(1, Ordering::Relaxed);
        let scope = transport_scope
            .lock()
            .expect("original scope")
            .clone()
            .expect("scope exists before original send");
        let remaining = scope
            .remaining_read_budget()
            .expect("original send admitted before deadline")
            .expect("original send retains the supplied total");
        std::thread::sleep(remaining + std::time::Duration::from_millis(10));
        Err(reset())
    });
    let mut recovery =
        OwnerRecovery::from_connector(fixture.paths.clone(), RequestContext::new(), |_| {
            panic!("exhausted original total cannot authenticate another owner")
        });
    let error = backend_client::index_with_journal_recovery(
        &fixture.paths,
        fixture.package.clone(),
        CompileExecutionIntent::Interactive,
        |dispatch| {
            // The journal has already persisted its claim. Filesystem setup
            // cannot consume this test's explicit original exchange budget.
            let total = RequestContext::new();
            total
                .read_deadline(std::time::Duration::from_millis(250))
                .expect("explicit original total");
            *sent_scope.lock().expect("install scope") = Some(total.clone());
            let result = recovery.journal(&mut original, dispatch, Some(&total));
            total.finish();
            result
        },
    )
    .expect_err("original caller total ended during first send");
    let ClientError::IndexOperationDeliveryUnknown {
        operation_key,
        cause,
    } = error
    else {
        panic!("original saved delivery identity retained");
    };
    assert_eq!(*cause, ClientError::RemoteDeadlineExceeded);
    assert_eq!(commands.load(Ordering::Relaxed), 1);
    assert!(!recovery.used);
    assert_eq!(fixture.claims().len(), 1);
    assert_eq!(
        fixture.claims()[0]["operation_key"],
        serde_json::to_value(operation_key).expect("original saved key")
    );
}

/// Scripted Session connectors exercise cancellation of the original request.
/// The same saved key and deadline survive retries; no mutation is dispatched.
#[test]
fn repeated_saved_key_reobservation_stops_on_original_cancellation_without_start() {
    let fixture = Fixture::new();
    let key = backend_library::IndexOperationKey::from_bytes([59; 32]).expect("key");
    let command = SurfaceCommand::IndexOperationStatus { operation_key: key };
    let reads = Arc::new(AtomicUsize::new(0));
    let seen = reads.clone();
    let expected = command.clone();
    let mut original = session(fixture.paths.endpoint(), move |request| {
        assert_eq!(request.command, Command::Surface(expected.clone()));
        seen.fetch_add(1, Ordering::Relaxed);
        Err(reset())
    });
    let wait = RequestContext::new();
    let deadline = wait
        .read_deadline(std::time::Duration::from_secs(10))
        .expect("original deadline");
    let captured = wait.clone();
    let connects = Arc::new(AtomicUsize::new(0));
    let connected = connects.clone();
    let mut recovery =
        OwnerRecovery::from_connector(fixture.paths.clone(), RequestContext::new(), move |scope| {
            assert!(scope.same_request(&captured));
            assert_eq!(
                scope
                    .read_deadline(std::time::Duration::from_secs(900))
                    .expect("same deadline"),
                deadline
            );
            if connected.fetch_add(1, Ordering::Relaxed) == 2 {
                scope.cancel(backend_client::RequestCancellationReason::Explicit);
                return Err(ClientError::RequestCancelled(
                    backend_client::RequestCancellationReason::Explicit,
                ));
            }
            let reads = reads.clone();
            let expected = SurfaceCommand::IndexOperationStatus { operation_key: key };
            Ok(session(
                std::path::Path::new("/replacement"),
                move |request| {
                    assert_eq!(request.command, Command::Surface(expected.clone()));
                    reads.fetch_add(1, Ordering::Relaxed);
                    Err(reset())
                },
            ))
        });
    assert_eq!(
        recovery.surface(&mut original, command.clone(), Some(&wait)),
        Err(ClientError::RequestCancelled(
            backend_client::RequestCancellationReason::Explicit
        ))
    );
    assert_eq!(connects.load(Ordering::Relaxed), 3);
    assert_eq!(
        recovery.surface(&mut original, command, Some(&wait)),
        Err(ClientError::RequestCancelled(
            backend_client::RequestCancellationReason::Explicit
        ))
    );
    assert_eq!(
        connects.load(Ordering::Relaxed),
        3,
        "cancel cannot authenticate again"
    );
}

/// Real private Unix transport and the production 256-frame server boundary.
/// Scripted Accepted receipts prove recovery/identity, not compiler publication.
#[test]
fn saved_key_read_survives_three_real_frame_budget_retirements_without_new_start() {
    saved_operation_reads_survive_real_frame_retirements(false);
}

#[test]
fn composed_ticket_progress_survives_real_frame_retirements_without_new_start() {
    saved_operation_reads_survive_real_frame_retirements(true);
}

fn saved_operation_reads_survive_real_frame_retirements(with_profiles: bool) {
    use backend_local_service::protocol::{
        EngineRequest, EngineStatus, FrameLimits, ProtocolError,
    };
    use backend_local_service::service::{LocaldService, OwnerService};
    use std::os::unix::fs::PermissionsExt as _;
    struct ReceiptOwner {
        library: backend_library::Library,
        package: PackageReference,
        key: Option<backend_library::IndexOperationKey>,
        starts: usize,
        reads: usize,
        ticket_reads: usize,
        with_profiles: bool,
    }
    impl ReceiptOwner {
        fn ticket(&self) -> backend_library::IndexJobTicket {
            backend_library::IndexJobTicket::new(
                std::num::NonZeroU64::new(2).expect("nonzero fixture ticket"),
                [7; 16],
                self.package.clone(),
            )
        }
    }
    impl OwnerService for ReceiptOwner {
        fn command(&mut self, body: &[u8]) -> Result<Vec<u8>, ProtocolError> {
            let request = CommandDto::decode_for_owner(body, self.library.cursor())
                .expect("actual request admission");
            if let Command::Surface(SurfaceCommand::IndexProgress {
                ticket,
                after_sequence,
            }) = &request.command
            {
                assert!(
                    self.with_profiles,
                    "status-only control never requests a ticket"
                );
                assert_eq!(
                    *ticket,
                    self.ticket(),
                    "the exact admitted ticket survives reconnect"
                );
                assert_eq!(
                    *after_sequence, 0,
                    "empty fixture history retains its cursor"
                );
                self.reads += 1;
                self.ticket_reads += 1;
                let page = backend_library::IndexProgressPage {
                    ticket: ticket.clone(),
                    stage: backend_library::IndexJobStage::Compiling,
                    events: Box::new([]),
                    next_sequence: 0,
                    truncated: false,
                    has_more: false,
                };
                return Ok(serde_json::to_vec(&ReplyDto::new(
                    request.request_id,
                    CommandReply::Surface(SurfaceReply::IndexProgress(
                        backend_library::IndexJobObservation::Pending(page),
                    )),
                ))
                .expect("exact ticket progress encoding"));
            }
            let (key, started) = match request.command {
                Command::Surface(SurfaceCommand::IndexOperationStart {
                    cargo_target: None,
                    operation_key,
                    package,
                    execution_intent,
                }) => {
                    assert_eq!(package, self.package);
                    assert_eq!(execution_intent, CompileExecutionIntent::Interactive);
                    assert!(
                        self.key.replace(operation_key).is_none(),
                        "no duplicate mutation"
                    );
                    self.starts += 1;
                    (operation_key, true)
                }
                Command::Surface(SurfaceCommand::IndexOperationStatus { operation_key }) => {
                    assert_eq!(
                        Some(operation_key),
                        self.key,
                        "every read retains original saved key"
                    );
                    self.reads += 1;
                    (operation_key, false)
                }
                command => panic!("unexpected command during saved-key read: {command:?}"),
            };
            let observation = IndexOperationObservation::Known(IndexOperationStatus::new(
                key,
                self.package.clone(),
                CompileExecutionIntent::Interactive,
                if self.with_profiles {
                    IndexOperationState::Active {
                        ticket: self.ticket(),
                        stage: backend_library::IndexJobStage::Compiling,
                    }
                } else {
                    IndexOperationState::Accepted
                },
            ));
            let reply = if started {
                SurfaceReply::IndexOperationStarted(observation)
            } else {
                SurfaceReply::IndexOperationStatus(observation)
            };
            Ok(serde_json::to_vec(&ReplyDto::new(
                request.request_id,
                CommandReply::Surface(reply),
            ))
            .expect("typed receipt encoding"))
        }
        fn engine(&mut self, _: u64, _: EngineRequest) -> Result<EngineStatus, ProtocolError> {
            panic!("receipt observation cannot issue engine mutations")
        }
        fn serve_one(&mut self) -> bool {
            false
        }
        fn close(&mut self) {}
    }
    let fixture = Fixture::new();
    let listener =
        std::os::unix::net::UnixListener::bind(fixture.paths.endpoint()).expect("private endpoint");
    std::fs::set_permissions(
        fixture.paths.endpoint(),
        std::fs::Permissions::from_mode(0o600),
    )
    .expect("private socket");
    let frames = FrameLimits::default().max_frames_per_connection;
    assert_eq!(
        frames, 256,
        "exercise actual default retirement, not a reduced test cap"
    );
    listener
        .set_nonblocking(true)
        .expect("bounded accept observation");
    let package = fixture.package.clone();
    let server = std::thread::spawn(move || {
        let accept_deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        let mut service = LocaldService::new(
            ReceiptOwner {
                library: backend_library::Library::new(),
                package,
                key: None,
                starts: 0,
                reads: 0,
                ticket_reads: 0,
                with_profiles,
            },
            FrameLimits::default(),
        )
        .expect("real frame service");
        for _ in 0..4 {
            let (mut stream, _) = loop {
                match listener.accept() {
                    Ok(connection) => break connection,
                    Err(error) if error.kind() == ErrorKind::WouldBlock => {
                        assert!(
                            std::time::Instant::now() < accept_deadline,
                            "expected connection absent"
                        );
                        std::thread::sleep(std::time::Duration::from_millis(1));
                    }
                    Err(error) => panic!("private accept failed: {error}"),
                }
            };
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .expect("bounded fixture read");
            assert_eq!(
                service
                    .serve_stream(&mut stream)
                    .expect("production frame retirement"),
                frames
            );
        }
        (
            service.owner().key,
            service.owner().starts,
            service.owner().reads,
            service.owner().ticket_reads,
        )
    });
    let mut original =
        Session::connect(fixture.paths.endpoint()).expect("real initial authentication");
    let mut recovery = OwnerRecovery::new(&fixture.paths, false, RequestContext::new());
    let started = backend_client::index_with_journal_recovery(
        &fixture.paths,
        fixture.package.clone(),
        CompileExecutionIntent::Interactive,
        |dispatch| recovery.journal(&mut original, dispatch, None),
    )
    .expect("one journaled mutation");
    let SurfaceReply::IndexOperationStarted(IndexOperationObservation::Known(initial)) = started
    else {
        panic!("original accepted operation");
    };
    let key = initial.operation_key;
    let wait = RequestContext::new();
    let deadline = wait
        .read_deadline(std::time::Duration::from_secs(30))
        .expect("original read deadline");
    for index in 1..(frames * 4) {
        // The start consumes frame 1, so frame 257 is a compiler-ticket read.
        // This is the subread that previously escaped keyed-wait recovery.
        let actual = if with_profiles && index % 2 == 0 {
            let composed = backend_client::observe_index_operation_progress(
                IndexOperationObservation::Known(initial.clone()),
                &wait,
                |command| recovery.surface(&mut original, command, Some(&wait)),
            )
            .expect("composed exact-ticket progress across real connection retirement");
            let IndexOperationState::Active { ticket, stage } = &initial.state else {
                panic!("profile control starts with its exact active ticket");
            };
            assert!(matches!(&composed.profile_progress, Some(page)
                if page.ticket == *ticket && page.stage == *stage));
            SurfaceReply::IndexOperationStatus(composed.observation)
        } else {
            recovery
                .surface(
                    &mut original,
                    SurfaceCommand::IndexOperationStatus { operation_key: key },
                    Some(&wait),
                )
                .expect("same-key read across healthy connection retirement")
        };
        assert_eq!(
            actual,
            SurfaceReply::IndexOperationStatus(IndexOperationObservation::Known(initial.clone()))
        );
        assert_eq!(
            wait.read_deadline(std::time::Duration::from_secs(900))
                .expect("unrenewed scope"),
            deadline
        );
    }
    recovery
        .detach(&mut original)
        .expect("retire exact scope binding");
    wait.finish();
    assert_eq!(
        fixture.claims().len(),
        1,
        "nonterminal original claim stays resumable"
    );
    assert_eq!(
        fixture.claims()[0]["operation_key"],
        serde_json::to_value(key).expect("saved key")
    );
    assert_eq!(
        server.join().expect("bounded owner thread"),
        (
            Some(key),
            1,
            frames * 4 - 1,
            if with_profiles {
                (frames * 4 - 1) / 2
            } else {
                0
            }
        )
    );
}

// ---------------------------------------------------------------------------
// Owner loss: classification, convergence and pacing.
// ---------------------------------------------------------------------------

#[test]
fn ticket_progress_recovery_requires_original_scope_and_rejects_protocol_or_stop() {
    for case in [
        "unscoped",
        "protocol",
        "expired",
        "cancelled",
        "read-deadline",
    ] {
        let fixture = Fixture::new();
        let command = SurfaceCommand::IndexProgress {
            ticket: backend_library::IndexJobTicket::new(
                std::num::NonZeroU64::new(2).expect("nonzero fixture ticket"),
                [7; 16],
                fixture.package.clone(),
            ),
            after_sequence: 7,
        };
        let expected = match case {
            "protocol" => ClientError::Protocol("foreign exact-ticket reply".into()),
            "expired" | "read-deadline" => ClientError::RemoteDeadlineExceeded,
            "cancelled" => {
                ClientError::RequestCancelled(backend_client::RequestCancellationReason::Explicit)
            }
            _ => ClientError::Disconnected(ErrorKind::BrokenPipe),
        };
        let response = expected.clone();
        let requested = command.clone();
        let reads = Arc::new(AtomicUsize::new(0));
        let seen = reads.clone();
        let mut original = session(fixture.paths.endpoint(), move |request| {
            assert_eq!(request.command, Command::Surface(requested.clone()));
            seen.fetch_add(1, Ordering::Relaxed);
            Err(response.clone())
        });
        let scope = RequestContext::new();
        let _ = scope.read_deadline(if case == "expired" {
            std::time::Duration::ZERO
        } else {
            backend_client::MAX_INDEX_OPERATION_WAIT
        });
        if case == "cancelled" {
            scope.cancel(backend_client::RequestCancellationReason::Explicit);
        }
        let mut recovery =
            OwnerRecovery::from_connector(fixture.paths.clone(), RequestContext::new(), |_| {
                panic!("missing scope, protocol failure or caller stop cannot reconnect")
            });
        assert_eq!(
            recovery.surface(
                &mut original,
                command,
                if case == "unscoped" {
                    None
                } else {
                    Some(&scope)
                },
            ),
            Err(expected),
            "{case}",
        );
        assert_eq!(
            reads.load(Ordering::Relaxed),
            if matches!(case, "expired" | "cancelled") {
                0
            } else {
                1
            },
            "{case}: an ended original scope cannot send another read",
        );
    }
}

fn lost(kind: ErrorKind) -> ClientError {
    ClientError::Disconnected(kind)
}

fn scope_of(duration: std::time::Duration) -> RequestContext {
    let scope = RequestContext::new();
    scope.read_deadline(duration).expect("fixture scope");
    scope
}

/// A real, typed startup-pending observation: a zero response budget never
/// starts a candidate, so it is the actual value a slow start produces.
fn pending(fixture: &Fixture) -> ClientError {
    let backend_runtime::LocaldStartup::Pending(pending) =
        backend_runtime::start_locald_cancellable(
            &fixture.paths,
            std::time::Duration::ZERO,
            |_| {},
            || true,
        )
        .expect("zero budget cannot fail to start")
    else {
        panic!("zero response budget leaves startup pending");
    };
    ClientError::from(backend_runtime::RuntimeError::StartupPending(pending))
}

fn accepted_status(key: backend_library::IndexOperationKey) -> IndexOperationObservation {
    IndexOperationObservation::Known(IndexOperationStatus::new(
        key,
        PackageReference::parse("/actual/project").expect("package"),
        CompileExecutionIntent::Interactive,
        IndexOperationState::Accepted,
    ))
}

/// A session whose owner answers every receipt read with the same Accepted
/// observation of `key`.
fn receipt_session(key: backend_library::IndexOperationKey) -> Session {
    session(std::path::Path::new("/replacement"), move |request| {
        Ok(ReplyDto::new(
            request.request_id,
            CommandReply::Surface(SurfaceReply::IndexOperationStatus(accepted_status(key))),
        ))
    })
}

/// The dial script shared by the convergence tests: each entry is one dial's
/// outcome, in order, and every dialed path is recorded.
struct Dials {
    script: VecDeque<Result<(), ClientError>>,
    log: Rc<RefCell<Vec<String>>>,
}
impl Dials {
    fn dial(&mut self, path: &std::path::Path) -> Result<Session, ClientError> {
        self.log
            .borrow_mut()
            .push(format!("dial {}", path.display()));
        self.script
            .pop_front()
            .expect("the scripted owner was dialed more often than scripted")
            .map(|()| session(path, |_| Err(reset())))
    }
}

/// The reported race: a killed owner's listener outlives its process for a
/// moment, so dials are accepted and then closed, and only a later dial is
/// refused. Waiting that out converges on exactly one new owner.
#[test]
fn a_dying_listener_is_waited_out_until_refused_then_exactly_one_owner_starts() {
    let fixture = Fixture::new();
    let scope = scope_of(std::time::Duration::from_secs(30));
    let log = Rc::new(RefCell::new(Vec::new()));
    let mut dials = Dials {
        script: VecDeque::from([
            Err(lost(ErrorKind::UnexpectedEof)),
            Err(lost(ErrorKind::ConnectionReset)),
            Err(lost(ErrorKind::ConnectionRefused)),
            Ok(()),
        ]),
        log: log.clone(),
    };
    let ready = fixture.root.join("started.sock");
    let endpoint = fixture.paths.endpoint().to_path_buf();
    let started = std::time::Instant::now();
    let connected = connect_replacement_with(
        &scope,
        &endpoint,
        |path| dials.dial(path),
        || {
            log.borrow_mut().push("bootstrap".to_owned());
            Ok(ready.clone())
        },
    )
    .expect("the owner gap converges");
    assert_eq!(connected.endpoint(), ready);
    let seen = log.borrow().clone();
    let endpoint_text = endpoint.display();
    assert_eq!(
        seen,
        [
            format!("dial {endpoint_text}"),
            format!("dial {endpoint_text}"),
            format!("dial {endpoint_text}"),
            "bootstrap".to_owned(),
            format!("dial {}", ready.display()),
        ],
        "one candidate, started only after a dial was refused"
    );
    assert!(
        started.elapsed() >= std::time::Duration::from_millis(75),
        "25 ms then 50 ms of backoff before the third dial: {:?}",
        started.elapsed()
    );
}

/// The exact R1 sequence: the readiness probe accepted a dying listener, the
/// authenticated dial to it was lost, and the next dial is refused.
#[test]
fn a_ready_owner_that_is_lost_on_the_final_dial_converges_instead_of_failing() {
    let fixture = Fixture::new();
    let scope = scope_of(std::time::Duration::from_secs(30));
    let log = Rc::new(RefCell::new(Vec::new()));
    let mut dials = Dials {
        script: VecDeque::from([
            Err(lost(ErrorKind::ConnectionRefused)), // pre-dial: no listener
            Err(lost(ErrorKind::UnexpectedEof)),     // probe said Ready; dial lost
            Err(lost(ErrorKind::NotFound)),          // the next pre-dial: socket gone
            Ok(()),                                  // the owner that finally started
        ]),
        log: log.clone(),
    };
    let ready = fixture.root.join("started.sock");
    let starts = Rc::new(RefCell::new(0usize));
    let started = starts.clone();
    connect_replacement_with(
        &scope,
        fixture.paths.endpoint(),
        |path| dials.dial(path),
        || {
            *started.borrow_mut() += 1;
            Ok(ready.clone())
        },
    )
    .expect("a lost final dial is retried, not surfaced");
    assert_eq!(*starts.borrow(), 2, "each absent observation may start one");
    assert!(dials.script.is_empty());
}

/// A slow or busy listener is still the owner. It must never authorize a
/// candidate, and it is not a lost connection either.
#[test]
fn slow_listeners_and_non_loss_errors_never_start_an_owner_or_loop() {
    let fixture = Fixture::new();
    for error in [
        lost(ErrorKind::TimedOut),
        lost(ErrorKind::WouldBlock),
        ClientError::Io("authority refused".to_owned()),
        ClientError::Protocol("mismatched owner proof".to_owned()),
    ] {
        let scope = scope_of(std::time::Duration::from_secs(30));
        let log = Rc::new(RefCell::new(Vec::new()));
        let mut dials = Dials {
            script: VecDeque::from([Err(error.clone())]),
            log: log.clone(),
        };
        let result = connect_replacement_with(
            &scope,
            fixture.paths.endpoint(),
            |path| dials.dial(path),
            || panic!("{error:?} must not authorize another owner"),
        );
        assert_eq!(result.err(), Some(error));
        assert_eq!(log.borrow().len(), 1, "no retry");
    }
}

#[test]
fn a_bootstrap_refusal_is_returned_not_retried() {
    let fixture = Fixture::new();
    let scope = scope_of(std::time::Duration::from_secs(30));
    let log = Rc::new(RefCell::new(Vec::new()));
    let mut dials = Dials {
        script: VecDeque::from([Err(lost(ErrorKind::ConnectionRefused))]),
        log,
    };
    let pending = pending(&fixture);
    let result = connect_replacement_with(
        &scope,
        fixture.paths.endpoint(),
        |path| dials.dial(path),
        || Err(pending.clone()),
    );
    assert_eq!(result.err(), Some(pending));
}

/// The loop is bounded by the request scope and nothing else.
#[test]
fn convergence_ends_at_the_request_scope_without_starting_an_owner() {
    let fixture = Fixture::new();
    let scope = scope_of(std::time::Duration::from_millis(300));
    let dials = Rc::new(RefCell::new(0usize));
    let counted = dials.clone();
    let started = std::time::Instant::now();
    let result = connect_replacement_with(
        &scope,
        fixture.paths.endpoint(),
        |path| {
            *counted.borrow_mut() += 1;
            let _ = path;
            Err(lost(ErrorKind::UnexpectedEof))
        },
        || panic!("an accepted-then-closed dial never proves the owner absent"),
    );
    assert_eq!(result.err(), Some(ClientError::RemoteDeadlineExceeded));
    assert!(started.elapsed() >= std::time::Duration::from_millis(300));
    assert!(
        started.elapsed() < std::time::Duration::from_secs(2),
        "the pause is clipped to the remaining budget: {:?}",
        started.elapsed()
    );
    let attempts = *dials.borrow();
    assert!(
        (3..=8).contains(&attempts),
        "paced by backoff, not a busy loop: {attempts}"
    );
}

#[test]
fn cancellation_interrupts_a_backoff_pause() {
    let fixture = Fixture::new();
    let scope = scope_of(std::time::Duration::from_secs(30));
    let canceller = scope.clone();
    let handle = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(60));
        canceller.cancel(backend_client::RequestCancellationReason::Explicit);
    });
    let started = std::time::Instant::now();
    let result = connect_replacement_with(
        &scope,
        fixture.paths.endpoint(),
        |_| Err(lost(ErrorKind::ConnectionReset)),
        || panic!("not absent"),
    );
    handle.join().expect("canceller");
    assert_eq!(
        result.err(),
        Some(ClientError::RequestCancelled(
            backend_client::RequestCancellationReason::Explicit
        ))
    );
    assert!(started.elapsed() < std::time::Duration::from_secs(2));
}

#[test]
fn an_unbounded_scope_is_refused_before_any_dial() {
    let fixture = Fixture::new();
    let result = connect_replacement_with(
        &RequestContext::new(),
        fixture.paths.endpoint(),
        |_| panic!("an unbounded loop must not start"),
        || panic!("an unbounded loop must not start"),
    );
    assert!(matches!(result.err(), Some(ClientError::Protocol(_))));
}

#[test]
fn backoff_doubles_from_25ms_to_its_ceiling() {
    let steps = |ceiling| {
        let mut backoff = pacing(ceiling);
        (0..8)
            .map(|_| backoff.step().as_millis())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        steps(REPLACEMENT_BACKOFF_CEILING),
        [25, 50, 100, 200, 400, 500, 500, 500]
    );
    assert_eq!(
        steps(REOBSERVE_BACKOFF_CEILING),
        [25, 50, 100, 200, 400, 800, 1000, 1000]
    );
}

/// The reported production failure with the real transport: the killed
/// owner's socket file is still on disk and refuses the dial. That is owner
/// absence, so one owner is started at that endpoint and authenticated.
#[test]
fn a_killed_owners_stale_endpoint_is_refused_then_a_fresh_owner_is_authenticated() {
    use backend_local_service::{EmbeddedLocalService, ProcessConfig};
    use std::os::unix::fs::PermissionsExt as _;
    let fixture = Fixture::new();
    drop(std::os::unix::net::UnixListener::bind(fixture.paths.endpoint()).expect("stale bind"));
    assert!(
        fixture.paths.endpoint().exists(),
        "a killed owner leaves its socket"
    );
    let scope = scope_of(std::time::Duration::from_secs(30));
    let mut owner = None;
    let mut session = connect_replacement_with(
        &scope,
        fixture.paths.endpoint(),
        |endpoint| Session::connect_with_context(endpoint, scope.clone()),
        || {
            let config = ProcessConfig::parse([
                "--workspace".to_owned(),
                fixture.paths.data().to_string_lossy().into_owned(),
                "--endpoint".to_owned(),
                fixture.paths.endpoint().to_string_lossy().into_owned(),
                "--profile".to_owned(),
                "builtin-echo".to_owned(),
                "--idle-timeout-ms".to_owned(),
                "0".to_owned(),
            ])
            .expect("echo owner config");
            owner = Some(EmbeddedLocalService::start(config).expect("fresh owner"));
            std::fs::set_permissions(
                fixture.paths.endpoint(),
                std::fs::Permissions::from_mode(0o600),
            )
            .expect("admitted endpoint mode");
            Ok(fixture.paths.endpoint().to_path_buf())
        },
    )
    .expect("a refused stale endpoint starts and authenticates one owner");
    assert_eq!(
        session
            .health()
            .expect("served by the new owner")
            .row_count(),
        0
    );
    drop(session);
    owner.expect("started").close().expect("stop owner");
}

#[test]
fn a_saved_key_read_waits_out_a_slow_owner_start_instead_of_ending_the_wait() {
    let fixture = Fixture::new();
    let key = backend_library::IndexOperationKey::from_bytes([61; 32]).expect("key");
    let command = SurfaceCommand::IndexOperationStatus { operation_key: key };
    let mut original = session(fixture.paths.endpoint(), |_| {
        Err(lost(ErrorKind::BrokenPipe))
    });
    let wait = scope_of(std::time::Duration::from_secs(30));
    let connects = Arc::new(AtomicUsize::new(0));
    let attempted = connects.clone();
    let pending = pending(&fixture);
    let mut recovery =
        OwnerRecovery::from_connector(fixture.paths.clone(), RequestContext::new(), move |_| {
            if attempted.fetch_add(1, Ordering::Relaxed) < 2 {
                Err(pending.clone())
            } else {
                Ok(receipt_session(key))
            }
        });
    let started = std::time::Instant::now();
    let reply = recovery
        .surface(&mut original, command, Some(&wait))
        .expect("pending starts are retried while the scope has budget");
    assert_eq!(
        reply,
        SurfaceReply::IndexOperationStatus(accepted_status(key))
    );
    assert_eq!(connects.load(Ordering::Relaxed), 3);
    assert!(
        started.elapsed() >= std::time::Duration::from_millis(175),
        "25 + 50 + 100 ms of backoff: {:?}",
        started.elapsed()
    );
}

#[test]
fn a_saved_key_read_treats_a_closed_peer_reported_as_invalid_input_as_lost() {
    let fixture = Fixture::new();
    let key = backend_library::IndexOperationKey::from_bytes([62; 32]).expect("key");
    let mut original = session(fixture.paths.endpoint(), |_| {
        Err(lost(ErrorKind::InvalidInput))
    });
    let wait = scope_of(std::time::Duration::from_secs(30));
    let mut recovery =
        OwnerRecovery::from_connector(fixture.paths.clone(), RequestContext::new(), move |_| {
            Ok(receipt_session(key))
        });
    assert_eq!(
        recovery.surface(
            &mut original,
            SurfaceCommand::IndexOperationStatus { operation_key: key },
            Some(&wait)
        ),
        Ok(SurfaceReply::IndexOperationStatus(accepted_status(key)))
    );
}

/// A replacement can be the last connection a dying owner accepted. The
/// unpinned read is repeated on a further replacement rather than failing.
#[test]
fn an_unpinned_read_repeats_on_a_further_replacement_when_the_first_is_also_lost() {
    let fixture = Fixture::new();
    let key = backend_library::IndexOperationKey::from_bytes([63; 32]).expect("key");
    let mut original = session(fixture.paths.endpoint(), |_| Err(reset()));
    let connects = Arc::new(AtomicUsize::new(0));
    let attempted = connects.clone();
    let mut recovery =
        OwnerRecovery::from_connector(fixture.paths.clone(), RequestContext::new(), move |_| {
            if attempted.fetch_add(1, Ordering::Relaxed) == 0 {
                Ok(session(std::path::Path::new("/dying"), |_| {
                    Err(lost(ErrorKind::UnexpectedEof))
                }))
            } else {
                Ok(receipt_session(key))
            }
        });
    // Initial authentication may already have replaced a refused stale socket.
    // That does not spend the later read's original bounded recovery scope.
    recovery.used = true;
    let reply = recovery
        .read(&mut original, RecoverableRead::Health, |session| {
            session.surface(SurfaceCommand::IndexOperationStatus { operation_key: key })
        })
        .expect("the second replacement serves the read");
    assert_eq!(
        reply,
        SurfaceReply::IndexOperationStatus(accepted_status(key))
    );
    assert_eq!(connects.load(Ordering::Relaxed), 2);
}

// ---------------------------------------------------------------------------
// A live but slow owner during a wait.
// ---------------------------------------------------------------------------

/// A keyed status poll changes nothing, and a timeout proves only that the
/// owner is busy. The poll is dropped and asked again on a fresh connection;
/// it neither ends the wait nor replaces the owner.
#[test]
fn a_saved_key_read_retries_a_slow_owner_on_a_fresh_connection() {
    for kind in [ErrorKind::WouldBlock, ErrorKind::TimedOut] {
        let fixture = Fixture::new();
        let key = backend_library::IndexOperationKey::from_bytes([64; 32]).expect("key");
        let mut original = session(fixture.paths.endpoint(), move |_| Err(lost(kind)));
        let wait = scope_of(std::time::Duration::from_secs(30));
        let connects = Arc::new(AtomicUsize::new(0));
        let attempted = connects.clone();
        let mut recovery = OwnerRecovery::from_connector(
            fixture.paths.clone(),
            RequestContext::new(),
            move |_| {
                // The first replacement is just as slow; the second answers.
                if attempted.fetch_add(1, Ordering::Relaxed) == 0 {
                    Ok(session(std::path::Path::new("/slow"), move |_| {
                        Err(lost(kind))
                    }))
                } else {
                    Ok(receipt_session(key))
                }
            },
        );
        assert_eq!(
            recovery.surface(
                &mut original,
                SurfaceCommand::IndexOperationStatus { operation_key: key },
                Some(&wait)
            ),
            Ok(SurfaceReply::IndexOperationStatus(accepted_status(key))),
            "{kind:?}"
        );
        assert_eq!(connects.load(Ordering::Relaxed), 2, "{kind:?}");
    }
}

/// The wait budget, not the first timeout, ends a poll against an owner that
/// never answers.
#[test]
fn a_saved_key_read_against_a_never_answering_owner_ends_only_at_the_wait_budget() {
    let fixture = Fixture::new();
    let key = backend_library::IndexOperationKey::from_bytes([65; 32]).expect("key");
    let mut original = session(fixture.paths.endpoint(), |_| {
        Err(lost(ErrorKind::WouldBlock))
    });
    let wait = scope_of(std::time::Duration::from_millis(400));
    let mut recovery =
        OwnerRecovery::from_connector(fixture.paths.clone(), RequestContext::new(), |_| {
            Ok(session(std::path::Path::new("/slow"), |_| {
                Err(lost(ErrorKind::WouldBlock))
            }))
        });
    let started = std::time::Instant::now();
    assert_eq!(
        recovery.surface(
            &mut original,
            SurfaceCommand::IndexOperationStatus { operation_key: key },
            Some(&wait)
        ),
        Err(ClientError::RemoteDeadlineExceeded)
    );
    assert!(started.elapsed() >= std::time::Duration::from_millis(400));
}

/// The same behaviour on the real private transport: a fake owner reads the
/// poll, stays silent past the client's read timeout, and only the *second*
/// connection to the same endpoint is answered. One owner, two connections.
#[test]
fn a_slow_status_reply_on_the_real_transport_is_retried_against_the_existing_listener() {
    use backend_local_service::protocol::{
        EngineRequest, EngineStatus, FrameLimits, ProtocolError, read_frame,
    };
    use backend_local_service::service::{LocaldService, OwnerService};
    use std::os::unix::fs::PermissionsExt as _;
    struct StatusOwner {
        library: backend_library::Library,
        key: backend_library::IndexOperationKey,
    }
    impl OwnerService for StatusOwner {
        fn command(&mut self, body: &[u8]) -> Result<Vec<u8>, ProtocolError> {
            let request = CommandDto::decode_for_owner(body, self.library.cursor())
                .expect("actual request admission");
            assert_eq!(
                request.command,
                Command::Surface(SurfaceCommand::IndexOperationStatus {
                    operation_key: self.key
                }),
                "only the saved-key read ever reaches the owner"
            );
            Ok(serde_json::to_vec(&ReplyDto::new(
                request.request_id,
                CommandReply::Surface(SurfaceReply::IndexOperationStatus(accepted_status(
                    self.key,
                ))),
            ))
            .expect("typed receipt encoding"))
        }
        fn engine(&mut self, _: u64, _: EngineRequest) -> Result<EngineStatus, ProtocolError> {
            panic!("a receipt read cannot issue engine mutations")
        }
        fn serve_one(&mut self) -> bool {
            false
        }
        fn close(&mut self) {}
    }
    let fixture = Fixture::new();
    let key = backend_library::IndexOperationKey::from_bytes([66; 32]).expect("key");
    let listener =
        std::os::unix::net::UnixListener::bind(fixture.paths.endpoint()).expect("private endpoint");
    std::fs::set_permissions(
        fixture.paths.endpoint(),
        std::fs::Permissions::from_mode(0o600),
    )
    .expect("private socket");
    listener.set_nonblocking(true).expect("bounded accept");
    let server = std::thread::spawn(move || {
        let accept = |listener: &std::os::unix::net::UnixListener| {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
            loop {
                match listener.accept() {
                    Ok((stream, _)) => {
                        stream.set_nonblocking(false).expect("blocking stream");
                        return stream;
                    }
                    Err(error) if error.kind() == ErrorKind::WouldBlock => {
                        assert!(std::time::Instant::now() < deadline, "no connection");
                        std::thread::sleep(std::time::Duration::from_millis(1));
                    }
                    Err(error) => panic!("private accept failed: {error}"),
                }
            }
        };
        // First connection: the owner is busy. It takes the request and says
        // nothing for longer than the client is willing to wait.
        let mut busy = accept(&listener);
        busy.set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .expect("bounded read");
        read_frame(&mut busy, FrameLimits::default()).expect("the poll reached the owner");
        std::thread::sleep(std::time::Duration::from_millis(900));
        drop(busy);
        // Second connection to the same listener: answered normally.
        let mut ready = accept(&listener);
        ready
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .expect("bounded read");
        let mut service = LocaldService::new(
            StatusOwner {
                library: backend_library::Library::new(),
                key,
            },
            FrameLimits::default(),
        )
        .expect("real frame service");
        service
            .serve_stream(&mut ready)
            .expect("served until the client left")
    });
    let mut original = Session::connect_with_timeouts(
        fixture.paths.endpoint(),
        std::time::Duration::from_secs(5),
        std::time::Duration::from_millis(300),
    )
    .expect("real initial authentication");
    // Never bootstraps: the endpoint accepts, so the owner is present.
    let mut recovery = OwnerRecovery::new(&fixture.paths, false, RequestContext::new());
    let wait = scope_of(std::time::Duration::from_secs(60));
    let started = std::time::Instant::now();
    let reply = recovery
        .surface(
            &mut original,
            SurfaceCommand::IndexOperationStatus { operation_key: key },
            Some(&wait),
        )
        .expect("a slow status reply is retried, not fatal");
    assert_eq!(
        reply,
        SurfaceReply::IndexOperationStatus(accepted_status(key))
    );
    assert!(
        started.elapsed() >= std::time::Duration::from_millis(300),
        "the first connection really waited out its read timeout"
    );
    assert!(recovery.used);
    recovery.detach(&mut original).expect("retire binding");
    wait.finish();
    drop(original);
    assert_eq!(server.join().expect("fake owner"), 1, "one answered poll");
}
