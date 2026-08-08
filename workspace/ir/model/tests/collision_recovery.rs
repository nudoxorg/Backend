//! Two declarations must never quietly become one.
//!
//! # The defect these tests pin
//!
//! `PristineIntroTable::insert_live` was a bare `HashMap::insert` whose
//! displaced entry was dropped on the floor, under a doc comment asserting
//! "the `seal` pass inserts each intro exactly once". That premise was false.
//!
//! `seal` selected a `Disambiguator` from the *structural* skeleton for
//! functions and impls, and from the source span for everything else — with no
//! fallthrough when the structural payload turned out to be degenerate and no
//! tier below `Span` when spans were equal. Anything that fell off the end of
//! that ladder was silently overwritten:
//!
//! * 28 real `Function` entries in Gson (~4.3% of the package) whose parameters
//!   had all been erased to `Type::Any`, so four `fromJson` overloads encoded to
//!   four byte-identical skeletons.
//! * Synthesised parameters that share a name, a parent name and a `0..0` span —
//!   the shape behind the Rust producer's 416-functions-to-188-identities
//!   measurement.
//!
//! Both are now recovered by `seal`'s escalation pass, and both are *reported*
//! in `SealReport::forced` rather than inferred.

use nudox_ir::{
    build::*,
    change::{EcosystemId, PackageName},
    foreign::Unlinked,
    kinds::ty::Type,
    package::Escalation,
};

fn lineage() -> PackageLineageId {
    PackageLineageId::new(EcosystemId::new("cargo"), PackageName::new("collide"))
}

/// A symbol whose span carries no information — the case that defeats the
/// `Span` tier. Real producers hit this two ways: synthesised entries (the Rust
/// producer's `plain_sym` uses `0..0`) and line-granular oracles (Java reports
/// `0..line`, so two declarations on one line are identical).
fn spanless(name: &str) -> Symbol {
    Symbol {
        name: name.to_owned(),
        visibility: Visibility::Public,
        documentation: String::new(),
        source: std::path::PathBuf::new(),
        span: 0..0,
        aliases: Box::new([]),
        deprecation: None,
        doc_links: Box::new([]),
        attrs: Box::new([]),
        cfg: None,
    }
}

/// Two functions with the same name under the same parent, whose parameters
/// have all been erased to `Type::Any`, mint identical signature skeletons.
///
/// This is the Gson `fromJson` shape reduced to its mechanism: four overloads
/// that differ only in parameter types the producer could not name. Before the
/// escalation pass, three of the four were `HashMap::insert`-ed away and the
/// package shipped without them.
#[test]
fn overloads_with_fully_erased_parameters_all_survive_seal() {
    let mut next = 0usize;
    let mut id = move || {
        next += 1;
        next
    };

    // Four `fromJson` overloads, every parameter erased to `Type::Any`, all
    // declared at the same (uninformative) span.
    let pkg = IrPackage::build(PackageId::path("collide"), spanless("gson"), |mut root| {
        root.create(id(), spanless("Gson"), |mut cls| {
            for _ in 0..4 {
                cls.create(id(), spanless("fromJson"), |mut f| {
                    let a = f.create(id(), spanless("a"), |_| {
                        Param::builder().ty(Type::Any).build()
                    });
                    let b = f.create(id(), spanless("b"), |_| {
                        Param::builder().ty(Type::Any).build()
                    });
                    Function::builder().input_params([a, b]).build()
                });
            }
            Record::builder().build()
        });
    });

    let outcome = pkg.seal(&lineage(), &Unlinked);

    // root + Gson + 4 × (fromJson + a + b) = 14. Assert on the surviving
    // declarations, not on a count alone: a count is satisfiable by a table
    // full of the wrong entries.
    let overloads: Vec<_> = outcome
        .table
        .iter()
        .filter(|(_, e)| e.sym().name == "fromJson")
        .map(|(i, _)| i)
        .collect();
    assert_eq!(
        overloads.len(),
        4,
        "all four erased-parameter overloads must survive seal as distinct \
         entries; got {} — the rest were silently overwritten",
        overloads.len()
    );

    let mut distinct = overloads.clone();
    distinct.sort();
    distinct.dedup();
    assert_eq!(
        distinct.len(),
        4,
        "the four overloads must hold four distinct IntroIds"
    );

    // Both `a` params and both `b` params also collide (same name, same parent
    // *name*, same span) and must equally survive.
    for param in ["a", "b"] {
        let ids: Vec<_> = outcome
            .table
            .iter()
            .filter(|(_, e)| e.sym().name == param)
            .map(|(i, _)| i)
            .collect();
        assert_eq!(
            ids.len(),
            4,
            "all four `{param}` params must survive; got {}",
            ids.len()
        );
    }

    assert_eq!(outcome.table.len(), 14, "root + Gson + 4 × (fn + 2 params)");

    // The recovery is *reported*, not silent. This is the difference between
    // "seal fixed it" and "seal noticed it": a producer author has to be able
    // to see that their type lowering erased something load-bearing.
    assert!(
        !outcome.report.forced.is_empty(),
        "escalating a degenerate disambiguator must be reported in \
         SealReport::forced, not inferred from the entry count"
    );
    let fn_forced: Vec<_> = outcome
        .report
        .forced
        .iter()
        .filter(|f| f.name == "fromJson")
        .collect();
    assert_eq!(
        fn_forced.len(),
        1,
        "one forced group for the fromJson overload set; got {:?}",
        outcome.report.forced
    );
    assert_eq!(fn_forced[0].group, 4, "the group held all four overloads");
    // Spans are identical here, so `Span` cannot separate them and the terminal
    // `Ordinal` tier is the one that must fire.
    assert_eq!(
        fn_forced[0].escalated_to,
        Escalation::Ordinal,
        "identical spans must escalate past Span to the terminal Ordinal tier"
    );

    assert!(
        outcome.report.collisions.is_empty(),
        "nothing may be lost after escalation; lost: {:?}",
        outcome.report.collisions
    );
}

/// When declarations *do* carry distinct spans, the `Span` tier is enough and
/// the non-content-derived `Ordinal` tier must not fire.
///
/// This matters because an `Ordinal` id is not stable across a producer
/// reordering its output, so it has to be a last resort rather than the
/// mechanism.
#[test]
fn distinct_spans_stop_at_the_span_tier() {
    let mut next = 0usize;
    let mut id = move || {
        next += 1;
        next
    };

    let pkg = IrPackage::build(PackageId::path("collide"), spanless("lib"), |mut root| {
        root.create(id(), spanless("Holder"), |mut cls| {
            for line in [10usize, 20, 30] {
                let mut s = spanless("overload");
                s.span = line..line + 5;
                cls.create(id(), s, |mut f| {
                    let p = f.create(id(), spanless("arg"), |_| {
                        Param::builder().ty(Type::Any).build()
                    });
                    Function::builder().input_params([p]).build()
                });
            }
            Record::builder().build()
        });
    });

    let outcome = pkg.seal(&lineage(), &Unlinked);

    let overloads: Vec<_> = outcome
        .table
        .iter()
        .filter(|(_, e)| e.sym().name == "overload")
        .collect();
    assert_eq!(overloads.len(), 3, "all three overloads survive");

    let fn_forced: Vec<_> = outcome
        .report
        .forced
        .iter()
        .filter(|f| f.name == "overload")
        .collect();
    assert_eq!(fn_forced.len(), 1);
    assert_eq!(
        fn_forced[0].escalated_to,
        Escalation::Span,
        "distinct spans separate the group; Ordinal must not be reached"
    );
}

/// A clean package reports nothing, so a non-empty report is always a real
/// signal rather than routine noise.
#[test]
fn a_package_with_no_collisions_reports_nothing() {
    let mut next = 0usize;
    let mut id = move || {
        next += 1;
        next
    };

    let pkg = IrPackage::build(PackageId::path("collide"), spanless("lib"), |mut root| {
        root.create(id(), spanless("alpha"), |_| Record::builder().build());
        root.create(id(), spanless("beta"), |_| Record::builder().build());
    });

    let outcome = pkg.seal(&lineage(), &Unlinked);
    assert_eq!(outcome.table.len(), 3);
    assert!(
        outcome.report.is_clean(),
        "a package with distinct names must seal clean; got {:?}",
        outcome.report
    );
    assert!(outcome.report.forced.is_empty());
    assert!(outcome.report.unmapped_local.is_empty());
    assert!(outcome.report.collisions.is_empty());
}

/// `try_insert_live` hands the rejected declaration back by value instead of
/// dropping it, and leaves the children index untouched.
///
/// The second half is a corruption that was never reported: the old
/// implementation pushed into `children[parent]` even when it overwrote, so
/// after a collision `children_of(parent)` held the same `IntroId` twice while
/// `map` held one entry — silently violating the table's own stated invariant.
#[test]
fn try_insert_live_returns_the_rejected_entry_and_keeps_children_consistent() {
    use nudox_ir::{apply::PristineIntroTable, entry::Node, kind::EntryKind, kinds::Module};

    let mut table = PristineIntroTable::new();
    let parent = nudox_ir::change::IntroId::from_raw([0xaa; 32]);
    let intro = nudox_ir::change::IntroId::from_raw([0xbb; 32]);

    let mk = |name: &str| {
        Entry::new(
            spanless(name),
            Node::build(None::<nudox_ir::index::RawRef>, []),
            Module.into_kind(),
        )
    };

    table
        .try_insert_live(intro, mk("incumbent"), Some(parent))
        .expect("first insert is fresh");
    let err = table
        .try_insert_live(intro, mk("newcomer"), Some(parent))
        .expect_err("a second claim on one IntroId must be refused");

    assert_eq!(err.incumbent.name, "incumbent");
    assert_eq!(
        err.rejected.sym().name,
        "newcomer",
        "the rejected entry must come back intact — it is the only copy"
    );
    assert_eq!(
        table.get(intro).expect("incumbent stays live").sym().name,
        "incumbent",
        "refusing means the incumbent is kept, not replaced"
    );
    assert_eq!(
        table.children_of(parent).len(),
        1,
        "a refused insert must not push a phantom child edge"
    );
}
