//! Capability budgets for sealed compute.

use std::num::{NonZeroU32, NonZeroU64};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::limits::{Limits, Network};
use crate::spec::{Env, Mounts};

/// Threat posture of the code a producer runs, driving *default* capability
/// policy (DAEMON-PLAN §2.3 / §5 Phase 6).
///
/// This is a policy dial, not a resource profile: [`ProducerProfile`] still
/// supplies the language-appropriate ceilings. The tier *clamps* those ceilings
/// (and picks the default network posture) so that regardless of how loose a
/// profile or per-package override is, hostile interpreter code can never be
/// granted more than the tier permits.
///
/// [`crate::ProducerProfile`]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ThreatTier {
	/// Interpreters that execute package code (nix / typescript / python).
	/// Tightest ceilings; network defaults off.
	Hostile,
	/// Compilers / oracles over untrusted source (rust / go / java).
	/// The default posture.
	#[default]
	Untrusted,
	/// Fully trusted host tooling (none today). Loosest; the profile ceilings
	/// pass through unclamped.
	Trusted,
}

impl ThreatTier {
	/// Default network posture for freshly sealed work at this tier.
	///
	/// Every tier seals with the network *off* — the acquire phase is the only
	/// place network is granted (see [`crate::seal::Job`]). This exists so the
	/// default is expressed by policy, not by an implicit call-site constant.
	pub const fn net_default(self) -> NetGrant {
		NetGrant::Off
	}

	/// Per-tier hard ceiling: the loosest limits this tier may ever be granted.
	///
	/// `None` (Trusted) means "no tier ceiling — the profile stands".
	const fn ceiling(self) -> Option<Limits> {
		match self {
			// Hostile: 2 GiB / 5 min wall / 300 cpu-s / 128 pids. An interpreter
			// running package code never gets the 6 GiB rustc profile.
			Self::Hostile => Some(Limits {
				mem_bytes: nz64(2 * 1024 * 1024 * 1024),
				cpu_secs: nz64(300),
				wall: Duration::from_secs(5 * 60),
				pids: nz32(128),
				max_stdout: 16 * 1024 * 1024,
				max_stderr: 512 * 1024,
				fsize_bytes: nz64(1024 * 1024 * 1024),
				nofile: nz64(2048),
			}),
			// Untrusted: 8 GiB / 20 min / 1200 cpu-s / 1024 pids — headroom above
			// the Rust profile so it never clamps a legitimate compile, but still
			// a fleet-wide backstop against a runaway override.
			Self::Untrusted => Some(Limits {
				mem_bytes: nz64(8 * 1024 * 1024 * 1024),
				cpu_secs: nz64(1200),
				wall: Duration::from_secs(20 * 60),
				pids: nz32(1024),
				max_stdout: 64 * 1024 * 1024,
				max_stderr: 4 * 1024 * 1024,
				fsize_bytes: nz64(4 * 1024 * 1024 * 1024),
				nofile: nz64(8192),
			}),
			Self::Trusted => None,
		}
	}

	/// Clamp resolved `limits` down to this tier's ceiling.
	///
	/// Each field is `min(resolved, ceiling)`; a tighter profile / override is
	/// left untouched, a looser one is capped. Trusted passes through.
	pub fn clamp(self, limits: Limits) -> Limits {
		let Some(cap) = self.ceiling() else {
			return limits;
		};
		Limits {
			mem_bytes: limits.mem_bytes.min(cap.mem_bytes),
			cpu_secs: limits.cpu_secs.min(cap.cpu_secs),
			wall: limits.wall.min(cap.wall),
			pids: limits.pids.min(cap.pids),
			max_stdout: limits.max_stdout.min(cap.max_stdout),
			max_stderr: limits.max_stderr.min(cap.max_stderr),
			fsize_bytes: limits.fsize_bytes.min(cap.fsize_bytes),
			nofile: limits.nofile.min(cap.nofile),
		}
	}
}

const fn nz64(v: u64) -> NonZeroU64 {
	match NonZeroU64::new(v) {
		Some(v) => v,
		None => panic!("tier ceiling must be non-zero"),
	}
}

const fn nz32(v: u32) -> NonZeroU32 {
	match NonZeroU32::new(v) {
		Some(v) => v,
		None => panic!("tier ceiling must be non-zero"),
	}
}

/// Filesystem grant: RO roots + one primary RW scratch (+ optional extra RW).
///
/// The plan calls for exactly one RW scratch; `writable` holds extra output
/// binds that producers still need this phase (cwd, json out). Prefer empty.
#[derive(Debug, Clone)]
pub struct FsGrant {
	/// Paths bind-mounted read-only (toolchains, package source, store).
	pub read_only: Vec<PathBuf>,
	/// Primary RW scratch directory (always present once sealed).
	pub scratch: PathBuf,
	/// Additional RW binds (outputs). Prefer zero.
	pub writable: Vec<PathBuf>,
}

impl FsGrant {
	/// Grant with a single scratch and no binds yet.
	pub fn scratch(scratch: impl Into<PathBuf>) -> Self {
		Self {
			read_only: Vec::new(),
			scratch: scratch.into(),
			writable: Vec::new(),
		}
	}

	/// Add a read-only bind.
	pub fn ro(mut self, path: impl Into<PathBuf>) -> Self {
		self.read_only.push(path.into());
		self
	}

	/// Add an extra writable bind (not the primary scratch).
	pub fn rw(mut self, path: impl Into<PathBuf>) -> Self {
		self.writable.push(path.into());
		self
	}

	/// Project into the legacy [`Mounts`] shape (scratch included as RW).
	pub fn to_mounts(&self) -> Mounts {
		let mut m = Mounts::new();
		for p in &self.read_only {
			m = m.ro(p);
		}
		m = m.rw(&self.scratch);
		for p in &self.writable {
			m = m.rw(p);
		}
		m
	}

	/// Build from mounts; `scratch` becomes the primary RW root.
	///
	/// Any entry in `mounts.writable` equal to `scratch` is dropped so the
	/// primary is not double-bound by [`to_mounts`](Self::to_mounts).
	pub fn from_mounts(mounts: Mounts, scratch: impl Into<PathBuf>) -> Self {
		let scratch = scratch.into();
		let writable = mounts
			.writable
			.into_iter()
			.filter(|p| p != &scratch)
			.collect();
		Self {
			read_only: mounts.read_only,
			scratch,
			writable,
		}
	}

	/// Borrow the scratch path.
	pub fn scratch_path(&self) -> &Path {
		&self.scratch
	}
}

/// Network capability inside a sealed (or acquiring) budget.
///
/// Same semantics as [`Network`]; separate name for sealed-compute vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NetGrant {
	/// Empty netns / denied outbound (sealed-phase default).
	#[default]
	Off,
	/// Network permitted (acquiring / fetch only).
	On,
}

impl From<Network> for NetGrant {
	fn from(n: Network) -> Self {
		match n {
			Network::Off => Self::Off,
			Network::On => Self::On,
		}
	}
}

impl From<NetGrant> for Network {
	fn from(n: NetGrant) -> Self {
		match n {
			NetGrant::Off => Self::Off,
			NetGrant::On => Self::On,
		}
	}
}

/// Fully resolved capability surface for one sealed run.
///
/// Once constructed, nothing ambient may be re-read: FS, net, env, and
/// resource ceilings are fixed.
#[derive(Debug, Clone)]
pub struct CapabilityBudget {
	/// Filesystem grant.
	pub fs: FsGrant,
	/// Network grant (Off in sealed phase).
	pub net: NetGrant,
	/// Explicit env allowlist (never ambient).
	pub env: Env,
	/// Resource ceilings.
	pub resources: Limits,
}

impl CapabilityBudget {
	/// Build a budget from its four surfaces.
	pub fn new(fs: FsGrant, net: NetGrant, env: Env, resources: Limits) -> Self {
		Self {
			fs,
			net,
			env,
			resources,
		}
	}
}
