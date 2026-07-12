//! Per-language backends: one `impl Backend` module each.
//!
//! Every module here is a thin combinator layer over the shared skeleton in
//! [`crate::render::backend`]. They differ only where the *languages* differ —
//! keywords, primitive spellings, where the field name sits relative to its
//! type, how generics and doc comments are written — and share the layout
//! decisions entirely, because those live in the document algebra.

pub mod go;
pub mod java;
pub mod nix;
pub mod python;
pub mod rust;
pub mod typescript;

use ir::generics::{Constraint, GenericArg, Generics};
use ir::parameter::Parameter;
use ir::record::{Field, SumField, SumVariant};
use ir::ty::Type;

use super::backend::{kw, punct, tyname, Language, Rendered};

/// The declaration-side view of a generic parameter list, distilled to the
/// pieces every language can spell. Exotic constraints (associated-type bounds,
/// HRTBs, logical predicates) are intentionally dropped here — a backend that
/// wants them can read `Generics` directly.
#[derive(Default)]
pub(crate) struct GenericInfo {
    /// Lifetime parameter names, without the leading `'`.
    pub lifetimes: Vec<String>,
    /// Type parameter names, in declaration order.
    pub types: Vec<String>,
    /// Const parameter names.
    pub consts: Vec<String>,
    /// Simple, single-trait bounds: `(param, trait_name)` for `T: Trait`.
    pub bounds: Vec<(String, String)>,
}

impl GenericInfo {
    pub(crate) fn is_empty(&self) -> bool {
        self.lifetimes.is_empty() && self.types.is_empty() && self.consts.is_empty()
    }

    /// Every simple bound attached to `param`.
    pub(crate) fn bounds_of<'a>(&'a self, param: &'a str) -> impl Iterator<Item = &'a str> {
        self.bounds.iter().filter(move |(p, _)| p == param).map(|(_, t)| t.as_str())
    }
}

/// Distil a `Generics` into the cross-language [`GenericInfo`].
pub(crate) fn analyze_generics(g: Option<&Generics>) -> GenericInfo {
    let mut info = GenericInfo::default();
    let Some(g) = g else { return info };

    for p in &g.params {
        match p {
            Parameter::Type(tp) => {
                info.types.push(tp.name.clone().unwrap_or_else(|| "_".into()))
            }
            Parameter::Lifetime(lp) => info.lifetimes.push(lp.name.clone()),
            Parameter::Const(cp) => info.consts.push(cp.name.clone()),
            _ => {}
        }
    }
    for c in &g.constraints {
        if let Constraint::TraitBound { param, trait_ref } = c {
            if trait_ref.args.is_empty() {
                // Strip the qualified path to the last segment (e.g. core::clone::Clone → Clone).
                let short = trait_ref
                    .name
                    .rfind("::")
                    .map(|i| &trait_ref.name[i + 2..])
                    .unwrap_or(&trait_ref.name);
                info.bounds.push((param.clone(), short.to_string()));
            }
        }
    }
    info
}

/// Like [`analyze_generics`], but for a sum type — which carries no `Generics`
/// of its own in the IR. Any free type parameters referenced by the variants
/// are recovered so the declaration can bind them (`enum Shape<T>`).
pub(crate) fn analyze_sum_generics(
    generics: Option<&Generics>,
    variants: &[SumVariant],
) -> GenericInfo {
    let mut info = analyze_generics(generics);
    for name in free_type_params(variants) {
        if !info.types.contains(&name) {
            info.types.push(name);
        }
    }
    info
}

/// The distinct type-parameter names referenced anywhere in the variants, in
/// first-seen order.
fn free_type_params(variants: &[SumVariant]) -> Vec<String> {
    let mut out = Vec::new();
    for v in variants {
        match &v.data {
            Some(SumField::Tuple(tys)) => tys.iter().for_each(|t| collect_params(t, &mut out)),
            Some(SumField::StructLike(fields)) => {
                for f in fields {
                    if let Field::Known(kf) = f {
                        if let Some(t) = &kf.r#type {
                            collect_params(t, &mut out);
                        }
                    }
                }
            }
            None => {}
        }
    }
    out
}

/// Walk a type, pushing each `GenericParam` name into `out` once.
fn collect_params(t: &Type, out: &mut Vec<String>) {
    match t {
        Type::GenericParam(g) => {
            if !out.contains(&g.name) {
                out.push(g.name.clone());
            }
        }
        Type::TypeReference(r) => {
            for a in r.generic_args.iter().flatten() {
                if let GenericArg::Type(t) = a {
                    collect_params(t, out);
                }
            }
        }
        Type::Tuple(ts) | Type::Union(ts) | Type::Intersection(ts) => {
            ts.iter().for_each(|t| collect_params(t, out))
        }
        Type::Slice(b) | Type::Variadic(b) => collect_params(b, out),
        Type::Array { r#type, .. }
        | Type::BorrowedRef { r#type, .. }
        | Type::RawPointer { r#type, .. } => collect_params(r#type, out),
        Type::RecordLiteral(rec) => {
            for f in &rec.fields {
                if let Field::Known(kf) = f {
                    if let Some(t) = &kf.r#type {
                        collect_params(t, out);
                    }
                }
            }
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Casing utilities
// ---------------------------------------------------------------------------

/// Convert a name to snake_case (e.g. `helloWorld` → `hello_world`, `hello_world` → `hello_world`).
pub(crate) fn to_snake_case(s: &str) -> String {
    let mut result = String::new();
    for (i, c) in s.char_indices() {
        if c == '_' {
            result.push('_');
        } else if c.is_uppercase() {
            if i > 0 && !result.ends_with('_') {
                result.push('_');
            }
            result.extend(c.to_lowercase());
        } else {
            result.push(c);
        }
    }
    result
}

/// Convert a name to camelCase (e.g. `hello_world` → `helloWorld`).
pub(crate) fn to_camel_case(s: &str) -> String {
    let mut result = String::new();
    let mut capitalize_next = false;
    let mut first_char = true;
    for c in s.chars() {
        if c == '_' {
            capitalize_next = true;
        } else {
            if capitalize_next {
                result.extend(c.to_uppercase());
                capitalize_next = false;
            } else if first_char {
                result.extend(c.to_lowercase());
            } else {
                result.push(c);
            }
            first_char = false;
        }
    }
    result
}

/// Convert a name to PascalCase (e.g. `hello_world` → `HelloWorld`).
pub(crate) fn to_pascal_case(s: &str) -> String {
    let mut result = String::new();
    let mut capitalize_next = true;
    for c in s.chars() {
        if c == '_' {
            capitalize_next = true;
        } else if capitalize_next {
            result.extend(c.to_uppercase());
            capitalize_next = false;
        } else {
            result.push(c);
        }
    }
    result
}

// ---------------------------------------------------------------------------
// Known-type mapping
// ---------------------------------------------------------------------------

/// Returns `Some(rendered)` if `short_name` + `args` maps to a language-native type.
///
/// `short_name` is the final path segment (case-insensitive match); `args` are
/// already-rendered type arguments. Returns `None` for unknown types or when the
/// default rendering is already correct (e.g. Rust `Vec<T>`).
pub(crate) fn known_type(short_name: &str, args: &[Rendered], lang: Language) -> Option<Rendered> {
    let name_lc = short_name.to_ascii_lowercase();
    let arity = args.len();

    match (name_lc.as_str(), arity, lang) {
        // ── Vec / list ──────────────────────────────────────────────────────
        ("vec", 1, Language::Go) => Some(punct("[]") + args[0].clone()),
        ("vec", 1, Language::Java) => {
            Some(tyname("List") + punct("<") + args[0].clone() + punct(">"))
        }
        ("vec", 1, Language::TypeScript) => Some(args[0].clone() + punct("[]")),
        ("vec", 1, Language::Python) => {
            Some(tyname("list") + punct("[") + args[0].clone() + punct("]"))
        }

        // ── Option ──────────────────────────────────────────────────────────
        ("option", 1, Language::Go) => Some(punct("*") + args[0].clone()),
        ("option", 1, Language::Java) => {
            Some(tyname("Optional") + punct("<") + args[0].clone() + punct(">"))
        }
        ("option", 1, Language::TypeScript) => {
            Some(args[0].clone() + punct(" | ") + tyname("null"))
        }
        ("option", 1, Language::Python) => {
            Some(args[0].clone() + punct(" | ") + kw("None"))
        }

        // ── HashMap / BTreeMap ───────────────────────────────────────────────
        ("hashmap" | "btreemap", 2, Language::Go) => Some(
            kw("map") + punct("[") + args[0].clone() + punct("]") + args[1].clone(),
        ),
        ("hashmap" | "btreemap", 2, Language::Java) => Some(
            tyname("Map")
                + punct("<")
                + args[0].clone()
                + punct(", ")
                + args[1].clone()
                + punct(">"),
        ),
        ("hashmap" | "btreemap", 2, Language::TypeScript) => Some(
            tyname("Record")
                + punct("<")
                + args[0].clone()
                + punct(", ")
                + args[1].clone()
                + punct(">"),
        ),
        ("hashmap" | "btreemap", 2, Language::Python) => Some(
            tyname("dict")
                + punct("[")
                + args[0].clone()
                + punct(", ")
                + args[1].clone()
                + punct("]"),
        ),

        // ── HashSet / BTreeSet ───────────────────────────────────────────────
        ("hashset" | "btreeset", 1, Language::Go) => Some(
            kw("map") + punct("[") + args[0].clone() + punct("]") + kw("struct") + punct("{}"),
        ),
        ("hashset" | "btreeset", 1, Language::Java) => {
            Some(tyname("Set") + punct("<") + args[0].clone() + punct(">"))
        }
        ("hashset" | "btreeset", 1, Language::TypeScript) => {
            Some(tyname("Set") + punct("<") + args[0].clone() + punct(">"))
        }
        ("hashset" | "btreeset", 1, Language::Python) => {
            Some(tyname("set") + punct("[") + args[0].clone() + punct("]"))
        }

        // ── Box / Rc / Arc ───────────────────────────────────────────────────
        // Rust keeps the wrapper as-is (None → normal path).
        ("box" | "rc" | "arc", 1, Language::Go) => Some(punct("*") + args[0].clone()),
        ("box" | "rc" | "arc", 1, Language::Java) => Some(args[0].clone()),
        ("box" | "rc" | "arc", 1, Language::TypeScript) => Some(args[0].clone()),
        ("box" | "rc" | "arc", 1, Language::Python) => Some(args[0].clone()),

        // ── Result ───────────────────────────────────────────────────────────
        ("result", 2, Language::Go) => Some(
            punct("(") + args[0].clone() + punct(", ") + tyname("error") + punct(")"),
        ),
        ("result", 2, Language::Java) => Some(args[0].clone()),
        ("result", 2, Language::TypeScript) => Some(args[0].clone()),
        ("result", 2, Language::Python) => Some(args[0].clone()),

        // ── String ───────────────────────────────────────────────────────────
        ("string", 0, Language::Go) => Some(kw("string")),
        ("string", 0, Language::TypeScript) => Some(kw("string")),
        ("string", 0, Language::Python) => Some(kw("str")),

        _ => None,
    }
}
