//! The access-path planner, exercised directly over the hint space.
//!
//! `tests/pushdown.rs` proves the *executed* plan touches the store the right
//! number of times, but it can only reach the hint shapes a GraphQL query
//! happens to produce. This file drives [`plan_symbols`] and [`plan_packages`]
//! with the [`CandidateValue`] variants the trustfall engine can hand an
//! adapter — including the two that used to fall through to a full scan —
//! and pins the choice itself.

use nudox_engine::graph::adapter::Error;
use nudox_engine::graph::plan::{PackagePlan, SymbolPlan, plan_packages, plan_symbols};
use nudox_ir::kind::KindDiscriminant;
use trustfall::FieldValue;
use trustfall::provider::{CandidateValue, Range};

fn s(v: &str) -> CandidateValue<FieldValue> {
    CandidateValue::Single(FieldValue::String(v.into()))
}

fn many(vs: &[&str]) -> CandidateValue<FieldValue> {
    CandidateValue::Multiple(vs.iter().map(|v| FieldValue::String((*v).into())).collect())
}

const KEY_A: &str = "cargo:alpha#0101010101010101010101010101010101010101010101010101010101010101";
const KEY_B: &str = "cargo:beta#0202020202020202020202020202020202020202020202020202020202020202";

fn plan(
    key: Option<CandidateValue<FieldValue>>,
    name: Option<CandidateValue<FieldValue>>,
    kind: Option<CandidateValue<FieldValue>>,
    coerced: Option<&str>,
) -> Result<SymbolPlan, Error> {
    plan_symbols(key, name, kind, coerced)
}

// ---------------------------------------------------------------------------
// Nothing known
// ---------------------------------------------------------------------------

/// With no hints at all the only honest plan is the scan — and the planner
/// must say so rather than invent a constraint.
#[test]
fn no_hints_plans_a_full_scan() {
    assert_eq!(plan(None, None, None, None).unwrap(), SymbolPlan::FullScan);
}

/// `CandidateValue::All` is "no constraint detected", which is the same
/// information as no hint at all.
#[test]
fn an_unconstrained_candidate_plans_a_full_scan() {
    assert_eq!(
        plan(Some(CandidateValue::All), None, None, None).unwrap(),
        SymbolPlan::FullScan
    );
}

/// A coercion to the `Symbol` interface constrains nothing — every symbol
/// vertex satisfies it — so it must not be mistaken for a kind constraint.
#[test]
fn coercing_to_the_symbol_interface_is_not_a_kind_constraint() {
    assert_eq!(
        plan(None, None, None, Some("Symbol")).unwrap(),
        SymbolPlan::FullScan
    );
}

/// `OtherSymbol` is the adapter's catch-all for entries carrying *no*
/// `KindDiscriminant`. `by_kind` is keyed by discriminant and therefore cannot
/// enumerate them, so the scan is the correct — and only — plan.
#[test]
fn coercing_to_other_symbol_cannot_use_the_kind_index() {
    assert_eq!(
        plan(None, None, None, Some("OtherSymbol")).unwrap(),
        SymbolPlan::FullScan
    );
}

// ---------------------------------------------------------------------------
// Provably empty
// ---------------------------------------------------------------------------

/// `Impossible` on any one property makes the whole vertex unsatisfiable,
/// whichever property carried it.
#[test]
fn impossible_on_any_property_plans_nothing_at_all() {
    for (k, n, d) in [
        (Some(CandidateValue::Impossible), None, None),
        (None, Some(CandidateValue::Impossible), None),
        (None, None, Some(CandidateValue::Impossible)),
    ] {
        assert_eq!(plan(k, n, d, None).unwrap(), SymbolPlan::Empty);
    }
}

/// `Impossible` must win over a usable constraint on another property: the
/// vertex cannot exist, so the other property's index must not be probed.
#[test]
fn impossible_beats_a_usable_constraint_elsewhere() {
    assert_eq!(
        plan(Some(s(KEY_A)), None, Some(CandidateValue::Impossible), None).unwrap(),
        SymbolPlan::Empty
    );
}

/// `one_of` with an empty operand list is satisfiable by nothing.
#[test]
fn an_empty_one_of_plans_nothing_at_all() {
    assert_eq!(
        plan(Some(many(&[])), None, None, None).unwrap(),
        SymbolPlan::Empty
    );
    assert_eq!(
        plan(None, Some(many(&[])), None, None).unwrap(),
        SymbolPlan::Empty
    );
    assert_eq!(
        plan(None, None, Some(many(&[])), None).unwrap(),
        SymbolPlan::Empty
    );
}

// ---------------------------------------------------------------------------
// Key
// ---------------------------------------------------------------------------

#[test]
fn a_single_key_plans_one_direct_lookup() {
    let SymbolPlan::Keys(refs) = plan(Some(s(KEY_A)), None, None, None).unwrap() else {
        panic!("expected a Keys plan");
    };
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0].to_string(), KEY_A);
}

/// `one_of` over keys must become one lookup per key, not a scan. This is the
/// case `CandidateValue::Single` pattern-matching used to drop.
#[test]
fn one_of_over_keys_plans_a_lookup_per_key() {
    let SymbolPlan::Keys(refs) = plan(Some(many(&[KEY_A, KEY_B])), None, None, None).unwrap()
    else {
        panic!("expected a Keys plan");
    };
    let rendered: Vec<String> = refs.iter().map(std::string::ToString::to_string).collect();
    assert_eq!(rendered, vec![KEY_A.to_string(), KEY_B.to_string()]);
}

/// An unparseable key is a diagnosis, not an empty result set: returning no
/// rows for `"cargo:memchr"` (no `#introhex`) would turn a typo into a wrong
/// answer that looks like a correct one.
#[test]
fn a_malformed_key_is_an_error_not_an_empty_plan() {
    let err = plan(Some(s("cargo:memchr")), None, None, None)
        .expect_err("a key with no '#' must not plan silently");
    assert!(
        matches!(&err, Error::InvalidKey(k) if k == "cargo:memchr"),
        "expected InvalidKey carrying the offending text, got {err:?}"
    );
}

/// One bad key inside a `one_of` must fail the whole plan rather than being
/// quietly skipped — a partially-honoured filter is the least diagnosable
/// failure of the three.
#[test]
fn a_malformed_key_inside_one_of_fails_the_whole_plan() {
    let err = plan(Some(many(&[KEY_A, "not-a-key"])), None, None, None)
        .expect_err("a malformed member must not be dropped");
    assert!(matches!(&err, Error::InvalidKey(k) if k == "not-a-key"));
}

// ---------------------------------------------------------------------------
// Name
// ---------------------------------------------------------------------------

/// `NameIndex` keys on a Unicode-lowercased name, so the plan must carry the
/// lowercased form — otherwise `name @filter(op: "=", value: ["Point"])`
/// probes for `"Point"` in an index that only contains `"point"` and returns
/// nothing while looking like a working pushdown.
#[test]
fn a_name_plan_carries_the_lowercased_probe_key() {
    assert_eq!(
        plan(None, Some(s("PointDisplay")), None, None).unwrap(),
        SymbolPlan::Names(vec!["pointdisplay".to_owned()])
    );
}

#[test]
fn one_of_over_names_plans_a_probe_per_name() {
    assert_eq!(
        plan(None, Some(many(&["Point", "DISTANCE"])), None, None).unwrap(),
        SymbolPlan::Names(vec!["point".to_owned(), "distance".to_owned()])
    );
}

/// A key constraint is strictly more selective than a name constraint, so it
/// must win when both are present.
#[test]
fn key_outranks_name() {
    assert!(matches!(
        plan(Some(s(KEY_A)), Some(s("Point")), None, None).unwrap(),
        SymbolPlan::Keys(_)
    ));
}

// ---------------------------------------------------------------------------
// Kind and coercion
// ---------------------------------------------------------------------------

#[test]
fn a_kind_filter_plans_a_kind_index_probe() {
    assert_eq!(
        plan(None, None, Some(s("Trait")), None).unwrap(),
        SymbolPlan::Kinds(vec![KindDiscriminant::Trait])
    );
}

/// The whole claim of the coercion pushdown: `... on Trait` and
/// `kind @filter(op: "=", value: ["Trait"])` describe the same constraint, so
/// they must plan identically.
#[test]
fn a_type_coercion_plans_the_same_probe_as_the_equivalent_kind_filter() {
    for kind in [
        "Module", "Record", "Field", "Function", "Alias", "Trait", "Impl", "Enum", "Variant",
        "Const", "Static", "Reexport", "Param",
    ] {
        let by_filter = plan(None, None, Some(s(kind)), None).unwrap();
        let by_coercion = plan(None, None, None, Some(kind)).unwrap();
        assert_eq!(
            by_filter, by_coercion,
            "`... on {kind}` must plan the same probe as kind == \"{kind}\""
        );
    }
}

#[test]
fn one_of_over_kinds_plans_a_probe_per_kind() {
    assert_eq!(
        plan(None, None, Some(many(&["Function", "Record"])), None).unwrap(),
        SymbolPlan::Kinds(vec![KindDiscriminant::Function, KindDiscriminant::Record])
    );
}

/// An explicit `kind` filter is at least as tight as the coercion it sits
/// inside, so the filter is used and the coercion adds nothing.
#[test]
fn an_explicit_kind_filter_outranks_the_coercion() {
    assert_eq!(
        plan(None, None, Some(s("Record")), Some("Function")).unwrap(),
        SymbolPlan::Kinds(vec![KindDiscriminant::Record])
    );
}

/// An unrecognised `kind` operand is an error — `by_kind` has no bucket for
/// it, and yielding nothing would make a misspelling indistinguishable from an
/// empty corpus.
#[test]
fn an_unknown_kind_string_is_an_error() {
    let err = plan(None, None, Some(s("Funktion")), None).expect_err("unknown kinds must not plan");
    assert!(matches!(&err, Error::InvalidKind(k) if k == "Funktion"));
}

// ---------------------------------------------------------------------------
// Hints that carry no usable set
// ---------------------------------------------------------------------------

/// A range candidate is honest to refuse: none of the three lookup keys is
/// range-addressable through the store's current API, so planning a probe from
/// one would mean planning a probe that cannot be made.
#[test]
fn a_range_candidate_falls_back_to_a_scan() {
    assert_eq!(
        plan(
            None,
            Some(CandidateValue::Range(Range::full_non_null())),
            None,
            None
        )
        .unwrap(),
        SymbolPlan::FullScan
    );
}

/// A non-string candidate on a `String!` property means the schema and the
/// decoder disagree. Falling back to the scan yields the same rows, more
/// slowly — which is the only safe way to be wrong here.
#[test]
fn a_non_string_candidate_falls_back_to_a_scan() {
    assert_eq!(
        plan(
            Some(CandidateValue::Single(FieldValue::Int64(7))),
            None,
            None,
            None
        )
        .unwrap(),
        SymbolPlan::FullScan
    );
}

// ---------------------------------------------------------------------------
// Packages
// ---------------------------------------------------------------------------

#[test]
fn no_lineage_hint_enumerates_every_package() {
    assert_eq!(plan_packages(None).unwrap(), PackagePlan::All);
}

#[test]
fn a_lineage_equality_plans_a_direct_package_lookup() {
    let PackagePlan::Lineages(ids) = plan_packages(Some(s("cargo:memchr"))).unwrap() else {
        panic!("expected a Lineages plan");
    };
    assert_eq!(ids.len(), 1);
    assert_eq!(ids[0].to_string(), "cargo:memchr");
}

#[test]
fn one_of_over_lineages_plans_a_lookup_per_lineage() {
    let PackagePlan::Lineages(ids) =
        plan_packages(Some(many(&["cargo:memchr", "npm:left-pad"]))).unwrap()
    else {
        panic!("expected a Lineages plan");
    };
    let rendered: Vec<String> = ids.iter().map(std::string::ToString::to_string).collect();
    assert_eq!(rendered, vec!["cargo:memchr", "npm:left-pad"]);
}

/// A lineage has no `#introhex` half, so reporting "invalid stable-ref key"
/// for `"memchr"` would send the reader hunting for a hex suffix that should
/// not be there. The two formats get two error variants.
#[test]
fn a_malformed_lineage_gets_its_own_error_variant() {
    let err = plan_packages(Some(s("memchr"))).expect_err("a lineage with no ':' must not plan");
    assert!(
        matches!(&err, Error::InvalidLineage(l) if l == "memchr"),
        "expected InvalidLineage, got {err:?}"
    );
}

#[test]
fn an_impossible_lineage_plans_nothing_at_all() {
    assert_eq!(
        plan_packages(Some(CandidateValue::Impossible)).unwrap(),
        PackagePlan::Empty
    );
}
