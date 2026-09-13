#![allow(missing_docs)]

use std::error::Error;
use std::io;
use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

type TestResult = Result<(), Box<dyn Error>>;

fn invoke(arguments: &[&str]) -> Result<Value, Box<dyn Error>> {
    let output = Command::new(env!("CARGO_BIN_EXE_backend-control"))
        .args(arguments)
        .output()?;
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "backend-control exited {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        ))
        .into());
    }
    Ok(serde_json::from_slice(&output.stdout)?)
}

fn invoke_failure(arguments: &[&str]) -> Result<String, Box<dyn Error>> {
    let output = Command::new(env!("CARGO_BIN_EXE_backend-control"))
        .args(arguments)
        .output()?;
    if output.status.success() {
        return Err(io::Error::other("backend-control unexpectedly accepted command").into());
    }
    Ok(String::from_utf8(output.stderr)?)
}

fn field<'a>(value: &'a Value, path: &[&str]) -> Result<&'a str, Box<dyn Error>> {
    let mut current = value;
    for segment in path {
        current = match current {
            Value::Object(object) => object
                .get(*segment)
                .ok_or_else(|| io::Error::other(format!("missing JSON field {segment}")))?,
            Value::Array(array) => segment
                .parse::<usize>()
                .ok()
                .and_then(|index| array.get(index))
                .ok_or_else(|| io::Error::other(format!("missing JSON index {segment}")))?,
            _ => return Err(io::Error::other("JSON path crosses a scalar").into()),
        };
    }
    current
        .as_str()
        .ok_or_else(|| io::Error::other(format!("JSON field {path:?} is not a string")).into())
}

fn ledger_path(label: &str) -> Result<PathBuf, Box<dyn Error>> {
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    Ok(std::env::temp_dir().join(format!(
        "backend-control-cli-{label}-{}-{stamp}",
        std::process::id(),
    )))
}

#[test]
fn executable_adapters_publish_and_reuse_a_reviewed_work_item() -> TestResult {
    let ledger = ledger_path("lifecycle")?;
    let ledger = ledger.to_string_lossy().into_owned();
    let spec = r#"{"cell":"cli-cell","role":"implement","model_class":"local","effort":"high","toolchain_root":"toolchain","input_digest":"input"}"#;

    let init = invoke(&["init", "--ledger", &ledger])?;
    assert_eq!(init.get("initialized"), Some(&Value::Bool(true)));
    let key = field(&invoke(&["key", "--json", spec])?, &["work_key"])?.to_owned();

    let planned = invoke(&["plan", "--ledger", &ledger, "--json", spec])?;
    assert_eq!(field(&planned, &["result", "kind"])?, "admitted");

    let admitted = invoke(&[
        "admit", "--ledger", &ledger, "--key", &key, "--owner", "worker",
    ])?;
    let fence = field(&admitted, &["admission", "lease", "fence"])?.to_owned();
    let renewed = invoke(&[
        "renew", "--ledger", &ledger, "--key", &key, "--fence", &fence,
    ])?;
    let fence = field(&renewed, &["lease", "fence"])?.to_owned();

    let frozen = invoke(&[
        "candidate",
        "--ledger",
        &ledger,
        "--key",
        &key,
        "--fence",
        &fence,
        "--output",
        "6f7574707574",
        "--evidence",
        "65766964656e6365",
    ])?;
    assert_eq!(field(&frozen, &["status"])?, "frozen");
    let evaluated = invoke(&[
        "evaluation",
        "--ledger",
        &ledger,
        "--key",
        &key,
        "--fence",
        &fence,
        "--evaluator",
        "evaluator",
        "--verdict",
        "passed",
        "--evidence",
        "6576616c756174696f6e",
    ])?;
    assert_eq!(field(&evaluated, &["status"])?, "evaluated");
    field(&evaluated, &["evaluation"])?;
    let before_review = invoke(&["plan", "--ledger", &ledger, "--json", spec])?;
    assert_eq!(field(&before_review, &["result", "kind"])?, "existing");

    let reviewed = invoke(&[
        "review",
        "--ledger",
        &ledger,
        "--key",
        &key,
        "--reviewer",
        "reviewer",
        "--verdict",
        "accepted",
        "--evidence",
        "726576696577",
    ])?;
    assert_eq!(field(&reviewed, &["status"])?, "reviewed");
    field(&reviewed, &["review"])?;
    let before_decision = invoke(&["plan", "--ledger", &ledger, "--json", spec])?;
    assert_eq!(field(&before_decision, &["result", "kind"])?, "existing");
    let decided = invoke(&[
        "decision",
        "--ledger",
        &ledger,
        "--key",
        &key,
        "--sol",
        "sol",
        "--verdict",
        "promoted",
        "--evidence",
        "6465636973696f6e",
    ])?;
    assert_eq!(field(&decided, &["status"])?, "completed");

    let reused = invoke(&["plan", "--ledger", &ledger, "--json", spec])?;
    assert_eq!(field(&reused, &["result", "kind"])?, "reused");
    let status = invoke(&["status", "--ledger", &ledger])?;
    assert_eq!(field(&status, &["records", "0", "status"])?, "completed");
    Ok(())
}

#[test]
fn executable_rejects_an_evaluator_that_owns_the_candidate() -> TestResult {
    let ledger = ledger_path("independence")?;
    let ledger = ledger.to_string_lossy().into_owned();
    let spec = r#"{"cell":"separation-cell","role":"implement","model_class":"local","effort":"high","toolchain_root":"toolchain","input_digest":"input"}"#;
    invoke(&["init", "--ledger", &ledger])?;
    let key = field(&invoke(&["key", "--json", spec])?, &["work_key"])?.to_owned();
    invoke(&["apply", "--ledger", &ledger, "--json", spec])?;
    let admitted = invoke(&[
        "admit", "--ledger", &ledger, "--key", &key, "--owner", "worker",
    ])?;
    let fence = field(&admitted, &["admission", "lease", "fence"])?.to_owned();
    invoke(&[
        "candidate",
        "--ledger",
        &ledger,
        "--key",
        &key,
        "--fence",
        &fence,
        "--output",
        "6f7574707574",
        "--evidence",
        "65766964656e6365",
    ])?;
    let error = invoke_failure(&[
        "evaluation",
        "--ledger",
        &ledger,
        "--key",
        &key,
        "--fence",
        &fence,
        "--evaluator",
        "worker",
        "--verdict",
        "passed",
        "--evidence",
        "6576616c756174696f6e",
    ])?;
    assert!(error.contains("authorities must be independent"));
    Ok(())
}
