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

/// A package that exists only on this machine (path dep, fork, workspace
/// member).
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
/// Minimal surface so the sidecar stays decoupled from `GlobalPackage`
/// hydration.
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
    /// Registry package id when [`HitSource::Registry`]; `None` for pure local
    /// rows.
    pub package_id: Option<PackageId>,
    pub name: String,
    /// Score after soft local re-key (registry) or a synthetic inject score
    /// (local).
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

mod order;
use order::{Working, local_name_matches, reorder_with_climb_cap};

#[cfg(test)]
mod tests;
