//! Exercises the `interface-mcp` tests framed-process contract through its observable boundary.
//! The cases target malformed, partial, reordered, and resource-constrained behavior.
//! Assertions retain exact typed causes so regressions cannot pass through lossy errors.
use std::{
    error::Error,
    fmt,
    io::{self, BufReader, Write},
    process::{ChildStdin, ChildStdout, Command, Stdio},
};

use interface_core::{CapabilityDomain, ContentId, GenerationId, IndexSnapshotId};
use interface_protocol::{read_frame, write_frame};
use serde::Serialize;
use serde_json::Value;

// The shared corpus also exposes CLI argument construction for the sibling process test.
#[allow(dead_code)]
#[path = "../../protocol/tests/support/golden_corpus.rs"]
mod golden_corpus;

#[derive(Debug)]
enum TestError {
    Io(io::Error),
    Json(serde_json::Error),
    Golden(golden_corpus::GoldenError),
}

impl fmt::Display for TestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(source) => source.fmt(formatter),
            Self::Json(source) => source.fmt(formatter),
            Self::Golden(source) => source.fmt(formatter),
        }
    }
}

impl Error for TestError {}

impl From<io::Error> for TestError {
    fn from(source: io::Error) -> Self {
        Self::Io(source)
    }
}

impl From<serde_json::Error> for TestError {
    fn from(source: serde_json::Error) -> Self {
        Self::Json(source)
    }
}

#[derive(Serialize)]
#[serde(rename_all = "kebab-case")]
enum ApplicationAction {
    Generate,
    Health,
    RecoverLocal,
    PollExecution,
    ReleaseLocal,
}

#[derive(Serialize)]
enum JsonRpcVersion {
    #[serde(rename = "2.0")]
    Version2,
}

#[derive(Serialize)]
enum RpcMethod {
    #[serde(rename = "tools/call")]
    ToolsCall,
    #[serde(rename = "$/cancelRequest")]
    CancelRequest,
}

#[derive(Serialize)]
enum ToolName {
    #[serde(rename = "interface.application")]
    Application,
}

#[derive(Serialize)]
enum SourceProfile {
    #[serde(rename = "rust-2024")]
    Rust2024,
}

#[derive(Serialize)]
#[serde(rename_all = "lowercase")]
enum SourceStage {
    #[serde(rename = "lower-ir")]
    LowerIr,
}

#[derive(Serialize)]
#[serde(rename_all = "lowercase")]
enum Pressure {
    Relaxed,
}

#[derive(Serialize)]
#[serde(rename_all = "lowercase")]
enum BatteryState {
    Normal,
    Critical,
}

#[derive(Serialize)]
enum OperationId {
    #[serde(rename = "1")]
    First,
    #[serde(rename = "2")]
    Second,
}

#[derive(Serialize)]
#[serde(untagged)]
enum JsonRpcRequestId<'request_id> {
    Null,
    Number(u64),
    Text(&'request_id str),
}

#[derive(Serialize)]
struct ToolCallRequest<'request_id, Arguments> {
    jsonrpc: JsonRpcVersion,
    id: JsonRpcRequestId<'request_id>,
    method: RpcMethod,
    params: ToolCallParams<Arguments>,
}

#[derive(Serialize)]
struct ToolCallNotification<Arguments> {
    jsonrpc: JsonRpcVersion,
    method: RpcMethod,
    params: ToolCallParams<Arguments>,
}

#[derive(Serialize)]
struct ToolCallParams<Arguments> {
    name: ToolName,
    arguments: ApplicationArguments<Arguments>,
}

#[derive(Serialize)]
struct EmptyArguments {}

#[derive(Serialize)]
struct ApplicationArguments<Arguments> {
    #[serde(flatten)]
    fields: Arguments,
    action: ApplicationAction,
    correlation: u64,
}

#[derive(Serialize)]
struct GenerateArguments<'source> {
    profile: SourceProfile,
    stage: SourceStage,
    source: &'source str,
}

#[derive(Serialize)]
struct PolicyArguments {
    generation: String,
    snapshot: String,
    bundle: String,
    ram_free: u64,
    nvme_free: u64,
    operations: u64,
    retries: u64,
    memory_pressure: Pressure,
    storage_pressure: Pressure,
    battery: BatteryState,
}

#[derive(Serialize)]
struct PollExecutionArguments {
    operation: OperationId,
}

#[derive(Serialize)]
struct CancellationNotification<'request_id> {
    jsonrpc: JsonRpcVersion,
    method: RpcMethod,
    params: CancellationParams<'request_id>,
}

#[derive(Serialize)]
struct CancellationParams<'request_id> {
    #[serde(rename = "requestId")]
    request_id: &'request_id str,
}

#[derive(Serialize)]
struct MalformedToolCallRequest<'request_id> {
    jsonrpc: JsonRpcVersion,
    id: JsonRpcRequestId<'request_id>,
    method: RpcMethod,
    params: MalformedToolCallParams,
}

#[derive(Serialize)]
struct MalformedToolCallParams {
    name: ToolName,
}

fn request<Arguments: Serialize>(
    id: JsonRpcRequestId<'_>,
    correlation: u64,
    action: ApplicationAction,
    arguments: Arguments,
) -> Result<Value, TestError> {
    Ok(serde_json::to_value(ToolCallRequest {
        jsonrpc: JsonRpcVersion::Version2,
        id,
        method: RpcMethod::ToolsCall,
        params: ToolCallParams {
            name: ToolName::Application,
            arguments: ApplicationArguments {
                fields: arguments,
                action,
                correlation,
            },
        },
    })?)
}

fn notification<Arguments: Serialize>(
    correlation: u64,
    action: ApplicationAction,
    arguments: Arguments,
) -> Result<Value, TestError> {
    Ok(serde_json::to_value(ToolCallNotification {
        jsonrpc: JsonRpcVersion::Version2,
        method: RpcMethod::ToolsCall,
        params: ToolCallParams {
            name: ToolName::Application,
            arguments: ApplicationArguments {
                fields: arguments,
                action,
                correlation,
            },
        },
    })?)
}

fn send(writer: &mut impl Write, value: &Value) -> Result<(), TestError> {
    let body = serde_json::to_vec(value)?;
    write_frame(writer, &body)?;
    Ok(())
}

fn receive(reader: &mut BufReader<impl io::Read>) -> Result<Value, TestError> {
    let body = read_frame(reader)?.ok_or_else(|| {
        TestError::Io(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "MCP child closed before its framed response",
        ))
    })?;
    Ok(serde_json::from_slice(&body)?)
}

fn generate(
    stdin: &mut ChildStdin,
    stdout: &mut BufReader<ChildStdout>,
    id: u64,
    source: &str,
) -> Result<Value, TestError> {
    let generation = request(
        JsonRpcRequestId::Number(id),
        id,
        ApplicationAction::Generate,
        GenerateArguments {
            profile: SourceProfile::Rust2024,
            stage: SourceStage::LowerIr,
            source,
        },
    )?;
    send(stdin, &generation)?;
    receive(stdout)
}

fn policy_arguments() -> PolicyArguments {
    PolicyArguments {
        generation: GenerationId::from_digest([11; 32]).to_string(),
        snapshot: IndexSnapshotId::from_digest([13; 32]).to_string(),
        bundle: ContentId::<CapabilityDomain>::from_digest([17; 32]).to_string(),
        ram_free: 4096,
        nvme_free: 8192,
        operations: 1,
        retries: 1,
        memory_pressure: Pressure::Relaxed,
        storage_pressure: Pressure::Relaxed,
        battery: BatteryState::Normal,
    }
}

fn effect<Arguments: Serialize>(
    stdin: &mut ChildStdin,
    stdout: &mut BufReader<ChildStdout>,
    id: JsonRpcRequestId<'_>,
    correlation: u64,
    action: ApplicationAction,
    arguments: Arguments,
) -> Result<Value, TestError> {
    let begin_request = request(id, correlation, action, arguments)?;
    send(stdin, &begin_request)?;
    receive(stdout)
}

fn recover(
    stdin: &mut ChildStdin,
    stdout: &mut BufReader<ChildStdout>,
    id: JsonRpcRequestId<'_>,
    correlation: u64,
) -> Result<Value, TestError> {
    effect(
        stdin,
        stdout,
        id,
        correlation,
        ApplicationAction::RecoverLocal,
        policy_arguments(),
    )
}

fn release_arguments() -> PolicyArguments {
    PolicyArguments {
        generation: GenerationId::from_digest([11; 32]).to_string(),
        snapshot: IndexSnapshotId::from_digest([13; 32]).to_string(),
        bundle: ContentId::<CapabilityDomain>::from_digest([17; 32]).to_string(),
        ram_free: 4096,
        nvme_free: 8192,
        operations: 1,
        retries: 0,
        memory_pressure: Pressure::Relaxed,
        storage_pressure: Pressure::Relaxed,
        battery: BatteryState::Critical,
    }
}

fn assert_generation(
    stdin: &mut ChildStdin,
    stdout: &mut BufReader<ChildStdout>,
    id: u64,
    source: &str,
) -> Result<(), TestError> {
    let generated = generate(stdin, stdout, id, source)?;
    let structured = &generated["result"]["structuredContent"];
    assert_eq!(structured["correlation"], id);
    assert_eq!(structured["body"]["kind"], "dependency_unavailable");
    assert_eq!(structured["body"]["capability"], "compiler_output");
    assert_eq!(structured["terminal"]["kind"], "degraded");
    assert_eq!(structured["terminal"]["unavailable"], "compiler_output");
    assert_eq!(structured["diagnostic"], Value::Null);
    Ok(())
}

fn assert_first_completed(
    stdin: &mut ChildStdin,
    stdout: &mut BufReader<ChildStdout>,
) -> Result<(), TestError> {
    let admitted = recover(stdin, stdout, JsonRpcRequestId::Number(82), 82)?;
    assert_eq!(
        admitted["result"]["structuredContent"]["body"]["kind"],
        "execution_started"
    );

    let pending_request = request(
        JsonRpcRequestId::Number(83),
        83,
        ApplicationAction::PollExecution,
        PollExecutionArguments {
            operation: OperationId::First,
        },
    )?;
    send(stdin, &pending_request)?;
    let pending = receive(stdout)?;
    assert_eq!(pending["id"], 83);
    assert_eq!(
        pending["result"]["structuredContent"]["body"]["state"]["kind"],
        "pending"
    );

    let completed_request = request(
        JsonRpcRequestId::Number(84),
        84,
        ApplicationAction::PollExecution,
        PollExecutionArguments {
            operation: OperationId::First,
        },
    )?;
    send(stdin, &completed_request)?;
    let completed = receive(stdout)?;
    assert_eq!(completed["id"], 84);
    assert_eq!(
        completed["result"]["structuredContent"]["body"]["state"]["kind"],
        "completed"
    );
    Ok(())
}

fn assert_cancelled(
    stdin: &mut ChildStdin,
    stdout: &mut BufReader<ChildStdout>,
) -> Result<(), TestError> {
    let admitted = effect(
        stdin,
        stdout,
        JsonRpcRequestId::Text("second-operation"),
        85,
        ApplicationAction::ReleaseLocal,
        release_arguments(),
    )?;
    assert_eq!(admitted["id"], "second-operation");
    assert_eq!(
        admitted["result"]["structuredContent"]["body"]["kind"],
        "execution_started"
    );

    let cancellation_request = serde_json::to_value(CancellationNotification {
        jsonrpc: JsonRpcVersion::Version2,
        method: RpcMethod::CancelRequest,
        params: CancellationParams {
            request_id: "second-operation",
        },
    })?;
    send(stdin, &cancellation_request)?;

    let progress_request = request(
        JsonRpcRequestId::Number(86),
        86,
        ApplicationAction::PollExecution,
        PollExecutionArguments {
            operation: OperationId::Second,
        },
    )?;
    send(stdin, &progress_request)?;
    let progress = receive(stdout)?;
    assert_eq!(progress["id"], 86);
    assert_eq!(
        progress["result"]["structuredContent"]["body"]["state"]["kind"],
        "cancelled"
    );
    Ok(())
}

fn assert_malformed(
    stdin: &mut ChildStdin,
    stdout: &mut BufReader<ChildStdout>,
) -> Result<(), TestError> {
    let malformed = serde_json::to_value(MalformedToolCallRequest {
        jsonrpc: JsonRpcVersion::Version2,
        id: JsonRpcRequestId::Number(87),
        method: RpcMethod::ToolsCall,
        params: MalformedToolCallParams {
            name: ToolName::Application,
        },
    })?;
    send(stdin, &malformed)?;
    let error = receive(stdout)?;
    assert_eq!(error["id"], 87);
    assert_eq!(error["error"]["data"]["code"], "missing_field");
    Ok(())
}

#[test]
fn framed_mcp_process_preserves_structured_results_and_named_cancellation() -> Result<(), TestError>
{
    let mut child = Command::new(env!("CARGO_BIN_EXE_interface-mcp"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| io::Error::other("MCP child did not retain stdin"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("MCP child did not retain stdout"))?;
    let mut stdout = BufReader::new(stdout);

    assert_generation(&mut stdin, &mut stdout, 81, "fn mcp() {}")?;
    assert_generation(&mut stdin, &mut stdout, 811, "fn mcp_second() {}")?;
    assert_first_completed(&mut stdin, &mut stdout)?;
    assert_cancelled(&mut stdin, &mut stdout)?;
    assert_malformed(&mut stdin, &mut stdout)?;

    drop(stdin);
    let status = child.wait()?;
    assert!(status.success());
    Ok(())
}

#[test]
fn framed_mcp_preserves_number_string_null_ids_and_silences_notifications() -> Result<(), TestError>
{
    let mut child = Command::new(env!("CARGO_BIN_EXE_interface-mcp"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| io::Error::other("MCP child did not retain stdin"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("MCP child did not retain stdout"))?;
    let mut stdout = BufReader::new(stdout);

    let notification = notification(
        90,
        ApplicationAction::Generate,
        GenerateArguments {
            profile: SourceProfile::Rust2024,
            stage: SourceStage::LowerIr,
            source: "fn notification() {}",
        },
    )?;
    send(&mut stdin, &notification)?;
    let health = request(
        JsonRpcRequestId::Number(91),
        91,
        ApplicationAction::Health,
        EmptyArguments {},
    )?;
    send(&mut stdin, &health)?;
    let health_reply = receive(&mut stdout)?;
    assert_eq!(health_reply["id"], 91);
    assert_eq!(
        health_reply["result"]["structuredContent"]["body"]["kind"],
        "health"
    );

    let null_id = request(
        JsonRpcRequestId::Null,
        92,
        ApplicationAction::Generate,
        GenerateArguments {
            profile: SourceProfile::Rust2024,
            stage: SourceStage::LowerIr,
            source: "fn null_id() {}",
        },
    )?;
    send(&mut stdin, &null_id)?;
    let null_reply = receive(&mut stdout)?;
    assert!(null_reply["id"].is_null());
    assert_eq!(
        null_reply["result"]["structuredContent"]["body"]["kind"],
        "dependency_unavailable"
    );

    drop(stdin);
    let status = child.wait()?;
    assert!(status.success());
    Ok(())
}

fn assert_golden_exchange(
    stdin: &mut ChildStdin,
    stdout: &mut BufReader<ChildStdout>,
    expected: &[golden_corpus::GoldenReply],
) -> Result<(), TestError> {
    let generated = generate(stdin, stdout, 101, "fn corpus() {}")?;
    let generated =
        golden_corpus::decode_mcp(&serde_json::to_vec(&generated)?).map_err(TestError::Golden)?;
    assert_eq!(generated.id, golden_corpus::GoldenResponseId::Number(101));
    assert_eq!(generated.result.structured_content, expected[0]);

    let health = effect(
        stdin,
        stdout,
        JsonRpcRequestId::Number(102),
        102,
        ApplicationAction::Health,
        EmptyArguments {},
    )?;
    let health =
        golden_corpus::decode_mcp(&serde_json::to_vec(&health)?).map_err(TestError::Golden)?;
    assert_eq!(health.id, golden_corpus::GoldenResponseId::Number(102));
    assert_eq!(health.result.structured_content, expected[1]);

    let admitted = recover(
        stdin,
        stdout,
        JsonRpcRequestId::Text("golden-operation"),
        103,
    )?;
    let admitted =
        golden_corpus::decode_mcp(&serde_json::to_vec(&admitted)?).map_err(TestError::Golden)?;
    assert_eq!(
        admitted.id,
        golden_corpus::GoldenResponseId::Text("golden-operation".to_owned())
    );
    assert_eq!(admitted.result.structured_content, expected[2]);

    let cancellation_request = serde_json::to_value(CancellationNotification {
        jsonrpc: JsonRpcVersion::Version2,
        method: RpcMethod::CancelRequest,
        params: CancellationParams {
            request_id: "golden-operation",
        },
    })?;
    send(stdin, &cancellation_request)?;
    let cancelled_request = request(
        JsonRpcRequestId::Number(104),
        104,
        ApplicationAction::PollExecution,
        PollExecutionArguments {
            operation: OperationId::First,
        },
    )?;
    send(stdin, &cancelled_request)?;
    let cancelled = receive(stdout)?;
    let cancelled =
        golden_corpus::decode_mcp(&serde_json::to_vec(&cancelled)?).map_err(TestError::Golden)?;
    assert_eq!(cancelled.id, golden_corpus::GoldenResponseId::Number(104));
    assert_eq!(cancelled.result.structured_content, expected[3]);
    Ok(())
}

#[test]
fn deterministic_golden_corpus_matches_independent_service_and_mcp_process() -> Result<(), TestError>
{
    let expected = golden_corpus::direct_replies().map_err(TestError::Golden)?;
    let mut child = Command::new(env!("CARGO_BIN_EXE_interface-mcp"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| io::Error::other("MCP child did not retain stdin"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("MCP child did not retain stdout"))?;
    let mut stdout = BufReader::new(stdout);
    assert_golden_exchange(&mut stdin, &mut stdout, &expected)?;

    drop(stdin);
    let status = child.wait()?;
    assert!(status.success());
    Ok(())
}
