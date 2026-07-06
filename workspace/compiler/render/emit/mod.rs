//! Per-language backends: one `impl Backend` module each.
//!
//! Every module here is a thin combinator layer over the shared skeleton in
//! [`crate::render::backend`]. They differ only where the *languages* differ —
//! keywords, primitive spellings, where the field name sits relative to its
//! type, how generics and doc comments are written — and share the layout
//! decisions entirely, because those live in the document algebra.

pub mod go;
pub mod java;
pub mod python;
pub mod rust;
pub mod typescript;

use ir::generics::{Constraint, GenericArg, Generics};
use ir::parameter::Parameter;
use ir::record::{Field, SumField, SumVariant};
use ir::ty::Type;

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
    pub fn is_empty(&self) -> bool {
        self.lifetimes.is_empty() && self.types.is_empty() && self.consts.is_empty()
    }

    /// Every simple bound attached to `param`.
    pub fn bounds_of<'a>(&'a self, param: &'a str) -> impl Iterator<Item = &'a str> {
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
                info.bounds.push((param.clone(), trait_ref.name.clone()));
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
