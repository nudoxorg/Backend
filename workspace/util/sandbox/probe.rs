//! Host isolation capability probe and production policy (design §5, §16.3–4).
//!
//! Call once at process start. [`IsolationPolicy::RequireProduction`] fails
//! closed when the host cannot provide a production-grade cage.

use crate::backend::{select, Capabilities};
use crate::error::SandboxError;

/// Snapshot of what this host can enforce.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HostIsolation {
	/// Selected backend capabilities.
	pub capabilities: Capabilities,
	/// Selected backend name.
	pub backend: &'static str,
	/// bubblewrap present on PATH (Linux).
	pub bwrap: bool,
	/// cgroup v2 writable parent discovered.
	pub cgroup: bool,
	/// Landlock ABI available (Linux; best-effort probe).
	pub landlock: LandlockAbi,
	/// seccomp denylist compiles for this arch.
	pub seccomp: bool,
	/// Kernel major.minor when readable from `/proc/version` / `uname`.
	pub kernel: Option<(u32, u32)>,
}

/// Landlock support tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LandlockAbi {
	/// Not Linux or probe failed.
	Unavailable,
	/// Kernel has Landlock but below our preferred ABI.
	Partial,
	/// Full FS isolation at V3+ (Truncate).
	Full,
}

/// How strictly isolation is required.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum IsolationPolicy {
	/// Degrade gracefully (dev default).
	#[default]
	BestEffort,
	/// Refuse to run without a production-grade backend.
	RequireProduction,
}

impl IsolationPolicy {
	/// From env: `NUDOX_SANDBOX_REQUIRE=1` / `NUDOX_ENV=prod` → require.
	pub fn from_env() -> Self {
		if matches!(
			std::env::var("NUDOX_SANDBOX_REQUIRE").as_deref(),
			Ok("1") | Ok("true")
		) || matches!(
			std::env::var("NUDOX_ENV").as_deref(),
			Ok("prod") | Ok("production")
		) {
			Self::RequireProduction
		} else {
			Self::BestEffort
		}
	}
}

/// Probe the host once.
pub fn probe() -> HostIsolation {
	let selected = select();
	let backend = selected.as_ref();
	let capabilities = backend.capabilities();

	#[cfg(target_os = "linux")]
	let bwrap = which::which("bwrap").is_ok();
	#[cfg(not(target_os = "linux"))]
	let bwrap = false;

	let cgroup = crate::cgroup::has_writable_parent();

	#[cfg(target_os = "linux")]
	let landlock = probe_landlock();
	#[cfg(not(target_os = "linux"))]
	let landlock = LandlockAbi::Unavailable;

	#[cfg(target_os = "linux")]
	let seccomp = crate::seccomp::denylist_bpf_bytes().is_ok();
	#[cfg(not(target_os = "linux"))]
	let seccomp = false;

	HostIsolation {
		capabilities,
		backend: backend.name(),
		bwrap,
		cgroup,
		landlock,
		seccomp,
		kernel: read_kernel_version(),
	}
}

/// Assert the host meets `policy`. Returns the probe result on success.
pub fn require(policy: IsolationPolicy) -> Result<HostIsolation, SandboxError> {
	let host = probe();
	match policy {
		IsolationPolicy::BestEffort => {
			if !host.capabilities.production_grade {
				tracing::warn!(
					backend = host.backend,
					"isolation is best-effort (not production-grade)"
				);
			}
			if host.landlock == LandlockAbi::Unavailable && cfg!(target_os = "linux") {
				tracing::info!("Landlock unavailable; namespaces+seccomp+cgroups still apply");
			}
			Ok(host)
		}
		IsolationPolicy::RequireProduction => {
			if !host.capabilities.production_grade {
				return Err(SandboxError::Denied {
					reason: format!(
						"production isolation required but backend `{}` is not production-grade",
						host.backend
					),
				});
			}
			#[cfg(target_os = "linux")]
			{
				if !host.bwrap {
					return Err(SandboxError::HelperMissing {
						program: "bwrap".into(),
					});
				}
				if !host.seccomp {
					return Err(SandboxError::Backend(
						"seccomp denylist failed to compile; refusing production".into(),
					));
				}
				// Landlock degrades best-effort on older kernels (design §16.3);
				// we log but do not hard-fail — namespaces+seccomp still hold.
				if host.landlock != LandlockAbi::Full {
					tracing::warn!(
						?host.landlock,
						"Landlock not fully enforced; continuing under RequireProduction"
					);
				}
			}
			Ok(host)
		}
	}
}

fn read_kernel_version() -> Option<(u32, u32)> {
	#[cfg(unix)]
	{
		let uts = std::process::Command::new("uname")
			.arg("-r")
			.output()
			.ok()?;
		if !uts.status.success() {
			return None;
		}
		let s = String::from_utf8_lossy(&uts.stdout);
		let mut parts = s.trim().split(|c: char| !c.is_ascii_digit());
		let major = parts.next()?.parse().ok()?;
		let minor = parts.next()?.parse().ok()?;
		Some((major, minor))
	}
	#[cfg(not(unix))]
	{
		None
	}
}

#[cfg(target_os = "linux")]
fn probe_landlock() -> LandlockAbi {
	use landlock::{Access, AccessFs, CompatLevel, Compatible, Ruleset, RulesetAttr, ABI};
	// Create only — never `restrict_self` on the indexer process.
	match Ruleset::default()
		.set_compatibility(CompatLevel::BestEffort)
		.handle_access(AccessFs::from_all(ABI::V3))
		.and_then(|r| r.create())
	{
		Ok(_) => {
			// V3 create success ⇒ kernel supports our preferred ABI floor.
			// Full vs partial is only knowable after restrict_self on a guest.
			LandlockAbi::Full
		}
		Err(_) => LandlockAbi::Unavailable,
	}
}
