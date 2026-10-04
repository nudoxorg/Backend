//! Test-only support for tests that run part of themselves in another process.
//!
//! A test re-executes this test binary as a *peer* that opens the same database
//! file. The parent drives the peer one line at a time over standard input and
//! reads one report line back, so a scenario is an exact interleaving of
//! operations between two real processes, and a peer can be killed at a chosen
//! point the way a crash would end it.

#![allow(clippy::expect_used, clippy::panic)]

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

const PEER: &str = "BACKEND_TURSO_PROCESS_PEER";
const DATABASE: &str = "BACKEND_TURSO_PROCESS_DATABASE";
const REPORT_MARKER: &str = "REPORT ";
const REPORT_TIMEOUT: Duration = Duration::from_secs(60);

static NEXT_PATH: AtomicU64 = AtomicU64::new(0);

/// A unique database path in the temporary directory.
pub(crate) fn database_path(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "backend-turso-process-{label}-{}-{}.db",
        std::process::id(),
        NEXT_PATH.fetch_add(1, Ordering::Relaxed)
    ))
}

/// Removes a test database and its isolated projection namespace after every
/// process that opened either object has exited.
pub(crate) fn remove_database(path: &Path) {
    if let Some(name) = path.file_name().and_then(|name| name.to_str()) {
        let namespace = path.with_file_name(format!("{name}.namespace-v1"));
        let _ = std::fs::remove_dir_all(namespace);
    }
    for suffix in ["", "-wal", "-shm", "-tshm"] {
        let mut sidecar = path.as_os_str().to_owned();
        sidecar.push(suffix);
        let _ = std::fs::remove_file(PathBuf::from(sidecar));
    }
}

/// Removes its database when a test ends, however it ends.
pub(crate) struct Database(pub(crate) PathBuf);

impl Database {
    pub(crate) fn new(label: &str) -> Self {
        Self(database_path(label))
    }
}

impl Drop for Database {
    fn drop(&mut self) {
        remove_database(&self.0);
    }
}

/// A child process serving line commands against a shared database.
pub(crate) struct Peer {
    process: Child,
    stdin: Option<ChildStdin>,
    reports: Receiver<String>,
}

impl Peer {
    /// Starts a peer that re-runs `test` (which must call [`peer_database`]
    /// first) and returns it with its first report, which says whether it
    /// opened `database`.
    pub(crate) fn spawn(test: &str, database: &Path) -> (Self, String) {
        Self::spawn_inner(test, database, None)
    }

    /// Starts a peer with one test-only environment variable.
    pub(crate) fn spawn_with_env(
        test: &str,
        database: &Path,
        name: &str,
        value: &str,
    ) -> (Self, String) {
        Self::spawn_inner(test, database, Some((name, value)))
    }

    fn spawn_inner(
        test: &str,
        database: &Path,
        extra_env: Option<(&str, &str)>,
    ) -> (Self, String) {
        let mut command = Command::new(std::env::current_exe().expect("test executable"));
        command
            .args(["--exact", test, "--nocapture", "--test-threads=1"])
            .env(PEER, "1")
            .env(DATABASE, database);
        if let Some((name, value)) = extra_env {
            command.env(name, value);
        }
        let mut process = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("spawn peer process");
        let stdout = process.stdout.take().expect("peer stdout");
        let stdin = process.stdin.take();
        let (sender, reports) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                // The test harness prints its own progress on the same line, so
                // a report is found by marker rather than by prefix.
                if let Some((_, report)) = line.split_once(REPORT_MARKER)
                    && sender.send(report.to_owned()).is_err()
                {
                    break;
                }
            }
        });
        let mut peer = Self {
            process,
            stdin,
            reports,
        };
        let opened = peer.next_report();
        (peer, opened)
    }

    /// The peer's next report line.
    pub(crate) fn next_report(&mut self) -> String {
        self.reports
            .recv_timeout(REPORT_TIMEOUT)
            .expect("peer reported in time")
    }

    /// Sends one command and returns the peer's report.
    pub(crate) fn ask(&mut self, command: &str) -> String {
        let stdin = self.stdin.as_mut().expect("peer stdin");
        writeln!(stdin, "{command}").expect("send peer command");
        stdin.flush().expect("flush peer command");
        self.next_report()
    }

    /// Closes the peer's input so it ends normally, and reports whether it
    /// exited cleanly.
    pub(crate) fn release(mut self) -> bool {
        drop(self.stdin.take());
        self.process.wait().expect("peer exit").success()
    }

    /// Ends the peer abruptly, as a crash would: no destructor runs and no
    /// lock is released by the peer itself.
    pub(crate) fn kill(mut self) {
        self.process.kill().expect("kill peer");
        let _ = self.process.wait();
    }
}

/// In a peer process, the database it must open; `None` in the parent, which
/// must then run the test body.
pub(crate) fn peer_database() -> Option<PathBuf> {
    std::env::var_os(PEER)?;
    Some(PathBuf::from(
        std::env::var_os(DATABASE).expect("peer database path"),
    ))
}

/// Prints one report line for the parent.
pub(crate) fn report(line: &str) {
    println!("{REPORT_MARKER}{line}");
}

/// The command lines the parent sends, until it closes the peer's input.
pub(crate) fn commands() -> impl Iterator<Item = String> {
    std::io::stdin().lock().lines().map_while(Result::ok)
}
