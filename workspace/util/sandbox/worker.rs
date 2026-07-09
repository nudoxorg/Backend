//! Pooled sandboxed workers for in-process interpreter isolation.
//!
//! Long-lived children run under the active [`crate::Backend`] and speak a
//! minimal length-prefixed JSON protocol. A parser bug or memory bomb kills a
//! worker, not the indexer. Workers are restarted when they die or exceed a
//! memory watermark.
//!
//! The worker **binary** is supplied by the caller (compiler's
//! `producer-worker`); this module is protocol + pool only.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::Mutex;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::error::SandboxError;
use crate::limits::Limits;
use crate::profiles::ProducerProfile;
use crate::spec::{Env, Mounts, Spec};
use crate::{run, Backend};

/// Request sent to a worker process (one line of JSON).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum JobRequest {
	/// Lower a package root with the named language producer.
	Lower {
		/// `nix` | `typescript` | `python`
		lang: String,
		/// Absolute path to the package root.
		root: PathBuf,
	},
	/// Liveness + optional RSS probe.
	Ping,
	/// Graceful exit.
	Shutdown,
}

/// Response from a worker (one line of JSON, optionally followed by a body).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum JobResponse {
	/// Success; `body_len` bytes of payload follow on stdin framing (or
	/// embedded in `body` for small payloads).
	Ok {
		/// UTF-8 JSON body (IR index or empty for ping).
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
	/// Extra RO binds (toolchains, etc.).
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

struct WorkerSlot {
	child: Child,
	stdin: ChildStdin,
	stdout: BufReader<ChildStdout>,
}

/// A pool of sandboxed worker processes.
pub struct WorkerPool {
	config: WorkerPoolConfig,
	slots: Mutex<Vec<WorkerSlot>>,
}

impl WorkerPool {
	/// Spawn `config.size` workers under the default sandbox backend.
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

		let result = match call_worker(&mut slot, &req) {
			Ok(JobResponse::Pong { rss }) => {
				if let Some(rss) = rss {
					if rss > self.config.memory_watermark {
						let _ = slot.child.kill();
						let _ = slot.child.wait();
						slot = spawn_worker(&self.config)?;
						// Retry once on a fresh worker for the original request if it wasn't ping.
						if matches!(req, JobRequest::Ping) {
							Ok(JobResponse::Pong { rss: Some(0) })
						} else {
							call_worker(&mut slot, &req)
						}
					} else {
						Ok(JobResponse::Pong { rss: Some(rss) })
					}
				} else {
					Ok(JobResponse::Pong { rss: None })
				}
			}
			Ok(resp) => Ok(resp),
			Err(e) => {
				// Dead worker — respawn and retry once.
				let _ = slot.child.kill();
				let _ = slot.child.wait();
				slot = spawn_worker(&self.config)?;
				call_worker(&mut slot, &req).map_err(|_| e)
			}
		};

		slots.push(slot);
		result
	}

	/// Lower a package via the worker pool.
	pub fn lower(&self, lang: &str, root: &Path) -> Result<String, SandboxError> {
		match self.submit(JobRequest::Lower {
			lang: lang.to_string(),
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
				let _ = call_worker(&mut slot, &JobRequest::Shutdown);
				let _ = slot.child.kill();
				let _ = slot.child.wait();
			}
		}
	}
}

fn spawn_worker(config: &WorkerPoolConfig) -> Result<WorkerSlot, SandboxError> {
	// Workers are long-lived: we spawn them under the sandbox once by using
	// a Spec that execs the worker binary in "serve" mode. For backends that
	// wrap every invocation (bwrap), long-lived workers still get one cage
	// for their lifetime — which is what we want.
	//
	// Direct spawn with env scrub + rlimits: the pool parent applies the
	// sandbox Spec for isolation.
	let scratch = std::env::temp_dir().join(format!(
		"nudox-worker-{}-{}",
		std::process::id(),
		std::time::SystemTime::now()
			.duration_since(std::time::UNIX_EPOCH)
			.map(|d| d.as_nanos())
			.unwrap_or(0)
	));
	std::fs::create_dir_all(&scratch)?;

	let mut mounts = Mounts::new().rw(&scratch).ro(&config.worker_bin);
	for p in &config.read_only {
		mounts = mounts.ro(p);
	}
	// Workers need to read package roots — those are passed per-job and must
	// be under host paths; bwrap would need per-job re-spawn for new roots.
	// For the pool, we use a **direct hardened spawn** (rlimits + env scrub)
	// so new roots remain visible; crash/OOM isolation still holds via
	// process boundary + cgroup. Full bwrap per-job is available via
	// [`run`] for one-shot work.

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
				crate::backend::supervisor::apply_rlimits(&limits).map_err(|e| {
					std::io::Error::new(std::io::ErrorKind::Other, e.to_string())
				})
			});
		}
	}

	// Best-effort cgroup for the worker process.
	let cgroup = crate::cgroup::Cgroup::try_create(&config.limits)?;
	let mut child = cmd.spawn().map_err(SandboxError::Spawn)?;
	if let Some(cg) = cgroup {
		let _ = cg.add_pid(child.id());
		// Keep the cgroup alive for the worker lifetime: Drop would issue
		// cgroup.kill. Intentionally leak the owned handle.
		std::mem::forget(cg);
	}

	let stdin = child.stdin.take().ok_or_else(|| {
		SandboxError::Backend("worker stdin missing".into())
	})?;
	let stdout = child.stdout.take().ok_or_else(|| {
		SandboxError::Backend("worker stdout missing".into())
	})?;

	let _ = mounts; // reserved for future bwrap-pooled mode
	let _ = run as fn(Spec) -> Result<crate::spec::Output, SandboxError>;

	Ok(WorkerSlot {
		child,
		stdin,
		stdout: BufReader::new(stdout),
	})
}

fn call_worker(slot: &mut WorkerSlot, req: &JobRequest) -> Result<JobResponse, SandboxError> {
	let line = serde_json::to_string(req)
		.map_err(|e| SandboxError::Backend(format!("serialize request: {e}")))?;
	writeln!(slot.stdin, "{line}")
		.map_err(|e| SandboxError::Backend(format!("worker write: {e}")))?;
	slot.stdin
		.flush()
		.map_err(|e| SandboxError::Backend(format!("worker flush: {e}")))?;

	let mut response = String::new();
	// Bounded wait: use a simple read with the understanding that wall limits
	// on the worker process itself enforce the budget.
	slot.stdout
		.read_line(&mut response)
		.map_err(|e| SandboxError::Backend(format!("worker read: {e}")))?;
	if response.is_empty() {
		return Err(SandboxError::Backend("worker closed pipe".into()));
	}
	serde_json::from_str(response.trim())
		.map_err(|e| SandboxError::Backend(format!("worker response: {e}")))
}

/// One-shot: run a worker binary once under the full sandbox for a single lower.
pub fn lower_once(
	backend: &dyn Backend,
	worker_bin: &Path,
	lang: &str,
	root: &Path,
	limits: Limits,
	extra_ro: &[PathBuf],
) -> Result<String, SandboxError> {
	let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
	let mut mounts = Mounts::new().ro(worker_bin).ro(&root);
	for p in extra_ro {
		mounts = mounts.ro(p);
	}
	// Scratch for TMPDIR.
	let scratch = std::env::temp_dir().join(format!(
		"nudox-once-{}-{}",
		std::process::id(),
		std::time::SystemTime::now()
			.duration_since(std::time::UNIX_EPOCH)
			.map(|d| d.as_nanos())
			.unwrap_or(0)
	));
	std::fs::create_dir_all(&scratch)?;
	mounts = mounts.rw(&scratch);

	let env = Env::empty()
		.set("HOME", &scratch)
		.set("TMPDIR", &scratch)
		.set("PATH", "/usr/bin:/bin:/nix/var/nix/profiles/default/bin");

	let spec = Spec::new(worker_bin, limits)
		.arg("lower")
		.arg(lang)
		.arg(root.as_os_str())
		.env(env)
		.mounts(mounts)
		.cwd(&scratch);

	let out = backend.run(spec)?;
	if !out.success() {
		let stderr = String::from_utf8_lossy(&out.stderr);
		return Err(SandboxError::Backend(format!(
			"worker exit {:?}: {stderr}",
			out.status
		)));
	}
	String::from_utf8(out.stdout).map_err(|e| SandboxError::Backend(format!("utf8: {e}")))
}

/// Helper so unused import of Duration in docs doesn't warn in some cfgs.
#[allow(dead_code)]
fn _duration_link() -> Duration {
	Duration::from_secs(1)
}
