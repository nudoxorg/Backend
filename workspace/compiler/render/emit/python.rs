//! IR → Python surface syntax (3.12+, PEP 695 type parameters).
//!
//! Records become `@dataclass` classes, sum types a union of per-variant
//! dataclasses, traits a `Protocol`. Python's blocks are indentation-based, so
//! this backend builds its own colon+indent bodies rather than the brace
//! [`block`](super::super::backend::block) combinator.

use ir::function::Function;
use ir::generics::{GenericArg, Generics};
use ir::kind::Visibility;
use ir::parameter::Parameter;
use ir::primitives::Primitive;
use ir::protocols::{TraitDef, TraitMethod};
use ir::record::{Field, FieldKey, Record, SumField, SumVariant};
use ir::ty::{Type, TypeReference};

use super::super::backend::*;
use super::super::doc::Doc;
use super::{analyze_generics, GenericInfo};

pub struct Python;

impl Backend for Python {
    fn doc_comment(&self, text: &str) -> Rendered {
        let inner = Doc::join(Doc::hardline(), text.lines().map(|l| txt(l)));
        txt("\"\"\"") + Doc::hardline() + inner + Doc::hardline() + txt("\"\"\"")
    }

    fn ty(&self, t: &Type, cx: &RenderCtx) -> Rendered {
        ty(t, cx)
    }

    fn record(&self, name: &str, _vis: &Visibility, rec: &Record, cx: &RenderCtx) -> Rendered {
        let info = analyze_generics(rec.generics.as_ref());
        let header = kw("class") + sp() + tyname(name) + type_params(&info);
        let mut field_pairs: Vec<(bool, Rendered)> = rec.fields.iter().filter_map(|f| field(f, cx)).collect();
        // Non-optional fields must precede optional fields in a @dataclass.
        field_pairs.sort_by_key(|(is_optional, _)| *is_optional);
        let fields: Vec<Rendered> = field_pairs.into_iter().map(|(_, r)| r).collect();
        decorator("@dataclass") + suite(header, fields)
    }

    fn sum(
        &self,
        name: &str,
        generics: Option<&Generics>,
        variants: &[SumVariant],
        _vis: &Visibility,
        cx: &RenderCtx,
    ) -> Rendered {
        let info = super::analyze_sum_generics(generics, variants);
        let tp = type_params(&info);
        let mut out = Doc::nil();
        for (i, v) in variants.iter().enumerate() {
            if i > 0 {
                out = out + Doc::hardline() + Doc::hardline();
            }
            let header = kw("class") + sp() + tyname(&v.name) + tp.clone();
            out = out + decorator("@dataclass") + suite(header, variant_fields(v, cx));
        }
        // The union alias tying the variants together.
        let alias_rhs = Doc::join(punct(" | "), variants.iter().map(|v| tyname(&v.name)));
        out + Doc::hardline() + Doc::hardline() + tyname(name) + tp + sp() + punct("=") + sp() + alias_rhs
    }

    fn function(&self, name: &str, f: &Function, _vis: &Visibility, cx: &RenderCtx) -> Rendered {
        let info = analyze_generics(f.generics.as_ref());
        let params = value_params(f.receiver.is_some(), f.input_parameters.as_deref(), cx);
        let header = kw("def")
            + sp()
            + ident(name)
            + type_params(&info)
            + arglist("(", params, ")")
            + punct(" -> ")
            + return_type(f.output_parameters.as_deref(), cx);
        header + punct(":") + sp() + punct("...")
    }

    fn interface(&self, name: &str, def: &TraitDef, _vis: &Visibility, cx: &RenderCtx) -> Rendered {
        let info = analyze_generics(def.generics.as_ref());
        let header = kw("class") + sp() + tyname(name) + type_params(&info)
            + arglist("(", vec![tyname("Protocol")], ")");
        let methods: Vec<Rendered> = def
            .required_methods
            .iter()
            .flatten()
            .chain(def.provided_methods.iter().flatten())
            .map(|m| method_sig(m, cx))
            .collect();
        suite(header, methods)
    }
}

/// A decorator line above a declaration (`@dataclass`, …).
fn decorator(name: &str) -> Rendered {
    punct("@") + ident(&name[1..]) + Doc::hardline()
}

/// A `header:` line followed by an indented suite of `body` items, or `...`
/// when the body is empty.
fn suite(header: Rendered, body: Vec<Rendered>) -> Rendered {
    if body.is_empty() {
        return header + punct(":") + sp() + punct("...");
    }
    let inner = Doc::join(Doc::hardline(), body);
    header + punct(":") + (Doc::hardline() + inner).nest(INDENT)
}

/// PEP 695 type parameters: `[T, U: Bound]`, empty when there are none.
fn type_params(info: &GenericInfo) -> Rendered {
    if info.types.is_empty() {
        return Doc::nil();
    }
    let params = info.types.iter().map(|name| {
        let bounds: Vec<&str> = info.bounds_of(name).collect();
        match bounds.first() {
            Some(b) => tyname(name) + punct(": ") + tyname(b),
            None => tyname(name),
        }
    });
    generic_list("[", params.collect(), "]")
}

fn field(f: &Field, cx: &RenderCtx) -> Option<(bool, Rendered)> {
    let Field::Known(kf) = f else { return None };
    let name = super::to_snake_case(&field_name(&kf.key));
    let base = kf.r#type.as_ref().map(|t| ty(t, cx)).unwrap_or_else(|| tyname("Any"));
    let is_optional = kf.attributes.is_optional;
    let (t, default) = if is_optional {
        (tyname("Optional") + punct("[") + base + punct("]"), sp() + punct("=") + sp() + kw("None"))
    } else {
        (base, Doc::nil())
    };
    Some((is_optional, ident(&name) + punct(": ") + t + default))
}

fn variant_fields(v: &SumVariant, cx: &RenderCtx) -> Vec<Rendered> {
    match &v.data {
        None => Vec::new(),
        Some(SumField::Tuple(tys)) => tys
            .iter()
            .enumerate()
            .map(|(i, t)| ident(&format!("field{i}")) + punct(": ") + ty(t, cx))
            .collect(),
        Some(SumField::StructLike(fields)) => {
            let mut pairs: Vec<(bool, Rendered)> =
                fields.iter().filter_map(|f| field(f, cx)).collect();
            pairs.sort_by_key(|(is_optional, _)| *is_optional);
            pairs.into_iter().map(|(_, r)| r).collect()
        }
    }
}

fn value_params(has_receiver: bool, inputs: Option<&[Parameter]>, cx: &RenderCtx) -> Vec<Rendered> {
    let mut params: Vec<Rendered> = Vec::new();
    if has_receiver {
        params.push(kw("self"));
    }
    for p in inputs.into_iter().flatten() {
        if let Parameter::Literal(lp) = p {
            let t = lp.r#type.as_ref().map(|t| ty(t, cx)).unwrap_or_else(|| tyname("Any"));
            params.push(ident(&lp.name) + punct(": ") + t);
        }
    }
    params
}

fn return_type(outputs: Option<&[Parameter]>, cx: &RenderCtx) -> Rendered {
    let tys: Vec<Rendered> = outputs
        .into_iter()
        .flatten()
        .filter_map(|p| match p {
            Parameter::Literal(lp) => lp.r#type.as_ref().map(|t| ty(t, cx)),
            _ => None,
        })
        .collect();
    match tys.len() {
        0 => kw("None"),
        1 => tys.into_iter().next().unwrap(),
        _ => tyname("tuple") + generic_list("[", tys, "]"),
    }
}

fn method_sig(m: &TraitMethod, cx: &RenderCtx) -> Rendered {
    let params = value_params(true, m.parameters.as_deref(), cx);
    let ret = m.return_type.as_ref().map(|t| ty(t, cx)).unwrap_or_else(|| kw("None"));
    kw("def") + sp() + ident(&m.name) + arglist("(", params, ")") + punct(" -> ") + ret
        + punct(":")
        + sp()
        + punct("...")
}

fn ty(t: &Type, cx: &RenderCtx) -> Rendered {
    match t {
        Type::TypeReference(r) => type_reference(r, cx),
        Type::Primitive(p) => tyname(primitive(p)),
        Type::GenericParam(g) => tyname(&g.name),
        Type::SelfType => tyname("Self"),
        Type::Slice(inner) | Type::Array { r#type: inner, .. } => {
            tyname("list") + punct("[") + ty(inner, cx) + punct("]")
        }
        Type::BorrowedRef { r#type, .. } | Type::RawPointer { r#type, .. } => ty(r#type, cx),
        Type::Tuple(ts) if ts.is_empty() => kw("None"),
        Type::Tuple(ts) => tyname("tuple") + generic_list("[", ts.iter().map(|t| ty(t, cx)).collect(), "]"),
        Type::Union(ts) => Doc::join(punct(" | "), ts.iter().map(|t| ty(t, cx))),
        Type::Any | Type::Infer => tyname("Any"),
        Type::Never => tyname("Never"),
        Type::Variadic(inner) => punct("*") + ty(inner, cx),
        _ => tyname("Any"),
    }
}

fn type_reference(r: &TypeReference, cx: &RenderCtx) -> Rendered {
    let sn = short_name(&r.identifier, cx);
    let type_args: Vec<Rendered> = r
        .generic_args
        .as_deref()
        .unwrap_or(&[])
        .iter()
        .filter_map(|a| match a {
            GenericArg::Type(t) => Some(ty(t, cx)),
            _ => None,
        })
        .collect();
    if let Some(known) = super::known_type(sn, &type_args, Language::Python) {
        return known;
    }
    let base = tyname(sn);
    match &r.generic_args {
        Some(args) if !args.is_empty() => {
            let items: Vec<Rendered> = args
                .iter()
                .filter_map(|a| match a {
                    GenericArg::Type(t) => Some(ty(t, cx)),
                    _ => None,
                })
                .collect();
            base + generic_list("[", items, "]")
        }
        _ => base,
    }
}

fn primitive(p: &Primitive) -> &'static str {
    match p {
        Primitive::Int(_) | Primitive::UInt(_) | Primitive::Address => "int",
        Primitive::Float(_) => "float",
        Primitive::Bool => "bool",
        Primitive::String | Primitive::Char => "str",
        Primitive::Bytes => "bytes",
        Primitive::Date => "datetime",
    }
}

fn field_name(key: &FieldKey) -> String {
    match key {
        FieldKey::Ident(n) => n.clone(),
        FieldKey::Index(i) => format!("field{i}"),
        FieldKey::Computed(_) => "field".into(),
    }
}
