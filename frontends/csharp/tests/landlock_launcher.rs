#![cfg(target_os = "linux")]

use std::{fs, process::Command};

#[test]
fn launcher_rejects_wrong_nonce_and_never_executes_program() {
    let fixture = tempfile::tempdir().expect("fixture directory");
    let marker = fixture.path().join("marker");
    let policy = fixture.path().join("policy.json");
    let payload = serde_json::json!({
        "version": 1,
        "nonce": "a".repeat(64),
        "program": "/bin/sh",
        "cwd": fixture.path(),
        "args": ["-c", format!("touch {}", marker.display())],
        "grants": [],
        "enumerate": [],
    });
    fs::write(&policy, serde_json::to_vec(&payload).unwrap()).unwrap();
    let status = Command::new(env!("CARGO_BIN_EXE_csharp-landlock-launcher-prototype"))
        .arg("b".repeat(64))
        .arg(policy)
        .status()
        .expect("launch fixture");
    assert_eq!(status.code(), Some(125));
    assert!(!marker.exists());
}
