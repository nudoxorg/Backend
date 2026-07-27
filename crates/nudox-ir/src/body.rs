//! The implementation (body) plane: merged tree-sitter + oracle facts attached
//! to a single owning entry.
//!
//! # Dual-fidelity tiers
//!
//! - [`TreesitterBody`]: CST/structural facts. Pre-resolution; no resolved
//!   targets. Produced by the tree-sitter tier on every body.
//! - [`OracleBody`]: Semantic facts. Resolved targets, type mentions, and
//!   dataflow crumbs. Produced by the language oracle when available.
//!
//! # Fidelity gate (`oracle_ran`)
//!
//! [`BodyMergeNote::oracle_ran`] records whether the language oracle actually
//! ran on this body. A treesitter-only skeleton must never be mistaken for
//! oracle-confirmed truth. The continuity matcher gates its resolved-ref Jaccard
//! axis on `oracle_ran` on BOTH sides — this is the core of the never-merge
//! safety property.
//!
//! # FIXME: Language unification
//!
//! [`Language`] is a LOCAL minimal placeholder. `workspace/ir` uses
//! `heart::Language`; nudox-ir must NOT depend on `heart`. Replace this with
//! the shared Language vocabulary once a common crate is established.

use serde::{Deserialize, Serialize};

use crate::{
    change::StableRef,
    vocab::{Confidence, ReferenceKind, RelSpan},
};

// ---------------------------------------------------------------------------
// Language
// ---------------------------------------------------------------------------

/// The source language a body was produced for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Language {
    Rust,
    TypeScript,
    Go,
    Java,
    Python,
    CSharp,
    C,
    Cpp,
    Nix,
    Other,
}

// ---------------------------------------------------------------------------
// BodyEmbed
// ---------------------------------------------------------------------------

/// The body payload attached to one entry: absent, or the merged facts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum BodyEmbed {
    Absent,
    Present(BodyFacts),
}

impl BodyEmbed {
    #[inline]
    pub fn is_absent(&self) -> bool {
        matches!(self, BodyEmbed::Absent)
    }

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
/// facts from the oracle, plus a merge note recording fidelity.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BodyFacts {
    pub language: Language,
    pub tree: TreesitterBody,
    pub oracle: OracleBody,
    /// Cross-tier merge metadata.  `oracle_ran` is the fidelity gate for the
    /// continuity matcher's resolved-ref axis.
    pub merge: BodyMergeNote,
}

impl BodyFacts {
    /// Detect the forbidden steady state: both producers ran but one tier is
    /// empty when the other is not.  Smell-detector for adversarial tests.
    pub fn is_forbidden_steady_state(&self) -> bool {
        self.merge.treesitter_ran
            && self.merge.oracle_ran
            && (self.tree.is_empty() != self.oracle.is_empty())
    }
}

// ---------------------------------------------------------------------------
// BodyMergeNote
// ---------------------------------------------------------------------------

/// Records which tiers ran and how conflicts were resolved.
///
/// `oracle_ran` is the fidelity gate for the continuity matcher's
/// resolved-ref Jaccard axis.  When `false`, the oracle produced nothing; a
/// treesitter-only body has no resolved `StableRef` targets, so Jaccard is
/// meaningless and the axis must abstain.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BodyMergeNote {
    pub treesitter_ran: bool,
    pub oracle_ran: bool,
    pub conflict_policy: ConflictPolicy,
}

impl BodyMergeNote {
    pub const CONFLICT_ORACLE_TARGET_TREESITTER_SPAN: ConflictPolicy =
        ConflictPolicy::OracleTargetTreesitterSpan;

    pub fn both_ran() -> Self {
        Self {
            treesitter_ran: true,
            oracle_ran: true,
            conflict_policy: Self::CONFLICT_ORACLE_TARGET_TREESITTER_SPAN,
        }
    }

    pub fn treesitter_only() -> Self {
        Self {
            treesitter_ran: true,
            oracle_ran: false,
            conflict_policy: Self::CONFLICT_ORACLE_TARGET_TREESITTER_SPAN,
        }
    }
}

/// Conflict-resolution policy applied when both tiers touch the same span.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum ConflictPolicy {
    OracleTargetTreesitterSpan = 0,
}

// ---------------------------------------------------------------------------
// TreesitterBody
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TreesitterBody {
    pub locals: Vec<LocalBind>,
    pub calls: Vec<BodyCall>,
    pub control: Vec<ControlSketch>,
    pub imports_in_body: Vec<BodyImport>,
    pub root_kind: Option<String>,
}

impl TreesitterBody {
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

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OracleBody {
    pub calls: Vec<OracleCall>,
    /// Types mentioned in the body (annotations, casts, generic args).
    /// Each carries the resolved `StableRef` and its span.
    pub type_mentions: Vec<OracleTypeMention>,
    pub reads_writes: Vec<OracleAccess>,
}

impl OracleBody {
    pub fn is_empty(&self) -> bool {
        self.calls.is_empty() && self.type_mentions.is_empty() && self.reads_writes.is_empty()
    }
}

// ---------------------------------------------------------------------------
// Fact records
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalBind {
    pub name: String,
    pub kind: LocalKind,
    pub rel_span: RelSpan,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum LocalKind {
    Let = 0,
    Pattern = 1,
    LoopVar = 2,
    Const = 3,
    Param = 4,
    Var = 5,
}

/// A call site as tree-sitter saw it (name + optional receiver + span).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BodyCall {
    pub name: String,
    pub receiver: Option<String>,
    pub rel_span: RelSpan,
}

/// A call site as the oracle resolved it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OracleCall {
    pub target: Option<StableRef>,
    pub kind: ReferenceKind,
    pub confidence: Confidence,
    pub rel_span: RelSpan,
}

/// A type mentioned in the body, resolved by the oracle.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OracleTypeMention {
    /// The resolved type reference.
    pub ty: StableRef,
    pub rel_span: RelSpan,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OracleAccess {
    pub place: String,
    pub mode: AccessMode,
    pub rel_span: RelSpan,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum AccessMode {
    Read = 0,
    Write = 1,
    ReadWrite = 2,
}

/// Coarse control-flow sketch (tree-sitter tier).
///
/// The arm count in `Match` is used by the continuity matcher's control-shape
/// signal so a function with 3 arms is distinguishable from one with 1 arm.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ControlSketch {
    If {
        cond: RelSpan,
        then_arm: RelSpan,
        else_arm: Option<RelSpan>,
    },
    Match {
        scrutinee: RelSpan,
        arms: Vec<RelSpan>,
    },
    Loop {
        body: RelSpan,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BodyImport {
    pub path: String,
    pub rel_span: RelSpan,
}

// ---------------------------------------------------------------------------
// merge_body
// ---------------------------------------------------------------------------

/// Merge two producer emissions for one entry into a single [`BodyEmbed`].
///
/// Rule 1: both tiers empty → [`BodyEmbed::Absent`].
/// Rule 2: either tier non-empty → [`BodyEmbed::Present`] with both structs.
/// Rule 5: the note records which tiers ran, enabling fidelity gates.
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
/// overlaps it (rule 3 join). Borrowing only; no allocation.
pub fn overlapping_call<'a>(call: &BodyCall, oracle: &'a OracleBody) -> Option<&'a OracleCall> {
    oracle
        .calls
        .iter()
        .find(|oc| oc.rel_span.overlaps(call.rel_span))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::change::{EcosystemId, IntroId, PackageLineageId, PackageName, StableRef};

    fn make_stable_ref(name: &str) -> StableRef {
        StableRef::new(
            PackageLineageId::new(EcosystemId::new("cargo"), PackageName::new("demo")),
            IntroId::from_domain("test.intro", name.as_bytes()),
        )
    }

    fn make_tree_with_call() -> TreesitterBody {
        TreesitterBody {
            calls: vec![BodyCall {
                name: "handle_api".to_string(),
                receiver: None,
                rel_span: RelSpan::new(40, 50),
            }],
            root_kind: Some("function_item".to_string()),
            ..Default::default()
        }
    }

    fn make_oracle_with_call() -> OracleBody {
        OracleBody {
            calls: vec![OracleCall {
                target: Some(make_stable_ref("handle_api")),
                kind: ReferenceKind::FunctionCall,
                confidence: Confidence::Oracle,
                rel_span: RelSpan::new(40, 50),
            }],
            ..Default::default()
        }
    }

    fn make_body_embed() -> BodyEmbed {
        merge_body(
            Language::Rust,
            make_tree_with_call(),
            make_oracle_with_call(),
            BodyMergeNote::both_ran(),
        )
    }

    #[test]
    fn body_serde_roundtrip() {
        let embed = make_body_embed();
        let json = serde_json::to_string(&embed).expect("serialize BodyEmbed");
        let back: BodyEmbed = serde_json::from_str(&json).expect("deserialize BodyEmbed");
        assert_eq!(embed, back, "BodyEmbed serde round-trip must be identity");
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
            make_tree_with_call(),
            OracleBody::default(),
            BodyMergeNote::treesitter_only(),
        );
        let facts = body.facts().expect("present");
        assert!(!facts.tree.is_empty());
        assert!(facts.oracle.is_empty());
        assert!(!facts.is_forbidden_steady_state());
        assert!(!facts.merge.oracle_ran, "treesitter_only must have oracle_ran=false");
    }

    #[test]
    fn overlapping_span_retains_both_tiers() {
        let body = merge_body(
            Language::Rust,
            make_tree_with_call(),
            make_oracle_with_call(),
            BodyMergeNote::both_ran(),
        );
        let facts = body.facts().expect("present");
        assert_eq!(facts.tree.calls[0].name, "handle_api");
        assert_eq!(facts.tree.root_kind.as_deref(), Some("function_item"));
        let joined = overlapping_call(&facts.tree.calls[0], &facts.oracle).expect("joined");
        assert_eq!(joined.confidence, Confidence::Oracle);
        assert!(joined.target.is_some());
    }

    #[test]
    fn non_overlapping_facts_never_dropped() {
        let mut tree = make_tree_with_call();
        tree.calls.push(BodyCall {
            name: "log".to_string(),
            receiver: None,
            rel_span: RelSpan::new(80, 83),
        });
        let mut oracle = make_oracle_with_call();
        oracle.type_mentions.push(OracleTypeMention {
            ty: make_stable_ref("Request"),
            rel_span: RelSpan::new(10, 17),
        });
        let body = merge_body(Language::Rust, tree, oracle, BodyMergeNote::both_ran());
        let facts = body.facts().expect("present");
        assert_eq!(facts.tree.calls.len(), 2);
        assert_eq!(facts.oracle.type_mentions.len(), 1);
    }

    #[test]
    fn forbidden_steady_state_detector() {
        let facts = BodyFacts {
            language: Language::Rust,
            tree: make_tree_with_call(),
            oracle: OracleBody::default(),
            merge: BodyMergeNote::both_ran(),
        };
        assert!(facts.is_forbidden_steady_state());
    }

    /// Core of the never-merge safety property: treesitter-only body and
    /// oracle-confirmed body must not compare as equal.
    #[test]
    fn treesitter_only_and_oracle_confirmed_are_not_equal() {
        let ts_only = merge_body(
            Language::Rust,
            TreesitterBody {
                calls: vec![BodyCall {
                    name: "foo".to_string(),
                    receiver: None,
                    rel_span: RelSpan::new(0, 10),
                }],
                ..Default::default()
            },
            OracleBody::default(),
            BodyMergeNote::treesitter_only(),
        );
        let oracle_confirmed = merge_body(
            Language::Rust,
            TreesitterBody {
                calls: vec![BodyCall {
                    name: "foo".to_string(),
                    receiver: None,
                    rel_span: RelSpan::new(0, 10),
                }],
                ..Default::default()
            },
            OracleBody {
                calls: vec![OracleCall {
                    target: Some(make_stable_ref("foo")),
                    kind: ReferenceKind::FunctionCall,
                    confidence: Confidence::Oracle,
                    rel_span: RelSpan::new(0, 10),
                }],
                ..Default::default()
            },
            BodyMergeNote::both_ran(),
        );

        assert_ne!(
            ts_only, oracle_confirmed,
            "treesitter-only body must not equal oracle-confirmed body: \
             oracle_ran=false vs oracle_ran=true is a fidelity distinction"
        );
        assert!(!ts_only.facts().unwrap().merge.oracle_ran);
        assert!(oracle_confirmed.facts().unwrap().merge.oracle_ran);
    }
}
