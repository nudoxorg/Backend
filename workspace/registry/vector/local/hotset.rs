//! Hot-set management (09-vector §20.4): which dep shards live locally.
//!
//! [`HotSetManager`] owns the budget and the per-package signals
//! ([`PackageStats`]: direct-dep flag, reference density, decayed
//! query-hit EMA, user pin), funnels them through vector-core's frozen
//! admission algorithm ([`crate::vector::core::admit`] — the crate's single
//! admission call site), and diffs the admitted set against what is
//! actually installed into an [`InstallPlan`].
//!
//! **Cadence (frozen):** plans are computed on DepSet change and on a
//! weekly timer; [`apply_plan`] runs **only at idle** — never mid-search —
//! and evictions never interrupt an in-flight query (they only swap
//! registrations; searches hold their own `Arc`s).
//!
//! Stats persist as JSON (tmp + rename) so the EMA survives restarts; a
//! corrupt state file degrades to empty stats with a warning — the signals
//! are advisory, losing them costs recall of the *hot set*, not
//! correctness.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use heart::PackageId;
use serde::{Deserialize, Serialize};
use crate::vector::core::{
	AdmissionBudget, AdmissionOutcome, DepCandidate, HitEma, ShardSchema, StoreError, admit,
};

use super::depshard::{
	ArtifactFetcher, DepManifestEntry, InstallError, InstallOutcome, RemoteRouteReason,
};
use super::fanout::SharedWorkingSet;

/// Per-package admission signals (§20.4 score inputs), persisted.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PackageStats {
	/// Bakery-published resident-RAM estimate (admission cost).
	pub ram_estimate: u64,
	/// Direct dependency in the lockfile/DepSet (weight 3.0).
	pub is_direct: bool,
	/// Fraction of project references resolving into this package, `0..=1`
	/// (weight 2.0; from the OccurrenceSet).
	pub ref_density: f32,
	/// Decayed remote-served-query hit signal (weight 1.0, 14-day
	/// half-life).
	pub query_hit_ema: HitEma,
	/// User "keep local" pin (admitted unconditionally).
	pub pinned: bool,
}

/// The persisted stats table. Keys serialize as package UUIDs.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AdmissionState {
	pub stats: BTreeMap<PackageId, PackageStats>,
}

/// The diff between what admission wants and what is installed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallPlan {
	/// Admitted but not installed, in admission (priority) order.
	pub install: Vec<PackageId>,
	/// Installed but no longer admitted, ascending [`PackageId`] order.
	/// Executed only at idle.
	pub evict: Vec<PackageId>,
	/// Pinned shards alone exceed the dep budget (§20.4: pins win, but the
	/// overshoot is surfaced, never silent).
	pub over_budget: bool,
}

impl InstallPlan {
	/// Nothing to do — the working set already matches admission.
	pub fn is_empty(&self) -> bool {
		self.install.is_empty() && self.evict.is_empty()
	}
}

/// Budget + stats + persistence; computes [`InstallPlan`]s.
pub struct HotSetManager {
	state_path: PathBuf,
	budget: AdmissionBudget,
	state: AdmissionState,
}

impl HotSetManager {
	/// Open the manager, loading persisted stats from `state_path` when
	/// present. An unreadable/corrupt state file logs a warning and starts
	/// empty (stats are advisory signals, not source of truth).
	pub fn open(state_path: PathBuf, budget: AdmissionBudget) -> Self {
		let state = match fs::read(&state_path) {
			Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|err| {
				tracing::warn!(
					path = %state_path.display(),
					%err,
					"corrupt hot-set state file; starting with empty stats"
				);
				AdmissionState::default()
			}),
			Err(_) => AdmissionState::default(),
		};
		Self { state_path, budget, state }
	}

	/// The current stats table.
	pub fn state(&self) -> &AdmissionState {
		&self.state
	}

	/// Replace the budget (e.g. the project shard grew — its measured RSS
	/// is `budget.project_ram`).
	pub fn set_budget(&mut self, budget: AdmissionBudget) {
		self.budget = budget;
	}

	/// Upsert a package's manifest-derived signals (on DepSet resolution).
	/// Preserves the existing EMA and pin — those are runtime/user state,
	/// not manifest state.
	pub fn upsert_package(
		&mut self,
		package: PackageId,
		ram_estimate: u64,
		is_direct: bool,
		ref_density: f32,
		now_secs: u64,
	) {
		self.state
			.stats
			.entry(package)
			.and_modify(|stats| {
				stats.ram_estimate = ram_estimate;
				stats.is_direct = is_direct;
				stats.ref_density = ref_density;
			})
			.or_insert(PackageStats {
				ram_estimate,
				is_direct,
				ref_density,
				query_hit_ema: HitEma::new(now_secs),
				pinned: false,
			});
	}

	/// Drop stats for packages that left the DepSet.
	pub fn retain_packages(&mut self, depset: &BTreeSet<PackageId>) {
		self.state.stats.retain(|package, _| depset.contains(package));
	}

	/// Fold in a remote-served query whose clicked result lives in
	/// `package` (the §20.4 EMA signal). Unknown packages are ignored — the
	/// signal only matters for packages that *could* be admitted.
	pub fn record_query_hit(&mut self, package: PackageId, now_secs: u64) {
		if let Some(stats) = self.state.stats.get_mut(&package) {
			stats.query_hit_ema.update(now_secs, true);
		}
	}

	/// Set/clear the user "keep local" pin.
	pub fn set_pinned(&mut self, package: PackageId, pinned: bool) {
		if let Some(stats) = self.state.stats.get_mut(&package) {
			stats.pinned = pinned;
		}
	}

	/// Compute the plan: run frozen admission over the current stats and
	/// diff against `installed` (from
	/// [`super::fanout::WorkingSet::resident_packages`]).
	pub fn plan(&self, now_secs: u64, installed: &BTreeSet<PackageId>) -> InstallPlan {
		let candidates = self
			.state
			.stats
			.iter()
			.map(|(package, stats)| DepCandidate {
				package: *package,
				ram_estimate: stats.ram_estimate,
				is_direct: stats.is_direct,
				ref_density: stats.ref_density,
				query_hit_ema: stats.query_hit_ema.value_at(now_secs),
				pinned: stats.pinned,
			})
			.collect();
		let outcome = admit(self.budget, candidates);
		diff_plan(&outcome, installed)
	}

	/// Persist the stats as JSON via tmp + fsync + rename.
	pub fn persist(&self) -> Result<(), StoreError> {
		let bytes = serde_json::to_vec_pretty(&self.state)
			.map_err(|err| StoreError::Backend(format!("hot-set state serialize: {err}")))?;
		let tmp = self.state_path.with_extension("json.tmp");
		if let Some(parent) = self.state_path.parent() {
			fs::create_dir_all(parent)?;
		}
		{
			let mut file = fs::File::create(&tmp)?;
			file.write_all(&bytes)?;
			file.sync_all()?;
		}
		fs::rename(&tmp, &self.state_path)?;
		Ok(())
	}
}

/// Diff an admission outcome against the installed set.
///
/// `install` preserves admission order (highest priority first — pins,
/// then value density); `evict` is ascending [`PackageId`] — both fully
/// deterministic for identical inputs.
pub fn diff_plan(outcome: &AdmissionOutcome, installed: &BTreeSet<PackageId>) -> InstallPlan {
	let admitted: BTreeSet<PackageId> = outcome.admitted.iter().copied().collect();
	InstallPlan {
		install: outcome
			.admitted
			.iter()
			.copied()
			.filter(|package| !installed.contains(package))
			.collect(),
		evict: installed.iter().copied().filter(|package| !admitted.contains(package)).collect(),
		over_budget: outcome.over_budget,
	}
}

/// Execute a plan against the working set. **Idle-only** (§20.4): callers
/// invoke this from the idle scheduler, never from a search path — swaps
/// and evictions must not race a query the UI already labeled.
///
/// Evictions run first (freeing budget), then installs in plan order. Each
/// install resolves its manifest entry from `manifest`; a missing entry is
/// an [`InstallOutcome::RemoteRoute`] with
/// [`RemoteRouteReason::ArtifactMissing`], mirroring §20.3. Per-package
/// outcomes are returned so the caller can label routing honestly.
pub async fn apply_plan(
	plan: &InstallPlan,
	manifest: &BTreeMap<PackageId, DepManifestEntry>,
	fetcher: &dyn ArtifactFetcher,
	schema: &ShardSchema,
	dep_root: &Path,
	working_set: &SharedWorkingSet,
) -> Result<Vec<(PackageId, InstallOutcome)>, InstallError> {
	for package in &plan.evict {
		super::depshard::evict(*package, dep_root, working_set).await?;
	}

	let mut outcomes = Vec::with_capacity(plan.install.len());
	for package in &plan.install {
		let outcome = match manifest.get(package) {
			Some(entry) => {
				super::depshard::install(fetcher, entry, dep_root, schema, working_set).await?
			}
			None => {
				tracing::warn!(
					package = %package,
					"admitted package has no manifest entry; remote-routing"
				);
				InstallOutcome::RemoteRoute(RemoteRouteReason::ArtifactMissing)
			}
		};
		outcomes.push((*package, outcome));
	}
	Ok(outcomes)
}
