#![cfg(unix)]

use backend_local_service::{EmbeddedLocalService, ProcessConfig};
use serde_json::Value;
use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn scratch(label: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after epoch")
        .as_nanos();
    std::env::temp_dir().join(format!(
        "backend-cli-passive-{label}-{}-{nonce:x}",
        std::process::id()
    ))
}

#[test]
fn passive_health_does_not_start_locald_or_create_state_when_owner_is_absent() {
    let root = scratch("absent");
    fs::create_dir_all(&root).expect("create fixture root");
    let project = root.join("project");
    let workspace = root.join("state");
    let endpoint = root.join("run").join("locald.sock");
    fs::create_dir(&project).expect("create project");
    let marker = root.join("locald-was-started");
    let fake_locald = root.join("fake-locald");
    fs::write(
        &fake_locald,
        format!("#!/bin/sh\nprintf started > '{}'\n", marker.display()),
    )
    .expect("write fake daemon");
    fs::set_permissions(&fake_locald, fs::Permissions::from_mode(0o700))
        .expect("make fake daemon executable");

    let output = Command::new(env!("CARGO_BIN_EXE_backend-cli"))
        .args([
            "--passive",
            "--format",
            "json",
            "--project",
            project.to_str().expect("UTF-8 fixture path"),
            "--workspace",
            workspace.to_str().expect("UTF-8 fixture path"),
            "--endpoint",
            endpoint.to_str().expect("UTF-8 fixture path"),
            "health",
        ])
        .env("BACKEND_LOCALD_BIN", &fake_locald)
        .output()
        .expect("run passive CLI health");

    assert!(!output.status.success(), "absent owner must fail health");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("without starting it"),
        "passive failure must say it did not start locald: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!marker.exists(), "passive health spawned the daemon");
    assert!(!workspace.exists(), "passive health created state");
    assert!(
        !endpoint.parent().expect("endpoint parent").exists(),
        "passive health created the endpoint directory"
    );

    fs::remove_dir_all(root).expect("remove fixture root");
}

#[test]
fn passive_health_returns_status_from_a_live_owner() {
    let root = scratch("live");
    let workspace = root.join("state");
    let endpoint = root.join("run").join("locald.sock");
    fs::create_dir_all(&root).expect("create fixture root");
    let arguments = [
        "--workspace".to_owned(),
        workspace.to_string_lossy().into_owned(),
        "--endpoint".to_owned(),
        endpoint.to_string_lossy().into_owned(),
        "--profile".to_owned(),
        "builtin-echo".to_owned(),
        "--idle-timeout-ms".to_owned(),
        "0".to_owned(),
    ];
    let config = ProcessConfig::parse(arguments).expect("parse embedded owner config");
    let owner = EmbeddedLocalService::start(config).expect("start real local owner");

    let project = std::env::current_dir().expect("current project");
    let output = Command::new(env!("CARGO_BIN_EXE_backend-cli"))
        .args([
            "--passive",
            "--format",
            "json",
            "--project",
            project.to_str().expect("UTF-8 project path"),
            "--workspace",
            workspace.to_str().expect("UTF-8 fixture path"),
            "--endpoint",
            endpoint.to_str().expect("UTF-8 fixture path"),
            "health",
        ])
        .output()
        .expect("run passive CLI health");

    let close = owner.close().expect("stop embedded owner");
    assert!(close.frames > 0, "owner received no health request");
    assert!(
        output.status.success(),
        "live owner health failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let status: Value = serde_json::from_slice(&output.stdout).expect("status JSON");
    assert_eq!(status["answer"], "status");
    assert_eq!(status["readiness"], "ready");
    assert_eq!(status["revision"].as_str().map(str::len), Some(64));
    assert_eq!(status["source"].as_str().map(str::len), Some(64));

    fs::remove_dir_all(root).expect("remove fixture root");
}
