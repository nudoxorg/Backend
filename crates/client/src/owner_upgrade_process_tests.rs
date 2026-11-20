// Real private DTO39-to-DTO40 handoff with a queued mutation, traced at the
// client socket. This does not emulate another protocol version.
use super::*;
use std::os::unix::{fs::PermissionsExt as _, net::UnixStream};
use std::path::PathBuf;

#[test]
#[ignore = "requires verified DTO39 owner, matched DTO40 CLI/locald, and strace"]
fn pending_mutation_is_sent_once_only_after_real_39_to_40_handoff() {
    let old = PathBuf::from(std::env::var_os("NUDOX_TEST_DTO39_LOCALD").expect("DTO39 owner"));
    let cli = PathBuf::from(std::env::var_os("NUDOX_TEST_DTO40_CLI").expect("DTO40 CLI"));
    let current = PathBuf::from(std::env::var_os("NUDOX_TEST_DTO40_LOCALD").expect("DTO40 owner"));
    let strace = PathBuf::from(std::env::var_os("NUDOX_TEST_STRACE").expect("strace executable"));
    assert_eq!(cli.parent(), current.parent(), "matched DTO40 pair");
    let root = std::env::temp_dir().join(format!(
        "nudox-dto40-mutation-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    std::fs::create_dir(&root).expect("fixture root");
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).expect("private root");
    let root = root.canonicalize().expect("canonical root");
    std::fs::create_dir(root.join("p")).expect("project");
    let project = root.join("p").canonicalize().expect("project path");
    let mut key = blake3::Hasher::new();
    key.update(b"backend.runtime.project-state.v1\0");
    key.update(project.as_os_str().as_encoded_bytes());
    let workspace = root
        .join("Nudox")
        .join("projects")
        .join(key.finalize().to_hex().as_str());
    let endpoint = root.join("s");
    let authority = workspace.join("authority.secret");
    let child = std::process::Command::new(old)
        .arg("--runtime-startup-v1")
        .arg("direct")
        .arg(&root)
        .arg(&project)
        .arg(&workspace)
        .arg(&endpoint)
        .arg(&authority)
        .arg("--endpoint")
        .arg(&endpoint)
        .arg("--workspace")
        .arg(&workspace)
        .arg("--authority-secret-file")
        .arg(&authority)
        .current_dir(&project)
        .env("XDG_STATE_HOME", &root)
        .env("XDG_DATA_HOME", &root)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn private DTO39 owner");
    struct Fixture {
        root: PathBuf,
        project: PathBuf,
        workspace: PathBuf,
        endpoint: PathBuf,
        authority: PathBuf,
        child: std::process::Child,
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let retired = UnixStream::connect(&self.endpoint)
                .ok()
                .and_then(|stream| {
                    let peer = backend_replication::AuthenticatedLocalPeer::authenticate(
                        &stream,
                        &self.endpoint,
                    )
                    .ok()?;
                    peer.retire_automatic_owner(
                        &stream,
                        &self.project,
                        &self.workspace,
                        &self.authority,
                        Instant::now() + Duration::from_secs(5),
                        || true,
                    )
                    .ok()
                })
                .is_some();
            if self.child.try_wait().ok().flatten().is_none() {
                let _ = self.child.kill();
            }
            let _ = self.child.wait();
            if retired || !self.endpoint.exists() {
                let _ = std::fs::remove_dir_all(&self.root);
            }
        }
    }
    let mut fixture = Fixture {
        root,
        project,
        workspace,
        endpoint,
        authority,
        child,
    };
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut old_stream = loop {
        if let Ok(stream) = UnixStream::connect(&fixture.endpoint) {
            break stream;
        }
        assert!(fixture.child.try_wait().expect("old owner state").is_none());
        assert!(Instant::now() < deadline, "old owner startup deadline");
        std::thread::sleep(Duration::from_millis(20));
    };
    old_stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("probe timeout");
    old_stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .expect("probe write timeout");
    let old_peer =
        backend_replication::AuthenticatedLocalPeer::authenticate(&old_stream, &fixture.endpoint)
            .expect("authenticated old peer");
    assert!(old_peer.is_locald_process(&old_stream));
    let mut health =
        serde_json::to_value(CommandDto::new(PROBE_ID, Command::Health)).expect("health");
    health["version"] = serde_json::json!(39);
    write_body(
        &mut old_stream,
        &serde_json::to_vec(&health).expect("encode health"),
    )
    .expect("old health");
    assert_eq!(
        version(&read_body(&mut old_stream).expect("old reply")).expect("correlated old reply"),
        39
    );
    drop(old_stream);
    let trace = fixture.root.join("mutation.trace");
    let mut traced = std::process::Command::new(strace)
        .args([
            "--kill-on-exit",
            "-ttt",
            "-yy",
            "-s",
            "65535",
            "-e",
            "trace=write,writev,sendto,sendmsg,connect,pidfd_send_signal,kill",
            "-o",
        ])
        .arg(&trace)
        .arg(&cli)
        .arg("--project")
        .arg(&fixture.project)
        .arg("--endpoint")
        .arg(&fixture.endpoint)
        .args(["--format", "json", "remove"])
        .arg(&fixture.project)
        .current_dir(&fixture.project)
        .env("XDG_STATE_HOME", &fixture.root)
        .env("XDG_DATA_HOME", &fixture.root)
        .env_remove("BACKEND_LOCALD_WORKSPACE")
        .env_remove("BACKEND_LOCALD_BIN")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("trace pending remove command");
    let until = Instant::now() + Duration::from_secs(30);
    while traced.try_wait().expect("traced CLI state").is_none() {
        if Instant::now() >= until {
            let _ = traced.kill();
            let _ = traced.wait();
            panic!("traced CLI deadline");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let output = traced.wait_with_output().expect("traced output");
    eprintln!(
        "traced CLI status={:?} stdout={} stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.status.success(),
        "the one private remove command must complete"
    );
    let trace_text = std::fs::read_to_string(&trace).expect("read syscall trace");
    if let Some(path) = std::env::var_os("NUDOX_TEST_TRACE_OUT") {
        std::fs::write(path, &trace_text).expect("save private fixture trace");
    }
    let writes = trace_text
        .lines()
        .filter(|line| {
            line.contains("sendto(")
                && line.contains("UNIX-STREAM:[")
                && line.contains("\\\"kind\\\"")
        })
        .collect::<Vec<_>>();
    eprintln!("traced command writes:\n{}", writes.join("\n"));
    let health_writes = writes
        .iter()
        .filter(|line| line.contains("\\\"kind\\\":\\\"health\\\""))
        .collect::<Vec<_>>();
    let mutation_writes = writes
        .iter()
        .filter(|line| line.contains("\\\"kind\\\":\\\"remove\\\""))
        .collect::<Vec<_>>();
    assert!(
        !health_writes.is_empty(),
        "old health must be observed in trace"
    );
    assert_eq!(
        mutation_writes.len(),
        1,
        "pending remove must be written exactly once"
    );
    let socket = |line: &str| -> String {
        line.split("UNIX-STREAM:[")
            .nth(1)
            .and_then(|rest| rest.split([']', '-']).next())
            .expect("traced socket identity")
            .to_owned()
    };
    assert_ne!(
        socket(health_writes[0]),
        socket(mutation_writes[0]),
        "mutation must not be written to old health peer"
    );
    assert!(
        writes
            .iter()
            .filter(|line| socket(line) == socket(health_writes[0]))
            .all(|line| line.contains("\\\"kind\\\":\\\"health\\\"")),
        "the old authenticated connection must receive only the health probe"
    );
    let mutation_pos = trace_text
        .find(*mutation_writes[0])
        .expect("mutation trace position");
    let owner_pid = format!("<pid:{}>", fixture.child.id());
    let retirement = trace_text
        .lines()
        .find(|line| {
            line.contains("pidfd_send_signal(")
                && line.contains(&owner_pid)
                && line.contains("SIGTERM")
                && line.ends_with("= 0")
        })
        .expect("trace must show successful SIGTERM to the exact old fixture owner");
    assert!(
        trace_text
            .find(retirement)
            .is_some_and(|position| position < mutation_pos),
        "the old owner must be signalled before the mutation is written"
    );
    assert!(
        health_writes
            .iter()
            .any(|line| socket(line) == socket(mutation_writes[0])
                && trace_text
                    .find(*line)
                    .is_some_and(|position| position < mutation_pos)),
        "replacement connection must receive its health probe before the mutation"
    );
    assert!(
        fixture.child.try_wait().expect("old owner state").is_some(),
        "old owner retired"
    );
    let mut replacement = UnixStream::connect(&fixture.endpoint).expect("replacement endpoint");
    replacement
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("replacement probe timeout");
    replacement
        .set_write_timeout(Some(Duration::from_secs(5)))
        .expect("replacement probe write timeout");
    let peer =
        backend_replication::AuthenticatedLocalPeer::authenticate(&replacement, &fixture.endpoint)
            .expect("replacement peer");
    let intended = peer
        .observe_default_locald_image(&current)
        .expect("intended owner image");
    assert!(
        intended.is_current()
            && intended
                .matches_peer(&peer, &replacement)
                .expect("exact replacement image")
    );
    write_body(
        &mut replacement,
        &encode_request(&CommandDto::new(PROBE_ID, Command::Health))
            .expect("replacement health body"),
    )
    .expect("send replacement health");
    assert_eq!(
        version(&read_body(&mut replacement).expect("replacement health reply"))
            .expect("correlated DTO40 reply"),
        40
    );
}

#[test]
#[ignore = "requires an independently verified DTO38 locald process"]
fn real_dto38_owner_is_refused_without_retirement() {
    let old =
        PathBuf::from(std::env::var_os("NUDOX_TEST_DTO38_LOCALD").expect("verified DTO38 owner"));
    let root = std::env::temp_dir().join(format!(
        "nudox-dto38-owner-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    std::fs::create_dir(&root).expect("fixture root");
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).expect("private root");
    let root = root.canonicalize().expect("canonical root");
    std::fs::create_dir(root.join("p")).expect("project");
    let project = root.join("p").canonicalize().expect("project path");
    let mut key = blake3::Hasher::new();
    key.update(b"backend.runtime.project-state.v1\0");
    key.update(project.as_os_str().as_encoded_bytes());
    let workspace = root
        .join("Nudox")
        .join("projects")
        .join(key.finalize().to_hex().as_str());
    let endpoint = root.join("s");
    let authority = workspace.join("authority.secret");
    let child = std::process::Command::new(old)
        .arg("--runtime-startup-v1")
        .arg("direct")
        .arg(&root)
        .arg(&project)
        .arg(&workspace)
        .arg(&endpoint)
        .arg(&authority)
        .arg("--endpoint")
        .arg(&endpoint)
        .arg("--workspace")
        .arg(&workspace)
        .arg("--authority-secret-file")
        .arg(&authority)
        .current_dir(&project)
        .env("XDG_STATE_HOME", &root)
        .env("XDG_DATA_HOME", &root)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn private DTO38 owner");
    struct Fixture {
        root: PathBuf,
        child: std::process::Child,
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            if self.child.try_wait().ok().flatten().is_none() {
                let _ = self.child.kill();
            }
            let _ = self.child.wait();
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }
    let mut fixture = Fixture { root, child };
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut stream = loop {
        if let Ok(stream) = UnixStream::connect(&endpoint) {
            break stream;
        }
        assert!(fixture.child.try_wait().expect("old owner state").is_none());
        assert!(Instant::now() < deadline, "old owner startup deadline");
        std::thread::sleep(Duration::from_millis(20));
    };
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("bounded health probe");
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .expect("bounded health probe write");
    let peer = backend_replication::AuthenticatedLocalPeer::authenticate(&stream, &endpoint)
        .expect("authenticate real DTO38 owner");
    assert!(peer.is_locald_process(&stream));
    let mut health =
        serde_json::to_value(CommandDto::new(PROBE_ID, Command::Health)).expect("health");
    health["version"] = serde_json::json!(38);
    write_body(
        &mut stream,
        &serde_json::to_vec(&health).expect("health body"),
    )
    .expect("probe");
    assert_eq!(
        version(&read_body(&mut stream).expect("probe reply")).expect("correlated DTO38"),
        38
    );
    drop(stream);
    let paths = WorkspacePaths::discover(Some(project), Some(workspace), Some(endpoint))
        .expect("fixture paths");
    let refusal = Session::connect_matched_owner(&paths, None, false)
        .err()
        .expect("DTO38 must be refused");
    assert!(
        matches!(&refusal, ClientError::OwnerUpdate { found: 38, supported: 40, detail } if detail.contains("restart the previous NuDox owner") && detail.contains("no command was submitted")),
        "typed unsupported-owner refusal: {refusal}"
    );
    assert!(
        fixture.child.try_wait().expect("old owner state").is_none(),
        "DTO38 owner must not be retired"
    );
}
