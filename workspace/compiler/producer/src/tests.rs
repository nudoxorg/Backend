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
    let table: PristineIntroTable =
        produce(&FakeProducer, &test_src(), &test_lineage(), &nudox_ir::foreign::Unlinked).expect("produce must succeed").table;

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
    let err = produce(&BrokenProducer, &test_src(), &test_lineage(), &nudox_ir::foreign::Unlinked)
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
