//! IR → TypeScript surface syntax.
//!
//! Records map to `interface`s, sum types to discriminated unions keyed on a
//! `tag` field, traits to `interface`s. TypeScript is the natural home of the
//! IR's structural type constructs.

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

pub struct TypeScript;

impl Backend for TypeScript {
    fn doc_comment(&self, text: &str) -> Rendered {
        let body = text
            .lines()
            .map(|l| txt(" * ").annotate(Annotation::Comment) + txt(l));
        (txt("/**")
            + Doc::hardline()
            + Doc::join(Doc::hardline(), body)
            + Doc::hardline()
            + txt(" */"))
        .annotate(Annotation::Comment)
    }

    fn ty(&self, t: &Type, cx: &RenderCtx) -> Rendered {
        ty(t, cx)
    }

    fn record(&self, name: &str, _vis: &Visibility, rec: &Record, cx: &RenderCtx) -> Rendered {
        let info = analyze_generics(rec.generics.as_ref());
        let header = kw("export interface") + sp() + tyname(name) + generics_decl(&info);
        let fields: Vec<Rendered> = rec.fields.iter().filter_map(|f| field(f, cx)).collect();
        header + sp() + block("{", fields, "}")
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
        let header =
            kw("export type") + sp() + tyname(name) + generics_decl(&info) + sp() + punct("=");
        // Each variant is an object type with a discriminant `tag`.
        let arms: Vec<Rendered> = variants
            .iter()
            .map(|v| punct("| ") + variant_object(v, cx))
            .collect();
        let body = Doc::join(Doc::hardline(), arms);
        header + (Doc::hardline() + body).nest(INDENT) + punct(";")
    }

    fn function(&self, name: &str, f: &Function, _vis: &Visibility, cx: &RenderCtx) -> Rendered {
        let info = analyze_generics(f.generics.as_ref());
        let params = value_params(f.input_parameters.as_deref(), cx);
        kw("export function")
            + sp()
            + ident(name)
            + generics_decl(&info)
            + arglist("(", params, ")")
            + punct(": ")
            + return_type(f.output_parameters.as_deref(), cx)
            + punct(";")
    }

    fn interface(&self, name: &str, def: &TraitDef, _vis: &Visibility, cx: &RenderCtx) -> Rendered {
        let info = analyze_generics(def.generics.as_ref());
        let header = kw("export interface") + sp() + tyname(name) + generics_decl(&info);
        let methods: Vec<Rendered> = def
            .required_methods
            .iter()
            .flatten()
            .chain(def.provided_methods.iter().flatten())
            .map(|m| method_sig(m, cx))
            .collect();
        header + sp() + block("{", methods, "}")
    }
}

fn generics_decl(info: &GenericInfo) -> Rendered {
    if info.types.is_empty() {
        return Doc::nil();
    }
    let params = info.types.iter().map(|name| {
        let bounds: Vec<&str> = info.bounds_of(name).collect();
        if bounds.is_empty() {
            tyname(name)
        } else {
            tyname(name)
                + sp()
                + kw("extends")
                + sp()
                + Doc::join(punct(" & "), bounds.iter().map(|b| tyname(b)))
        }
    });
    generic_list("<", params.collect(), ">")
}

fn field(f: &Field, cx: &RenderCtx) -> Option<Rendered> {
    let Field::Known(kf) = f else { return None };
    let name = field_name(&kf.key);
    let opt = if kf.attributes.is_optional {
        punct("?")
    } else {
        Doc::nil()
    };
    let readonly = if !kf.attributes.is_mutable {
        kw("readonly") + sp()
    } else {
        Doc::nil()
    };
    let t = kf
        .r#type
        .as_ref()
        .map(|t| ty(t, cx))
        .unwrap_or_else(|| tyname("unknown"));
    Some(readonly + ident(&name) + opt + punct(": ") + t + punct(";"))
}

fn variant_object(v: &SumVariant, cx: &RenderCtx) -> Rendered {
    let tag =
        ident("tag") + punct(": ") + txt(&format!("\"{}\"", v.name)).annotate(Annotation::Type);
    let mut members = vec![tag + punct(";")];
    match &v.data {
        None => {}
        Some(SumField::Tuple(tys)) => {
            for (i, t) in tys.iter().enumerate() {
                members.push(ident(&format!("field{i}")) + punct(": ") + ty(t, cx) + punct(";"));
            }
        }
        Some(SumField::StructLike(fields)) => {
            for f in fields {
                if let Some(d) = field(f, cx) {
                    members.push(d);
                }
            }
        }
    }
    block("{", members, "}")
}

fn value_params(inputs: Option<&[Parameter]>, cx: &RenderCtx) -> Vec<Rendered> {
    inputs
        .into_iter()
        .flatten()
        .filter_map(|p| match p {
            Parameter::Literal(lp) => {
                let t = lp
                    .r#type
                    .as_ref()
                    .map(|t| ty(t, cx))
                    .unwrap_or_else(|| tyname("unknown"));
                Some(ident(&lp.name) + punct(": ") + t)
            }
            _ => None,
        })
        .collect()
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
        0 => kw("void"),
        1 => tys.into_iter().next().unwrap(),
        _ => generic_list("[", tys, "]"),
    }
}

fn method_sig(m: &TraitMethod, cx: &RenderCtx) -> Rendered {
    let info = analyze_generics(m.generics.as_ref());
    let params = value_params(m.parameters.as_deref(), cx);
    let ret = m
        .return_type
        .as_ref()
        .map(|t| ty(t, cx))
        .unwrap_or_else(|| kw("void"));
    let method_name = super::to_camel_case(&m.name);
    ident(&method_name)
        + generics_decl(&info)
        + arglist("(", params, ")")
        + punct(": ")
        + ret
        + punct(";")
}

fn ty(t: &Type, cx: &RenderCtx) -> Rendered {
    match t {
        Type::TypeReference(r) => type_reference(r, cx),
        Type::Primitive(p) => tyname(primitive(p)),
        Type::GenericParam(g) => tyname(&g.name),
        Type::SelfType => kw("this"),
        Type::Slice(inner) | Type::Array { r#type: inner, .. } => ty(inner, cx) + punct("[]"),
        Type::BorrowedRef { r#type, .. } | Type::RawPointer { r#type, .. } => ty(r#type, cx),
        Type::Tuple(ts) => generic_list("[", ts.iter().map(|t| ty(t, cx)).collect(), "]"),
        Type::Union(ts) => Doc::join(punct(" | "), ts.iter().map(|t| ty(t, cx))),
        Type::Intersection(ts) => Doc::join(punct(" & "), ts.iter().map(|t| ty(t, cx))),
        Type::Any => kw("any"),
        Type::Infer => kw("unknown"),
        Type::Never => kw("never"),
        Type::Variadic(inner) => punct("...") + ty(inner, cx) + punct("[]"),
        _ => kw("unknown"),
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
    if let Some(known) = super::known_type(sn, &type_args, Language::TypeScript) {
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
            base + generic_list("<", items, ">")
        }
        _ => base,
    }
}

fn primitive(p: &Primitive) -> &'static str {
    match p {
        Primitive::Int(_) | Primitive::UInt(_) | Primitive::Float(_) | Primitive::Address => {
            "number"
        }
        Primitive::Bool => "boolean",
        Primitive::String | Primitive::Char => "string",
        Primitive::Bytes => "Uint8Array",
        Primitive::Date => "Date",
    }
}

fn field_name(key: &FieldKey) -> String {
    match key {
        FieldKey::Ident(n) => n.clone(),
        FieldKey::Index(i) => format!("field{i}"),
        FieldKey::Computed(_) => "field".into(),
    }
}
