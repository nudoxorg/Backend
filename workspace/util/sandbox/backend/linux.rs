//! Linux bubblewrap backend: namespaces + seccomp-via-FD + cgroups + rlimits.
//!
//! ## Layering (research-backed)
//!
//! 1. **cgroup** — create in parent; self-attach in bwrap's `pre_exec` (reduces
//!    fork race vs post-spawn attach only).
//! 2. **bwrap** — mount/pid/ipc/uts/net/user cage; `--die-with-parent`,
//!    `--new-session`, `--clearenv`, selective binds.
//! 3. **rlimits** — applied in `pre_exec` of bwrap (inherit to guest).
//! 4. **seccomp** — `bwrap --seccomp FD` only. Never `apply_filter` on the
//!    bwrap process: denylisting `unshare`/`mount` would break setup.
//! 5. **Landlock** — optional on the *direct* path (`run_direct_hardened`);
//!    bwrap already provides the FS cage.
//!
//! Resource authority: cgroup `memory.max` is real RAM; `RLIMIT_AS` is VA
//! headroom (~4×).

use std::ffi::{CString, OsString};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::backend::supervisor::{self, apply_rlimits};
use crate::backend::{Backend, Capabilities};
use crate::cgroup::Cgroup;
use crate::error::SandboxError;
use crate::limits::Network;
use crate::spec::{Output, Spec};

/// FD number passed to `bwrap --seccomp` (must stay clear of 0–2).
const SECCOMP_CHILD_FD: i32 = 3;

/// bubblewrap-backed Linux isolation.
#[derive(Debug, Default)]
pub struct LinuxBwrap {
	bwrap: Option<PathBuf>,
}

impl LinuxBwrap {
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

	fn build_bwrap_args(
		&self,
		spec: &Spec,
		seccomp: bool,
	) -> Result<Vec<OsString>, SandboxError> {
		let mut args: Vec<OsString> = Vec::new();

		// Namespace cage — hard-require net-off when requested; soft-try user/cgroup.
		args.push("--unshare-user-try".into());
		args.push("--unshare-ipc".into());
		args.push("--unshare-pid".into());
		args.push("--unshare-uts".into());
		args.push("--unshare-cgroup-try".into());
		if matches!(spec.network, Network::Off) {
			args.push("--unshare-net".into());
		}

		args.push("--die-with-parent".into());
		args.push("--new-session".into());
		args.push("--clearenv".into());

		if seccomp {
			// Filter installed *after* bwrap setup (not on the bwrap process's
			// own unshare/mount path).
			args.push("--seccomp".into());
			args.push(SECCOMP_CHILD_FD.to_string().into());
		}

		// Minimal FS.
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

		// Required roots (fail closed if we claim them — only add when present).
		// Nix store: full store RO is the practical linker/toolchain answer
		// (absolute PT_INTERP + RPATH into /nix/store/…).
		ro_bind_if(&mut args, "/nix/store", "/nix/store");
		ro_bind_if(&mut args, "/nix/var", "/nix/var");
		// FHS / distro toolchains (try-semantics via existence check).
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
		// resolv/hosts only matter when Network::On; bind RO either way for
		// tools that open them unconditionally.
		ro_bind_if(&mut args, "/etc/resolv.conf", "/etc/resolv.conf");
		ro_bind_if(&mut args, "/etc/hosts", "/etc/hosts");

		// Caller mounts.
		for path in &spec.mounts.read_only {
			let path = path.canonicalize().unwrap_or_else(|_| path.clone());
			if path.exists() {
				args.push("--ro-bind".into());
				args.push(path.as_os_str().into());
				args.push(path.as_os_str().into());
			}
		}
		for path in &spec.mounts.writable {
			let path = path.canonicalize().unwrap_or_else(|_| path.clone());
			if path.exists() {
				args.push("--bind".into());
				args.push(path.as_os_str().into());
				args.push(path.as_os_str().into());
			}
		}

		// Resolved command binary (and its store path is already under /nix).
		if let Ok(cmd) = which::which(&spec.command) {
			args.push("--ro-bind".into());
			args.push(cmd.as_os_str().into());
			args.push(cmd.as_os_str().into());
		} else if spec.command.exists() {
			let cmd = spec.command.canonicalize().unwrap_or_else(|_| spec.command.clone());
			args.push("--ro-bind".into());
			args.push(cmd.as_os_str().into());
			args.push(cmd.as_os_str().into());
		}

		// Env allowlist.
		for (k, v) in spec.env.iter() {
			args.push("--setenv".into());
			args.push(k.into());
			args.push(v.into());
		}
		let has = |key: &str| spec.env.iter().any(|(k, _)| k == key);
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

		if let Some(cwd) = &spec.cwd {
			let cwd = cwd.canonicalize().unwrap_or_else(|_| cwd.clone());
			args.push("--chdir".into());
			args.push(cwd.as_os_str().into());
		}

		args.push("--".into());
		if let Ok(cmd) = which::which(&spec.command) {
			args.push(cmd.into());
		} else {
			args.push(spec.command.as_os_str().into());
		}
		args.extend(spec.args.iter().cloned());

		Ok(args)
	}
}

fn ro_bind_if(args: &mut Vec<OsString>, src: &str, dst: &str) {
	if Path::new(src).exists() {
		args.push("--ro-bind".into());
		args.push(src.into());
		args.push(dst.into());
	}
}

/// Write seccomp BPF to a temp file; return path for `pre_exec` open+dup2.
#[cfg(target_os = "linux")]
fn write_seccomp_bpf_file() -> Result<PathBuf, SandboxError> {
	let bytes = crate::seccomp::denylist_bpf_bytes()?;
	let path = std::env::temp_dir().join(format!(
		"nudox-seccomp-{}-{}.bpf",
		std::process::id(),
		std::time::SystemTime::now()
			.duration_since(std::time::UNIX_EPOCH)
			.map(|d| d.as_nanos())
			.unwrap_or(0)
	));
	fs::write(&path, bytes)?;
	Ok(path)
}

#[cfg(not(target_os = "linux"))]
fn write_seccomp_bpf_file() -> Result<PathBuf, SandboxError> {
	Err(SandboxError::Backend("seccomp only on Linux".into()))
}

impl Backend for LinuxBwrap {
	fn name(&self) -> &'static str {
		"linux-bwrap"
	}

	fn capabilities(&self) -> Capabilities {
		Capabilities {
			isolation: true,
			network_off: true,
			fs_scope: true,
			seccomp: cfg!(target_os = "linux"),
			cgroups: true,
			production_grade: true,
		}
	}

	fn run(&self, spec: Spec) -> Result<Output, SandboxError> {
		let bwrap = self.bwrap.as_ref().ok_or_else(|| SandboxError::HelperMissing {
			program: "bwrap".into(),
		})?;

		let limits = spec.limits;
		let cgroup = Cgroup::try_create(&limits)?;
		let cgroup_procs = cgroup.as_ref().map(|c| c.procs_path());

		#[cfg(target_os = "linux")]
		let bpf_path = match write_seccomp_bpf_file() {
			Ok(p) => Some(p),
			Err(e) => {
				tracing::warn!(error = %e, "seccomp BPF compile failed; running without --seccomp");
				None
			}
		};
		#[cfg(not(target_os = "linux"))]
		let bpf_path: Option<PathBuf> = None;
		let use_seccomp = bpf_path.is_some();
		let bwrap_args = self.build_bwrap_args(&spec, use_seccomp)?;

		let mut cmd = Command::new(bwrap);
		cmd.args(&bwrap_args)
			.stdin(Stdio::null())
			.stdout(Stdio::piped())
			.stderr(Stdio::piped())
			.env_clear();

		#[cfg(unix)]
		{
			use std::os::unix::process::CommandExt;
			let limits = limits;
			let bpf_path = bpf_path.clone();
			let cgroup_procs = cgroup_procs;
			unsafe {
				cmd.pre_exec(move || {
					// 1) cgroup self-attach before any further forks from bwrap.
					if let Some(ref procs) = cgroup_procs {
						let _ = self_attach_cgroup(procs);
					}
					// 2) rlimits inherit to guest.
					apply_rlimits(&limits).map_err(|e| {
						std::io::Error::new(std::io::ErrorKind::Other, e.to_string())
					})?;
					// 3) Install seccomp BPF as FD for bwrap --seccomp (not apply_filter).
					if let Some(ref path) = bpf_path {
						open_seccomp_fd(path, SECCOMP_CHILD_FD)?;
					}
					// Landlock/seccomp-on-self deliberately omitted: they break bwrap setup.
					Ok(())
				});
			}
		}

		let child = cmd.spawn().map_err(SandboxError::Spawn)?;
		// Best-effort late attach if pre_exec missed (e.g. non-unix).
		if let Some(ref cg) = cgroup {
			let _ = cg.add_pid(child.id());
		}
		let result = supervisor::supervise(child, &limits, cgroup);
		if let Some(path) = bpf_path {
			let _ = fs::remove_file(path);
		}
		result
	}
}

/// Open `path` and `dup2` onto `target_fd` for bwrap inheritance.
#[cfg(unix)]
fn open_seccomp_fd(path: &Path, target_fd: i32) -> std::io::Result<()> {
	use std::os::unix::ffi::OsStrExt;
	let c_path = CString::new(path.as_os_str().as_bytes())
		.map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
	// SAFETY: open/dup2/close on a path we control; child is single-threaded.
	unsafe {
		let fd = libc::open(c_path.as_ptr(), libc::O_RDONLY | libc::O_CLOEXEC);
		if fd < 0 {
			return Err(std::io::Error::last_os_error());
		}
		// Clear CLOEXEC so bwrap inherits after exec… actually bwrap reads the
		// FD before exec of guest; the FD must survive into the bwrap process.
		// pre_exec runs in the child *before* exec of bwrap, so the open FD is
		// in the process that will become bwrap — dup2 to target, clear CLOEXEC.
		if libc::dup2(fd, target_fd) < 0 {
			let err = std::io::Error::last_os_error();
			libc::close(fd);
			return Err(err);
		}
		if fd != target_fd {
			libc::close(fd);
		}
		// Clear CLOEXEC on target_fd so it survives bwrap's own exec of guest
		// only if bwrap keeps it open for --seccomp read (it reads then can close).
		let flags = libc::fcntl(target_fd, libc::F_GETFD);
		if flags >= 0 {
			let _ = libc::fcntl(target_fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC);
		}
	}
	Ok(())
}

#[cfg(unix)]
fn self_attach_cgroup(procs_path: &Path) -> std::io::Result<()> {
	use std::io::Write;
	let pid = std::process::id();
	let mut f = fs::OpenOptions::new().write(true).open(procs_path)?;
	write!(f, "{pid}")?;
	Ok(())
}

/// Run without bwrap but with Landlock+seccomp+rlimit+cgroup (fallback / tests).
pub fn run_direct_hardened(spec: Spec) -> Result<Output, SandboxError> {
	let limits = spec.limits;
	let cgroup = Cgroup::try_create(&limits)?;
	let cgroup_procs = cgroup.as_ref().map(|c| c.procs_path());

	if which::which(&spec.command).is_err() && !spec.command.exists() {
		return Err(SandboxError::ToolchainMissing {
			program: spec.command_display(),
		});
	}

	let mut cmd = supervisor::base_command(&spec.command);
	cmd.args(&spec.args);
	for (k, v) in spec.env.iter() {
		cmd.env(k, v);
	}
	if let Some(cwd) = &spec.cwd {
		cmd.current_dir(cwd);
	}

	#[cfg(unix)]
	{
		use std::os::unix::process::CommandExt;
		let limits = limits;
		#[cfg(target_os = "linux")]
		let mounts = spec.mounts.clone();
		let cgroup_procs = cgroup_procs;
		unsafe {
			cmd.pre_exec(move || {
				if let Some(ref procs) = cgroup_procs {
					let _ = self_attach_cgroup(procs);
				}
				// Order: rlimit → landlock → seccomp (seccomp last).
				apply_rlimits(&limits).map_err(|e| {
					std::io::Error::new(std::io::ErrorKind::Other, e.to_string())
				})?;
				#[cfg(target_os = "linux")]
				{
					let _ = crate::landlock::restrict_self(&mounts);
					let _ = crate::seccomp::apply_denylist();
				}
				Ok(())
			});
		}
	}

	let child = cmd.spawn().map_err(SandboxError::Spawn)?;
	if let Some(ref cg) = cgroup {
		let _ = cg.add_pid(child.id());
	}
	supervisor::supervise(child, &limits, cgroup)
}
