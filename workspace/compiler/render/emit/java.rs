//! IR → Java surface syntax.
//!
//! Records map to `class`es, sum types to a `sealed interface` plus a `record`
//! per variant (Java 17+), traits to `interface`s. Java has no unsigned
//! integers, references, or tuples, so those collapse to their nearest analogue.

use ir::function::Function;
use ir::generics::{GenericArg, Generics};
use ir::kind::Visibility;
use ir::parameter::Parameter;
use ir::primitives::{Primitive, Width};
use ir::protocols::{TraitDef, TraitMethod};
use ir::record::{Field, FieldKey, Record, SumField, SumVariant};
use ir::ty::{Type, TypeReference};

use super::super::backend::*;
use super::super::doc::Doc;
use super::{analyze_generics, GenericInfo};

pub struct Java;

impl Backend for Java {
    fn doc_comment(&self, text: &str) -> Rendered {
        let body = text.lines().map(|l| txt(" * ").annotate(Annotation::Comment) + txt(l));
        (txt("/**") + Doc::hardline() + Doc::join(Doc::hardline(), body) + Doc::hardline() + txt(" */"))
            .annotate(Annotation::Comment)
    }

    fn ty(&self, t: &Type, cx: &RenderCtx) -> Rendered {
        ty(t, cx)
    }

    fn record(&self, name: &str, vis: &Visibility, rec: &Record, cx: &RenderCtx) -> Rendered {
        let info = analyze_generics(rec.generics.as_ref());
        let header =
            vis_prefix(vis) + kw("class") + sp() + tyname(name) + generics_decl(&info);
        let fields: Vec<Rendered> = rec.fields.iter().filter_map(|f| field(f, cx)).collect();
        header + sp() + block("{", fields, "}")
    }

    fn sum(
        &self,
        name: &str,
        generics: Option<&Generics>,
        variants: &[SumVariant],
        vis: &Visibility,
        cx: &RenderCtx,
    ) -> Rendered {
        let info = super::analyze_sum_generics(generics, variants);
        let decl = generics_decl(&info);
        let permits = Doc::join(punct(", "), variants.iter().map(|v| tyname(&v.name)));
        let header = vis_prefix(vis)
            + kw("sealed interface")
            + sp()
            + tyname(name)
            + decl.clone()
            + sp()
            + kw("permits")
            + sp()
            + permits
            + sp()
            + punct("{}");

        let mut out = header;
        for v in variants {
            let components = record_components(v, cx);
            let rec = kw("record")
                + sp()
                + tyname(&v.name)
                + decl.clone()
                + arglist("(", components, ")")
                + sp()
                + kw("implements")
                + sp()
                + tyname(name)
                + decl.clone()
                + sp()
                + punct("{}");
            out = out + Doc::hardline() + rec;
        }
        out
    }

    fn function(&self, name: &str, f: &Function, vis: &Visibility, cx: &RenderCtx) -> Rendered {
        let info = analyze_generics(f.generics.as_ref());
        let gd = generics_decl(&info);
        let type_params = if info.is_empty() { Doc::nil() } else { gd + sp() };
        let params = value_params(f.input_parameters.as_deref(), cx);
        vis_prefix(vis)
            + type_params
            + return_type(f.output_parameters.as_deref(), cx)
            + sp()
            + ident(name)
            + arglist("(", params, ")")
            + punct(";")
    }

    fn interface(&self, name: &str, def: &TraitDef, vis: &Visibility, cx: &RenderCtx) -> Rendered {
        let info = analyze_generics(def.generics.as_ref());
        let header =
            vis_prefix(vis) + kw("interface") + sp() + tyname(name) + generics_decl(&info);
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

fn vis_prefix(v: &Visibility) -> Rendered {
    match v {
        Visibility::Public => kw("public") + sp(),
        Visibility::Protected => kw("protected") + sp(),
        Visibility::Private => kw("private") + sp(),
        Visibility::Internal | Visibility::Package => Doc::nil(),
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
    let name = super::to_camel_case(&field_name(&kf.key));
    let base = kf.r#type.as_ref().map(|t| ty(t, cx)).unwrap_or_else(|| tyname("Object"));
    let t = if kf.attributes.is_optional {
        tyname("Optional") + punct("<") + base + punct(">")
    } else {
        base
    };
    let vis = kf.visibility.as_ref().map(vis_prefix).unwrap_or_else(|| kw("private") + sp());
    Some(vis + t + sp() + ident(&name) + punct(";"))
}

fn record_components(v: &SumVariant, cx: &RenderCtx) -> Vec<Rendered> {
    match &v.data {
        None => Vec::new(),
        Some(SumField::Tuple(tys)) => tys
            .iter()
            .enumerate()
            .map(|(i, t)| ty(t, cx) + sp() + ident(&format!("field{i}")))
            .collect(),
        Some(SumField::StructLike(fields)) => fields
            .iter()
            .filter_map(|f| match f {
                Field::Known(kf) => {
                    let t = kf.r#type.as_ref().map(|t| ty(t, cx))?;
                    Some(t + sp() + ident(&super::to_camel_case(&field_name(&kf.key))))
                }
                _ => None,
            })
            .collect(),
    }
}

fn value_params(inputs: Option<&[Parameter]>, cx: &RenderCtx) -> Vec<Rendered> {
    inputs
        .into_iter()
        .flatten()
        .filter_map(|p| match p {
            Parameter::Literal(lp) => {
                let t = lp.r#type.as_ref().map(|t| ty(t, cx)).unwrap_or_else(|| tyname("Object"));
                Some(t + sp() + ident(&lp.name))
            }
            _ => None,
        })
        .collect()
}

fn return_type(outputs: Option<&[Parameter]>, cx: &RenderCtx) -> Rendered {
    outputs
        .into_iter()
        .flatten()
        .find_map(|p| match p {
            Parameter::Literal(lp) => lp.r#type.as_ref().map(|t| ty(t, cx)),
            _ => None,
        })
        .unwrap_or_else(|| kw("void"))
}

fn method_sig(m: &TraitMethod, cx: &RenderCtx) -> Rendered {
    let ret = m
        .return_type
        .as_ref()
        .map(|t| ty(t, cx))
        .unwrap_or_else(|| kw("void"));
    let params = value_params(m.parameters.as_deref(), cx);
    let method_name = super::to_camel_case(&m.name);
    ret + sp() + ident(&method_name) + arglist("(", params, ")") + punct(";")
}

fn ty(t: &Type, cx: &RenderCtx) -> Rendered {
    match t {
        Type::TypeReference(r) => type_reference(r, cx),
        Type::Primitive(p) => tyname(primitive(p)),
        Type::GenericParam(g) => tyname(&g.name),
        Type::SelfType => tyname("T"),
        Type::Slice(inner) | Type::Array { r#type: inner, .. } => ty(inner, cx) + punct("[]"),
        Type::BorrowedRef { r#type, .. } | Type::RawPointer { r#type, .. } => ty(r#type, cx),
        Type::Tuple(ts) if ts.is_empty() => kw("void"),
        Type::Tuple(ts) => {
            tyname("Tuple") + generic_list("<", ts.iter().map(|t| ty(t, cx)).collect(), ">")
        }
        Type::Any | Type::Infer => tyname("Object"),
        _ => tyname("Object"),
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
    if let Some(known) = super::known_type(sn, &type_args, Language::Java) {
        return known;
    }
    let base = tyname(sn);
    match &r.generic_args {
        Some(ga) if !ga.is_empty() => {
            let items: Vec<Rendered> = ga
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
        Primitive::Int(w) | Primitive::UInt(w) => match w {
            Width::W8 => "byte",
            Width::W16 => "short",
            Width::W32 => "int",
            Width::W64 | Width::W128 | Width::Arch => "long",
        },
        Primitive::Float(w) => match w {
            Width::W32 => "float",
            _ => "double",
        },
        Primitive::Bool => "boolean",
        Primitive::String => "String",
        Primitive::Char => "char",
        Primitive::Bytes => "byte[]",
        Primitive::Date => "Instant",
        Primitive::Address => "long",
    }
}

fn field_name(key: &FieldKey) -> String {
    match key {
        FieldKey::Ident(n) => n.clone(),
        FieldKey::Index(i) => format!("field{i}"),
        FieldKey::Computed(_) => "field".into(),
    }
}
