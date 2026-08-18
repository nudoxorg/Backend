//! Seam-level tests for the producer contract.
//!
//! No real language toolchain is available in CI, so these tests use a fake
//! in-crate [`Producer`] whose `invoke` returns a canned struct and whose
//! `lower` declares a couple of entries.  The tests verify that:
//!
//! 1. A well-formed producer drives through `produce` to a non-empty
//!    [`PristineIntroTable`].
//! 2. A `refer` whose target is never declared surfaces as
//!    [`ProducerError::LoweringFailed`].
//! 3. The `ProducerError` display messages name the package.

use nudox_ir::{
    apply::PristineIntroTable,
    body::Language,
    change::{EcosystemId, PackageLineageId, PackageName},
    entry::Symbol,
    kinds::{Field, FieldKey, Module, Record, Type},
    lower::{Lowering, LoweringError},
};

use crate::{
    DegradedYield, PackageSource, Producer, ProducerError, ProducerId, YieldContract, produce,
};

// ── Helpers ───────────────────────────────────────────────────────────────────

fn test_lineage() -> PackageLineageId {
    PackageLineageId::new(
        EcosystemId::new("test"),
        PackageName::new("nudox-producer-test"),
    )
}

fn test_src() -> PackageSource {
    PackageSource::new("/tmp/fake", "fake-pkg", "0.1.0")
}

fn sym(name: &str) -> Symbol {
    Symbol {
        name: name.to_owned(),
        visibility: nudox_ir::entry::Visibility::Public,
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

// ── FakeProducer — happy path ─────────────────────────────────────────────────

/// A canned oracle output type: one record with two fields.
struct CannedOracle;

/// A producer that declares: root module → Record("Point") → Field("x"), Field("y").
struct FakeProducer;

impl Producer for FakeProducer {
    type Id = &'static str;
    type Oracle = CannedOracle;

    const ID: ProducerId = ProducerId("fake/1");
    const LANGUAGE: Language = Language::Go;

    fn invoke(&self, _src: &PackageSource) -> Result<CannedOracle, ProducerError> {
        Ok(CannedOracle)
    }

    fn lower(
        &self,
        _oracle: &CannedOracle,
        out: &mut Lowering<&'static str>,
    ) -> Result<(), ProducerError> {
        // Forward-refer the fields so they can be named in the Record.
        let x_ref = out.refer::<Field>("point.x");
        let y_ref = out.refer::<Field>("point.y");

        out.declare(
            "Point",
            None,
            sym("Point"),
            Record::builder().fields([x_ref, y_ref]).build(),
        );
        out.declare(
            "point.x",
            Some("Point"),
            sym("x"),
            Field::builder().key(FieldKey::Named).ty(Type::I32).build(),
        );
        out.declare(
            "point.y",
            Some("Point"),
            sym("y"),
            Field::builder().key(FieldKey::Named).ty(Type::I64).build(),
        );
        Ok(())
    }
}

#[test]
fn fake_producer_seals_non_empty_table() {
    let table: PristineIntroTable = produce(
        &FakeProducer,
        &test_src(),
        &test_lineage(),
        &nudox_ir::foreign::Unlinked,
    )
    .expect("produce must succeed")
    .table;

    // root module + Point + x + y = 4 entries.
    assert_eq!(table.len(), 4, "expected root + Point + x + y");

    // Every entry has at most one parent (the root has no parent).
    let roots = table
        .iter()
        .filter(|(id, _)| table.parent_of(*id).is_none())
        .count();
    assert_eq!(roots, 1, "exactly one root");
}

// ── BrokenProducer — lowering error surfaces correctly ───────────────────────

/// A producer that refers to an ID it never declares.
struct BrokenProducer;

impl Producer for BrokenProducer {
    type Id = u32;
    type Oracle = ();

    const ID: ProducerId = ProducerId("broken/1");
    const LANGUAGE: Language = Language::Rust;

    fn invoke(&self, _src: &PackageSource) -> Result<(), ProducerError> {
        Ok(())
    }

    fn lower(&self, _oracle: &(), out: &mut Lowering<u32>) -> Result<(), ProducerError> {
        // Refer to id 99 — never declare it.
        let _dangling = out.refer::<Module>(99);
        Ok(())
    }
}

#[test]
fn undeclared_refer_surfaces_as_lowering_failed() {
    let err = produce(
        &BrokenProducer,
        &test_src(),
        &test_lineage(),
        &nudox_ir::foreign::Unlinked,
    )
    .expect_err("BrokenProducer must fail");

    match &err {
        ProducerError::LoweringFailed { package, source } => {
            assert_eq!(package, "fake-pkg", "error must name the package");

            // Verify the underlying LoweringError is preserved in the chain.
            // BrokenProducer refers to ID 99 but never declares it, so we expect
            // Undeclared with that ID. By asserting on the typed variant, we verify
            // the error chain is properly preserved — a stronger contract than checking
            // the Display message (doctrine §4: "never assert on a message string where
            // you can assert on a typed variant").
            let lowering_err = source
                .downcast_ref::<LoweringError<u32>>()
                .expect("source must be a LoweringError<u32>");

            match lowering_err {
                LoweringError::Undeclared(ids) => {
                    assert_eq!(ids, &vec![99], "must report the undeclared ID 99");
                }
                other => panic!("expected Undeclared variant, got: {other:?}"),
            }
        }
        other => panic!("expected LoweringFailed, got: {other}"),
    }
}

// ── Display sanity ────────────────────────────────────────────────────────────

#[test]
fn producer_error_display_includes_package() {
    let err = ProducerError::Decode {
        package: "my-pkg".to_owned(),
        reason: serde_json::from_str::<serde_json::Value>("invalid").unwrap_err(),
    };
    let msg = err.to_string();
    assert!(msg.contains("my-pkg"), "Display must contain package name");
}

// ── Yield contract ────────────────────────────────────────────────────────────
//
// The adversarial axis here is *how much a producer contributes* crossed with
// *what it declared it would contribute*. All four cells are covered:
//
//                          | contributes 0        | contributes ≥ 1
//   Declarations (default) | NoDeclarations…      | Ok  (FakeProducer above)
//   RootOnly (declared)    | Ok, marked degraded  | YieldContractOutgrown
//
// The `contributes 0` column is the empty-package case: a producer whose
// package genuinely has no public API is indistinguishable at this seam from
// one that never ran, which is why the left column is never a silent success.

/// Contributes exactly the root `produce` synthesized, and nothing else — the
/// shape 26 of 154 corpus entries had while being counted as successes.
struct SilentlyEmptyProducer;

impl Producer for SilentlyEmptyProducer {
    type Id = u32;
    type Oracle = ();

    const ID: ProducerId = ProducerId("silently-empty/1");
    const LANGUAGE: Language = Language::Python;

    fn invoke(&self, _src: &PackageSource) -> Result<(), ProducerError> {
        Ok(())
    }

    fn lower(&self, _oracle: &(), _out: &mut Lowering<u32>) -> Result<(), ProducerError> {
        Ok(())
    }
}

/// The minimum a producer can contribute and still be doing its job: one
/// declaration beyond the synthesized root.
struct SingleDeclarationProducer;

impl Producer for SingleDeclarationProducer {
    type Id = u32;
    type Oracle = ();

    const ID: ProducerId = ProducerId("single-declaration/1");
    const LANGUAGE: Language = Language::Go;

    fn invoke(&self, _src: &PackageSource) -> Result<(), ProducerError> {
        Ok(())
    }

    fn lower(&self, _oracle: &(), out: &mut Lowering<u32>) -> Result<(), ProducerError> {
        out.declare(1, None, sym("only"), Module);
        Ok(())
    }
}

/// Declares its own degradation and honours it.
struct HonestlyDegradedProducer;

impl Producer for HonestlyDegradedProducer {
    type Id = u32;
    type Oracle = ();

    const ID: ProducerId = ProducerId("honestly-degraded/1");
    const LANGUAGE: Language = Language::Python;

    fn invoke(&self, _src: &PackageSource) -> Result<(), ProducerError> {
        Ok(())
    }

    fn yield_contract(&self) -> YieldContract {
        YieldContract::RootOnly(DegradedYield::new(Self::ID, "no oracle in this build"))
    }

    fn lower(&self, _oracle: &(), _out: &mut Lowering<u32>) -> Result<(), ProducerError> {
        Ok(())
    }
}

/// Declares degradation and then contributes anyway — the shape a producer
/// takes on the day its blocker is fixed and nobody retracts the claim.
struct RepairedButStillDeclaringDegradation;

impl Producer for RepairedButStillDeclaringDegradation {
    type Id = u32;
    type Oracle = ();

    const ID: ProducerId = ProducerId("repaired-stale-claim/1");
    const LANGUAGE: Language = Language::Python;

    fn invoke(&self, _src: &PackageSource) -> Result<(), ProducerError> {
        Ok(())
    }

    fn yield_contract(&self) -> YieldContract {
        YieldContract::RootOnly(DegradedYield::new(
            Self::ID,
            "pyrefly-shaped blocker that has in fact been removed",
        ))
    }

    fn lower(&self, _oracle: &(), out: &mut Lowering<u32>) -> Result<(), ProducerError> {
        out.declare(1, None, sym("real_module"), Module);
        out.declare(2, None, sym("another"), Module);
        Ok(())
    }
}

#[test]
fn producer_contributing_only_the_synthesized_root_is_rejected_not_counted_as_success() {
    let err = produce(
        &SilentlyEmptyProducer,
        &test_src(),
        &test_lineage(),
        &nudox_ir::foreign::Unlinked,
    )
    .expect_err("a producer that declared nothing must not seal a 'successful' table");

    match err {
        ProducerError::NoDeclarationsContributed {
            package,
            producer,
            source,
        } => {
            assert_eq!(package, "fake-pkg");
            assert_eq!(producer, SilentlyEmptyProducer::ID);
            assert!(
                source.is_none(),
                "the generic gate has no cause to offer — the absence of one is the failure"
            );
        }
        other => panic!("expected NoDeclarationsContributed, got {other:?}"),
    }
}

#[test]
fn one_declaration_beyond_the_root_satisfies_the_default_contract() {
    // The boundary the gate turns on: `SilentlyEmptyProducer` above and this
    // producer differ by exactly one `declare` call, and land on opposite sides.
    let produced = produce(
        &SingleDeclarationProducer,
        &test_src(),
        &test_lineage(),
        &nudox_ir::foreign::Unlinked,
    )
    .expect("one declaration is enough");

    assert_eq!(produced.table.len(), 2, "root + the one declaration");
    assert!(
        !produced.contract.is_degraded(),
        "a producer that said nothing gets the strong contract by default"
    );
}

#[test]
fn a_declared_degradation_travels_on_the_output_rather_than_being_erased() {
    let produced = produce(
        &HonestlyDegradedProducer,
        &test_src(),
        &test_lineage(),
        &nudox_ir::foreign::Unlinked,
    )
    .expect("a producer that declares its degradation and honours it may still produce");

    // The point of the whole exercise: this `Ok` is *not* interchangeable with
    // the one above. A consumer tallying successes can tell them apart without
    // knowing which producer ran.
    let degraded = produced
        .contract
        .degraded()
        .expect("the degradation must survive onto the sealed output");
    assert_eq!(degraded.producer(), HonestlyDegradedProducer::ID);
    assert_eq!(degraded.blocker(), "no oracle in this build");
    assert_eq!(produced.table.len(), 1, "root only, as declared");
}

#[test]
fn a_degraded_producer_that_starts_contributing_fails_instead_of_keeping_a_stale_claim() {
    let err = produce(
        &RepairedButStillDeclaringDegradation,
        &test_src(),
        &test_lineage(),
        &nudox_ir::foreign::Unlinked,
    )
    .expect_err("a RootOnly declaration must not survive the repair it describes");

    match err {
        ProducerError::YieldContractOutgrown {
            package,
            producer,
            contributed,
            declared,
        } => {
            assert_eq!(package, "fake-pkg");
            assert_eq!(producer, RepairedButStillDeclaringDegradation::ID);
            assert_eq!(
                contributed, 2,
                "the count must be the declarations beyond the root, not the table length"
            );
            assert_eq!(
                declared.blocker(),
                "pyrefly-shaped blocker that has in fact been removed",
                "the now-false claim is handed back so a reader can check it"
            );
        }
        other => panic!("expected YieldContractOutgrown, got {other:?}"),
    }
}

#[test]
fn the_gate_runs_before_seal_so_a_lowering_bug_still_reports_itself_first() {
    // Ordering matters: `BrokenProducer` contributes nothing *and* refers to an
    // undeclared id. If the yield gate ran before `finish`, the structural bug
    // would be masked by "contributed nothing", sending the reader to the
    // oracle instead of to the `refer` call. `undeclared_refer_surfaces_as_
    // lowering_failed` above pins the variant; this pins that the new gate did
    // not reorder itself in front of it.
    assert!(matches!(
        produce(
            &BrokenProducer,
            &test_src(),
            &test_lineage(),
            &nudox_ir::foreign::Unlinked
        ),
        Err(ProducerError::LoweringFailed { .. })
    ));
}

#[test]
fn oracle_exit_display_includes_stderr() {
    let err = ProducerError::OracleExit {
        command: "go run .".to_owned(),
        code: "1".to_owned(),
        stderr: "cannot find module".to_owned(),
    };
    let msg = err.to_string();
    assert!(msg.contains("go run ."), "Display must contain command");
    assert!(
        msg.contains("cannot find module"),
        "Display must contain stderr"
    );
}

// ── Identity contract ─────────────────────────────────────────────────────────

/// A producer that declares two records with the same name, under the same
/// parent, at the same (degenerate) span.
///
/// This is not a synthetic corner: it is the exact shape three real producers
/// emit today, because `go/src/lower/mod.rs`, `csharp/src/lower.rs` and
/// `python/src/emit/mod.rs` all hardcode `0..0` for every declaration's span.
/// Two same-named declarations from any of them collapse the `Span`
/// disambiguator to nothing and force `seal` down to `Disambiguator::Ordinal`
/// — an identity that is a function of emission order rather than of content.
struct DegenerateSpanProducer;

impl Producer for DegenerateSpanProducer {
    type Id = u32;
    type Oracle = ();

    const ID: ProducerId = ProducerId("degenerate-span/1");
    const LANGUAGE: Language = Language::Go;

    fn invoke(&self, _src: &PackageSource) -> Result<(), ProducerError> {
        Ok(())
    }

    fn lower(&self, _oracle: &(), out: &mut Lowering<u32>) -> Result<(), ProducerError> {
        // `sym` above builds every symbol with `span: 0..0`, which is the point.
        out.declare(1, None, sym("Dup"), Record::builder().build());
        out.declare(2, None, sym("Dup"), Record::builder().build());
        Ok(())
    }
}

/// Seal `DegenerateSpanProducer`'s package and hand back the outcome, so both
/// policy tests below measure the same input.
fn degenerate_span_outcome() -> nudox_ir::package::SealOutcome {
    let mut sink: Lowering<u32> = Lowering::new(
        nudox_ir::package::PackageId::path("/tmp/fake"),
        sym("fake-pkg"),
    );
    DegenerateSpanProducer
        .lower(&(), &mut sink)
        .expect("lowering a two-record package cannot fail");
    sink.finish()
        .expect("the package is structurally valid; only its identities collide")
        .seal(&test_lineage(), &nudox_ir::foreign::Unlinked)
}

/// Two declarations that differ only by emission order must be *recorded* as
/// such by `seal`, not silently separated.
///
/// The precondition for both policy tests below, and a real assertion in its
/// own right: if `seal` ever stopped escalating to `Ordinal` here — by growing
/// a better disambiguator, which is the outcome we want — the two tests after
/// this one would start passing for a reason that has nothing to do with the
/// gate, and this one going red is what says so.
#[test]
fn a_degenerate_span_collision_is_recorded_as_an_ordinal_escalation() {
    let outcome = degenerate_span_outcome();

    assert!(
        outcome.report.collisions.is_empty(),
        "the escalation ladder reaches Ordinal, so nothing should have been dropped"
    );
    assert_eq!(
        outcome.table.len(),
        3,
        "root + two records: both declarations must survive with distinct ids"
    );

    let ordinal: Vec<&nudox_ir::package::ForcedDisambiguation> = outcome
        .report
        .forced
        .iter()
        .filter(|forced| forced.escalated_to == nudox_ir::package::Escalation::Ordinal)
        .collect();
    assert_eq!(
        ordinal.len(),
        1,
        "exactly one colliding group, reported once — `seal` re-mints whole groups, so \
         `forced` must not carry one row per escalation round; got {:?}",
        outcome.report.forced,
    );
    assert_eq!(ordinal[0].name, "Dup");
    assert_eq!(
        ordinal[0].group, 2,
        "the group size is the number of declarations that shared one key"
    );
}

/// Under the policy the workspace runs today, an ordinal-keyed identity travels
/// on the report and does not fail the run.
///
/// This is the *tolerated* half of the identity contract, and it is asserted
/// rather than assumed so that flipping `OrdinalPolicy::CURRENT` cannot happen
/// by accident: the day someone does, this test goes red and tells them which
/// promise they changed.
#[test]
fn an_ordinal_keyed_identity_is_tolerated_under_the_report_policy() {
    let outcome = crate::enforce_identity_contract_under(
        &test_src(),
        degenerate_span_outcome(),
        crate::OrdinalPolicy::Report,
    )
    .expect("the Report policy must let an ordinal-keyed identity through");

    assert_eq!(outcome.table.len(), 3, "the table passes through unchanged");
    assert!(
        outcome
            .report
            .forced
            .iter()
            .any(|forced| forced.escalated_to == nudox_ir::package::Escalation::Ordinal),
        "tolerated is not the same as erased: the report must still say it happened"
    );
}

/// Under the strict policy the same input is a typed failure carrying the group
/// that has no content-derived identity.
///
/// Doctrine §8: "verify the guard by mutation — break the repair verdict
/// deliberately and confirm the suite goes red. A guard nobody has watched fail
/// is a guard nobody has tested." Without this test the strict half of the gate
/// would be unexecuted code until three producers in three languages were
/// fixed.
#[test]
fn an_ordinal_keyed_identity_is_a_typed_failure_under_the_reject_policy() {
    let error = crate::enforce_identity_contract_under(
        &test_src(),
        degenerate_span_outcome(),
        crate::OrdinalPolicy::Reject,
    )
    .expect_err("the Reject policy must refuse an identity that rests on emission order");

    let ProducerError::IdentityNotInjective {
        package,
        lost,
        order_dependent,
    } = error
    else {
        panic!("expected IdentityNotInjective, got {error:?}");
    };

    assert_eq!(package, "fake-pkg");
    assert!(
        lost.is_empty(),
        "nothing was dropped here — the ladder reached Ordinal — so the dropped list must \
         be empty and the failure must come from the order-dependent list alone"
    );
    assert_eq!(order_dependent.len(), 1);
    assert_eq!(order_dependent[0].name, "Dup");
    assert_eq!(order_dependent[0].group, 2);
}

/// A package whose identities are all content-derived passes through untouched.
///
/// The other half of the guard: a gate that fails everything is as useless as
/// one that fails nothing, and `FakeProducer`'s four distinct declarations are
/// the case the corpus is supposed to be made of.
#[test]
fn a_clean_seal_passes_the_identity_gate_unchanged() {
    let mut sink: Lowering<&'static str> = Lowering::new(
        nudox_ir::package::PackageId::path("/tmp/fake"),
        sym("fake-pkg"),
    );
    FakeProducer
        .lower(&CannedOracle, &mut sink)
        .expect("lowering must succeed");
    let sealed = sink
        .finish()
        .expect("finish must succeed")
        .seal(&test_lineage(), &nudox_ir::foreign::Unlinked);
    let before = sealed.table.len();

    let outcome = crate::enforce_identity_contract(&test_src(), sealed)
        .expect("a package with four distinct declarations must pass the identity gate");

    assert_eq!(outcome.table.len(), before, "the table is not modified");
    assert!(
        outcome.report.forced.is_empty(),
        "no declaration in this package needed an escalated disambiguator"
    );
}

/// The failure message must name the package and the specific declaration
/// whose identity is not content-derived, not merely count them.
///
/// Asserted on the rendered message rather than on the variant — which the two
/// tests above already do — because the rendering is the thing under test here.
/// `IntroCollision` and `ForcedDisambiguation` are kept whole on the variant
/// precisely so this text can exist; a message that said only "1 collision"
/// would make carrying them pointless, and the reader's next action is always
/// to open the declaration it names.
#[test]
fn identity_failure_message_names_the_package_and_the_declaration() {
    let mut sink: Lowering<u32> = Lowering::new(
        nudox_ir::package::PackageId::path("/tmp/fake"),
        sym("fake-pkg"),
    );
    DegenerateSpanProducer
        .lower(&(), &mut sink)
        .expect("lowering must succeed");
    let sealed = sink
        .finish()
        .expect("finish must succeed")
        .seal(&test_lineage(), &nudox_ir::foreign::Unlinked);

    let error =
        crate::enforce_identity_contract_under(&test_src(), sealed, crate::OrdinalPolicy::Reject)
            .expect_err("must fail");

    let message = error.to_string();
    assert!(
        message.contains("fake-pkg"),
        "message must name the package: {message}"
    );
    assert!(
        message.contains("Dup"),
        "message must name the declaration whose identity is not content-derived: {message}"
    );
    assert!(
        message.contains("order-dependent"),
        "message must say which half of the contract failed: {message}"
    );
}
