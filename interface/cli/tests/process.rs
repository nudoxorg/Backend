//! Exercises the `interface-cli` tests process contract through its observable boundary.
//! The cases target malformed, partial, reordered, and resource-constrained behavior.
//! Assertions retain exact typed causes so regressions cannot pass through lossy errors.
use std::{error::Error, fmt, io, process::Command};

use interface_core::{CapabilityDomain, ContentId, GenerationId, IndexSnapshotId};
use serde_json::Value;

// The shared corpus also exposes MCP envelope decoding for the sibling process test.
#[allow(dead_code)]
#[path = "../../protocol/tests/support/golden_corpus.rs"]
mod golden_corpus;

#[derive(Debug)]
enum CliTestError {
    Io(io::Error),
    Json(serde_json::Error),
    Golden(golden_corpus::GoldenError),
    MissingLine(usize),
}

impl fmt::Display for CliTestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(source) => source.fmt(formatter),
            Self::Json(source) => source.fmt(formatter),
            Self::Golden(source) => source.fmt(formatter),
            Self::MissingLine(index) => write!(formatter, "missing JSON output line {index}"),
        }
    }
}

impl Error for CliTestError {}

impl From<io::Error> for CliTestError {
    fn from(source: io::Error) -> Self {
        Self::Io(source)
    }
}

impl From<serde_json::Error> for CliTestError {
    fn from(source: serde_json::Error) -> Self {
        Self::Json(source)
    }
}

fn run(arguments: &[&str]) -> Result<std::process::Output, io::Error> {
    Command::new(env!("CARGO_BIN_EXE_interface-cli"))
        .args(arguments)
        .output()
}

fn stdout_json(output: &std::process::Output, line: usize) -> Result<Value, CliTestError> {
    let bytes = output
        .stdout
        .split(|byte| *byte == b'\n')
        .filter(|candidate| !candidate.is_empty())
        .nth(line)
        .ok_or(CliTestError::MissingLine(line))?;
    Ok(serde_json::from_slice(bytes)?)
}

#[test]
fn child_process_preserves_honest_compiler_unavailable_terminal() -> Result<(), CliTestError> {
    let output = run(&["generate", "71", "rust-2024", "lower-ir", "fn cli() {}"])?;
    assert!(output.status.success());
    let reply = stdout_json(&output, 0)?;
    assert_eq!(reply["correlation"], 71);
    assert_eq!(reply["terminal"]["kind"], "degraded");
    assert_eq!(reply["terminal"]["unavailable"], "compiler_output");
    assert_eq!(reply["body"]["kind"], "dependency_unavailable");
    assert_eq!(reply["body"]["capability"], "compiler_output");
    assert_eq!(reply["diagnostic"], Value::Null);
    Ok(())
}

#[test]
fn one_process_preserves_the_real_adaptive_future_across_commands() -> Result<(), CliTestError> {
    let generation = GenerationId::from_digest([11; 32]).to_string();
    let snapshot = IndexSnapshotId::from_digest([13; 32]).to_string();
    let bundle = ContentId::<CapabilityDomain>::from_digest([17; 32]).to_string();
    let output = run(&[
        "recover-local",
        "80",
        &generation,
        &snapshot,
        &bundle,
        "4096",
        "8192",
        "1",
        "1",
        "relaxed",
        "relaxed",
        "normal",
        "--",
        "poll-execution",
        "81",
        "1",
        "--",
        "poll-execution",
        "82",
        "1",
        "--",
        "health",
        "83",
    ])?;
    assert!(output.status.success());
    assert_eq!(
        stdout_json(&output, 0)?["body"]["kind"],
        "execution_started"
    );
    assert_eq!(stdout_json(&output, 1)?["body"]["state"]["kind"], "pending");
    assert_eq!(
        stdout_json(&output, 2)?["body"]["state"]["kind"],
        "completed"
    );
    let health = stdout_json(&output, 3)?;
    assert!(health["body"]["facts"].as_array().is_some_and(|facts| {
        facts
            .iter()
            .any(|fact| fact["capability"] == "local_analyzer" && fact["state"] == "local_ready")
    }));
    Ok(())
}

#[test]
fn child_process_distinguishes_business_diagnostic_from_transport_diagnostic()
-> Result<(), CliTestError> {
    let semantic = run(&["generate", "72", "rust-2024", "parse", "fn demo() {}"])?;
    assert_eq!(semantic.status.code(), Some(2));
    let semantic_reply = stdout_json(&semantic, 0)?;
    assert_eq!(semantic_reply["terminal"]["kind"], "failed");
    assert_eq!(
        semantic_reply["diagnostic"]["code"],
        "unsupported_compiler_stage"
    );

    let transport = run(&["search", "not-a-number", "primary", "render", "2"])?;
    assert_eq!(transport.status.code(), Some(64));
    assert_eq!(
        stdout_json(&transport, 0)?["adapter_error"]["field"],
        "correlation"
    );

    let too_many = run(&[
        "generate",
        "73",
        "rust-2024",
        "parse",
        "fn demo() {}",
        "excess",
    ])?;
    assert_eq!(too_many.status.code(), Some(64));
    let too_many_reply = stdout_json(&too_many, 0)?;
    assert_eq!(too_many_reply["adapter_error"]["code"], "too_many_fields");
    assert_eq!(too_many_reply["adapter_error"]["actual"], 6);
    Ok(())
}

#[test]
fn deterministic_golden_corpus_matches_independent_service_and_cli_process()
-> Result<(), CliTestError> {
    let expected = golden_corpus::direct_replies().map_err(CliTestError::Golden)?;
    let commands = golden_corpus::command_arguments();
    let mut arguments = Vec::new();
    for (index, command) in commands.iter().enumerate() {
        if index != 0 {
            arguments.push("--".to_owned());
        }
        arguments.extend(command.iter().cloned());
    }
    let references = arguments.iter().map(String::as_str).collect::<Vec<_>>();
    let output = run(&references)?;
    assert!(output.status.success());
    for (line, expected) in expected.iter().enumerate() {
        let actual = stdout_golden(&output, line)?;
        assert_eq!(&actual, expected);
    }
    Ok(())
}

fn stdout_golden(
    output: &std::process::Output,
    line: usize,
) -> Result<golden_corpus::GoldenReply, CliTestError> {
    let bytes = output
        .stdout
        .split(|byte| *byte == b'\n')
        .filter(|candidate| !candidate.is_empty())
        .nth(line)
        .ok_or(CliTestError::MissingLine(line))?;
    golden_corpus::decode_reply(bytes).map_err(CliTestError::Golden)
}
