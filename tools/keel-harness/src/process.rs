//! Bounded subprocess execution and process-group custody for the harness.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

const STREAM_LIMIT: usize = 8 * 1024 * 1024;
const DRAIN_GRACE: Duration = Duration::from_secs(2);

pub(crate) struct Output {
    pub code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub elapsed_ms: u64,
}

fn prepare_file(path: &Path) -> Result<File, String> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    }
    File::create(path).map_err(|e| format!("create {}: {e}", path.display()))
}

// The child is the leader of a fresh process group. Kill the group even when
// its leader has already exited, since a descendant may still hold its pipes.
fn kill_group(id: u32) -> Result<(), String> {
    let status = Command::new("/bin/kill")
        .args(["-KILL", "--", &format!("-{id}")])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|e| format!("kill process group {id}: {e}"))?;
    // A group that has already disappeared is an expected outcome. A failed
    // kill is also followed by Child::kill as a fallback at each call site.
    if !status.success() {
        return Err(format!("kill process group {id} returned {status}"));
    }
    Ok(())
}

struct Capture {
    bytes: Vec<u8>,
    error: Option<String>,
}

fn drain<R: Read + Send + 'static>(mut pipe: R, mut file: File) -> mpsc::Receiver<Capture> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut bytes = Vec::new();
        let mut buf = [0_u8; 16 * 1024];
        let mut error = None;
        loop {
            match pipe.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    let keep = n.min(STREAM_LIMIT.saturating_sub(bytes.len()));
                    if keep > 0 {
                        bytes.extend_from_slice(&buf[..keep]);
                        if let Err(e) = file.write_all(&buf[..keep]) {
                            error.get_or_insert_with(|| format!("write capture: {e}"));
                        }
                    }
                    if keep < n {
                        error
                            .get_or_insert_with(|| format!("output exceeded {STREAM_LIMIT} bytes"));
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => {
                    error.get_or_insert_with(|| format!("read capture: {e}"));
                    break;
                }
            }
        }
        if let Err(e) = file.flush() {
            error.get_or_insert_with(|| format!("flush capture: {e}"));
        }
        let _ = tx.send(Capture { bytes, error });
    });
    rx
}

fn record_error(path: &Path, message: &str) {
    // Preserve the limit even when adding a diagnostic to an otherwise empty
    // or partial capture. Failures here cannot hide the original error.
    if let Ok(mut file) = OpenOptions::new().append(true).open(path) {
        if let Ok(metadata) = file.metadata() {
            let available = STREAM_LIMIT.saturating_sub(metadata.len() as usize);
            let marker = format!("\n[harness: {message}]\n");
            let _ = file.write_all(&marker.as_bytes()[..marker.len().min(available)]);
        }
    }
}

pub(crate) fn run(
    command: &mut Command,
    input: Option<Vec<u8>>,
    deadline: Duration,
    prefix: &Path,
) -> Result<Output, String> {
    let mut stdout_name = prefix.as_os_str().to_os_string();
    stdout_name.push(".stdout");
    let stdout_path = std::path::PathBuf::from(stdout_name);
    let mut stderr_name = prefix.as_os_str().to_os_string();
    stderr_name.push(".stderr");
    let stderr_path = std::path::PathBuf::from(stderr_name);
    let stdout_file = prepare_file(&stdout_path)?;
    let stderr_file = prepare_file(&stderr_path)?;
    let start = Instant::now();
    command
        .process_group(0)
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().map_err(|e| {
        let message = format!("spawn process: {e}");
        record_error(&stderr_path, &message);
        message
    })?;
    let id = child.id();
    let stdout = drain(child.stdout.take().expect("piped stdout"), stdout_file);
    let stderr = drain(child.stderr.take().expect("piped stderr"), stderr_file);
    if let Some(data) = input {
        let mut stdin = child.stdin.take().expect("piped stdin");
        thread::spawn(move || {
            let _ = stdin.write_all(&data);
        });
    }
    let mut failure = None;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if start.elapsed() < deadline => thread::sleep(
                Duration::from_millis(10).min(deadline.saturating_sub(start.elapsed())),
            ),
            Ok(None) => {
                failure = Some(format!(
                    "process timed out after {} ms",
                    deadline.as_millis()
                ));
                break None;
            }
            Err(e) => {
                failure = Some(format!("wait for process: {e}"));
                break None;
            }
        }
    };
    // Kill descendants even after successful exit. If the leader is still
    // alive, retain its Child until after the group signal and then reap it.
    let _ = kill_group(id);
    if status.is_none() {
        let _ = child.kill();
        if let Err(e) = child.wait() {
            failure.get_or_insert_with(|| format!("reap process: {e}"));
        }
    }
    let collect = |rx: mpsc::Receiver<Capture>, name: &str| match rx.recv_timeout(DRAIN_GRACE) {
        Ok(capture) => capture,
        Err(e) => Capture {
            bytes: Vec::new(),
            error: Some(format!("{name} drain did not finish: {e}")),
        },
    };
    let out = collect(stdout, "stdout");
    let err = collect(stderr, "stderr");
    if let Some(error) = out.error {
        failure.get_or_insert_with(|| format!("stdout: {error}"));
    }
    if let Some(error) = err.error {
        failure.get_or_insert_with(|| format!("stderr: {error}"));
    }
    if let Some(message) = failure {
        record_error(&stderr_path, &message);
        return Err(message);
    }
    Ok(Output {
        code: status.and_then(|s| s.code()),
        stdout: out.bytes,
        stderr: err.bytes,
        elapsed_ms: start.elapsed().as_millis().min(u64::MAX as u128) as u64,
    })
}

pub(crate) struct Daemon {
    child: Option<Child>,
    id: u32,
}

impl Daemon {
    pub(crate) fn spawn(command: &mut Command, log: &Path) -> Result<Self, String> {
        let file = prepare_file(log)?;
        let stderr = file
            .try_clone()
            .map_err(|e| format!("clone daemon log: {e}"))?;
        command
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(Stdio::from(file))
            .stderr(Stdio::from(stderr));
        let child = command.spawn().map_err(|e| {
            let message = format!("spawn daemon: {e}");
            record_error(log, &message);
            message
        })?;
        let id = child.id();
        Ok(Self {
            child: Some(child),
            id,
        })
    }

    pub(crate) fn is_running(&mut self) -> Result<bool, String> {
        let Some(child) = self.child.as_mut() else {
            return Ok(false);
        };
        match child.try_wait() {
            Ok(None) => Ok(true),
            Ok(Some(_)) => {
                let _ = kill_group(self.id);
                self.child = None;
                Ok(false)
            }
            Err(e) => Err(format!("check daemon: {e}")),
        }
    }

    pub(crate) fn stop(&mut self) -> Result<(), String> {
        if let Some(mut child) = self.child.take() {
            let group_result = kill_group(self.id);
            let _ = child.kill();
            child.wait().map_err(|e| format!("reap daemon: {e}"))?;
            // The group may have already vanished, e.g. after a normal exit.
            let _ = group_result;
        }
        Ok(())
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}
