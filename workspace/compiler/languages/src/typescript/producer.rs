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

    fn count_symbol(pkg: &str, source: &str, symbol: &str) -> usize {
        let dir = tempfile::tempdir().expect("create tempdir");
        std::fs::write(
            dir.path().join("package.json"),
            format!(r#"{{"name":"{pkg}","version":"1.0.0","types":"index.d.ts"}}"#),
        )
        .unwrap();
        std::fs::write(dir.path().join("index.d.ts"), source).unwrap();
        let package = PackageSource::new(dir.path(), pkg, "1.0.0");
        let lineage = PackageLineageId::new(EcosystemId::new("npm"), PackageName::new(pkg));
        let produced = produce(&TypescriptProducer::new(), &package, &lineage, &Unlinked)
            .unwrap_or_else(|err| panic!("{pkg} must seal, not drop the second body: {err}"));
        produced
            .table
            .iter()
            .filter(|(_, entry)| entry.sym().name == symbol)
            .count()
    }

    #[test]
    fn same_file_shape_differences_both_survive() {
        let cases = [
            (
                "variance",
                "namespace N { export interface I<T> { x: T; } }\n\
                 namespace N { export interface I<out T> { x: T; } }\n",
                "I",
            ),
            (
                "receiver",
                "namespace N { export class C { m(this: C): void; } }\n\
                 namespace N { export class C { m(): void; } }\n",
                "m",
            ),
            (
                "readonly-param",
                "namespace N { export function f(a: string[]): void; }\n\
                 namespace N { export function f(readonly a: string[]): void; }\n",
                "f",
            ),
            (
                "optional-fn-type",
                "namespace N { export interface I { f: (x?: string) => void; } }\n\
                 namespace N { export interface I { f: (x: string) => void; } }\n",
                "f",
            ),
            (
                "rest-fn-type",
                "namespace N { export interface I { f: (x: string[]) => void; } }\n\
                 namespace N { export interface I { f: (...x: string[]) => void; } }\n",
                "f",
            ),
            (
                "fn-type-bound",
                "namespace N { export interface I { f: <T>(x: T) => T; } }\n\
                 namespace N { export interface I { f: <T extends string>(x: T) => T; } }\n",
                "f",
            ),
            (
                "alias-generics",
                "namespace N { export type T<A> = A; }\n\
                 namespace N { export type T<A extends string> = A; }\n",
                "T",
            ),
            (
                "extends-implements",
                "namespace N { export class C extends A {} }\n\
                 namespace N { export class C implements A {} }\n",
                "C",
            ),
            (
                "method-decorator",
                "namespace N { export class C { m(): void; } }\n\
                 namespace N { export class C { @dec m(): void; } }\n",
                "m",
            ),
            (
                "accessor",
                "namespace N { export class C { x: string; } }\n\
                 namespace N { export class C { accessor x: string; } }\n",
                "x",
            ),
            (
                "static-method",
                "namespace N { export class C { m(): void; } }\n\
                 namespace N { export class C { static m(): void; } }\n",
                "m",
            ),
            (
                "accessibility",
                "namespace N { export class C { public m(): void; } }\n\
                 namespace N { export class C { private m(): void; } }\n",
                "m",
            ),
            (
                "readonly-optional",
                "namespace N { export class C { a: string; } }\n\
                 namespace N { export class C { readonly a?: string; } }\n",
                "a",
            ),
            (
                "optional-method",
                "namespace N { export interface I { f(a: string): void; } }\n\
                 namespace N { export interface I { f?(a: string): void; } }\n",
                "f",
            ),
            (
                "abstract-method",
                "namespace N { export abstract class C { m(): void; } }\n\
                 namespace N { export abstract class C { abstract m(): void; } }\n",
                "m",
            ),
            (
                "visibility",
                "namespace N { export function f(): void; }\n\
                 namespace N { function f(): void; }\n",
                "f",
            ),
            (
                "getter",
                "namespace N { export interface I { get f(): string; } }\n\
                 namespace N { export interface I { f(): string; } }\n",
                "f",
            ),
            (
                "readonly-index",
                "namespace N { export interface I { [k: string]: string; } }\n\
                 namespace N { export interface I { readonly [k: string]: string; } }\n",
                "__index",
            ),
            (
                "call-this",
                "namespace N { export interface I { (this: string, x: number): void; } }\n\
                 namespace N { export interface I { (this: boolean, x: number): void; } }\n",
                "I",
            ),
            (
                "abstract-new",
                "namespace N { export type T = new () => object; }\n\
                 namespace N { export type T = abstract new () => object; }\n",
                "T",
            ),
            (
                "class-index",
                "namespace N { export class C { [k: string]: string; } }\n\
                 namespace N { export class C { [k: string]: number; } }\n",
                "C",
            ),
            (
                "object-index",
                "namespace N { export interface I { a: { [k: string]: string }; } }\n\
                 namespace N { export interface I { a: { [k: string]: number }; } }\n",
                "a",
            ),
            (
                "class-getter",
                "namespace N { export class C { get f(): string { return \"\"; } } }\n\
                 namespace N { export class C { f(): string { return \"\"; } } }\n",
                "f",
            ),
            (
                "class-setter",
                "namespace N { export class C { set f(v: number) {} } }\n\
                 namespace N { export class C { f(v: number) {} } }\n",
                "f",
            ),
            (
                "declare-field",
                "namespace N { export class C { x: string; } }\n\
                 namespace N { export class C { declare x: string; } }\n",
                "x",
            ),
            (
                "definite-field",
                "namespace N { export class C { x: string; } }\n\
                 namespace N { export class C { x!: string; } }\n",
                "x",
            ),
            (
                "override-method",
                "namespace N { export class C { m(): void {} } }\n\
                 namespace N { export class C { override m(): void {} } }\n",
                "m",
            ),
            (
                "field-initializer",
                "namespace N { export class C { x = 1; } }\n\
                 namespace N { export class C { x = 2; } }\n",
                "x",
            ),
            (
                "static-index",
                "namespace N { export class C { static [k: string]: string; } }\n\
                 namespace N { export class C { [k: string]: string; } }\n",
                "C",
            ),
            (
                "tuple-optional",
                "namespace N { export type T = [string]; }\n\
                 namespace N { export type T = [string?]; }\n",
                "T",
            ),
            (
                "tuple-rest",
                "namespace N { export type T = [string, ...number[]]; }\n\
                 namespace N { export type T = [string, number[]]; }\n",
                "T",
            ),
            (
                "readonly-fn-param",
                "namespace N { export type T = (readonly x: string[]) => void; }\n\
                 namespace N { export type T = (x: string[]) => void; }\n",
                "T",
            ),
            (
                "enum-unary",
                "namespace N { export enum E { A = -1 } }\n\
                 namespace N { export enum E { A = -2 } }\n",
                "A",
            ),
            (
                "computed-key",
                "namespace N { export interface I { a: { [k1]: string } } }\n\
                 namespace N { export interface I { a: { [k2]: string } } }\n",
                "a",
            ),
            (
                "accessor-override",
                "namespace N { export class C { accessor x: string; } }\n\
                 namespace N { export class C { override accessor x: string; } }\n",
                "x",
            ),
            (
                "accessor-definite",
                "namespace N { export class C { accessor x: string; } }\n\
                 namespace N { export class C { accessor x!: string; } }\n",
                "x",
            ),
            (
                "accessor-initializer",
                "namespace N { export class C { accessor x: string = 1; } }\n\
                 namespace N { export class C { accessor x: string = 2; } }\n",
                "x",
            ),
            (
                "static-block",
                "namespace N { export class C { static { const x = 1; } } }\n\
                 namespace N { export class C { static { const x = 2; } } }\n",
                "__static",
            ),
            (
                "function-body",
                "namespace N { export function f() { return 1; } }\n\
                 namespace N { export function f() { return 2; } }\n",
                "f",
            ),
            (
                "method-body",
                "namespace N { export class C { m() { return 1; } } }\n\
                 namespace N { export class C { m() { return 2; } } }\n",
                "m",
            ),
            (
                "object-pattern",
                "namespace N { export function f({ x }: { x: number }): void; }\n\
                 namespace N { export function f({ y }: { x: number }): void; }\n",
                "f",
            ),
            (
                "array-pattern",
                "namespace N { export function f([a]: number[]): void; }\n\
                 namespace N { export function f([b]: number[]): void; }\n",
                "f",
            ),
            (
                "param-default",
                "namespace N { export function f(a = 1) { return a; } }\n\
                 namespace N { export function f(a = 2) { return a; } }\n",
                "f",
            ),
            (
                "param-decorator",
                "namespace N { export class C { m(@dec a: string) {} } }\n\
                 namespace N { export class C { m(@other a: string) {} } }\n",
                "m",
            ),
            (
                "ctor-override",
                "namespace N { export class C { constructor(public x: number) {} } }\n\
                 namespace N { export class C { constructor(override public x: number) {} } }\n",
                "x",
            ),
            (
                "rest-decorator",
                "namespace N { export function f(@dec ...a: string[]) { return a; } }\n\
                 namespace N { export function f(@other ...a: string[]) { return a; } }\n",
                "f",
            ),
            (
                "call-doc",
                "namespace N { export interface I { /** a */ (x: string): void; } }\n\
                 namespace N { export interface I { /** b */ (x: string): void; } }\n",
                "I",
            ),
            (
                "construct-doc",
                "namespace N { export interface I { /** a */ new (x: string): I; } }\n\
                 namespace N { export interface I { /** b */ new (x: string): I; } }\n",
                "I",
            ),
            (
                "index-doc",
                "namespace N { export interface I { /** a */ [k: string]: string; } }\n\
                 namespace N { export interface I { /** b */ [k: string]: string; } }\n",
                "I",
            ),
            (
                "call-deprecated",
                "namespace N { export interface I { /** @deprecated a */ (x: string): void; } }\n\
                 namespace N { export interface I { /** @deprecated b */ (x: string): void; } }\n",
                "I",
            ),
            (
                "construct-deprecated",
                "namespace N { export interface I { /** @deprecated a */ new (x: string): I; } }\n\
                 namespace N { export interface I { /** @deprecated b */ new (x: string): I; } }\n",
                "I",
            ),
            (
                "index-deprecated",
                "namespace N { export interface I { /** @deprecated a */ [k: string]: string; } }\n\
                 namespace N { export interface I { /** @deprecated b */ [k: string]: string; } }\n",
                "I",
            ),
            (
                "call-ignore",
                "namespace N { export interface I { /** @ignore */ (x: string): void; } }\n\
                 namespace N { export interface I { (x: string): void; } }\n",
                "I",
            ),
            (
                "object-index-doc",
                "namespace N { export interface I { a: { /** a */ [k: string]: string } } }\n\
                 namespace N { export interface I { a: { /** b */ [k: string]: string } } }\n",
                "a",
            ),
            (
                "object-property-doc",
                "namespace N { export interface I { a: { /** a */ x: string } } }\n\
                 namespace N { export interface I { a: { /** b */ x: string } } }\n",
                "a",
            ),
            (
                "docs",
                "namespace N { /** one */ export interface I { x: string; } }\n\
                 namespace N { /** two */ export interface I { x: string; } }\n",
                "I",
            ),
        ];
        for (pkg, source, symbol) in cases {
            let count = count_symbol(pkg, source, symbol);
            assert!(
                count >= 2,
                "{pkg}: both {symbol} bodies must survive, got {count}"
            );
        }
    }

    #[test]
    fn namespace_interfaces_with_different_type_param_bounds_both_survive() {
        let dir = tempfile::tempdir().expect("create tempdir");
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"name":"bound","version":"1.0.0","types":"index.d.ts"}"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("index.d.ts"),
            "namespace N { export interface I { f<T>(a: T): T; } }\n\
             namespace N { export interface I { f<T extends string>(a: T): T; } }\n",
        )
        .unwrap();
        let source = PackageSource::new(dir.path(), "bound", "1.0.0");
        let lineage = PackageLineageId::new(EcosystemId::new("npm"), PackageName::new("bound"));
        let produced = produce(&TypescriptProducer::new(), &source, &lineage, &Unlinked)
            .expect("different type-parameter bounds must seal");
        let fs = produced
            .table
            .iter()
            .filter(|(_, entry)| entry.sym().name == "f")
            .count();
        assert!(fs >= 2, "an unbounded T and T extends string are different overloads");
    }

    #[test]
    fn namespace_interfaces_with_different_index_signatures_both_survive() {
        let dir = tempfile::tempdir().expect("create tempdir");
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"name":"idx","version":"1.0.0","types":"index.d.ts"}"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("index.d.ts"),
            "namespace N { export interface I { [k: string]: string; } }\n\
             namespace N { export interface I { [k: number]: number; } }\n",
        )
        .unwrap();
        let source = PackageSource::new(dir.path(), "idx", "1.0.0");
        let lineage = PackageLineageId::new(EcosystemId::new("npm"), PackageName::new("idx"));
        let produced = produce(&TypescriptProducer::new(), &source, &lineage, &Unlinked)
            .expect("different index signatures in one file must seal");
        let indexes = produced
            .table
            .iter()
            .filter(|(_, entry)| entry.sym().name == "__index")
            .count();
        assert!(
            indexes >= 2,
            "string and number index signatures are different bodies"
        );
    }

    #[test]
    fn namespace_interfaces_with_different_signatures_both_survive() {
        let dir = tempfile::tempdir().expect("create tempdir");
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"name":"merge","version":"1.0.0","types":"index.d.ts"}"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("index.d.ts"),
            "namespace N { export interface I { f(a: string): string; } }\n\
             namespace N { export interface I { f(a: number): number; } }\n",
        )
        .unwrap();
        let source = PackageSource::new(dir.path(), "merge", "1.0.0");
        let lineage = PackageLineageId::new(EcosystemId::new("npm"), PackageName::new("merge"));
        let produced = produce(&TypescriptProducer::new(), &source, &lineage, &Unlinked)
            .expect("different interface signatures in one file must seal");
        let fs = produced
            .table
            .iter()
            .filter(|(_, entry)| entry.sym().name == "f")
            .count();
        assert!(
            fs >= 2,
            "f(string) and f(number) are different overloads; dropping one is not a merge"
        );
    }

    #[test]
    fn enum_and_namespace_member_of_the_same_name_both_survive() {
        let dir = tempfile::tempdir().expect("create tempdir");
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"name":"enum-ns","version":"1.0.0","types":"index.d.ts"}"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("index.d.ts"),
            "export enum E { A = 1 }\nexport namespace E { export function A(): void; }\n",
        )
        .unwrap();
        let source = PackageSource::new(dir.path(), "enum-ns", "1.0.0");
        let lineage = PackageLineageId::new(EcosystemId::new("npm"), PackageName::new("enum-ns"));
        let produced = produce(&TypescriptProducer::new(), &source, &lineage, &Unlinked)
            .expect("an enum variant and a namespace function of the same name must both seal");
        let variants = produced
            .table
            .iter()
            .filter(|(_, entry)| {
                entry.sym().name == "A" && matches!(entry.kind(), EntryInner::Owned(Kind::Variant(_)))
            })
            .count();
        let functions = produced
            .table
            .iter()
            .filter(|(_, entry)| {
                entry.sym().name == "A" && matches!(entry.kind(), EntryInner::Owned(Kind::Function(_)))
            })
            .count();
        assert_eq!(variants, 1, "enum variant A must be declared");
        assert_eq!(functions, 1, "namespace function A must be declared");
    }

    #[test]
    fn class_and_interface_namespace_methods_of_the_same_name_both_survive() {
        let dir = tempfile::tempdir().expect("create tempdir");
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"name":"class-ns","version":"1.0.0","types":"index.d.ts"}"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("index.d.ts"),
            "export class C { method(): void; }\n\
             export namespace C { export function method(): void; }\n\
             export interface I { call(): void; }\n\
             export namespace I { export function call(): void; }\n",
        )
        .unwrap();
        let source = PackageSource::new(dir.path(), "class-ns", "1.0.0");
        let lineage = PackageLineageId::new(EcosystemId::new("npm"), PackageName::new("class-ns"));
        let produced = produce(&TypescriptProducer::new(), &source, &lineage, &Unlinked)
            .expect("a type's method and a merged namespace function must both seal");
        let named = |name: &str| {
            produced
                .table
                .iter()
                .filter(|(_, entry)| {
                    entry.sym().name == name
                        && matches!(entry.kind(), EntryInner::Owned(Kind::Function(_)))
                })
                .count()
        };
        assert_eq!(named("method"), 2, "class method and namespace function");
        assert_eq!(named("call"), 2, "interface method and namespace function");
    }

    #[test]
    fn class_field_and_namespace_value_of_the_same_name_both_survive() {
        let dir = tempfile::tempdir().expect("create tempdir");
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"name":"field-ns","version":"1.0.0","types":"index.d.ts"}"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("index.d.ts"),
            "export class C { field: number; }\n\
             export namespace C { export const field: number; }\n",
        )
        .unwrap();
        let source = PackageSource::new(dir.path(), "field-ns", "1.0.0");
        let lineage = PackageLineageId::new(EcosystemId::new("npm"), PackageName::new("field-ns"));
        let produced = produce(&TypescriptProducer::new(), &source, &lineage, &Unlinked)
            .expect("a class field and a merged namespace const must both seal");
        let fields = produced
            .table
            .iter()
            .filter(|(_, entry)| {
                entry.sym().name == "field"
                    && matches!(entry.kind(), EntryInner::Owned(Kind::Field(_)))
            })
            .count();
        let consts = produced
            .table
            .iter()
            .filter(|(_, entry)| {
                entry.sym().name == "field"
                    && matches!(entry.kind(), EntryInner::Owned(Kind::Const(_)))
            })
            .count();
        assert_eq!(fields, 1, "class field must be declared");
        assert_eq!(consts, 1, "namespace const must be declared");
    }

    #[test]
    fn an_index_signature_does_not_take_a_method_named_index() {
        let dir = tempfile::tempdir().expect("create tempdir");
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"name":"index-method","version":"1.0.0","types":"index.d.ts"}"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("index.d.ts"),
            "export interface Bag { __index(): void; [key: string]: unknown; }\n",
        )
        .unwrap();
        let source = PackageSource::new(dir.path(), "index-method", "1.0.0");
        let lineage =
            PackageLineageId::new(EcosystemId::new("npm"), PackageName::new("index-method"));
        let produced = produce(&TypescriptProducer::new(), &source, &lineage, &Unlinked)
            .expect("a method named __index and an index signature must both seal");
        let indexes = produced
            .table
            .iter()
            .filter(|(_, entry)| entry.sym().name == "__index")
            .count();
        assert_eq!(
            indexes, 2,
            "the method and the index signature are different declarations"
        );
    }

    #[test]
    fn a_call_signature_is_declared() {
        let dir = tempfile::tempdir().expect("create tempdir");
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"name":"callable","version":"1.0.0","types":"index.d.ts"}"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("index.d.ts"),
            "export interface Fn { (value: number): string; }\n\
             export interface Bag { __call(): void; (value: number): string; }\n",
        )
        .unwrap();
        let source = PackageSource::new(dir.path(), "callable", "1.0.0");
        let lineage = PackageLineageId::new(EcosystemId::new("npm"), PackageName::new("callable"));
        let produced = produce(&TypescriptProducer::new(), &source, &lineage, &Unlinked)
            .expect("a callable interface must seal");
        let calls = produced
            .table
            .iter()
            .filter(|(_, entry)| {
                entry.sym().name == "__call"
                    && matches!(entry.kind(), EntryInner::Owned(Kind::Function(_)))
            })
            .count();
        assert_eq!(calls, 3, "Fn's call, Bag's method, and Bag's call");
    }

    #[test]
    fn a_this_parameter_is_declared() {
        let dir = tempfile::tempdir().expect("create tempdir");
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"name":"this-param","version":"1.0.0","types":"index.d.ts"}"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("index.d.ts"),
            "export function f(this: Widget, x: number): void;\n",
        )
        .unwrap();
        let source = PackageSource::new(dir.path(), "this-param", "1.0.0");
        let lineage =
            PackageLineageId::new(EcosystemId::new("npm"), PackageName::new("this-param"));
        let produced = produce(&TypescriptProducer::new(), &source, &lineage, &Unlinked)
            .expect("a this-parameter must seal");
        let this_params = produced
            .table
            .iter()
            .filter(|(_, entry)| {
                entry.sym().name == "this"
                    && matches!(entry.kind(), EntryInner::Owned(Kind::Param(_)))
            })
            .count();
        assert_eq!(this_params, 1, "the this parameter must be declared");
    }

    #[test]
    fn a_late_field_does_not_take_a_method_discriminant() {
        // Method discs are `index * 1000`. A field's disc is its own index.
        // Member 1's method and member 1000's field are the same number.
        let mut source_text = String::from("export class C {\n  pad() {}\n  slot() {}\n");
        for n in 2..1000 {
            source_text.push_str(&format!("  f{n}: number;\n"));
        }
        source_text.push_str("  static slot: number;\n}\n");
        let dir = tempfile::tempdir().expect("create tempdir");
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"name":"wide-class","version":"1.0.0","types":"index.d.ts"}"#,
        )
        .unwrap();
        std::fs::write(dir.path().join("index.d.ts"), &source_text).unwrap();
        let source = PackageSource::new(dir.path(), "wide-class", "1.0.0");
        let lineage =
            PackageLineageId::new(EcosystemId::new("npm"), PackageName::new("wide-class"));
        let produced = produce(&TypescriptProducer::new(), &source, &lineage, &Unlinked)
            .expect("a field past the method lattice must seal");
        let slots = produced
            .table
            .iter()
            .filter(|(_, entry)| entry.sym().name == "slot")
            .count();
        assert_eq!(slots, 2, "the method and the static field both survive");
    }

    #[test]
    fn a_static_block_does_not_take_a_method_discriminant() {
        // The first static block is named `__static` and its disc is the
        // member index. A method at index 1 is also disc 1000.
        let mut source_text =
            String::from("export class C {\n  pad() {}\n  __static() {}\n");
        for n in 2..1000 {
            source_text.push_str(&format!("  f{n}: number;\n"));
        }
        source_text.push_str("  static { const x = 1; }\n}\n");
        let dir = tempfile::tempdir().expect("create tempdir");
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"name":"static-block-disc","version":"1.0.0","types":"index.d.ts"}"#,
        )
        .unwrap();
        std::fs::write(dir.path().join("index.d.ts"), &source_text).unwrap();
        let source = PackageSource::new(dir.path(), "static-block-disc", "1.0.0");
        let lineage = PackageLineageId::new(
            EcosystemId::new("npm"),
            PackageName::new("static-block-disc"),
        );
        let produced = produce(&TypescriptProducer::new(), &source, &lineage, &Unlinked)
            .expect("a static block on the method lattice must seal");
        let blocks = produced
            .table
            .iter()
            .filter(|(_, entry)| entry.sym().name == "__static")
            .count();
        assert_eq!(blocks, 2, "the method and the static block both survive");
    }

    #[test]
    fn a_class_index_signature_does_not_take_a_method_discriminant() {
        // Class index signatures use `3_000_000 + index`. A method's disc is
        // `index * 1000`, so method 3000 is the same number as the first
        // index signature when both are named `__index`.
        let mut source_text = String::from("export class C {\n");
        for n in 0..3000 {
            source_text.push_str(&format!("  m{n}(): void;\n"));
        }
        source_text.push_str("  __index(): void;\n  [k: string]: unknown;\n}\n");
        let dir = tempfile::tempdir().expect("create tempdir");
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"name":"class-index-disc","version":"1.0.0","types":"index.d.ts"}"#,
        )
        .unwrap();
        std::fs::write(dir.path().join("index.d.ts"), &source_text).unwrap();
        let source = PackageSource::new(dir.path(), "class-index-disc", "1.0.0");
        let lineage =
            PackageLineageId::new(EcosystemId::new("npm"), PackageName::new("class-index-disc"));
        let produced = produce(&TypescriptProducer::new(), &source, &lineage, &Unlinked)
            .expect("a class index signature on the method lattice must seal");
        let indexes = produced
            .table
            .iter()
            .filter(|(_, entry)| entry.sym().name == "__index")
            .count();
        assert_eq!(indexes, 2, "the method and the index signature both survive");
    }

    #[test]
    fn a_construct_signature_does_not_take_a_method_discriminant() {
        // Construct signatures use their index as the disc and the name
        // `new_N`. A method at index 1 is also disc 1000.
        let mut source_text = String::from("export interface Bag {\n  pad(): void;\n  new_1000(): void;\n");
        for _ in 0..=1000 {
            source_text.push_str("  new (): object;\n");
        }
        source_text.push_str("}\n");
        let dir = tempfile::tempdir().expect("create tempdir");
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"name":"construct-disc","version":"1.0.0","types":"index.d.ts"}"#,
        )
        .unwrap();
        std::fs::write(dir.path().join("index.d.ts"), &source_text).unwrap();
        let source = PackageSource::new(dir.path(), "construct-disc", "1.0.0");
        let lineage =
            PackageLineageId::new(EcosystemId::new("npm"), PackageName::new("construct-disc"));
        let produced = produce(&TypescriptProducer::new(), &source, &lineage, &Unlinked)
            .expect("a construct signature on the method lattice must seal");
        let news = produced
            .table
            .iter()
            .filter(|(_, entry)| entry.sym().name == "new_1000")
            .count();
        assert_eq!(news, 2, "the method and the construct signature both survive");
    }

    #[test]
    fn a_call_signature_and_namespace_function_of_the_same_name_both_survive() {
        let dir = tempfile::tempdir().expect("create tempdir");
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"name":"call-ns","version":"1.0.0","types":"index.d.ts"}"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("index.d.ts"),
            "export interface I { (value: number): string; }\n\
             export namespace I { export function __call(): void; }\n",
        )
        .unwrap();
        let source = PackageSource::new(dir.path(), "call-ns", "1.0.0");
        let lineage = PackageLineageId::new(EcosystemId::new("npm"), PackageName::new("call-ns"));
        let produced = produce(&TypescriptProducer::new(), &source, &lineage, &Unlinked)
            .expect("a call signature and a namespace function must both seal");
        let calls = produced
            .table
            .iter()
            .filter(|(_, entry)| {
                entry.sym().name == "__call"
                    && matches!(entry.kind(), EntryInner::Owned(Kind::Function(_)))
            })
            .count();
        assert_eq!(calls, 2, "the call signature and the namespace function both survive");
    }

    #[test]
    fn an_index_signature_and_namespace_function_of_the_same_name_both_survive() {
        let dir = tempfile::tempdir().expect("create tempdir");
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"name":"index-ns","version":"1.0.0","types":"index.d.ts"}"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("index.d.ts"),
            "export interface I { [key: string]: number; }\n\
             export namespace I { export function __index(): void; }\n",
        )
        .unwrap();
        let source = PackageSource::new(dir.path(), "index-ns", "1.0.0");
        let lineage = PackageLineageId::new(EcosystemId::new("npm"), PackageName::new("index-ns"));
        let produced = produce(&TypescriptProducer::new(), &source, &lineage, &Unlinked)
            .expect("an index signature and a namespace function must both seal");
        let indexes = produced
            .table
            .iter()
            .filter(|(_, entry)| {
                entry.sym().name == "__index"
                    && matches!(entry.kind(), EntryInner::Owned(Kind::Function(_)))
            })
            .count();
        assert_eq!(indexes, 2, "the index signature and the namespace function both survive");
    }

    #[test]
    fn a_construct_signature_and_namespace_overload_both_survive() {
        let dir = tempfile::tempdir().expect("create tempdir");
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"name":"ctor-ns","version":"1.0.0","types":"index.d.ts"}"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("index.d.ts"),
            "export interface I { new (): object; new (value: number): object; }\n\
             export namespace I {\n\
               export function new_1(): void;\n\
               export function new_1(value: string): void;\n\
             }\n",
        )
        .unwrap();
        let source = PackageSource::new(dir.path(), "ctor-ns", "1.0.0");
        let lineage = PackageLineageId::new(EcosystemId::new("npm"), PackageName::new("ctor-ns"));
        let produced = produce(&TypescriptProducer::new(), &source, &lineage, &Unlinked)
            .expect("a construct signature and a namespace overload must both seal");
        let ctors = produced
            .table
            .iter()
            .filter(|(_, entry)| {
                entry.sym().name == "new_1"
                    && matches!(entry.kind(), EntryInner::Owned(Kind::Function(_)))
            })
            .count();
        assert_eq!(ctors, 3, "the construct signature and both namespace overloads survive");
    }

    #[test]
    fn a_local_function_and_a_reexport_of_the_same_name_both_survive() {
        let dir = tempfile::tempdir().expect("create tempdir");
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"name":"reexport-local","version":"1.0.0","types":"index.d.ts"}"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("other.d.ts"),
            "export function bar(b: string): void;\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("index.d.ts"),
            "export function foo(a: number): void;\nexport { bar as foo } from \"./other\";\n",
        )
        .unwrap();
        let source = PackageSource::new(dir.path(), "reexport-local", "1.0.0");
        let lineage =
            PackageLineageId::new(EcosystemId::new("npm"), PackageName::new("reexport-local"));
        let produced = produce(&TypescriptProducer::new(), &source, &lineage, &Unlinked)
            .expect("a local function and a same-named reexport must both seal");
        let foos = produced
            .table
            .iter()
            .filter(|(_, entry)| entry.sym().name == "foo")
            .count();
        assert_eq!(foos, 2, "the local function and the reexport both survive");
    }

    #[test]
    fn two_commonjs_reexports_of_one_name_both_survive() {
        let dir = tempfile::tempdir().expect("create tempdir");
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"name":"cjs-re","version":"1.0.0","main":"index.js"}"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("index.js"),
            "var left = require(\"left-pad\");\n\
             var right = require(\"right-pad\");\n\
             exports.foo = left;\n\
             exports.foo = right;\n",
        )
        .unwrap();
        let source = PackageSource::new(dir.path(), "cjs-re", "1.0.0");
        let lineage = PackageLineageId::new(EcosystemId::new("npm"), PackageName::new("cjs-re"));
        let produced = produce(&TypescriptProducer::new(), &source, &lineage, &Unlinked)
            .expect("two CommonJS reexports of one name must seal");
        let foos = produced
            .table
            .iter()
            .filter(|(_, entry)| entry.sym().name == "foo")
            .count();
        assert_eq!(foos, 2, "both package reexports survive");
    }

    #[test]
    fn one_commonjs_package_assigned_twice_is_one_export() {
        let dir = tempfile::tempdir().expect("create tempdir");
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"name":"cjs-same","version":"1.0.0","main":"index.js"}"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("index.js"),
            "var left = require(\"left-pad\");\n\
             exports.foo = left;\n\
             exports.foo = left;\n",
        )
        .unwrap();
        let source = PackageSource::new(dir.path(), "cjs-same", "1.0.0");
        let lineage = PackageLineageId::new(EcosystemId::new("npm"), PackageName::new("cjs-same"));
        let produced = produce(&TypescriptProducer::new(), &source, &lineage, &Unlinked)
            .expect("assigning one package twice must seal");
        let foos = produced
            .table
            .iter()
            .filter(|(_, entry)| entry.sym().name == "foo")
            .count();
        assert_eq!(foos, 1, "the same package is one export");
    }

    #[test]
    fn an_import_equals_require_is_declared() {
        let dir = tempfile::tempdir().expect("create tempdir");
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"name":"import-eq","version":"1.0.0","types":"index.d.ts"}"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("index.d.ts"),
            "import Foo = require(\"left-pad\");\nexport = Foo;\n",
        )
        .unwrap();
        let source = PackageSource::new(dir.path(), "import-eq", "1.0.0");
        let lineage = PackageLineageId::new(EcosystemId::new("npm"), PackageName::new("import-eq"));
        let produced = produce(&TypescriptProducer::new(), &source, &lineage, &Unlinked)
            .expect("import equals must seal");
        let foos = produced
            .table
            .iter()
            .filter(|(_, entry)| {
                entry.sym().name == "Foo" && matches!(entry.kind(), EntryInner::Reference(_))
            })
            .count();
        assert_eq!(foos, 1, "import Foo = require must reference the package");
    }

    #[test]
    fn an_export_default_literal_is_declared() {
        let dir = tempfile::tempdir().expect("create tempdir");
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"name":"default-lit","version":"1.0.0","types":"index.d.ts"}"#,
        )
        .unwrap();
        std::fs::write(dir.path().join("value.d.ts"), "export default 42;\n").unwrap();
        std::fs::write(
            dir.path().join("index.d.ts"),
            "export { default as answer } from \"./value\";\n\
             export function foo(): number;\n\
             export default foo;\n",
        )
        .unwrap();
        let source = PackageSource::new(dir.path(), "default-lit", "1.0.0");
        let lineage =
            PackageLineageId::new(EcosystemId::new("npm"), PackageName::new("default-lit"));
        let produced = produce(&TypescriptProducer::new(), &source, &lineage, &Unlinked)
            .expect("a literal default must seal");
        let literal = produced
            .table
            .iter()
            .filter(|(_, entry)| {
                entry.sym().name == "default"
                    && entry.sym().source.ends_with("value.d.ts")
                    && matches!(entry.kind(), EntryInner::Owned(Kind::Const(_)))
            })
            .count();
        assert_eq!(literal, 1, "export default 42 must be a const named default");
        let answer = produced.table.iter().any(|(_, entry)| {
            entry.sym().name == "answer"
                && matches!(entry.kind(), EntryInner::Reference(Ref::Intro(_)))
        });
        assert!(
            answer,
            "export {{ default as answer }} must point at the literal default"
        );
        let alias = produced
            .table
            .iter()
            .filter(|(_, entry)| {
                entry.sym().name == "default"
                    && entry.sym().source.ends_with("index.d.ts")
                    && matches!(entry.kind(), EntryInner::Reference(Ref::Intro(_)))
            })
            .count();
        assert_eq!(alias, 1, "export default foo must stay one local alias");
    }

    #[test]
    fn an_exported_namespace_import_is_declared() {
        let dir = tempfile::tempdir().expect("create tempdir");
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"name":"ns-import","version":"1.0.0","types":"index.d.ts"}"#,
        )
        .unwrap();
        std::fs::write(dir.path().join("there.d.ts"), "export function kept(): void;\n").unwrap();
        std::fs::write(
            dir.path().join("mid.d.ts"),
            "import * as left from \"left-pad\";\n\
             export { left };\n\
             import * as local from \"./there\";\n\
             export { local };\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("index.d.ts"),
            "export { left, local } from \"./mid\";\n",
        )
        .unwrap();
        let source = PackageSource::new(dir.path(), "ns-import", "1.0.0");
        let lineage = PackageLineageId::new(EcosystemId::new("npm"), PackageName::new("ns-import"));
        let produced = produce(&TypescriptProducer::new(), &source, &lineage, &Unlinked)
            .expect("an exported namespace import must seal");
        let left = produced
            .table
            .iter()
            .filter(|(_, entry)| {
                entry.sym().name == "left"
                    && entry.sym().source.ends_with("mid.d.ts")
                    && matches!(entry.kind(), EntryInner::Reference(_))
            })
            .count();
        assert_eq!(left, 1, "import * as left from a package must be declared");
        let local = produced.table.iter().any(|(_, entry)| {
            entry.sym().name == "local"
                && entry.sym().source.ends_with("mid.d.ts")
                && matches!(entry.kind(), EntryInner::Reference(Ref::Intro(_)))
        });
        assert!(
            local,
            "import * as local from a file in the package must stay local"
        );
        let barreled = produced.table.iter().any(|(_, entry)| {
            entry.sym().name == "left"
                && entry.sym().source.ends_with("index.d.ts")
                && matches!(entry.kind(), EntryInner::Reference(Ref::Intro(_)))
        });
        assert!(
            barreled,
            "export {{ left }} from the mid file must point at that local binding"
        );
    }

    #[test]
    fn an_export_assignment_and_namespace_export_are_declared() {
        let dir = tempfile::tempdir().expect("create tempdir");
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"name":"export-assign","version":"1.0.0","types":"index.d.ts"}"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("index.d.ts"),
            "declare function pad(s: string): string;\nexport = pad;\nexport as namespace leftPad;\n",
        )
        .unwrap();
        let source = PackageSource::new(dir.path(), "export-assign", "1.0.0");
        let lineage =
            PackageLineageId::new(EcosystemId::new("npm"), PackageName::new("export-assign"));
        let produced = produce(&TypescriptProducer::new(), &source, &lineage, &Unlinked)
            .expect("export = and export as namespace must seal");
        let exported = produced
            .table
            .iter()
            .filter(|(_, entry)| {
                entry.sym().name == "pad"
                    && matches!(entry.kind(), EntryInner::Owned(Kind::Function(_)))
            })
            .count();
        assert_eq!(exported, 1, "declare function pad must survive export =");
        let ns = produced
            .table
            .iter()
            .filter(|(_, entry)| entry.sym().name == "leftPad")
            .count();
        assert_eq!(ns, 1, "export as namespace leftPad must declare leftPad");
    }

    #[test]
    fn an_export_assignment_of_a_function_expression_is_declared() {
        let dir = tempfile::tempdir().expect("create tempdir");
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"name":"export-fn","version":"1.0.0","main":"index.ts"}"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("index.ts"),
            "export = function (s: string): string { return s; };\n",
        )
        .unwrap();
        let source = PackageSource::new(dir.path(), "export-fn", "1.0.0");
        let lineage = PackageLineageId::new(EcosystemId::new("npm"), PackageName::new("export-fn"));
        let produced = produce(&TypescriptProducer::new(), &source, &lineage, &Unlinked)
            .expect("export = of a function expression must seal");
        let exported = produced
            .table
            .iter()
            .filter(|(_, entry)| {
                entry.sym().name == "default"
                    && matches!(entry.kind(), EntryInner::Owned(Kind::Function(_)))
            })
            .count();
        assert_eq!(exported, 1, "export = function must declare the function");
        let param = produced
            .table
            .iter()
            .filter(|(_, entry)| {
                entry.sym().name == "s" && matches!(entry.kind(), EntryInner::Owned(Kind::Param(_)))
            })
            .count();
        assert_eq!(param, 1, "the function parameter must be declared");
    }

    #[test]
    fn a_global_augmentation_declares_its_members() {
        let dir = tempfile::tempdir().expect("create tempdir");
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"name":"global-aug","version":"1.0.0","types":"index.d.ts"}"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("index.d.ts"),
            "export {};\n\
             declare global {\n\
               interface Window { customProp: string; }\n\
               function greet(name: string): void;\n\
             }\n",
        )
        .unwrap();
        let source = PackageSource::new(dir.path(), "global-aug", "1.0.0");
        let lineage = PackageLineageId::new(EcosystemId::new("npm"), PackageName::new("global-aug"));
        let produced = produce(&TypescriptProducer::new(), &source, &lineage, &Unlinked)
            .expect("declare global must seal");
        let window = produced
            .table
            .iter()
            .filter(|(_, entry)| entry.sym().name == "Window")
            .count();
        assert_eq!(window, 1, "declare global must declare interface Window");
        let greet = produced
            .table
            .iter()
            .filter(|(_, entry)| {
                entry.sym().name == "greet"
                    && matches!(entry.kind(), EntryInner::Owned(Kind::Function(_)))
            })
            .count();
        assert_eq!(greet, 1, "declare global must declare function greet");
        let param = produced
            .table
            .iter()
            .filter(|(_, entry)| {
                entry.sym().name == "name" && matches!(entry.kind(), EntryInner::Owned(Kind::Param(_)))
            })
            .count();
        assert_eq!(param, 1, "greet's parameter must be declared");
    }

    #[test]
    fn a_destructured_export_declares_each_binding() {
        let dir = tempfile::tempdir().expect("create tempdir");
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"name":"destructure","version":"1.0.0","types":"index.d.ts"}"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("index.d.ts"),
            "declare const pair: { left: number; right: number; items: number[] };\n\
             export const { left, right: renamed, ...rest } = pair;\n\
             export const [first, second] = pair.items;\n",
        )
        .unwrap();
        let source = PackageSource::new(dir.path(), "destructure", "1.0.0");
        let lineage =
            PackageLineageId::new(EcosystemId::new("npm"), PackageName::new("destructure"));
        let produced = produce(&TypescriptProducer::new(), &source, &lineage, &Unlinked)
            .expect("a destructured export must seal");
        for name in ["left", "renamed", "rest", "first", "second"] {
            let count = produced
                .table
                .iter()
                .filter(|(_, entry)| entry.sym().name == name)
                .count();
            assert_eq!(count, 1, "{name} must be its own declaration");
        }
    }

    #[test]
    fn a_commonjs_object_export_declares_each_property() {
        let dir = tempfile::tempdir().expect("create tempdir");
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"name":"cjs-object","version":"1.0.0","main":"index.js"}"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("index.js"),
            "module.exports = {\n\
               left: function (s) { return s; },\n\
               right: 1,\n\
             };\n",
        )
        .unwrap();
        let source = PackageSource::new(dir.path(), "cjs-object", "1.0.0");
        let lineage =
            PackageLineageId::new(EcosystemId::new("npm"), PackageName::new("cjs-object"));
        let produced = produce(&TypescriptProducer::new(), &source, &lineage, &Unlinked)
            .expect("a CommonJS object export must seal");
        let left = produced
            .table
            .iter()
            .filter(|(_, entry)| {
                entry.sym().name == "left"
                    && matches!(entry.kind(), EntryInner::Owned(Kind::Function(_)))
            })
            .count();
        assert_eq!(left, 1, "left must be the function property");
        let right = produced
            .table
            .iter()
            .filter(|(_, entry)| {
                entry.sym().name == "right"
                    && matches!(entry.kind(), EntryInner::Owned(Kind::Const(_)))
            })
            .count();
        assert_eq!(right, 1, "right must be the literal property");
    }

    #[test]
    fn a_commonjs_class_export_is_declared() {
        let dir = tempfile::tempdir().expect("create tempdir");
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"name":"cjs-class","version":"1.0.0","main":"index.js"}"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("index.js"),
            "module.exports = class Foo {\n\
               bar() { return 1; }\n\
             };\n\
             exports.Baz = class {\n\
               qux() { return 2; }\n\
             };\n",
        )
        .unwrap();
        let source = PackageSource::new(dir.path(), "cjs-class", "1.0.0");
        let lineage = PackageLineageId::new(EcosystemId::new("npm"), PackageName::new("cjs-class"));
        let produced = produce(&TypescriptProducer::new(), &source, &lineage, &Unlinked)
            .expect("a CommonJS class export must seal");
        let foo = produced
            .table
            .iter()
            .filter(|(_, entry)| {
                entry.sym().name == "Foo" && matches!(entry.kind(), EntryInner::Owned(Kind::Record(_)))
            })
            .count();
        assert_eq!(foo, 1, "module.exports = class Foo must declare Foo");
        let bar = produced
            .table
            .iter()
            .filter(|(_, entry)| {
                entry.sym().name == "bar"
                    && matches!(entry.kind(), EntryInner::Owned(Kind::Function(_)))
            })
            .count();
        assert_eq!(bar, 1, "Foo.bar must be declared");
        let baz = produced
            .table
            .iter()
            .filter(|(_, entry)| {
                entry.sym().name == "Baz" && matches!(entry.kind(), EntryInner::Owned(Kind::Record(_)))
            })
            .count();
        assert_eq!(baz, 1, "exports.Baz = class must declare Baz");
        let qux = produced
            .table
            .iter()
            .filter(|(_, entry)| {
                entry.sym().name == "qux"
                    && matches!(entry.kind(), EntryInner::Owned(Kind::Function(_)))
            })
            .count();
        assert_eq!(qux, 1, "Baz.qux must be declared");
    }

    #[test]
    fn a_commonjs_function_export_is_declared() {
        let dir = tempfile::tempdir().expect("create tempdir");
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"name":"cjs-fn","version":"1.0.0","main":"index.js"}"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("index.js"),
            "module.exports = function pad(s) { return s; };\n\
             exports.left = (s) => s;\n",
        )
        .unwrap();
        let source = PackageSource::new(dir.path(), "cjs-fn", "1.0.0");
        let lineage = PackageLineageId::new(EcosystemId::new("npm"), PackageName::new("cjs-fn"));
        let produced = produce(&TypescriptProducer::new(), &source, &lineage, &Unlinked)
            .expect("a CommonJS function export must seal");
        let pad = produced
            .table
            .iter()
            .filter(|(_, entry)| {
                entry.sym().name == "pad"
                    && matches!(entry.kind(), EntryInner::Owned(Kind::Function(_)))
            })
            .count();
        assert_eq!(pad, 1, "module.exports = function pad must declare pad");
        let param = produced
            .table
            .iter()
            .filter(|(_, entry)| {
                entry.sym().name == "s" && matches!(entry.kind(), EntryInner::Owned(Kind::Param(_)))
            })
            .count();
        assert_eq!(param, 2, "pad and left must each declare parameter s");
        let left = produced
            .table
            .iter()
            .filter(|(_, entry)| {
                entry.sym().name == "left"
                    && matches!(entry.kind(), EntryInner::Owned(Kind::Function(_)))
            })
            .count();
        assert_eq!(left, 1, "exports.left = (s) => s must declare left");
    }

    #[test]
    fn a_commonjs_object_export_keeps_arrow_and_class_properties() {
        let dir = tempfile::tempdir().expect("create tempdir");
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"name":"cjs-members","version":"1.0.0","main":"index.js"}"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("index.js"),
            "module.exports = {\n\
               left: (s) => s,\n\
               Baz: class { qux() { return 2; } },\n\
             };\n",
        )
        .unwrap();
        let source = PackageSource::new(dir.path(), "cjs-members", "1.0.0");
        let lineage =
            PackageLineageId::new(EcosystemId::new("npm"), PackageName::new("cjs-members"));
        let produced = produce(&TypescriptProducer::new(), &source, &lineage, &Unlinked)
            .expect("object members that are functions and classes must seal");
        let left = produced
            .table
            .iter()
            .filter(|(_, entry)| {
                entry.sym().name == "left"
                    && matches!(entry.kind(), EntryInner::Owned(Kind::Function(_)))
            })
            .count();
        assert_eq!(left, 1, "an arrow property must be a function");
        let baz = produced
            .table
            .iter()
            .filter(|(_, entry)| {
                entry.sym().name == "Baz" && matches!(entry.kind(), EntryInner::Owned(Kind::Record(_)))
            })
            .count();
        assert_eq!(baz, 1, "a class property must be a class");
        let qux = produced
            .table
            .iter()
            .filter(|(_, entry)| {
                entry.sym().name == "qux"
                    && matches!(entry.kind(), EntryInner::Owned(Kind::Function(_)))
            })
            .count();
        assert_eq!(qux, 1, "the class method must be declared");
    }

    #[test]
    fn a_commonjs_prototype_method_is_declared() {
        let dir = tempfile::tempdir().expect("create tempdir");
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"name":"cjs-proto","version":"1.0.0","main":"index.js"}"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("index.js"),
            "function Foo() {}\n\
             Foo.prototype.bar = function (s) { return s; };\n\
             module.exports = Foo;\n",
        )
        .unwrap();
        let source = PackageSource::new(dir.path(), "cjs-proto", "1.0.0");
        let lineage = PackageLineageId::new(EcosystemId::new("npm"), PackageName::new("cjs-proto"));
        let produced = produce(&TypescriptProducer::new(), &source, &lineage, &Unlinked)
            .expect("a prototype method must seal");
        let foo = produced
            .table
            .iter()
            .filter(|(_, entry)| {
                entry.sym().name == "Foo"
                    && matches!(entry.kind(), EntryInner::Owned(Kind::Function(_)))
            })
            .count();
        assert_eq!(foo, 1, "the constructor must stay a function");
        let bar = produced
            .table
            .iter()
            .filter(|(_, entry)| {
                entry.sym().name == "bar"
                    && matches!(entry.kind(), EntryInner::Owned(Kind::Function(_)))
            })
            .count();
        assert_eq!(bar, 1, "Foo.prototype.bar must declare bar");
        let param = produced.table.iter().any(|(_, entry)| {
            entry.sym().name == "s" && matches!(entry.kind(), EntryInner::Owned(Kind::Param(_)))
        });
        assert!(param, "bar's parameter must be declared");
    }

    #[test]
    fn a_commonjs_prototype_object_declares_each_method() {
        let dir = tempfile::tempdir().expect("create tempdir");
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"name":"cjs-proto-obj","version":"1.0.0","main":"index.js"}"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("index.js"),
            "function Foo() {}\n\
             Foo.prototype = {\n\
               bar: function (s) { return s; },\n\
               baz: (n) => n,\n\
             };\n\
             module.exports = Foo;\n",
        )
        .unwrap();
        let source = PackageSource::new(dir.path(), "cjs-proto-obj", "1.0.0");
        let lineage =
            PackageLineageId::new(EcosystemId::new("npm"), PackageName::new("cjs-proto-obj"));
        let produced = produce(&TypescriptProducer::new(), &source, &lineage, &Unlinked)
            .expect("a prototype object must seal");
        let foo = produced
            .table
            .iter()
            .filter(|(_, entry)| {
                entry.sym().name == "Foo"
                    && matches!(entry.kind(), EntryInner::Owned(Kind::Function(_)))
            })
            .count();
        assert_eq!(foo, 1, "the constructor must stay a function");
        for name in ["bar", "baz"] {
            let count = produced
                .table
                .iter()
                .filter(|(_, entry)| {
                    entry.sym().name == name
                        && matches!(entry.kind(), EntryInner::Owned(Kind::Function(_)))
                })
                .count();
            assert_eq!(count, 1, "{name} must be a function");
        }
    }

    #[test]
    fn a_destructured_parameter_declares_each_binding() {
        let dir = tempfile::tempdir().expect("create tempdir");
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"name":"param-pat","version":"1.0.0","types":"index.d.ts"}"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("index.d.ts"),
            "export function take({ left, right }: { left: number; right: number }, [first]: number[]): void;\n",
        )
        .unwrap();
        let source = PackageSource::new(dir.path(), "param-pat", "1.0.0");
        let lineage = PackageLineageId::new(EcosystemId::new("npm"), PackageName::new("param-pat"));
        let produced = produce(&TypescriptProducer::new(), &source, &lineage, &Unlinked)
            .expect("a destructured parameter must seal");
        for name in ["left", "right", "first"] {
            let count = produced
                .table
                .iter()
                .filter(|(_, entry)| {
                    entry.sym().name == name
                        && matches!(entry.kind(), EntryInner::Owned(Kind::Param(_)))
                })
                .count();
            assert_eq!(count, 1, "{name} must be its own parameter");
        }
    }

    #[test]
    fn a_function_type_declares_each_destructured_binding() {
        let dir = tempfile::tempdir().expect("create tempdir");
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"name":"fn-type","version":"1.0.0","types":"index.d.ts"}"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("index.d.ts"),
            "export type Take = ({ left, right }: { left: number; right: number }) => void;\n",
        )
        .unwrap();
        let source = PackageSource::new(dir.path(), "fn-type", "1.0.0");
        let lineage = PackageLineageId::new(EcosystemId::new("npm"), PackageName::new("fn-type"));
        let produced = produce(&TypescriptProducer::new(), &source, &lineage, &Unlinked)
            .expect("a function type must seal");
        for name in ["left", "right"] {
            let count = produced
                .table
                .iter()
                .filter(|(_, entry)| {
                    entry.sym().name == name
                        && matches!(entry.kind(), EntryInner::Owned(Kind::Param(_)))
                })
                .count();
            assert_eq!(count, 1, "{name} must be its own parameter");
        }
    }

    #[test]
    fn an_import_equals_require_of_a_missing_file_is_a_foreign_reference() {
        let dir = tempfile::tempdir().expect("create tempdir");
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"name":"import-missing","version":"1.0.0","types":"index.d.ts"}"#,
        )
        .unwrap();
        std::fs::write(dir.path().join("there.d.ts"), "export function kept(): void;\n").unwrap();
        std::fs::write(
            dir.path().join("index.d.ts"),
            "import Gone = require(\"./missing\");\nimport Local = require(\"./there\");\n",
        )
        .unwrap();
        let source = PackageSource::new(dir.path(), "import-missing", "1.0.0");
        let lineage =
            PackageLineageId::new(EcosystemId::new("npm"), PackageName::new("import-missing"));
        let produced = produce(&TypescriptProducer::new(), &source, &lineage, &Unlinked)
            .expect("import equals of a missing file must seal");
        let gone = produced
            .table
            .iter()
            .filter(|(_, entry)| {
                entry.sym().name == "Gone" && matches!(entry.kind(), EntryInner::Reference(_))
            })
            .count();
        assert_eq!(gone, 1, "require of a missing file must still declare Gone");
        let local = produced.table.iter().any(|(_, entry)| {
            entry.sym().name == "Local"
                && matches!(entry.kind(), EntryInner::Reference(Ref::Intro(_)))
        });
        assert!(
            local,
            "require of a file in the package must stay a local reference"
        );
    }

    #[test]
    fn a_local_import_equals_is_an_alias_of_its_target() {
        let dir = tempfile::tempdir().expect("create tempdir");
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"name":"import-alias","version":"1.0.0","types":"index.d.ts"}"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("index.d.ts"),
            "export class Bar {}\n\
             import Foo = Bar;\n\
             export import Pub = Bar;\n\
             import Gone = NotDeclared;\n\
             export namespace NS { export class Bar {} }\n\
             import Qual = NS.Bar;\n",
        )
        .unwrap();
        let source = PackageSource::new(dir.path(), "import-alias", "1.0.0");
        let lineage =
            PackageLineageId::new(EcosystemId::new("npm"), PackageName::new("import-alias"));
        let produced = produce(&TypescriptProducer::new(), &source, &lineage, &Unlinked)
            .expect("a local import-equals must seal");
        let bar = produced
            .table
            .iter()
            .find(|(id, entry)| {
                entry.sym().name == "Bar"
                    && matches!(entry.kind(), EntryInner::Owned(Kind::Record(_)))
                    && produced.table.parent_of(*id).is_some_and(|parent| {
                        produced
                            .table
                            .get(parent)
                            .is_some_and(|p| p.sym().name != "NS")
                    })
            })
            .map(|(id, _)| id);
        let foo_points_at_bar = produced.table.iter().any(|(_, entry)| {
            entry.sym().name == "Foo"
                && match entry.kind() {
                    EntryInner::Owned(Kind::Alias(alias)) => match alias.target.as_ref() {
                        Some(Type::Nominal(Ref::Intro(id))) => bar == Some(*id),
                        _ => false,
                    },
                    _ => false,
                }
        });
        assert!(
            foo_points_at_bar,
            "import Foo = Bar must be an alias of the local class"
        );
        let pub_is_public = produced.table.iter().any(|(_, entry)| {
            entry.sym().name == "Pub"
                && entry.sym().visibility == nudox_ir::entry::Visibility::Public
                && matches!(entry.kind(), EntryInner::Owned(Kind::Alias(_)))
        });
        assert!(pub_is_public, "export import Pub = Bar must be a public alias");
        let gone = produced.table.iter().any(|(_, entry)| {
            entry.sym().name == "Gone"
                && match entry.kind() {
                    EntryInner::Owned(Kind::Alias(alias)) => {
                        matches!(
                            alias.target.as_ref(),
                            Some(Type::Unknown(nudox_ir::kinds::ty::UnknownType::UnresolvedExternal {
                                name,
                            })) if name == "NotDeclared"
                        )
                    }
                    _ => false,
                }
        });
        assert!(
            gone,
            "import Gone = NotDeclared must stay an alias and must not invent a slot"
        );
        let qual = produced.table.iter().any(|(_, entry)| {
            entry.sym().name == "Qual"
                && match entry.kind() {
                    EntryInner::Owned(Kind::Alias(alias)) => {
                        matches!(
                            alias.target.as_ref(),
                            Some(Type::Unknown(nudox_ir::kinds::ty::UnknownType::UnresolvedExternal {
                                name,
                            })) if name == "NS.Bar"
                        )
                    }
                    _ => false,
                }
        });
        assert!(
            qual,
            "import Qual = NS.Bar must name the qualified target"
        );
    }

    #[test]
    fn a_named_reexport_of_a_package_is_a_foreign_reference() {
        let dir = tempfile::tempdir().expect("create tempdir");
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"name":"reexport-pkg","version":"1.0.0","types":"index.d.ts"}"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("index.d.ts"),
            "export { pad } from \"left-pad\";\nexport * as right from \"right-pad\";\n",
        )
        .unwrap();
        let source = PackageSource::new(dir.path(), "reexport-pkg", "1.0.0");
        let lineage =
            PackageLineageId::new(EcosystemId::new("npm"), PackageName::new("reexport-pkg"));
        let produced = produce(&TypescriptProducer::new(), &source, &lineage, &Unlinked)
            .expect("a package reexport must seal");
        let pad = produced
            .table
            .iter()
            .filter(|(_, entry)| {
                entry.sym().name == "pad" && matches!(entry.kind(), EntryInner::Reference(_))
            })
            .count();
        assert_eq!(pad, 1, "export {{ pad }} from a package must reference that package");
        let right = produced
            .table
            .iter()
            .filter(|(_, entry)| {
                entry.sym().name == "right" && matches!(entry.kind(), EntryInner::Reference(_))
            })
            .count();
        assert_eq!(right, 1, "export * as from a package must reference that package");
    }

    #[test]
    fn a_named_reexport_of_a_missing_file_is_a_foreign_reference() {
        let dir = tempfile::tempdir().expect("create tempdir");
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"name":"reexport-missing","version":"1.0.0","types":"index.d.ts"}"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("index.d.ts"),
            "export { gone } from \"./missing\";\n\
             export { local as renamed } from \"./also-missing\";\n\
             export * as bundle from \"./no-bundle\";\n",
        )
        .unwrap();
        let source = PackageSource::new(dir.path(), "reexport-missing", "1.0.0");
        let lineage =
            PackageLineageId::new(EcosystemId::new("npm"), PackageName::new("reexport-missing"));
        let produced = produce(&TypescriptProducer::new(), &source, &lineage, &Unlinked)
            .expect("a reexport of a missing file must seal");
        for name in ["gone", "renamed", "bundle"] {
            let count = produced
                .table
                .iter()
                .filter(|(_, entry)| {
                    entry.sym().name == name && matches!(entry.kind(), EntryInner::Reference(_))
                })
                .count();
            assert_eq!(count, 1, "{name} must be a reference, not a dropped export");
        }
    }

    #[test]
    fn a_barrel_of_a_package_reexport_stays_local() {
        let dir = tempfile::tempdir().expect("create tempdir");
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"name":"reexport-chain","version":"1.0.0","types":"index.d.ts"}"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("mid.d.ts"),
            "export { pad } from \"left-pad\";\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("index.d.ts"),
            "export { pad } from \"./mid\";\n",
        )
        .unwrap();
        let source = PackageSource::new(dir.path(), "reexport-chain", "1.0.0");
        let lineage =
            PackageLineageId::new(EcosystemId::new("npm"), PackageName::new("reexport-chain"));
        let produced = produce(&TypescriptProducer::new(), &source, &lineage, &Unlinked)
            .expect("a barrel of a package reexport must seal");
        let local = produced.table.iter().any(|(_, entry)| {
            entry.sym().name == "pad"
                && entry.sym().source.ends_with("index.d.ts")
                && matches!(entry.kind(), EntryInner::Reference(Ref::Intro(_)))
        });
        assert!(
            local,
            "the barrel pad must point at the local reexport, not a foreign key"
        );
    }

    #[test]
    fn an_interface_property_does_not_take_a_method_discriminant() {
        // Interface methods use `index * 1000`. Properties use `2_000_000 + index`.
        // Method 2000 and property 0 are the same number.
        let mut source_text = String::from("export interface Bag {\n  slot: number;\n");
        for n in 0..2000 {
            source_text.push_str(&format!("  m{n}(): void;\n"));
        }
        source_text.push_str("  slot(): void;\n}\n");
        let dir = tempfile::tempdir().expect("create tempdir");
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"name":"wide-iface","version":"1.0.0","types":"index.d.ts"}"#,
        )
        .unwrap();
        std::fs::write(dir.path().join("index.d.ts"), &source_text).unwrap();
        let source = PackageSource::new(dir.path(), "wide-iface", "1.0.0");
        let lineage =
            PackageLineageId::new(EcosystemId::new("npm"), PackageName::new("wide-iface"));
        let produced = produce(&TypescriptProducer::new(), &source, &lineage, &Unlinked)
            .expect("an interface property on the method lattice must seal");
        let slots = produced
            .table
            .iter()
            .filter(|(_, entry)| entry.sym().name == "slot")
            .count();
        assert_eq!(slots, 2, "the method and the property both survive");
    }

    #[test]
    fn stepped_interface_properties_do_not_share_a_discriminant() {
        // Properties 0..=1000 share a name. Methods 2000 and 2001 hold
        // 2000000 and 2001000. Each property that steps must see the disc
        // the earlier property actually took.
        let mut source_text = String::from("export interface Bag {\n");
        for _ in 0..=1000 {
            source_text.push_str("  slot: number;\n");
        }
        for n in 0..2000 {
            source_text.push_str(&format!("  m{n}(): void;\n"));
        }
        source_text.push_str("  slot(): void;\n  slot(): void;\n}\n");
        let dir = tempfile::tempdir().expect("create tempdir");
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"name":"wide-iface-step","version":"1.0.0","types":"index.d.ts"}"#,
        )
        .unwrap();
        std::fs::write(dir.path().join("index.d.ts"), &source_text).unwrap();
        let source = PackageSource::new(dir.path(), "wide-iface-step", "1.0.0");
        let lineage =
            PackageLineageId::new(EcosystemId::new("npm"), PackageName::new("wide-iface-step"));
        let produced = produce(&TypescriptProducer::new(), &source, &lineage, &Unlinked)
            .expect("stepped properties must not share a disc");
        let slots = produced
            .table
            .iter()
            .filter(|(_, entry)| entry.sym().name == "slot")
            .count();
        assert_eq!(slots, 1003, "1001 properties and 2 methods");
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

    /// An interface method's id is `member_index * 1000`. Folding that into
    /// the parameter discriminant (`* 1000` again) overflows `u32` once the
    /// interface has more than 4294 members. Two overloads of one name past
    /// that point used to declare the same parameter id.
    #[test]
    fn late_interface_overloads_keep_distinct_parameters() {
        let mut src = String::from("export interface Huge {\n");
        for i in 0..4295 {
            src.push_str(&format!("  m{i}(x: number): void;\n"));
        }
        src.push_str("  tail(x: number): void;\n  tail(x: string): void;\n}\n");

        let dir = tempfile::tempdir().expect("create tempdir");
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"name":"huge-iface","version":"1.0.0","types":"index.d.ts"}"#,
        )
        .expect("write package manifest");
        std::fs::write(dir.path().join("index.d.ts"), src).expect("write declarations");

        let source = PackageSource::new(dir.path(), "huge-iface", "1.0.0");
        let lineage =
            PackageLineageId::new(EcosystemId::new("npm"), PackageName::new("huge-iface"));
        let produced = produce(&TypescriptProducer::new(), &source, &lineage, &Unlinked)
            .expect("overloads past the discriminant ceiling must seal, not Duplicate");

        let tails = produced
            .table
            .iter()
            .filter(|(_, entry)| entry.sym().name == "tail")
            .count();
        assert_eq!(
            tails, 2,
            "both tail overloads must be declared, not collapsed onto one parameter id"
        );
    }

    /// Overload discriminants are adjacent integers. The parameter
    /// discriminant is `function_disc * 1000 + index`, so parameter 1000 of
    /// one overload is the same id as parameter 0 of the next when the
    /// names match.
    #[test]
    fn thousandth_parameter_does_not_collide_with_the_next_overload() {
        let mut params = String::new();
        for i in 0..1000 {
            if i > 0 {
                params.push_str(", ");
            }
            params.push_str(&format!("a{i}: number"));
        }
        params.push_str(", x: number");
        let src = format!(
            "export function tail({params}): void;\nexport function tail(x: string): void;\n"
        );

        let dir = tempfile::tempdir().expect("create tempdir");
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"name":"wide-fn","version":"1.0.0","types":"index.d.ts"}"#,
        )
        .expect("write package manifest");
        std::fs::write(dir.path().join("index.d.ts"), src).expect("write declarations");

        let source = PackageSource::new(dir.path(), "wide-fn", "1.0.0");
        let lineage = PackageLineageId::new(EcosystemId::new("npm"), PackageName::new("wide-fn"));
        let produced = produce(&TypescriptProducer::new(), &source, &lineage, &Unlinked)
            .expect("a 1001-parameter overload must not share a parameter id with the next overload");

        let tails = produced
            .table
            .iter()
            .filter(|(_, entry)| entry.sym().name == "tail")
            .count();
        assert_eq!(tails, 2, "both tail overloads must be declared");
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
                    let names: Vec<&str> = produced
                        .table
                        .iter()
                        .map(|(_, entry)| entry.sym().name.as_str())
                        .collect();
                    let required = match name.as_str() {
                        "axios" => "AxiosHeaders",
                        "follow-redirects" => "wrap",
                        "form-data" => "FormData",
                        "proxy-from-env" => "getProxyForUrl",
                        other => panic!("unexpected package {other}"),
                    };
                    assert!(
                        names.contains(&required),
                        "{name} must declare {required}, got {names:?}"
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
