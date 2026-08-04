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
    lower::Lowering,
};

use crate::{PackageSource, ProducerError, ProducerId, Producer, produce};

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
        produce(&FakeProducer, &test_src(), &test_lineage()).expect("produce must succeed");

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

    fn lower(
        &self,
        _oracle: &(),
        out: &mut Lowering<u32>,
    ) -> Result<(), ProducerError> {
        // Refer to id 99 — never declare it.
        let _dangling = out.refer::<Module>(99);
        Ok(())
    }
}

#[test]
fn undeclared_refer_surfaces_as_lowering_failed() {
    let err = produce(&BrokenProducer, &test_src(), &test_lineage())
        .expect_err("BrokenProducer must fail");

    match &err {
        ProducerError::LoweringFailed { package, detail } => {
            assert_eq!(
                package, "fake-pkg",
                "error must name the package"
            );
            assert!(
                !detail.is_empty(),
                "detail must carry the LoweringError description"
            );
        }
        other => panic!("expected LoweringFailed, got: {other}"),
    }

    // The Display impl must include the package name.
    let msg = err.to_string();
    assert!(
        msg.contains("fake-pkg"),
        "Display output must name the package; got: {msg}"
    );
}

// ── Display sanity ────────────────────────────────────────────────────────────

#[test]
fn producer_error_display_includes_package() {
    let err = ProducerError::Decode {
        package: "my-pkg".to_owned(),
        reason: "unexpected end of JSON".to_owned(),
    };
    let msg = err.to_string();
    assert!(msg.contains("my-pkg"), "Display must contain package name");
    assert!(msg.contains("unexpected end of JSON"), "Display must contain reason");
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
    assert!(msg.contains("cannot find module"), "Display must contain stderr");
}
