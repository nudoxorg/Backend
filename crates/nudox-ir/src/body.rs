//! The implementation (body) plane: merged tree-sitter + oracle facts attached
//! to a single owning entry.
//!
//! # Where this sits
//!
//! The declaration plane answers *what a symbol is*. This module answers *what
//! a symbol does*: the union of what tree-sitter saw structurally and what the
//! language oracle resolved semantically, both attached to the one entry that
//! owns them, both keyed by spans **relative** to that entry's declaration span
//! start.
//!
//! # Dual-fidelity tiers
//!
//! - [`TreesitterBody`]: CST/structural facts. Pre-resolution; no resolved
//!   targets. Produced by the tree-sitter tier on every body.
//! - [`OracleBody`]: Semantic facts. Resolved targets, type mentions, and
//!   dataflow crumbs. Produced by the language oracle when available.
//!
//! # Merge contract (§5.1, normative)
//!
//! Both producers run on every impl body. The host merges their emissions with
//! [`merge_body`], which is an additive union: no facts are ever dropped. When
//! both sides are [`BodyEmbed::Absent`] the result is [`BodyEmbed::Absent`];
//! otherwise the result is [`BodyEmbed::Present`] carrying the union of both
//! tiers. Dedup and confidence-lattice join are deferred.
//!
//! # FIXME: Language unification
//!
//! [`Language`] is a LOCAL minimal placeholder. `workspace/ir` uses
//! `heart::Language`; nudox-ir must NOT depend on `heart` (a separate crate).
//! When the shared Language vocabulary crate is established this local enum
//! should be replaced by the canonical import.

use serde::{Deserialize, Serialize};

use crate::{
    change::StableRef,
    vocab::{Confidence, ReferenceKind, RelSpan},
};

// ---------------------------------------------------------------------------
// Language
// ---------------------------------------------------------------------------

/// The source language a body was produced for.
///
/// # FIXME: Language unification
/// This is a LOCAL minimal placeholder. `workspace/ir` uses `heart::Language`.
/// nudox-ir must NOT depend on `heart`. Replace this with the shared Language
/// vocabulary once a common crate is established (Rev2 unification work).
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
///
/// Serialized as its own format-versioned channel so a body edit touches only
/// that channel and the declaration bytes stay byte-identical. There is no
/// parallel IR universe and no occurrence side table.
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
/// facts from the oracle.
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
}

// ---------------------------------------------------------------------------
// TreesitterBody
// ---------------------------------------------------------------------------

/// The structural (CST) view of a body. Every field is filled to the maximum
/// tree-sitter reached; the fidelity is honest — no field is fabricated.
///
/// # FIXME: deferred fields
/// `imports_in_body` and `root_kind` from `workspace/ir::TreesitterBody` are
/// omitted here. `root_kind` (CST root node kind, debug-only) and
/// `imports_in_body` (body-scope imports) await the body-scope import model and
/// `SmolStr` dependency decision before being added.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TreesitterBody {
    /// Local bindings introduced in the body, in declaration order.
    pub locals: Vec<LocalBind>,
    /// Call sites (name + span). Pre-resolution; the oracle supplies resolved
    /// targets on the [`OracleBody`] side.
    pub calls: Vec<BodyCall>,
    /// Coarse control-flow sketches (if / match / loop). Coarse is fine.
    pub control: Vec<ControlSketch>,
}

// ---------------------------------------------------------------------------
// OracleBody
// ---------------------------------------------------------------------------

/// The semantic view of a body from the language oracle. Filled to the maximum
/// the oracle resolved; targets carry [`StableRef`] and [`Confidence`].
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OracleBody {
    /// Resolved (or resolvable) call sites.
    pub calls: Vec<OracleCall>,
    /// Types mentioned in the body (annotations, casts, generic args).
    pub type_mentions: Vec<StableRef>,
    /// Optional semantic dataflow crumbs (reads / writes of places).
    pub reads_writes: Vec<OracleAccess>,
}

// ---------------------------------------------------------------------------
// Fact records
// ---------------------------------------------------------------------------

/// A call site as tree-sitter saw it (name + span). Pre-resolution; no target.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BodyCall {
    /// The callee name as written at the call site.
    pub name: String,
    /// Span of the call, relative to the owning entry span start.
    pub span: RelSpan,
}

/// A call site as the oracle resolved it (target + kind + confidence + span).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OracleCall {
    /// The resolved callee. `None` when resolution is still pending (the oracle
    /// saw a call but could not bind it).
    pub target: Option<StableRef>,
    /// The kind of reference (call / method call / …).
    pub kind: ReferenceKind,
    /// The confidence at which the target was resolved.
    pub confidence: Confidence,
    /// Span of the call, relative to the owning entry span start.
    pub span: RelSpan,
}

/// A local binding introduced inside a body (tree-sitter tier).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalBind {
    /// The binding name (`_` for a wildcard binding).
    pub name: String,
    /// The syntactic form of the binding.
    pub kind: LocalKind,
    /// Span of the binding, relative to the owning entry span start.
    pub span: RelSpan,
}

/// The syntactic form of a [`LocalBind`]. Coarse across languages.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum LocalKind {
    /// A `let` / `var` / `:=` style local.
    Let,
    /// A `var` binding (distinguished from `let` for languages where the
    /// difference is load-bearing, e.g. JavaScript).
    Var,
    /// A `const` / immutable local.
    Const,
    /// A function parameter.
    Param,
    /// A binding introduced by a pattern (destructuring, match arm).
    Pattern,
}

/// A coarse control-flow sketch (tree-sitter tier).
///
/// # FIXME: spans and nesting
/// This is a simple enum with no span or nesting data. `workspace/ir` carries
/// richer `ControlSketch` variants with condition/arm spans. Add span fields
/// and nesting when the body-plane structured-CF model (Rev2 region tree)
/// lands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ControlSketch {
    /// An `if` / `else` region.
    If,
    /// A `match` / `switch` region.
    Match,
    /// A loop region (`for` / `while` / `loop`).
    Loop,
}

/// A semantic read/write crumb from the oracle (optional dataflow signal).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OracleAccess {
    /// The accessed place's leaf name (a projection root; full places arrive
    /// with the region-tree body in a later wave).
    pub place: String,
    /// Whether this access reads or writes the place.
    pub mode: AccessMode,
    /// Span of the access, relative to the owning entry span start.
    pub span: RelSpan,
}

/// Whether an [`OracleAccess`] reads or writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AccessMode {
    /// A read / borrow of the place.
    Read,
    /// A write / assignment to the place.
    Write,
    /// A read-modify-write (compound assignment, `+=`, etc.).
    ReadWrite,
}

// ---------------------------------------------------------------------------
// merge_body
// ---------------------------------------------------------------------------

/// Merge two [`BodyEmbed`] values for one entry into a single [`BodyEmbed`].
///
/// This is an **additive union**: no facts are ever dropped. Rules:
/// - Both `Absent` → `Absent`.
/// - Either `Present` → `Present` carrying the union of both tiers (vecs
///   concatenated; the non-absent side's language is used when only one is
///   `Present`).
/// - Both `Present` → both tiers' vecs concatenated; language from `a`.
///
/// # FIXME: Rev2 dedup and lattice join
/// This is a straight union (vec concatenation). Rev2 wants a span-indexed
/// confidence-lattice join: on overlapping spans the oracle target+confidence
/// wins and duplicate tree-sitter entries are merged rather than duplicated.
/// Dedup/lattice-join is deferred; add a span-indexed pass once the body-plane
/// structured-CF model lands.
///
/// If one side is `Absent`, the other is returned unchanged (identity element).
pub fn merge_body(a: BodyEmbed, b: BodyEmbed) -> BodyEmbed {
    match (a, b) {
        (BodyEmbed::Absent, other) | (other, BodyEmbed::Absent) => other,
        (BodyEmbed::Present(mut fa), BodyEmbed::Present(fb)) => {
            // Concatenate tree tier.
            fa.tree.locals.extend(fb.tree.locals);
            fa.tree.calls.extend(fb.tree.calls);
            fa.tree.control.extend(fb.tree.control);
            // Concatenate oracle tier.
            fa.oracle.calls.extend(fb.oracle.calls);
            fa.oracle.type_mentions.extend(fb.oracle.type_mentions);
            fa.oracle.reads_writes.extend(fb.oracle.reads_writes);
            BodyEmbed::Present(fa)
        }
    }
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

    fn make_body_embed() -> BodyEmbed {
        BodyEmbed::Present(BodyFacts {
            language: Language::Rust,
            tree: TreesitterBody {
                locals: vec![LocalBind {
                    name: "x".to_string(),
                    kind: LocalKind::Let,
                    span: RelSpan::new(0, 10),
                }],
                calls: vec![BodyCall {
                    name: "foo".to_string(),
                    span: RelSpan::new(10, 20),
                }],
                control: vec![ControlSketch::If],
            },
            oracle: OracleBody {
                calls: vec![OracleCall {
                    target: Some(make_stable_ref("foo")),
                    kind: ReferenceKind::FunctionCall,
                    confidence: Confidence::Oracle,
                    span: RelSpan::new(10, 20),
                }],
                type_mentions: vec![make_stable_ref("Bar")],
                reads_writes: vec![OracleAccess {
                    place: "x".to_string(),
                    mode: AccessMode::Read,
                    span: RelSpan::new(5, 8),
                }],
            },
        })
    }

    #[test]
    fn body_serde_roundtrip() {
        let embed = make_body_embed();
        let json = serde_json::to_string(&embed).expect("serialize BodyEmbed");
        let back: BodyEmbed = serde_json::from_str(&json).expect("deserialize BodyEmbed");
        assert_eq!(embed, back, "BodyEmbed serde round-trip must be identity");
    }

    #[test]
    fn merge_absent_left_identity() {
        let x = make_body_embed();
        let result = merge_body(BodyEmbed::Absent, x.clone());
        assert_eq!(result, x, "merge(Absent, x) must be x");
    }

    #[test]
    fn merge_absent_right_identity() {
        let x = make_body_embed();
        let result = merge_body(x.clone(), BodyEmbed::Absent);
        assert_eq!(result, x, "merge(x, Absent) must be x");
    }

    #[test]
    fn merge_both_absent_is_absent() {
        let result = merge_body(BodyEmbed::Absent, BodyEmbed::Absent);
        assert!(result.is_absent(), "merge(Absent, Absent) must be Absent");
    }

    #[test]
    fn merge_two_present_unions_calls() {
        let a = BodyEmbed::Present(BodyFacts {
            language: Language::Rust,
            tree: TreesitterBody {
                calls: vec![BodyCall {
                    name: "alpha".to_string(),
                    span: RelSpan::new(0, 5),
                }],
                ..Default::default()
            },
            oracle: OracleBody {
                calls: vec![OracleCall {
                    target: Some(make_stable_ref("alpha")),
                    kind: ReferenceKind::FunctionCall,
                    confidence: Confidence::Oracle,
                    span: RelSpan::new(0, 5),
                }],
                ..Default::default()
            },
        });
        let b = BodyEmbed::Present(BodyFacts {
            language: Language::TypeScript,
            tree: TreesitterBody {
                calls: vec![BodyCall {
                    name: "beta".to_string(),
                    span: RelSpan::new(10, 15),
                }],
                ..Default::default()
            },
            oracle: OracleBody {
                calls: vec![OracleCall {
                    target: None,
                    kind: ReferenceKind::MethodCall,
                    confidence: Confidence::Syntactic,
                    span: RelSpan::new(10, 15),
                }],
                ..Default::default()
            },
        });

        let merged = merge_body(a, b);
        let facts = merged.facts().expect("merged must be Present");

        // Both tree calls must be present.
        assert_eq!(facts.tree.calls.len(), 2, "tree calls must be unioned");
        assert_eq!(facts.tree.calls[0].name, "alpha");
        assert_eq!(facts.tree.calls[1].name, "beta");

        // Both oracle calls must be present.
        assert_eq!(facts.oracle.calls.len(), 2, "oracle calls must be unioned");
        assert_eq!(facts.oracle.calls[0].confidence, Confidence::Oracle);
        assert_eq!(facts.oracle.calls[1].confidence, Confidence::Syntactic);
    }
}
