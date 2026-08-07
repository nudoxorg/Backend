//! [`PythonProducer`] — implements [`nudox_producer::Producer`].
//!
//! # Oracle strategy (two modes)
//!
//! **Without `pyrefly` feature (default):** `invoke` ignores its
//! `PackageSource` entirely and returns an empty `PythonOracle`. This producer
//! declares that fact through
//! [`Producer::yield_contract`](nudox_producer::Producer::yield_contract) —
//! see [`PythonProducer::yield_contract`] — so `nudox_producer::produce`
//! surfaces it as [`ProducerError::NoDeclarationsContributed`]-class honesty
//! rather than an `Ok` holding a one-entry table. Useful for unit tests of the
//! lowering layer and for compile-time verification that the crate contract is
//! satisfied; **not** useful for documenting a Python package.
//!
//! **With `pyrefly` feature:** `invoke` is *intended* to drive pyrefly
//! in-process: discover `.py` / `.pyi` files, load them into a pyrefly `State`,
//! commit a transaction, walk the result, extract into owned `ModuleData`, and
//! return `PythonOracle`, with the pyrefly `State` dropped before `invoke`
//! returns so no lifetime escapes.
//!
//! **That mode does not exist today, and cannot be reached by turning the
//! feature on.** Two independent blockers, both verified against this tree:
//!
//!  1. `src/context.rs` — the module `lib.rs` declares as
//!     `#[cfg(feature = "pyrefly")] pub mod context;` and that `invoke` calls —
//!     is **not present on disk**. Enabling the feature therefore fails to
//!     *compile*, with `file not found for module `context``.
//!  2. The git dependency `Cargo.toml`'s "Pyrefly gate" comment prescribes
//!     fails Cargo **dependency resolution**, before compilation: pyrefly pins
//!     `blake3 =1.8.2`, `workspace/index`'s `iroh` requires `^1.8.3`, and the
//!     two ranges are disjoint.
//!
//! The previous wording of this comment ("See `context.rs` … for the
//! implementation") described a file that is not there — the exact failure mode
//! AGENTS-DOCTRINE.md §8 names: "a comment describing behaviour is a claim, and
//! claims rot".

use nudox_ir::body::Language;
use nudox_producer::{PackageSource, Producer, ProducerError, ProducerId};
// Only `yield_contract` names these, and that override exists only in the
// degraded build (see its doc comment); importing them unconditionally would be
// an unused-import warning with `pyrefly` on.
#[cfg(not(feature = "pyrefly"))]
use nudox_producer::{DegradedYield, YieldContract};

use crate::{
    emit::emit_package,
    oracle::PythonOracle,
    oracle::PythonId,
};
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
            crate::context::invoke_oracle(src)
        }
        #[cfg(not(feature = "pyrefly"))]
        {
            // `src` is genuinely unread here — that is the whole content of
            // this branch and the reason [`Self::yield_contract`] below
            // declares `YieldContract::RootOnly`. The binding is discarded
            // rather than the parameter renamed `_src` so that the two
            // `cfg` arms keep one signature and the pyrefly arm above stays a
            // one-line edit away. (AGENTS-DOCTRINE.md §2 requires a `let _ =`
            // in non-test code to justify what it suppresses: it suppresses
            // nothing but the unused-variable lint, and the fact it stands for
            // is declared in the type system a few lines down rather than left
            // in this comment.)
            let _ = src;
            Ok(PythonOracle::default())
        }
    }

    /// Declares, in the type system, that this producer reads nothing without
    /// the `pyrefly` feature.
    ///
    /// # Why the `cfg` is on the arm and not on the method
    ///
    /// With `pyrefly` on, this method is absent and the trait default
    /// ([`YieldContract::Declarations`]) applies, so the live oracle is held to
    /// the strong contract with no further edit. With it off, the declaration
    /// is `RootOnly` and `nudox_producer::produce` refuses to seal any table
    /// that contradicts it in either direction: a lowering that suddenly
    /// contains real declarations fails as
    /// [`ProducerError::YieldContractOutgrown`] rather than quietly succeeding
    /// under a stale "this is a stub" claim.
    ///
    /// # Why the blocker text is this specific
    ///
    /// Both facts below were verified against the tree, and both are worse than
    /// "the feature is off":
    ///
    /// * `src/context.rs` — which `lib.rs` declares as
    ///   `#[cfg(feature = "pyrefly")] pub mod context;` and which
    ///   [`Self::invoke`] calls above — **does not exist on disk**. Turning the
    ///   feature on does not produce a working oracle; it produces a
    ///   `file not found for module `context`` compile error.
    /// * The git dependency `Cargo.toml` prescribes cannot be resolved at all:
    ///   pyrefly pins `blake3 =1.8.2` while `workspace/index`'s `iroh` requires
    ///   `^1.8.3` — two requirements with no version in common, so Cargo fails
    ///   in *dependency resolution*, before any compilation.
    ///
    /// A reader who only saw "pyrefly feature off" would reasonably conclude
    /// the fix is a `--features` flag. It is not, and this string is the only
    /// place that says so at the point of use.
    #[cfg(not(feature = "pyrefly"))]
    fn yield_contract(&self) -> YieldContract {
        YieldContract::RootOnly(DegradedYield::new(
            Self::ID,
            "the `pyrefly` feature is off, and it cannot simply be turned on: \
             `nudox-producer-python/src/context.rs` — declared by `lib.rs` and called by \
             `invoke` under `cfg(feature = \"pyrefly\")` — is absent from the tree, and the \
             git dependency `Cargo.toml` prescribes fails Cargo dependency resolution outright \
             (pyrefly pins `blake3 =1.8.2`; `workspace/index`'s `iroh` requires `^1.8.3`, and \
             the two ranges are disjoint). Until both are fixed, `invoke` ignores its \
             `PackageSource` and returns `PythonOracle::default()`",
        ))
    }

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
    use nudox_ir::entry::Visibility;
    use nudox_producer::PackageSource;

    /// Without pyrefly, `invoke` must succeed (returning an empty oracle) and
    /// `lower` must produce a valid (empty) package.
    #[test]
    #[cfg(not(feature = "pyrefly"))]
    fn empty_oracle_roundtrip() {
        let producer = PythonProducer;
        let src = PackageSource::new("/tmp/fake_pkg", "fake_pkg", "0.1.0");
        let oracle = producer
            .invoke(&src)
            .expect("invoke must succeed without pyrefly");
        assert!(
            oracle.modules.is_empty(),
            "no-pyrefly oracle must be empty"
        );

        let pkg_id = nudox_ir::package::PackageId::path("/tmp/fake_pkg");
        let root_sym = nudox_ir::entry::Symbol {
            name: "fake_pkg".to_owned(),
            visibility: Visibility::Public,
            documentation: String::new(),
            source: std::path::PathBuf::new(),
            span: 0..0,
            aliases: Box::new([]),
            deprecation: None,
            doc_links: Box::new([]),
            attrs: Box::new([]),
            cfg: None,
        };
        let mut sink: Lowering<PythonId> = Lowering::new(pkg_id, root_sym);
        producer
            .lower(&oracle, &mut sink)
            .expect("lower must succeed for empty oracle");

        let pkg = sink.finish().expect("finish must succeed");
        // Only the implicit root module; no user-declared entries.
        assert_eq!(
            pkg.iter().count(),
            1,
            "empty oracle must produce exactly the root entry"
        );
    }

    /// The degradation is declared, and its reason names both real blockers.
    ///
    /// This is the fixture-free twin of `tests/corpus_sweep.rs`'s per-entry
    /// assertion: that sweep needs `.real-crates/` on disk, so on a machine
    /// without the corpus it silently proves nothing. The declaration is a
    /// property of the *producer*, not of any package, so it should be provable
    /// without one.
    ///
    /// The string checks exist because the failure mode being guarded is the
    /// reason decaying back into "the pyrefly feature is off" — which is true,
    /// useless, and would send a reader to `--features pyrefly`, where they
    /// would hit a missing module and then an unsatisfiable `blake3` range. The
    /// typed half of the claim is the `RootOnly` match itself.
    #[test]
    #[cfg(not(feature = "pyrefly"))]
    fn without_pyrefly_the_producer_declares_root_only_and_names_both_blockers() {
        let contract = PythonProducer.yield_contract();
        let degraded = match &contract {
            nudox_producer::YieldContract::RootOnly(d) => d,
            other => panic!(
                "without pyrefly this producer reads no source, so it must declare \
                 YieldContract::RootOnly; got {other:?}"
            ),
        };
        assert_eq!(degraded.producer(), PythonProducer::ID);
        assert!(
            degraded.blocker().contains("context.rs"),
            "blocker must name the module that is missing from the tree: {:?}",
            degraded.blocker()
        );
        assert!(
            degraded.blocker().contains("blake3"),
            "blocker must name the dependency-resolution conflict: {:?}",
            degraded.blocker()
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
