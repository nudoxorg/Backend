//! Capability budgets for sealed compute.

use std::path::{Path, PathBuf};

use crate::limits::{Limits, Network};
use crate::spec::{Env, Mounts};

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

	/// Build from legacy mounts; `scratch` becomes the primary RW root.
	///
	/// Any entry in `mounts.writable` equal to `scratch` is dropped so the
	/// primary is not double-bound by [`to_mounts`](Self::to_mounts). Matches
	/// [`crate::SealedCommand::from_spec`]'s peel of the first writable.
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
