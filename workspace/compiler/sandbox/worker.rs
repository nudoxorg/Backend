//! Pooled workers for library-form producer isolation — **in-guest** in the
//! VM model.
//!
//! Cage-internal (SMOLVM-PLAN §3.3 / SV-11): long-lived children under
//! rlimits + a fairness cgroup speak a line-oriented JSON protocol. Not a
//! peer of any backend enum — [`SmolvmCage`](crate::smolvm::SmolvmCage) is
//! the production cage, and this pool is how library producers
//! (nix/ts/python) run *inside the guest image* (`ExecPlan::Library`). On
//! the dev passthrough path the pool runs directly on the host.
//!
//! Concurrency: free-list of slots (`Mutex<Vec<WorkerSlot>>` + condvar) so the
//! free-list lock is never held across worker I/O. Real parallelism equals pool
//! size. A hung worker costs one slot for one wall budget, not the whole pool
//! forever (deadline-safe non-blocking pipe I/O).

use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::cage::Cage;
use crate::cancel::CancelToken;
use crate::error::{CageError, KillReason, SandboxError};
use crate::limits::Limits;
use crate::profiles::ProducerProfile;
use crate::spec::Env;

/// Languages the worker binary can lower (closed set — no free strings at the
/// isolate boundary).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerLang {
    /// snix hermetic eval + static fusion.
    Nix,
    /// deno_doc.
    Typescript,
    /// pyrefly.
    Python,
}

impl WorkerLang {
    /// Wire token for the worker CLI / protocol.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Nix => "nix",
            Self::Typescript => "typescript",
            Self::Python => "python",
        }
    }

    /// Parse a wire token.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "nix" => Some(Self::Nix),
            "typescript" | "ts" => Some(Self::Typescript),
            "python" | "py" => Some(Self::Python),
            _ => None,
        }
    }

    /// Resource profile for this language class.
    pub const fn profile(self) -> ProducerProfile {
        match self {
            Self::Nix => ProducerProfile::Nix,
            Self::Typescript | Self::Python => ProducerProfile::StaticParser,
        }
    }
}

/// Request sent to a worker process (one line of JSON).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum JobRequest {
    /// Lower a package root with the named language producer.
    Lower {
        /// Closed language set.
        lang: WorkerLang,
        /// Absolute path to the package root.
        root: PathBuf,
    },
    /// Liveness + optional RSS probe.
    Ping,
    /// Graceful exit.
    Shutdown,
}

/// Response from a worker (one line of JSON).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum JobResponse {
    /// Success body (IR JSON, or empty for shutdown).
    Ok {
        /// UTF-8 JSON body.
        #[serde(default)]
        body: String,
    },
    /// Soft failure (parse error, eval timeout) — worker still healthy.
    Err {
        /// Stable error kind for metrics.
        kind: String,
        /// Human detail.
        message: String,
    },
    /// Pong.
    Pong {
        /// Optional RSS bytes if the worker can report it.
        #[serde(default)]
        rss: Option<u64>,
    },
}

/// Pool configuration.
#[derive(Debug, Clone)]
pub struct WorkerPoolConfig {
    /// Path to the worker binary.
    pub worker_bin: PathBuf,
    /// How many workers to keep warm (= max concurrent jobs).
    pub size: usize,
    /// Resource ceilings per worker process.
    pub limits: Limits,
    /// Extra RO binds (toolchains, etc.) — reserved for pooled-guest mode.
    pub read_only: Vec<PathBuf>,
    /// Restart worker if self-reported RSS exceeds this (bytes).
    pub memory_watermark: u64,
}

impl WorkerPoolConfig {
    /// Defaults for static parsers (design §7.5).
    ///
    /// Profile base limits; the `ForgeRuntime`'s `OverrideTable` can override at
    /// the `run_producer` boundary.
    pub fn static_parser(worker_bin: impl Into<PathBuf>) -> Self {
        Self {
            worker_bin: worker_bin.into(),
            size: 2,
            limits: ProducerProfile::StaticParser.base_limits(),
            read_only: Vec::new(),
            memory_watermark: 768 * 1024 * 1024,
        }
    }

    /// Defaults for snix workers.
    ///
    /// Profile base limits; the `ForgeRuntime`'s `OverrideTable` can override at
    /// the `run_producer` boundary.
    pub fn nix(worker_bin: impl Into<PathBuf>) -> Self {
        Self {
            worker_bin: worker_bin.into(),
            size: 2,
            limits: ProducerProfile::Nix.base_limits(),
            read_only: Vec::new(),
            memory_watermark: 768 * 1024 * 1024,
        }
    }
}

/// One warm worker process.
///
/// **Drop contract:** pool restart / pool drop paths kill and `wait` the child
/// before the slot is dropped so the owned cgroup is empty when reaped.
struct WorkerSlot {
    child: Child,
    stdin: ChildStdin,
    stdout: ChildStdout,
    /// Partial line assembly for non-blocking reads.
    line_buf: Vec<u8>,
    /// Owned for the worker's lifetime; dropped on restart so cgroup dirs are
    /// reaped.
    cgroup: Option<crate::cgroup::Cgroup>,
    /// Per-worker HOME/TMPDIR; removed when the slot is dropped.
    scratch: PathBuf,
}

impl WorkerSlot {
    /// Kill the process tree (cgroup.kill when available) and wait.
    fn kill_tree(&mut self) {
        if let Some(ref cg) = self.cgroup {
            let _ = cg.kill_all();
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for WorkerSlot {
    fn drop(&mut self) {
        self.kill_tree();
        let _ = std::fs::remove_dir_all(&self.scratch);
    }
}

/// Free-list of warm slots + condvar. Lock is held only to take/return a slot,
/// never across worker I/O — so real parallelism equals pool size.
struct FreeList {
    /// Idle slots (owned by the pool when here).
    slots: Mutex<Vec<WorkerSlot>>,
    /// Signaled when a slot is returned.
    cv: Condvar,
    /// Configured size (for diagnostics).
    size: usize,
}

impl FreeList {
    fn take(&self) -> WorkerSlot {
        let mut slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if let Some(slot) = slots.pop() {
                return slot;
            }
            slots = self.cv.wait(slots).unwrap_or_else(|e| e.into_inner());
        }
    }

    fn put(&self, slot: WorkerSlot) {
        let mut slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        slots.push(slot);
        self.cv.notify_one();
    }
}

/// RAII lease: always returns the slot to the free-list on drop (including panic).
struct Lease<'a> {
    free: &'a FreeList,
    slot: Option<WorkerSlot>,
}

impl<'a> Lease<'a> {
    fn take(free: &'a FreeList) -> Self {
        Self {
            free,
            slot: Some(free.take()),
        }
    }

    fn slot_mut(&mut self) -> Result<&mut WorkerSlot, CageError> {
        self.slot.as_mut().ok_or_else(|| CageError::WorkerProtocol {
            detail: "lease slot accessed after drop (internal invariant violated)".into(),
        })
    }
}

impl Drop for Lease<'_> {
    fn drop(&mut self) {
        if let Some(slot) = self.slot.take() {
            self.free.put(slot);
        }
    }
}

/// A pool of sandboxed worker processes (cage-internal runner).
///
/// Real parallelism equals [`WorkerPoolConfig::size`]: a free-list hands out
/// slots; the free-list mutex is **not** held during worker I/O (unlike the
/// Phase-0 serial design).
///
/// **Drop contract:** do not call [`submit`](Self::submit) concurrently with
/// dropping the pool (e.g. while other threads hold an `Arc<WorkerPool>` and
/// still submit). Drop only drains slots currently on the free list; in-flight
/// leases return workers after Drop has finished. Join in-flight work first.
pub struct WorkerPool {
    config: WorkerPoolConfig,
    free: FreeList,
}

impl WorkerPool {
    /// Spawn `config.size` workers under rlimits + cgroup.
    pub fn new(config: WorkerPoolConfig) -> Result<Self, SandboxError> {
        let size = config.size.max(1);
        let mut slots = Vec::with_capacity(size);
        for _ in 0..size {
            slots.push(spawn_worker(&config)?);
        }
        Ok(Self {
            config,
            free: FreeList {
                slots: Mutex::new(slots),
                cv: Condvar::new(),
                size,
            },
        })
    }

    /// How many workers (max concurrent jobs).
    pub fn size(&self) -> usize {
        self.free.size
    }

    /// Submit a job to a free worker. Blocks until a slot is available.
    ///
    /// Parallelism: up to `size` submits run at once on distinct slots.
    pub fn submit(&self, req: JobRequest) -> Result<JobResponse, SandboxError> {
        self.submit_cancel(req, &CancelToken::never())
    }

    /// Submit with a cancellation token (honored via cgroup.kill + child kill).
    pub fn submit_cancel(
        &self,
        req: JobRequest,
        cancel: &CancelToken,
    ) -> Result<JobResponse, SandboxError> {
        if cancel.is_cancelled() {
            return Err(CageError::Cancelled.into());
        }

        // Lease returns the slot even if run_on_slot panics.
        let mut lease = Lease::take(&self.free);
        let slot = lease.slot_mut().map_err(SandboxError::from)?;
        self.run_on_slot(slot, &req, cancel)
    }

    fn run_on_slot(
        &self,
        slot: &mut WorkerSlot,
        req: &JobRequest,
        cancel: &CancelToken,
    ) -> Result<JobResponse, SandboxError> {
        let wall = self.config.limits.wall;
        match call_worker(slot, req, wall, cancel) {
            Ok(resp) => {
                if !matches!(req, JobRequest::Ping | JobRequest::Shutdown)
                    && let Ok(JobResponse::Pong { rss: Some(rss) }) =
                        call_worker(slot, &JobRequest::Ping, Duration::from_secs(5), cancel)
                    && rss > self.config.memory_watermark
                {
                    tracing::debug!("worker restart: rss_watermark");
                    slot.kill_tree();
                    *slot = spawn_worker(&self.config)?;
                }
                Ok(resp)
            }
            Err(e) => {
                // Cancelled: do not restart-and-retry.
                if cancel.is_cancelled() {
                    slot.kill_tree();
                    *slot = spawn_worker(&self.config)?;
                    return Err(e);
                }
                tracing::debug!("worker restart: dead");
                slot.kill_tree();
                *slot = spawn_worker(&self.config)?;
                call_worker(slot, req, wall, cancel).map_err(|_| e)
            }
        }
    }

    /// Lower a package via the worker pool.
    pub fn lower(&self, lang: WorkerLang, root: &Path) -> Result<String, SandboxError> {
        match self.submit(JobRequest::Lower {
            lang,
            root: root.to_path_buf(),
        })? {
            JobResponse::Ok { body } => Ok(body),
            JobResponse::Err { kind, message } => Err(CageError::WorkerProtocol {
                detail: format!("{kind}: {message}"),
            }
            .into()),
            JobResponse::Pong { .. } => Err(CageError::WorkerProtocol {
                detail: "worker returned pong for lower".into(),
            }
            .into()),
        }
    }
}

impl Drop for WorkerPool {
    fn drop(&mut self) {
        if let Ok(mut slots) = self.free.slots.lock() {
            for mut slot in slots.drain(..) {
                let _ = call_worker(
                    &mut slot,
                    &JobRequest::Shutdown,
                    Duration::from_secs(2),
                    &CancelToken::never(),
                );
                slot.kill_tree();
            }
        }
    }
}

fn spawn_worker(config: &WorkerPoolConfig) -> Result<WorkerSlot, SandboxError> {
    let scratch = tempfile::Builder::new()
        .prefix("nudox-worker-")
        .tempdir()
        .map_err(SandboxError::Io)?;
    // Persist the directory for the worker process; TempDir would delete on drop.
    let scratch = scratch.keep();

    let mut cmd = Command::new(&config.worker_bin);
    cmd.arg("serve")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .env_clear()
        .env("HOME", &scratch)
        .env("TMPDIR", &scratch)
        .env("PATH", "/usr/bin:/bin")
        .current_dir(&scratch);

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let limits = config.limits;
        unsafe {
            cmd.pre_exec(move || {
                crate::backend::supervisor::apply_rlimits(&limits)
                    .map_err(crate::error::to_io_error)
            });
        }
    }

    let cgroup = crate::cgroup::Cgroup::try_create(&config.limits)?;
    let mut child = cmd.spawn().map_err(SandboxError::Spawn)?;
    if let Some(ref cg) = cgroup {
        let _ = cg.add_pid(child.id());
    }

    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| CageError::WorkerProtocol {
            detail: "worker stdin missing".into(),
        })?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| CageError::WorkerProtocol {
            detail: "worker stdout missing".into(),
        })?;

    // Non-blocking pipes so wall/cancel deadlines cannot hang the slot forever
    // on a full write buffer or a silent worker.
    make_nonblocking(&stdin);
    make_nonblocking(&stdout);

    Ok(WorkerSlot {
        child,
        stdin,
        stdout,
        line_buf: Vec::new(),
        cgroup,
        scratch,
    })
}

/// Cap on a single protocol line (worker responses are IR JSON, not unbounded).
const MAX_LINE_BYTES: usize = 4 * 1024 * 1024;

fn call_worker(
    slot: &mut WorkerSlot,
    req: &JobRequest,
    wall: Duration,
    cancel: &CancelToken,
) -> Result<JobResponse, SandboxError> {
    if cancel.is_cancelled() {
        return Err(CageError::Cancelled.into());
    }

    let mut line = serde_json::to_string(req).map_err(|e| CageError::WorkerProtocol {
        detail: format!("serialize request: {e}"),
    })?;
    line.push('\n');

    let deadline = Instant::now() + wall;
    write_all_deadline(slot, line.as_bytes(), deadline, cancel, wall)?;

    loop {
        if cancel.is_cancelled() {
            slot.kill_tree();
            return Err(CageError::Cancelled.into());
        }
        if Instant::now() >= deadline {
            slot.kill_tree();
            return Err(SandboxError::Killed {
                reason: KillReason::Wall,
                wall,
            });
        }

        match read_line_nonblock(slot) {
            Ok(Some(response)) => {
                return serde_json::from_str(response.trim()).map_err(|e| {
                    CageError::WorkerProtocol {
                        detail: format!("worker response: {e}"),
                    }
                    .into()
                });
            }
            Ok(None) => {
                // Would block — brief sleep, still bound by deadline.
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(e) => return Err(e),
        }
    }
}

/// Write `bytes` to the worker stdin with a wall/cancel deadline (non-blocking).
fn write_all_deadline(
    slot: &mut WorkerSlot,
    mut bytes: &[u8],
    deadline: Instant,
    cancel: &CancelToken,
    wall: Duration,
) -> Result<(), SandboxError> {
    while !bytes.is_empty() {
        if cancel.is_cancelled() {
            slot.kill_tree();
            return Err(CageError::Cancelled.into());
        }
        if Instant::now() >= deadline {
            slot.kill_tree();
            return Err(SandboxError::Killed {
                reason: KillReason::Wall,
                wall,
            });
        }
        match slot.stdin.write(bytes) {
            Ok(0) => {
                slot.kill_tree();
                return Err(CageError::WorkerProtocol {
                    detail: "worker stdin closed".into(),
                }
                .into());
            }
            Ok(n) => bytes = &bytes[n..],
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => {
                slot.kill_tree();
                return Err(CageError::WorkerProtocol {
                    detail: format!("worker write: {e}"),
                }
                .into());
            }
        }
    }
    // Best-effort flush (non-blocking may WouldBlock; remaining data is already
    // in the kernel buffer after successful write).
    match slot.stdin.flush() {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::WouldBlock => Ok(()),
        Err(e) => Err(CageError::WorkerProtocol {
            detail: format!("worker flush: {e}"),
        }
        .into()),
    }
}

/// Read one full line if available; `Ok(None)` on would-block.
fn read_line_nonblock(slot: &mut WorkerSlot) -> Result<Option<String>, SandboxError> {
    let mut tmp = [0u8; 4096];
    loop {
        match slot.stdout.read(&mut tmp) {
            Ok(0) => {
                return Err(CageError::WorkerProtocol {
                    detail: "worker closed pipe".into(),
                }
                .into());
            }
            Ok(n) => {
                if slot.line_buf.len() + n > MAX_LINE_BYTES {
                    slot.kill_tree();
                    return Err(CageError::WorkerProtocol {
                        detail: format!("worker line exceeds {MAX_LINE_BYTES} bytes"),
                    }
                    .into());
                }
                slot.line_buf.extend_from_slice(&tmp[..n]);
                if let Some(pos) = slot.line_buf.iter().position(|&b| b == b'\n') {
                    let line = slot.line_buf.drain(..=pos).collect::<Vec<u8>>();
                    let s = String::from_utf8_lossy(&line).into_owned();
                    return Ok(Some(s));
                }
                // More data may still be available.
                continue;
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(None),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => {
                return Err(CageError::WorkerProtocol {
                    detail: format!("worker read: {e}"),
                }
                .into());
            }
        }
    }
}

fn make_nonblocking(pipe: &impl std::os::fd::AsFd) {
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        let fd = pipe.as_fd().as_raw_fd();
        // SAFETY: fcntl on a pipe fd we own.
        unsafe {
            let flags = libc::fcntl(fd, libc::F_GETFL);
            if flags >= 0 {
                let _ = libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = pipe;
    }
}

/// One-shot: run a worker binary once under the full sandbox for a single lower.
pub fn lower_once(
    cage: &dyn Cage,
    worker_bin: &Path,
    lang: WorkerLang,
    root: &Path,
    limits: Limits,
    extra_ro: &[PathBuf],
) -> Result<String, SandboxError> {
    use crate::budget::{CapabilityBudget, FsGrant, NetGrant};
    use crate::seal::SealedCommand;

    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let mut ro = vec![worker_bin.to_path_buf(), root.clone()];
    for p in extra_ro {
        ro.push(p.clone());
    }

    let scratch = tempfile::Builder::new()
        .prefix("nudox-once-")
        .tempdir()
        .map_err(SandboxError::Io)?;
    let scratch = scratch.keep();

    let env = Env::empty()
        .set("HOME", &scratch)
        .set("TMPDIR", &scratch)
        .set("PATH", "/usr/bin:/bin:/nix/var/nix/profiles/default/bin");

    let mut fs = FsGrant::scratch(&scratch);
    for p in &ro {
        fs = fs.ro(p);
    }

    let budget = CapabilityBudget::new(fs, NetGrant::Off, env, limits);
    let args: Vec<std::ffi::OsString> = vec![
        "lower".into(),
        lang.as_str().into(),
        root.as_os_str().into(),
    ];
    let cmd = SealedCommand::new(worker_bin, args, budget).cwd(&scratch);

    let out = cage.run(cmd, &CancelToken::never());
    let _ = std::fs::remove_dir_all(&scratch);
    let out = out.map_err(SandboxError::from)?;
    if !out.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(CageError::WorkerProtocol {
            detail: format!("worker exit {:?}: {stderr}", out.end),
        }
        .into());
    }
    String::from_utf8(out.stdout).map_err(|e| {
        CageError::WorkerProtocol {
            detail: format!("utf8: {e}"),
        }
        .into()
    })
}
