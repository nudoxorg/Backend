//! Exercises the `interface-mcp` framed-process contract through its observable boundary.
//! The cases target handshake, registry, tool content, malformed arguments, and frame bounds.
//! Assertions retain exact typed causes so regressions cannot pass through lossy errors.
//!
//! Every case drives the real `nudox-mcp` binary over real frames against a temporary workspace
//! root with the compiler detached. Nothing here inspects an internal value: what is asserted is
//! the text and JSON a client actually receives, because a renderer that is only tested through
//! its own types can be wrong in exactly the way a reader would notice first.

use std::{
    error::Error,
    fmt,
    io::{self, BufRead as _, BufReader, Write},
    path::PathBuf,
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
};

use interface_protocol::{read_frame_bounded, write_frame_bounded};
use serde_json::{Value, json};

const RESPONSE_BOUND: usize = 4 * 1024 * 1024;

#[derive(Debug)]
enum TestError {
    Io(io::Error),
    Json(serde_json::Error),
    Shape(String),
}

impl fmt::Display for TestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(source) => source.fmt(formatter),
            Self::Json(source) => source.fmt(formatter),
            Self::Shape(detail) => formatter.write_str(detail),
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

/// One live server process over a temporary workspace root with no compiler attached.
struct Session {
    child: Child,
    input: BufReader<ChildStdout>,
    output: ChildStdin,
    root: PathBuf,
    next_id: u64,
}

impl Session {
    fn start(name: &str) -> Result<Self, TestError> {
        let root = std::env::temp_dir().join(format!(
            "nudox-mcp-{name}-{}-{}",
            std::process::id(),
            name.len()
        ));
        std::fs::create_dir_all(&root)?;
        let mut child = Command::new(env!("CARGO_BIN_EXE_nudox-mcp"))
            .env("NUDOX_DATA_ROOT", &root)
            .env("NUDOX_MCP_DETACHED", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let output = child
            .stdin
            .take()
            .ok_or_else(|| TestError::Shape("the child has no stdin".to_owned()))?;
        let input = BufReader::new(
            child
                .stdout
                .take()
                .ok_or_else(|| TestError::Shape("the child has no stdout".to_owned()))?,
        );
        Ok(Self {
            child,
            input,
            output,
            root,
            next_id: 1,
        })
    }

    fn send(&mut self, body: &Value) -> Result<(), TestError> {
        let bytes = serde_json::to_vec(body)?;
        write_frame_bounded(&mut self.output, &bytes, RESPONSE_BOUND)?;
        Ok(())
    }

    fn receive(&mut self) -> Result<Value, TestError> {
        let Some(frame) = read_frame_bounded(&mut self.input, RESPONSE_BOUND)? else {
            return Err(TestError::Shape("the server closed without answering".to_owned()));
        };
        Ok(serde_json::from_slice(&frame)?)
    }

    /// Sends one request and returns its `result`, failing loudly on a JSON-RPC error.
    fn request(&mut self, method: &str, params: Value) -> Result<Value, TestError> {
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        self.send(&json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}))?;
        let response = self.receive()?;
        if response.get("id") != Some(&json!(id)) {
            return Err(TestError::Shape(format!("answered the wrong request: {response}")));
        }
        response
            .get("result")
            .cloned()
            .ok_or_else(|| TestError::Shape(format!("no result in {response}")))
    }

    /// Calls one tool and returns its Markdown text with its `isError` flag.
    fn call(&mut self, tool: &str, arguments: Value) -> Result<(String, bool), TestError> {
        let result = self.request("tools/call", json!({"name":tool,"arguments":arguments}))?;
        let text = result
            .pointer("/content/0/text")
            .and_then(Value::as_str)
            .ok_or_else(|| TestError::Shape(format!("no text content in {result}")))?
            .to_owned();
        let is_error = result
            .get("isError")
            .and_then(Value::as_bool)
            .ok_or_else(|| TestError::Shape(format!("no isError in {result}")))?;
        Ok((text, is_error))
    }

    fn stderr(&mut self) -> String {
        self.child.stderr.take().map_or_else(String::new, |stream| {
            BufReader::new(stream)
                .lines()
                .map_while(Result::ok)
                .collect::<Vec<String>>()
                .join("\n")
        })
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn handshake(name: &str) -> Result<Session, TestError> {
    let mut session = Session::start(name)?;
    let result = session.request(
        "initialize",
        json!({"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"test","version":"0"}}),
    )?;
    if result.get("protocolVersion") != Some(&json!("2025-06-18")) {
        return Err(TestError::Shape(format!("bad negotiation: {result}")));
    }
    session.send(&json!({"jsonrpc":"2.0","method":"notifications/initialized"}))?;
    Ok(session)
}

#[test]
fn initialize_teaches_the_shelf_first_workflow_and_the_fingerprint_rule(
) -> Result<(), Box<dyn Error>> {
    let mut session = Session::start("initialize")?;
    let result = session.request("initialize", json!({"protocolVersion":"2025-06-18"}))?;
    let instructions = result
        .get("instructions")
        .and_then(Value::as_str)
        .ok_or_else(|| TestError::Shape("no instructions".to_owned()))?;
    assert!(
        instructions.contains("`packages` first"),
        "instructions must send a reader to the shelf first: {instructions}"
    );
    assert!(
        instructions.contains("fingerprint, never input"),
        "instructions must say the 8-hex is not an input: {instructions}"
    );
    assert!(
        instructions.contains("it runs a compiler"),
        "instructions must warn that add is slow: {instructions}"
    );
    assert_eq!(
        result.pointer("/serverInfo/name"),
        Some(&json!("nudox")),
        "the server names itself"
    );
    assert!(result.pointer("/capabilities/resources").is_some());
    Ok(())
}

#[test]
fn an_unknown_protocol_revision_is_answered_with_the_one_this_server_speaks(
) -> Result<(), Box<dyn Error>> {
    let mut session = Session::start("negotiate")?;
    let result = session.request("initialize", json!({"protocolVersion":"2099-01-01"}))?;
    assert_eq!(result.get("protocolVersion"), Some(&json!("2025-06-18")));
    Ok(())
}

#[test]
fn tools_list_publishes_exactly_the_twelve_registry_rows() -> Result<(), Box<dyn Error>> {
    let mut session = handshake("tools")?;
    let result = session.request("tools/list", json!({}))?;
    let tools = result
        .get("tools")
        .and_then(Value::as_array)
        .ok_or_else(|| TestError::Shape("no tools array".to_owned()))?;
    let names: Vec<&str> = tools
        .iter()
        .filter_map(|tool| tool.get("name").and_then(Value::as_str))
        .collect();
    assert_eq!(
        names,
        [
            "packages",
            "add",
            "remove",
            "show",
            "outline",
            "resolve",
            "search",
            "graph",
            "health",
            "index-search",
            "package-versions",
            "package-profile"
        ],
        "tools/list is the registry in registry order"
    );
    for (tool, spec) in tools.iter().zip(interface_library::COMMANDS) {
        let description = tool
            .get("description")
            .and_then(Value::as_str)
            .ok_or_else(|| TestError::Shape("a tool has no description".to_owned()))?;
        assert!(
            description.starts_with(spec.description),
            "{} must lead with the registry sentence verbatim, got {description}",
            spec.name
        );
        assert!(
            description.len() > spec.description.len() + 20,
            "{} must add its own guidance sentence",
            spec.name
        );
    }
    Ok(())
}

#[test]
fn an_empty_shelf_says_so_and_hands_back_the_call_that_would_fill_it(
) -> Result<(), Box<dyn Error>> {
    let mut session = handshake("empty")?;
    let (text, is_error) = session.call("packages", json!({}))?;
    assert!(!is_error, "an empty shelf is a fact, not a failure");
    assert!(text.contains("(no packages)"), "got: {text}");
    assert!(
        text.contains("→ add {\"package\":\"pkg:cargo/serde@1.0.196\"}"),
        "the empty shelf must teach the exact add call: {text}"
    );
    Ok(())
}

#[test]
fn add_without_a_compiler_names_the_refused_url_and_the_capability_to_check(
) -> Result<(), Box<dyn Error>> {
    let mut session = handshake("detached")?;
    let (text, is_error) = session.call("add", json!({"package":"pkg:cargo/serde@1.0.196"}))?;
    assert!(is_error, "a refused add is flagged: {text}");
    assert!(
        text.contains("✗ compiler-detached pkg:cargo/serde@1.0.196"),
        "the fault must name the slug and the refused url verbatim: {text}"
    );
    assert!(
        text.contains("→ health {}"),
        "the affordance must be the call that explains it: {text}"
    );
    Ok(())
}

#[test]
fn show_on_an_absent_package_names_the_exact_coordinate_and_offers_the_add(
) -> Result<(), Box<dyn Error>> {
    let mut session = handshake("absent")?;
    let (text, is_error) = session.call(
        "show",
        json!({"address":"cargo:serde@1.0.196::de::Deserializer[trait]"}),
    )?;
    assert!(is_error);
    assert!(
        text.contains("✗ package-not-on-shelf cargo:serde@1.0.196"),
        "got: {text}"
    );
    assert!(
        text.contains("absent here never means absent everywhere"),
        "got: {text}"
    );
    assert!(
        text.contains("→ add {\"package\":\"pkg:cargo/serde@1.0.196\"}"),
        "got: {text}"
    );
    Ok(())
}

#[test]
fn search_states_every_lane_and_its_reason_before_any_row() -> Result<(), Box<dyn Error>> {
    let mut session = handshake("lanes")?;
    let (text, is_error) = session.call("search", json!({"query":"deserialize"}))?;
    assert!(!is_error, "a search with no rows is still an answer: {text}");
    let lanes = text
        .lines()
        .find(|line| line.starts_with("~lanes "))
        .ok_or_else(|| TestError::Shape(format!("no ~lanes line in {text}")))?;
    for lane in ["exact", "names", "graph", "semantic"] {
        assert!(lanes.contains(lane), "{lane} is missing from {lanes}");
    }
    assert!(
        lanes.contains("no-packages"),
        "an empty shelf is why every lane is down, and the line must say so: {lanes}"
    );
    assert!(text.contains("(no matches)"), "got: {text}");
    Ok(())
}

#[test]
fn a_misspelled_argument_is_named_with_the_fields_that_exist() -> Result<(), Box<dyn Error>> {
    let mut session = handshake("fields")?;
    let (text, is_error) = session.call("add", json!({"packages":"serde"}))?;
    assert!(is_error);
    assert!(text.contains("✗ unknown-argument packages"), "got: {text}");
    assert!(text.contains("this tool accepts package"), "got: {text}");

    let (text, is_error) = session.call("show", json!({"address":"serde::de"}))?;
    assert!(is_error);
    assert!(text.contains("✗ address serde::de"), "got: {text}");
    assert!(
        text.contains("no `ecosystem:` prefix"),
        "the fault must name the part that was missing: {text}"
    );
    Ok(())
}

#[test]
fn the_schema_card_reads_back_with_its_worked_calls() -> Result<(), Box<dyn Error>> {
    let mut session = handshake("card")?;
    let listed = session.request("resources/list", json!({}))?;
    let uris: Vec<&str> = listed
        .get("resources")
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter_map(|row| row.get("uri").and_then(Value::as_str))
                .collect()
        })
        .unwrap_or_default();
    assert!(uris.contains(&"nudox://packages"), "got: {uris:?}");
    assert!(uris.contains(&"nudox://schema-card"), "got: {uris:?}");

    let result = session.request("resources/read", json!({"uri":"nudox://schema-card"}))?;
    let text = result
        .pointer("/contents/0/text")
        .and_then(Value::as_str)
        .ok_or_else(|| TestError::Shape(format!("no card text in {result}")))?;
    assert!(text.contains("# nudox schema card"), "got: {text}");
    assert!(text.contains("`called-by`"), "the card names both directions");
    assert!(
        text.contains("never accepted as input"),
        "the card must state the fingerprint rule"
    );
    assert_eq!(
        interface_mcp::card::worked_examples(text).len(),
        3,
        "the card carries three worked calls"
    );
    Ok(())
}

#[test]
fn an_unknown_resource_names_what_this_server_does_publish() -> Result<(), Box<dyn Error>> {
    let mut session = handshake("resource")?;
    session.send(&json!({
        "jsonrpc":"2.0","id":900,"method":"resources/read",
        "params":{"uri":"nudox://invented"}
    }))?;
    let response = session.receive()?;
    let message = response
        .pointer("/error/message")
        .and_then(Value::as_str)
        .ok_or_else(|| TestError::Shape(format!("expected an error, got {response}")))?;
    assert!(message.contains("nudox://invented"), "got: {message}");
    assert!(message.contains("nudox://schema-card"), "got: {message}");
    Ok(())
}

#[test]
fn a_cancellation_for_an_unknown_request_is_ignored_and_the_session_continues(
) -> Result<(), Box<dyn Error>> {
    let mut session = handshake("cancel")?;
    session.send(&json!({
        "jsonrpc":"2.0","method":"notifications/cancelled",
        "params":{"requestId":4242,"reason":"user"}
    }))?;
    // A notification is never answered; the next request proves the stream stayed in step.
    let (text, _) = session.call("health", json!({}))?;
    assert!(text.contains("compiler"), "got: {text}");
    assert!(
        text.contains("detached"),
        "health must report this process has no compiler: {text}"
    );
    Ok(())
}

#[test]
fn an_oversized_request_frame_is_refused_before_it_is_parsed() -> Result<(), Box<dyn Error>> {
    let mut session = handshake("bounds")?;
    let oversized = 64 * 1024 + 1;
    write!(
        session.output,
        "Content-Length: {oversized}\r\n\r\n"
    )?;
    session.output.flush()?;
    let closed = session.receive().is_err();
    assert!(closed, "an oversized frame must not be answered");
    let stderr = session.stderr();
    assert!(
        stderr.is_empty() || stderr.contains("exceeds"),
        "the refusal must name the bound it broke: {stderr}"
    );
    Ok(())
}

#[test]
fn an_unimplemented_method_is_named_rather_than_silently_dropped() -> Result<(), Box<dyn Error>> {
    let mut session = handshake("method")?;
    session.send(&json!({"jsonrpc":"2.0","id":77,"method":"prompts/list"}))?;
    let response = session.receive()?;
    assert_eq!(response.pointer("/error/code"), Some(&json!(-32_601)));
    let message = response
        .pointer("/error/message")
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert!(message.contains("prompts/list"), "got: {message}");
    Ok(())
}
