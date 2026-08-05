//! [`PythonProducer`] — implements [`nudox_producer::Producer`].
//!
//! # Oracle strategy (two modes)
//!
//! **Without `pyrefly` feature (default):** `invoke` returns an empty
//! `PythonOracle`. Useful for unit tests of the lowering layer and for
//! compile-time verification that the crate contract is satisfied.
//!
//! **With `pyrefly` feature:** `invoke` drives pyrefly in-process: discover
//! `.py` / `.pyi` files, load them into a pyrefly `State`, commit a
//! transaction, walk the result, extract into owned `ModuleData`, and return
//! `PythonOracle`. The pyrefly `State` is dropped before `invoke` returns —
//! no lifetime escapes. See `context.rs` (gated behind `#[cfg(feature =
//! "pyrefly")]`) for the implementation.

use nudox_ir::body::Language;
use nudox_producer::{PackageSource, ProducerError, ProducerId, Producer};

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
            let _ = src; // suppress unused-variable warning
            // Without the pyrefly feature, return an empty oracle.  This is
            // the correct behaviour for offline / CI builds that cannot build
            // pyrefly_bundled (which downloads a typeshed at build time).
            Ok(PythonOracle::default())
        }
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

    #[test]
    fn producer_id_is_stable() {
        assert_eq!(PythonProducer::ID.0, "python-pyrefly/1");
    }

    #[test]
    fn producer_language_is_python() {
        assert_eq!(PythonProducer::LANGUAGE, Language::Python);
    }
}
