//! [`Producer`] implementation for the TypeScript/OXC tier.
//!
//! # tsz seam
//! The [`TsOracle`] sealed trait is the plug point for a future tsz tier.
//! Today only [`OwnedOracle`] (the OXC path) implements it. A tsz oracle
//! would implement `TsOracle` and be selected by constructing
//! `TypescriptProducer::<TszOracle>::new()`.
//!
//! The trait is sealed (private supertrait) so external crates cannot
//! implement it without touching this crate.

use nudox_ir::body::Language;

use crate::{PackageSource, Producer, ProducerError, ProducerId};
use nudox_ir::lower::Lowering;

use crate::typescript::{
    emit::lower_package, entry::discover_entry_points, extract::ModuleFacts,
    graph::build_and_extract, id::TsId,
};

// ── Sealed trait (tsz seam) ───────────────────────────────────────────────────

// `pub(crate)` so the tsz oracle in `oracle::tsz` can implement `TsOracleSeal`
// without the trait being implementable by external crates.
pub(crate) mod sealed {
    pub trait TsOracleSeal {}
}

/// Marker trait for TypeScript oracle implementations.
///
/// Currently only [`OwnedOracle`] (OXC) implements this. A future tsz oracle
/// would implement both this trait and the `Producer::Oracle` contract.
pub trait TsOracle: sealed::TsOracleSeal {
    fn modules(&self) -> &[ModuleFacts];
}

// ── OXC oracle ────────────────────────────────────────────────────────────────

/// The owned result of the OXC extraction pass.
///
/// All AST arenas have been dropped by the time this exists. It contains
/// only fully-owned `ModuleFacts` — no arena references.
pub struct OwnedOracle {
    pub(crate) modules: Vec<ModuleFacts>,
}

impl OwnedOracle {
    /// Wrap an already-extracted set of module facts.
    ///
    /// Used in integration tests and the tsz oracle path to promote OXC output
    /// into an `OwnedOracle` without accessing the private `modules` field.
    pub fn new(modules: Vec<ModuleFacts>) -> Self {
        OwnedOracle { modules }
    }
}

impl sealed::TsOracleSeal for OwnedOracle {}

impl TsOracle for OwnedOracle {
    fn modules(&self) -> &[ModuleFacts] {
        &self.modules
    }
}

// ── Producer ──────────────────────────────────────────────────────────────────

/// The TypeScript producer, parameterized over the oracle tier.
///
/// Use `TypescriptProducer::new()` for the default OXC tier.
/// A future tsz tier would be selected via `TypescriptProducer::<TszOracle>::new_tsz(...)`.
pub struct TypescriptProducer<O = OwnedOracle> {
    _marker: std::marker::PhantomData<O>,
}

impl TypescriptProducer<OwnedOracle> {
    /// Construct a new TypeScript producer backed by the OXC in-process tier.
    pub fn new() -> Self {
        TypescriptProducer {
            _marker: std::marker::PhantomData,
        }
    }
}

impl Default for TypescriptProducer<OwnedOracle> {
    fn default() -> Self {
        Self::new()
    }
}

impl<O> Producer for TypescriptProducer<O>
where
    O: TsOracle + From<OwnedOracle>,
{
    type Id = TsId;
    type Oracle = O;

    const ID: ProducerId = ProducerId("typescript-oxc/1");
    const LANGUAGE: Language = Language::TypeScript;

    fn invoke(&self, src: &PackageSource) -> Result<O, ProducerError> {
        // Canonicalize once, up front, and use the canonical path for BOTH
        // discovery passes below — this is not cosmetic.
        //
        // `discover_entry_points`'s `deep_import_roots` builds every path by
        // joining onto whatever `root` it is given (`WalkDir::new(root)`,
        // `entry.path().to_path_buf()`), so any literal `..` component in
        // `root` survives unchanged into every path it returns. `graph.rs`'s
        // import-edge BFS instead resolves paths through `oxc_resolver`,
        // which normalizes `..`/`.` as part of Node module resolution. When
        // the caller's `root` still carries a literal `..` — the real case
        // that surfaced this: `nudox-store`'s `corpus_contract.rs` builds its
        // fixture root as `CARGO_MANIFEST_DIR.join("../..")`, never resolved
        // — those two passes produce two different `PathBuf` *strings* for
        // the identical physical file. `discover_entry_points_with`'s
        // `BTreeSet<PathBuf>` dedup compares strings, not inodes, so it
        // cannot catch this: the same file was walked and lowered twice,
        // producing two `Module` entries with the same name and identical
        // content (and, since real spans landed, identical spans too — the
        // identity gate correctly reported that pair as order-dependent;
        // the gate was never wrong, the input handed to it was). Found via
        // lodash 4.17.21's `deburr.js`, reachable both directly
        // (deep-import-root) and, through `fp/deburr.js`'s
        // `require('../deburr')`, via the resolver. Canonicalizing `root`
        // once here makes both passes agree on one path per file, so the
        // `BTreeSet` dedup actually works and the duplicate stops existing.
        let root = std::fs::canonicalize(src.root()).map_err(|e| ProducerError::OracleSpawn {
            command: "oxc-canonicalize-root".to_string(),
            reason: e,
        })?;

        let entry_points = discover_entry_points(&root);

        let modules =
            build_and_extract(&entry_points, &root).map_err(|e| ProducerError::OracleSpawn {
                command: "oxc-extract".to_string(),
                reason: std::io::Error::other(e),
            })?;

        Ok(OwnedOracle { modules }.into())
    }

    fn lower(&self, oracle: &O, out: &mut Lowering<TsId>) -> Result<(), ProducerError> {
        lower_package(oracle.modules(), out);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{PackageSource, ProducerError, YieldContract, produce};
    use nudox_ir::{
        change::{EcosystemId, PackageLineageId, PackageName},
        entry::EntryInner,
        foreign::Unlinked,
        index::Ref,
        kind::Kind,
        kinds::ty::Type,
    };

    fn type_is_nominal(kind: &EntryInner) -> bool {
        match kind {
            EntryInner::Owned(Kind::Param(p)) => matches!(p.ty, Some(Type::Nominal(_))),
            EntryInner::Owned(Kind::Record(r)) => {
                r.super_types.iter().any(|t| matches!(t, Type::Nominal(_)))
            }
            _ => false,
        }
    }

    #[test]
    fn commonjs_require_bindings_are_not_fabricated_constants() {
        let dir = tempfile::tempdir().expect("create tempdir");
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"name":"tiny-cjs","version":"1.0.0","main":"index.js"}"#,
        )
        .expect("write package manifest");
        std::fs::write(
            dir.path().join("index.js"),
            "const answer = require('./answer');\nmodule.exports = answer;\n",
        )
        .expect("write entry module");
        std::fs::write(
            dir.path().join("answer.js"),
            "function answer() { return 42; }\nmodule.exports = answer;\n",
        )
        .expect("write exported module");

        let source = PackageSource::new(dir.path(), "tiny-cjs", "1.0.0");
        let lineage = PackageLineageId::new(EcosystemId::new("npm"), PackageName::new("tiny-cjs"));
        let produced = produce(&TypescriptProducer::new(), &source, &lineage, &Unlinked)
            .expect("a CommonJS package with a real export must lower");

        assert!(matches!(produced.contract, YieldContract::Declarations));
        assert!(
            produced
                .table
                .iter()
                .any(|(_, entry)| entry.sym().name == "answer"),
            "the exported function must be discovered and lowered"
        );
        assert!(
            produced.table.iter().all(|(_, entry)| {
                !matches!(
                    entry.kind(),
                    EntryInner::Owned(nudox_ir::kind::Kind::Const(_))
                ) || entry.sym().name != "answer"
            }),
            "require('./answer') must not become a fabricated Const named `answer`"
        );
    }

    #[test]
    fn star_reexport_of_a_missing_file_seals_without_a_star_symbol() {
        let dir = tempfile::tempdir().expect("create tempdir");
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"name":"star-gap","version":"1.0.0","types":"index.d.ts"}"#,
        )
        .expect("write package manifest");
        std::fs::write(
            dir.path().join("index.d.ts"),
            "export function keep(): void;\nexport * from \"./gone\";\n",
        )
        .expect("write entry module");

        let source = PackageSource::new(dir.path(), "star-gap", "1.0.0");
        let lineage = PackageLineageId::new(EcosystemId::new("npm"), PackageName::new("star-gap"));
        let produced = produce(&TypescriptProducer::new(), &source, &lineage, &Unlinked)
            .expect("a star that names nothing must not reject the package");

        assert!(
            produced.table.iter().all(|(_, entry)| entry.sym().name != "*"),
            "no symbol may be named *"
        );
        assert!(
            produced
                .table
                .iter()
                .any(|(_, entry)| entry.sym().name == "keep"),
            "the real declaration must still seal"
        );
    }

    #[test]
    fn barrel_reexport_of_a_later_reexport_stays_local() {
        let dir = tempfile::tempdir().expect("create tempdir");
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"name":"barrel","version":"1.0.0","types":"index.d.ts"}"#,
        )
        .expect("write package manifest");
        std::fs::write(
            dir.path().join("index.d.ts"),
            "export { answer } from \"./answer\";\n",
        )
        .expect("write barrel");
        std::fs::write(
            dir.path().join("answer.d.ts"),
            "export { answer } from \"./impl\";\n",
        )
        .expect("write middle reexport");
        std::fs::write(
            dir.path().join("impl.d.ts"),
            "export function answer(): number;\n",
        )
        .expect("write implementation");

        let source = PackageSource::new(dir.path(), "barrel", "1.0.0");
        let lineage = PackageLineageId::new(EcosystemId::new("npm"), PackageName::new("barrel"));
        let produced = produce(&TypescriptProducer::new(), &source, &lineage, &Unlinked)
            .expect("a reexport chain inside the package must seal");

        let local = produced.table.iter().any(|(_, entry)| {
            entry.sym().name == "answer"
                && matches!(entry.kind(), EntryInner::Reference(Ref::Intro(_)))
        });
        assert!(
            local,
            "the barrel's answer reexport must point at the later file, not a foreign key"
        );
    }

    #[test]
    fn same_file_nominal_is_a_local_ref() {
        let dir = tempfile::tempdir().expect("create tempdir");
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"name":"noms","version":"1.0.0","types":"index.d.ts"}"#,
        )
        .expect("write package manifest");
        std::fs::write(
            dir.path().join("index.d.ts"),
            "export class B {}\nexport class A extends B {}\nexport class AxiosResponse {}\nexport function f(): AxiosResponse;\n",
        )
        .expect("write declarations");

        let source = PackageSource::new(dir.path(), "noms", "1.0.0");
        let lineage = PackageLineageId::new(EcosystemId::new("npm"), PackageName::new("noms"));
        let produced = produce(&TypescriptProducer::new(), &source, &lineage, &Unlinked)
            .expect("same-file nominals must seal");

        let nominal = produced.table.iter().any(|(_, entry)| {
            entry.sym().name == "return" && type_is_nominal(entry.kind())
        });
        assert!(
            nominal,
            "f(): AxiosResponse must lower the return type to a nominal ref"
        );
        let extends = produced.table.iter().any(|(_, entry)| {
            entry.sym().name == "A" && type_is_nominal(entry.kind())
        });
        assert!(extends, "class A extends B must record B as a nominal ref");
    }

    #[test]
    fn an_empty_package_is_rejected_instead_of_sealing_the_root_stub() {
        let dir = tempfile::tempdir().expect("create tempdir");
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"name":"empty-npm","version":"1.0.0"}"#,
        )
        .expect("write package manifest");

        let source = PackageSource::new(dir.path(), "empty-npm", "1.0.0");
        let lineage = PackageLineageId::new(EcosystemId::new("npm"), PackageName::new("empty-npm"));
        let error = produce(&TypescriptProducer::new(), &source, &lineage, &Unlinked)
            .expect_err("a package with no declarations must not count as successful");

        assert!(
            matches!(error, ProducerError::NoDeclarationsContributed { .. }),
            "expected the yield-contract refusal, got {error:?}"
        );
    }
}

// Note: `From<OwnedOracle> for OwnedOracle` is NOT implemented here because
// `impl<T> From<T> for T` already exists in core (the reflexive blanket impl).
// The `where O: From<OwnedOracle>` bound on `Producer for TypescriptProducer<O>`
// is satisfied for `O = OwnedOracle` by the core blanket impl.

#[cfg(feature = "tsz")]
impl TypescriptProducer<crate::typescript::oracle::tsz::TszOracle> {
    /// Construct a TypeScript producer backed by the tsz checker oracle.
    ///
    /// The tsz tier runs OXC first for structure, then enriches missing types
    /// (inferred returns, cross-module resolution, `Promise<T>` unwrapping,
    /// object shapes) via the in-process tsz TypeScript checker.
    ///
    /// Requires `--features tsz`.  Any tsz failure falls back to OXC-only
    /// output transparently.
    pub fn new_tsz() -> Self {
        TypescriptProducer {
            _marker: std::marker::PhantomData,
        }
    }
}
