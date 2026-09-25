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

// ── Sealed trait (tsz seam)
// ───────────────────────────────────────────────────

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

// ── OXC oracle
// ────────────────────────────────────────────────────────────────

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

// ── Producer
// ──────────────────────────────────────────────────────────────────

/// The TypeScript producer, parameterized over the oracle tier.
///
/// Use `TypescriptProducer::new()` for the default OXC tier.
/// A future tsz tier would be selected via
/// `TypescriptProducer::<TszOracle>::new_tsz(...)`.
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
            produced
                .table
                .iter()
                .all(|(_, entry)| entry.sym().name != "*"),
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
    fn unknown_bare_name_is_not_a_type_variable() {
        let dir = tempfile::tempdir().expect("create tempdir");
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"name":"bare","version":"1.0.0","types":"index.d.ts"}"#,
        )
        .expect("write package manifest");
        std::fs::write(
            dir.path().join("index.d.ts"),
            "export function id<T>(x: T): T;\nexport function g(): ImportedWidget;\n",
        )
        .expect("write declarations");

        let source = PackageSource::new(dir.path(), "bare", "1.0.0");
        let lineage = PackageLineageId::new(EcosystemId::new("npm"), PackageName::new("bare"));
        let produced = produce(&TypescriptProducer::new(), &source, &lineage, &Unlinked)
            .expect("bare names must seal");

        let mut saw_t = false;
        let mut saw_widget_as_type_var = false;
        for (_, entry) in produced.table.iter() {
            if let EntryInner::Owned(Kind::Param(p)) = entry.kind() {
                match &p.ty {
                    Some(Type::TypeVar(name)) if name == "T" => saw_t = true,
                    Some(Type::TypeVar(name)) if name == "ImportedWidget" => {
                        saw_widget_as_type_var = true
                    }
                    _ => {}
                }
            }
        }
        assert!(saw_t, "T in id<T>(x: T): T must stay a type variable");
        assert!(
            !saw_widget_as_type_var,
            "ImportedWidget is not a type parameter"
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

        let nominal = produced
            .table
            .iter()
            .any(|(_, entry)| entry.sym().name == "return" && type_is_nominal(entry.kind()));
        assert!(
            nominal,
            "f(): AxiosResponse must lower the return type to a nominal ref"
        );
        let extends = produced
            .table
            .iter()
            .any(|(_, entry)| entry.sym().name == "A" && type_is_nominal(entry.kind()));
        assert!(extends, "class A extends B must record B as a nominal ref");
    }

    #[test]
    fn exports_use_state_is_an_owned_function() {
        let dir = tempfile::tempdir().expect("create tempdir");
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"name":"react-hooks","version":"1.0.0","main":"index.js"}"#,
        )
        .expect("write package manifest");
        std::fs::write(
            dir.path().join("index.js"),
            "exports.useState = function (initialState) { return initialState; };\n",
        )
        .expect("write entry module");

        let source = PackageSource::new(dir.path(), "react-hooks", "1.0.0");
        let lineage =
            PackageLineageId::new(EcosystemId::new("npm"), PackageName::new("react-hooks"));
        let produced = produce(&TypescriptProducer::new(), &source, &lineage, &Unlinked)
            .expect("an assigned function export must lower");

        let function = produced.table.iter().any(|(_, entry)| {
            entry.sym().name == "useState"
                && entry.sym().source.ends_with("index.js")
                && matches!(entry.kind(), EntryInner::Owned(Kind::Function(_)))
        });
        assert!(
            function,
            "exports.useState = function (initialState) must be an owned Function in that file"
        );
        assert!(
            produced
                .table
                .iter()
                .any(|(_, entry)| entry.sym().name == "initialState"),
            "the function parameter initialState must be observable"
        );
    }

    #[test]
    fn exports_router_require_is_a_package_reference() {
        let dir = tempfile::tempdir().expect("create tempdir");
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"name":"app","version":"1.0.0","main":"index.js"}"#,
        )
        .expect("write package manifest");
        std::fs::write(
            dir.path().join("index.js"),
            "var Router = require('router');\nexports.Router = Router;\n",
        )
        .expect("write entry module");

        let source = PackageSource::new(dir.path(), "app", "1.0.0");
        let lineage = PackageLineageId::new(EcosystemId::new("npm"), PackageName::new("app"));
        let produced = produce(&TypescriptProducer::new(), &source, &lineage, &Unlinked)
            .expect("a required package binding must lower");

        let reference = produced
            .table
            .iter()
            .find(|(_, entry)| entry.sym().name == "Router");
        let Some((_, entry)) = reference else {
            panic!("Router must be present");
        };
        match entry.kind() {
            EntryInner::Reference(Ref::Foreign { key, .. }) => {
                let lineage = key
                    .origin
                    .lineage()
                    .expect("Router's target must name a package");
                assert_eq!(lineage.name.as_str(), "router");
            }
            other => {
                panic!("exports.Router = require('router') must be a Reference, got {other:?}")
            }
        }
        assert!(
            produced.table.iter().all(|(_, entry)| {
                entry.sym().name != "Router"
                    || !matches!(entry.kind(), EntryInner::Owned(Kind::Const(_)))
            }),
            "require('router') must not become a Const"
        );
        assert!(
            produced.table.iter().all(|(_, entry)| {
                entry.sym().name != "Router"
                    || !matches!(entry.kind(), EntryInner::Owned(Kind::Function(_)))
            }),
            "require('router') must not become an invented function"
        );
    }

    #[test]
    fn twin_kind_enums_seal_as_one_kind() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let language = dir.path().join("language");
        std::fs::create_dir(&language).expect("create language dir");
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"name":"kinds","version":"1.0.0","exports":{"./language/kinds.js":"./language/kinds.js","./language/kinds.mjs":"./language/kinds.mjs"}}"#,
        )
        .expect("write package manifest");
        let source_text = "export enum Kind { A = \"A\" }\n";
        std::fs::write(language.join("kinds.d.ts"), source_text).expect("write d.ts");
        std::fs::write(language.join("kinds.d.mts"), source_text).expect("write d.mts");

        let source = PackageSource::new(dir.path(), "kinds", "1.0.0");
        let lineage = PackageLineageId::new(EcosystemId::new("npm"), PackageName::new("kinds"));
        let produced = produce(&TypescriptProducer::new(), &source, &lineage, &Unlinked)
            .expect("twin declaration files with the same enum must seal");

        let kinds: Vec<_> = produced
            .table
            .iter()
            .filter(|(_, entry)| {
                entry.sym().name == "Kind"
                    && matches!(entry.kind(), EntryInner::Owned(Kind::Enum(_)))
            })
            .collect();
        assert_eq!(
            kinds.len(),
            1,
            "language/kinds.d.ts and language/kinds.d.mts must seal as one Kind, got {}",
            kinds.len()
        );
    }

    #[test]
    fn same_file_uniform_functions_stay_distinct() {
        let dir = tempfile::tempdir().expect("create tempdir");
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"name":"three","version":"1.0.0","main":"index.js"}"#,
        )
        .expect("write package manifest");
        std::fs::write(
            dir.path().join("index.js"),
            "function Uniform(alpha) { return alpha; }\nfunction Uniform(beta, gamma) { return beta + gamma; }\n",
        )
        .expect("write entry module");

        let source = PackageSource::new(dir.path(), "three", "1.0.0");
        let lineage = PackageLineageId::new(EcosystemId::new("npm"), PackageName::new("three"));
        match produce(&TypescriptProducer::new(), &source, &lineage, &Unlinked) {
            Ok(produced) => {
                let bodies: Vec<_> = produced
                    .table
                    .iter()
                    .filter(|(_, entry)| {
                        entry.sym().name == "Uniform"
                            && matches!(entry.kind(), EntryInner::Owned(Kind::Function(_)))
                    })
                    .collect();
                assert!(
                    bodies.len() >= 2,
                    "both Uniform bodies must be observable when finish succeeds"
                );
                let spans: std::collections::HashSet<_> = bodies
                    .iter()
                    .map(|(_, entry)| entry.sym().span.clone())
                    .collect();
                assert!(
                    spans.len() >= 2,
                    "the two Uniform bodies must be different declarations"
                );
                assert!(
                    produced
                        .table
                        .iter()
                        .any(|(_, entry)| entry.sym().name == "alpha")
                );
                assert!(
                    produced
                        .table
                        .iter()
                        .any(|(_, entry)| entry.sym().name == "gamma")
                );
            }
            Err(error) => {
                let duplicate = matches!(
                    &error,
                    ProducerError::LoweringFailed { source, .. }
                        if source
                            .downcast_ref::<nudox_ir::lower::Error<TsId>>()
                            .is_some_and(|err| matches!(err, nudox_ir::lower::Error::Duplicate(_)))
                );
                assert!(
                    duplicate,
                    "a shared Uniform id must surface as Error::Duplicate, got {error}"
                );
            }
        }
    }

    /// axios 1.6.7 plus the three packages its manifest names.
    /// Extract them under `/tmp/medium/npm/src` before running.
    #[test]
    #[ignore = "extract axios 1.6.7 and its dependencies under /tmp/medium/npm/src"]
    fn axios_1_6_7_and_its_dependencies_seal() {
        let root = std::path::PathBuf::from("/tmp/medium/npm/src");
        let mut failures = Vec::new();
        let mut sealed = Vec::new();
        for entry in std::fs::read_dir(&root).unwrap() {
            let path = entry.unwrap().path();
            let manifest: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(path.join("package.json")).unwrap())
                    .unwrap();
            let name = manifest["name"].as_str().unwrap().to_string();
            let version = manifest["version"].as_str().unwrap().to_string();
            let source = PackageSource::new(&path, name.clone(), version);
            let lineage =
                PackageLineageId::new(EcosystemId::new("npm"), PackageName::new(name.clone()));
            match produce(&TypescriptProducer::new(), &source, &lineage, &Unlinked) {
                Ok(produced) => {
                    assert!(
                        produced.table.iter().any(|(_, entry)| !entry.sym().name.is_empty()),
                        "{name} sealed with no named declaration"
                    );
                    sealed.push(name);
                }
                Err(error) => {
                    let detail = std::error::Error::source(&error)
                        .map(|source| source.to_string())
                        .unwrap_or_else(|| error.to_string());
                    failures.push(format!("{name}: {detail}"));
                }
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
        for required in ["axios", "follow-redirects", "form-data", "proxy-from-env"] {
            assert!(
                sealed.iter().any(|name| name == required),
                "missing {required} in {sealed:?}"
            );
        }
    }

    /// The 21 packages that used to die in `Lowering::finish`. Extract each
    /// latest npm tarball under `/tmp/npm21/src/<name>` before running.
    /// Not part of the default gate: the tarballs are not in the repo.
    #[test]
    #[ignore = "extract the 21 latest npm tarballs under /tmp/npm21/src"]
    fn measured_npm_packages_seal() {
        let root = std::path::PathBuf::from("/tmp/npm21/src");
        assert!(root.is_dir(), "extract the 21 tarballs under /tmp/npm21/src");
        let mut failures = Vec::new();
        for entry in std::fs::read_dir(&root).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if !path.join("package.json").is_file() {
                continue;
            }
            let manifest: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(path.join("package.json")).unwrap())
                    .unwrap();
            let name = manifest["name"].as_str().unwrap_or("unknown").to_string();
            let version = manifest["version"].as_str().unwrap_or("0").to_string();
            let source = PackageSource::new(&path, name.clone(), version);
            let lineage =
                PackageLineageId::new(EcosystemId::new("npm"), PackageName::new(name.clone()));
            if let Err(error) = produce(&TypescriptProducer::new(), &source, &lineage, &Unlinked) {
                let detail = std::error::Error::source(&error)
                    .map(|source| source.to_string())
                    .unwrap_or_else(|| error.to_string());
                failures.push(format!("{name}: {detail}"));
            }
        }
        assert!(
            failures.is_empty(),
            "packages that must seal:\n{}",
            failures.join("\n")
        );
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
// The `where O: From<OwnedOracle>` bound on `Producer for
// TypescriptProducer<O>` is satisfied for `O = OwnedOracle` by the core blanket
// impl.

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
