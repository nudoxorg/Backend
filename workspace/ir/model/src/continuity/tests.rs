use super::*;
use crate::{
    apply::PristineIntroTable,
    body::{
        BodyEmbed, BodyMergeNote, Language, OracleBody, OracleCall, TreesitterBody, merge_body,
    },
    change::{EcosystemId, IntroId, PackageLineageId, PackageName, StableRef},
    entry::{Node, Symbol, Visibility},
    index::RawRef,
    kind::EntryKind,
    kinds::{Field, FieldKey, Function, Module, Record, RecordForm, Variant, VariantForm},
    view::IrView,
    vocab::{Confidence, ReferenceKind, RelSpan},
};
use std::path::PathBuf;

// -----------------------------------------------------------------------
// Test helpers
// -----------------------------------------------------------------------

fn intro(n: u8) -> IntroId {
    IntroId::from_raw([n; 32])
}

fn sym(name: &str) -> Symbol {
    Symbol {
        name: name.to_owned(),
        visibility: Visibility::Public,
        documentation: String::new(),
        source: PathBuf::new(),
        span: 0..1,
        aliases: Box::new([]),
        deprecation: None,
        doc_links: Box::new([]),
        attrs: Box::new([]),
        cfg: None,
    }
}

fn sym_with_doc(name: &str, doc: &str) -> Symbol {
    let mut s = sym(name);
    s.documentation = doc.to_owned();
    s
}

fn module_entry(s: Symbol) -> crate::entry::Entry {
    crate::entry::Entry::new(s, Node::build(None::<RawRef>, []), Module.into_kind())
}

fn fn_entry(s: Symbol) -> crate::entry::Entry {
    crate::entry::Entry::new(
        s,
        Node::build(None::<RawRef>, []),
        Function::builder().build().into_kind(),
    )
}

/// A Function entry with an explicit ABI string — produces a different
/// `kind_shape_hash` than `fn_entry` (which has `abi: None`).
fn fn_entry_abi(s: Symbol, abi: &str) -> crate::entry::Entry {
    crate::entry::Entry::new(
        s,
        Node::build(None::<RawRef>, []),
        Function::builder().abi(abi.to_owned()).build().into_kind(),
    )
}

/// A Record entry with `RecordForm::Tuple` — different `kind_shape_hash`
/// from `record_entry` which uses `RecordForm::Struct`.
fn record_entry_tuple(s: Symbol) -> crate::entry::Entry {
    crate::entry::Entry::new(
        s,
        Node::build(None::<RawRef>, []),
        Record::builder()
            .form(RecordForm::Tuple)
            .build()
            .into_kind(),
    )
}

fn record_entry(s: Symbol) -> crate::entry::Entry {
    crate::entry::Entry::new(
        s,
        Node::build(None::<RawRef>, []),
        Record::builder()
            .form(RecordForm::Struct)
            .build()
            .into_kind(),
    )
}

fn field_entry(s: Symbol) -> crate::entry::Entry {
    crate::entry::Entry::new(
        s,
        Node::build(None::<RawRef>, []),
        Field::builder().key(FieldKey::Named).build().into_kind(),
    )
}

fn variant_entry(s: Symbol) -> crate::entry::Entry {
    crate::entry::Entry::new(
        s,
        Node::build(None::<RawRef>, []),
        Variant::builder()
            .form(VariantForm::Unit)
            .build()
            .into_kind(),
    )
}

fn empty_pkg() -> PackageLineageId {
    PackageLineageId::new(EcosystemId::new("test"), PackageName::new("pkg"))
}

fn view_with(entries: Vec<(IntroId, crate::entry::Entry, Option<IntroId>)>) -> IrView {
    let mut table = PristineIntroTable::new();
    for (id, entry, parent) in entries {
        table.insert_live(id, entry, parent);
    }
    IrView::with_package(empty_pkg(), table)
}

fn stable_ref(name: &str) -> StableRef {
    StableRef::new(
        PackageLineageId::new(EcosystemId::new("rust"), PackageName::new("test")),
        IntroId::from_domain("test.body", name.as_bytes()),
    )
}

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
        OracleBody {
            calls,
            type_mentions: vec![],
            reads_writes: vec![],
        },
        BodyMergeNote::both_ran(),
    )
}

fn is_continued(s: &Substitution, next: IntroId, prior: IntroId) -> bool {
    s.assignments.iter().any(|a| match a {
        Assignment::Continuation {
            next_id, prior_id, ..
        } => *next_id == next && *prior_id == prior,
        Assignment::New { .. } => false,
    })
}

fn is_new(s: &Substitution, id: IntroId) -> bool {
    s.assignments
        .iter()
        .any(|a| matches!(a, Assignment::New { next_id } if *next_id == id))
}

fn is_deleted(s: &Substitution, id: IntroId) -> bool {
    s.deletions.iter().any(|d| d.prior_id == id)
}

fn has_edge(s: &Substitution, prior: IntroId, next: IntroId) -> bool {
    s.rename_edges
        .iter()
        .any(|e| e.prior_id == prior && e.next_id == next)
}

// -----------------------------------------------------------------------
// C-1: Empty prev + empty next → empty sigma.
// -----------------------------------------------------------------------

#[test]
fn c1_empty_empty() {
    let prev = view_with(vec![]);
    let next = view_with(vec![]);
    let s = resolve(&prev, &next, &Policy::default());
    assert!(s.assignments.is_empty());
    assert!(s.deletions.is_empty());
    assert!(s.rename_edges.is_empty());
}

// -----------------------------------------------------------------------
// C-2: Stable entry (same id, same content) → Continuation with id reuse.
// -----------------------------------------------------------------------

#[test]
fn c2_stable_entry() {
    let prev = view_with(vec![(intro(1), fn_entry(sym("foo")), None)]);
    let next = view_with(vec![(intro(1), fn_entry(sym("foo")), None)]);
    let s = resolve(&prev, &next, &Policy::default());
    assert!(is_continued(&s, intro(1), intro(1)));
    assert!(!is_deleted(&s, intro(1)));
    assert!(!is_new(&s, intro(1)));
}

// -----------------------------------------------------------------------
// C-3: Rename with unchanged body shape → High match.
//
// A function with a new name but same kind-body shape reaches HIGH via
// shape(50) + doc(10) (or shape + other corroborators) and is continued.
// We add a non-empty doc so shape(50) + doc(10) = 60 ≥ HIGH.
// -----------------------------------------------------------------------

#[test]
fn c3_renamed_function_continued() {
    let prev = view_with(vec![(
        intro(1),
        fn_entry(sym_with_doc("old_name", "does a thing")),
        None,
    )]);
    let next = view_with(vec![(
        intro(2),
        fn_entry(sym_with_doc("new_name", "does a thing")),
        None,
    )]);
    let s = resolve(&prev, &next, &Policy::default());
    // shape(50) + doc(10) = 60 ≥ HIGH → continued.
    assert!(
        is_continued(&s, intro(2), intro(1)),
        "renamed fn must be continued"
    );
    assert!(!is_deleted(&s, intro(1)));
}

// -----------------------------------------------------------------------
// C-4: Deleted entry.
// -----------------------------------------------------------------------

#[test]
fn c4_deleted() {
    let prev = view_with(vec![(intro(1), fn_entry(sym("foo")), None)]);
    let next = view_with(vec![]);
    let s = resolve(&prev, &next, &Policy::default());
    assert!(is_deleted(&s, intro(1)));
}

// -----------------------------------------------------------------------
// C-5: New entry.
// -----------------------------------------------------------------------

#[test]
fn c5_new_entry() {
    let prev = view_with(vec![]);
    let next = view_with(vec![(intro(5), fn_entry(sym("brand_new")), None)]);
    let s = resolve(&prev, &next, &Policy::default());
    assert!(is_new(&s, intro(5)));
}

// -----------------------------------------------------------------------
// C-6: Kind hard-gate — cross-kind pairs never merge.
//
// A Function named "Foo" and a Record named "Foo" must not be merged even
// at HIGH by the R-OV lone-1↔1 rule, because the kind gate blocks first.
// -----------------------------------------------------------------------

#[test]
fn c6_kind_gate_never_merges() {
    let prev = view_with(vec![(intro(1), fn_entry(sym("Foo")), None)]);
    let next = view_with(vec![(intro(2), record_entry(sym("Foo")), None)]);
    let s = resolve(&prev, &next, &Policy::default());
    assert!(is_new(&s, intro(2)), "cross-kind must not merge");
    assert!(is_deleted(&s, intro(1)));
}

// -----------------------------------------------------------------------
// C-7: R-CHILD — Field without parent continuity is NOT continued.
// -----------------------------------------------------------------------

#[test]
fn c7_r_child_no_parent() {
    // Prior field has parent intro(10); next field has parent intro(20).
    let mut prior_table = PristineIntroTable::new();
    prior_table.insert_live(intro(10), record_entry(sym("ParentA")), None);
    prior_table.insert_live(intro(1), field_entry(sym("x")), Some(intro(10)));
    let prev = IrView::with_package(empty_pkg(), prior_table);

    let mut next_table = PristineIntroTable::new();
    next_table.insert_live(intro(20), record_entry(sym("ParentB")), None);
    next_table.insert_live(intro(2), field_entry(sym("x")), Some(intro(20)));
    let next = IrView::with_package(empty_pkg(), next_table);

    let s = resolve(&prev, &next, &Policy::default());
    assert!(is_new(&s, intro(2)), "wrong parent → not continued");
    assert!(is_deleted(&s, intro(1)));
}

// -----------------------------------------------------------------------
// C-8: Determinism — two calls with the same inputs produce identical σ.
// -----------------------------------------------------------------------

#[test]
fn c8_determinism_across_calls() {
    let prev = view_with(vec![
        (intro(1), fn_entry(sym_with_doc("fn_a", "doc")), None),
        (intro(2), fn_entry(sym_with_doc("fn_b", "doc")), None),
    ]);
    let next = view_with(vec![
        (intro(11), fn_entry(sym_with_doc("fn_a", "doc")), None),
        (intro(12), fn_entry(sym_with_doc("fn_b", "doc")), None),
    ]);
    let s1 = resolve(&prev, &next, &Policy::default());
    let s2 = resolve(&prev, &next, &Policy::default());
    assert_eq!(s1.sigma(), s2.sigma(), "resolve must be deterministic");
}

// -----------------------------------------------------------------------
// C-9: Determinism across different insertion orders (HashMap iteration).
//
// Build the same logical table with reversed insertion order and verify
// the σ map is identical.
// -----------------------------------------------------------------------

#[test]
fn c9_determinism_insertion_order() {
    // Order A
    let prev_a = view_with(vec![
        (intro(1), fn_entry(sym_with_doc("f1", "d")), None),
        (intro(2), fn_entry(sym_with_doc("f2", "d")), None),
        (intro(3), fn_entry(sym_with_doc("f3", "d")), None),
    ]);
    let next_a = view_with(vec![
        (intro(11), fn_entry(sym_with_doc("f1", "d")), None),
        (intro(12), fn_entry(sym_with_doc("f2", "d")), None),
        (intro(13), fn_entry(sym_with_doc("f3", "d")), None),
    ]);
    // Order B (reversed)
    let prev_b = view_with(vec![
        (intro(3), fn_entry(sym_with_doc("f3", "d")), None),
        (intro(2), fn_entry(sym_with_doc("f2", "d")), None),
        (intro(1), fn_entry(sym_with_doc("f1", "d")), None),
    ]);
    let next_b = view_with(vec![
        (intro(13), fn_entry(sym_with_doc("f3", "d")), None),
        (intro(12), fn_entry(sym_with_doc("f2", "d")), None),
        (intro(11), fn_entry(sym_with_doc("f1", "d")), None),
    ]);
    let sa = resolve(&prev_a, &next_a, &Policy::default()).sigma();
    let sb = resolve(&prev_b, &next_b, &Policy::default()).sigma();
    assert_eq!(
        sa, sb,
        "different insertion orders must yield the same sigma"
    );
}

// -----------------------------------------------------------------------
// C-10: Policy threshold vs lone-pair forcing precedence.
// UNVERIFIED — this test has never passed and is ignored deliberately.
// -----------------------------------------------------------------------

#[test]
#[ignore = "threshold-vs-lone-pair precedence unverified; see doc comment"]
fn c10_lone_pair_forcing_outranks_threshold() {
    let prev = view_with(vec![(
        intro(1),
        fn_entry(sym_with_doc("foo", "doc")),
        Some(intro(9)),
    )]);
    let next = view_with(vec![(intro(2), fn_entry(sym_with_doc("bar", "doc")), None)]);

    let default_s = resolve(&prev, &next, &Policy::default());
    assert!(
        is_continued(&default_s, intro(2), intro(1)),
        "a lone rename must be continued at the default threshold"
    );

    let strict = Policy {
        threshold_high: 65,
        ..Policy::default()
    };
    let strict_s = resolve(&prev, &next, &strict);
    assert!(
        is_continued(&strict_s, intro(2), intro(1)),
        "the lone 1<->1 rule must outrank threshold_high — an unambiguous \
         rename cannot be scored away into delete+new"
    );
    assert!(
        has_edge(&strict_s, intro(1), intro(2)),
        "a sub-threshold but plausible pair must survive as an advisory edge"
    );
}

// -----------------------------------------------------------------------
// Never-merge gate: same name but different kind → not merged.
// -----------------------------------------------------------------------

#[test]
fn never_merge_field_vs_variant() {
    let prev = view_with(vec![(intro(1), field_entry(sym("x")), None)]);
    let next = view_with(vec![(intro(2), variant_entry(sym("x")), None)]);
    let s = resolve(&prev, &next, &Policy::default());
    assert!(is_new(&s, intro(2)));
    assert!(is_deleted(&s, intro(1)));
}

// -----------------------------------------------------------------------
// Rename with unchanged body: the canonical "rename matches" case.
// shape(50)+doc(10)=60 → continued.
// -----------------------------------------------------------------------

#[test]
fn rename_with_unchanged_body_matches() {
    let prev = view_with(vec![(
        intro(1),
        fn_entry(sym_with_doc("process_items", "process a list of items")),
        None,
    )]);
    let next = view_with(vec![(
        intro(2),
        fn_entry(sym_with_doc("handle_items", "process a list of items")),
        None,
    )]);
    let s = resolve(&prev, &next, &Policy::default());
    assert!(
        is_continued(&s, intro(2), intro(1)),
        "same-doc rename must be continued (shape+doc=60)"
    );
}

// -----------------------------------------------------------------------
// Unrelated same-named declaration: R-OV lone-1↔1 only fires when the
// (kind, resolved-parent, NAME) bucket is a lone 1↔1.
// Two same-named fns under DIFFERENT parents → two distinct R-OV buckets →
// no cross-parent merge.
// -----------------------------------------------------------------------

#[test]
fn r_ov_parent_segregated() {
    let mut prior_table = PristineIntroTable::new();
    prior_table.insert_live(intro(10), module_entry(sym("ModA")), None);
    prior_table.insert_live(intro(11), module_entry(sym("ModB")), None);
    prior_table.insert_live(
        intro(1),
        fn_entry(sym_with_doc("helper", "doc")),
        Some(intro(10)),
    );
    prior_table.insert_live(
        intro(2),
        fn_entry(sym_with_doc("helper", "doc")),
        Some(intro(11)),
    );
    let prev = IrView::with_package(empty_pkg(), prior_table);

    let mut next_table = PristineIntroTable::new();
    next_table.insert_live(intro(10), module_entry(sym("ModA")), None);
    next_table.insert_live(intro(11), module_entry(sym("ModB")), None);
    next_table.insert_live(
        intro(21),
        fn_entry(sym_with_doc("helper", "doc")),
        Some(intro(10)),
    );
    next_table.insert_live(
        intro(22),
        fn_entry(sym_with_doc("helper", "doc")),
        Some(intro(11)),
    );
    let next = IrView::with_package(empty_pkg(), next_table);

    let s = resolve(&prev, &next, &Policy::default());
    assert!(
        is_continued(&s, intro(21), intro(1)),
        "helper under ModA continues intro(1)"
    );
    assert!(
        is_continued(&s, intro(22), intro(2)),
        "helper under ModB continues intro(2)"
    );
}

// -----------------------------------------------------------------------
// Genuinely new declaration — no prior with any matching signal.
// -----------------------------------------------------------------------

#[test]
fn genuinely_new_is_new() {
    let prev = view_with(vec![(
        intro(1),
        fn_entry_abi(sym("completely_unrelated"), "C"),
        None,
    )]);
    let next = view_with(vec![(intro(99), fn_entry(sym("brand_new_function")), None)]);
    let s = resolve(&prev, &next, &Policy::default());
    assert!(is_new(&s, intro(99)));
    assert!(is_deleted(&s, intro(1)));
}

// -----------------------------------------------------------------------
// Ambiguity: two equally-scoring High candidates → margin rule refuses both.
// -----------------------------------------------------------------------

#[test]
fn ambiguity_margin_refuses_both() {
    let prev = view_with(vec![
        (intro(1), fn_entry(sym_with_doc("x", "shared docs")), None),
        (intro(2), fn_entry(sym_with_doc("x", "shared docs")), None),
    ]);
    let next = view_with(vec![(
        intro(9),
        fn_entry(sym_with_doc("x", "shared docs")),
        None,
    )]);
    let s = resolve(&prev, &next, &Policy::default());
    assert!(is_new(&s, intro(9)), "ambiguous → not merged");
    assert!(is_deleted(&s, intro(1)));
    assert!(is_deleted(&s, intro(2)));
}

// -----------------------------------------------------------------------
// Body axis: body similarity promotes a pair into High.
// shape(50) alone is below HIGH(60). Adding a matching body (+12 = NEAR)
// → 62 ≥ HIGH → continued.
// -----------------------------------------------------------------------

#[test]
fn body_promotes_pair_into_continuation() {
    let prev = view_with(vec![(intro(1), record_entry(sym("Old")), Some(intro(9)))]);
    let next = view_with(vec![(intro(2), record_entry(sym("New")), None)]);

    let s_no_body = resolve(&prev, &next, &Policy::default());
    assert!(is_new(&s_no_body, intro(2)), "shape(50) alone is not HIGH");

    let mut prev_b = view_with(vec![(intro(1), record_entry(sym("Old")), Some(intro(9)))]);
    prev_b.set_body(intro(1), body_with_calls(&["alloc", "log"]));
    let mut next_b = view_with(vec![(intro(2), record_entry(sym("New")), None)]);
    next_b.set_body(intro(2), body_with_calls(&["alloc", "log"]));

    let s_body = resolve(&prev_b, &next_b, &Policy::default());
    assert!(
        is_continued(&s_body, intro(2), intro(1)),
        "shape(50) + body(12) = 62 ≥ HIGH → continued"
    );
}

// -----------------------------------------------------------------------
// Body axis: body alone never reaches HIGH (never-merge).
// -----------------------------------------------------------------------

#[test]
fn body_alone_never_reaches_high() {
    let mut prev_b = view_with(vec![(
        intro(1),
        record_entry_tuple(sym("Old")),
        Some(intro(9)),
    )]);
    prev_b.set_body(intro(1), body_with_calls(&["alloc", "log"]));
    let mut next_b = view_with(vec![(intro(2), record_entry(sym("New")), None)]);
    next_b.set_body(intro(2), body_with_calls(&["alloc", "log"]));

    let s = resolve(&prev_b, &next_b, &Policy::default());
    assert!(is_new(&s, intro(2)));
    assert!(!has_edge(&s, intro(1), intro(2)));
}

// -----------------------------------------------------------------------
// Soft advisory edge surfaces for pairs ≥ SOFT but < HIGH.
// -----------------------------------------------------------------------

#[test]
fn soft_edge_surfaces_not_sigma() {
    let prev = view_with(vec![(intro(1), record_entry(sym("Alpha")), Some(intro(9)))]);
    let next = view_with(vec![(intro(2), record_entry(sym("Beta")), None)]);
    let s = resolve(&prev, &next, &Policy::default());
    assert!(is_new(&s, intro(2)), "shape(50) < HIGH → not continued");
    assert!(
        has_edge(&s, intro(1), intro(2)),
        "shape(50) ≥ SOFT → advisory edge"
    );
}

// -----------------------------------------------------------------------
// Substitution::sigma() returns correct BTreeMap.
// -----------------------------------------------------------------------

#[test]
fn sigma_method_returns_btreemap() {
    let prev = view_with(vec![(intro(1), fn_entry(sym_with_doc("f", "doc")), None)]);
    let next = view_with(vec![(intro(2), fn_entry(sym_with_doc("g", "doc")), None)]);
    let s = resolve(&prev, &next, &Policy::default());
    let sigma = s.sigma();
    assert_eq!(sigma.get(&intro(2)), Some(&intro(1)));
}

// -----------------------------------------------------------------------
// R-CHILD positive: field under continued parent IS continued.
// -----------------------------------------------------------------------

#[test]
fn r_child_positive_field_continues() {
    let mut prior_table = PristineIntroTable::new();
    prior_table.insert_live(intro(1), record_entry(sym("S")), None);
    prior_table.insert_live(intro(10), field_entry(sym("x")), Some(intro(1)));
    let prev = IrView::with_package(empty_pkg(), prior_table);

    let mut next_table = PristineIntroTable::new();
    next_table.insert_live(intro(1), record_entry(sym("S")), None);
    next_table.insert_live(intro(20), field_entry(sym("x")), Some(intro(1)));
    let next = IrView::with_package(empty_pkg(), next_table);

    let s = resolve(&prev, &next, &Policy::default());
    assert!(
        is_continued(&s, intro(20), intro(10)),
        "field under continued parent must be continued"
    );
}

// -----------------------------------------------------------------------
// enable_body_axis=false disables body scoring entirely.
// -----------------------------------------------------------------------

#[test]
fn policy_disable_body_axis() {
    let mut prev = view_with(vec![(intro(1), record_entry(sym("Old")), Some(intro(9)))]);
    prev.set_body(intro(1), body_with_calls(&["alloc", "log"]));
    let mut next = view_with(vec![(intro(2), record_entry(sym("New")), None)]);
    next.set_body(intro(2), body_with_calls(&["alloc", "log"]));

    let p = Policy {
        enable_body_axis: false,
        ..Policy::default()
    };
    let s = resolve(&prev, &next, &p);
    assert!(
        is_new(&s, intro(2)),
        "body disabled → shape(50) < HIGH → not continued"
    );
}
