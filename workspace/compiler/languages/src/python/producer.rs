//! [`PythonProducer`] — implements [`crate::Producer`].
//!
//! # Oracle strategy (two modes)
//!
//! **With `pyrefly` feature (default since 2026-08-09):** `invoke` calls
//! [`crate::python::context::invoke_oracle`], which runs the same syntactic walk for
//! structure and then loads every discovered module into a pyrefly `State`,
//! commits one transaction so imports resolve across files, and fills the
//! type slots the syntactic tier left unresolvable. The `State` is dropped
//! before `invoke` returns, so no pyrefly lifetime escapes into the owned
//! `PythonOracle` — the same discipline `oracle.rs` documents.
//!
//! **Without `pyrefly` feature (`--no-default-features`):** `invoke` calls
//! [`crate::python::syntax::build_oracle`], which walks every `.py` file under the
//! package root, parses each with `ruff_python_parser` in-process, and
//! extracts a real `PythonOracle` from the syntax tree — see `syntax.rs` for
//! what is and is not represented this way. This producer takes the trait
//! default [`Producer::yield_contract`] (`YieldContract::Declarations`): a
//! run that ends with only the synthesized root is now a genuine failure,
//! not a documented degradation, exactly like every other producer on this
//! program.
//!
//! Both modes therefore produce declarations, and both are held to the same
//! `YieldContract::Declarations`. The tests below deliberately carry no
//! `cfg(feature)` gate for that reason: whichever tier `invoke` dispatches to,
//! a real package must lower and a missing root must be a typed error.

use crate::{PackageSource, Producer, ProducerError, ProducerId};
use nudox_ir::body::Language;

use crate::python::{emit::emit_package, oracle::PythonId, oracle::PythonOracle};
use nudox_ir::lower::Lowering;

// ---------------------------------------------------------------------------
// PythonProducer
// ---------------------------------------------------------------------------

/// Python package producer (pyrefly in-process oracle).
///
/// Stateless: no fields. Construct with `PythonProducer` and call `produce`.
#[derive(Debug, Default, Clone, Copy)]
pub struct PythonProducer;

impl Producer for PythonProducer {
    type Id = PythonId;
    type Oracle = PythonOracle;

    /// `"python-pyrefly/1"` — bump the `N` suffix when the output shape changes
    /// (e.g. when new IR kinds are emitted or field semantics change).
    const ID: ProducerId = ProducerId("python-pyrefly/1");

    const LANGUAGE: Language = Language::Python;

    fn invoke(&self, src: &PackageSource) -> Result<Self::Oracle, ProducerError> {
        #[cfg(feature = "pyrefly")]
        {
            crate::python::context::invoke_oracle(src)
        }
        #[cfg(not(feature = "pyrefly"))]
        {
            crate::python::syntax::build_oracle(src)
        }
    }

    // No `yield_contract` override: the trait default
    // (`YieldContract::Declarations`, `workspace/compiler/producer/src/lib.rs`)
    // applies unconditionally now that `invoke` genuinely reads `src` in both
    // build configurations. `enforce_yield_contract` (called from `produce`)
    // therefore rejects any package this producer contributes zero
    // declarations for, the same way every other producer on this program is
    // held to its output.

    fn lower(
        &self,
        oracle: &Self::Oracle,
        out: &mut Lowering<Self::Id>,
    ) -> Result<(), ProducerError> {
        emit_package(oracle, out);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{PackageSource, YieldContract, produce};

    /// `invoke` on a package root that does not exist on disk at all must
    /// still return a typed `ProducerError`, never panic — the walk failure
    /// at `discover_py_files`'s root read is the only thing this producer
    /// treats as fatal (see `syntax.rs`'s module doc: everything past that
    /// point is best-effort and logs rather than fails).
    #[test]
    fn invoke_on_a_missing_root_is_a_typed_error_not_a_panic() {
        let producer = PythonProducer;
        let src = PackageSource::new("/does/not/exist/anywhere", "fake_pkg", "0.1.0");
        let err = producer
            .invoke(&src)
            .expect_err("a package root that does not exist must fail, not fabricate an oracle");
        assert!(
            matches!(err, ProducerError::OracleSpawn { .. }),
            "expected OracleSpawn (the in-process-walk failure variant, matching the \
             TypeScript producer's own OXC-walk-failure precedent); got {err:?}"
        );
    }

    /// End-to-end: a tiny real package on disk lowers through the full
    /// `produce()` pipeline (invoke -> lower -> finish -> yield-contract gate
    /// -> seal) and is reported as `YieldContract::Declarations` — the trait
    /// default this producer no longer overrides — with real, named
    /// declarations in the table, not just the synthesized root.
    ///
    /// This is the fixture-free twin of `tests/corpus_sweep.rs`'s per-entry
    /// assertions against the real pypi corpus; it needs no `result/`
    /// checkout, so it proves the wiring even on a machine without the corpus.
    #[test]
    fn a_real_temp_package_produces_named_declarations_under_the_default_contract() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let pkg_dir = dir.path().join("tiny_pkg");
        std::fs::create_dir(&pkg_dir).expect("mkdir");
        std::fs::write(
            pkg_dir.join("__init__.py"),
            "\"\"\"A tiny real package.\"\"\"\n\n\
             class Greeter:\n\
             \x20\x20\x20\x20\"\"\"Says hello.\"\"\"\n\n\
             \x20\x20\x20\x20def greet(self, name: str) -> str:\n\
             \x20\x20\x20\x20\x20\x20\x20\x20return f\"hello {name}\"\n",
        )
        .expect("write __init__.py");

        let src = PackageSource::new(&pkg_dir, "tiny_pkg", "0.1.0");
        let lid = nudox_ir::change::PackageLineageId::new(
            nudox_ir::change::EcosystemId::new("pypi"),
            nudox_ir::change::PackageName::new("tiny_pkg"),
        );
        let produced = produce(&PythonProducer, &src, &lid, &nudox_ir::foreign::Unlinked)
            .expect("a real, tiny, valid package must produce successfully");

        assert!(
            matches!(produced.contract, YieldContract::Declarations),
            "the trait default must apply now that `invoke` genuinely reads `src`; got {:?}",
            produced.contract
        );
        assert!(
            produced.table.len() > 1,
            "must contribute more than just the synthesized root; got table_len={}",
            produced.table.len()
        );
        let names: Vec<&str> = produced
            .table
            .iter()
            .filter_map(|(_, e)| {
                let n = e.sym().name.as_str();
                (!n.is_empty()).then_some(n)
            })
            .collect();
        assert!(
            names.contains(&"Greeter"),
            "expected a `Greeter` class entry; got {names:?}"
        );
        assert!(
            names.contains(&"greet"),
            "expected a `greet` method entry; got {names:?}"
        );
        let greet = produced
            .table
            .iter()
            .find(|(_, e)| e.sym().name == "greet")
            .expect("the real method entry must be present")
            .1;
        // Package-relative, not the absolute temp path: `produce()`
        // (`crate::produce`, `lib.rs`) calls `Lowering::relativize_sources`
        // unconditionally after every producer's `lower()` returns — "Absolute
        // parser paths must never become part of IR identity or cross the
        // engine/GUI seam" (`nudox_ir::lower::Lowering::relativize_sources`'s
        // own doc comment). This assertion used to compare against
        // `pkg_dir.join("__init__.py")` (the absolute path `discover_py_files`
        // reports before relativization), which made this test's pass/fail
        // depend on the OS temp directory's path — it could never actually
        // fail on a machine-specific absolute-path regression, only on the
        // unrelated, deliberate relativization step. `__init__.py` is the
        // correct, stable expectation: `greet` is declared directly in the
        // package root, one path segment, exactly what `strip_prefix(pkg_dir)`
        // leaves behind.
        assert_eq!(
            greet.sym().source,
            std::path::PathBuf::from("__init__.py"),
            "declared Python methods must retain their source file, package-relative"
        );
        assert!(
            !greet.sym().span.is_empty(),
            "declared Python methods must retain a non-empty source span"
        );
    }

    #[test]
    fn producer_id_is_stable() {
        assert_eq!(PythonProducer::ID.0, "python-pyrefly/1");
    }

    #[test]
    fn producer_language_is_python() {
        assert_eq!(PythonProducer::LANGUAGE, Language::Python);
    }
}
