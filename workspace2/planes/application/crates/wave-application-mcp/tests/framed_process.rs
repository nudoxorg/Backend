use std::{
    error::Error,
    fmt,
    io::{self, BufReader, Write},
    process::{ChildStdin, ChildStdout, Command, Stdio},
};

use serde_json::{Value, json};
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

fn request(id: u64, action: &str, arguments: &Value) -> Result<Value, TestError> {
    let request_arguments = arguments.as_object().ok_or_else(|| {
        TestError::Io(io::Error::new(
            io::ErrorKind::InvalidInput,
            "MCP test arguments must be a JSON object",
        ))
    })?;
    let mut request_arguments = request_arguments.clone();
    request_arguments.insert("action".to_owned(), json!(action));
    request_arguments.insert("correlation".to_owned(), json!(id));
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
        id,
        "generate",
        &json!({"language": "rust", "stage": "parse", "package": "mcp-package", "source": source}),
    )?;
    send(stdin, &generation)?;
    receive(stdout)
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

fn assert_cancelled(
    stdin: &mut ChildStdin,
    stdout: &mut BufReader<ChildStdout>,
) -> Result<(), TestError> {
    let begin_request = request(
        82,
        "recover-local",
        &json!({
            "generation": "application-generation",
            "snapshot": "application-snapshot",
            "bundle": "verified-analyzer-bundle",
            "ram_free": 4096,
            "nvme_free": 8192,
            "operations": 1,
            "retries": 1,
            "memory_pressure": "relaxed",
            "storage_pressure": "relaxed",
            "battery": "normal",
        }),
    )?;
    send(stdin, &begin_request)?;
    let admitted = receive(stdout)?;
    assert_eq!(
        admitted["result"]["structuredContent"]["body"]["kind"],
        "execution_started"
    );

    let cancellation_request = json!({
        "jsonrpc": "2.0",
        "method": "$/cancelRequest",
        "params": {"requestId": 82},
    });
    send(stdin, &cancellation_request)?;

    let progress_request = request(83, "poll-execution", &json!({"operation": "1"}))?;
    send(stdin, &progress_request)?;
    let progress = receive(stdout)?;
    assert_eq!(progress["id"], 83);
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
        "id": 84,
        "method": "tools/call",
        "params": {"name": "nudox.application"},
    });
    send(stdin, &malformed)?;
    let error = receive(stdout)?;
    assert_eq!(error["id"], 84);
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
    assert_cancelled(&mut stdin, &mut stdout)?;
    assert_malformed(&mut stdin, &mut stdout)?;

    drop(stdin);
    let status = child.wait()?;
    assert!(status.success());
    Ok(())
}
