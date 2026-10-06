//! Passive health must describe a configured owner without ever starting one.

use backend_local_service::{EmbeddedLocalService, ProcessConfig};
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

/// A short private scratch directory.
///
/// The endpoint lives beneath it, and both Unix `sun_path` and the Windows
/// endpoint budget are about one hundred bytes, so the name stays terse and the
/// root is the shortest temporary location the platform offers (`/tmp` where it
/// exists, because Nix's `TMPDIR` is itself deep).
fn scratch(label: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after epoch")
        .as_nanos();
    let base = if cfg!(unix) {
        PathBuf::from("/tmp")
    } else {
        std::env::temp_dir()
    };
    base.join(format!(
        "bcp-{label}-{}-{:x}",
        std::process::id(),
        nonce & 0xffff_ffff
    ))
}

/// Writes an executable that, if the CLI ever launched it, leaves `marker`
/// behind, and returns its path.
fn marker_daemon(root: &Path, marker: &Path) -> PathBuf {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let path = root.join("fake-locald");
        fs::write(
            &path,
            format!("#!/bin/sh\nprintf started > '{}'\n", marker.display()),
        )
        .expect("write fake daemon");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700))
            .expect("make fake daemon executable");
        path
    }
    #[cfg(windows)]
    {
        // `CreateProcess` runs a `.cmd` through the command interpreter, so
        // the batch file plays the part a shell script plays on Unix.
        let path = root.join("fake-locald.cmd");
        fs::write(
            &path,
            format!("@echo started> \"{}\"\r\n", marker.display()),
        )
        .expect("write fake daemon");
        path
    }
}

/// Makes `path` usable by its owner alone, the state every private workspace
/// parent must be in: mode `0700`, or a protected DACL on Windows.
fn restrict_to_owner(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .expect("restrict the fixture to its owner");
    }
    #[cfg(windows)]
    backend_platform::win32::security::restrict_to_current_user(path)
        .expect("restrict the fixture to its owner");
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
    let fake_locald = marker_daemon(&root, &marker);

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
    // `--format json` is a machine rendering, so the typed fault is the
    // reply on stdout and nothing is written to the diagnostics stream.
    let fault: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "a machine rendering of the fault is JSON on stdout ({error}): {}",
            String::from_utf8_lossy(&output.stdout)
        )
    });
    assert_eq!(fault["answer"], "fault");
    assert_eq!(fault["slug"], "endpoint");
    assert_eq!(fault["cause"], "unreachable");
    let recovery = fault["shell"].as_str().expect("owner startup recovery command");
    assert!(recovery.starts_with("backend health "), "{recovery}");
    assert!(!recovery.contains("--passive"), "{recovery}");
    for selected_path in [&project, &workspace, &endpoint] {
        assert!(
            recovery.contains(selected_path.to_str().expect("UTF-8 fixture path")),
            "recovery must retain selected paths: {recovery}"
        );
    }
    assert!(
        fault["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("without starting it")),
        "passive failure must say it did not start locald: {fault}"
    );
    assert!(
        output.stderr.is_empty(),
        "a machine rendering leaves stderr empty: {}",
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
    // The root holds only the private workspace. The endpoint is its sibling:
    // a Windows root restricted to its owner cannot be removed by
    // `std::fs::remove_dir_all` while it holds a child with the default ACL.
    let root = scratch("live");
    let workspace = root.join("state");
    let endpoint = root.with_extension("sock");
    fs::create_dir_all(&root).expect("create fixture root");
    restrict_to_owner(&root);
    // Every surface prepares a workspace through the runtime before it starts
    // an owner on it, and the owner then expects that private directory to
    // exist. Doing the same here keeps the fixture on the product's own path
    // instead of on a state a user can never reach.
    let project = std::env::current_dir().expect("current project");
    backend_runtime::WorkspacePaths::discover(
        Some(project.clone()),
        Some(workspace.clone()),
        Some(endpoint.clone()),
    )
    .and_then(|paths| paths.initialize())
    .expect("prepare the private workspace the way every surface does");
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
    let _ = fs::remove_file(endpoint);
}
