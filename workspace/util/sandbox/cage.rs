//! The one cage abstraction (DAEMON-PLAN §2.2).
//!
//! Isolation is a trait, not a peer-of-backends enum. Production work runs under
//! [`LinuxNamespaces`]; dev work under [`DevPassthrough`] (constructible only
//! with [`Policy::Development`]). [`WorkerPool`](crate::WorkerPool) is
//! cage-internal for library-form producers, not a top-level backend peer.

use std::path::PathBuf;

use crate::backend::supervisor::{self, apply_rlimits};
use crate::budget::NetGrant;
use crate::cancel::CancelToken;
use crate::cgroup::Cgroup;
use crate::error::CageError;
#[cfg(target_os = "linux")]
use crate::error::SandboxError;
use crate::seal::SealedCommand;
use crate::spec::Output;

/// Stable identity of a cage instance (logs / metrics).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CageId(pub &'static str);

impl CageId {
	/// Borrow the id string.
	pub fn as_str(&self) -> &'static str {
		self.0
	}
}

impl std::fmt::Display for CageId {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.write_str(self.0)
	}
}

/// What a cage can enforce.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CageCaps {
	/// Namespaces / seatbelt profile available.
	pub isolation: bool,
	/// Network can be forced off.
	pub network_off: bool,
	/// Landlock or equivalent FS scoping.
	pub fs_scope: bool,
	/// seccomp denylist.
	pub seccomp: bool,
	/// cgroup resource control.
	pub cgroups: bool,
	/// Suitable as production security boundary of record.
	pub production_grade: bool,
}


/// Runtime policy resolved once at assemble (Phase 4 owns full wiring).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Policy {
	/// Production: only production-grade cages.
	Production,
	/// Development: passthrough / seatbelt allowed.
	Development,
}

impl Policy {
	/// From the existing isolation env gate.
	pub fn from_env() -> Self {
		if crate::probe::IsolationPolicy::env_requires_production() {
			Self::Production
		} else {
			Self::Development
		}
	}
}

/// Capability-budgeted process isolation.
pub trait Cage: Send + Sync {
	/// Stable cage identity.
	fn id(&self) -> CageId;

	/// What this cage can enforce on this host.
	fn capabilities(&self) -> CageCaps;

	/// Run a sealed command under the budget; honor `cancel` when possible.
	fn run(&self, cmd: SealedCommand, cancel: &CancelToken) -> Result<Output, CageError>;
}

// ─── LinuxNamespaces ────────────────────────────────────────────────────────

/// FD number passed to `bwrap --seccomp` (must stay clear of 0–2).
const SECCOMP_CHILD_FD: i32 = 3;

/// Production cage: bwrap namespaces + cgroup v2 + seccomp-via-FD + rlimits.
///
/// Absorbs today's `LinuxBwrap` Backend. Seccomp BPF is compiled once per
/// process into a cached blob (memfd when available).
#[derive(Debug, Default)]
pub struct LinuxNamespaces {
	bwrap: Option<PathBuf>,
}

impl LinuxNamespaces {
	/// Construct and locate `bwrap`.
	pub fn new() -> Self {
		Self {
			bwrap: which::which("bwrap").ok(),
		}
	}

	/// Whether bwrap is on PATH.
	pub fn probe_available(&self) -> bool {
		self.bwrap.is_some()
	}

	#[allow(clippy::vec_init_then_push)]
	fn build_bwrap_args(
		&self,
		cmd: &SealedCommand,
		seccomp: bool,
	) -> Result<Vec<std::ffi::OsString>, CageError> {
		use std::ffi::OsString;

		let mut args: Vec<OsString> = Vec::new();

		args.push("--unshare-user-try".into());
		args.push("--unshare-ipc".into());
		args.push("--unshare-pid".into());
		args.push("--unshare-uts".into());
		args.push("--unshare-cgroup-try".into());
		if matches!(cmd.budget.net, NetGrant::Off) {
			args.push("--unshare-net".into());
		}

		args.push("--die-with-parent".into());
		args.push("--new-session".into());
		args.push("--clearenv".into());

		if seccomp {
			args.push("--seccomp".into());
			args.push(SECCOMP_CHILD_FD.to_string().into());
		}

		args.push("--proc".into());
		args.push("/proc".into());
		args.push("--dev".into());
		args.push("/dev".into());
		args.push("--tmpfs".into());
		args.push("/tmp".into());
		args.push("--tmpfs".into());
		args.push("/var".into());
		args.push("--tmpfs".into());
		args.push("/run".into());

		ro_bind_if(&mut args, "/nix/store", "/nix/store");
		ro_bind_if(&mut args, "/nix/var", "/nix/var");
		for (src, dst) in [
			("/usr", "/usr"),
			("/bin", "/bin"),
			("/lib", "/lib"),
			("/lib64", "/lib64"),
			("/sbin", "/sbin"),
			("/etc/ssl", "/etc/ssl"),
			("/etc/ca-certificates", "/etc/ca-certificates"),
			("/etc/alternatives", "/etc/alternatives"),
			("/etc/static", "/etc/static"),
			("/etc/passwd", "/etc/passwd"),
			("/etc/group", "/etc/group"),
			("/etc/nsswitch.conf", "/etc/nsswitch.conf"),
		] {
			ro_bind_if(&mut args, src, dst);
		}
		ro_bind_if(&mut args, "/etc/resolv.conf", "/etc/resolv.conf");
		ro_bind_if(&mut args, "/etc/hosts", "/etc/hosts");

		// Primary scratch is required; optional extra binds stay soft-skip.
		if !cmd.budget.fs.scratch.exists() {
			return Err(CageError::MountMissing {
				path: cmd.budget.fs.scratch.clone(),
			});
		}
		let mounts = cmd.budget.fs.to_mounts();
		for path in &mounts.read_only {
			let path = path.canonicalize().unwrap_or_else(|_| path.clone());
			if path.exists() {
				args.push("--ro-bind".into());
				args.push(path.as_os_str().into());
				args.push(path.as_os_str().into());
			}
		}
		for path in &mounts.writable {
			let path = path.canonicalize().unwrap_or_else(|_| path.clone());
			if path.exists() {
				args.push("--bind".into());
				args.push(path.as_os_str().into());
				args.push(path.as_os_str().into());
			}
		}

		if let Ok(bin) = which::which(&cmd.command) {
			args.push("--ro-bind".into());
			args.push(bin.as_os_str().into());
			args.push(bin.as_os_str().into());
		} else if cmd.command.exists() {
			let bin = cmd
				.command
				.canonicalize()
				.unwrap_or_else(|_| cmd.command.clone());
			args.push("--ro-bind".into());
			args.push(bin.as_os_str().into());
			args.push(bin.as_os_str().into());
		}

		for (k, v) in cmd.budget.env.iter() {
			args.push("--setenv".into());
			args.push(k.into());
			args.push(v.into());
		}
		let has = |key: &str| cmd.budget.env.iter().any(|(k, _)| k == key);
		if !has("PATH") {
			args.push("--setenv".into());
			args.push("PATH".into());
			args.push("/usr/bin:/bin:/nix/var/nix/profiles/default/bin".into());
		}
		if !has("HOME") {
			args.push("--setenv".into());
			args.push("HOME".into());
			args.push("/tmp".into());
		}
		if !has("TMPDIR") {
			args.push("--setenv".into());
			args.push("TMPDIR".into());
			args.push("/tmp".into());
		}

		if let Some(cwd) = &cmd.cwd {
			let cwd = cwd.canonicalize().unwrap_or_else(|_| cwd.clone());
			args.push("--chdir".into());
			args.push(cwd.as_os_str().into());
		}

		args.push("--".into());
		if let Ok(bin) = which::which(&cmd.command) {
			args.push(bin.into());
		} else {
			args.push(cmd.command.as_os_str().into());
		}
		args.extend(cmd.args.iter().cloned());
		Ok(args)
	}

	fn run_inner(&self, cmd: SealedCommand, cancel: &CancelToken) -> Result<Output, CageError> {
		if cancel.is_cancelled() {
			return Err(CageError::Cancelled);
		}

		let bwrap = self
			.bwrap
			.as_ref()
			.ok_or_else(|| CageError::HelperMissing {
				program: "bwrap".into(),
			})?;

		let limits = cmd.budget.resources;
		let cgroup = Cgroup::try_create(&limits).map_err(CageError::from)?;
		let cgroup_procs = cgroup.as_ref().map(|c| c.procs_path());

		#[cfg(target_os = "linux")]
		let bpf_path = match seccomp_bpf_path() {
			Ok(p) => Some(p),
			Err(e) => {
				tracing::warn!(error = %e, "seccomp BPF compile failed; running without --seccomp");
				None
			}
		};
		#[cfg(not(target_os = "linux"))]
		let bpf_path: Option<PathBuf> = None;
		let use_seccomp = bpf_path.is_some();
		let bwrap_args = self.build_bwrap_args(&cmd, use_seccomp)?;

		use std::process::{Command, Stdio};
		let mut proc = Command::new(bwrap);
		proc.args(&bwrap_args)
			.stdin(Stdio::null())
			.stdout(Stdio::piped())
			.stderr(Stdio::piped())
			.env_clear();

		#[cfg(unix)]
		{
			use std::os::unix::process::CommandExt;
			#[allow(clippy::redundant_locals)]
			let limits = limits;
			let bpf_path = bpf_path.clone();
			#[allow(clippy::redundant_locals)]
			let cgroup_procs = cgroup_procs;
			unsafe {
				proc.pre_exec(move || {
					if let Some(ref procs) = cgroup_procs {
						let _ = self_attach_cgroup(procs);
					}
					apply_rlimits(&limits).map_err(crate::error::to_io_error)?;
					if let Some(ref path) = bpf_path {
						open_seccomp_fd(path, SECCOMP_CHILD_FD)?;
					}
					Ok(())
				});
			}
		}

		if cancel.is_cancelled() {
			// No child yet — nothing to kill.
			return Err(CageError::Cancelled);
		}

		let child = proc.spawn().map_err(CageError::Spawn)?;
		if let Some(ref cg) = cgroup {
			let _ = cg.add_pid(child.id());
		}

		// Mid-run cancel: supervisor polls cancel each iteration and force-kills
		// via cgroup.kill + child.kill.
		supervisor::supervise(child, &limits, cgroup, cancel).map_err(CageError::from)
	}
}

impl Cage for LinuxNamespaces {
	fn id(&self) -> CageId {
		CageId("linux-namespaces")
	}

	fn capabilities(&self) -> CageCaps {
		CageCaps {
			isolation: true,
			network_off: true,
			fs_scope: true,
			seccomp: cfg!(target_os = "linux"),
			cgroups: true,
			production_grade: true,
		}
	}

	fn run(&self, cmd: SealedCommand, cancel: &CancelToken) -> Result<Output, CageError> {
		self.run_inner(cmd, cancel)
	}
}

/// Historical name for [`LinuxNamespaces`] (Backend selection + re-exports).
pub type LinuxBwrap = LinuxNamespaces;

fn ro_bind_if(args: &mut Vec<std::ffi::OsString>, src: &str, dst: &str) {
	use std::path::Path;
	if Path::new(src).exists() {
		args.push("--ro-bind".into());
		args.push(src.into());
		args.push(dst.into());
	}
}

/// Compile denylist BPF once on **success**; failures are not cached so a
/// later retry can still install the filter.
#[cfg(target_os = "linux")]
fn seccomp_bpf_path() -> Result<PathBuf, CageError> {
	static PATH: OnceLock<PathBuf> = OnceLock::new();
	if let Some(p) = PATH.get() {
		return Ok(p.clone());
	}
	let p = compile_seccomp_path()?;
	let _ = PATH.set(p.clone());
	Ok(p)
}

#[cfg(target_os = "linux")]
fn compile_seccomp_path() -> Result<PathBuf, CageError> {
	#[cfg(target_os = "linux")]
	{
		let bytes = crate::seccomp::denylist_bpf_bytes().map_err(|e| match e {
			SandboxError::Backend(s) => CageError::SeccompCompile(s),
			other => CageError::SeccompCompile(other.to_string()),
		})?;

		if let Some(path) = write_memfd_seccomp(&bytes) {
			return Ok(path);
		}

		use std::io::Write;
		let mut file = tempfile::Builder::new()
			.prefix("nudox-seccomp-")
			.suffix(".bpf")
			.tempfile()
			.map_err(CageError::Io)?;
		file.write_all(&bytes).map_err(CageError::Io)?;
		let (_, path) = file.keep().map_err(|e| CageError::Io(e.error))?;
		Ok(path)
	}
	#[cfg(not(target_os = "linux"))]
	{
		Err(CageError::SeccompCompile("seccomp only on Linux".into()))
	}
}

/// Write BPF bytes into a memfd and return a `/proc/self/fd/N` path.
#[cfg(target_os = "linux")]
fn write_memfd_seccomp(bytes: &[u8]) -> Option<PathBuf> {
	use std::io::Write;
	use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

	// SAFETY: memfd_create with a static name; we own the resulting fd.
	let fd = unsafe {
		let name = c"nudox-seccomp";
		let raw = libc::memfd_create(name.as_ptr(), libc::MFD_CLOEXEC);
		if raw < 0 {
			return None;
		}
		OwnedFd::from_raw_fd(raw)
	};
	let mut file = std::fs::File::from(fd);
	file.write_all(bytes).ok()?;
	let raw = file.as_raw_fd();
	// Leak so the memfd lives for the process (OnceLock).
	std::mem::forget(file);
	Some(PathBuf::from(format!("/proc/self/fd/{raw}")))
}

#[cfg(unix)]
fn open_seccomp_fd(path: &std::path::Path, target_fd: i32) -> std::io::Result<()> {
	use std::ffi::CString;
	use std::os::unix::ffi::OsStrExt;
	let c_path = CString::new(path.as_os_str().as_bytes())
		.map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
	// SAFETY: open/dup2/close on a path we control; child is single-threaded.
	unsafe {
		let fd = libc::open(c_path.as_ptr(), libc::O_RDONLY | libc::O_CLOEXEC);
		if fd < 0 {
			return Err(std::io::Error::last_os_error());
		}
		if libc::dup2(fd, target_fd) < 0 {
			let err = std::io::Error::last_os_error();
			libc::close(fd);
			return Err(err);
		}
		if fd != target_fd {
			libc::close(fd);
		}
		let flags = libc::fcntl(target_fd, libc::F_GETFD);
		if flags >= 0 {
			let _ = libc::fcntl(target_fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC);
		}
	}
	Ok(())
}

#[cfg(unix)]
fn self_attach_cgroup(procs_path: &std::path::Path) -> std::io::Result<()> {
	use std::io::Write;
	let pid = std::process::id();
	let mut f = std::fs::OpenOptions::new().write(true).open(procs_path)?;
	write!(f, "{pid}")?;
	Ok(())
}

// ─── DevPassthrough ─────────────────────────────────────────────────────────

/// Dev-only cage: rlimits (+ best-effort cgroup), no namespaces.
///
/// Constructible only from [`Policy::Development`] — the type cannot be built
/// under a production policy, replacing thrice-read env gates at the boundary.
#[derive(Debug)]
pub struct DevPassthrough {
	_policy: DevOnly,
}

/// Zero-sized token proving construction went through [`Policy::Development`].
#[derive(Debug, Clone, Copy)]
struct DevOnly;

impl DevPassthrough {
	/// Construct when `policy` is [`Policy::Development`].
	pub fn try_new(policy: Policy) -> Result<Self, CageError> {
		match policy {
			Policy::Development => Ok(Self { _policy: DevOnly }),
			Policy::Production => Err(CageError::Denied {
				reason: "DevPassthrough is not constructible under Policy::Production".into(),
			}),
		}
	}

	/// Convenience: build from the process env policy (dev only).
	pub fn from_env() -> Result<Self, CageError> {
		Self::try_new(Policy::from_env())
	}
}

impl Cage for DevPassthrough {
	fn id(&self) -> CageId {
		CageId("dev-passthrough")
	}

	fn capabilities(&self) -> CageCaps {
		CageCaps {
			isolation: false,
			network_off: false,
			fs_scope: false,
			seccomp: false,
			cgroups: false,
			production_grade: false,
		}
	}

	fn run(&self, cmd: SealedCommand, cancel: &CancelToken) -> Result<Output, CageError> {
		if cancel.is_cancelled() {
			return Err(CageError::Cancelled);
		}

		if which::which(&cmd.command).is_err() && !cmd.command.exists() {
			return Err(CageError::ToolchainMissing {
				program: cmd.command_display(),
			});
		}

		let limits = cmd.budget.resources;
		let cgroup = Cgroup::try_create(&limits).map_err(CageError::from)?;

		use std::process::Stdio;
		let mut proc = supervisor::base_command(&cmd.command);
		proc.args(&cmd.args);
		for (k, v) in cmd.budget.env.iter() {
			proc.env(k, v);
		}
		if let Some(cwd) = &cmd.cwd {
			proc.current_dir(cwd);
		}
		proc.stdin(Stdio::null());

		#[cfg(unix)]
		{
			use std::os::unix::process::CommandExt;
			#[allow(clippy::redundant_locals)]
			let limits = limits;
			unsafe {
				proc.pre_exec(move || {
					apply_rlimits(&limits).map_err(crate::error::to_io_error)?;
					Ok(())
				});
			}
		}

		if cancel.is_cancelled() {
			return Err(CageError::Cancelled);
		}

		let child = proc.spawn().map_err(CageError::Spawn)?;
		supervisor::supervise(child, &limits, cgroup, cancel).map_err(CageError::from)
	}
}

/// Run a sealed command on a platform-appropriate cage, honoring `cancel`.
///
/// Uses [`LinuxNamespaces`] when bwrap is available; otherwise
/// [`DevPassthrough`] under a development policy. Fails with
/// [`CageError::Denied`] on a production host without bwrap.
pub fn run_sealed(cmd: SealedCommand, cancel: &CancelToken) -> Result<Output, CageError> {
	if cancel.is_cancelled() {
		return Err(CageError::Cancelled);
	}

	#[cfg(target_os = "linux")]
	{
		let linux = LinuxNamespaces::new();
		if linux.probe_available() {
			return Cage::run(&linux, cmd, cancel);
		}
	}

	match DevPassthrough::try_new(Policy::from_env()) {
		Ok(dev) => Cage::run(&dev, cmd, cancel),
		Err(_) => Err(CageError::Denied {
			reason: "no production-grade cage available (bwrap missing) and \
			         DevPassthrough is not permitted under Policy::Production"
				.into(),
		}),
	}
}
