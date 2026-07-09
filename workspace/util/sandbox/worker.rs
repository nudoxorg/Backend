//! Pooled sandboxed workers for in-process interpreter isolation.
//!
//! Long-lived children run under rlimits + cgroup (and env scrub) and speak a
//! minimal line-oriented JSON protocol. A parser bug or memory bomb kills a
//! worker, not the indexer. Workers restart on death or RSS watermark.
//!
//! The worker **binary** is supplied by the caller (compiler's
//! `producer-worker`); this module is protocol + pool only.
//!
//! Full bwrap-per-job remains available via [`crate::run`] / [`lower_once`]
//! for one-shot work where the package root set is fixed at spawn.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::error::SandboxError;
use crate::limits::Limits;
use crate::observer;
use crate::profiles::ProducerProfile;
use crate::spec::{Env, Mounts, Spec};
use crate::Backend;

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
	/// How many workers to keep warm.
	pub size: usize,
	/// Resource ceilings per worker process.
	pub limits: Limits,
	/// Extra RO binds (toolchains, etc.) — reserved for bwrap-pooled mode.
	pub read_only: Vec<PathBuf>,
	/// Restart worker if self-reported RSS exceeds this (bytes).
	pub memory_watermark: u64,
}

impl WorkerPoolConfig {
	/// Defaults for static parsers (design §7.5).
	pub fn static_parser(worker_bin: impl Into<PathBuf>) -> Self {
		Self {
			worker_bin: worker_bin.into(),
			size: 2,
			limits: ProducerProfile::StaticParser.limits(),
			read_only: Vec::new(),
			memory_watermark: 768 * 1024 * 1024,
		}
	}

	/// Defaults for snix workers.
	pub fn nix(worker_bin: impl Into<PathBuf>) -> Self {
		Self {
			worker_bin: worker_bin.into(),
			size: 2,
			limits: ProducerProfile::Nix.limits(),
			read_only: Vec::new(),
			memory_watermark: 768 * 1024 * 1024,
		}
	}
}

/// One warm worker process.
///
/// **Drop contract:** pool restart / pool drop paths kill and `wait` the child
/// before the slot is dropped so the owned cgroup is empty when reaped. An
/// accidental early drop still best-effort kills via `Child::Drop` and then
/// removes the cgroup dir (may leave a non-empty cgroup if procs linger).
struct WorkerSlot {
	child: Child,
	stdin: ChildStdin,
	stdout: BufReader<ChildStdout>,
	/// Owned for the worker's lifetime; dropped on restart so cgroup dirs are
	/// reaped (was previously leaked via `mem::forget`).
	_cgroup: Option<crate::cgroup::Cgroup>,
	/// Per-worker HOME/TMPDIR; removed when the slot is dropped.
	scratch: PathBuf,
}

impl Drop for WorkerSlot {
	fn drop(&mut self) {
		// Best-effort: pool paths already waited; this covers accidental drops.
		let _ = self.child.kill();
		let _ = self.child.wait();
		let _ = std::fs::remove_dir_all(&self.scratch);
	}
}

/// A pool of sandboxed worker processes.
pub struct WorkerPool {
	config: WorkerPoolConfig,
	slots: Mutex<Vec<WorkerSlot>>,
}

impl WorkerPool {
	/// Spawn `config.size` workers under rlimits + cgroup.
	pub fn new(config: WorkerPoolConfig) -> Result<Self, SandboxError> {
		let mut slots = Vec::with_capacity(config.size);
		for _ in 0..config.size.max(1) {
			slots.push(spawn_worker(&config)?);
		}
		Ok(Self {
			config,
			slots: Mutex::new(slots),
		})
	}

	/// Submit a job to any free worker (serialised for simplicity).
	pub fn submit(&self, req: JobRequest) -> Result<JobResponse, SandboxError> {
		let mut slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
		if slots.is_empty() {
			slots.push(spawn_worker(&self.config)?);
		}
		let mut slot = slots.pop().expect("just ensured non-empty");

		let result = match call_worker(&mut slot, &req, self.config.limits.wall) {
			Ok(resp) => {
				// Opportunistic watermark: ping after work when RSS is cheap.
				if !matches!(req, JobRequest::Ping | JobRequest::Shutdown) {
					if let Ok(JobResponse::Pong { rss: Some(rss) }) =
						call_worker(&mut slot, &JobRequest::Ping, Duration::from_secs(5))
					{
						if rss > self.config.memory_watermark {
							observer::global().worker_restart("rss_watermark");
							let _ = slot.child.kill();
							let _ = slot.child.wait();
							slot = spawn_worker(&self.config)?;
						}
					}
				}
				Ok(resp)
			}
			Err(e) => {
				observer::global().worker_restart("dead");
				let _ = slot.child.kill();
				let _ = slot.child.wait();
				slot = spawn_worker(&self.config)?;
				call_worker(&mut slot, &req, self.config.limits.wall).map_err(|_| e)
			}
		};

		slots.push(slot);
		result
	}

	/// Lower a package via the worker pool.
	pub fn lower(&self, lang: WorkerLang, root: &Path) -> Result<String, SandboxError> {
		match self.submit(JobRequest::Lower {
			lang,
			root: root.to_path_buf(),
		})? {
			JobResponse::Ok { body } => Ok(body),
			JobResponse::Err { kind, message } => Err(SandboxError::Backend(format!(
				"worker {kind}: {message}"
			))),
			JobResponse::Pong { .. } => Err(SandboxError::Backend(
				"worker returned pong for lower".into(),
			)),
		}
	}
}

impl Drop for WorkerPool {
	fn drop(&mut self) {
		if let Ok(mut slots) = self.slots.lock() {
			for mut slot in slots.drain(..) {
				let _ = call_worker(&mut slot, &JobRequest::Shutdown, Duration::from_secs(2));
				let _ = slot.child.kill();
				let _ = slot.child.wait();
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
				crate::backend::supervisor::apply_rlimits(&limits).map_err(crate::error::to_io_error)
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
		.ok_or_else(|| SandboxError::Backend("worker stdin missing".into()))?;
	let stdout = child
		.stdout
		.take()
		.ok_or_else(|| SandboxError::Backend("worker stdout missing".into()))?;

	Ok(WorkerSlot {
		child,
		stdin,
		stdout: BufReader::new(stdout),
		_cgroup: cgroup,
		scratch,
	})
}

fn call_worker(
	slot: &mut WorkerSlot,
	req: &JobRequest,
	wall: Duration,
) -> Result<JobResponse, SandboxError> {
	let line = serde_json::to_string(req)
		.map_err(|e| SandboxError::Backend(format!("serialize request: {e}")))?;
	writeln!(slot.stdin, "{line}")
		.map_err(|e| SandboxError::Backend(format!("worker write: {e}")))?;
	slot.stdin
		.flush()
		.map_err(|e| SandboxError::Backend(format!("worker flush: {e}")))?;

	let deadline = Instant::now() + wall;
	loop {
		if Instant::now() >= deadline {
			let _ = slot.child.kill();
			return Err(SandboxError::Killed {
				reason: crate::error::KillReason::Wall,
				wall,
			});
		}
		// Non-blocking poll of the child; blocking read_line is acceptable
		// under wall budget because the supervisor-equivalent is the kill above
		// on next iteration — use a short approach: read_line blocks, so we
		// rely on the worker process rlimit/wall via OS. For hung workers,
		// the outer indexer deadline is the backstop.
		let mut response = String::new();
		match slot.stdout.read_line(&mut response) {
			Ok(0) => return Err(SandboxError::Backend("worker closed pipe".into())),
			Ok(_) => {
				return serde_json::from_str(response.trim())
					.map_err(|e| SandboxError::Backend(format!("worker response: {e}")));
			}
			Err(e) => return Err(SandboxError::Backend(format!("worker read: {e}"))),
		}
	}
}

/// One-shot: run a worker binary once under the full sandbox for a single lower.
pub fn lower_once(
	backend: &dyn Backend,
	worker_bin: &Path,
	lang: WorkerLang,
	root: &Path,
	limits: Limits,
	extra_ro: &[PathBuf],
) -> Result<String, SandboxError> {
	let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
	let mut mounts = Mounts::new().ro(worker_bin).ro(&root);
	for p in extra_ro {
		mounts = mounts.ro(p);
	}
	let scratch = tempfile::Builder::new()
		.prefix("nudox-once-")
		.tempdir()
		.map_err(SandboxError::Io)?;
	let scratch = scratch.keep();
	mounts = mounts.rw(&scratch);

	let env = Env::empty()
		.set("HOME", &scratch)
		.set("TMPDIR", &scratch)
		.set("PATH", "/usr/bin:/bin:/nix/var/nix/profiles/default/bin");

	let spec = Spec::new(worker_bin, limits)
		.arg("lower")
		.arg(lang.as_str())
		.arg(root.as_os_str())
		.env(env)
		.mounts(mounts)
		.cwd(&scratch);

	let out = backend.run(spec);
	let _ = std::fs::remove_dir_all(&scratch);
	let out = out?;
	if !out.success() {
		let stderr = String::from_utf8_lossy(&out.stderr);
		return Err(SandboxError::Backend(format!(
			"worker exit {:?}: {stderr}",
			out.end
		)));
	}
	String::from_utf8(out.stdout).map_err(|e| SandboxError::Backend(format!("utf8: {e}")))
}
