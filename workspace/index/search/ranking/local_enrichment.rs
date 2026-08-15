//! Desktop-only **LocalEnrichment** sidecar.
//!
//! Pure post-rank annotation + soft re-order + local-only injection. Never part
//! of INDEX/SERP ranking (`ranking.rs`), never on the wire: [`LocalContext`]
//! deliberately does **not** implement `Serialize` / `Deserialize`.
//!
//! # Pipeline (after core `rank_full` / `collect_ranked_hits`)
//!
//! 1. Annotate each ranked hit with `dep_relation` + `usage` labels from maps.
//! 2. Soft re-key scores with **capped** additive bonuses (used-before, direct,
//!    dev). Transitive is label-only (no score lift).
//! 3. Enforce a **max rank climb** so bonuses cannot leapfrog a much stronger
//!    head hit (e.g. exact-name / high rank-score item).
//! 4. Inject up to `max_local_inject` name-matching local-only packages.
//!
//! # Identity
//!
//! Empty [`LocalContext`] → order identical to input, labels empty / `None`.
//!
//! # Bonus hard cap
//!
//! Default total additive bonus is well below core
//! [`super::cascade::RankingConfig::exact_name_bonus`] (`10.0`). Combined with
//! [`LocalEnrichment::max_rank_climb`], a used-before package may rise a few
//! slots but cannot overtake a far-higher exact-quality head item.

use std::collections::HashMap;

use heart::{Language, PackageId};
use smol_str::SmolStr;

// ── Public types ─────────────────────────────────────────────────────────────

/// Desktop-only context. Built from the open project + local sqlite.
///
/// **Privacy:** intentionally has no `Serialize` / `Deserialize` derive — this
/// type must never appear on the INDEX wire or inside the public
/// [`heart::query::Query`] wire.
#[derive(Debug, Clone, Default)]
pub struct LocalContext {
	/// Optional open-project marker (opaque string; keep simple).
	pub project_id: Option<String>,
	/// How packages relate to the open project, if any.
	pub dep_relation: HashMap<PackageId, DepRelation>,
	/// Historical use across saved projects / installs.
	pub usage: HashMap<PackageId, UsageStat>,
	/// Forks, path deps, unpublished local packages — machine-local only.
	pub local_only: Vec<LocalOnlyPackage>,
}

/// How a registry package relates to the open project's dependency tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DepRelation {
	/// Declared direct runtime dependency.
	Direct,
	/// Declared dev-only dependency.
	Dev,
	/// Pulled in transitively.
	Transitive,
}

/// Local usage counters for a registry package.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UsageStat {
	/// Times this package was depended on / resolved in saved projects.
	pub times_depended: u32,
	/// Last-used unix timestamp, if known.
	pub last_used: Option<i64>,
}

/// A package that exists only on this machine (path dep, fork, workspace member).
///
/// Never assigned a registry [`PackageId`] that INDEX owns; identity is
/// `local_id` only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalOnlyPackage {
	pub local_id: String,
	pub ecosystem: Language,
	pub name: String,
	pub description: Option<String>,
	pub keywords: Vec<SmolStr>,
	/// Optional link to an upstream registry package this forks/overlays.
	pub upstream: Option<PackageId>,
}

/// One already-ranked core SERP hit (post-INDEX / post-`rank_full`).
///
/// Minimal surface so the sidecar stays decoupled from `GlobalPackage` hydration.
#[derive(Debug, Clone, PartialEq)]
pub struct RankedItem {
	pub id: PackageId,
	pub name: String,
	/// Rank / fused score from the core pipeline (higher = better).
	pub score: f32,
}

/// Where an enriched row originated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HitSource {
	/// Came from the registry SERP (INDEX or local replica).
	Registry,
	/// Injected from machine-local docs only.
	LocalOnly,
}

/// UI / client labels produced solely by local enrichment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LocalLabel {
	/// Appears in the local usage map (`times_depended > 0` or any entry).
	UsedBefore,
	/// Direct dependency of the open project.
	DirectDep,
	/// Dev dependency of the open project.
	DevDep,
	/// Transitive dependency of the open project.
	TransitiveDep,
	/// Local fork (has an upstream registry package).
	LocalFork,
	/// Path / unpublished local package (no upstream).
	PathDep,
}

/// A SERP row after desktop enrichment.
#[derive(Debug, Clone, PartialEq)]
pub struct EnrichedHit {
	/// Registry package id when [`HitSource::Registry`]; `None` for pure local rows.
	pub package_id: Option<PackageId>,
	pub name: String,
	/// Score after soft local re-key (registry) or a synthetic inject score (local).
	pub score: f32,
	pub source: HitSource,
	pub dep_relation: Option<DepRelation>,
	pub usage: Option<UsageStat>,
	pub labels: Vec<LocalLabel>,
	/// Populated when [`HitSource::LocalOnly`].
	pub local_only: Option<LocalOnlyPackage>,
}

/// Desktop-only enrichment knobs. Defaults are intentionally small.
#[derive(Debug, Clone, PartialEq)]
pub struct LocalEnrichment {
	/// Soft additive bonus when the package appears in usage history.
	pub used_before_bonus: f32,
	/// Soft additive bonus for a direct project dependency.
	pub direct_dep_bonus: f32,
	/// Soft additive bonus for a dev dependency (smaller than direct).
	pub dev_dep_bonus: f32,
	/// Max local-only rows injected into a single page.
	pub max_local_inject: usize,
	/// Hard cap on how many positions a single registry hit may climb after
	/// bonuses. Combined with [`Self::max_additive_bonus`] so used-before
	/// packages cannot leapfrog a much stronger exact-quality head item.
	pub max_rank_climb: usize,
	/// Hard cap on the **sum** of additive bonuses applied to one hit.
	///
	/// Must stay well below core `RankingConfig::exact_name_bonus` (`10.0`) so
	/// local personalization cannot invert a decisive exact-name advantage when
	/// rank scores are on a fused/raw scale. On unit-spaced ordinal rank scores
	/// (`rank_score` in `search::mod`), this alone allows at most one-slot
	/// moves; [`Self::max_rank_climb`] is the position-level guard for denser
	/// score scales.
	pub max_additive_bonus: f32,
}

impl Default for LocalEnrichment {
	fn default() -> Self {
		Self {
			used_before_bonus: 0.5,
			direct_dep_bonus: 0.35,
			dev_dep_bonus: 0.15,
			max_local_inject: 3,
			max_rank_climb: 3,
			// << ranking exact_name_bonus (10.0)
			max_additive_bonus: 0.9,
		}
	}
}

// ── Implementation ───────────────────────────────────────────────────────────

impl LocalEnrichment {
	/// Annotate, soft re-key (capped), and inject local-only name matches.
	///
	/// Pure and deterministic: no I/O, no network, no clocks. Reads only `ctx`
	/// maps/vecs. Empty `ctx` is identity on order with empty labels / `None`
	/// annotations.
	pub fn apply(
		&self,
		ranked: &[RankedItem],
		query: &str,
		ctx: &LocalContext,
	) -> Vec<EnrichedHit> {
		let page_limit = ranked.len();

		// 1–2. Annotate + soft bonuses (capped).
		let working: Vec<Working> = ranked
			.iter()
			.enumerate()
			.map(|(original_idx, item)| {
				let dep_relation = ctx.dep_relation.get(&item.id).copied();
				let usage = ctx.usage.get(&item.id).copied();
				let (bonus, labels) = self.bonus_and_labels(dep_relation, usage);
				let boosted = item.score + bonus;
				Working {
					original_idx,
					item: item.clone(),
					dep_relation,
					usage,
					labels,
					boosted,
				}
			})
			.collect();

		// 3. Soft re-order with max rank-climb enforcement.
		let reordered = reorder_with_climb_cap(working, self.max_rank_climb);

		let mut registry_hits: Vec<EnrichedHit> = reordered
			.into_iter()
			.map(|w| EnrichedHit {
				package_id: Some(w.item.id),
				name: w.item.name,
				score: w.boosted,
				source: HitSource::Registry,
				dep_relation: w.dep_relation,
				usage: w.usage,
				labels: w.labels,
				local_only: None,
			})
			.collect();

		// 4. Inject local-only name matches (up to max_local_inject).
		let local_hits = self.matching_local_only(query, ctx);
		if local_hits.is_empty() {
			return registry_hits;
		}

		// Prefer keeping all injected locals near the top; drop lowest registry
		// hits so the page stays at the original limit when the SERP was non-empty.
		let inject_n = local_hits.len();
		let mut out = local_hits;
		out.append(&mut registry_hits);

		if page_limit == 0 {
			// Empty SERP: still surface matching locals (desktop-only discovery).
			out.truncate(inject_n);
		} else {
			// Same page size as input: locals take prefix slots, bottom registry drops.
			out.truncate(page_limit);
		}

		out
	}

	fn bonus_and_labels(
		&self,
		dep_relation: Option<DepRelation>,
		usage: Option<UsageStat>,
	) -> (f32, Vec<LocalLabel>) {
		let mut labels = Vec::new();
		let mut bonus = 0.0_f32;

		if usage.is_some() {
			labels.push(LocalLabel::UsedBefore);
			bonus += self.used_before_bonus;
		}

		match dep_relation {
			Some(DepRelation::Direct) => {
				labels.push(LocalLabel::DirectDep);
				bonus += self.direct_dep_bonus;
			}
			Some(DepRelation::Dev) => {
				labels.push(LocalLabel::DevDep);
				bonus += self.dev_dep_bonus;
			}
			Some(DepRelation::Transitive) => {
				// Label only — transitive must not score like a direct dep.
				labels.push(LocalLabel::TransitiveDep);
			}
			None => {}
		}

		if bonus > self.max_additive_bonus {
			bonus = self.max_additive_bonus;
		}
		(bonus, labels)
	}

	fn matching_local_only(&self, query: &str, ctx: &LocalContext) -> Vec<EnrichedHit> {
		let q = query.trim();
		if q.is_empty() || self.max_local_inject == 0 {
			return Vec::new();
		}

		let mut matched: Vec<&LocalOnlyPackage> = ctx
			.local_only
			.iter()
			.filter(|pkg| local_name_matches(pkg, q))
			.collect();

		// Prefer exact name matches, then shorter names, then stable local_id.
		matched.sort_by(|a, b| {
			let a_exact = a.name.eq_ignore_ascii_case(q);
			let b_exact = b.name.eq_ignore_ascii_case(q);
			b_exact
				.cmp(&a_exact)
				.then_with(|| a.name.len().cmp(&b.name.len()))
				.then_with(|| a.local_id.cmp(&b.local_id))
		});

		matched.truncate(self.max_local_inject);

		matched
			.into_iter()
			.map(|pkg| {
				let label = if pkg.upstream.is_some() {
					LocalLabel::LocalFork
				} else {
					LocalLabel::PathDep
				};
				// Synthetic score: sit above typical registry rank stamps so UI
				// can still sort if needed; source chrome is the real signal.
				EnrichedHit {
					package_id: None,
					name: pkg.name.clone(),
					score: f32::MAX / 4.0,
					source: HitSource::LocalOnly,
					dep_relation: None,
					usage: None,
					labels: vec![label],
					local_only: Some(pkg.clone()),
				}
			})
			.collect()
	}
}

// ── Internals ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct Working {
	original_idx: usize,
	item: RankedItem,
	dep_relation: Option<DepRelation>,
	usage: Option<UsageStat>,
	labels: Vec<LocalLabel>,
	boosted: f32,
}

/// Place items best-first under the constraint that an item originally at
/// index `i` may not land earlier than `i - max_rank_climb`.
///
/// When all bonuses are zero this recovers the input order for strictly
/// descending scores (and preserves relative order via `original_idx` ties).
fn reorder_with_climb_cap(items: Vec<Working>, max_rank_climb: usize) -> Vec<Working> {
	let n = items.len();
	if n == 0 {
		return items;
	}

	let mut remaining = items;
	let mut out = Vec::with_capacity(n);

	for pos in 0..n {
		// Eligible = not yet placed and allowed at `pos` under the climb cap.
		// Every remaining item becomes eligible eventually as `pos` grows, so the
		// filter is non-empty whenever `remaining` is.
		let best_idx = remaining
			.iter()
			.enumerate()
			.filter(|(_, w)| w.original_idx.saturating_sub(max_rank_climb) <= pos)
			.max_by(|(_, a), (_, b)| cmp_working(a, b))
			.map(|(i, _)| i);

		let best_idx = if let Some(i) = best_idx {
			i
		} else {
			if remaining.is_empty() {
				break;
			}
			0
		};
		out.push(remaining.remove(best_idx));
	}

	out
}

/// Prefer higher boosted score; on ties prefer earlier original rank (identity
/// when bonuses are zero), then lower package id for full determinism.
fn cmp_working(a: &Working, b: &Working) -> std::cmp::Ordering {
	a.boosted
		.partial_cmp(&b.boosted)
		.unwrap_or(std::cmp::Ordering::Equal)
		.then_with(|| b.original_idx.cmp(&a.original_idx)) // smaller original_idx wins under max_by
		.then_with(|| b.item.id.cmp(&a.item.id))
}

fn local_name_matches(pkg: &LocalOnlyPackage, query: &str) -> bool {
	let q = query.to_ascii_lowercase();
	let name = pkg.name.to_ascii_lowercase();
	if name == q || name.contains(&q) {
		return true;
	}
	pkg.keywords.iter().any(|k| k.eq_ignore_ascii_case(query))
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
	use super::*;
	use uuid::Uuid;

	fn pid(n: u128) -> PackageId {
		PackageId::from_uuid(Uuid::from_u128(n))
	}

	fn item(n: u128, name: &str, score: f32) -> RankedItem {
		RankedItem {
			id: pid(n),
			name: name.to_string(),
			score,
		}
	}

	fn ids(hits: &[EnrichedHit]) -> Vec<Option<PackageId>> {
		hits.iter().map(|h| h.package_id).collect()
	}

	fn registry_ids(hits: &[EnrichedHit]) -> Vec<PackageId> {
		hits.iter().filter_map(|h| h.package_id).collect()
	}

	/// 1. Empty LocalContext → order identical, labels empty / None.
	#[test]
	fn empty_context_is_identity() {
		let ranked = vec![
			item(1, "alpha", 10.0),
			item(2, "beta", 9.0),
			item(3, "gamma", 8.0),
			item(4, "delta", 7.0),
		];
		let enrich = LocalEnrichment::default();
		let out = enrich.apply(&ranked, "alpha", &LocalContext::default());

		assert_eq!(out.len(), ranked.len());
		for (i, hit) in out.iter().enumerate() {
			assert_eq!(hit.package_id, Some(ranked[i].id));
			assert_eq!(hit.name, ranked[i].name);
			assert_eq!(hit.score.to_bits(), ranked[i].score.to_bits());
			assert_eq!(hit.source, HitSource::Registry);
			assert!(hit.dep_relation.is_none());
			assert!(hit.usage.is_none());
			assert!(hit.labels.is_empty());
			assert!(hit.local_only.is_none());
		}
	}

	/// 2. Used-before can rise a few slots but cannot beat a much higher
	///    exact-quality head item when bonuses are capped.
	#[test]
	fn used_before_climbs_but_cannot_leapfrog_top_exact() {
		// Dense scores just below a decisive head: default max_additive_bonus
		// (0.9) lifts the used package past mid-pack peers but not past head
		// (10.0). max_rank_climb further bounds position even if bonuses were wild.
		let ranked = vec![
			item(1, "serde", 10.0), // exact high-quality head
			item(2, "serde-json", 9.1),
			item(3, "serde-yaml", 9.05),
			item(4, "serde-bytes", 9.02),
			item(5, "other", 9.01),
			item(6, "used-pkg", 9.0), // used before — may climb a little
		];

		let mut ctx = LocalContext::default();
		ctx.usage.insert(
			pid(6),
			UsageStat {
				times_depended: 12,
				last_used: Some(1_700_000_000),
			},
		);

		let enrich = LocalEnrichment::default();
		let out = enrich.apply(&ranked, "serde", &ctx);

		assert_eq!(
			out[0].package_id,
			Some(pid(1)),
			"used-before must not leapfrog top exact-quality hit"
		);
		assert!(
			out[0].labels.is_empty(),
			"top exact hit has no local usage in this fixture"
		);

		let used_pos = out
			.iter()
			.position(|h| h.package_id == Some(pid(6)))
			.expect("used package still present");
		let original_pos = 5usize;
		assert!(
			used_pos < original_pos,
			"used-before should rise at least one slot when climb allows; pos={used_pos}"
		);
		assert!(
			original_pos - used_pos <= enrich.max_rank_climb,
			"climb must respect max_rank_climb"
		);
		assert!(out[used_pos].labels.contains(&LocalLabel::UsedBefore));

		// Adversarial: even with huge configured bonuses, hard caps hold.
		let wild = LocalEnrichment {
			used_before_bonus: 100.0,
			direct_dep_bonus: 100.0,
			max_additive_bonus: 0.9, // still capped under exact_name scale
			max_rank_climb: 2,
			..Default::default()
		};
		let out_wild = wild.apply(&ranked, "serde", &ctx);
		assert_eq!(
			out_wild[0].package_id,
			Some(pid(1)),
			"capped bonus must not invert head even if raw knobs are huge"
		);
		let used_pos_wild = out_wild
			.iter()
			.position(|h| h.package_id == Some(pid(6)))
			.unwrap();
		assert!(
			original_pos - used_pos_wild <= wild.max_rank_climb,
			"max_rank_climb hard-stops leapfrogging"
		);
	}

	/// 3. Local-only name match injects with LocalOnly + LocalFork/PathDep.
	#[test]
	fn local_only_matching_name_injects() {
		let ranked = vec![item(1, "serde", 10.0), item(2, "tokio", 9.0)];
		let mut ctx = LocalContext::default();
		ctx.local_only.push(LocalOnlyPackage {
			local_id: "path:my-fork".into(),
			ecosystem: Language::Rust,
			name: "my-fork".into(),
			description: Some("local path dep".into()),
			keywords: vec![],
			upstream: None,
		});
		ctx.local_only.push(LocalOnlyPackage {
			local_id: "fork:serde-local".into(),
			ecosystem: Language::Rust,
			name: "serde-local".into(),
			description: None,
			keywords: vec![],
			upstream: Some(pid(1)),
		});

		let enrich = LocalEnrichment::default();
		let out = enrich.apply(&ranked, "my-fork", &ctx);

		let local = out
			.iter()
			.find(|h| h.source == HitSource::LocalOnly)
			.expect("local-only row injected");
		assert_eq!(local.name, "my-fork");
		assert_eq!(local.package_id, None);
		assert!(local.labels.contains(&LocalLabel::PathDep));
		assert!(local.local_only.is_some());

		// Fork (with upstream) gets LocalFork when its name matches.
		let out_fork = enrich.apply(&ranked, "serde-local", &ctx);
		let fork = out_fork
			.iter()
			.find(|h| h.source == HitSource::LocalOnly)
			.expect("fork injected");
		assert!(fork.labels.contains(&LocalLabel::LocalFork));
	}

	/// 4. Local-only does not appear when name doesn't match the query.
	#[test]
	fn local_only_non_matching_name_not_injected() {
		let ranked = vec![item(1, "serde", 10.0)];
		let mut ctx = LocalContext::default();
		ctx.local_only.push(LocalOnlyPackage {
			local_id: "path:other".into(),
			ecosystem: Language::Rust,
			name: "totally-unrelated".into(),
			description: None,
			keywords: vec![SmolStr::from("zzz")],
			upstream: None,
		});

		let out = LocalEnrichment::default().apply(&ranked, "serde", &ctx);
		assert!(out.iter().all(|h| h.source == HitSource::Registry));
		assert_eq!(registry_ids(&out), vec![pid(1)]);
	}

	/// 5. max_local_inject is respected.
	#[test]
	fn max_local_inject_respected() {
		let ranked = vec![
			item(1, "a", 10.0),
			item(2, "b", 9.0),
			item(3, "c", 8.0),
			item(4, "d", 7.0),
		];
		let mut ctx = LocalContext::default();
		for i in 0..4 {
			ctx.local_only.push(LocalOnlyPackage {
				local_id: format!("local-{i}"),
				ecosystem: Language::Rust,
				name: format!("widget-{i}"),
				description: None,
				keywords: vec![],
				upstream: None,
			});
		}

		let enrich = LocalEnrichment {
			max_local_inject: 3,
			..Default::default()
		};
		// Query substring matches all widget-* names.
		let out = enrich.apply(&ranked, "widget", &ctx);
		let local_count = out.iter().filter(|h| h.source == HitSource::LocalOnly).count();
		assert_eq!(local_count, 3, "only max_local_inject locals may appear");
	}

	/// 6. Privacy: LocalEnrichment is separate from the public wire query;
	///    serializing the public `heart::query::Query` JSON has no usage / local
	///    fields.
	#[test]
	fn privacy_public_query_json_has_no_local_fields() {
		// Compile-time documentation: LocalContext is a separate type and does
		// not derive Serialize (see type definition above). The public wire type
		// heart::query::Query must never grow usage / dep_relation / local_only
		// fields.
		use heart::query::{
			PageSpecification, Query, QueryMode, RankSpecification, Routing, Scope, Target,
		};

		let q = Query {
			target: Target::Packages,
			text: "serde".into(),
			scope: Scope::default(),
			rank: RankSpecification::default(),
			mode: QueryMode::default(),
			routing: Routing::default(),
			session: None,
			at: None,
			page: PageSpecification::default(),
			query_id: None,
		};
		let json = serde_json::to_value(&q).expect("heart::query::Query serializes");
		let obj = json.as_object().expect("object");

		for forbidden in [
			"usage",
			"dep_relation",
			"local_only",
			"local_context",
			"times_depended",
			"last_used",
			"used_before",
		] {
			assert!(
				!obj.contains_key(forbidden),
				"heart::query::Query JSON must not contain privacy field {forbidden:?}: {obj:?}"
			);
		}

		// LocalEnrichment / LocalContext are distinct types from heart::query::Query.
		let _enrich = LocalEnrichment::default();
		let _ctx = LocalContext::default();
		assert_ne!(
			std::any::type_name::<LocalContext>(),
			std::any::type_name::<Query>()
		);
		assert_ne!(
			std::any::type_name::<LocalEnrichment>(),
			std::any::type_name::<Query>()
		);

		// Keys that *are* on the wire stay only the public query surface.
		use std::collections::HashSet;
		let keys: HashSet<&str> = obj.keys().map(String::as_str).collect();
		assert!(keys.contains("text"));
		assert!(keys.contains("target"));
	}

	/// 7. Transitive vs Direct labels differ (and only Direct gets a score lift).
	#[test]
	fn transitive_vs_direct_labels_differ() {
		let ranked = vec![
			item(1, "head", 10.0),
			item(2, "direct-pkg", 8.0),
			item(3, "trans-pkg", 8.0),
		];
		let mut ctx = LocalContext::default();
		ctx.dep_relation.insert(pid(2), DepRelation::Direct);
		ctx.dep_relation.insert(pid(3), DepRelation::Transitive);

		let enrich = LocalEnrichment::default();
		let out = enrich.apply(&ranked, "pkg", &ctx);

		let direct = out
			.iter()
			.find(|h| h.package_id == Some(pid(2)))
			.expect("direct");
		let trans = out
			.iter()
			.find(|h| h.package_id == Some(pid(3)))
			.expect("transitive");

		assert!(direct.labels.contains(&LocalLabel::DirectDep));
		assert!(!direct.labels.contains(&LocalLabel::TransitiveDep));
		assert!(trans.labels.contains(&LocalLabel::TransitiveDep));
		assert!(!trans.labels.contains(&LocalLabel::DirectDep));

		assert_eq!(direct.dep_relation, Some(DepRelation::Direct));
		assert_eq!(trans.dep_relation, Some(DepRelation::Transitive));

		// Same base score: direct's bonus lifts it above transitive.
		assert!(
			direct.score > trans.score,
			"direct dep bonus should lift score; direct={} trans={}",
			direct.score,
			trans.score
		);
	}

	#[test]
	fn dev_dep_label_and_smaller_bonus_than_direct() {
		let ranked = vec![item(1, "a", 5.0), item(2, "b", 5.0)];
		let mut ctx = LocalContext::default();
		ctx.dep_relation.insert(pid(1), DepRelation::Direct);
		ctx.dep_relation.insert(pid(2), DepRelation::Dev);

		let out = LocalEnrichment::default().apply(&ranked, "x", &ctx);
		let d = out.iter().find(|h| h.package_id == Some(pid(1))).unwrap();
		let v = out.iter().find(|h| h.package_id == Some(pid(2))).unwrap();
		assert!(d.labels.contains(&LocalLabel::DirectDep));
		assert!(v.labels.contains(&LocalLabel::DevDep));
		assert!(d.score > v.score);
	}

	#[test]
	fn additive_bonus_is_hard_capped() {
		let ranked = vec![item(1, "only", 1.0)];
		let mut ctx = LocalContext::default();
		ctx.usage.insert(
			pid(1),
			UsageStat {
				times_depended: 99,
				last_used: None,
			},
		);
		ctx.dep_relation.insert(pid(1), DepRelation::Direct);

		let enrich = LocalEnrichment {
			used_before_bonus: 5.0,
			direct_dep_bonus: 5.0,
			max_additive_bonus: 0.75,
			..Default::default()
		};

		let out = enrich.apply(&ranked, "only", &ctx);
		assert_eq!(out.len(), 1);
		assert!((out[0].score - (1.0 + 0.75)).abs() < 1e-5);
	}

	/// Silence dead-code style warning if helper is unused in future edits.
	#[test]
	fn ids_helper_smoke() {
		let ranked = vec![item(1, "a", 1.0)];
		let out = LocalEnrichment::default().apply(&ranked, "a", &LocalContext::default());
		assert_eq!(ids(&out), vec![Some(pid(1))]);
	}
}
