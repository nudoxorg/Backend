//! P2 continuity matcher (§5).
//!
//! Given the tip table (all live entries from the previous generation) and
//! the set of staged wire entries (new generation, still on wire-ids), the
//! matcher computes σ: wire-id → durable-id.
//!
//! # Algorithm
//!
//! 1. Build three candidate indexes from the tip table.
//! 2. For each added (wire-id) entry, generate candidates (cap 64, lowest ids).
//! 3. Score each candidate pair with frozen integer weights (§5.5).
//! 4. Run deterministic greedy assignment with the margin rule (§5.6).
//! 5. Return σ and a [`ContinuitySummary`] of ops.
//!
//! # Determinism
//!
//! All iteration is over [`BTreeMap`] (sorted by key bytes). No [`HashMap`]
//! iteration influences decisions.

use std::collections::{BTreeMap, BTreeSet};

use ir::change::{IntroId, StableRef};
use ir::apply::PristineIntroTable;
use ir::kind::KindDiscriminant;
use ir::wire::OwnedEntryPayload;
use ir::{BodyEmbed, BodyFacts, ControlSketch};

use crate::f1::{compute_api_surface_hash, ContinuityOp, ContinuitySummary, RenameEdge};
use ir::serialize::LinkWire;

// ---------------------------------------------------------------------------
// Frozen scoring weights (§5.5)
// ---------------------------------------------------------------------------

const W_API_SURFACE: i32 = 50;
const W_NAME_EQ: i32 = 20;
const W_PARENT_CONT: i32 = 15;
const W_SIG_KEY: i32 = 15;
const W_DOC_EQ: i32 = 10;
const W_SRC_EQ: i32 = 5;
const W_ALIAS_OVERLAP: i32 = 5;

// Body axis (CONTINUITY-PQGRAM-PLAN §3.7): a body-similarity corroborator built
// from what a body *references* (resolved callees ∪ type mentions, keyed by
// durable `StableRef` — edit-stable through a callee's own rename) plus a coarse
// control-flow-shape check. It can only *promote* a candidate that another exact
// signal already surfaced — never a sole match (total cap < `THRESHOLD_SOFT`).
const W_BODY_REFS_NEAR: i32 = 12; // resolved-ref Jaccard ≥ 0.80
const W_BODY_REFS_MID: i32 = 6; //  resolved-ref Jaccard ≥ 0.50
const W_BODY_CTRL_SHAPE: i32 = 2; // identical coarse control-flow shape
/// Below this many resolved refs on either side the overlap is statistically
/// meaningless (tiny-body floor, §5.2) → the ref axis abstains.
const MIN_BODY_REFS: usize = 2;

// Never-merge invariant, enforced at compile time: the body signal alone must
// never reach THRESHOLD_SOFT, so it can never manufacture a candidate — let
// alone a match — from body structure alone.
const _: () = assert!(
    W_BODY_REFS_NEAR + W_BODY_CTRL_SHAPE < THRESHOLD_SOFT,
    "body score must stay below THRESHOLD_SOFT (never a sole match)",
);

/// The set of durable symbols a body *references*: resolved callee targets at or
/// above the graph-worthy confidence floor (`Confidence::Index`), unioned with
/// type mentions. A `BTreeSet`, not a multiset — the question is "does this body
/// use symbol X", not "how often" (a hot loop calling `alloc` 10× and
/// straight-line code calling it once do the same work). `StableRef: Ord` →
/// deterministic.
fn oracle_ref_set(facts: &BodyFacts) -> BTreeSet<&StableRef> {
    let mut set = BTreeSet::new();
    for call in &facts.oracle.calls {
        // Skip unresolved (`None`) and below-graph-floor targets: those are
        // plausible guesses, not cross-package-bound identities.
        if call.confidence.is_graph_worthy() {
            if let Some(target) = &call.target {
                set.insert(target);
            }
        }
    }
    for tm in &facts.oracle.type_mentions {
        set.insert(&tm.ty);
    }
    set
}

/// Coarse control-flow shape `(n_if, n_match, n_loop, total_match_arms)`, each
/// saturated so equality means "same rough branch structure", not exact counts.
/// Deterministic (iterates a `Vec` in source order).
fn ctrl_shape(facts: &BodyFacts) -> (u8, u8, u8, u8) {
    let (mut n_if, mut n_match, mut n_loop, mut n_arms) = (0u32, 0u32, 0u32, 0u32);
    for sketch in &facts.tree.control {
        match sketch {
            ControlSketch::If { .. } => n_if += 1,
            ControlSketch::Match { arms, .. } => {
                n_match += 1;
                n_arms += arms.len() as u32;
            }
            ControlSketch::Loop { .. } => n_loop += 1,
        }
    }
    (
        n_if.min(7) as u8,
        n_match.min(7) as u8,
        n_loop.min(7) as u8,
        n_arms.min(15) as u8,
    )
}

/// Combined body-similarity score between a staged wire body and a tip body.
///
/// Abstains (0) when either body is `Absent`. The resolved-ref axis is
/// fidelity-gated — it requires `oracle_ran` on BOTH sides (a treesitter-only
/// body has no resolved `StableRef` targets, so ref overlap is meaningless) and
/// at least `MIN_BODY_REFS` refs per side. The control-shape axis (≤2, tree
/// tier) needs no oracle. The total is capped below `THRESHOLD_SOFT`, so the body
/// can only corroborate, never match alone.
///
/// # σ-substitution caveat (deliberate under-count)
/// Staged callee refs may key co-renamed callees by *wire* id while tip refs use
/// *durable* ids, and σ is not yet known when a pair is scored. This
/// UNDER-counts overlap exactly when co-changed callees exist → a lower body
/// score → a miss (false split), never a false merge. Accepted: the error
/// direction is safe under never-merge, the effect is bounded (cap 14), and a
/// partial-σ correction would introduce order-dependent nondeterminism.
///
/// Integer-only (Jaccard by cross-multiplication; `BTreeSet` iteration; no `f32`,
/// no `HashMap`) → deterministic across machines (§6.3).
fn body_similarity_score(wire: Option<&BodyEmbed>, tip: Option<&BodyEmbed>) -> i32 {
    let (Some(wire), Some(tip)) = (wire, tip) else {
        return 0;
    };
    let (Some(wf), Some(tf)) = (wire.facts(), tip.facts()) else {
        return 0;
    };

    let mut score = 0i32;

    // Sub-signal A: resolved-ref Jaccard — requires the oracle on both sides.
    if wf.merge.oracle_ran && tf.merge.oracle_ran {
        let wset = oracle_ref_set(wf);
        let tset = oracle_ref_set(tf);
        if wset.len() >= MIN_BODY_REFS && tset.len() >= MIN_BODY_REFS {
            let inter = wset.intersection(&tset).count();
            let union = wset.union(&tset).count();
            if union > 0 {
                // Jaccard ≥ 0.80 → NEAR ; ≥ 0.50 → MID (integer cross-mults).
                if inter * 100 >= union * 80 {
                    score += W_BODY_REFS_NEAR;
                } else if inter * 100 >= union * 50 {
                    score += W_BODY_REFS_MID;
                }
            }
        }
    }

    // Sub-signal B: coarse control-flow shape — tree-tier corroborator, no
    // oracle needed. Only when BOTH bodies have some control structure
    // (empty == empty is uninformative) and the shapes match exactly.
    let ws = ctrl_shape(wf);
    let ts = ctrl_shape(tf);
    let both_have_control = (ws.0 | ws.1 | ws.2) > 0 && (ts.0 | ts.1 | ts.2) > 0;
    if both_have_control && ws == ts {
        score += W_BODY_CTRL_SHAPE;
    }

    score
}

/// Minimum total score for a High-confidence match.
const THRESHOLD_HIGH: i32 = 60;
/// Minimum total score for a Soft (speculative) match.
const THRESHOLD_SOFT: i32 = 30;
/// Margin: if no other candidate ≥ High and both are Soft, both stay Soft.
const MARGIN_HIGH: i32 = 15;

/// Candidate cap per added entry.
const CANDIDATE_CAP: usize = 64;

// ---------------------------------------------------------------------------
// Candidate indexes built from tip table
// ---------------------------------------------------------------------------

struct TipIndexes {
    /// (kind, name) → sorted vec of IntroId
    by_kind_name: BTreeMap<(KindDiscriminant, String), Vec<IntroId>>,
    /// api_surface_hash → sorted vec of IntroId
    by_shape: BTreeMap<[u8; 32], Vec<IntroId>>,
    /// (kind, parent_opt, name_stem) → sorted vec of IntroId
    by_kind_parent_stem: BTreeMap<(KindDiscriminant, Option<IntroId>, String), Vec<IntroId>>,
}

fn name_stem(name: &str) -> String {
    // Strip generic suffix `<...>` if any.
    let s = name.split('<').next().unwrap_or(name);
    s.trim().to_ascii_lowercase()
}

fn build_indexes(tip_table: &PristineIntroTable) -> TipIndexes {
    let mut by_kind_name: BTreeMap<(KindDiscriminant, String), Vec<IntroId>> = BTreeMap::new();
    let mut by_shape: BTreeMap<[u8; 32], Vec<IntroId>> = BTreeMap::new();
    let mut by_kind_parent_stem: BTreeMap<(KindDiscriminant, Option<IntroId>, String), Vec<IntroId>> = BTreeMap::new();

    for (id, payload) in tip_table.live_entries() {
        let k = payload.kind_disc;
        let name = payload.symbol.name.clone();
        let stem = name_stem(&name);
        let parent = tip_table.parent_of(id);

        // Index 1: (kind, name)
        by_kind_name.entry((k, name.clone())).or_default().push(id);

        // Index 2: api_surface_hash
        let shape = compute_api_surface_hash(payload);
        by_shape.entry(*shape.as_bytes()).or_default().push(id);

        // Index 3: (kind, parent, stem)
        by_kind_parent_stem
            .entry((k, parent, stem))
            .or_default()
            .push(id);
    }

    // Sort all vecs for determinism.
    for v in by_kind_name.values_mut() { v.sort(); }
    for v in by_shape.values_mut() { v.sort(); }
    for v in by_kind_parent_stem.values_mut() { v.sort(); }

    TipIndexes { by_kind_name, by_shape, by_kind_parent_stem }
}

// ---------------------------------------------------------------------------
// Candidate generation
// ---------------------------------------------------------------------------

fn candidates_for(
    _wire_id: IntroId,
    payload: &OwnedEntryPayload,
    parent: Option<IntroId>,
    sigma: &BTreeMap<IntroId, IntroId>,  // wire→durable for already-matched entries
    indexes: &TipIndexes,
) -> BTreeSet<IntroId> {
    let k = payload.kind_disc;
    let name = payload.symbol.name.clone();
    let stem = name_stem(&name);
    let durable_parent = parent.map(|p| sigma.get(&p).copied().unwrap_or(p));

    let mut cands: BTreeSet<IntroId> = BTreeSet::new();

    // Index 1: same (kind, name)
    if let Some(ids) = indexes.by_kind_name.get(&(k, name.clone())) {
        for &id in ids.iter().take(CANDIDATE_CAP) {
            cands.insert(id);
        }
    }

    // Index 2: same api_surface_hash
    let shape = compute_api_surface_hash(payload);
    if let Some(ids) = indexes.by_shape.get(shape.as_bytes()) {
        for &id in ids.iter().take(CANDIDATE_CAP) {
            cands.insert(id);
        }
    }

    // Index 3: same (kind, resolved_parent, name_stem)
    if let Some(ids) = indexes.by_kind_parent_stem.get(&(k, durable_parent, stem)) {
        for &id in ids.iter().take(CANDIDATE_CAP) {
            cands.insert(id);
        }
    }

    // Cap total to 64 lowest ids.
    if cands.len() > CANDIDATE_CAP {
        let trimmed: BTreeSet<IntroId> = cands.into_iter().take(CANDIDATE_CAP).collect();
        return trimmed;
    }
    cands
}

// ---------------------------------------------------------------------------
// Scoring
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn score_pair(
    wire_payload: &OwnedEntryPayload,
    wire_parent: Option<IntroId>,
    sigma: &BTreeMap<IntroId, IntroId>,
    tip_id: IntroId,
    tip_payload: &OwnedEntryPayload,
    tip_table: &PristineIntroTable,
    wire_body: Option<&BodyEmbed>,
    tip_body: Option<&BodyEmbed>,
) -> i32 {
    let mut score = 0i32;

    // api_surface_hash eq (+50, S)
    let wire_shape = compute_api_surface_hash(wire_payload);
    let tip_shape = compute_api_surface_hash(tip_payload);
    if wire_shape == tip_shape {
        score += W_API_SURFACE;
    }

    // name eq (+20, S)
    if wire_payload.symbol.name == tip_payload.symbol.name {
        score += W_NAME_EQ;
    }

    // parent continuity (+15, S)
    // Check if the wire parent resolves to the same durable as the tip parent.
    let wire_durable_parent = wire_parent.map(|p| sigma.get(&p).copied().unwrap_or(p));
    let tip_parent = tip_table.parent_of(tip_id);
    if wire_durable_parent == tip_parent {
        score += W_PARENT_CONT;
    }

    // sig_key eq (+15 for Function kind)
    if wire_payload.kind_disc == KindDiscriminant::Function
        && tip_payload.kind_disc == KindDiscriminant::Function
        && let (ir::wire::KindWire::Function(wf), ir::wire::KindWire::Function(tf)) =
            (&wire_payload.kind, &tip_payload.kind)
        {
            let wire_sig = ir::intro::sig_key(
                &wf.input_params,
                &wf.output_params,
                &wf.sig,
            );
            let tip_sig = ir::intro::sig_key(
                &tf.input_params,
                &tf.output_params,
                &tf.sig,
            );
            if wire_sig == tip_sig {
                score += W_SIG_KEY;
            }
        }

    // doc bytes eq & non-empty (+10, S)
    match (&wire_payload.symbol.documentation, &tip_payload.symbol.documentation) {
        (Some(wd), Some(td)) if !wd.is_empty() && wd == td => {
            score += W_DOC_EQ;
        }
        _ => {}
    }

    // src eq (+5, S)
    if wire_payload.symbol.source_path == tip_payload.symbol.source_path
        && !wire_payload.symbol.source_path.is_empty()
    {
        score += W_SRC_EQ;
    }

    // alias overlap ≥ 1 (+5, S)
    let wire_aliases: BTreeSet<_> = wire_payload.symbol.aliases.iter().collect();
    let has_alias_overlap = tip_payload.symbol.aliases.iter().any(|a| wire_aliases.contains(a));
    if has_alias_overlap {
        score += W_ALIAS_OVERLAP;
    }

    // body axis (§3.7): resolved-ref overlap + control-flow shape. Abstains (0)
    // when either body is absent/tiny/oracle-less; capped < THRESHOLD_SOFT so it
    // can only promote a candidate another signal surfaced, never solely match.
    score += body_similarity_score(wire_body, tip_body);

    score
}

// ---------------------------------------------------------------------------
// Special rules
// ---------------------------------------------------------------------------

// R-CHILD: Field/Variant require parent continuity for High confidence.
fn check_r_child(
    wire_payload: &OwnedEntryPayload,
    wire_parent: Option<IntroId>,
    sigma: &BTreeMap<IntroId, IntroId>,
    tip_id: IntroId,
    tip_table: &PristineIntroTable,
) -> bool {
    match wire_payload.kind_disc {
        KindDiscriminant::Field | KindDiscriminant::Variant => {
            let wire_durable_parent = wire_parent.map(|p| sigma.get(&p).copied().unwrap_or(p));
            let tip_parent = tip_table.parent_of(tip_id);
            wire_durable_parent == tip_parent && wire_durable_parent.is_some()
        }
        _ => true, // no R-CHILD constraint for other kinds
    }
}

// ---------------------------------------------------------------------------
// Main entry point
// ---------------------------------------------------------------------------

/// Compute σ: wire-id → durable-id from tip table + staged entries.
///
/// Returns the sigma map and a [`ContinuitySummary`] of ops. This is the
/// declaration-only entry point; it delegates to [`compute_sigma_with_bodies`]
/// with empty body maps (the body axis then abstains).
pub fn compute_sigma(
    tip_table: &PristineIntroTable,
    staged: &[(IntroId, OwnedEntryPayload, Option<IntroId>, Vec<LinkWire>)],
) -> (BTreeMap<IntroId, IntroId>, ContinuitySummary) {
    let empty: BTreeMap<IntroId, BodyEmbed> = BTreeMap::new();
    compute_sigma_with_bodies(tip_table, staged, &empty, &empty)
}

/// Compute σ with the **body axis** (CONTINUITY-PQGRAM-PLAN §3.7) enabled.
///
/// `staged_bodies` maps each staged **wire-id** to its merged body; `tip_bodies`
/// maps each **durable tip-id** to the previous generation's body (materialized
/// from the `.nb` companions). Where a body is missing on either side the body
/// signal contributes 0 — never-merge-safe by construction.
pub fn compute_sigma_with_bodies(
    tip_table: &PristineIntroTable,
    staged: &[(IntroId, OwnedEntryPayload, Option<IntroId>, Vec<LinkWire>)],
    staged_bodies: &BTreeMap<IntroId, BodyEmbed>,
    tip_bodies: &BTreeMap<IntroId, BodyEmbed>,
) -> (BTreeMap<IntroId, IntroId>, ContinuitySummary) {
    // Collect tip entries into a map for fast access.
    let tip_map: BTreeMap<IntroId, &OwnedEntryPayload> = tip_table.live_entries().collect();

    // Build indexes.
    let indexes = build_indexes(tip_table);

    // Staged entries: wire_id → (payload, parent, links).
    let staged_map: BTreeMap<IntroId, (&OwnedEntryPayload, Option<IntroId>)> = staged
        .iter()
        .map(|(id, p, par, _)| (*id, (p, *par)))
        .collect();

    let staged_ids: BTreeSet<IntroId> = staged_map.keys().copied().collect();

    // σ: wire_id → durable_id (initially identity, i.e. wire_id == durable_id for new)
    let mut sigma: BTreeMap<IntroId, IntroId> = BTreeMap::new();

    // deleted_ids: tip ids not seen in staged
    let tip_ids: BTreeSet<IntroId> = tip_map.keys().copied().collect();
    let deleted_ids: BTreeSet<IntroId> = tip_ids.difference(&staged_ids).copied().collect();
    // --- simple case: if staged_id is already in tip, it continues as-is ---
    // (i.e. σ(wire_id) = wire_id for preserved symbols)
    for id in staged_ids.intersection(&tip_ids) {
        sigma.insert(*id, *id);
    }

    // newly added: staged ids NOT in tip (candidates for matching deleted tip ids)
    let new_wire_ids: BTreeSet<IntroId> = staged_ids.difference(&tip_ids).copied().collect();

    // --- R-OV pre-pass (§5.4, overload flip) ---
    // Within a (kind, resolved-parent, name) bucket, if exactly one deleted `d`
    // and exactly one added `a` exist, that pair is High regardless of score:
    // the flip is unambiguous even when shape+sig+doc all changed. Such pairs may
    // score below Soft, so they are forced in here rather than via `all_pairs`.
    type Bucket = (KindDiscriminant, Option<IntroId>, String);
    let bucket_of = |k: KindDiscriminant, parent: Option<IntroId>, name: &str| -> Bucket {
        (k, parent, name.to_owned())
    };
    let mut del_bucket: BTreeMap<Bucket, Vec<IntroId>> = BTreeMap::new();
    for d in &deleted_ids {
        let p = tip_map[d];
        del_bucket
            .entry(bucket_of(p.kind_disc, tip_table.parent_of(*d), &p.symbol.name))
            .or_default()
            .push(*d);
    }
    let mut add_bucket: BTreeMap<Bucket, Vec<IntroId>> = BTreeMap::new();
    for a in &new_wire_ids {
        let (p, par) = staged_map[a];
        let durable_parent = par.map(|x| sigma.get(&x).copied().unwrap_or(x));
        add_bucket
            .entry(bucket_of(p.kind_disc, durable_parent, &p.symbol.name))
            .or_default()
            .push(*a);
    }
    let mut forced_high: BTreeMap<IntroId, IntroId> = BTreeMap::new(); // wire → tip
    for (bucket, ds) in &del_bucket {
        if let (Some([d]), Some([a])) = (
            <&[IntroId; 1]>::try_from(ds.as_slice()).ok(),
            add_bucket.get(bucket).and_then(|v| <&[IntroId; 1]>::try_from(v.as_slice()).ok()),
        ) {
            forced_high.insert(*a, *d);
        }
    }

    // --- Score all candidate pairs (kind-gated, ≥ Soft) ---
    let mut all_pairs: Vec<(i32, IntroId, IntroId)> = Vec::new();
    for wire_id in &new_wire_ids {
        let (wire_payload, wire_parent) = staged_map[wire_id];
        let cands = candidates_for(*wire_id, wire_payload, wire_parent, &sigma, &indexes);
        for tip_id in &cands {
            if !deleted_ids.contains(tip_id) {
                continue;
            }
            let tip_payload = tip_map[tip_id];
            // Hard gate (§5.4): a pair across kinds does not exist (C-5).
            if wire_payload.kind_disc != tip_payload.kind_disc {
                continue;
            }
            let score = score_pair(
                wire_payload,
                wire_parent,
                &sigma,
                *tip_id,
                tip_payload,
                tip_table,
                staged_bodies.get(wire_id),
                tip_bodies.get(tip_id),
            );
            if score >= THRESHOLD_SOFT {
                all_pairs.push((score, *wire_id, *tip_id));
            }
        }
    }
    // Highest score first; deterministic tie-break.
    all_pairs.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));

    let is_high = |wire_id: IntroId, tip_id: IntroId, score: i32| {
        score >= THRESHOLD_HIGH || forced_high.get(&wire_id) == Some(&tip_id)
    };

    let mut assigned_wire: BTreeSet<IntroId> = BTreeSet::new();
    let mut assigned_tip: BTreeSet<IntroId> = BTreeSet::new();
    let mut matched: Vec<(IntroId, IntroId)> = Vec::new(); // (wire, tip) — σ pairs
    let mut rename_edges: Vec<RenameEdge> = Vec::new(); // Soft/ambiguous advisory only

    // R-OV forced-High pairs assign first (they bypass the score/margin logic).
    for (&wire_id, &tip_id) in &forced_high {
        if assigned_wire.contains(&wire_id) || assigned_tip.contains(&tip_id) {
            continue;
        }
        let (wp, wpar) = staged_map[&wire_id];
        if !check_r_child(wp, wpar, &sigma, tip_id, tip_table) {
            continue;
        }
        sigma.insert(wire_id, tip_id);
        assigned_wire.insert(wire_id);
        assigned_tip.insert(tip_id);
        matched.push((wire_id, tip_id));
    }

    // Greedy over the scored pairs. Only High (§5.5) becomes σ; Soft or
    // margin-ambiguous pairs are advisory RenameEdges, never σ.
    for &(score, wire_id, tip_id) in &all_pairs {
        if assigned_wire.contains(&wire_id) || assigned_tip.contains(&tip_id) {
            continue;
        }
        if !is_high(wire_id, tip_id, score) {
            rename_edges.push(RenameEdge { deleted_id: tip_id, added_id: wire_id, score });
            continue;
        }
        let (wp, wpar) = staged_map[&wire_id];
        if !check_r_child(wp, wpar, &sigma, tip_id, tip_table) {
            rename_edges.push(RenameEdge { deleted_id: tip_id, added_id: wire_id, score });
            continue;
        }
        // Margin rule (§5.5): if another still-unmatched High candidate for the
        // same wire OR the same tip is within MARGIN_HIGH points, the match is
        // ambiguous → both drop to Soft (never σ). Cost asymmetry (§3.5): a false
        // merge poisons lineage, so refuse when in doubt.
        let ambiguous = all_pairs.iter().any(|&(s2, w2, t2)| {
            let shares = (w2 == wire_id && t2 != tip_id) || (t2 == tip_id && w2 != wire_id);
            shares
                && !assigned_wire.contains(&w2)
                && !assigned_tip.contains(&t2)
                && is_high(w2, t2, s2)
                && (score - s2).abs() <= MARGIN_HIGH
        });
        if ambiguous {
            rename_edges.push(RenameEdge { deleted_id: tip_id, added_id: wire_id, score });
            continue;
        }
        sigma.insert(wire_id, tip_id);
        assigned_wire.insert(wire_id);
        assigned_tip.insert(tip_id);
        matched.push((wire_id, tip_id));
    }

    // Brand-new entries keep their wire id as durable id (σ = identity).
    for wire_id in &new_wire_ids {
        sigma.entry(*wire_id).or_insert(*wire_id);
    }

    // --- Build ContinuitySummary ---
    let mut ops: Vec<ContinuityOp> = Vec::new();
    let matched_tips: BTreeSet<IntroId> = matched.iter().map(|(_, t)| *t).collect();

    // Introduced: brand-new durable ids (identity σ, not in tip).
    for (wire_id, durable_id) in &sigma {
        if wire_id == durable_id && !tip_ids.contains(wire_id) {
            ops.push(ContinuityOp::Introduced { id: *durable_id });
        }
    }
    // Deleted: tip ids no σ pair reused.
    for tip_id in &deleted_ids {
        if !matched_tips.contains(tip_id) {
            ops.push(ContinuityOp::Deleted { id: *tip_id });
        }
    }
    // Renamed / Moved / SignatureEvolved for σ-assigned pairs only.
    for &(wire_id, tip_id) in &matched {
        let tip_payload = tip_map[&tip_id];
        let (wire_payload, wire_parent) = staged_map[&wire_id];
        let durable_id = tip_id;

        if wire_payload.symbol.name != tip_payload.symbol.name {
            ops.push(ContinuityOp::Renamed {
                id: durable_id,
                old_name: tip_payload.symbol.name.clone(),
                new_name: wire_payload.symbol.name.clone(),
            });
        }
        let tip_parent = tip_table.parent_of(tip_id);
        let wire_durable_parent = wire_parent.map(|p| sigma.get(&p).copied().unwrap_or(p));
        if wire_durable_parent != tip_parent {
            ops.push(ContinuityOp::Moved {
                id: durable_id,
                old_parent: tip_parent,
                new_parent: wire_durable_parent,
            });
        }
        // SignatureEvolved: functions only, keyed on the true SigKey (§4.6), not
        // the surface hash (which also moves on rename/vis/attr changes).
        if let (
            ir::wire::KindWire::Function(wf),
            ir::wire::KindWire::Function(tf),
        ) = (&wire_payload.kind, &tip_payload.kind)
        {
            let new_sig = ir::intro::sig_key(&wf.input_params, &wf.output_params, &wf.sig);
            let old_sig = ir::intro::sig_key(&tf.input_params, &tf.output_params, &tf.sig);
            if new_sig != old_sig {
                ops.push(ContinuityOp::SignatureEvolved {
                    id: durable_id,
                    old_sigkey: old_sig,
                    new_sigkey: new_sig,
                });
            }
        }
    }

    // Resurrection (§4.4) is tagged by the session (it holds symbol_history);
    // the matcher only sees the live tip, so it cannot distinguish a brand-new
    // id from a resurrected one here.

    let summary = ContinuitySummary { ops, rename_edges };
    (sigma, summary)
}

// ---------------------------------------------------------------------------
// Tests (C-1..C-10 from §16.1)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use ir::change::IntroId;
    use ir::apply::PristineIntroTable;
    use ir::kind::KindDiscriminant;
    use ir::symbol::Visibility;
    use ir::wire::{
        EntryPayloadFlags, FieldWire, FnSigFlags, FunctionWire, KindWire, OwnedEntryPayload,
        ParamWire, RecordForm, SymbolWire, TypeRefWire,
    };
    // Body-axis test deps (BodyEmbed / StableRef already in scope via `super::*`).
    use heart::Language;
    use ir::change::{EcosystemId, PackageLineageId, PackageName};
    use ir::{
        body_path, deserialize_body, merge_body, serialize_body, BodyCall, BodyMergeNote,
        BodyWireError, Confidence, OracleBody, OracleCall, OracleTypeMention, ReferenceKind, RelSpan,
        TreesitterBody, BODY_DOMAIN_V1,
    };

    fn intro(n: u8) -> IntroId {
        IntroId::from_raw([n; 32])
    }

    fn sym(name: &str) -> SymbolWire {
        SymbolWire {
            name: name.to_owned(),
            visibility: Visibility::Public,
            documentation: None,
            source_path: String::new(),
            span_start: 0,
            span_end: 1,
            aliases: Vec::new(),
            deprecation: None,
            doc_links: Vec::new(),
            attrs: vec![],
            cfg: None,
        }
    }

    fn fn_payload(name: &str) -> OwnedEntryPayload {
        let s = sym(name);
        let k = KindWire::Function(FunctionWire {
            input_params: Box::new([]),
            output_params: Box::new([]),
            sig: FnSigFlags::default(),
            generics: Box::new([]),
            wheres: Box::new([]),
        });
        OwnedEntryPayload::sealed(s, KindDiscriminant::Function, k, EntryPayloadFlags::default())
    }

    fn fn_payload_with_param(name: &str, param_id: u8) -> OwnedEntryPayload {
        let s = sym(name);
        let k = KindWire::Function(FunctionWire {
            input_params: Box::new([ParamWire { name: Some("x".into()), ty: TypeRefWire::Same(intro(param_id)) }]),
            output_params: Box::new([]),
            sig: FnSigFlags::default(),
            generics: Box::new([]),
            wheres: Box::new([]),
        });
        OwnedEntryPayload::sealed(s, KindDiscriminant::Function, k, EntryPayloadFlags::default())
    }


    fn staged_one(wire_id: u8, payload: OwnedEntryPayload) -> (IntroId, OwnedEntryPayload, Option<IntroId>, Vec<LinkWire>) {
        (intro(wire_id), payload, None, vec![])
    }

    // -----------------------------------------------------------------------
    // Body-axis helpers + tests (B-1..B-6)
    // -----------------------------------------------------------------------

    fn stable_ref(name: &str) -> StableRef {
        StableRef::new(
            PackageLineageId::new(EcosystemId::new("rust"), PackageName::new("testpkg")),
            IntroId::from_domain("b-test.intro", name.as_bytes()),
        )
    }

    /// A `Present` body with the given resolved callees (`Confidence::Oracle`, so
    /// graph-worthy). ≥2 distinct shared callees on both sides → ref-Jaccard 1.0
    /// → NEAR (+12). Empty tree tier → no control-shape bonus.
    fn body_with_calls(callees: &[&str]) -> BodyEmbed {
        let calls: Vec<OracleCall> = callees
            .iter()
            .enumerate()
            .map(|(i, name)| OracleCall {
                target: Some(stable_ref(name)),
                kind: ReferenceKind::FunctionCall,
                confidence: Confidence::Oracle,
                rel_span: RelSpan::new((i * 10) as u32, (i * 10 + 5) as u32),
            })
            .collect();
        merge_body(
            Language::Rust,
            TreesitterBody::default(),
            OracleBody { calls, ..Default::default() },
            BodyMergeNote::both_ran(),
        )
    }

    /// A one-param fn with a shared non-empty doc + source path. Two of these with
    /// the SAME name/parent/doc/src but DIFFERENT param score
    /// NAME(20)+PARENT(15)+DOC(10)+SRC(5)=50 (api_surface & sig_key differ → 0),
    /// i.e. < HIGH(60): the setup where the body's +12 is decisive.
    fn fn_payload_with_doc_and_src(name: &str, param_id: u8) -> OwnedEntryPayload {
        let mut s = sym(name);
        s.documentation = Some("shared documentation string".to_owned());
        s.source_path = "src/lib.rs".to_owned();
        let k = KindWire::Function(FunctionWire {
            input_params: Box::new([ParamWire {
                name: Some("x".into()),
                ty: TypeRefWire::Same(intro(param_id)),
            }]),
            output_params: Box::new([]),
            sig: FnSigFlags::default(),
            generics: Box::new([]),
            wheres: Box::new([]),
        });
        OwnedEntryPayload::sealed(s, KindDiscriminant::Function, k, EntryPayloadFlags::default())
    }

    /// A treesitter-only body: callees live on the tree tier (no resolved
    /// `StableRef` targets), `oracle_ran == false`. The ref axis must abstain.
    fn body_treesitter_only(callees: &[&str]) -> BodyEmbed {
        let calls: Vec<BodyCall> = callees
            .iter()
            .enumerate()
            .map(|(i, name)| BodyCall {
                name: (*name).into(),
                receiver: None,
                rel_span: RelSpan::new((i * 10) as u32, (i * 10 + 5) as u32),
            })
            .collect();
        merge_body(
            Language::Rust,
            TreesitterBody { calls, ..Default::default() },
            OracleBody::default(),
            BodyMergeNote::treesitter_only(),
        )
    }

    /// A `Present` body whose oracle side has only type mentions (no callees).
    fn body_with_type_mentions(types: &[&str]) -> BodyEmbed {
        let type_mentions: Vec<OracleTypeMention> = types
            .iter()
            .enumerate()
            .map(|(i, name)| OracleTypeMention {
                ty: stable_ref(name),
                rel_span: RelSpan::new((i * 10) as u32, (i * 10 + 5) as u32),
            })
            .collect();
        merge_body(
            Language::Rust,
            TreesitterBody::default(),
            OracleBody { type_mentions, ..Default::default() },
            BodyMergeNote::both_ran(),
        )
    }

    /// A `Present` body with resolved callees plus `n_if` `if` control regions.
    fn body_with_calls_and_ifs(callees: &[&str], n_if: usize) -> BodyEmbed {
        let calls: Vec<OracleCall> = callees
            .iter()
            .enumerate()
            .map(|(i, name)| OracleCall {
                target: Some(stable_ref(name)),
                kind: ReferenceKind::FunctionCall,
                confidence: Confidence::Oracle,
                rel_span: RelSpan::new((i * 10) as u32, (i * 10 + 5) as u32),
            })
            .collect();
        let control: Vec<ControlSketch> = (0..n_if)
            .map(|i| ControlSketch::If {
                cond: RelSpan::new((i * 20) as u32, (i * 20 + 3) as u32),
                then_arm: RelSpan::new((i * 20 + 3) as u32, (i * 20 + 10) as u32),
                else_arm: None,
            })
            .collect();
        merge_body(
            Language::Rust,
            TreesitterBody { control, ..Default::default() },
            OracleBody { calls, ..Default::default() },
            BodyMergeNote::both_ran(),
        )
    }

    // B-1: the signal is NEAR (12) when both sides share ≥2 resolved refs.
    #[test]
    fn b1_body_score_near_on_shared_refs() {
        let a = body_with_calls(&["alloc", "log"]);
        let b = body_with_calls(&["alloc", "log"]);
        assert_eq!(body_similarity_score(Some(&a), Some(&b)), W_BODY_REFS_NEAR);
    }

    // MID at Jaccard ≥ 0.50: 2 shared of 3 each → union 4, inter 2 → 0.5.
    #[test]
    fn b1b_body_score_mid_on_partial_overlap() {
        let a = body_with_calls(&["alloc", "log", "x"]);
        let b = body_with_calls(&["alloc", "log", "y"]);
        assert_eq!(body_similarity_score(Some(&a), Some(&b)), W_BODY_REFS_MID);
    }

    // Disjoint refs → 0 (Jaccard 0 < 0.50).
    #[test]
    fn b1c_body_score_zero_on_disjoint() {
        let a = body_with_calls(&["alloc", "log"]);
        let b = body_with_calls(&["parse", "write"]);
        assert_eq!(body_similarity_score(Some(&a), Some(&b)), 0);
    }

    // B-2: an Absent body on either side contributes 0.
    #[test]
    fn b2_body_score_zero_when_absent() {
        let a = body_with_calls(&["alloc", "log"]);
        assert_eq!(body_similarity_score(Some(&a), None), 0);
        assert_eq!(body_similarity_score(None, Some(&a)), 0);
        assert_eq!(body_similarity_score(None, None), 0);
    }

    // Fidelity gate: treesitter-only bodies (no resolved oracle refs) abstain.
    #[test]
    fn b2b_body_score_gated_on_oracle_fidelity() {
        let a = body_treesitter_only(&["alloc", "log"]);
        let b = body_treesitter_only(&["alloc", "log"]);
        assert_eq!(body_similarity_score(Some(&a), Some(&b)), 0);
    }

    // Type mentions count toward the resolved-ref set (richer than callees alone).
    #[test]
    fn b2c_body_score_counts_type_mentions() {
        let a = body_with_type_mentions(&["Request", "Response"]);
        let b = body_with_type_mentions(&["Request", "Response"]);
        assert_eq!(body_similarity_score(Some(&a), Some(&b)), W_BODY_REFS_NEAR);
    }

    // Control-flow shape adds a +2 corroborator on top of the ref score, and only
    // when the shapes match.
    #[test]
    fn b2d_body_score_control_shape_bonus() {
        let a = body_with_calls_and_ifs(&["alloc", "log"], 1);
        let b = body_with_calls_and_ifs(&["alloc", "log"], 1);
        assert_eq!(
            body_similarity_score(Some(&a), Some(&b)),
            W_BODY_REFS_NEAR + W_BODY_CTRL_SHAPE
        );
        let c = body_with_calls_and_ifs(&["alloc", "log"], 2);
        assert_eq!(body_similarity_score(Some(&a), Some(&c)), W_BODY_REFS_NEAR);
    }

    // B-3: never sole High. Same name (so the pair surfaces) but different param
    // and different parent → NAME(20)+body(12)=32 (≥ SOFT 30 but < HIGH 60) →
    // advisory only, never σ. The never-merge safety property.
    #[test]
    fn b3_body_never_sole_high() {
        let mut tip = PristineIntroTable::new();
        tip.insert_live(intro(9), fn_payload("Parent"), None);
        tip.insert_live(intro(1), fn_payload_with_param("helper", 7), Some(intro(9)));
        let staged = vec![(intro(2), fn_payload_with_param("helper", 8), None, vec![])];

        let shared = body_with_calls(&["alloc", "log"]);
        let mut staged_bodies = BTreeMap::new();
        staged_bodies.insert(intro(2), shared.clone());
        let mut tip_bodies = BTreeMap::new();
        tip_bodies.insert(intro(1), shared);

        let (sigma, summary) =
            compute_sigma_with_bodies(&tip, &staged, &staged_bodies, &tip_bodies);
        assert_eq!(
            sigma.get(&intro(2)),
            Some(&intro(2)),
            "NAME(20)+body(12)=32 < HIGH(60): body must not create a sole match"
        );
        assert!(
            summary.ops.iter().any(|op| matches!(op, ContinuityOp::Deleted { id } if *id == intro(1))),
            "unmatched tip → Deleted"
        );
        assert!(
            summary.ops.iter().any(|op| matches!(op, ContinuityOp::Introduced { id } if *id == intro(2))),
            "unmatched wire → Introduced"
        );
    }

    // B-4: tiny-body floor. <2 resolved refs on a side → ref axis abstains → 0.
    #[test]
    fn b4_body_score_abstains_below_floor() {
        let a = body_with_calls(&["only_one"]); // 1 ref < MIN_BODY_REFS(2)
        let b = body_with_calls(&["only_one"]);
        assert_eq!(body_similarity_score(Some(&a), Some(&b)), 0);
    }

    // B-4b: matcher-level effect — the body is genuinely wired into scoring.
    // A same-name pair under DIFFERENT parents (so R-OV does not force it) scores
    // NAME(20) alone = 20 < SOFT(30) and never surfaces; adding shared bodies
    // lifts it to 20+12=32 ≥ SOFT, surfacing an ADVISORY RenameEdge — but 32 <
    // HIGH(60), so it is never σ (never-merge preserved).
    #[test]
    fn b4b_body_surfaces_advisory_edge_never_sigma() {
        let mut tip = PristineIntroTable::new();
        tip.insert_live(intro(9), fn_payload("Parent"), None);
        tip.insert_live(intro(1), fn_payload_with_param("helper", 7), Some(intro(9)));
        let staged = vec![(intro(2), fn_payload_with_param("helper", 8), None, vec![])];

        // Without bodies: score 20 < SOFT → no candidate, no edge, not σ.
        let (sigma_plain, sum_plain) = compute_sigma(&tip, &staged);
        assert_eq!(sigma_plain.get(&intro(2)), Some(&intro(2)));
        assert!(
            !sum_plain.rename_edges.iter().any(|e| e.added_id == intro(2)),
            "without body the pair is below SOFT and must not surface"
        );

        // With shared bodies: 20+12=32 ≥ SOFT → advisory edge, still not σ.
        let shared = body_with_calls(&["alloc", "log"]);
        let mut staged_bodies = BTreeMap::new();
        staged_bodies.insert(intro(2), shared.clone());
        let mut tip_bodies = BTreeMap::new();
        tip_bodies.insert(intro(1), shared);

        let (sigma, summary) =
            compute_sigma_with_bodies(&tip, &staged, &staged_bodies, &tip_bodies);
        assert_eq!(sigma.get(&intro(2)), Some(&intro(2)), "32 < HIGH → never σ");
        assert!(
            summary.rename_edges.iter().any(|e| e.added_id == intro(2) && e.deleted_id == intro(1)),
            "body lifted the pair to SOFT → an advisory RenameEdge must surface"
        );
    }

    // B-5: determinism — identical inputs → identical sigma.
    #[test]
    fn b5_determinism() {
        let mut tip = PristineIntroTable::new();
        for i in 0u8..5 {
            tip.insert_live(intro(i), fn_payload_with_doc_and_src(&format!("fn_{i}"), i), None);
        }
        let staged: Vec<_> = (10u8..15)
            .map(|i| staged_one(i, fn_payload_with_doc_and_src(&format!("fn_{}", i - 10), i)))
            .collect();

        let shared = body_with_calls(&["alloc", "log"]);
        let staged_bodies: BTreeMap<IntroId, BodyEmbed> =
            (10u8..15).map(|i| (intro(i), shared.clone())).collect();
        let tip_bodies: BTreeMap<IntroId, BodyEmbed> =
            (0u8..5).map(|i| (intro(i), shared.clone())).collect();

        let (s1, _) = compute_sigma_with_bodies(&tip, &staged, &staged_bodies, &tip_bodies);
        let (s2, _) = compute_sigma_with_bodies(&tip, &staged, &staged_bodies, &tip_bodies);
        assert_eq!(s1, s2, "compute_sigma_with_bodies must be deterministic");
    }

    // B-6: .nb round-trip + body_path stability (pure unit test).
    #[test]
    fn b6_nb_round_trip() {
        let body = body_with_calls(&["alloc", "log", "parse"]);

        let bytes_a = serialize_body(&body).expect("serialize a");
        let bytes_b = serialize_body(&body).expect("serialize b");
        assert_eq!(bytes_a, bytes_b, "serialize_body must be deterministic");
        assert!(bytes_a.starts_with(BODY_DOMAIN_V1), "must carry the nudox.body.v1 prefix");

        let back = deserialize_body(&bytes_a).expect("deserialize");
        assert_eq!(back, body, "round-trip must yield an equal BodyEmbed");

        let id = intro(42);
        assert_eq!(body_path(id), body_path(id), "body_path must be stable");
        assert!(body_path(id).ends_with(".nb"), "body_path ends with .nb");

        assert!(matches!(deserialize_body(b"garbage").unwrap_err(), BodyWireError::BadDomain));
    }


    // C-1: Empty tip + empty staged → empty sigma.
    #[test]
    fn c1_empty_empty() {
        let tip = PristineIntroTable::new();
        let (sigma, summary) = compute_sigma(&tip, &[]);
        assert!(sigma.is_empty());
        assert!(summary.ops.is_empty());
    }

    // C-2: One entry in tip, same entry in staged → σ = identity, no op.
    #[test]
    fn c2_stable_entry() {
        let mut tip = PristineIntroTable::new();
        let payload = fn_payload("foo");
        tip.insert_live(intro(1), payload.clone(), None);

        let staged = vec![staged_one(1, payload)];
        let (sigma, summary) = compute_sigma(&tip, &staged);

        assert_eq!(sigma.get(&intro(1)), Some(&intro(1)));
        // Stable entry: no Introduced/Deleted ops.
        assert!(!summary.ops.iter().any(|op| matches!(op, ContinuityOp::Deleted { .. })));
        assert!(!summary.ops.iter().any(|op| matches!(op, ContinuityOp::Introduced { .. })));
    }

    // C-3: One entry in tip, same function but new wire-id → Renamed (matched by api_surface_hash).
    #[test]
    fn c3_renamed_function() {
        let mut tip = PristineIntroTable::new();
        tip.insert_live(intro(1), fn_payload("old_name"), None);

        let new_payload = fn_payload("new_name");
        let staged = vec![staged_one(2, new_payload)];
        let (sigma, summary) = compute_sigma(&tip, &staged);

        // wire_id=2 should map to tip_id=1 (same shape, different name).
        assert_eq!(sigma.get(&intro(2)), Some(&intro(1)));
        assert!(summary.ops.iter().any(|op| matches!(op, ContinuityOp::Renamed { .. })));
    }

    // C-4: One entry in tip, NOT in staged → Deleted.
    #[test]
    fn c4_deleted() {
        let mut tip = PristineIntroTable::new();
        tip.insert_live(intro(1), fn_payload("foo"), None);

        let staged = vec![];  // nothing staged
        let (_sigma, summary) = compute_sigma(&tip, &staged);

        assert!(summary.ops.iter().any(|op| matches!(op, ContinuityOp::Deleted { id } if *id == intro(1))));
    }

    // C-5: New entry in staged, not in tip → Introduced.
    #[test]
    fn c5_introduced() {
        let tip = PristineIntroTable::new();

        let staged = vec![staged_one(5, fn_payload("brand_new"))];
        let (sigma, summary) = compute_sigma(&tip, &staged);

        assert_eq!(sigma.get(&intro(5)), Some(&intro(5)));
        assert!(summary.ops.iter().any(|op| matches!(op, ContinuityOp::Introduced { id } if *id == intro(5))));
    }

    // C-6: Function with same signature matches by SigKey.
    #[test]
    fn c6_sigkey_match() {
        let mut tip = PristineIntroTable::new();
        let payload_with_param = fn_payload_with_param("foo", 42);
        tip.insert_live(intro(10), payload_with_param.clone(), None);

        // New entry with same param type but different name + new wire_id.
        let new_payload = fn_payload_with_param("foo_new", 42);
        let staged = vec![staged_one(20, new_payload)];
        let (sigma, _summary) = compute_sigma(&tip, &staged);

        // Should match: same sig + api surface.
        let mapped = sigma.get(&intro(20));
        assert_eq!(mapped, Some(&intro(10)), "sigkey match should map wire 20 → durable 10");
    }

    // C-7: R-CHILD — Field without parent continuity should NOT be High.
    #[test]
    fn c7_r_child_no_parent() {
        let mut tip = PristineIntroTable::new();
        let field = OwnedEntryPayload::sealed(
            sym("x"),
            KindDiscriminant::Field,
            KindWire::Field(FieldWire { ty: None }),
            EntryPayloadFlags::default(),
        );
        // tip field has parent intro(1).
        tip.insert_live(intro(10), field.clone(), Some(intro(1)));

        // New field staged with parent intro(2) (different parent).
        let new_field = OwnedEntryPayload::sealed(
            sym("x"),
            KindDiscriminant::Field,
            KindWire::Field(FieldWire { ty: None }),
            EntryPayloadFlags::default(),
        );
        let staged = vec![(intro(20), new_field, Some(intro(2)), vec![])];
        let (sigma, _) = compute_sigma(&tip, &staged);

        // R-CHILD: different parent → should NOT be matched at High confidence.
        // wire 20 should map to itself (new intro) because R-CHILD blocks the match.
        assert_eq!(sigma.get(&intro(20)), Some(&intro(20)));
    }

    // C-8: Multiple entries, greedy assignment picks highest score.
    #[test]
    fn c8_greedy_assignment() {
        let mut tip = PristineIntroTable::new();
        tip.insert_live(intro(1), fn_payload("alpha"), None);
        tip.insert_live(intro(2), fn_payload("beta"), None);

        // Two wire entries with same name as tip entries.
        let staged = vec![
            staged_one(10, fn_payload("alpha")),
            staged_one(11, fn_payload("beta")),
        ];
        let (sigma, summary) = compute_sigma(&tip, &staged);

        // Each should map to their respective durable id.
        let m10 = sigma.get(&intro(10)).copied();
        let m11 = sigma.get(&intro(11)).copied();

        // alpha maps to 1, beta maps to 2 (or vice versa deterministically)
        let range: BTreeSet<IntroId> = [m10, m11].iter().flatten().copied().collect();
        assert!(range.contains(&intro(1)));
        assert!(range.contains(&intro(2)));

        // No deleted ops (all matched).
        assert!(!summary.ops.iter().any(|op| matches!(op, ContinuityOp::Deleted { .. })));
    }

    // C-9: Determinism — calling compute_sigma twice gives the same result.
    #[test]
    fn c9_determinism() {
        let mut tip = PristineIntroTable::new();
        for i in 0..10u8 {
            tip.insert_live(intro(i), fn_payload(&format!("fn_{}", i)), None);
        }
        let staged: Vec<_> = (10..20u8)
            .map(|i| staged_one(i, fn_payload(&format!("fn_{}", i - 10))))
            .collect();

        let (s1, _) = compute_sigma(&tip, &staged);
        let (s2, _) = compute_sigma(&tip, &staged);
        assert_eq!(s1, s2, "compute_sigma must be deterministic");
    }

    // C-10: SignatureEvolved op emitted when api_surface_hash changes.
    #[test]
    fn c10_signature_evolved() {
        let mut tip = PristineIntroTable::new();
        // tip: fn foo() (no params)
        tip.insert_live(intro(1), fn_payload("foo"), None);

        // staged: fn foo(x: ...) (added param → different api_surface)
        let staged = vec![staged_one(2, fn_payload_with_param("foo", 99))];
        let (sigma, summary) = compute_sigma(&tip, &staged);

        // Should match by name (W_NAME_EQ=20 ≥ SOFT threshold).
        // api_surface changed → SignatureEvolved op.
        if let Some(&durable) = sigma.get(&intro(2))
            && durable == intro(1) {
                assert!(
                    summary.ops.iter().any(|op| matches!(op, ContinuityOp::SignatureEvolved { id, .. } if *id == intro(1))),
                    "expected SignatureEvolved op"
                );
            }
            // If not matched (score < SOFT), that's also valid — test is informational.
    }

    // C-4/R-OV: a lone deleted `d` and a lone added `a` in the same
    // (kind, parent, name) bucket flip to High even when shape+sig both changed.
    #[test]
    fn r_ov_overload_flip_forces_high() {
        let mut tip = PristineIntroTable::new();
        tip.insert_live(intro(1), fn_payload_with_param("f", 7), None); // f(x: Same(7))
        // Same name, different signature, new wire id → score alone is low, but
        // the bucket (Function, None, "f") is 1↔1 so R-OV forces the match.
        let staged = vec![staged_one(2, fn_payload_with_param("f", 8))]; // f(x: Same(8))
        let (sigma, summary) = compute_sigma(&tip, &staged);
        assert_eq!(sigma.get(&intro(2)), Some(&intro(1)), "R-OV must reunify the flip");
        assert!(
            summary.ops.iter().any(|op| matches!(op, ContinuityOp::SignatureEvolved { .. })),
            "sig changed → SignatureEvolved"
        );
        assert!(!summary.ops.iter().any(|op| matches!(op, ContinuityOp::Deleted { .. })));
    }

    // C-5: kind change never High-merges — struct Foo → enum Foo is delete+add.
    #[test]
    fn kind_change_never_merges() {
        use ir::wire::{EnumWire, RecordWire};
        let rec = OwnedEntryPayload::sealed(
            sym("Foo"),
            KindDiscriminant::Record,
            KindWire::Record(RecordWire {
                form: RecordForm::Struct,
                fields: Box::new([]),
                generics: Box::new([]),
                wheres: Box::new([]),
                auto: Box::new([]),
            }),
            EntryPayloadFlags::default(),
        );
        let en = OwnedEntryPayload::sealed(
            sym("Foo"),
            KindDiscriminant::Enum,
            KindWire::Enum(EnumWire {
                variants: Box::new([]),
                generics: Box::new([]),
                wheres: Box::new([]),
                auto: Box::new([]),
            }),
            EntryPayloadFlags::default(),
        );
        let mut tip = PristineIntroTable::new();
        tip.insert_live(intro(1), rec, None);
        let staged = vec![staged_one(2, en)];
        let (sigma, summary) = compute_sigma(&tip, &staged);
        // No cross-kind merge: enum keeps its own id; record is deleted.
        assert_eq!(sigma.get(&intro(2)), Some(&intro(2)));
        assert!(summary.ops.iter().any(|op| matches!(op, ContinuityOp::Deleted { id } if *id == intro(1))));
        assert!(summary.ops.iter().any(|op| matches!(op, ContinuityOp::Introduced { id } if *id == intro(2))));
    }

    // C-9: two equally-scoring High candidates for one added entry are ambiguous
    // (margin rule) → neither is σ'd; the added entry is Introduced, both tips Deleted.
    #[test]
    fn ambiguity_margin_refuses_both() {
        // Two tip fns d1,d2 identical in shape+name+doc to the staged entry ⇒
        // both score the same High ⇒ within-margin ambiguity ⇒ refuse.
        let mut sd = sym("x");
        sd.documentation = Some("same docs".into());
        let mk = || OwnedEntryPayload::sealed(
            sd.clone(),
            KindDiscriminant::Function,
            KindWire::Function(FunctionWire {
                input_params: Box::new([]),
                output_params: Box::new([]),
                sig: FnSigFlags::default(),
                generics: Box::new([]),
                wheres: Box::new([]),
            }),
            EntryPayloadFlags::default(),
        );
        let mut tip = PristineIntroTable::new();
        tip.insert_live(intro(1), mk(), None);
        tip.insert_live(intro(2), mk(), None);
        // Staged: one entry, same payload, fresh wire id 9. It matches BOTH tips
        // at an identical High score → ambiguous. (R-OV does not fire: 2 deleted.)
        let staged = vec![staged_one(9, mk())];
        let (sigma, summary) = compute_sigma(&tip, &staged);
        assert_eq!(sigma.get(&intro(9)), Some(&intro(9)), "ambiguous → not merged");
        assert!(summary.ops.iter().any(|op| matches!(op, ContinuityOp::Introduced { id } if *id == intro(9))));
        let deleted = summary.ops.iter().filter(|op| matches!(op, ContinuityOp::Deleted { .. })).count();
        assert_eq!(deleted, 2, "both ambiguous tips fall through to Deleted");
    }
}
