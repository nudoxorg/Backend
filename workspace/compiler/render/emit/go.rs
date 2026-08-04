//! IR → Go surface syntax.
//!
//! Go lacks sum types and reference types, so those IR constructs are lowered
//! to their idiomatic Go encodings: a sum becomes a sealed interface plus one
//! struct per variant; a borrowed reference becomes a pointer.

use ir::function::Function;
use ir::generics::{GenericArg, Generics};
use ir::kind::Visibility;
use ir::parameter::Parameter;
use ir::primitives::{Primitive, Width};
use ir::protocols::{TraitDef, TraitMethod};
use ir::record::{Field, FieldKey, Record, SumField, SumVariant};
use ir::ty::{Type, TypeOperator, TypeReference};

use super::super::backend::*;
use super::super::doc::Doc;
use super::{analyze_generics, GenericInfo};

pub struct Go;

impl Backend for Go {
    fn doc_comment(&self, text: &str) -> Rendered {
        line_comments("// ", text)
    }

    fn ty(&self, t: &Type, cx: &RenderCtx) -> Rendered {
        ty(t, cx)
    }

    fn record(&self, name: &str, _vis: &Visibility, rec: &Record, cx: &RenderCtx) -> Rendered {
        let info = analyze_generics(rec.generics.as_ref());
        let header = kw("type") + sp() + tyname(name) + generics_decl(&info) + sp() + kw("struct");
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
        let decl = generics_decl(&info);
        let sealed = format!("is{name}");
        // The sealed interface: `type Name interface { isName() }`.
        let iface = kw("type")
            + sp()
            + tyname(name)
            + decl.clone()
            + sp()
            + kw("interface")
            + sp()
            + block("{", vec![ident(&sealed) + punct("()")], "}");
        // One struct + marker method per variant.
        let mut out = iface;
        for v in variants {
            let vname = format!("{name}{}", v.name);
            let fields = variant_fields(v, cx);
            let struct_decl = kw("type")
                + sp()
                + tyname(&vname)
                + decl.clone()
                + sp()
                + kw("struct")
                + sp()
                + block("{", fields, "}");
            // Build receiver type: ShapeCircle when no type params, ShapeCircle[T] when has them.
            let receiver_ty = if info.types.is_empty() {
                tyname(&vname)
            } else {
                let tp_args: Vec<Rendered> = info.types.iter().map(|t| tyname(t)).collect();
                tyname(&vname) + generic_list("[", tp_args, "]")
            };
            let marker = kw("func")
                + sp()
                + punct("(")
                + receiver_ty
                + punct(")")
                + sp()
                + ident(&sealed)
                + punct("() {}");
            out = out + Doc::hardline() + Doc::hardline() + struct_decl + Doc::hardline() + marker;
        }
        out
    }

    fn function(&self, name: &str, f: &Function, _vis: &Visibility, cx: &RenderCtx) -> Rendered {
        let info = analyze_generics(f.generics.as_ref());
        let params = value_params(f.input_parameters.as_deref(), cx);
        kw("func")
            + sp()
            + ident(name)
            + generics_decl(&info)
            + arglist("(", params, ")")
            + returns(f.output_parameters.as_deref(), cx)
    }

    fn interface(&self, name: &str, def: &TraitDef, _vis: &Visibility, cx: &RenderCtx) -> Rendered {
        let info = analyze_generics(def.generics.as_ref());
        let header = kw("type") + sp() + tyname(name) + generics_decl(&info) + sp() + kw("interface");
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
    if info.types.is_empty() && info.consts.is_empty() {
        return Doc::nil();
    }
    let mut params: Vec<Rendered> = Vec::new();
    for name in &info.types {
        let bounds: Vec<&str> = info.bounds_of(name).collect();
        let constraint = if bounds.is_empty() {
            kw("any")
        } else {
            Doc::join(punct(" | "), bounds.iter().map(|b| tyname(b)))
        };
        params.push(tyname(name) + sp() + constraint);
    }
    for c in &info.consts {
        params.push(tyname(c) + sp() + kw("int"));
    }
    generic_list("[", params, "]")
}

fn field(f: &Field, cx: &RenderCtx) -> Option<Rendered> {
    let Field::Known(kf) = f else { return None };
    let name = go_export(&field_name(&kf.key));
    let mut t = kf.r#type.as_ref().map(|t| ty(t, cx)).unwrap_or_else(|| kw("any"));
    if kf.attributes.is_optional {
        t = punct("*") + t;
    }
    Some(tyname(&name) + sp() + t)
}

fn variant_fields(v: &SumVariant, cx: &RenderCtx) -> Vec<Rendered> {
    match &v.data {
        None => Vec::new(),
        Some(SumField::Tuple(tys)) => tys
            .iter()
            .enumerate()
            .map(|(i, t)| tyname(&format!("Field{i}")) + sp() + ty(t, cx))
            .collect(),
        Some(SumField::StructLike(fields)) => {
            fields.iter().filter_map(|f| field(f, cx)).collect()
        }
    }
}

fn value_params(inputs: Option<&[Parameter]>, cx: &RenderCtx) -> Vec<Rendered> {
    inputs
        .into_iter()
        .flatten()
        .filter_map(|p| match p {
            Parameter::Literal(lp) => {
                let t = lp.r#type.as_ref().map(|t| ty(t, cx)).unwrap_or_else(|| kw("any"));
                Some(ident(&lp.name) + sp() + t)
            }
            _ => None,
        })
        .collect()
}

fn returns(outputs: Option<&[Parameter]>, cx: &RenderCtx) -> Rendered {
    let tys: Vec<Rendered> = outputs
        .into_iter()
        .flatten()
        .filter_map(|p| match p {
            Parameter::Literal(lp) => lp.r#type.as_ref().map(|t| ty(t, cx)),
            _ => None,
        })
        .collect();
    match tys.len() {
        0 => Doc::nil(),
        1 => sp() + tys.into_iter().next().unwrap(),
        _ => sp() + arglist("(", tys, ")"),
    }
}

fn method_sig(m: &TraitMethod, cx: &RenderCtx) -> Rendered {
    let params = value_params(m.parameters.as_deref(), cx);
    let ret = m.return_type.as_ref().map(|t| sp() + ty(t, cx)).unwrap_or_else(Doc::nil);
    tyname(&go_export(&m.name)) + arglist("(", params, ")") + ret
}

fn ty(t: &Type, cx: &RenderCtx) -> Rendered {
    match t {
        Type::TypeReference(r) => type_reference(r, cx),
        Type::Primitive(p) => tyname(primitive(p)),
        Type::GenericParam(g) => tyname(&g.name),
        Type::SelfType => tyname("T"),
        Type::Slice(inner) => punct("[]") + ty(inner, cx),
        Type::Array { r#type, length } => {
            punct("[") + tyname(&length.to_string()) + punct("]") + ty(r#type, cx)
        }
        Type::BorrowedRef { is_mutable, r#type, .. } => {
            if *is_mutable {
                punct("*") + ty(r#type, cx)
            } else {
                ty(r#type, cx)
            }
        }
        Type::RawPointer { r#type, .. } => punct("*") + ty(r#type, cx),
        Type::Tuple(ts) if ts.is_empty() => kw("struct") + punct("{}"),
        Type::Tuple(ts) => tyname("Tuple") + generic_args_raw(ts.iter().map(|t| ty(t, cx))),
        Type::NamedTuple(ms) => {
            let parts: Vec<Rendered> = ms
                .iter()
                .map(|m| match &m.label {
                    Some(label) if !label.is_empty() => {
                        ident(label) + sp() + ty(&m.r#type, cx)
                    }
                    _ => ty(&m.r#type, cx),
                })
                .collect();
            arglist("(", parts, ")")
        }
        Type::TypeOperator(op) => type_operator(op, cx),
        // Index-signature record used historically for maps: `map[K]V`.
        Type::RecordLiteral(rec) if rec.fields.len() == 1 => {
            if let Some(Field::Pattern(idx)) = rec.fields.first() {
                return kw("map")
                    + punct("[")
                    + ty(&idx.key_type, cx)
                    + punct("]")
                    + ty(&idx.value_type, cx);
            }
            kw("any")
        }
        Type::Any | Type::Infer => kw("any"),
        Type::Variadic(inner) => punct("...") + ty(inner, cx),
        _ => kw("any"),
    }
}

/// Render first-class Go type operators emitted by the lowerer:
/// `map` over `Tuple([K, V])`, and `chan` / `chan<-` / `<-chan` over the
/// element type. Unknown operators fall back to `any` rather than
/// inventing syntax.
fn type_operator(op: &TypeOperator, cx: &RenderCtx) -> Rendered {
    match op.operator.as_str() {
        "map" => match op.r#type.as_ref() {
            Type::Tuple(parts) if parts.len() == 2 => {
                kw("map")
                    + punct("[")
                    + ty(&parts[0], cx)
                    + punct("]")
                    + ty(&parts[1], cx)
            }
            _ => kw("map") + punct("[") + kw("any") + punct("]") + kw("any"),
        },
        "chan" => kw("chan") + sp() + ty(&op.r#type, cx),
        "chan<-" => kw("chan") + punct("<-") + sp() + ty(&op.r#type, cx),
        "<-chan" => punct("<-") + kw("chan") + sp() + ty(&op.r#type, cx),
        // Approximation terms from constraint type sets — not surface Go
        // outside a constraint, so render the inner type alone.
        "~" => ty(&op.r#type, cx),
        _ => kw("any"),
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
    if let Some(known) = super::known_type(sn, &type_args, Language::Go) {
        return known;
    }
    let base = tyname(sn);
    match &r.generic_args {
        Some(args) if !args.is_empty() => base + generic_args(args, cx),
        _ => base,
    }
}

fn generic_args(args: &[GenericArg], cx: &RenderCtx) -> Rendered {
    let items = args.iter().filter_map(|a| match a {
        GenericArg::Type(t) => Some(ty(t, cx)),
        _ => None,
    });
    generic_args_raw(items)
}

fn generic_args_raw(items: impl IntoIterator<Item = Rendered>) -> Rendered {
    generic_list("[", items.into_iter().collect(), "]")
}

fn primitive(p: &Primitive) -> &'static str {
    match p {
        Primitive::Int(w) => match w {
            Width::W8 => "int8",
            Width::W16 => "int16",
            Width::W32 => "int32",
            Width::W64 | Width::W128 => "int64",
            Width::Arch => "int",
        },
        Primitive::UInt(w) => match w {
            Width::W8 => "uint8",
            Width::W16 => "uint16",
            Width::W32 => "uint32",
            Width::W64 | Width::W128 => "uint64",
            Width::Arch => "uint",
        },
        Primitive::Float(w) => match w {
            Width::W32 => "float32",
            _ => "float64",
        },
        Primitive::Bool => "bool",
        Primitive::String => "string",
        Primitive::Char => "rune",
        Primitive::Bytes => "[]byte",
        Primitive::Date => "time.Time",
        Primitive::Address => "uintptr",
    }
}

/// Go exports identifiers by capitalising the first letter.
fn go_export(name: &str) -> String {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().chain(chars).collect(),
        None => name.to_string(),
    }
}

fn field_name(key: &FieldKey) -> String {
    match key {
        FieldKey::Ident(n) => n.clone(),
        FieldKey::Index(i) => format!("Field{i}"),
        FieldKey::Computed(_) => "Field".into(),
    }
}

fn line_comments(prefix: &str, text: &str) -> Rendered {
    let lines = text.lines().map(|l| txt(prefix).annotate(Annotation::Comment) + txt(l));
    Doc::join(Doc::hardline(), lines).annotate(Annotation::Comment)
}
