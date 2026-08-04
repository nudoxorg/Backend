//! The query routing table (09-vector §20.5): scope × quality mode ×
//! connectivity → which dense targets run, whether they hedge, what got
//! omitted, and whether a rerank stage follows.
//!
//! Pure decision logic — the router *executes* a [`RoutePlan`]; this module
//! only derives it, so every row of the table is unit-testable.

use heart::PackageId;

/// How many stage-1 candidates feed the Deep rerank stage (09-vector §20.5).
pub const DEEP_STAGE1_TOP_K: usize = 100;

/// What corpus the query ranges over.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum QueryScope {
	/// The user's own project shard only.
	Project,
	/// The project's dependency closure.
	Deps,
	/// The whole org/global index.
	Org,
}

/// The quality tier the user (or config) asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum QualityMode {
	/// Local-only; never touches the network.
	Local,
	/// Local + remote parity collection (same jina model both planes).
	Parity,
	/// Remote premium collection (voyage). Requires online.
	Premium,
	/// Premium/parity stage-1 top-[`DEEP_STAGE1_TOP_K`] + rerank stage.
	/// Requires online.
	Deep,
}

/// Everything the routing decision reads. `hot` answers "is this dep's shard
/// resident locally?" (the §20.4 admission outcome); cold deps are derived as
/// its complement over `deps`.
pub struct RouteInputs<'a> {
	pub scope: QueryScope,
	pub mode: QualityMode,
	/// Network reachability of the remote index.
	pub online: bool,
	/// The premium (voyage) collection is available to this user/org. When
	/// false, `Premium` degrades to `Parity` and `Deep` runs its stage-1 on
	/// the parity collection with the self-host reranker (09-vector §20.5:
	/// "Premium/parity Stage-1 top-100 → server rerank").
	pub premium_enabled: bool,
	/// The project's own shard exists and is queryable.
	pub project_ready: bool,
	/// The dependency closure in scope (empty for [`QueryScope::Project`]).
	pub deps: &'a [PackageId],
	/// Hot-set membership (locally admitted dep shards).
	pub hot: &'a dyn Fn(&PackageId) -> bool,
}

/// One dense stage-1 target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DenseTarget {
	/// The local project shard.
	LocalProject,
	/// One locally-resident dep shard.
	LocalDepShard(PackageId),
	/// The remote parity collection, optionally filtered to a dep set
	/// (`None` = the whole index, for org scope).
	RemoteParity { dep_filter: Option<Vec<PackageId>> },
	/// The remote premium (voyage) collection.
	RemotePremium,
}

/// The rerank backend for the Deep stage (the CC-BY-NC jina rerankers are
/// banned — I15 — these are the replacements: mxbai / Voyage cross-encoders).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RerankTier {
	/// Self-hosted cross-encoder (mxbai).
	SelfHost,
	/// Premium API cross-encoder (Voyage).
	Premium,
}

/// The post-fusion rerank stage (Deep mode only).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stage2 {
	pub rerank: RerankTier,
}

/// User-visible annotations about what the plan could *not* do — degraded
/// results are labeled, never silent (I14).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteLabel {
	/// Cold deps were skipped because remote search was unavailable (I14).
	ColdDepsOmitted,
	/// The project shard is not built yet; project results are missing.
	ProjectShardPending,
	/// Premium/Deep was requested offline and was downgraded (I14).
	RemoteQualityUnavailableOffline,
	/// Org scope needs the remote index, which was unreachable/forbidden.
	OrgScopeRequiresRemote,
}

/// The routing decision: run every `dense` target, fuse (RRF), then `stage2`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct RoutePlan {
	pub dense: Vec<DenseTarget>,
	/// True when local and remote targets race concurrently (latency hedge).
	pub hedged: bool,
	/// Cold deps not covered by any target — surfaced to the user (I14).
	pub omitted_cold: Vec<PackageId>,
	pub stage2: Option<Stage2>,
	pub labels: Vec<RouteLabel>,
}

/// Derive the plan for one query (the §20.5 table).
///
/// Row summary:
/// - Project → `LocalProject` (labeled pending if the shard isn't built).
/// - Deps: hot dep → its `LocalDepShard`; cold dep → `RemoteParity` filtered
///   to the cold set when remote is allowed+reachable, else *omitted and
///   labeled* (I14).
/// - Org → remote only (`RemoteParity`/`RemotePremium` by mode).
/// - Premium/Deep require online; offline they degrade to local behavior
///   with [`RouteLabel::RemoteQualityUnavailableOffline`] (I14).
/// - Deep = premium stage-1 (top [`DEEP_STAGE1_TOP_K`]) + Stage2 rerank.
/// - `hedged` whenever local and remote targets coexist.
pub fn plan_route(inputs: &RouteInputs<'_>) -> RoutePlan {
	let mut plan = RoutePlan::default();

	// Premium/Deep need the network; degrade rather than fail (I14: labeled).
	let effective_mode = match inputs.mode {
		QualityMode::Premium | QualityMode::Deep if !inputs.online => {
			plan.labels.push(RouteLabel::RemoteQualityUnavailableOffline);
			QualityMode::Local
		}
		mode => mode,
	};
	let remote_allowed = inputs.online && effective_mode != QualityMode::Local;
	// Premium stage-1 requires the premium collection; without it, Premium
	// degrades to parity and Deep runs its stage-1 on parity (§20.5).
	let premium_tier = inputs.premium_enabled
		&& matches!(effective_mode, QualityMode::Premium | QualityMode::Deep);

	// ── Local targets ────────────────────────────────────────────────────
	if inputs.scope != QueryScope::Org {
		if inputs.project_ready {
			plan.dense.push(DenseTarget::LocalProject);
		} else {
			plan.labels.push(RouteLabel::ProjectShardPending);
		}
	}
	if inputs.scope == QueryScope::Deps {
		let (hot, cold): (Vec<_>, Vec<_>) =
			inputs.deps.iter().copied().partition(|pkg| (inputs.hot)(pkg));
		plan.dense.extend(hot.into_iter().map(DenseTarget::LocalDepShard));

		// Cold deps: remote premium spans the whole index (covers them),
		// parity covers them via a dep filter; otherwise omitted + labeled.
		if !cold.is_empty() && !premium_tier {
			if remote_allowed {
				plan.dense.push(DenseTarget::RemoteParity { dep_filter: Some(cold) });
			} else {
				plan.omitted_cold = cold;
				plan.labels.push(RouteLabel::ColdDepsOmitted);
			}
		}
	}

	// ── Remote targets ───────────────────────────────────────────────────
	if inputs.scope == QueryScope::Org {
		if remote_allowed && premium_tier {
			plan.dense.push(DenseTarget::RemotePremium);
		} else if remote_allowed {
			plan.dense.push(DenseTarget::RemoteParity { dep_filter: None });
		} else {
			plan.labels.push(RouteLabel::OrgScopeRequiresRemote);
		}
	} else if remote_allowed && premium_tier {
		plan.dense.push(DenseTarget::RemotePremium);
	} else if remote_allowed
		&& effective_mode == QualityMode::Deep
		&& inputs.scope == QueryScope::Project
	{
		// Deep-on-parity needs remote stage-1 breadth even at project scope:
		// the rerank stage reads (query, candidate) pairs served by INDEX.
		plan.dense.push(DenseTarget::RemoteParity { dep_filter: None });
	}

	// ── Stage 2 (Deep only, and only when it survived the online gate) ───
	// Tier follows the stage-1 collection: premium → Voyage rerank-2.5;
	// parity → self-host cross-encoder (mxbai, Apache-2.0 — I15).
	if effective_mode == QualityMode::Deep {
		let rerank =
			if premium_tier { RerankTier::Premium } else { RerankTier::SelfHost };
		plan.stage2 = Some(Stage2 { rerank });
	}

	plan.hedged = plan.dense.iter().any(|t| {
		matches!(t, DenseTarget::LocalProject | DenseTarget::LocalDepShard(_))
	}) && plan.dense.iter().any(|t| {
		matches!(t, DenseTarget::RemoteParity { .. } | DenseTarget::RemotePremium)
	});

	plan
}

#[cfg(test)]
mod tests {
	use super::*;
	use super::super::store::NAMESPACE_NUDOX;

	fn pkg(name: &str) -> PackageId { PackageId::from_name(&NAMESPACE_NUDOX, name.as_bytes()) }

	struct Case<'a> {
		scope: QueryScope,
		mode: QualityMode,
		online: bool,
		project_ready: bool,
		deps: &'a [PackageId],
		hot: &'a dyn Fn(&PackageId) -> bool,
	}

	fn route(case: Case<'_>) -> RoutePlan {
		plan_route(&RouteInputs {
			scope: case.scope,
			mode: case.mode,
			online: case.online,
			premium_enabled: true,
			project_ready: case.project_ready,
			deps: case.deps,
			hot: case.hot,
		})
	}

	/// Same as [`route`] but without a premium subscription.
	fn route_no_premium(case: Case<'_>) -> RoutePlan {
		plan_route(&RouteInputs {
			scope: case.scope,
			mode: case.mode,
			online: case.online,
			premium_enabled: false,
			project_ready: case.project_ready,
			deps: case.deps,
			hot: case.hot,
		})
	}

	const ALL_HOT: &dyn Fn(&PackageId) -> bool = &|_| true;
	const ALL_COLD: &dyn Fn(&PackageId) -> bool = &|_| false;

	// Row: project scope, local mode → LocalProject only.
	#[test]
	fn row_project_local() {
		let plan = route(Case {
			scope: QueryScope::Project,
			mode: QualityMode::Local,
			online: true,
			project_ready: true,
			deps: &[],
			hot: ALL_HOT,
		});
		assert_eq!(plan.dense, vec![DenseTarget::LocalProject]);
		assert!(!plan.hedged && plan.stage2.is_none() && plan.labels.is_empty());
	}

	// Row: project shard not built → labeled pending, nothing silently wrong.
	#[test]
	fn row_project_pending_is_labeled() {
		let plan = route(Case {
			scope: QueryScope::Project,
			mode: QualityMode::Local,
			online: false,
			project_ready: false,
			deps: &[],
			hot: ALL_HOT,
		});
		assert!(plan.dense.is_empty());
		assert_eq!(plan.labels, vec![RouteLabel::ProjectShardPending]);
	}

	// Row: deps scope, dep in hot set → its local shard is queried.
	#[test]
	fn row_deps_hot_local_shard() {
		let deps = [pkg("serde")];
		let plan = route(Case {
			scope: QueryScope::Deps,
			mode: QualityMode::Local,
			online: false,
			project_ready: true,
			deps: &deps,
			hot: ALL_HOT,
		});
		assert_eq!(
			plan.dense,
			vec![DenseTarget::LocalProject, DenseTarget::LocalDepShard(pkg("serde"))]
		);
		assert!(plan.omitted_cold.is_empty() && plan.labels.is_empty());
	}

	// Row: deps scope, cold dep, online parity → remote parity with dep filter.
	#[test]
	fn row_deps_cold_online_parity_filter() {
		let deps = [pkg("tokio")];
		let plan = route(Case {
			scope: QueryScope::Deps,
			mode: QualityMode::Parity,
			online: true,
			project_ready: true,
			deps: &deps,
			hot: ALL_COLD,
		});
		assert!(plan.dense.contains(&DenseTarget::RemoteParity {
			dep_filter: Some(vec![pkg("tokio")]),
		}));
		assert!(plan.hedged, "local project + remote parity race");
		assert!(plan.omitted_cold.is_empty());
	}

	// Row (I14): deps scope, cold dep, offline → omitted and labeled.
	#[test]
	fn row_deps_cold_offline_omitted_and_labeled() {
		let deps = [pkg("tokio"), pkg("serde")];
		let hot = |p: &PackageId| *p == pkg("serde");
		let plan = route(Case {
			scope: QueryScope::Deps,
			mode: QualityMode::Parity,
			online: false,
			project_ready: true,
			deps: &deps,
			hot: &hot,
		});
		assert_eq!(
			plan.dense,
			vec![DenseTarget::LocalProject, DenseTarget::LocalDepShard(pkg("serde"))]
		);
		assert_eq!(plan.omitted_cold, vec![pkg("tokio")]);
		assert!(plan.labels.contains(&RouteLabel::ColdDepsOmitted));
		assert!(!plan.hedged);
	}

	// Row: local mode ignores the network even when online (cold → omitted).
	#[test]
	fn row_deps_local_mode_never_remote() {
		let deps = [pkg("tokio")];
		let plan = route(Case {
			scope: QueryScope::Deps,
			mode: QualityMode::Local,
			online: true,
			project_ready: true,
			deps: &deps,
			hot: ALL_COLD,
		});
		assert_eq!(plan.dense, vec![DenseTarget::LocalProject]);
		assert_eq!(plan.omitted_cold, vec![pkg("tokio")]);
		assert!(plan.labels.contains(&RouteLabel::ColdDepsOmitted));
	}

	// Row: org scope, parity online → whole-index remote parity, no local.
	#[test]
	fn row_org_parity() {
		let plan = route(Case {
			scope: QueryScope::Org,
			mode: QualityMode::Parity,
			online: true,
			project_ready: true,
			deps: &[],
			hot: ALL_HOT,
		});
		assert_eq!(plan.dense, vec![DenseTarget::RemoteParity { dep_filter: None }]);
		assert!(!plan.hedged);
	}

	// Row: org scope offline → labeled unavailable, empty plan.
	#[test]
	fn row_org_offline_labeled() {
		let plan = route(Case {
			scope: QueryScope::Org,
			mode: QualityMode::Parity,
			online: false,
			project_ready: true,
			deps: &[],
			hot: ALL_HOT,
		});
		assert!(plan.dense.is_empty());
		assert!(plan.labels.contains(&RouteLabel::OrgScopeRequiresRemote));
	}

	// Row: premium online → RemotePremium hedged with local; premium spans
	// cold deps (no separate parity target).
	#[test]
	fn row_premium_online() {
		let deps = [pkg("tokio")];
		let plan = route(Case {
			scope: QueryScope::Deps,
			mode: QualityMode::Premium,
			online: true,
			project_ready: true,
			deps: &deps,
			hot: ALL_COLD,
		});
		assert!(plan.dense.contains(&DenseTarget::RemotePremium));
		assert!(!plan.dense.iter().any(|t| matches!(t, DenseTarget::RemoteParity { .. })));
		assert!(plan.omitted_cold.is_empty(), "premium covers cold deps");
		assert!(plan.hedged);
		assert!(plan.stage2.is_none());
	}

	// Row (I14): premium offline → downgraded to local behavior + label.
	#[test]
	fn row_premium_offline_downgrades_labeled() {
		let deps = [pkg("tokio")];
		let plan = route(Case {
			scope: QueryScope::Deps,
			mode: QualityMode::Premium,
			online: false,
			project_ready: true,
			deps: &deps,
			hot: ALL_COLD,
		});
		assert_eq!(plan.dense, vec![DenseTarget::LocalProject]);
		assert!(plan.labels.contains(&RouteLabel::RemoteQualityUnavailableOffline));
		assert!(plan.labels.contains(&RouteLabel::ColdDepsOmitted));
		assert_eq!(plan.omitted_cold, vec![pkg("tokio")]);
		assert!(plan.stage2.is_none(), "no rerank without stage-1 breadth");
	}

	// Row: deep online → premium stage-1 + Stage2 premium rerank.
	#[test]
	fn row_deep_online_two_stage() {
		let plan = route(Case {
			scope: QueryScope::Org,
			mode: QualityMode::Deep,
			online: true,
			project_ready: false,
			deps: &[],
			hot: ALL_HOT,
		});
		assert_eq!(plan.dense, vec![DenseTarget::RemotePremium]);
		assert_eq!(plan.stage2, Some(Stage2 { rerank: RerankTier::Premium }));
	}

	// Row (§20.5): deep without a premium subscription → parity stage-1 +
	// self-host cross-encoder rerank (mxbai — I15-clean), never Voyage.
	#[test]
	fn row_deep_on_parity_selects_self_host_rerank() {
		let plan = route_no_premium(Case {
			scope: QueryScope::Org,
			mode: QualityMode::Deep,
			online: true,
			project_ready: false,
			deps: &[],
			hot: ALL_HOT,
		});
		assert_eq!(plan.dense, vec![DenseTarget::RemoteParity { dep_filter: None }]);
		assert_eq!(plan.stage2, Some(Stage2 { rerank: RerankTier::SelfHost }));
	}

	// Row (§20.5): deep-on-parity at project scope still adds remote parity
	// breadth — the rerank stage needs INDEX-served stage-1 candidates.
	#[test]
	fn row_deep_on_parity_project_scope_hedges_remote() {
		let plan = route_no_premium(Case {
			scope: QueryScope::Project,
			mode: QualityMode::Deep,
			online: true,
			project_ready: true,
			deps: &[],
			hot: ALL_HOT,
		});
		assert!(plan.dense.contains(&DenseTarget::LocalProject));
		assert!(plan.dense.contains(&DenseTarget::RemoteParity { dep_filter: None }));
		assert!(plan.hedged);
		assert_eq!(plan.stage2, Some(Stage2 { rerank: RerankTier::SelfHost }));
	}

	// Row: premium mode without subscription degrades to parity stage-1.
	#[test]
	fn row_premium_without_subscription_uses_parity() {
		let deps = [pkg("tokio")];
		let plan = route_no_premium(Case {
			scope: QueryScope::Deps,
			mode: QualityMode::Premium,
			online: true,
			project_ready: true,
			deps: &deps,
			hot: ALL_COLD,
		});
		assert!(!plan.dense.contains(&DenseTarget::RemotePremium));
		assert!(plan.dense.contains(&DenseTarget::RemoteParity {
			dep_filter: Some(vec![pkg("tokio")]),
		}));
	}

	// ── adversarial: exhaustive cartesian sweep ──────────────────────────────

	/// For every (scope × mode × online × premium_enabled × project_ready)
	/// combination with a 2-dep set (one hot, one cold), assert three global
	/// invariants on every cell:
	///   (a) offline plans contain no remote target.
	///   (b) any plan with omitted_cold has the ColdDepsOmitted label (I14).
	///   (c) stage2 is present iff effective_mode == Deep survived the online gate.
	///
	/// Also asserts exact dense target counts for known cell families.
	#[test]
	fn exhaustive_cartesian_routing_invariants() {
		let hot_dep = pkg("hot");
		let cold_dep = pkg("cold");
		let deps = [hot_dep, cold_dep];
		let is_hot = |p: &PackageId| *p == hot_dep;

		let all_scopes = [QueryScope::Project, QueryScope::Deps, QueryScope::Org];
		let all_modes = [QualityMode::Local, QualityMode::Parity, QualityMode::Premium, QualityMode::Deep];
		let bools = [false, true];

		let mut cell_count = 0usize;

		for &scope in &all_scopes {
			for &mode in &all_modes {
				for &online in &bools {
					for &premium_enabled in &bools {
						for &project_ready in &bools {
							let plan = plan_route(&RouteInputs {
								scope,
								mode,
								online,
								premium_enabled,
								project_ready,
								deps: if scope == QueryScope::Deps { &deps } else { &[] },
								hot: &is_hot,
							});

							let cell_desc = format!(
								"scope={:?} mode={:?} online={} premium={} ready={}",
								scope, mode, online, premium_enabled, project_ready
							);

							// (a) Offline plans have no remote targets.
							if !online {
								let has_remote = plan.dense.iter().any(|t| matches!(
									t,
									DenseTarget::RemoteParity { .. } | DenseTarget::RemotePremium
								));
								assert!(!has_remote,
									"(a) offline must have no remote targets — {}", cell_desc);
							}

							// (b) omitted_cold → ColdDepsOmitted label.
							if !plan.omitted_cold.is_empty() {
								assert!(plan.labels.contains(&RouteLabel::ColdDepsOmitted),
									"(b) omitted_cold without ColdDepsOmitted label — {}", cell_desc);
							}
							// Contrapositive: ColdDepsOmitted label → omitted_cold non-empty.
							if plan.labels.contains(&RouteLabel::ColdDepsOmitted) {
								assert!(!plan.omitted_cold.is_empty(),
									"(b) ColdDepsOmitted label without any omitted cold deps — {}", cell_desc);
							}

							// (c) stage2 present iff effective Deep survived the online gate.
							let effective_is_deep = online && mode == QualityMode::Deep;
							if effective_is_deep {
								assert!(plan.stage2.is_some(),
									"(c) effective Deep must have stage2 — {}", cell_desc);
							} else {
								assert!(plan.stage2.is_none(),
									"(c) non-Deep effective mode must not have stage2 — {}", cell_desc);
							}

							cell_count += 1;
						}
					}
				}
			}
		}

		// 3 scopes × 4 modes × 2 online × 2 premium × 2 ready = 96 cells.
		assert_eq!(cell_count, 96, "must have swept all 96 cells");
	}

	/// Local mode, Deps scope, hot dep: exactly 2 dense targets
	/// (LocalProject + LocalDepShard), no remote.
	#[test]
	fn deps_local_mode_all_hot_exact_dense_count() {
		let hot = pkg("serde");
		let cold = pkg("tokio");
		let deps = [hot, cold];
		let plan = plan_route(&RouteInputs {
			scope: QueryScope::Deps,
			mode: QualityMode::Local,
			online: true,
			premium_enabled: true,
			project_ready: true,
			deps: &deps,
			hot: &|p: &PackageId| *p == hot,
		});
		// LocalProject + LocalDepShard(hot) = 2; cold is omitted (local mode).
		assert_eq!(plan.dense.len(), 2, "exact: LocalProject + 1 hot shard");
		assert!(plan.dense.contains(&DenseTarget::LocalProject));
		assert!(plan.dense.contains(&DenseTarget::LocalDepShard(hot)));
		assert!(!plan.dense.iter().any(|t| matches!(t, DenseTarget::RemoteParity { .. })));
	}

	// Row (I14): deep offline → downgraded, labeled, no stage2.
	#[test]
	fn row_deep_offline_downgrades() {
		let plan = route(Case {
			scope: QueryScope::Project,
			mode: QualityMode::Deep,
			online: false,
			project_ready: true,
			deps: &[],
			hot: ALL_HOT,
		});
		assert_eq!(plan.dense, vec![DenseTarget::LocalProject]);
		assert!(plan.labels.contains(&RouteLabel::RemoteQualityUnavailableOffline));
		assert!(plan.stage2.is_none());
	}
}
