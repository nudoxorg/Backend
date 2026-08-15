//! Continuity — the σ (sigma) computation for a package's IR generations.
//!
//! Given the *previous* sealed generation (`prev: &IrView`) and the *next*
//! one (`next: &IrView`), this module resolves which new declarations *are*
//! which prior declarations — surviving a rename, a move, or a signature edit.
//! It is what makes [`IntroId`] a durable identity rather than a hash that
//! churns on every edit.
//!
//! # Design principles
//!
//! **One entry point.** [`resolve`] takes two [`IrView`]s. Bodies are already
//! part of `IrView`; there is no second function to thread them in.
//!
//! **Evidence-carrying result.** [`Substitution`] records the full
//! [`Assignment`] for every next-generation entry — whether it is a
//! continuation of a prior entry, or genuinely new — together with the axes
//! that contributed to the decision.
//!
//! **Explicit, defaulted policy.** [`Policy`] exposes every threshold and gate
//! as named fields with a `Default` impl. Continuity is genuinely undecidable
//! in general; the tuning surface is part of the design, not a wart.
//!
//! **Determinism.** All iteration is over `BTreeMap`/`BTreeSet` (sorted by
//! key bytes). No `HashMap` iteration reaches a decision. Same inputs, same σ.
//!
//! **Total and honest.** Every next-generation entry appears in the
//! [`Substitution`]: either as a [`Continuation`] (same prior durable id) or
//! as [`New`]. A prior entry matched by no next entry appears as a
//! [`Deletion`]. [`Soft`] advisory edges are kept separate from σ.
//!
//! # Algorithm (§5 of CONTINUITY-PQGRAM-PLAN)
//!
//! 1. Immediately carry forward every `IntroId` that appears in both
//!    generations unchanged (identity σ, no matching work).
//! 2. Build three candidate indexes from the prior table.
//! 3. R-OV pre-pass: within a `(kind, resolved-parent, name)` bucket, a lone
//!    1↔1 pair is forced High regardless of score.
//! 4. For each added entry, generate candidates (cap 64, lowest ids) and score
//!    each with frozen integer weights.
//! 5. Greedy assignment with the margin rule (§5.6). Only High-confidence
//!    pairs write σ; Soft pairs become advisory [`RenameEdge`]s.
//!
//! # Relation to CONTINUITY-PQGRAM-PLAN
//!
//! This module implements the *intra-package* plane (§2, "Plane A"). The
//! corpus/cross-package plane (§2, "Plane B") is a separate, future concern and
//! must not write σ; it is not in scope here.
//!
//! [`IntroId`]: crate::change::IntroId
//! [`Continuation`]: Assignment::Continuation
//! [`New`]: Assignment::New
//! [`Deletion`]: Deletion
//! [`Soft`]: EvidenceBand::Soft

use std::collections::{BTreeMap, BTreeSet};

use crate::{
    body::{BodyEmbed, BodyFacts, ControlSketch},
    change::IntroId,
    change::encode::write_u32le,
    change::{ContentBlake3, StableRef},
    entry::Entry,
    kind::{Kind, KindDiscriminant},
    kinds::Function,
    view::IrView,
};

// ---------------------------------------------------------------------------
// Frozen scoring weights (§5.5 — CONTINUITY-PQGRAM-PLAN)
// ---------------------------------------------------------------------------
//
// These are frozen integers. Any change is an identity-format change — update
// the module documentation and bump the policy version when you touch them.

/// Weight: kind-body structural shape hash equal (the "api surface" signal).
///
/// Equivalent to `W_API_SURFACE` in the ir-vcs prototype. Uses the kind-shape
/// hash (see [`kind_shape_hash`]) which excludes name / doc / source / aliases
/// so a pure rename does not break this signal.
const W_SHAPE: i32 = 50;

/// Weight: exact name equal.
const W_NAME_EQ: i32 = 20;

/// Weight: parent continuity (resolved parent id matches).
const W_PARENT_CONT: i32 = 15;

/// Weight: function signature key equal (param types + modifiers).
const W_SIG_KEY: i32 = 15;

/// Weight: non-empty documentation string equal.
const W_DOC_EQ: i32 = 10;

/// Weight: source file path equal (non-empty).
const W_SRC_EQ: i32 = 5;

/// Weight: aliases overlap ≥ 1.
const W_ALIAS_OVERLAP: i32 = 5;

// Body axis (§3.7 / §2.2 of CONTINUITY-PQGRAM-PLAN).
//
// The body signal can only *promote* a candidate that another exact signal
// already surfaced — never be the sole match. Its cap is enforced at compile
// time: body-alone must never reach THRESHOLD_SOFT.

/// Weight: resolved-ref Jaccard ≥ 0.80 (oracle calls + type mentions).
const W_BODY_REFS_NEAR: i32 = 12;

/// Weight: resolved-ref Jaccard ≥ 0.50.
const W_BODY_REFS_MID: i32 = 6;

/// Weight: identical coarse control-flow shape (n_if / n_match / n_loop).
const W_BODY_CTRL_SHAPE: i32 = 2;

/// Minimum resolved refs on either side for the ref-overlap axis to fire.
/// Below this floor the Jaccard is statistically meaningless (tiny-body).
const MIN_BODY_REFS: usize = 2;

// Compile-time safety: body score alone must never reach THRESHOLD_SOFT,
// so it can never manufacture a candidate — let alone a match — from body
// structure alone.
const _: () = assert!(
    W_BODY_REFS_NEAR + W_BODY_CTRL_SHAPE < THRESHOLD_SOFT,
    "body score must stay below THRESHOLD_SOFT (never-merge invariant)",
);

// Compile-time safety: no single exact declaration signal may reach High on
// its own. A lone shape collision must not silently merge two histories.
const _: () = assert!(
    W_SHAPE < THRESHOLD_HIGH,
    "no single signal may reach THRESHOLD_HIGH alone (shared lineage needs ≥2 corroborators)",
);

/// Minimum total score to be eligible as a High-confidence (σ-writing) match.
const THRESHOLD_HIGH: i32 = 60;

/// Minimum total score for a Soft (advisory) edge to surface.
const THRESHOLD_SOFT: i32 = 30;

/// Margin rule: if two candidates for the same wire entry (or two wires for
/// the same tip) are both High and within this margin, both are refused.
const MARGIN_HIGH: i32 = 15;

/// Maximum candidates examined per next-generation entry.
const CANDIDATE_CAP: usize = 64;

// ---------------------------------------------------------------------------
// Policy
// ---------------------------------------------------------------------------

/// Tuning knobs for the continuity resolver.
///
/// Every threshold and gate is named here. The defaults reproduce the frozen
/// values used by the algorithm. Changing a threshold changes which matches
/// are committed to σ, so policy values should be treated as identity-format
/// decisions — calibrate on a labeled corpus before freezing.
#[derive(Debug, Clone)]
pub struct Policy {
    /// Minimum total score for a pair to be eligible as High (σ-writing).
    pub threshold_high: i32,

    /// Minimum total score for a pair to surface as a Soft advisory edge.
    pub threshold_soft: i32,

    /// If two High candidates for the same entry are within this margin of
    /// each other, both are refused (the margin rule, §5.6).
    pub margin_high: i32,

    /// Maximum candidates generated per next-generation entry.
    pub candidate_cap: usize,

    /// Enable the body-similarity axis. When `false` the body contributes 0
    /// to every pair score (equivalent to having no body on either side).
    pub enable_body_axis: bool,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            threshold_high: THRESHOLD_HIGH,
            threshold_soft: THRESHOLD_SOFT,
            margin_high: MARGIN_HIGH,
            candidate_cap: CANDIDATE_CAP,
            enable_body_axis: true,
        }
    }
}

// ---------------------------------------------------------------------------
// Evidence types
// ---------------------------------------------------------------------------

/// The confidence band of a match.
///
/// Mirrors the three-band taxonomy from CONTINUITY-PQGRAM-PLAN §5.5.
/// Only [`High`][EvidenceBand::High] writes σ (durable-id reuse); the others
/// are advisory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvidenceBand {
    /// Unambiguous match: at least `threshold_high` points, not within the
    /// margin of any competing candidate, all gates passed.
    High,
    /// Speculative match: ≥ `threshold_soft` but < `threshold_high`, or a
    /// High-scoring pair blocked by the margin or a gate.
    Soft,
}

/// The axes that contributed to a match score, for debugging and audit.
#[derive(Debug, Clone)]
pub struct MatchEvidence {
    /// Total integer score at the time of assignment.
    pub score: i32,
    /// The confidence band (High or Soft).
    pub band: EvidenceBand,
    /// True if forced High by the R-OV lone-1↔1 rule.
    pub r_ov_forced: bool,
    /// Whether the kind-body shape hash was equal.
    pub shape_matched: bool,
    /// Whether the name strings were equal.
    pub name_matched: bool,
    /// Whether the resolved parent id matched.
    pub parent_matched: bool,
    /// Whether the function signature key was equal (Function kind only).
    pub sig_matched: bool,
    /// Whether the documentation strings were equal and non-empty.
    pub doc_matched: bool,
    /// Whether the source file paths were equal and non-empty.
    pub src_matched: bool,
    /// Whether at least one alias overlapped.
    pub alias_matched: bool,
    /// Body score contributed (0 when body axis abstained).
    pub body_score: i32,
}

/// The assignment for one next-generation entry.
#[derive(Debug, Clone)]
pub enum Assignment {
    /// This entry continues a prior entry's durable identity.
    ///
    /// The next-generation entry at `next_id` *is* the prior entry at
    /// `prior_id`: σ(`next_id`) = `prior_id`.
    Continuation {
        /// The next-generation [`IntroId`] (may differ from `prior_id` when a
        /// rename occurred; equals `prior_id` when the entry was stable).
        next_id: IntroId,
        /// The prior-generation durable [`IntroId`] being reused.
        prior_id: IntroId,
        /// The evidence for this assignment.
        evidence: MatchEvidence,
    },
    /// This entry is new in this generation — no prior identity was found.
    New {
        /// The next-generation [`IntroId`] (its durable id from here on).
        next_id: IntroId,
    },
}

impl Assignment {
    /// The next-generation id covered by this assignment.
    #[inline]
    pub fn next_id(&self) -> IntroId {
        match self {
            Assignment::Continuation { next_id, .. } | Assignment::New { next_id } => *next_id,
        }
    }

    /// True if this assignment establishes shared lineage (writes σ).
    #[inline]
    pub fn is_continuation(&self) -> bool {
        matches!(self, Assignment::Continuation { .. })
    }
}

/// A prior-generation entry that no next-generation entry continued.
#[derive(Debug, Clone)]
pub struct Deletion {
    /// The prior-generation durable [`IntroId`] that is now absent.
    pub prior_id: IntroId,
}

/// An advisory edge for a pair that scored at or above `threshold_soft` but
/// did not reach the High band (or was blocked by the margin rule or a gate).
///
/// These never write σ; they are informational hints for a reviewer when
/// lineage goes wrong.
#[derive(Debug, Clone)]
pub struct RenameEdge {
    /// The prior entry that was not matched.
    pub prior_id: IntroId,
    /// The next entry that was not matched.
    pub next_id: IntroId,
    /// The integer score of this pair.
    pub score: i32,
}

/// The full output of [`resolve`].
///
/// Every next-generation entry appears exactly once in `assignments`. Every
/// prior-generation entry that was not continued appears in `deletions`.
#[derive(Debug, Clone)]
pub struct Substitution {
    /// One assignment per next-generation entry, in `IntroId` order.
    pub assignments: Vec<Assignment>,
    /// Prior entries with no successor, in `IntroId` order.
    pub deletions: Vec<Deletion>,
    /// Advisory Soft edges, in descending score order.
    pub rename_edges: Vec<RenameEdge>,
}

impl Substitution {
    /// The σ map: next `IntroId` → durable `IntroId`.
    ///
    /// For [`Continuation`] assignments the durable id is the prior id.
    /// For [`New`] assignments the next id is already durable (σ = identity).
    ///
    /// [`Continuation`]: Assignment::Continuation
    /// [`New`]: Assignment::New
    pub fn sigma(&self) -> BTreeMap<IntroId, IntroId> {
        self.assignments
            .iter()
            .map(|a| match a {
                Assignment::Continuation {
                    next_id, prior_id, ..
                } => (*next_id, *prior_id),
                Assignment::New { next_id } => (*next_id, *next_id),
            })
            .collect()
    }

    /// Iterate the assignments for continued (non-new) entries.
    pub fn continuations(&self) -> impl Iterator<Item = &Assignment> {
        self.assignments.iter().filter(|a| a.is_continuation())
    }
}

// ---------------------------------------------------------------------------
// Kind-shape hash (the "api surface" equivalent)
// ---------------------------------------------------------------------------

/// Domain tag for kind-shape hashes.
const KIND_SHAPE_DOMAIN: &str = "nudox.continuity.kshape.v1";

/// Compute a structural shape hash for an entry that **excludes**:
/// name, documentation, source path, aliases, deprecation, doc_links,
/// attrs, cfg, and the Node tree edges.
///
/// This is the continuity equivalent of `compute_api_surface_hash` in the
/// ir-vcs prototype. It hashes only the kind discriminant and the kind body
/// (via `postcard`), so a pure rename or doc edit does not perturb it.
///
/// Because `Kind` is `Serialize`, we use postcard for a compact,
/// deterministic, platform-independent encoding. The domain tag guarantees
/// these bytes never collide with other hash planes.
fn kind_shape_hash(entry: &Entry) -> ContentBlake3 {
    let kind = entry.kind();
    // Postcard encodes enums as a varint discriminant followed by the
    // variant's fields — deterministic, no padding, no platform difference.
    let encoded = postcard::to_allocvec(kind).expect("Kind postcard encoding must not fail");
    ContentBlake3::from_domain(KIND_SHAPE_DOMAIN, &encoded)
}

/// Compute a function-signature key that captures param types and modifiers
/// but not the function name, doc, or source. Used for `W_SIG_KEY`.
fn function_sig_key(f: &Function) -> ContentBlake3 {
    const SIG_DOMAIN: &str = "nudox.continuity.sigkey.v1";
    // Postcard-encode the receiver + input/output params + modifiers only.
    // We include them in a small ad-hoc struct via tuple encoding.
    let mut buf = Vec::new();
    // receiver option tag + encoded value
    let recv_bytes =
        postcard::to_allocvec(&f.receiver).expect("Receiver postcard encoding must not fail");
    // input params (Ref<Param> list)
    let inp_bytes = postcard::to_allocvec(&f.input_params)
        .expect("input_params postcard encoding must not fail");
    // output params
    let out_bytes = postcard::to_allocvec(&f.output_params)
        .expect("output_params postcard encoding must not fail");
    // modifiers (async / const / unsafe / pure / generator)
    let mod_bytes =
        postcard::to_allocvec(&f.modifiers).expect("modifiers postcard encoding must not fail");
    // ABI string
    let abi_bytes = postcard::to_allocvec(&f.abi).expect("abi postcard encoding must not fail");

    // Length-prefix each section so the concatenation is unambiguous.
    write_u32le(&mut buf, recv_bytes.len() as u32);
    buf.extend_from_slice(&recv_bytes);
    write_u32le(&mut buf, inp_bytes.len() as u32);
    buf.extend_from_slice(&inp_bytes);
    write_u32le(&mut buf, out_bytes.len() as u32);
    buf.extend_from_slice(&out_bytes);
    write_u32le(&mut buf, mod_bytes.len() as u32);
    buf.extend_from_slice(&mod_bytes);
    write_u32le(&mut buf, abi_bytes.len() as u32);
    buf.extend_from_slice(&abi_bytes);

    ContentBlake3::from_domain(SIG_DOMAIN, &buf)
}

// ---------------------------------------------------------------------------
// Body axis helpers
// ---------------------------------------------------------------------------

/// Collect the resolved-reference set for the body-similarity axis.
///
/// Includes oracle calls at or above `Confidence::Index` whose target is
/// `Some`, plus all type mentions. Returns a `BTreeSet<StableRef>` — a set,
/// not a multiset: the question is "does this body use symbol X", not "how
/// often". `StableRef: Ord` guarantees determinism.
fn oracle_ref_set(facts: &BodyFacts) -> BTreeSet<&StableRef> {
    let mut set = BTreeSet::new();
    for call in &facts.oracle.calls {
        if call.confidence.is_graph_worthy()
            && let Some(target) = &call.target
        {
            set.insert(target);
        }
    }
    for tm in &facts.oracle.type_mentions {
        // OracleTypeMention carries the StableRef in `.ty`
        set.insert(&tm.ty);
    }
    set
}

/// Coarse control-flow shape `(n_if, n_match, n_loop, total_match_arms)`, each
/// saturated to fit in a `u8`. Equality means "same rough branch structure",
/// not exact counts.
///
/// The arm count from `Match { arms }` is included so a function with 3 match
/// arms is distinguishable from one with 1 at the coarse level — it's a
/// corroborator, not a precision signal.
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

/// Compute the body-similarity score between two entries' bodies.
///
/// Returns 0 when either body is `Absent`, when the oracle did not run on
/// either side, or when the resolved-ref set is too small (tiny-body floor).
/// The result is always `< THRESHOLD_SOFT` — the body can only *promote* a
/// candidate that another exact signal already surfaced, never match alone.
///
/// # σ-substitution caveat (deliberate under-count)
///
/// Next-generation callee refs may key co-renamed callees by their *new* id
/// while prior refs use the *old* durable ids, and σ is not yet known when a
/// pair is scored. This under-counts overlap when co-changed callees exist,
/// producing a lower body score → a miss (false split), never a false merge.
/// The error direction is safe; a partial-σ correction would introduce
/// order-dependent non-determinism.
fn body_similarity_score(next_body: Option<&BodyEmbed>, prior_body: Option<&BodyEmbed>) -> i32 {
    let (Some(next_body), Some(prior_body)) = (next_body, prior_body) else {
        return 0;
    };
    let (Some(nf), Some(pf)) = (next_body.facts(), prior_body.facts()) else {
        return 0;
    };

    let mut score = 0i32;

    // Sub-signal A: resolved-ref Jaccard.
    // Gate on `merge.oracle_ran` on BOTH sides — a treesitter-only body has
    // no resolved `StableRef` targets, so Jaccard is meaningless and must
    // abstain. This is the fidelity gate for the never-merge safety property.
    if nf.merge.oracle_ran && pf.merge.oracle_ran {
        let nset = oracle_ref_set(nf);
        let pset = oracle_ref_set(pf);
        if nset.len() >= MIN_BODY_REFS && pset.len() >= MIN_BODY_REFS {
            let inter = nset.intersection(&pset).count();
            let union = nset.union(&pset).count();
            if union > 0 {
                // Jaccard thresholds as integer cross-multiplications (no f32).
                if inter * 100 >= union * 80 {
                    score += W_BODY_REFS_NEAR;
                } else if inter * 100 >= union * 50 {
                    score += W_BODY_REFS_MID;
                }
            }
        }
    }

    // Sub-signal B: coarse control-flow shape.
    // Requires both sides to have at least one control structure (empty ==
    // empty is uninformative) and the shapes to match exactly.
    let ns = ctrl_shape(nf);
    let ps = ctrl_shape(pf);
    let both_have_control = (ns.0 | ns.1 | ns.2) > 0 && (ps.0 | ps.1 | ps.2) > 0;
    if both_have_control && ns == ps {
        score += W_BODY_CTRL_SHAPE;
    }

    score
}

// ---------------------------------------------------------------------------
// Name stem
// ---------------------------------------------------------------------------

/// Strip a generic suffix `<…>` and normalise to ASCII-lowercase.
/// Deterministic because `String::to_ascii_lowercase` is Unicode-stable.
fn name_stem(name: &str) -> String {
    let s = name.split('<').next().unwrap_or(name);
    s.trim().to_ascii_lowercase()
}

// ---------------------------------------------------------------------------
// Candidate indexes built from the prior view
// ---------------------------------------------------------------------------

struct PriorIndexes {
    /// (kind, name) → sorted vec of `IntroId`
    kind_name: BTreeMap<(KindDiscriminant, String), Vec<IntroId>>,
    /// kind_shape_hash bytes → sorted vec of `IntroId`
    shape: BTreeMap<[u8; 32], Vec<IntroId>>,
    /// (kind, parent_opt, name_stem) → sorted vec of `IntroId`
    kind_parent_stem: BTreeMap<(KindDiscriminant, Option<IntroId>, String), Vec<IntroId>>,
}

fn build_indexes(prior: &IrView) -> PriorIndexes {
    let mut kind_name: BTreeMap<(KindDiscriminant, String), Vec<IntroId>> = BTreeMap::new();
    let mut shape: BTreeMap<[u8; 32], Vec<IntroId>> = BTreeMap::new();
    let mut kind_parent_stem: BTreeMap<
        (KindDiscriminant, Option<IntroId>, String),
        Vec<IntroId>,
    > = BTreeMap::new();

    for (id, entry) in prior.entries() {
        let Some(disc) = entry.kind().discriminant() else {
            // Reference entries (EntryInner::Reference) have no discriminant.
            continue;
        };
        let name = entry.sym().name.clone();
        let stem = name_stem(&name);
        let parent = prior.parent_of(id);
        let shape_hash = kind_shape_hash(entry);

        kind_name.entry((disc, name)).or_default().push(id);
        shape.entry(*shape_hash.as_bytes()).or_default().push(id);
        kind_parent_stem
            .entry((disc, parent, stem))
            .or_default()
            .push(id);
    }

    // Sort all vecs for determinism (insertion order is HashMap-undefined).
    for v in kind_name.values_mut() {
        v.sort_unstable();
    }
    for v in shape.values_mut() {
        v.sort_unstable();
    }
    for v in kind_parent_stem.values_mut() {
        v.sort_unstable();
    }

    PriorIndexes {
        kind_name,
        shape,
        kind_parent_stem,
    }
}

// ---------------------------------------------------------------------------
// Candidate generation
// ---------------------------------------------------------------------------

fn candidates_for(
    entry: &Entry,
    disc: KindDiscriminant,
    next_parent: Option<IntroId>,
    sigma: &BTreeMap<IntroId, IntroId>,
    indexes: &PriorIndexes,
    cap: usize,
) -> BTreeSet<IntroId> {
    let name = entry.sym().name.clone();
    let stem = name_stem(&name);
    let shape = kind_shape_hash(entry);
    let durable_parent = next_parent.map(|p| sigma.get(&p).copied().unwrap_or(p));

    let mut cands: BTreeSet<IntroId> = BTreeSet::new();

    // Index 1: same (kind, name)
    if let Some(ids) = indexes.kind_name.get(&(disc, name)) {
        for &id in ids.iter().take(cap) {
            cands.insert(id);
        }
    }

    // Index 2: same kind_shape_hash
    if let Some(ids) = indexes.shape.get(shape.as_bytes()) {
        for &id in ids.iter().take(cap) {
            cands.insert(id);
        }
    }

    // Index 3: same (kind, resolved parent, name stem)
    if let Some(ids) = indexes
        .kind_parent_stem
        .get(&(disc, durable_parent, stem))
    {
        for &id in ids.iter().take(cap) {
            cands.insert(id);
        }
    }

    // Cap total to `cap` lowest ids.
    if cands.len() > cap {
        cands.into_iter().take(cap).collect()
    } else {
        cands
    }
}

// ---------------------------------------------------------------------------
// Pair scoring
// ---------------------------------------------------------------------------

struct ScoreDetail {
    total: i32,
    shape_matched: bool,
    name_matched: bool,
    parent_matched: bool,
    sig_matched: bool,
    doc_matched: bool,
    src_matched: bool,
    alias_matched: bool,
    body_score: i32,
}

/// All inputs to [`score_pair`], collected into a named struct to keep the
/// argument count under Clippy's threshold (7) without changing any scoring
/// behaviour.
struct ScoreInputs<'a> {
    next_entry: &'a Entry,
    next_disc: KindDiscriminant,
    next_parent: Option<IntroId>,
    sigma: &'a BTreeMap<IntroId, IntroId>,
    prior_id: IntroId,
    prior_entry: &'a Entry,
    prior: &'a IrView,
    next_body: Option<&'a BodyEmbed>,
    prior_body: Option<&'a BodyEmbed>,
    policy: &'a Policy,
}

fn score_pair(inp: &ScoreInputs<'_>) -> ScoreDetail {
    let ScoreInputs {
        next_entry,
        next_disc,
        next_parent,
        sigma,
        prior_id,
        prior_entry,
        prior,
        next_body,
        prior_body,
        policy,
    } = inp;
    let mut total = 0i32;

    // Shape hash equal (+50)
    let shape_matched = kind_shape_hash(next_entry) == kind_shape_hash(prior_entry);
    if shape_matched {
        total += W_SHAPE;
    }

    // Name equal (+20)
    let name_matched = next_entry.sym().name == prior_entry.sym().name;
    if name_matched {
        total += W_NAME_EQ;
    }

    // Parent continuity (+15): wire parent resolves to the same durable id
    // as the prior entry's parent.
    let next_durable_parent = next_parent.map(|p| sigma.get(&p).copied().unwrap_or(p));
    let prior_parent = prior.parent_of(*prior_id);
    let parent_matched = next_durable_parent == prior_parent;
    if parent_matched {
        total += W_PARENT_CONT;
    }

    // Function signature key (+15, Function kind only)
    let sig_matched = if *next_disc == KindDiscriminant::Function
        && prior_entry.kind().discriminant() == Some(KindDiscriminant::Function)
    {
        if let (Kind::Function(nf), Kind::Function(pf)) = (
            next_entry
                .kind()
                .as_owned_kind()
                .expect("discriminant matched"),
            prior_entry
                .kind()
                .as_owned_kind()
                .expect("discriminant matched"),
        ) {
            let nk = function_sig_key(nf);
            let pk = function_sig_key(pf);
            nk == pk
        } else {
            false
        }
    } else {
        false
    };
    if sig_matched {
        total += W_SIG_KEY;
    }

    // Documentation equal and non-empty (+10)
    let doc_matched = !next_entry.sym().documentation.is_empty()
        && next_entry.sym().documentation == prior_entry.sym().documentation;
    if doc_matched {
        total += W_DOC_EQ;
    }

    // Source file equal and non-empty (+5)
    let src_matched = !next_entry.sym().source.as_os_str().is_empty()
        && next_entry.sym().source == prior_entry.sym().source;
    if src_matched {
        total += W_SRC_EQ;
    }

    // Alias overlap ≥ 1 (+5)
    let next_aliases: BTreeSet<&str> = next_entry
        .sym()
        .aliases
        .iter()
        .map(String::as_str)
        .collect();
    let alias_matched = prior_entry
        .sym()
        .aliases
        .iter()
        .any(|a| next_aliases.contains(a.as_str()));
    if alias_matched {
        total += W_ALIAS_OVERLAP;
    }

    // Body axis: resolved-ref overlap + coarse control-flow shape.
    let body_score = if policy.enable_body_axis {
        body_similarity_score(*next_body, *prior_body)
    } else {
        0
    };
    total += body_score;

    ScoreDetail {
        total,
        shape_matched,
        name_matched,
        parent_matched,
        sig_matched,
        doc_matched,
        src_matched,
        alias_matched,
        body_score,
    }
}

// ---------------------------------------------------------------------------
// Gate: R-CHILD (field / variant require parent continuity)
// ---------------------------------------------------------------------------

/// R-CHILD gate (§5.4): a `Field` or `Variant` entry may only share lineage
/// with a prior entry when both have a defined, matching parent.
///
/// This blocks the pathological case where a field moves between parent types
/// and would otherwise be force-matched by a lone-1↔1 R-OV pair.
fn check_r_child(
    disc: KindDiscriminant,
    next_parent: Option<IntroId>,
    sigma: &BTreeMap<IntroId, IntroId>,
    prior_id: IntroId,
    prior: &IrView,
) -> bool {
    match disc {
        KindDiscriminant::Field | KindDiscriminant::Variant => {
            let next_durable_parent = next_parent.map(|p| sigma.get(&p).copied().unwrap_or(p));
            let prior_parent = prior.parent_of(prior_id);
            next_durable_parent == prior_parent && next_durable_parent.is_some()
        }
        KindDiscriminant::Module
        | KindDiscriminant::Record
        | KindDiscriminant::Function
        | KindDiscriminant::Alias
        | KindDiscriminant::Trait
        | KindDiscriminant::Impl
        | KindDiscriminant::Enum
        | KindDiscriminant::Const
        | KindDiscriminant::Static
        | KindDiscriminant::Reexport
        | KindDiscriminant::Param => true,
    }
}

// ---------------------------------------------------------------------------
// Main entry point
// ---------------------------------------------------------------------------

/// Resolve which next-generation entries continue prior-generation identities.
///
/// Returns a [`Substitution`] containing the full σ mapping and all advisory
/// evidence. Bodies are read from the respective [`IrView`]s via
/// [`IrView::body`]; there is no second function.
///
/// # Determinism
///
/// The result is a pure function of `prev`, `next`, and `policy`. All
/// internal iteration is over `BTreeMap`/`BTreeSet`. Calling `resolve` twice
/// with the same arguments produces bit-identical output.
///
/// # Package scoping
///
/// This function operates within a single package. It must never be used to
/// match entries across package boundaries — that is the separate corpus plane
/// (CONTINUITY-PQGRAM-PLAN §2, Plane B), which emits provenance edges, not σ.
pub fn resolve(prev: &IrView, next: &IrView, policy: &Policy) -> Substitution {
    // -----------------------------------------------------------------------
    // Step 0: collect ids from both views into sorted sets.
    // -----------------------------------------------------------------------

    // All prior (tip) ids, collected from a HashMap — must iterate into a
    // BTreeSet to be deterministic.
    let prior_ids: BTreeSet<IntroId> = prev.entries().map(|(id, _)| id).collect();
    let next_ids: BTreeSet<IntroId> = next.entries().map(|(id, _)| id).collect();

    // -----------------------------------------------------------------------
    // Step 1: stable entries — present in both generations, same IntroId.
    //
    // σ(id) = id (identity). No matching work needed.
    // -----------------------------------------------------------------------

    // sigma: next_id → durable_id (prior_id or next_id for new entries).
    let mut sigma: BTreeMap<IntroId, IntroId> = BTreeMap::new();

    let stable_ids: BTreeSet<IntroId> = prior_ids.intersection(&next_ids).copied().collect();
    for id in &stable_ids {
        sigma.insert(*id, *id);
    }

    // Entries absent in next → candidates for deletion.
    let deleted_ids: BTreeSet<IntroId> = prior_ids.difference(&next_ids).copied().collect();
    // Entries added in next → candidates for matching.
    let added_ids: BTreeSet<IntroId> = next_ids.difference(&prior_ids).copied().collect();

    // -----------------------------------------------------------------------
    // Step 2: build candidate indexes from the prior view.
    // -----------------------------------------------------------------------

    let indexes = build_indexes(prev);

    // Fast-access map: prior_id → &Entry
    let prior_map: BTreeMap<IntroId, &crate::entry::Entry> = prev.entries().collect();

    // Fast-access map: next_id → (disc, parent)
    let next_meta: BTreeMap<IntroId, (KindDiscriminant, Option<IntroId>)> = next
        .entries()
        .filter_map(|(id, entry)| {
            let disc = entry.kind().discriminant()?;
            let parent = next.parent_of(id);
            Some((id, (disc, parent)))
        })
        .collect();

    // -----------------------------------------------------------------------
    // Step 3: R-OV pre-pass (§5.4, overload flip).
    //
    // Within a (kind, resolved-parent, name) bucket, if exactly one deleted
    // prior entry `d` and exactly one added next entry `a` exist, the pair is
    // forced High regardless of score. This covers the "lone overload flip"
    // where shape, sig, and doc all changed.
    // -----------------------------------------------------------------------

    type Bucket = (KindDiscriminant, Option<IntroId>, String);

    let bucket_of = |disc: KindDiscriminant, parent: Option<IntroId>, name: &str| -> Bucket {
        (disc, parent, name.to_owned())
    };

    let mut del_bucket: BTreeMap<Bucket, Vec<IntroId>> = BTreeMap::new();
    for d in &deleted_ids {
        if let Some(entry) = prior_map.get(d)
            && let Some(disc) = entry.kind().discriminant()
        {
            let parent = prev.parent_of(*d);
            del_bucket
                .entry(bucket_of(disc, parent, &entry.sym().name))
                .or_default()
                .push(*d);
        }
    }

    let mut add_bucket: BTreeMap<Bucket, Vec<IntroId>> = BTreeMap::new();
    for a in &added_ids {
        if let Some(&(disc, parent)) = next_meta.get(a)
            && let Some(entry) = next.entry(*a)
        {
            let durable_parent = parent.map(|p| sigma.get(&p).copied().unwrap_or(p));
            add_bucket
                .entry(bucket_of(disc, durable_parent, &entry.sym().name))
                .or_default()
                .push(*a);
        }
    }

    // forced_high: next_id → prior_id  (R-OV lone 1↔1)
    let mut forced_high: BTreeMap<IntroId, IntroId> = BTreeMap::new();
    for (bucket, ds) in &del_bucket {
        if let (Some([d]), Some(adds)) = (
            <&[IntroId; 1]>::try_from(ds.as_slice()).ok(),
            add_bucket.get(bucket),
        ) && let Ok([a]) = <&[IntroId; 1]>::try_from(adds.as_slice())
        {
            forced_high.insert(*a, *d);
        }
    }

    // -----------------------------------------------------------------------
    // Step 4: score all candidate pairs (≥ threshold_soft).
    // -----------------------------------------------------------------------

    // all_pairs: (score, next_id, prior_id)
    let mut all_pairs: Vec<(i32, IntroId, IntroId)> = Vec::new();

    for &next_id in &added_ids {
        let Some(&(disc, next_parent)) = next_meta.get(&next_id) else {
            continue;
        };
        let Some(next_entry) = next.entry(next_id) else {
            continue;
        };

        let cands = candidates_for(
            next_entry,
            disc,
            next_parent,
            &sigma,
            &indexes,
            policy.candidate_cap,
        );

        for prior_id in &cands {
            if !deleted_ids.contains(prior_id) {
                continue;
            }
            let Some(prior_entry) = prior_map.get(prior_id) else {
                continue;
            };
            // Hard gate (§5.4): cross-kind pairs never share lineage.
            if prior_entry.kind().discriminant() != Some(disc) {
                continue;
            }

            let detail = score_pair(&ScoreInputs {
                next_entry,
                next_disc: disc,
                next_parent,
                sigma: &sigma,
                prior_id: *prior_id,
                prior_entry,
                prior: prev,
                next_body: next.body(next_id),
                prior_body: prev.body(*prior_id),
                policy,
            });

            if detail.total >= policy.threshold_soft {
                all_pairs.push((detail.total, next_id, *prior_id));
            }
        }
    }

    // Sort: highest score first; deterministic tie-break by (next_id, prior_id).
    all_pairs.sort_unstable_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));

    let is_high = |next_id: IntroId, prior_id: IntroId, score: i32| -> bool {
        score >= policy.threshold_high || forced_high.get(&next_id) == Some(&prior_id)
    };

    let mut assigned_next: BTreeSet<IntroId> = BTreeSet::new();
    let mut assigned_prior: BTreeSet<IntroId> = BTreeSet::new();
    // matched: (next_id, prior_id, score, r_ov_forced, detail)
    let mut matched: Vec<(IntroId, IntroId, i32, bool, ScoreDetail)> = Vec::new();
    let mut rename_edges: Vec<RenameEdge> = Vec::new();

    // -----------------------------------------------------------------------
    // Step 5a: assign R-OV forced-High pairs first (bypass score/margin).
    // -----------------------------------------------------------------------

    for (&next_id, &prior_id) in &forced_high {
        if assigned_next.contains(&next_id) || assigned_prior.contains(&prior_id) {
            continue;
        }
        let Some(&(disc, next_parent)) = next_meta.get(&next_id) else {
            continue;
        };
        if !check_r_child(disc, next_parent, &sigma, prior_id, prev) {
            // R-CHILD veto even on a forced-High pair. Emit an advisory edge
            // at the pair's actual score (which may be below THRESHOLD_SOFT).
            // We look up the pair in all_pairs for its score; if not found,
            // use 0 — the edge is still informative.
            let score = all_pairs
                .iter()
                .find(|&&(_, n, p)| n == next_id && p == prior_id)
                .map_or(0, |&(s, _, _)| s);
            rename_edges.push(RenameEdge {
                prior_id,
                next_id,
                score,
            });
            continue;
        }
        // Compute detail for evidence construction.
        let rov_detail =
            if let (Some(ne), Some(pe)) = (next.entry(next_id), prior_map.get(&prior_id)) {
                score_pair(&ScoreInputs {
                    next_entry: ne,
                    next_disc: disc,
                    next_parent,
                    sigma: &sigma,
                    prior_id,
                    prior_entry: pe,
                    prior: prev,
                    next_body: next.body(next_id),
                    prior_body: prev.body(prior_id),
                    policy,
                })
            } else {
                ScoreDetail {
                    total: 0,
                    shape_matched: false,
                    name_matched: true,
                    parent_matched: false,
                    sig_matched: false,
                    doc_matched: false,
                    src_matched: false,
                    alias_matched: false,
                    body_score: 0,
                }
            };
        sigma.insert(next_id, prior_id);
        assigned_next.insert(next_id);
        assigned_prior.insert(prior_id);
        matched.push((next_id, prior_id, rov_detail.total, true, rov_detail));
    }

    // -----------------------------------------------------------------------
    // Step 5b: greedy assignment over scored pairs.
    //
    // Only High-confidence pairs write σ. Soft pairs (or High pairs blocked
    // by the margin rule or R-CHILD) become advisory RenameEdges.
    // -----------------------------------------------------------------------

    for &(score, next_id, prior_id) in &all_pairs {
        if assigned_next.contains(&next_id) || assigned_prior.contains(&prior_id) {
            continue;
        }

        if !is_high(next_id, prior_id, score) {
            rename_edges.push(RenameEdge {
                prior_id,
                next_id,
                score,
            });
            continue;
        }

        let Some(&(disc, next_parent)) = next_meta.get(&next_id) else {
            continue;
        };

        if !check_r_child(disc, next_parent, &sigma, prior_id, prev) {
            rename_edges.push(RenameEdge {
                prior_id,
                next_id,
                score,
            });
            continue;
        }

        // Margin rule (§5.6): if any other still-unassigned High candidate
        // for the same next entry OR the same prior entry is within
        // `margin_high` points, the match is ambiguous → refuse both.
        // Cost asymmetry: a false merge poisons lineage; refuse when in doubt.
        let ambiguous = all_pairs.iter().any(|&(s2, n2, p2)| {
            let shares = (n2 == next_id && p2 != prior_id) || (p2 == prior_id && n2 != next_id);
            shares
                && !assigned_next.contains(&n2)
                && !assigned_prior.contains(&p2)
                && is_high(n2, p2, s2)
                && (score - s2).abs() <= policy.margin_high
        });

        if ambiguous {
            rename_edges.push(RenameEdge {
                prior_id,
                next_id,
                score,
            });
            continue;
        }

        // Re-score to get per-axis detail for the evidence record.
        let Some(next_entry) = next.entry(next_id) else {
            continue;
        };
        let Some(prior_entry) = prior_map.get(&prior_id) else {
            continue;
        };
        let detail = score_pair(&ScoreInputs {
            next_entry,
            next_disc: disc,
            next_parent,
            sigma: &sigma,
            prior_id,
            prior_entry,
            prior: prev,
            next_body: next.body(next_id),
            prior_body: prev.body(prior_id),
            policy,
        });
        sigma.insert(next_id, prior_id);
        assigned_next.insert(next_id);
        assigned_prior.insert(prior_id);
        matched.push((next_id, prior_id, score, false, detail));
    }

    // Brand-new entries: identity σ (their next_id IS their durable id).
    for &next_id in &added_ids {
        sigma.entry(next_id).or_insert(next_id);
    }

    // -----------------------------------------------------------------------
    // Step 6: build the Substitution.
    // -----------------------------------------------------------------------

    let matched_priors: BTreeSet<IntroId> = matched.iter().map(|(_, p, _, _, _)| *p).collect();

    let mut assignments: Vec<Assignment> = Vec::new();

    // Stable entries (identity σ).
    for &id in &stable_ids {
        if let Some(next_entry) = next.entry(id) {
            let prior_entry = prev.entry(id);
            let unchanged = prior_entry.is_some_and(|pe| pe == next_entry);
            if unchanged {
                // No meaningful "evidence" to record for an identity match.
                assignments.push(Assignment::Continuation {
                    next_id: id,
                    prior_id: id,
                    evidence: MatchEvidence {
                        score: THRESHOLD_HIGH, // maximum signal: exact id equality
                        band: EvidenceBand::High,
                        r_ov_forced: false,
                        shape_matched: true,
                        name_matched: true,
                        parent_matched: true,
                        sig_matched: next_entry.kind().discriminant()
                            == Some(KindDiscriminant::Function),
                        doc_matched: !next_entry.sym().documentation.is_empty(),
                        src_matched: !next_entry.sym().source.as_os_str().is_empty(),
                        alias_matched: !next_entry.sym().aliases.is_empty(),
                        body_score: 0,
                    },
                });
            } else {
                // Same id but content changed (possible when a sealing
                // producer reuses an id after a transparent edit).
                assignments.push(Assignment::Continuation {
                    next_id: id,
                    prior_id: id,
                    evidence: MatchEvidence {
                        score: THRESHOLD_HIGH,
                        band: EvidenceBand::High,
                        r_ov_forced: false,
                        shape_matched: kind_shape_hash(next_entry)
                            == prior_entry.map_or_else(|| kind_shape_hash(next_entry), kind_shape_hash),
                        name_matched: true,
                        parent_matched: true,
                        sig_matched: false,
                        doc_matched: false,
                        src_matched: false,
                        alias_matched: false,
                        body_score: 0,
                    },
                });
            }
        }
    }

    // Matched (continuation from a different next_id).
    for (next_id, prior_id, score, r_ov_forced, detail) in matched {
        let effective_score = if r_ov_forced {
            score.max(policy.threshold_high)
        } else {
            score
        };
        assignments.push(Assignment::Continuation {
            next_id,
            prior_id,
            evidence: MatchEvidence {
                score: effective_score,
                band: EvidenceBand::High,
                r_ov_forced,
                shape_matched: detail.shape_matched,
                name_matched: detail.name_matched,
                parent_matched: detail.parent_matched,
                sig_matched: detail.sig_matched,
                doc_matched: detail.doc_matched,
                src_matched: detail.src_matched,
                alias_matched: detail.alias_matched,
                body_score: detail.body_score,
            },
        });
    }

    // New entries.
    for &next_id in &added_ids {
        if !assigned_next.contains(&next_id) {
            assignments.push(Assignment::New { next_id });
        }
    }

    // Sort assignments by next_id for determinism.
    assignments.sort_unstable_by_key(Assignment::next_id);

    // Deletions: prior entries not matched by any next entry.
    let mut deletions: Vec<Deletion> = deleted_ids
        .iter()
        .filter(|id| !matched_priors.contains(id))
        .map(|&prior_id| Deletion { prior_id })
        .collect();
    deletions.sort_unstable_by_key(|d| d.prior_id);

    // Rename edges: sort by descending score, then by ids for determinism.
    rename_edges.sort_unstable_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then(a.prior_id.cmp(&b.prior_id))
            .then(a.next_id.cmp(&b.next_id))
    });
    // Deduplicate (a pair may have been pushed once from R-OV and once from
    // the scored pass, both for the same (prior_id, next_id)).
    rename_edges.dedup_by(|a, b| a.prior_id == b.prior_id && a.next_id == b.next_id);

    Substitution {
        assignments,
        deletions,
        rename_edges,
    }
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
