//! Exercises opt-in human CLI projection while preserving JSON as the default mode.

use std::process::Command;

#[test]
fn human_flag_projects_the_same_typed_health_reply_without_replacing_json_default() {
    let human = Command::new(env!("CARGO_BIN_EXE_interface-cli"))
        .args(["--human", "health", "1"])
        .output();
    let json = Command::new(env!("CARGO_BIN_EXE_interface-cli"))
        .args(["health", "1"])
        .output();
    let (Ok(human), Ok(json)) = (human, json) else {
        panic!("CLI process could not be launched");
    };
    assert!(human.status.success());
    assert!(json.status.success());
    assert!(human.stdout.starts_with(b"health:\n"));
    assert!(json.stdout.starts_with(b"{\"correlation\":1,"));
}
