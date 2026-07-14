//! Thin cgroup v2 controller — no zbus, just sysfs writes.
//!
//! Creates a child under the current process cgroup (or a configured parent),
//! applies memory/cpu/pids ceilings, attaches a pid, and reads `memory.peak`.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::error::{CageError, SandboxError};
use crate::limits::Limits;

static SEQ: AtomicU64 = AtomicU64::new(1);

/// A live cgroup directory we own for the duration of a job.
#[derive(Debug)]
pub struct Cgroup {
	path: PathBuf,
}

impl Cgroup {
	/// Best-effort: create a job cgroup and apply `limits`.
	///
	/// Returns `Ok(None)` when cgroup v2 is unavailable or undelegated — callers
	/// still have rlimits as belt-and-suspenders.
	pub fn try_create(limits: &Limits) -> Result<Option<Self>, SandboxError> {
		let Some(parent) = discover_writable_parent() else {
			tracing::debug!("cgroup v2: no writable parent; skipping");
			return Ok(None);
		};

		let name = format!(
			"nudox-job-{}-{}",
			std::process::id(),
			SEQ.fetch_add(1, Ordering::Relaxed)
		);
		let path = parent.join(&name);
		fs::create_dir(&path).map_err(|e| {
			SandboxError::from(CageError::CgroupWrite {
				path: path.clone(),
				message: format!("create: {e}"),
			})
		})?;

		let cg = Self { path };
		if let Err(e) = cg.apply(limits) {
			let _ = fs::remove_dir(&cg.path);
			return Err(e);
		}
		Ok(Some(cg))
	}

	fn apply(&self, limits: &Limits) -> Result<(), SandboxError> {
		write_file(
			self.path.join("memory.max"),
			&limits.mem_bytes.get().to_string(),
		)?;
		// Pin swap so memory.max is a real RAM ceiling.
		let _ = write_file(self.path.join("memory.swap.max"), "0");
		// cpu.max: quota period — 100ms period, one full CPU of quota scaled by...
		// We use 100000 100000 ≈ 1 CPU.
		write_file(self.path.join("cpu.max"), "100000 100000")?;
		write_file(self.path.join("pids.max"), &limits.pids.get().to_string())?;
		Ok(())
	}

	/// Move `pid` into this cgroup.
	pub fn add_pid(&self, pid: u32) -> Result<(), SandboxError> {
		write_file(self.path.join("cgroup.procs"), &pid.to_string())
	}

	/// Path to `cgroup.procs` for child self-attach in `pre_exec` (reduces
	/// fork-before-attach race vs parent-only post-spawn move).
	pub fn procs_path(&self) -> PathBuf {
		self.path.join("cgroup.procs")
	}

	/// Write `cgroup.kill` to stop every process in this cgroup (kernel ≥ 5.14).
	///
	/// Used for cancellation and hung-worker recovery. Best-effort: returns
	/// `Ok` when the file is missing (older kernels) after a silent skip.
	pub fn kill_all(&self) -> Result<(), SandboxError> {
		let path = self.path.join("cgroup.kill");
		match fs::write(&path, "1") {
			Ok(()) => Ok(()),
			Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
			Err(e) => Err(SandboxError::from(CageError::CgroupWrite {
				path,
				message: e.to_string(),
			})),
		}
	}

	/// Read `memory.peak` when present (kernel ≥ 5.19).
	pub fn peak_mem(&self) -> Option<u64> {
		fs::read_to_string(self.path.join("memory.peak"))
			.ok()
			.and_then(|s| s.trim().parse().ok())
	}

	/// Path for diagnostics.
	pub fn path(&self) -> &Path {
		&self.path
	}
}

impl Drop for Cgroup {
	fn drop(&mut self) {
		// Best-effort: kill remaining procs (kernel 5.14+), then remove.
		// Callers that own a live job (see `WorkerSlot`) should kill+wait the
		// primary PID first so `cgroup.procs` is empty; a non-empty cgroup can
		// leave the directory behind after a single `remove_dir`.
		let _ = self.kill_all();
		let _ = fs::remove_dir(&self.path);
	}
}

fn write_file(path: PathBuf, contents: &str) -> Result<(), SandboxError> {
	fs::write(&path, contents).map_err(|e| {
		SandboxError::from(CageError::CgroupWrite {
			path,
			message: e.to_string(),
		})
	})
}

/// Whether a writable cgroup v2 parent exists (for probes / admission).
pub fn has_writable_parent() -> bool {
	discover_writable_parent().is_some()
}

fn discover_writable_parent() -> Option<PathBuf> {
	// Prefer our own cgroup (user delegation under systemd).
	if let Some(own) = read_own_cgroup_path()
		&& dir_is_writable(&own) {
			return Some(own);
		}
	// Fallback: /sys/fs/cgroup if writable (rare for unprivileged).
	let root = PathBuf::from("/sys/fs/cgroup");
	if dir_is_writable(&root) {
		return Some(root);
	}
	None
}

fn read_own_cgroup_path() -> Option<PathBuf> {
	// cgroup v2: /proc/self/cgroup is `0::/user.slice/...`
	let text = fs::read_to_string("/proc/self/cgroup").ok()?;
	for line in text.lines() {
		if let Some(rest) = line.strip_prefix("0::") {
			let rel = rest.trim().trim_start_matches('/');
			let path = if rel.is_empty() {
				PathBuf::from("/sys/fs/cgroup")
			} else {
				PathBuf::from("/sys/fs/cgroup").join(rel)
			};
			return Some(path);
		}
	}
	None
}

fn dir_is_writable(path: &Path) -> bool {
	if !path.is_dir() {
		return false;
	}
	// Probe by creating + removing a temp child name.
	let probe = path.join(format!(".nudox-probe-{}", std::process::id()));
	match fs::create_dir(&probe) {
		Ok(()) => {
			let _ = fs::remove_dir(&probe);
			true
		}
		Err(e) if e.kind() == io::ErrorKind::PermissionDenied => false,
		Err(_) => false,
	}
}
