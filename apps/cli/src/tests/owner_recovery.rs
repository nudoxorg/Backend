//! Production replacement policy and caller journal. Scripted owners below
//! prove dispatch/identity, not native publication. The socket control uses
//! the actual private endpoint/authentication/frame path for replacement.
use super::*;
use backend_client::CommandTransport;
use backend_library::{
    Command, CommandDto, CommandReply, CompileExecutionIntent, IndexOperationObservation,
    IndexOperationState, IndexOperationStatus, PackageReference, ReplyDto,
};
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
    use backend_replication::{LocalControlLimits, read_frame, write_frame};
    use std::os::unix::fs::PermissionsExt as _;
    let fixture = Fixture::new();
    let listener = std::os::unix::net::UnixListener::bind(fixture.paths.endpoint())
        .expect("private owner endpoint");
    std::fs::set_permissions(
        fixture.paths.endpoint(),
        std::fs::Permissions::from_mode(0o600),
    )
    .expect("admitted endpoint mode");
    let server = std::thread::spawn(move || {
        // Recovery authenticates the existing owner directly, without a bootstrap probe.
        let (mut stream, _) = listener
            .accept()
            .expect("replacement authenticated connection");
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(2)))
            .expect("bounded owner");
        let body = read_frame(&mut stream, LocalControlLimits::default())
            .expect("correlated health frame");
        let library = backend_library::Library::new();
        let request = CommandDto::decode_for_owner(&body, library.cursor())
            .expect("actual owner request admission");
        assert_eq!(request.command, Command::Health);
        let reply = library.execute_dto(request.clone());
        write_frame(
            &mut stream,
            &serde_json::to_vec(&reply).expect("reply"),
            LocalControlLimits::default(),
        )
        .expect("reply frame");
        request.request_id
    });
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
    assert_eq!(
        server.join().expect("owner drained"),
        1,
        "fresh Session has its own correlated request sequence"
    );
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
        ClientError::Disconnected(ErrorKind::InvalidInput),
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
        assert!(matches!(request.command, Command::Surface(SurfaceCommand::IndexOperationStart { .. })));
        sent.fetch_add(1, Ordering::Relaxed);
        Ok(ReplyDto::error(request.request_id, "the admitted start was refused"))
    });
    let mut recovery = OwnerRecovery::from_connector(
        fixture.paths.clone(), RequestContext::new(), |_| panic!("refusal cannot reconnect"),
    );
    let error = backend_client::index_with_journal_recovery(
        &fixture.paths, fixture.package.clone(), CompileExecutionIntent::Interactive,
        |dispatch| recovery.journal(&mut original, dispatch, None),
    ).expect_err("actual command rejection is retained");
    assert!(matches!(&error, ClientError::CommandFailed(_)), "{error}");
    assert_eq!(sends.load(Ordering::Relaxed), 1);
    assert!(!recovery.used);
    assert_eq!(fixture.claims().len(), 1, "no durable terminal receipt was invented");
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
    let deadline = wait.read_deadline(std::time::Duration::from_secs(10)).expect("original deadline");
    let captured = wait.clone();
    let connects = Arc::new(AtomicUsize::new(0));
    let connected = connects.clone();
    let mut recovery = OwnerRecovery::from_connector(
        fixture.paths.clone(), RequestContext::new(), move |scope| {
            assert!(scope.same_request(&captured));
            assert_eq!(scope.read_deadline(std::time::Duration::from_secs(900)).expect("same deadline"), deadline);
            if connected.fetch_add(1, Ordering::Relaxed) == 2 {
                scope.cancel(backend_client::RequestCancellationReason::Explicit);
                return Err(ClientError::RequestCancelled(backend_client::RequestCancellationReason::Explicit));
            }
            let reads = reads.clone();
            let expected = SurfaceCommand::IndexOperationStatus { operation_key: key };
            Ok(session(std::path::Path::new("/replacement"), move |request| {
                assert_eq!(request.command, Command::Surface(expected.clone()));
                reads.fetch_add(1, Ordering::Relaxed);
                Err(reset())
            }))
        },
    );
    assert_eq!(recovery.surface(&mut original, command.clone(), Some(&wait)),
        Err(ClientError::RequestCancelled(backend_client::RequestCancellationReason::Explicit)));
    assert_eq!(connects.load(Ordering::Relaxed), 3);
    assert_eq!(recovery.surface(&mut original, command, Some(&wait)),
        Err(ClientError::RequestCancelled(backend_client::RequestCancellationReason::Explicit)));
    assert_eq!(connects.load(Ordering::Relaxed), 3, "cancel cannot authenticate again");
}

/// Real private Unix transport and the production 256-frame server boundary.
/// Scripted Accepted receipts prove recovery/identity, not compiler publication.
#[test]
fn saved_key_read_survives_three_real_frame_budget_retirements_without_new_start() {
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
    }
    impl OwnerService for ReceiptOwner {
        fn command(&mut self, body: &[u8]) -> Result<Vec<u8>, ProtocolError> {
            let request = CommandDto::decode_for_owner(body, self.library.cursor())
                .expect("actual request admission");
            let (key, started) = match request.command {
                Command::Surface(SurfaceCommand::IndexOperationStart {
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
                IndexOperationState::Accepted,
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
    for _ in 1..(frames * 4) {
        let actual = recovery
            .surface(
                &mut original,
                SurfaceCommand::IndexOperationStatus { operation_key: key },
                Some(&wait),
            )
            .expect("same-key read across healthy connection retirement");
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
        (Some(key), 1, frames * 4 - 1)
    );
}
