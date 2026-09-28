#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock is after Unix epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "backend-worker-cluster-process-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).expect("create process fixture directory");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))
                .expect("restrict process fixture directory");
        }
        #[cfg(windows)]
        backend_platform::win32::security::restrict_to_current_user(&path)
            .expect("restrict process fixture directory");
        Self(path)
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct RunningWorker {
    child: Child,
    lines: Receiver<String>,
}

impl RunningWorker {
    fn start(config: &Path, data_dir: &Path) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_backend-worker"))
            .args([
                "cluster",
                "run",
                "--config",
                config.to_str().expect("fixture config path is UTF-8"),
                "--data-dir",
                data_dir.to_str().expect("fixture data path is UTF-8"),
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("start actual backend-worker cluster run process");
        let stdout = child.stdout.take().expect("capture worker diagnostics");
        let (sender, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                match line {
                    Ok(line) if sender.send(line).is_ok() => {}
                    _ => break,
                }
            }
        });
        Self { child, lines }
    }

    fn wait_for_identity(&self) -> String {
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while std::time::Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            let line = self
                .lines
                .recv_timeout(remaining)
                .expect("worker should print its local identity after direct bind");
            if line.starts_with("worker identity fingerprint: ") {
                return line;
            }
        }
        panic!("worker did not report its direct identity before timeout");
    }

    fn assert_still_running(&mut self) {
        std::thread::sleep(Duration::from_millis(250));
        assert!(
            self.child
                .try_wait()
                .expect("check worker process status")
                .is_none(),
            "cluster run must remain live after startup"
        );
    }

    fn stop(mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn cluster_run_is_a_real_restartable_process_with_persisted_identity_and_local_store() {
    let directory = TestDirectory::new();
    let config = directory.0.join("worker.identity");
    let data_dir = directory.0.join("data");
    let namespace = "11".repeat(16);
    let recipe = "22".repeat(32);
    let init = Command::new(env!("CARGO_BIN_EXE_backend-worker"))
        .args([
            "cluster",
            "init",
            "--config",
            config.to_str().expect("fixture config path is UTF-8"),
            "--bind",
            "127.0.0.1:0",
            "--namespace",
            &namespace,
            "--recipe",
            &recipe,
        ])
        .output()
        .expect("run explicit local identity setup");
    assert!(init.status.success(), "identity init must succeed");
    assert!(String::from_utf8_lossy(&init.stdout).contains("direct-only"));

    let mut first = RunningWorker::start(&config, &data_dir);
    let first_identity = first.wait_for_identity();
    assert!(first_identity.contains("fingerprint:"));
    first.assert_still_running();
    first.stop();

    let orphan = data_dir
        .join("workspaces")
        .join("worker-snapshot-after-kill")
        .join("src");
    std::fs::create_dir_all(&orphan).expect("create crash-left snapshot fixture");
    std::fs::write(orphan.join("lib.rs"), b"pub fn interrupted() {}\n")
        .expect("write crash-left snapshot fixture");
    let outside = data_dir.join("outside-snapshot-sentinel");
    std::fs::write(&outside, b"preserve").expect("write outside cleanup sentinel");

    let pending = Command::new(env!("CARGO_BIN_EXE_backend-worker"))
        .args([
            "cluster",
            "pending",
            "--config",
            config.to_str().expect("fixture config path is UTF-8"),
            "--data-dir",
            data_dir.to_str().expect("fixture data path is UTF-8"),
        ])
        .output()
        .expect("inspect cold worker store");
    assert!(pending.status.success(), "local FileStore opens without S3");
    assert!(String::from_utf8_lossy(&pending.stdout).contains("no retained result"));

    let mut second = RunningWorker::start(&config, &data_dir);
    let second_identity = second.wait_for_identity();
    assert_eq!(
        first_identity, second_identity,
        "identity survives cold restart"
    );
    second.assert_still_running();
    assert!(
        !orphan.parent().expect("snapshot root").exists(),
        "cold startup removes an abandoned workspace snapshot"
    );
    assert_eq!(
        std::fs::read(&outside).expect("read outside cleanup sentinel"),
        b"preserve",
        "orphan cleanup stays within its private snapshot root"
    );
    second.stop();
}
