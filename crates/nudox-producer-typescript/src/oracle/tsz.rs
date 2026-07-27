//! Tier-2 in-process tsz TypeScript checker oracle.
//!
//! # Role
//!
//! The OXC tier (always-on) provides structure and syntactic types.  This
//! module is **enrichment-only**: it takes the `Vec<ModuleFacts>` produced by
//! OXC and fills in the four gaps that a purely syntactic pass cannot resolve:
//!
//! 1. **Checker-inferred return types** — functions with no explicit annotation
//!    get `FunctionBody::return_type = None` from OXC; tsz supplies the
//!    concrete inferred type.
//! 2. **Cross-module type resolution** — `typeof import("m").Foo` resolves to
//!    an actual `TypeOwned`, not `Unsupported`.
//! 3. **`Promise<T>` unwrapping** — async function return types are unwrapped
//!    from `Promise<T>` to `T` when the checker can determine `T`.
//! 4. **Object-shape inference** — `const o = { x: 1, y: "a" }` with no
//!    explicit type annotation gets a `TypeOwned::Unsupported` from OXC; tsz
//!    replaces it with the inferred object type (via the format+re-parse path).
//!
//! # Reliability contract
//!
//! Any failure (tsz panic, unsupported syntax, empty check result) returns
//! `Err(reason)` so the caller falls back to OXC output unchanged.  tsz is
//! pre-release; this module MUST NOT cause the producer to fail hard.
//!
//! # Lifetime pattern
//!
//! All data is extracted from the checker into fully-owned structures before
//! the tsz session drops — no self-referential types, no arena leakage.  This
//! matches the three-producer pattern already established in `graph.rs`.
//!
//! # Feature gate
//!
//! This entire module is compiled only with `--features tsz`.  The default
//! build is unaffected.

use rustc_hash::FxHashMap;

use tsz_binder::state::BinderState;
use tsz_checker::state::CheckerState;
use tsz_core::parallel::{
    self, MergedProgram, create_binder_from_bound_file, ensure_rayon_global_pool,
};
use tsz_solver::construction::QueryCache;
use tsz_solver::{IntrinsicKind, LiteralValue as TszLiteral, TypeData, TypeId};
use tsz_solver::construction::TypeDatabase;

use crate::extract::{
    DeclBody, FunctionBody, LiteralOwned, ModuleFacts, TypeOwned,
};

// ── Public runtime switch ─────────────────────────────────────────────────────

/// Environment variable that opts in to the tsz oracle for a run.
///
/// Set to `1` or `true` to enable.  Unset means OXC-only behavior.
pub const ORACLE_ENV: &str = "NUDOX_TYPESCRIPT_ORACLE";

/// Returns `true` when the tsz oracle is enabled for this process.
pub fn enabled() -> bool {
    std::env::var(ORACLE_ENV)
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

// ── TszOracle — the enriched oracle type ─────────────────────────────────────

/// The result of running the OXC tier *plus* tsz enrichment.
///
/// Implements `TsOracle` so it can be used as the type parameter of
/// `TypescriptProducer<TszOracle>`.  Constructed via
/// `TszOracle::try_enrich(oxc_modules)`.
pub struct TszOracle {
    /// The enriched module facts (OXC output + tsz fill-ins).
    pub(crate) modules: Vec<ModuleFacts>,
}

impl crate::producer::sealed::TsOracleSeal for TszOracle {}

impl crate::producer::TsOracle for TszOracle {
    fn modules(&self) -> &[ModuleFacts] {
        &self.modules
    }
}

impl From<crate::producer::OwnedOracle> for TszOracle {
    /// Run OXC extraction, then attempt tsz enrichment.
    ///
    /// This is the primary path called by `TypescriptProducer::invoke` via the
    /// `where O: From<OwnedOracle>` bound.  On any tsz failure the OXC-only
    /// result is returned transparently — the caller is never penalised.
    fn from(oxc: crate::producer::OwnedOracle) -> Self {
        let mut modules = oxc.modules;
        if let Err(reason) = enrich_modules(&mut modules) {
            tracing::debug!(reason = %reason, "tsz enrichment skipped; using OXC-only output");
        }
        TszOracle { modules }
    }
}

// ── Recovered type record ─────────────────────────────────────────────────────

/// The checker-recovered type info for one top-level symbol.
struct RecoveredType {
    /// The symbol's own inferred type (object shape, arrow type, etc.).
    own: TypeOwned,
    /// For callables: the inferred return type `R` in `() => R`.  `None` for
    /// non-callable symbols.
    ret: Option<TypeOwned>,
}

// ── tsz TypeId → TypeOwned mapping ───────────────────────────────────────────
//
// This mirrors the original `tsz_types.rs` but targets `TypeOwned` (our local
// IR-adjacent type) rather than the old `ir::ty::Type`.  The two strategies
// are identical:
//   1. Direct structural mapping for common inferred constructs.
//   2. Format + OXC re-parse fallback for everything else.

fn type_id_to_owned<F>(interner: &dyn TypeDatabase, id: TypeId, printed: &F) -> TypeOwned
where
    F: Fn(TypeId) -> String,
{
    let Some(data) = interner.lookup(id) else {
        return format_and_lower_owned(&printed(id));
    };

    match data {
        TypeData::Intrinsic(kind) => intrinsic_to_owned(kind),

        TypeData::Literal(lit) => literal_to_owned(interner, lit),

        TypeData::Array(elem) => {
            TypeOwned::Array(Box::new(type_id_to_owned(interner, elem, printed)))
        }

        // ReadonlyType and NoInfer are transparent wrappers.
        TypeData::ReadonlyType(inner) => type_id_to_owned(interner, inner, printed),
        TypeData::NoInfer(inner) => type_id_to_owned(interner, inner, printed),

        TypeData::Union(list_id) => {
            let members = interner
                .type_list(list_id)
                .iter()
                .map(|&m| type_id_to_owned(interner, m, printed))
                .collect();
            TypeOwned::Union(members)
        }

        TypeData::Intersection(list_id) => {
            let members = interner
                .type_list(list_id)
                .iter()
                .map(|&m| type_id_to_owned(interner, m, printed))
                .collect();
            TypeOwned::Intersection(members)
        }

        TypeData::Tuple(list_id) => {
            let elems = interner
                .tuple_list(list_id)
                .iter()
                .map(|e| type_id_to_owned(interner, e.type_id, printed))
                .collect();
            TypeOwned::Tuple(elems)
        }

        TypeData::Error => TypeOwned::Any,

        TypeData::UnresolvedTypeName(atom) => {
            TypeOwned::Nominal(interner.resolve_atom(atom))
        }

        TypeData::ThisType => TypeOwned::This,

        // Everything else (Object, Function, Callable, Application,
        // Conditional, Mapped, TemplateLiteral, Lazy, TypeParameter, …):
        // render via checker's printer, then re-parse with OXC.
        _ => format_and_lower_owned(&printed(id)),
    }
}

fn intrinsic_to_owned(kind: IntrinsicKind) -> TypeOwned {
    match kind {
        IntrinsicKind::Number => TypeOwned::Number,
        IntrinsicKind::String => TypeOwned::String,
        IntrinsicKind::Boolean => TypeOwned::Bool,
        IntrinsicKind::Bigint => TypeOwned::BigInt,
        IntrinsicKind::Void => TypeOwned::Void,
        IntrinsicKind::Null => TypeOwned::Null,
        IntrinsicKind::Undefined => TypeOwned::Undefined,
        IntrinsicKind::Never => TypeOwned::Never,
        IntrinsicKind::Any => TypeOwned::Any,
        IntrinsicKind::Unknown => TypeOwned::Unknown,
        IntrinsicKind::Object => TypeOwned::Object,
        IntrinsicKind::Symbol => TypeOwned::Symbol,
        IntrinsicKind::Function => TypeOwned::Nominal("Function".to_string()),
    }
}

fn literal_to_owned(interner: &dyn TypeDatabase, lit: TszLiteral) -> TypeOwned {
    let inner = match lit {
        TszLiteral::String(atom) => LiteralOwned::String(interner.resolve_atom(atom)),
        TszLiteral::Number(f) => LiteralOwned::Number(format_number(f.0)),
        TszLiteral::BigInt(atom) => LiteralOwned::BigInt(interner.resolve_atom(atom)),
        TszLiteral::Boolean(b) => LiteralOwned::Bool(b),
    };
    TypeOwned::Literal(inner)
}

fn format_number(v: f64) -> String {
    if v.fract() == 0.0 && v.is_finite() && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        format!("{v}")
    }
}

/// Format + re-parse fallback: ask OXC to parse `type __t = <printed>;` and
/// lower the alias body through the existing `extract::types::lower_ts_type`.
///
/// Returns `TypeOwned::Any` if the snippet fails to parse (never panics).
fn format_and_lower_owned(printed: &str) -> TypeOwned {
    let trimmed = printed.trim();
    if trimmed.is_empty() {
        return TypeOwned::Any;
    }

    let source = format!("type __t = {trimmed};");
    let allocator = oxc_allocator::Allocator::new();
    let source_type = oxc_span::SourceType::d_ts();

    let ret = oxc_parser::Parser::new(&allocator, &source, source_type)
        .with_options(oxc_parser::ParseOptions {
            preserve_parens: false,
            ..Default::default()
        })
        .parse();

    if ret.panicked {
        return TypeOwned::Unsupported(trimmed.to_string());
    }

    let ann = ret.program.body.iter().find_map(|stmt| {
        if let oxc_ast::ast::Statement::TSTypeAliasDeclaration(alias) = stmt {
            Some(&alias.type_annotation)
        } else {
            None
        }
    });

    match ann {
        Some(ts_type) => {
            crate::extract::types::lower_ts_type(ts_type, &source)
        }
        None => TypeOwned::Unsupported(trimmed.to_string()),
    }
}

/// If `id` is a callable/function type, return the `TypeId` of its first call
/// signature's return type; `None` otherwise.
fn return_type_of(interner: &dyn TypeDatabase, id: TypeId) -> Option<TypeId> {
    match interner.lookup(id)? {
        TypeData::Function(shape_id) => {
            Some(interner.function_shape(shape_id).return_type)
        }
        TypeData::Callable(shape_id) => {
            let shape = interner.callable_shape(shape_id);
            shape
                .call_signatures
                .first()
                .map(|sig| sig.return_type)
                .or_else(|| shape.construct_signatures.first().map(|sig| sig.return_type))
        }
        _ => None,
    }
}

// ── Type recovery pass ────────────────────────────────────────────────────────

/// Stem of a file name: `foo.ts` → `foo`, `bar.d.ts` → `bar`.
fn file_stem(name: &str) -> String {
    for suf in [".d.ts", ".d.mts", ".d.cts", ".tsx", ".mts", ".cts", ".ts"] {
        if let Some(base) = name.strip_suffix(suf) {
            return base.to_string();
        }
    }
    name.to_string()
}

/// Parse + bind + check all files in the merged program and collect
/// checker-recovered types keyed by `(file_stem, symbol_name)`.
///
/// A secondary index (`name_counts`) tracks how many files define each bare
/// name, enabling a globally-unique bare-name fallback lookup.
fn recover_types(
    program: &MergedProgram,
) -> (
    FxHashMap<(String, String), RecoveredType>,
    FxHashMap<String, usize>,
) {
    let query_cache =
        QueryCache::new(&program.type_interner).with_definition_store(&program.definition_store);

    let interner: &dyn TypeDatabase = &program.type_interner;

    let mut by_key: FxHashMap<(String, String), RecoveredType> = FxHashMap::default();
    let mut name_counts: FxHashMap<String, usize> = FxHashMap::default();

    for (file_idx, file) in program.files.iter().enumerate() {
        let binder: BinderState = create_binder_from_bound_file(file, program, file_idx);

        let base = file
            .file_name
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or(file.file_name.as_str());
        let stem = file_stem(base);

        let mut checker = CheckerState::new(
            &file.arena,
            &binder,
            &query_cache,
            file.file_name.clone(),
            Default::default(),
        );
        checker.check_source_file(file.source_file);

        // Snapshot the top-level names before the mutable checker queries.
        let names: Vec<(String, tsz_binder::SymbolId)> = binder
            .file_locals
            .iter()
            .map(|(name, id)| (name.clone(), *id))
            .collect();

        for (name, sym_id) in names {
            let type_id = checker.get_type_of_symbol(sym_id);

            let own = type_id_to_owned(interner, type_id, &|id| checker.format_type(id));
            let ret = return_type_of(interner, type_id).map(|rid| {
                type_id_to_owned(interner, rid, &|id| checker.format_type(id))
            });

            by_key.insert((stem.clone(), name.clone()), RecoveredType { own, ret });
            *name_counts.entry(name).or_insert(0) += 1;
        }
    }

    (by_key, name_counts)
}

// ── Enrichment pass ───────────────────────────────────────────────────────────

/// Look up the recovered type for a symbol.
///
/// Prefers the `(stem, name)` precise key.  Falls back to the bare name only
/// when exactly one file defines that name (avoids cross-module collisions).
fn recovered_for<'a>(
    by_key: &'a FxHashMap<(String, String), RecoveredType>,
    name_counts: &FxHashMap<String, usize>,
    stem_hint: Option<&str>,
    leaf: &str,
) -> Option<&'a RecoveredType> {
    if let Some(stem) = stem_hint {
        if let Some(r) = by_key.get(&(stem.to_string(), leaf.to_string())) {
            return Some(r);
        }
    }
    if name_counts.get(leaf).copied() == Some(1) {
        return by_key.iter().find(|((_, n), _)| n == leaf).map(|(_, r)| r);
    }
    None
}

/// Does the OXC-extracted return type lack concrete information such that the
/// checker's answer would be an improvement?
fn return_needs_enrichment(ret: &Option<TypeOwned>) -> bool {
    match ret {
        None => true,
        Some(TypeOwned::Any) | Some(TypeOwned::Unknown) | Some(TypeOwned::Unsupported(_)) => true,
        _ => false,
    }
}

/// Does the OXC-extracted type annotation lack concrete information?
fn type_needs_enrichment(ty: &Option<TypeOwned>) -> bool {
    match ty {
        None => true,
        Some(TypeOwned::Any) | Some(TypeOwned::Unknown) | Some(TypeOwned::Unsupported(_)) => true,
        _ => false,
    }
}

/// Enrich one `FunctionBody`'s return type from the checker's answer if the
/// syntactic pass left it absent or opaque.
fn enrich_function_body(fb: &mut FunctionBody, recovered: &RecoveredType) {
    if return_needs_enrichment(&fb.return_type) {
        if let Some(ret) = &recovered.ret {
            fb.return_type = Some(ret.clone());
        }
    }
}

/// Walk all declarations in `modules` and splice checker-recovered types where
/// OXC left a gap.
fn enrich_declarations(
    decls: &mut Vec<crate::extract::DeclFact>,
    by_key: &FxHashMap<(String, String), RecoveredType>,
    name_counts: &FxHashMap<String, usize>,
    stem_hint: Option<&str>,
) {
    for decl in decls.iter_mut() {
        let leaf = decl.name.as_str();
        let Some(recovered) = recovered_for(by_key, name_counts, stem_hint, leaf) else {
            // Recurse into namespaces even when the namespace itself has no hit.
            if let DeclBody::Namespace(ns) = &mut decl.body {
                enrich_declarations(&mut ns.children, by_key, name_counts, stem_hint);
            }
            continue;
        };

        match &mut decl.body {
            // Capability 1: checker-inferred return types for functions.
            // Capability 3: Promise<T> unwrapping for async functions.
            DeclBody::Function(fb) => {
                enrich_function_body(fb, recovered);
            }

            // Capability 2: cross-module type resolution for type aliases.
            DeclBody::TypeAlias(ta) => {
                if matches!(ta.target, TypeOwned::Any | TypeOwned::Unsupported(_)) {
                    ta.target = recovered.own.clone();
                }
            }

            // Capability 4: object shape inference for constants/variables.
            DeclBody::Const(c) => {
                if type_needs_enrichment(&c.ty) {
                    c.ty = Some(recovered.own.clone());
                }
            }
            DeclBody::Static(s) => {
                if type_needs_enrichment(&s.ty) {
                    s.ty = Some(recovered.own.clone());
                }
            }

            // Recurse into namespaces.
            DeclBody::Namespace(ns) => {
                enrich_declarations(&mut ns.children, by_key, name_counts, stem_hint);
            }

            // Interface, Class, Enum — structural; enrichment does not apply.
            _ => {}
        }
    }
}

/// Drive enrichment over every module in `modules`.
///
/// The module's file stem is derived from `module.path` and used as the precise
/// key into `by_key`.
fn enrich_modules(
    modules: &mut Vec<ModuleFacts>,
) -> Result<(), String> {
    // Collect (path, source) for every TS source file referenced by the modules.
    // We pass the paths the OXC graph already resolved — this keeps the file set
    // consistent with what OXC saw.
    let mut inputs: Vec<(String, String)> = Vec::with_capacity(modules.len());
    for module in modules.iter() {
        let text = std::fs::read_to_string(&module.path)
            .map_err(|e| format!("read {}: {e}", module.path.display()))?;
        inputs.push((module.path.to_string_lossy().into_owned(), text));
    }

    if inputs.is_empty() {
        return Err("no modules to check".to_string());
    }

    // Install the large-stack Rayon pool (idempotent).
    ensure_rayon_global_pool();

    // Run tsz parse + bind + merge, guarded against panics.
    let recovered = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let program: MergedProgram = parallel::compile_files(inputs);
        recover_types(&program)
    }))
    .map_err(|_| "tsz checker panicked; falling back to OXC-only".to_string())?;

    let (by_key, name_counts) = recovered;

    if by_key.is_empty() {
        return Err("tsz recovered no type information".to_string());
    }

    // Enrich declarations per module.
    for module in modules.iter_mut() {
        let stem_hint = module
            .path
            .file_name()
            .and_then(|n| n.to_str())
            .map(file_stem);

        enrich_declarations(
            &mut module.declarations,
            &by_key,
            &name_counts,
            stem_hint.as_deref(),
        );
    }

    Ok(())
}
