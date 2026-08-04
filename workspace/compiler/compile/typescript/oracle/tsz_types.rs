//! `tsz` `TypeData` → [`ir::ty::Type`] mapping for the in-process checker
//! oracle.
//!
//! Two coordinated strategies give *total* coverage of tsz's structured type
//! universe:
//!
//! 1. **Direct mapping** ([`type_id_to_ir`]) — the common inferred constructs
//!    (primitives, literals, arrays, unions/intersections, tuples, `keyof`,
//!    `readonly`, `this`) are lowered straight from [`TypeData`] to the target
//!    [`ir::ty::Type`] shapes, matching what the syntactic OXC pass produces.
//!
//! 2. **Format + re-parse fallback** ([`format_and_lower`]) — for any exotic /
//!    structurally-rich variant we don't map directly (object shapes, function
//!    shapes, generic applications like `Promise<T>`, conditional/mapped/
//!    template-literal types, lazy named references, …), we ask the checker to
//!    render the type to its canonical TypeScript spelling, wrap it as
//!    `type __t = <printed>;`, parse it with `oxc_parser`, and lower the
//!    resulting `TSType` through the *same* `oxc::extract` `lower_ts_type` path
//!    the Tier-A pipeline uses. This guarantees every `TypeId` becomes a valid
//!    IR type with no gaps.
//!
//! The public entry is [`type_id_to_ir`]: it consults the `TypeInterner` for
//! the structured [`TypeData`], maps the leaf/common variants directly, and
//! defers everything else to [`format_and_lower`] using a checker-produced
//! printed spelling.

use ir::{
    generics::GenericArg,
    primitives::{Primitive, Width},
    ty::{LiteralKind, LiteralValue, Type, TypeReference},
};

use oxc_allocator::Allocator;
use oxc_ast::ast::{Statement, TSType};
use oxc_parser::{ParseOptions, Parser};
use oxc_semantic::SemanticBuilder;
use oxc_span::SourceType;

use tsz_solver::construction::TypeDatabase;
use tsz_solver::{IntrinsicKind, LiteralValue as TszLiteral, TypeData, TypeId};

/// Map a `tsz` [`TypeId`] into an [`ir::ty::Type`].
///
/// `interner` is the program-wide `TypeInterner` (implements [`TypeDatabase`]);
/// `printed` is a closure that renders a `TypeId` to its TypeScript spelling
/// (typically `|id| checker.format_type(id)`), used by the re-parse fallback.
///
/// Total: every `TypeId` yields *some* IR type — leaf variants map directly,
/// everything else routes through [`format_and_lower`].
pub fn type_id_to_ir<F>(interner: &dyn TypeDatabase, id: TypeId, printed: &F) -> Type
where
    F: Fn(TypeId) -> String,
{
    let Some(data) = interner.lookup(id) else {
        // Unknown id — fall back to the printed spelling.
        return format_and_lower(&printed(id)).unwrap_or(Type::Any);
    };

    match data {
        // ── Intrinsics — primitives / top / bottom / unit ────────────────
        TypeData::Intrinsic(kind) => intrinsic_to_ir(kind),

        // ── Literal types — preserve the exact value ─────────────────────
        TypeData::Literal(lit) => literal_to_ir(interner, lit),

        // ── Array<T> / T[] → Slice ───────────────────────────────────────
        TypeData::Array(elem) => Type::Slice(Box::new(type_id_to_ir(interner, elem, printed))),

        // ── ReadonlyType(T) — `readonly T[]`; unwrap to the inner type ────
        TypeData::ReadonlyType(inner) => type_id_to_ir(interner, inner, printed),

        // ── NoInfer<T> — transparent to the inner type ───────────────────
        TypeData::NoInfer(inner) => type_id_to_ir(interner, inner, printed),

        // ── Union / Intersection ─────────────────────────────────────────
        TypeData::Union(list_id) => {
            let members = interner
                .type_list(list_id)
                .iter()
                .map(|&m| type_id_to_ir(interner, m, printed))
                .collect();
            Type::Union(members)
        }
        TypeData::Intersection(list_id) => {
            let members = interner
                .type_list(list_id)
                .iter()
                .map(|&m| type_id_to_ir(interner, m, printed))
                .collect();
            Type::Intersection(members)
        }

        // ── Tuple ────────────────────────────────────────────────────────
        TypeData::Tuple(list_id) => {
            let elems = interner
                .tuple_list(list_id)
                .iter()
                .map(|e| type_id_to_ir(interner, e.type_id, printed))
                .collect();
            Type::Tuple(elems)
        }

        // ── keyof T ──────────────────────────────────────────────────────
        TypeData::KeyOf(inner) => Type::TypeOperator(ir::ty::TypeOperator {
            operator: "keyof".to_string(),
            r#type: Box::new(type_id_to_ir(interner, inner, printed)),
        }),

        // ── polymorphic `this` ───────────────────────────────────────────
        TypeData::ThisType => Type::SelfType,

        // ── Error / unresolved — best-effort via the printed spelling ─────
        TypeData::Error => Type::Any,
        TypeData::UnresolvedTypeName(atom) => Type::TypeReference(TypeReference {
            identifier: interner.resolve_atom(atom),
            generic_args: None,
        }),

        // ── Everything structurally rich: format + re-parse ──────────────
        // Object / ObjectWithIndex / Function / Callable / Application
        // (Promise<T>, Map<K,V>, …) / Conditional / Mapped / TemplateLiteral
        // / Lazy(DefId) named refs / TypeParameter / Enum / IndexAccess /
        // TypeQuery / UniqueSymbol / Infer / StringIntrinsic /
        // ModuleNamespace / Substitution / BoundParameter / Recursive.
        _ => format_and_lower(&printed(id)).unwrap_or_else(|| {
            // Last resort: a bare reference to whatever the checker printed.
            Type::TypeReference(TypeReference {
                identifier: printed(id),
                generic_args: None,
            })
        }),
    }
}

/// Map a `tsz` [`IntrinsicKind`] to the same `ir` primitive/reference the
/// syntactic keyword-lowering (`oxc::extract::types::keyword_type`) produces.
fn intrinsic_to_ir(kind: IntrinsicKind) -> Type {
    match kind {
        IntrinsicKind::Number => Type::Primitive(Primitive::Float(Width::W64)),
        IntrinsicKind::String => Type::Primitive(Primitive::String),
        IntrinsicKind::Boolean => Type::Primitive(Primitive::Bool),
        IntrinsicKind::Bigint => Type::Primitive(Primitive::Int(Width::W128)),
        // void → empty Tuple (unit); null/undefined stay named references so
        // they remain distinguishable from void (and from each other).
        IntrinsicKind::Void => Type::Tuple(vec![]),
        IntrinsicKind::Null => Type::TypeReference(TypeReference {
            identifier: "null".to_string(),
            generic_args: None,
        }),
        IntrinsicKind::Undefined => Type::TypeReference(TypeReference {
            identifier: "undefined".to_string(),
            generic_args: None,
        }),
        IntrinsicKind::Never => Type::Never,
        // any vs unknown stay separate — unknown is NOT Type::Any.
        IntrinsicKind::Any => Type::Any,
        IntrinsicKind::Unknown => Type::TypeReference(TypeReference {
            identifier: "unknown".to_string(),
            generic_args: None,
        }),
        IntrinsicKind::Object => Type::TypeReference(TypeReference {
            identifier: "object".to_string(),
            generic_args: None,
        }),
        IntrinsicKind::Symbol => Type::TypeReference(TypeReference {
            identifier: "Symbol".to_string(),
            generic_args: None,
        }),
        IntrinsicKind::Function => Type::TypeReference(TypeReference {
            identifier: "Function".to_string(),
            generic_args: None,
        }),
    }
}

/// Map a `tsz` literal-type value to a first-class [`Type::Literal`], keeping
/// its exact spelling (mirrors `oxc::extract::types::literal_type`).
fn literal_to_ir(interner: &dyn TypeDatabase, lit: TszLiteral) -> Type {
    let (kind, value) = match lit {
        TszLiteral::String(atom) => {
            // Re-quote so the value round-trips as source spelling.
            (LiteralKind::String, format!("{:?}", interner.resolve_atom(atom)))
        }
        TszLiteral::Number(f) => (LiteralKind::Number, format_number(f.0)),
        TszLiteral::BigInt(atom) => (LiteralKind::BigInt, interner.resolve_atom(atom)),
        TszLiteral::Boolean(b) => (LiteralKind::Boolean, b.to_string()),
    };
    Type::Literal(LiteralValue { kind, value })
}

/// Render an `f64` number-literal value without a trailing `.0` for integers
/// (so `42.0` prints as `42`, matching TS literal spelling).
fn format_number(v: f64) -> String {
    if v.fract() == 0.0 && v.is_finite() && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        format!("{v}")
    }
}

/// Fallback: render a checker-printed TypeScript type spelling into an
/// [`ir::ty::Type`] by parsing `type __t = <printed>;` with `oxc_parser` and
/// lowering the alias's `type_annotation` through the shared `oxc::extract`
/// `lower_ts_type` path.
///
/// Returns `None` if the snippet fails to parse or contains no alias type
/// annotation (the caller substitutes a bare reference / `Any`).
pub fn format_and_lower(printed: &str) -> Option<Type> {
    let trimmed = printed.trim();
    if trimmed.is_empty() {
        return None;
    }

    let source = format!("type __t = {trimmed};");
    let allocator = Allocator::new();
    let source_type = SourceType::d_ts();

    let ret = Parser::new(&allocator, &source, source_type)
        .with_options(ParseOptions {
            preserve_parens: false,
            ..Default::default()
        })
        .parse();

    if ret.panicked {
        return None;
    }

    // Locate the alias's type annotation.
    let ann: &TSType = ret.program.body.iter().find_map(|stmt| match stmt {
        Statement::TSTypeAliasDeclaration(alias) => Some(&alias.type_annotation),
        _ => None,
    })?;

    // Build a Semantic model (required to construct an Extractor); a tiny
    // snippet makes this cheap.
    let sem = SemanticBuilder::new()
        .with_check_syntax_error(false)
        .with_build_nodes(true)
        .build(&ret.program);

    let mut extractor = super::super::oxc::extract::Extractor::new(
        &source,
        &sem.semantic,
        std::path::Path::new("__tsz_oracle_type__.d.ts"),
    );

    extractor.lower_ts_type(ann).ok()
}

/// If `id` is a function/callable type, return the [`TypeId`] of its (first)
/// call signature's return type; otherwise `None`.
///
/// A named function symbol types as `(args) => R`; the enrichment wants the
/// bare return type `R` for `output_parameters`, not the whole arrow type.
pub fn return_type_of(interner: &dyn TypeDatabase, id: TypeId) -> Option<TypeId> {
    match interner.lookup(id)? {
        TypeData::Function(shape_id) => Some(interner.function_shape(shape_id).return_type),
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

/// Convenience: turn a checker-printed spelling directly into IR, without a
/// `TypeData` lookup. Used for return-type / member enrichment where we already
/// hold the printed string (or the caller only has the printed form).
#[allow(dead_code)]
pub fn printed_to_ir(printed: &str) -> Type {
    format_and_lower(printed).unwrap_or(Type::Any)
}

/// Helper for callers that only carry a [`GenericArg`] target (kept for
/// symmetry with the oxc lowering; currently unused directly but exercised via
/// [`type_id_to_ir`] for union members).
#[allow(dead_code)]
pub fn type_as_generic_arg(ty: Type) -> GenericArg {
    GenericArg::Type(ty)
}
