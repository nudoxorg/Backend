use std::process::Command;

use serde_json::Value;

fn run(arguments: &[&str]) -> std::io::Result<std::process::Output> {
    Command::new(env!("CARGO_BIN_EXE_wave-application-cli"))
        .args(arguments)
        .output()
}

fn stdout_json(output: &std::process::Output) -> serde_json::Result<Value> {
    serde_json::from_slice(&output.stdout)
}

#[test]
fn child_process_returns_the_core_owned_real_compiler_reply()
-> Result<(), Box<dyn std::error::Error>> {
    let output = run(&[
        "generate",
        "71",
        "rust",
        "parse",
        "cli-package",
        "fn cli() {}",
    ])?;
    assert!(output.status.success());
    let reply = stdout_json(&output)?;
    assert_eq!(reply["correlation"], 71);
    assert_eq!(reply["terminal"]["kind"], "complete");
    assert_eq!(reply["body"]["kind"], "generated");
    assert_eq!(reply["body"]["package"], "cli-package");
    assert_eq!(reply["body"]["output"], "fn cli() {}");
    let second = run(&[
        "generate",
        "711",
        "rust",
        "parse",
        "cli-package",
        "fn cli_second() {}",
    ])?;
    assert!(second.status.success());
    let second_reply = stdout_json(&second)?;
    assert_eq!(second_reply["body"]["output"], "fn cli_second() {}");
    assert_ne!(reply["body"]["output"], second_reply["body"]["output"]);
    Ok(())
}

#[test]
fn child_process_distinguishes_business_diagnostic_from_transport_diagnostic()
-> Result<(), Box<dyn std::error::Error>> {
    let semantic = run(&["generate", "72", "go", "parse", "demo", "fn demo() {}"])?;
    assert_eq!(semantic.status.code(), Some(2));
    let semantic_reply = stdout_json(&semantic)?;
    assert_eq!(semantic_reply["terminal"]["kind"], "failed");
    assert_eq!(semantic_reply["diagnostic"]["code"], "unknown_language");
    assert_eq!(semantic_reply["diagnostic"]["detail"]["value"], "go");

    let transport = run(&["search", "not-a-number", "primary", "render", "2"])?;
    assert_eq!(transport.status.code(), Some(64));
    let transport_reply = stdout_json(&transport)?;
    assert_eq!(transport_reply["adapter_error"]["field"], "correlation");

    let too_many = run(&[
        "generate",
        "73",
        "rust",
        "parse",
        "demo",
        "fn demo() {}",
        "excess",
    ])?;
    assert_eq!(too_many.status.code(), Some(64));
    let too_many_reply = stdout_json(&too_many)?;
    assert_eq!(too_many_reply["adapter_error"]["code"], "too_many_fields");
    assert_eq!(too_many_reply["adapter_error"]["actual"], 7);
    Ok(())
}
