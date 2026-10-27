//! Normal stdio and SessionProduct/caller-journal routing. The transport seam
//! supplies admitted typed owner observations; real owner capture, refusal,
//! replacement and cold-root controls live in local-service index admission.
use super::*;
use backend_client::CommandTransport;
use backend_library::{
    Command, CommandDto, IndexOperationKey, IndexOperationObservation, IndexOperationState,
    IndexOperationStatus, ProductText,
};
use std::io::BufReader;
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Default)]
struct OwnerState {
    starts: Vec<IndexOperationKey>,
    active: Option<IndexOperationStatus>,
    terminal: Option<IndexOperationStatus>,
    commands: Vec<SurfaceCommand>,
    cancelled: Vec<IndexJobTicket>,
}

struct Owner(Arc<Mutex<OwnerState>>);
impl CommandTransport for Owner {
    fn request(&mut self, request: CommandDto) -> Result<ReplyDto, ClientError> {
        let Command::Surface(command) = request.command else {
            panic!("index routing must not probe unrelated owner lanes");
        };
        let mut state = self.0.lock().expect("fixture owner state");
        state.commands.push(command.clone());
        let reply = match command {
            SurfaceCommand::IndexOperationStart {
                cargo_target: None,
                operation_key,
                package,
                execution_intent,
            } => {
                assert!(
                    state.active.is_none(),
                    "a terminal claim must be reconciled before replacement"
                );
                assert!(!state.starts.contains(&operation_key));
                state.starts.push(operation_key);
                let ticket = IndexJobTicket::new(
                    std::num::NonZeroU64::new(state.starts.len() as u64).expect("owner ticket"),
                    [0xa1; 16],
                    package.clone(),
                );
                let status = IndexOperationStatus::new(
                    operation_key,
                    package,
                    execution_intent,
                    IndexOperationState::Active {
                        ticket,
                        stage: IndexJobStage::Compiling,
                    },
                );
                state.active = Some(status.clone());
                SurfaceReply::IndexOperationStarted(IndexOperationObservation::Known(status))
            }
            SurfaceCommand::IndexStart {
                cargo_target: None,
                package,
                execution_intent,
            } => {
                let active = state
                    .active
                    .as_ref()
                    .expect("original actual routed request is active");
                assert_eq!(&active.package, &package);
                assert_eq!(active.execution_intent, execution_intent);
                let IndexOperationState::Active { ticket, stage } = &active.state else {
                    panic!("only an active owner-issued ticket may be joined");
                };
                SurfaceReply::IndexStarted(IndexStartResult::Started {
                    ticket: ticket.clone(),
                    stage: *stage,
                })
            }
            SurfaceCommand::IndexCancel { ticket } => {
                let matches = state.active.as_ref().is_some_and(|active| {
                    matches!(&active.state, IndexOperationState::Active { ticket: actual, .. } if actual == &ticket)
                });
                let status = if matches {
                    let mut terminal = state.active.take().expect("exact resident job");
                    terminal.state = IndexOperationState::Failed {
                        reason: backend_library::IndexOperationFailureReason::Cancelled,
                        detail: ProductText::from_static("original accepted job cancelled"),
                        compiler_failure: None,
                    };
                    state.terminal = Some(terminal);
                    state.cancelled.push(ticket.clone());
                    IndexCancelStatus::Requested
                } else {
                    IndexCancelStatus::Unknown
                };
                SurfaceReply::IndexCancellation(IndexCancelReceipt {
                    ticket,
                    status,
                })
            }
            SurfaceCommand::IndexOperationStatus { operation_key } => {
                let status = state
                    .active
                    .as_ref()
                    .or(state.terminal.as_ref())
                    .expect("retained operation");
                assert_eq!(status.operation_key, operation_key);
                SurfaceReply::IndexOperationStatus(IndexOperationObservation::Known(status.clone()))
            }
            other => panic!("unexpected owner command: {other:?}"),
        };
        Ok(ReplyDto::new(
            request.request_id,
            CommandReply::Surface(reply),
        ))
    }
}

fn receive(reader: &mut impl BufRead, id: u64) -> Value {
    let mut line = String::new();
    assert!(reader.read_line(&mut line).expect("bounded stdio reply") > 0);
    let wire = assert_stdio_line_accounting(&line);
    assert_eq!(wire["id"], id);
    assert!(wire.get("error").is_none(), "{wire}");
    wire["result"].clone()
}

fn call_line(writer: &mut impl Write, id: u64, params: Value) {
    writeln!(
        writer,
        "{}",
        json!({"jsonrpc":"2.0", "id":id, "method":"tools/call", "params":params})
    )
    .expect("normal tools/call line");
}

#[test]
fn stdio_index_then_index_start_keeps_exact_ticket_poll_and_durable_retry_identity() {
    assert_stdio_shared_default_index(false);
}

#[test]
fn stdio_default_index_start_then_cli_add_joins_same_saved_key_before_work() {
    assert_stdio_shared_default_index(true);
}

fn assert_stdio_shared_default_index(start_first: bool) {
    let fixture = LazyAuthorityFixture::new();
    fixture
        .paths
        .initialize()
        .expect("real private caller journal attachment");
    let package =
        PackageReference::parse(fixture.project.to_str().expect("project UTF-8")).expect("project");
    let state = Arc::new(Mutex::new(OwnerState::default()));
    let session = Session::from_transport(fixture.paths.endpoint(), Owner(Arc::clone(&state)));
    let server = Server::with_authority(
        SessionProduct::new(session, fixture.paths.clone()),
        fixture.project.to_str().expect("project").to_owned(),
        [7; 32],
    );
    let (mut input, server_input) = std::os::unix::net::UnixStream::pair().expect("stdin pipe");
    let (output, mut server_output) = std::os::unix::net::UnixStream::pair().expect("stdout pipe");
    output
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("bounded fixture read");
    let mut output = BufReader::new(output);
    let serving = std::thread::spawn(move || {
        serve_stdio_server(
            server,
            BufReader::new(server_input),
            &mut server_output,
        )
    });
    writeln!(input, "{}", json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"index-overlap-law","version":"1"}}})).expect("initialize");
    writeln!(
        input,
        "{}",
        json!({"jsonrpc":"2.0","method":"notifications/initialized"})
    )
    .expect("initialized");
    // Both requests are on ordinary stdin before either operation reply is
    // read; the first command has returned an active owner job, not a terminal.
    call_line(
        &mut input,
        2,
        if start_first {
            json!({"name":INDEX_START_TOOL,"arguments":{"package":package.as_str(),"detail":"full"}})
        } else {
            json!({"name":"backend.index", "arguments":{"path":package.as_str(),"detail":"full"}})
        },
    );
    call_line(
        &mut input,
        3,
        if start_first {
            json!({"name":"backend.index","arguments":{"path":package.as_str(),"detail":"full"}})
        } else {
            json!({"name":INDEX_START_TOOL, "arguments":{"package":package.as_str(),"detail":"full"}})
        },
    );
    let initialized = receive(&mut output, 1);
    assert!(initialized["serverInfo"].is_object());
    let first = receive(&mut output, 2);
    assert_eq!(first["isError"], false);
    let poll = first["structuredContent"]["index_poll"]["call"].clone();
    assert_eq!(poll["name"], INDEX_PROGRESS_TOOL);
    let initial = state
        .lock()
        .expect("state")
        .active
        .clone()
        .expect("routed durable start");
    let IndexOperationState::Active { ticket, .. } = &initial.state else {
        panic!("live job");
    };
    assert_eq!(
        poll["arguments"]["operation_key"],
        serde_json::to_value(initial.operation_key).expect("key")
    );
    let joined = receive(&mut output, 3);
    assert_eq!(joined["isError"], false);
    let started = if start_first { &first } else { &joined };
    assert_eq!(
        started["structuredContent"]["surface"]["data"],
        serde_json::to_value(IndexOperationObservation::Known(initial.clone()))
            .expect("same complete active receipt")
    );
    assert_eq!(
        started["structuredContent"]["index_poll"]["call"]["arguments"]["operation_key"],
        serde_json::to_value(initial.operation_key).expect("key")
    );
    let mut cli = Session::from_transport(fixture.paths.endpoint(), Owner(Arc::clone(&state)));
    let cli_reply = backend_client::index_with_journal(
        &fixture.paths,
        package.clone(),
        initial.execution_intent,
        |command| cli.surface(command),
    )
    .expect("ordinary CLI/shared journal join");
    assert_eq!(
        cli_reply,
        SurfaceReply::IndexOperationStatus(IndexOperationObservation::Known(initial.clone()))
    );
    assert_eq!(ticket.package(), &package);
    {
        let mut owner = state.lock().expect("state");
        assert_eq!(owner.starts, [initial.operation_key]);
        assert_eq!(
            owner.commands.len(),
            3,
            "one initial start then MCP and CLI same-key receipt reads, no extra mutation/revision/probe"
        );
        owner.active = None;
        owner.terminal = Some(IndexOperationStatus::new(
            initial.operation_key,
            package.clone(),
            initial.execution_intent,
            IndexOperationState::Failed {
                reason: backend_library::IndexOperationFailureReason::Refused,
                detail: ProductText::from_static(
                    "closed fixture compiler refused the exact admitted request",
                ),
                compiler_failure: None,
            },
        ));
    }
    call_line(&mut input, 4, poll);
    let failed = receive(&mut output, 4);
    assert_eq!(
        failed["isError"], true,
        "terminal refusal must remain visible"
    );
    let fault = &failed["structuredContent"]["fault"];
    assert_eq!(fault["slug"], "rejected");
    assert_eq!(fault["cause"], "refused");
    assert_eq!(fault["operand"], initial.operation_key.to_hex());
    assert_eq!(
        fault["detail"],
        "closed fixture compiler refused the exact admitted request"
    );
    assert!(failed["structuredContent"].get("index_poll").is_none());
    assert_eq!(
        failed["structuredContent"]["surface"]["data"]["detail"],
        serde_json::to_value(
            state
                .lock()
                .expect("state")
                .terminal
                .as_ref()
                .expect("terminal")
        )
        .expect("unchanged exact failure")
    );
    let claims = fixture.paths.data().join("client-index-operations-v1");
    assert!(
        !claims
            .join(format!(
                "{}.json",
                blake3::Hash::from(initial.request_digest).to_hex()
            ))
            .exists(),
        "exact terminal observation acknowledges only its default claim"
    );
    call_line(
        &mut input,
        5,
        json!({"name":"backend.index","arguments":{"path":package.as_str(),"detail":"full"}}),
    );
    input
        .shutdown(std::net::Shutdown::Write)
        .expect("EOF is a read half-close");
    let retry = receive(&mut output, 5);
    assert_eq!(retry["isError"], false);
    let owner = state.lock().expect("state");
    assert_eq!(owner.starts.len(), 2);
    assert_ne!(owner.starts[0], owner.starts[1]);
    let fresh = owner
        .active
        .as_ref()
        .expect("fresh request remains admitted");
    assert_eq!(fresh.request_digest, initial.request_digest);
    assert_eq!(fresh.package, initial.package);
    assert_eq!(
        retry["structuredContent"]["index_poll"]["call"]["arguments"],
        json!({
            "operation_key":fresh.operation_key,
            "after_observation":backend_client::IndexObservationFingerprint::active(fresh)
                .expect("full durable receipt fingerprint").expect("active receipt"),
            "wait_millis":backend_client::MAX_INDEX_PROGRESS_WAIT_MILLIS,
            "detail":"full",
        })
    );
    drop(owner);
    serving
        .join()
        .expect("stdio actor joined")
        .expect("finite stdio drain");
}

#[test]
fn default_mcp_start_cli_join_and_exact_ticket_cancel_retain_original_receipt_key() {
    let fixture = LazyAuthorityFixture::new();
    fixture
        .paths
        .initialize()
        .expect("real private caller attachment");
    let package =
        PackageReference::parse(fixture.project.to_str().expect("path")).expect("package");
    let state = Arc::new(Mutex::new(OwnerState::default()));
    let session = Session::from_transport(fixture.paths.endpoint(), Owner(Arc::clone(&state)));
    let mut server = Server::with_authority(
        SessionProduct::new(session, fixture.paths.clone()),
        fixture.project.to_str().expect("path").to_owned(),
        [7; 32],
    );
    server.handle(br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"default-join-cancel","version":"1"}}}"#).expect("initialize");
    server.handle(br#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#);
    fn session_tool(server: &mut Server<SessionProduct>, name: &str, arguments: &Value) -> Value {
        let request = serde_json::to_vec(&json!({"jsonrpc":"2.0","id":9,"method":"tools/call","params":{"name":name,"arguments":arguments}})).expect("request bytes");
        let response = server.handle(&request).expect("correlated tool response");
        assert!(response.get("error").is_none(), "{response}");
        response["result"].clone()
    }
    let started = session_tool(
        &mut server,
        INDEX_START_TOOL,
        &json!({"package":package.as_str(),"detail":"full"}),
    );
    assert_eq!(started["isError"], false);
    let initial = state
        .lock()
        .expect("state")
        .active
        .clone()
        .expect("actual routed active key");
    let IndexOperationState::Active { ticket, .. } = &initial.state else {
        panic!("active owner ticket");
    };
    let mut cli = Session::from_transport(fixture.paths.endpoint(), Owner(Arc::clone(&state)));
    let joined = backend_client::index_with_journal(
        &fixture.paths,
        package,
        initial.execution_intent,
        |command| cli.surface(command),
    )
    .expect("ordinary CLI saved-request join");
    assert_eq!(
        joined,
        SurfaceReply::IndexOperationStatus(IndexOperationObservation::Known(initial.clone()))
    );
    let foreign = IndexJobTicket::new(
        std::num::NonZeroU64::new(ticket.id() + 1).expect("different id"),
        ticket.owner_epoch(),
        ticket.package().clone(),
    );
    let refused = session_tool(
        &mut server,
        INDEX_CANCEL_TOOL,
        &json!({"ticket":foreign,"detail":"full"}),
    );
    let expected_foreign = SurfaceReply::IndexCancellation(IndexCancelReceipt {
        ticket: foreign,
        status: IndexCancelStatus::Unknown,
    });
    assert_eq!(
        refused["structuredContent"]["surface"]["data"],
        serde_json::to_value(expected_foreign).expect("wire")["data"]
    );
    assert!(
        state.lock().expect("state").cancelled.is_empty(),
        "foreign ticket never cancels current job"
    );
    let cancelled = session_tool(
        &mut server,
        INDEX_CANCEL_TOOL,
        &json!({"ticket":ticket,"detail":"full"}),
    );
    assert_eq!(cancelled["isError"], false);
    let progress = session_tool(
        &mut server,
        INDEX_PROGRESS_TOOL,
        &json!({"operation_key":initial.operation_key,"detail":"full"}),
    );
    assert_eq!(
        progress["isError"], true,
        "actual cancellation remains a typed terminal refusal"
    );
    let terminal = state
        .lock()
        .expect("state")
        .terminal
        .clone()
        .expect("same original cancelled operation");
    assert_eq!(terminal.operation_key, initial.operation_key);
    assert_eq!(terminal.request_digest, initial.request_digest);
    assert_eq!(terminal.package, initial.package);
    assert_eq!(terminal.execution_intent, initial.execution_intent);
    assert!(matches!(
        terminal.state,
        IndexOperationState::Failed {
            reason: backend_library::IndexOperationFailureReason::Cancelled,
            ..
        }
    ));
    assert_eq!(
        progress["structuredContent"]["surface"]["data"],
        serde_json::to_value(IndexOperationObservation::Known(terminal))
            .expect("exact full receipt")
    );
    let owner = state.lock().expect("state");
    assert_eq!(
        owner.starts,
        [initial.operation_key],
        "no duplicate capture/start"
    );
    assert_eq!(owner.cancelled, [ticket.clone()]);
}
