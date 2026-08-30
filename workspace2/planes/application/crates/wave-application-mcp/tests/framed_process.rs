use std::{
    io::{self, BufReader, Write},
    process::{ChildStdin, ChildStdout, Command, Stdio},
};

use serde_json::{Value, json};
use wave_application_protocol::{read_frame, write_frame};

fn request(id: u64, action: &str, arguments: &Value) -> io::Result<Value> {
    let request_arguments = arguments.as_object().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "MCP test arguments must be a JSON object",
        )
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

fn send(writer: &mut impl Write, value: &Value) -> io::Result<()> {
    let body = serde_json::to_vec(value).map_err(io::Error::other)?;
    write_frame(writer, &body)
}

fn receive(reader: &mut BufReader<impl io::Read>) -> io::Result<Value> {
    let body = read_frame(reader)?.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "MCP child closed before its framed response",
        )
    })?;
    serde_json::from_slice(&body).map_err(io::Error::other)
}

fn generate(
    stdin: &mut ChildStdin,
    stdout: &mut BufReader<ChildStdout>,
    id: u64,
    source: &str,
) -> Result<Value, Box<dyn std::error::Error>> {
    let generation = request(
        id,
        "generate",
        &json!({"language": "rust", "stage": "parse", "package": "mcp-package", "source": source}),
    )?;
    send(stdin, &generation)?;
    receive(stdout).map_err(Into::into)
}

#[test]
fn framed_mcp_process_preserves_structured_results_and_named_cancellation()
-> Result<(), Box<dyn std::error::Error>> {
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

    let generated = generate(&mut stdin, &mut stdout, 81, "fn mcp() {}")?;
    let structured = &generated["result"]["structuredContent"];
    assert_eq!(structured["correlation"], 81);
    assert_eq!(structured["body"]["kind"], "generated");
    assert_eq!(structured["body"]["package"], "mcp-package");
    assert_eq!(structured["body"]["output"], "fn mcp() {}");
    assert_eq!(structured["terminal"]["kind"], "complete");

    let second_generated = generate(&mut stdin, &mut stdout, 811, "fn mcp_second() {}")?;
    let second_structured = &second_generated["result"]["structuredContent"];
    assert_eq!(second_structured["body"]["output"], "fn mcp_second() {}");
    assert_ne!(
        structured["body"]["output"],
        second_structured["body"]["output"]
    );

    let begin_request = request(82, "begin-progress", &json!({}))?;
    send(&mut stdin, &begin_request)?;
    let admitted = receive(&mut stdout)?;
    assert_eq!(
        admitted["result"]["structuredContent"]["body"]["kind"],
        "progress_started"
    );

    let cancellation_request = json!({
        "jsonrpc": "2.0",
        "method": "$/cancelRequest",
        "params": {"requestId": 82},
    });
    send(&mut stdin, &cancellation_request)?;

    let progress_request = request(83, "progress", &json!({"cursor": "start"}))?;
    send(&mut stdin, &progress_request)?;
    let progress = receive(&mut stdout)?;
    assert_eq!(progress["id"], 83);
    let page = &progress["result"]["structuredContent"]["body"]["page"];
    assert_eq!(page["kind"], "terminal");
    assert_eq!(page["terminal"]["kind"], "cancelled");

    let malformed = json!({
        "jsonrpc": "2.0",
        "id": 84,
        "method": "tools/call",
        "params": {"name": "nudox.application"},
    });
    send(&mut stdin, &malformed)?;
    let error = receive(&mut stdout)?;
    assert_eq!(error["id"], 84);
    assert_eq!(error["error"]["data"]["code"], "missing_field");

    drop(stdin);
    let status = child.wait()?;
    assert!(status.success());
    Ok(())
}
