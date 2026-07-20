//! Adversarial property tests for [`nudox_ir::merge_body`] (INDEX-PLAN §5.1).
//!
//! These are integration tests (they live outside the crate's `#[cfg(test)]`
//! unit modules) so they exercise only the public API. They assert the five
//! normative merge rules under proptest-generated synthetic span sets:
//!
//! - both tiers empty ⇒ `Absent`;
//! - one-sided input ⇒ `Present` with an honest merge note;
//! - overlapping spans ⇒ oracle target + tree-sitter structure both retained;
//! - non-overlapping facts are never dropped;
//! - the forbidden steady state is constructible only by hand and is detected.

use ecosystem::Language;
use proptest::prelude::*;

use crate::change::{EcosystemId, IntroId, PackageLineageId, PackageName, StableRef};

use crate::body::{
    merge_body, overlapping_call, BodyCall, BodyEmbed, BodyFacts, BodyMergeNote, OracleBody,
    OracleCall, OracleTypeMention, TreesitterBody,
};
use crate::vocab::{Confidence, ReferenceKind, RelSpan};

fn sample_ref(seed: u64) -> StableRef {
    StableRef::new(
        PackageLineageId::new(EcosystemId::new("rust"), PackageName::new("http")),
        IntroId::from_domain("proptest.intro", &seed.to_le_bytes()),
    )
}

/// A strategy for a non-degenerate relative span `[start, start+len)`.
fn rel_span_strategy() -> impl Strategy<Value = RelSpan> {
    (0u32..1000, 1u32..50).prop_map(|(start, len)| RelSpan::new(start, start + len))
}

fn tree_call_strategy() -> impl Strategy<Value = BodyCall> {
    (rel_span_strategy(), 0u64..8).prop_map(|(span, n)| BodyCall {
        name: format!("call_{n}").into(),
        receiver: None,
        rel_span: span,
    })
}

fn oracle_call_strategy() -> impl Strategy<Value = OracleCall> {
    (rel_span_strategy(), 0u64..8).prop_map(|(span, n)| OracleCall {
        target: Some(sample_ref(n)),
        kind: ReferenceKind::FunctionCall,
        confidence: Confidence::Oracle,
        rel_span: span,
    })
}

proptest! {
    /// Rule 1: two empty tiers always collapse to `Absent`, regardless of the note.
    #[test]
    fn both_empty_is_always_absent(ts_ran in any::<bool>(), or_ran in any::<bool>()) {
        let note = BodyMergeNote {
            treesitter_ran: ts_ran,
            oracle_ran: or_ran,
            conflict_policy: BodyMergeNote::CONFLICT_ORACLE_TARGET_TREESITTER_SPAN,
        };
        let body = merge_body(Language::Rust, TreesitterBody::default(), OracleBody::default(), note);
        prop_assert!(matches!(body, BodyEmbed::Absent));
    }

    /// Rule 2: any non-empty tier yields `Present`, carrying both structs, with a note.
    #[test]
    fn one_sided_yields_present_with_honest_note(
        tree_calls in proptest::collection::vec(tree_call_strategy(), 1..6),
    ) {
        let tree = TreesitterBody { calls: tree_calls.clone(), ..Default::default() };
        let body = merge_body(Language::Rust, tree, OracleBody::default(), BodyMergeNote::treesitter_only());
        let facts = match &body {
            BodyEmbed::Present(f) => f,
            BodyEmbed::Absent => return Err(TestCaseError::fail("expected Present")),
        };
        prop_assert_eq!(facts.tree.calls.len(), tree_calls.len());
        prop_assert!(facts.oracle.is_empty());
        // Honest note: only tree-sitter ran, so this is NOT the forbidden state.
        prop_assert!(!facts.is_forbidden_steady_state());
        prop_assert!(facts.merge.treesitter_ran && !facts.merge.oracle_ran);
    }

    /// Rule 3: overlapping spans keep tree-sitter structure AND expose the oracle
    /// target via the span join; both tiers survive.
    #[test]
    fn overlapping_spans_retain_both_tiers(span in rel_span_strategy(), seed in 0u64..8) {
        let tree = TreesitterBody {
            calls: vec![BodyCall { name: "f".into(), receiver: Some("recv".into()), rel_span: span }],
            ..Default::default()
        };
        let oracle = OracleBody {
            calls: vec![OracleCall {
                target: Some(sample_ref(seed)),
                kind: ReferenceKind::MethodCall,
                confidence: Confidence::Oracle,
                rel_span: span,
            }],
            ..Default::default()
        };
        let body = merge_body(Language::Rust, tree, oracle, BodyMergeNote::both_ran());
        let facts = match &body {
            BodyEmbed::Present(f) => f,
            BodyEmbed::Absent => return Err(TestCaseError::fail("expected Present")),
        };
        // tree-sitter structure retained:
        prop_assert_eq!(facts.tree.calls[0].receiver.as_deref(), Some("recv"));
        // oracle target reachable by the span join (rule 3 read path):
        let joined = overlapping_call(&facts.tree.calls[0], &facts.oracle);
        prop_assert!(joined.is_some());
        prop_assert_eq!(joined.unwrap().confidence, Confidence::Oracle);
        prop_assert!(!facts.is_forbidden_steady_state());
    }

    /// Rule "never drop non-overlapping facts": every input fact survives the merge.
    #[test]
    fn non_overlapping_facts_are_never_dropped(
        tree_calls in proptest::collection::vec(tree_call_strategy(), 0..6),
        oracle_calls in proptest::collection::vec(oracle_call_strategy(), 0..6),
        mentions in proptest::collection::vec(rel_span_strategy(), 0..4),
    ) {
        prop_assume!(!(tree_calls.is_empty() && oracle_calls.is_empty() && mentions.is_empty()));
        let tree = TreesitterBody { calls: tree_calls.clone(), ..Default::default() };
        let oracle = OracleBody {
            calls: oracle_calls.clone(),
            type_mentions: mentions
                .iter()
                .enumerate()
                .map(|(i, s)| OracleTypeMention { ty: sample_ref(i as u64), rel_span: *s })
                .collect(),
            ..Default::default()
        };
        let body = merge_body(Language::Rust, tree, oracle, BodyMergeNote::both_ran());
        match &body {
            BodyEmbed::Present(f) => {
                // Every fact count is preserved exactly — union, not pick-one.
                prop_assert_eq!(f.tree.calls.len(), tree_calls.len());
                prop_assert_eq!(f.oracle.calls.len(), oracle_calls.len());
                prop_assert_eq!(f.oracle.type_mentions.len(), mentions.len());
            }
            BodyEmbed::Absent => {
                // Only legal if truly everything was empty, excluded by prop_assume.
                return Err(TestCaseError::fail("dropped facts to Absent"));
            }
        }
    }
}

/// The forbidden steady state (rule 5) is not reachable through `merge_body`
/// with real inputs: whenever both tiers ran and one produced facts, the merge
/// keeps that tier's facts, so the detector stays false. It is only
/// constructible by a direct struct literal (a test hook), which the detector
/// then flags — proving the invariant is enforced where it matters.
#[test]
fn forbidden_steady_state_unreachable_via_merge_but_flagged_when_hand_built() {
    // Real merge with both tiers producing facts: never forbidden.
    let tree = TreesitterBody {
        calls: vec![BodyCall { name: "a".into(), receiver: None, rel_span: RelSpan::new(0, 3) }],
        ..Default::default()
    };
    let oracle = OracleBody {
        calls: vec![OracleCall {
            target: Some(sample_ref(1)),
            kind: ReferenceKind::FunctionCall,
            confidence: Confidence::Oracle,
            rel_span: RelSpan::new(0, 3),
        }],
        ..Default::default()
    };
    let body = merge_body(Language::Rust, tree.clone(), oracle, BodyMergeNote::both_ran());
    if let BodyEmbed::Present(f) = &body {
        assert!(!f.is_forbidden_steady_state());
    } else {
        panic!("expected Present");
    }

    // Hand-built smell: both ran, oracle empty. Detector rejects it.
    let smell = BodyFacts {
        language: Language::Rust,
        tree,
        oracle: OracleBody::default(),
        merge: BodyMergeNote::both_ran(),
    };
    assert!(smell.is_forbidden_steady_state());
}
