use std::{
    error::Error,
    fmt,
    io::{self, BufReader, Write},
    process::{ChildStdin, ChildStdout, Command, Stdio},
};

use serde_json::{Value, json};
use wave_application_core::{CapabilityDomain, ContentId, GenerationId, IndexSnapshotId};
use wave_application_protocol::{read_frame, write_frame};

#[derive(Debug)]
enum TestError {
    Io(io::Error),
    Json(serde_json::Error),
}

impl fmt::Display for TestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(source) => source.fmt(formatter),
            Self::Json(source) => source.fmt(formatter),
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

fn request(
    id: &Value,
    correlation: u64,
    action: &str,
    arguments: &Value,
) -> Result<Value, TestError> {
    let request_arguments = arguments.as_object().ok_or_else(|| {
        TestError::Io(io::Error::new(
            io::ErrorKind::InvalidInput,
            "MCP test arguments must be a JSON object",
        ))
    })?;
    let mut request_arguments = request_arguments.clone();
    request_arguments.insert("action".to_owned(), json!(action));
    request_arguments.insert("correlation".to_owned(), json!(correlation));
    Ok(json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "tools/call",
        "params": {
            "name": "nudox.application",
            "arguments": request_arguments,
        },
    }))
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
        &json!(id),
        id,
        "generate",
        &json!({"language": "rust", "stage": "parse", "package": "mcp-package", "source": source}),
    )?;
    send(stdin, &generation)?;
    receive(stdout)
}

fn policy_arguments() -> Value {
    json!({
        "generation": GenerationId::from_digest([11; 32]).to_string(),
        "snapshot": IndexSnapshotId::from_digest([13; 32]).to_string(),
        "bundle": ContentId::<CapabilityDomain>::from_digest([17; 32]).to_string(),
        "ram_free": 4096,
        "nvme_free": 8192,
        "operations": 1,
        "retries": 1,
        "memory_pressure": "relaxed",
        "storage_pressure": "relaxed",
        "battery": "normal",
    })
}

fn effect(
    stdin: &mut ChildStdin,
    stdout: &mut BufReader<ChildStdout>,
    id: &Value,
    correlation: u64,
    action: &str,
    arguments: &Value,
) -> Result<Value, TestError> {
    let begin_request = request(id, correlation, action, arguments)?;
    send(stdin, &begin_request)?;
    receive(stdout)
}

fn recover(
    stdin: &mut ChildStdin,
    stdout: &mut BufReader<ChildStdout>,
    id: &Value,
    correlation: u64,
) -> Result<Value, TestError> {
    effect(
        stdin,
        stdout,
        id,
        correlation,
        "recover-local",
        &policy_arguments(),
    )
}

fn release_arguments() -> Value {
    json!({
        "generation": GenerationId::from_digest([11; 32]).to_string(),
        "snapshot": IndexSnapshotId::from_digest([13; 32]).to_string(),
        "bundle": ContentId::<CapabilityDomain>::from_digest([17; 32]).to_string(),
        "ram_free": 4096,
        "nvme_free": 8192,
        "operations": 1,
        "retries": 0,
        "memory_pressure": "relaxed",
        "storage_pressure": "relaxed",
        "battery": "critical",
    })
}

fn assert_generation(
    stdin: &mut ChildStdin,
    stdout: &mut BufReader<ChildStdout>,
    id: u64,
    source: &str,
) -> Result<String, TestError> {
    let generated = generate(stdin, stdout, id, source)?;
    let structured = &generated["result"]["structuredContent"];
    assert_eq!(structured["correlation"], id);
    assert_eq!(structured["body"]["kind"], "compiler_passthrough");
    assert_eq!(structured["body"]["package"], "mcp-package");
    assert_eq!(structured["body"]["source"], source);
    assert_eq!(structured["terminal"]["kind"], "partial");
    structured["body"]["source"]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| TestError::Io(io::Error::new(io::ErrorKind::InvalidData, "source missing")))
}

fn assert_first_completed(
    stdin: &mut ChildStdin,
    stdout: &mut BufReader<ChildStdout>,
) -> Result<(), TestError> {
    let admitted = recover(stdin, stdout, &json!(82), 82)?;
    assert_eq!(
        admitted["result"]["structuredContent"]["body"]["kind"],
        "execution_started"
    );

    let pending_request = request(&json!(83), 83, "poll-execution", &json!({"operation": "1"}))?;
    send(stdin, &pending_request)?;
    let pending = receive(stdout)?;
    assert_eq!(pending["id"], 83);
    assert_eq!(
        pending["result"]["structuredContent"]["body"]["state"]["kind"],
        "pending"
    );

    let completed_request = request(&json!(84), 84, "poll-execution", &json!({"operation": "1"}))?;
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
        &json!("second-operation"),
        85,
        "release-local",
        &release_arguments(),
    )?;
    assert_eq!(
        admitted["result"]["structuredContent"]["body"]["kind"],
        "execution_started"
    );

    let cancellation_request = json!({
        "jsonrpc": "2.0",
        "method": "$/cancelRequest",
        "params": {"requestId": "second-operation"},
    });
    send(stdin, &cancellation_request)?;

    let progress_request = request(&json!(86), 86, "poll-execution", &json!({"operation": "2"}))?;
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
    let malformed = json!({
        "jsonrpc": "2.0",
        "id": 87,
        "method": "tools/call",
        "params": {"name": "nudox.application"},
    });
    send(stdin, &malformed)?;
    let error = receive(stdout)?;
    assert_eq!(error["id"], 87);
    assert_eq!(error["error"]["data"]["code"], "missing_field");
    Ok(())
}

#[test]
fn framed_mcp_process_preserves_structured_results_and_named_cancellation() -> Result<(), TestError>
{
    let mut child = Command::new(env!("CARGO_BIN_EXE_wave-application-mcp"))
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

    let first_source = assert_generation(&mut stdin, &mut stdout, 81, "fn mcp() {}")?;
    let second_source = assert_generation(&mut stdin, &mut stdout, 811, "fn mcp_second() {}")?;
    assert_ne!(first_source, second_source);
    assert_first_completed(&mut stdin, &mut stdout)?;
    assert_cancelled(&mut stdin, &mut stdout)?;
    assert_malformed(&mut stdin, &mut stdout)?;

    drop(stdin);
    let status = child.wait()?;
    assert!(status.success());
    Ok(())
}
