//! Landlock LSM self-restriction (Linux, best-effort).
//!
//! ## Layering
//!
//! - **Direct spawn**: call [`restrict_self`] in the child's `pre_exec` *after*
//!   rlimits and *before* seccomp (seccomp last freezes the syscall surface).
//! - **bwrap**: Landlock is secondary — bwrap already provides the mount cage.
//!   Optional: guest helper can still stack Landlock; we do not apply it to
//!   the bwrap process itself (would constrain setup).
//!
//! ## ABI
//!
//! Pin **V3** so `from_all` includes `Truncate` (real write isolation). Without
//! Truncate, `O_TRUNC` / `truncate(2)` can still wipe files where WriteFile is
//! denied.
//!
//! ## pre_exec note
//!
//! Building a ruleset is not strictly async-signal-safe (opens, alloc). After
//! `fork` the child is single-threaded, which is the common sandbox pattern.
//! Prefer this over restricting the multi-threaded parent indexer.

use std::path::{Path, PathBuf};

use landlock::{
	path_beneath_rules, Access, AccessFs, CompatLevel, Compatible, Ruleset, RulesetAttr,
	RulesetCreatedAttr, RulesetStatus, ABI,
};

use crate::error::SandboxError;
use crate::spec::Mounts;

/// Outcome of attempting Landlock restriction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LandlockStatus {
	/// Fully enforced at the requested ABI.
	FullyEnforced,
	/// Partially enforced (older kernel / missing features).
	PartiallyEnforced,
	/// Kernel has no Landlock — degraded; namespaces + seccomp still hold.
	NotEnforced,
}

/// Restrict the calling thread's FS access to `mounts` (RO + RW).
///
/// Intended for `CommandExt::pre_exec` on the **guest** (not bwrap).
pub fn restrict_self(mounts: &Mounts) -> Result<LandlockStatus, SandboxError> {
	// V3: Truncate is part of write isolation (kernel ≥ 6.2).
	let abi = ABI::V3;

	let mut ro: Vec<PathBuf> = mounts
		.read_only
		.iter()
		.filter(|p| p.exists())
		.cloned()
		.collect();
	let rw: Vec<PathBuf> = mounts
		.writable
		.iter()
		.filter(|p| p.exists())
		.cloned()
		.collect();

	// Essentials for dynamic linker + tmp; only if present (BestEffort skips
	// missing via path_beneath_rules, but we filter first for clarity).
	for essential in [
		"/usr", "/bin", "/lib", "/lib64", "/etc", "/dev", "/proc", "/tmp", "/nix", "/nix/store",
	] {
		let p = Path::new(essential);
		if p.exists() {
			ro.push(p.to_path_buf());
		}
	}

	// Floor: BestEffort so older kernels degrade rather than abort the parse.
	// CI can assert FullyEnforced on a known-good host.
	let status = Ruleset::default()
		.set_compatibility(CompatLevel::BestEffort)
		.handle_access(AccessFs::from_all(abi))
		.map_err(|e| SandboxError::Backend(format!("landlock handle_access: {e}")))?
		// Opportunistic newer bits (IoctlDev, …) when available.
		.handle_access(AccessFs::from_all(ABI::V5))
		.map_err(|e| SandboxError::Backend(format!("landlock handle_access V5: {e}")))?
		.create()
		.map_err(|e| SandboxError::Backend(format!("landlock create: {e}")))?
		.add_rules(path_beneath_rules(&ro, AccessFs::from_read(abi)))
		.map_err(|e| SandboxError::Backend(format!("landlock ro rules: {e}")))?
		.add_rules(path_beneath_rules(&rw, AccessFs::from_all(abi)))
		.map_err(|e| SandboxError::Backend(format!("landlock rw rules: {e}")))?
		.restrict_self()
		.map_err(|e| SandboxError::Backend(format!("landlock restrict_self: {e}")))?;

	let out = match status.ruleset {
		RulesetStatus::FullyEnforced => LandlockStatus::FullyEnforced,
		RulesetStatus::PartiallyEnforced => LandlockStatus::PartiallyEnforced,
		RulesetStatus::NotEnforced => LandlockStatus::NotEnforced,
	};
	if out != LandlockStatus::FullyEnforced {
		// pre_exec: no tracing; parent logs via degraded status if wired.
	}
	Ok(out)
}
