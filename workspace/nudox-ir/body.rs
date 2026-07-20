//! The implementation (body) plane: merged tree-sitter + oracle facts attached
//! to a single owning entry.
//!
//! # Where this sits
//!
//! The declaration plane ([`crate::wire::OwnedEntryPayload`]) answers *what a
//! symbol is*. This module answers *what a symbol does*: the union of what
//! tree-sitter saw structurally and what the language oracle resolved
//! semantically, both attached to the one entry that owns them, both keyed by
//! spans **relative** to that entry's declaration span start.
//!
//! # It is never a sibling store
//!
//! [`BodyEmbed`] is an extension slot *on* the entry, versioned with it. It is
//! serialized as its own format-versioned channel (the `nudox.body.v1`
//! envelope, [`crate::body_wire`]) so a body edit touches only that channel and
//! the declaration bytes stay byte-identical — but it is still one entry, one
//! [`nudox_change::IntroId`]. There is no parallel IR universe and no occurrence
//! side table: usages are read back off the union of body call sites.
//!
//! # The merge contract (§5.1, normative)
//!
//! Both producers run on every impl body. The host merges their emissions with
//! [`merge_body`], which honours five rules that must never be softened:
//!
//! 1. No body at all → [`BodyEmbed::Absent`].
//! 2. Either tier produced any fact → [`BodyEmbed::Present`] carrying **both**
//!    the [`TreesitterBody`] and the [`OracleBody`], each filled to the maximum
//!    that tier saw. A tier's struct is empty only when that tier truly saw
//!    nothing.
//! 3. On overlapping call/type spans the tree-sitter structural fields are
//!    kept and the oracle's [`nudox_change::StableRef`] / higher [`Confidence`]
//!    rides on the matching span. Neither tier's non-overlapping facts are ever
//!    dropped.
//! 4. Host `resolve_occurrences` still writes occurrence frames from the union
//!    of call sites; the body embed remains for structure / snippet context.
//! 5. Forbidden steady state: shipping only a [`TreesitterBody`] or only an
//!    [`OracleBody`] when both producers ran. [`BodyFacts::is_forbidden_steady_state`]
//!    detects the smell so tests can assert it is rejected.

use ecosystem::Language;
use serde::{Deserialize, Serialize};
use smol_str::SmolStr;

use nudox_change::StableRef;

use crate::vocab::{Confidence, ReferenceKind, RelSpan};

// ---------------------------------------------------------------------------
// BodyEmbed
// ---------------------------------------------------------------------------

/// The body payload attached to one entry: absent, or the merged facts.
///
/// This is the value stored in the `nudox.body.v1` channel for an entry. It is
/// deliberately *not* shaped like the declaration [`crate::wire::OwnedEntryPayload`]:
/// the body speaks its own vocabulary (calls, locals, control, type mentions)
/// and shares only the entry's identity.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum BodyEmbed {
    /// The entry has no implementation body: a type alias, a forward
    /// declaration, a pure interface method, an empty module, and so on.
    Absent,
    /// The normative non-absent case: the merged tree-sitter + oracle facts.
    Present(BodyFacts),
}

impl BodyEmbed {
    /// True when no body facts are present.
    #[inline]
    pub fn is_absent(&self) -> bool {
        matches!(self, BodyEmbed::Absent)
    }

    /// Borrow the facts if present.
    #[inline]
    pub fn facts(&self) -> Option<&BodyFacts> {
        match self {
            BodyEmbed::Absent => None,
            BodyEmbed::Present(facts) => Some(facts),
        }
    }
}

// ---------------------------------------------------------------------------
// BodyFacts
// ---------------------------------------------------------------------------

/// The merged body of one entry: structural facts from tree-sitter and semantic
/// facts from the oracle, plus a note recording what each tier contributed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BodyFacts {
    /// The language this body was produced for.
    pub language: Language,
    /// CST-derived structure. Filled to the maximum tree-sitter saw; empty
    /// vectors only when tree-sitter genuinely produced nothing.
    pub tree: TreesitterBody,
    /// Semantic outline from the language oracle. Filled to the maximum the
    /// oracle saw; empty vectors only when the oracle genuinely produced
    /// nothing (or did not run on this body).
    pub oracle: OracleBody,
    /// Cross-tier merge metadata (honesty / debug).
    pub merge: BodyMergeNote,
}

impl BodyFacts {
    /// Detect the forbidden steady state (merge rule 5): both producers ran, yet
    /// one tier's struct is empty while it demonstrably produced facts.
    ///
    /// This is a *smell detector* used by adversarial tests. A well-formed merge
    /// can never produce it because [`merge_body`] preserves both emissions
    /// verbatim; a value that returns `true` here is constructible only via a
    /// direct struct literal (a test hook), never via the merge path.
    pub fn is_forbidden_steady_state(&self) -> bool {
        self.merge.treesitter_ran
            && self.merge.oracle_ran
            && (self.tree.is_empty() != self.oracle.is_empty())
    }
}

// ---------------------------------------------------------------------------
// TreesitterBody
// ---------------------------------------------------------------------------

/// The structural (CST) view of a body. Every field is filled to the maximum
/// tree-sitter reached; the fidelity is honest — no field is fabricated.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TreesitterBody {
    /// Local bindings introduced in the body, in declaration order.
    pub locals: Vec<LocalBind>,
    /// Call sites (name + optional receiver + span). Pre-resolution; the oracle
    /// supplies resolved targets on the [`OracleBody`] side.
    pub calls: Vec<BodyCall>,
    /// Coarse control-flow sketches (if / match / loop). Coarse is fine.
    pub control: Vec<ControlSketch>,
    /// Imports pulled in at body scope (rare; usually module-level).
    pub imports_in_body: Vec<BodyImport>,
    /// The CST root kind, for debug only — never part of identity.
    pub root_kind: Option<SmolStr>,
}

impl TreesitterBody {
    /// True when tree-sitter contributed nothing at all.
    pub fn is_empty(&self) -> bool {
        self.locals.is_empty()
            && self.calls.is_empty()
            && self.control.is_empty()
            && self.imports_in_body.is_empty()
            && self.root_kind.is_none()
    }
}

// ---------------------------------------------------------------------------
// OracleBody
// ---------------------------------------------------------------------------

/// The semantic view of a body from the language oracle. Filled to the maximum
/// the oracle resolved; targets carry [`nudox_change::StableRef`] and
/// [`Confidence`].
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OracleBody {
    /// Resolved (or resolvable) call sites.
    pub calls: Vec<OracleCall>,
    /// Types mentioned in the body (annotations, casts, generic args).
    pub type_mentions: Vec<OracleTypeMention>,
    /// Optional semantic dataflow crumbs (reads / writes of places).
    pub reads_writes: Vec<OracleAccess>,
}

impl OracleBody {
    /// True when the oracle contributed nothing at all.
    pub fn is_empty(&self) -> bool {
        self.calls.is_empty() && self.type_mentions.is_empty() && self.reads_writes.is_empty()
    }
}

// ---------------------------------------------------------------------------
// BodyMergeNote
// ---------------------------------------------------------------------------

/// Records which tiers ran and how conflicts were resolved. Advisory metadata,
/// not identity; it lets consumers reason honestly about fidelity.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BodyMergeNote {
    /// Whether tree-sitter ran on this body.
    pub treesitter_ran: bool,
    /// Whether the oracle analyzed this body.
    pub oracle_ran: bool,
    /// The conflict-resolution policy applied to overlapping spans.
    /// Currently always [`BodyMergeNote::CONFLICT_ORACLE_TARGET_TREESITTER_SPAN`].
    pub conflict_policy: ConflictPolicy,
}

impl BodyMergeNote {
    /// The single conflict policy: on overlapping call/type spans the oracle
    /// wins the target and confidence; tree-sitter keeps its structure.
    pub const CONFLICT_ORACLE_TARGET_TREESITTER_SPAN: ConflictPolicy =
        ConflictPolicy::OracleTargetTreesitterSpan;

    /// A note stating that both tiers ran under the standard conflict policy.
    pub fn both_ran() -> Self {
        Self {
            treesitter_ran: true,
            oracle_ran: true,
            conflict_policy: Self::CONFLICT_ORACLE_TARGET_TREESITTER_SPAN,
        }
    }

    /// A note stating that only tree-sitter ran (no oracle available).
    pub fn treesitter_only() -> Self {
        Self {
            treesitter_ran: true,
            oracle_ran: false,
            conflict_policy: Self::CONFLICT_ORACLE_TARGET_TREESITTER_SPAN,
        }
    }
}

/// The conflict-resolution policy applied when both tiers touch the same span.
///
/// Modelled as an enum (not a `&'static str`) so it survives postcard
/// round-trips and can never be a free-form typo. Frozen discriminants.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum ConflictPolicy {
    /// On overlap: oracle target + confidence win; tree-sitter structure kept.
    OracleTargetTreesitterSpan = 0,
}

// ---------------------------------------------------------------------------
// Fact records
// ---------------------------------------------------------------------------

/// A local binding introduced inside a body (tree-sitter tier).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalBind {
    /// The binding name (`_` for a wildcard binding).
    pub name: SmolStr,
    /// The syntactic form of the binding.
    pub kind: LocalKind,
    /// Span of the binding, relative to the owning entry span start.
    pub rel_span: RelSpan,
}

/// The syntactic form of a [`LocalBind`]. Coarse across languages; frozen
/// discriminants.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum LocalKind {
    /// A `let` / `var` / `:=` style local.
    Let = 0,
    /// A binding introduced by a pattern (destructuring, match arm).
    Pattern = 1,
    /// A loop / comprehension iteration variable.
    LoopVar = 2,
    /// A `const` / immutable local.
    Const = 3,
}

/// A call site as tree-sitter saw it (name + optional receiver + span).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BodyCall {
    /// The callee name as written at the call site.
    pub name: SmolStr,
    /// The receiver expression's leaf name, if this is a method-style call.
    pub receiver: Option<SmolStr>,
    /// Span of the call, relative to the owning entry span start.
    pub rel_span: RelSpan,
}

/// A call site as the oracle resolved it (target + kind + confidence + span).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OracleCall {
    /// The resolved callee. `None` when host resolution is still pending (the
    /// oracle saw a call but could not bind it).
    pub target: Option<StableRef>,
    /// The kind of reference (call / method call / …).
    pub kind: ReferenceKind,
    /// The confidence at which the target was resolved.
    pub confidence: Confidence,
    /// Span of the call, relative to the owning entry span start.
    pub rel_span: RelSpan,
}

/// A type mentioned in the body, resolved by the oracle.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OracleTypeMention {
    /// The mentioned type.
    pub ty: StableRef,
    /// Span of the mention, relative to the owning entry span start.
    pub rel_span: RelSpan,
}

/// A semantic read/write crumb from the oracle (optional dataflow signal).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OracleAccess {
    /// The accessed place's leaf name (a projection root; full places arrive
    /// with the region-tree body in a later wave).
    pub place: SmolStr,
    /// Whether this access reads or writes the place.
    pub mode: AccessMode,
    /// Span of the access, relative to the owning entry span start.
    pub rel_span: RelSpan,
}

/// Whether an [`OracleAccess`] reads or writes. Frozen discriminants.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum AccessMode {
    /// A read / borrow of the place.
    Read = 0,
    /// A write / assignment to the place.
    Write = 1,
}

/// A coarse control-flow sketch (tree-sitter tier). Spans are relative to the
/// owning entry span start. Deliberately coarse: the full region tree arrives
/// with the `.nb` body in a later wave; this is the snippet-outline shape.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ControlSketch {
    /// An `if` / `else` region.
    If {
        /// The condition span.
        cond: RelSpan,
        /// The then-arm span.
        then_arm: RelSpan,
        /// The else-arm span, if any (else-if lowers to a nested `If`).
        else_arm: Option<RelSpan>,
    },
    /// A `match` / `switch` region over a scrutinee, with N arm spans.
    Match {
        /// The scrutinee span.
        scrutinee: RelSpan,
        /// One span per arm, in source order.
        arms: Vec<RelSpan>,
    },
    /// A loop region (`for` / `while` / `loop`).
    Loop {
        /// The loop body span.
        body: RelSpan,
    },
}

/// An import brought into body scope (rare; usually module-level).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BodyImport {
    /// The imported path as written.
    pub path: SmolStr,
    /// Span of the import, relative to the owning entry span start.
    pub rel_span: RelSpan,
}

// ---------------------------------------------------------------------------
// merge_body
// ---------------------------------------------------------------------------

/// Merge the two producer emissions for one entry into a single [`BodyEmbed`]
/// (§5.1, normative).
///
/// The merge is *additive*: it is a union, never a pick-one. It preserves both
/// emissions verbatim — the oracle's resolved targets and the tree-sitter
/// structure both survive, side by side, keyed by their (relative) spans. The
/// only place confidence precedence matters is *reading* overlapping facts back
/// out (rule 3); the storage keeps everything.
///
/// Rules honoured:
/// - Rule 1: both tiers empty → [`BodyEmbed::Absent`].
/// - Rule 2: either tier non-empty → [`BodyEmbed::Present`] with both structs
///   carried through untouched (max fill).
/// - Rule 3: overlapping spans keep tree-sitter structure and let the oracle
///   ride its `StableRef` / higher confidence — realised by keeping *both*
///   `tree.calls` and `oracle.calls` so a reader can join them by span; see
///   [`overlapping_call`].
/// - Rule 5: the note records that both ran, making a later
///   [`BodyFacts::is_forbidden_steady_state`] check meaningful.
///
/// This takes ownership and performs no clones on the hot path — the vectors
/// move straight into the result.
pub fn merge_body(
    language: Language,
    tree: TreesitterBody,
    oracle: OracleBody,
    note: BodyMergeNote,
) -> BodyEmbed {
    if tree.is_empty() && oracle.is_empty() {
        return BodyEmbed::Absent;
    }
    BodyEmbed::Present(BodyFacts { language, tree, oracle, merge: note })
}

/// For a given tree-sitter [`BodyCall`], find the oracle call whose span
/// overlaps it (rule 3 join). Returns the first overlapping [`OracleCall`], if
/// any — the reader uses it to attach the resolved target to the structural
/// call. Borrowing only; no allocation.
pub fn overlapping_call<'a>(call: &BodyCall, oracle: &'a OracleBody) -> Option<&'a OracleCall> {
    oracle
        .calls
        .iter()
        .find(|oc| oc.rel_span.overlaps(call.rel_span))
}

#[cfg(test)]
mod tests {
    use super::*;
    use nudox_change::{EcosystemId, IntroId, PackageLineageId, PackageName};

    fn sr(name: &str) -> StableRef {
        StableRef::new(
            PackageLineageId::new(EcosystemId::new("rust"), PackageName::new("http")),
            IntroId::from_domain("test.intro", name.as_bytes()),
        )
    }

    fn tree_with_call() -> TreesitterBody {
        TreesitterBody {
            calls: vec![BodyCall {
                name: "handle_api".into(),
                receiver: None,
                rel_span: RelSpan::new(40, 50),
            }],
            root_kind: Some("function_item".into()),
            ..Default::default()
        }
    }

    fn oracle_with_call() -> OracleBody {
        OracleBody {
            calls: vec![OracleCall {
                target: Some(sr("handle_api")),
                kind: ReferenceKind::FunctionCall,
                confidence: Confidence::Oracle,
                rel_span: RelSpan::new(40, 50),
            }],
            ..Default::default()
        }
    }

    #[test]
    fn both_empty_is_absent() {
        let body = merge_body(
            Language::Rust,
            TreesitterBody::default(),
            OracleBody::default(),
            BodyMergeNote::both_ran(),
        );
        assert!(body.is_absent());
    }

    #[test]
    fn one_sided_treesitter_is_present() {
        let body = merge_body(
            Language::Rust,
            tree_with_call(),
            OracleBody::default(),
            BodyMergeNote::treesitter_only(),
        );
        let facts = body.facts().expect("present");
        assert!(!facts.tree.is_empty());
        assert!(facts.oracle.is_empty());
        // Not the forbidden steady state: the oracle did not run.
        assert!(!facts.is_forbidden_steady_state());
    }

    #[test]
    fn overlapping_span_retains_both_tiers() {
        let body = merge_body(
            Language::Rust,
            tree_with_call(),
            oracle_with_call(),
            BodyMergeNote::both_ran(),
        );
        let facts = body.facts().expect("present");
        // tree-sitter structure kept:
        assert_eq!(facts.tree.calls[0].name, "handle_api");
        assert_eq!(facts.tree.root_kind.as_deref(), Some("function_item"));
        // oracle target + confidence rides the matching span:
        let joined = overlapping_call(&facts.tree.calls[0], &facts.oracle).expect("joined");
        assert_eq!(joined.confidence, Confidence::Oracle);
        assert!(joined.target.is_some());
    }

    #[test]
    fn non_overlapping_facts_never_dropped() {
        let mut tree = tree_with_call();
        tree.calls.push(BodyCall {
            name: "log".into(),
            receiver: None,
            rel_span: RelSpan::new(80, 83),
        });
        let mut oracle = oracle_with_call();
        oracle.type_mentions.push(OracleTypeMention {
            ty: sr("Request"),
            rel_span: RelSpan::new(10, 17),
        });
        let body = merge_body(Language::Rust, tree, oracle, BodyMergeNote::both_ran());
        let facts = body.facts().expect("present");
        assert_eq!(facts.tree.calls.len(), 2); // both tree calls kept
        assert_eq!(facts.oracle.type_mentions.len(), 1); // oracle-only fact kept
    }

    #[test]
    fn forbidden_steady_state_detector_flags_hand_built_smell() {
        // Constructible only by a direct literal: both ran but oracle empty.
        let facts = BodyFacts {
            language: Language::Rust,
            tree: tree_with_call(),
            oracle: OracleBody::default(),
            merge: BodyMergeNote::both_ran(),
        };
        assert!(facts.is_forbidden_steady_state());
    }
}
