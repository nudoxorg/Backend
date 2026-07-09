//! macOS Seatbelt (`sandbox-exec`) — accident prevention for local dev.
//!
//! Documented gaps: Mach-IPC can bypass coarse network denies; same-UID kernel
//! share. Never the security boundary of record (CI/prod is Linux).
//!
//! Opt-in via `NUDOX_SANDBOX=seatbelt` (default backend is passthrough — design
//! §12 / research: tight profiles Abort-trap Nix store binaries).

use std::process::{Command, Stdio};

use crate::backend::supervisor::{self, apply_rlimits};
use crate::backend::{Backend, Capabilities};
use crate::error::SandboxError;
use crate::limits::Network;
use crate::spec::{Output, Spec};

/// Seatbelt backend.
#[derive(Debug, Default)]
pub struct MacSeatbelt {
	sandbox_exec: Option<std::path::PathBuf>,
}

impl MacSeatbelt {
	/// Construct and probe for `sandbox-exec` (prefer absolute path).
	pub fn new() -> Self {
		Self {
			sandbox_exec: {
				let p = std::path::PathBuf::from("/usr/bin/sandbox-exec");
				if p.exists() {
					Some(p)
				} else {
					which::which("sandbox-exec").ok()
				}
			},
		}
	}

	/// Whether `sandbox-exec` is present.
	pub fn probe_available(&self) -> bool {
		self.sandbox_exec.is_some()
	}

	fn profile(spec: &Spec) -> String {
		let mut allows = String::new();
		for path in spec
			.mounts
			.read_only
			.iter()
			.chain(spec.mounts.writable.iter())
		{
			let s = path.display().to_string().replace('\\', "\\\\").replace('"', "\\\"");
			allows.push_str(&format!("  (subpath \"{s}\")\n"));
		}
		// System + Nix store (required for store-linked cargo/rustc dyld).
		for sys in [
			"/usr",
			"/bin",
			"/sbin",
			"/System",
			"/Library",
			"/opt",
			"/nix",
			"/private/tmp",
			"/private/var/folders",
			"/private/var/db", // dyld shared cache
			"/private/etc",
			"/tmp",
			"/dev",
		] {
			allows.push_str(&format!("  (subpath \"{sys}\")\n"));
		}

		let mut rw = String::new();
		for path in &spec.mounts.writable {
			let s = path.display().to_string().replace('\\', "\\\\").replace('"', "\\\"");
			rw.push_str(&format!("  (subpath \"{s}\")\n"));
		}
		rw.push_str("  (subpath \"/private/tmp\")\n");
		rw.push_str("  (subpath \"/tmp\")\n");
		rw.push_str("  (subpath \"/private/var/folders\")\n");
		rw.push_str("  (literal \"/dev/null\")\n");
		rw.push_str("  (literal \"/dev/stdout\")\n");
		rw.push_str("  (literal \"/dev/stderr\")\n");

		let net = match spec.network {
			Network::Off => "(deny network*)\n(deny network-outbound)\n(deny network-inbound)\n",
			Network::On => "(allow network*)\n",
		};

		// Curated mach-lookup: blanket allow is weak but deny-all aborts most
		// CLIs. Research: bring-up uses allow; tighten from log denials later.
		format!(
			r#"(version 1)
(deny default)
(allow process-exec)
(allow process-fork)
(allow signal (target self))
(allow file-map-executable)
(allow sysctl-read)
(allow mach-lookup
  (global-name "com.apple.system.logger")
  (global-name "com.apple.bsd.dirhelper")
  (global-name "com.apple.system.opendirectoryd.libinfo")
  (global-name "com.apple.system.opendirectoryd.membership")
  (global-name "com.apple.SystemConfiguration.configd")
  (global-name "com.apple.SecurityServer")
  (global-name "com.apple.coreservices.launchservicesd")
)
(allow file-read-metadata (subpath "/"))
(allow file-read*
{allows})
(allow file-read* file-write*
{rw})
{net}"#
		)
	}
}

impl Backend for MacSeatbelt {
	fn name(&self) -> &'static str {
		"mac-seatbelt"
	}

	fn capabilities(&self) -> Capabilities {
		Capabilities {
			isolation: true,
			network_off: true,
			fs_scope: true,
			seccomp: false,
			cgroups: false,
			production_grade: false,
		}
	}

	fn run(&self, spec: Spec) -> Result<Output, SandboxError> {
		let sandbox_exec = self.sandbox_exec.as_ref().ok_or_else(|| {
			SandboxError::HelperMissing {
				program: "sandbox-exec".into(),
			}
		})?;

		if which::which(&spec.command).is_err() && !spec.command.exists() {
			return Err(SandboxError::ToolchainMissing {
				program: spec.command_display(),
			});
		}

		let profile = Self::profile(&spec);
		let mut profile_file = tempfile_profile()?;
		profile_file
			.write_all(profile.as_bytes())
			.map_err(SandboxError::Io)?;
		let profile_path = profile_file.path().to_path_buf();

		let limits = spec.limits;
		let mut cmd = Command::new(sandbox_exec);
		cmd.arg("-f")
			.arg(&profile_path)
			.arg(&spec.command)
			.args(&spec.args)
			.stdin(Stdio::null())
			.stdout(Stdio::piped())
			.stderr(Stdio::piped())
			.env_clear();

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
			unsafe {
				cmd.pre_exec(move || {
					apply_rlimits(&limits).map_err(|e| {
						std::io::Error::new(std::io::ErrorKind::Other, e.to_string())
					})
				});
			}
		}

		let child = cmd.spawn().map_err(SandboxError::Spawn)?;
		drop(profile_file);
		supervisor::supervise(child, &limits, None)
	}
}

struct ProfileFile {
	path: std::path::PathBuf,
}

impl ProfileFile {
	fn path(&self) -> &std::path::Path {
		&self.path
	}

	fn write_all(&mut self, data: &[u8]) -> std::io::Result<()> {
		std::fs::write(&self.path, data)
	}
}

impl Drop for ProfileFile {
	fn drop(&mut self) {
		let _ = std::fs::remove_file(&self.path);
	}
}

fn tempfile_profile() -> Result<ProfileFile, SandboxError> {
	let path = std::env::temp_dir().join(format!(
		"nudox-seatbelt-{}-{}.sb",
		std::process::id(),
		std::time::SystemTime::now()
			.duration_since(std::time::UNIX_EPOCH)
			.map(|d| d.as_nanos())
			.unwrap_or(0)
	));
	Ok(ProfileFile { path })
}
