//! IR → Rust surface syntax.

use ir::function::Function;
use ir::generics::{GenericArg, Generics};
use ir::kind::Visibility;
use ir::parameter::Parameter;
use ir::primitives::{Primitive, Width};
use ir::protocols::{ReceiverKind, TraitDef, TraitMethod};
use ir::record::{Field, FieldKey, Record, SumField, SumVariant};
use ir::ty::{Type, TypeReference};

use super::super::backend::*;
use super::super::doc::Doc;
use super::{analyze_generics, GenericInfo};

/// The Rust backend. Zero-sized; all state lives in the [`RenderCtx`].
pub struct Rust;

impl Backend for Rust {
    fn doc_comment(&self, text: &str) -> Rendered {
        let lines = text.lines().map(|l| txt("/// ").annotate(Annotation::Comment) + txt(l));
        Doc::join(Doc::hardline(), lines).annotate(Annotation::Comment)
    }

    fn ty(&self, t: &Type, cx: &RenderCtx) -> Rendered {
        ty(t, cx)
    }

    fn record(&self, name: &str, vis: &Visibility, rec: &Record, cx: &RenderCtx) -> Rendered {
        let info = analyze_generics(rec.generics.as_ref());
        let header = vis_prefix(vis) + kw("struct") + sp() + tyname(name) + generics_decl(&info);

        let fields: Vec<Rendered> = rec
            .fields
            .iter()
            .filter_map(|f| field(f, cx))
            .collect();

        if fields.is_empty() {
            header + punct(";")
        } else {
            header + sp() + block("{", fields, "}")
        }
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
        let header = vis_prefix(vis) + kw("enum") + sp() + tyname(name) + generics_decl(&info);
        let arms: Vec<Rendered> = variants.iter().map(|v| variant(v, cx)).collect();
        header + sp() + block("{", arms, "}")
    }

    fn function(&self, name: &str, f: &Function, vis: &Visibility, cx: &RenderCtx) -> Rendered {
        let info = analyze_generics(f.generics.as_ref());
        let params = signature_params(f.receiver.as_ref(), f.input_parameters.as_deref(), cx);
        let sig = vis_prefix(vis)
            + kw("fn")
            + sp()
            + ident(name)
            + generics_decl(&info)
            + arglist("(", params, ")")
            + return_suffix(f.output_parameters.as_deref(), cx);
        sig + punct(";")
    }

    fn interface(&self, name: &str, def: &TraitDef, vis: &Visibility, cx: &RenderCtx) -> Rendered {
        let info = analyze_generics(def.generics.as_ref());
        let header = vis_prefix(vis) + kw("trait") + sp() + tyname(name) + generics_decl(&info);

        let mut members: Vec<Rendered> = Vec::new();
        for m in def.required_methods.iter().flatten() {
            members.push(trait_method(m, false, cx));
        }
        for m in def.provided_methods.iter().flatten() {
            members.push(trait_method(m, true, cx));
        }
        if members.is_empty() {
            header + sp() + punct("{}")
        } else {
            header + sp() + block("{", members, "}")
        }
    }
}

// ---------------------------------------------------------------------------
// Declarations
// ---------------------------------------------------------------------------

fn vis_prefix(v: &Visibility) -> Rendered {
    match v {
        Visibility::Public => kw("pub") + sp(),
        Visibility::Private => Doc::nil(),
        Visibility::Internal | Visibility::Protected => kw("pub(crate)") + sp(),
        Visibility::Package => kw("pub(super)") + sp(),
    }
}

fn generics_decl(info: &GenericInfo) -> Rendered {
    if info.is_empty() {
        return Doc::nil();
    }
    let mut params: Vec<Rendered> = Vec::new();
    for lt in &info.lifetimes {
        params.push(punct("'") + ident(lt));
    }
    for name in &info.types {
        let bounds: Vec<&str> = info.bounds_of(name).collect();
        let mut p = tyname(name);
        if !bounds.is_empty() {
            p = p + punct(": ") + Doc::join(punct(" + "), bounds.iter().map(|b| tyname(b)));
        }
        params.push(p);
    }
    for c in &info.consts {
        params.push(kw("const") + sp() + ident(c) + punct(": ") + tyname("usize"));
    }
    generic_list("<", params, ">")
}

fn field(f: &Field, cx: &RenderCtx) -> Option<Rendered> {
    field_with(f, true, cx)
}

/// Render a field; `with_vis` is false inside enum variants, where Rust does
/// not permit per-field visibility.
fn field_with(f: &Field, with_vis: bool, cx: &RenderCtx) -> Option<Rendered> {
    let Field::Known(kf) = f else { return None };
    let name = match &kf.key {
        FieldKey::Ident(n) => n.clone(),
        FieldKey::Index(i) => i.to_string(),
        FieldKey::Computed(_) => "_".into(),
    };
    let vis = if with_vis {
        kf.visibility.as_ref().map(vis_prefix).unwrap_or_else(Doc::nil)
    } else {
        Doc::nil()
    };
    let mut ty_doc = kf
        .r#type
        .as_ref()
        .map(|t| ty(t, cx))
        .unwrap_or_else(|| tyname("()"));
    if kf.attributes.is_optional {
        ty_doc = tyname("Option") + punct("<") + ty_doc + punct(">");
    }
    let doc = doc_prefix(kf.documentation.as_deref(), cx);
    Some(doc + vis + ident(&name) + punct(": ") + ty_doc + punct(","))
}

fn variant(v: &SumVariant, cx: &RenderCtx) -> Rendered {
    let doc = doc_prefix(v.documentation.as_deref(), cx);
    let head = doc + tyname(&v.name);
    let body = match &v.data {
        None => Doc::nil(),
        Some(SumField::Tuple(tys)) => {
            arglist("(", tys.iter().map(|t| ty(t, cx)).collect(), ")")
        }
        Some(SumField::StructLike(fields)) => {
            let fs: Vec<Rendered> = fields.iter().filter_map(|f| field_with(f, false, cx)).collect();
            sp() + block("{", fs, "}")
        }
    };
    head + body + punct(",")
}

fn trait_method(m: &TraitMethod, provided: bool, cx: &RenderCtx) -> Rendered {
    let info = analyze_generics(m.generics.as_ref());
    let params = signature_params(m.receiver.as_ref(), m.parameters.as_deref(), cx);
    let ret = m
        .return_type
        .as_ref()
        .map(|t| punct(" -> ") + ty(t, cx))
        .unwrap_or_else(Doc::nil);
    let doc = doc_prefix(m.documentation.as_deref(), cx);
    let sig = doc
        + kw("fn")
        + sp()
        + ident(&m.name)
        + generics_decl(&info)
        + arglist("(", params, ")")
        + ret;
    if provided {
        sig + sp() + punct("{") + sp() + txt("...") + sp() + punct("}")
    } else {
        sig + punct(";")
    }
}

// ---------------------------------------------------------------------------
// Signatures
// ---------------------------------------------------------------------------

fn signature_params(
    receiver: Option<&ReceiverKind>,
    inputs: Option<&[Parameter]>,
    cx: &RenderCtx,
) -> Vec<Rendered> {
    let mut params: Vec<Rendered> = Vec::new();
    if let Some(r) = receiver {
        match r {
            ReceiverKind::Owned => params.push(kw("self")),
            ReceiverKind::SharedRef => params.push(punct("&") + kw("self")),
            ReceiverKind::MutRef => params.push(punct("&") + kw("mut") + sp() + kw("self")),
            ReceiverKind::Arbitrary => params.push(kw("self") + punct(": ") + tyname("_")),
            ReceiverKind::Static => {}
        }
    }
    for p in inputs.into_iter().flatten() {
        if let Parameter::Literal(lp) = p {
            let ty_doc = lp
                .r#type
                .as_ref()
                .map(|t| ty(t, cx))
                .unwrap_or_else(|| tyname("_"));
            params.push(ident(&lp.name) + punct(": ") + ty_doc);
        }
    }
    params
}

fn return_suffix(outputs: Option<&[Parameter]>, cx: &RenderCtx) -> Rendered {
    let tys: Vec<&Type> = outputs
        .into_iter()
        .flatten()
        .filter_map(|p| match p {
            Parameter::Literal(lp) => lp.r#type.as_ref(),
            _ => None,
        })
        .collect();
    match tys.as_slice() {
        [] => Doc::nil(),
        [one] => punct(" -> ") + ty(one, cx),
        many => punct(" -> ") + arglist("(", many.iter().map(|t| ty(t, cx)).collect(), ")"),
    }
}

fn doc_prefix(text: Option<&str>, cx: &RenderCtx) -> Rendered {
    match (cx.options.show_docs, text) {
        (true, Some(t)) if !t.is_empty() => Rust.doc_comment(t) + Doc::hardline(),
        _ => Doc::nil(),
    }
}

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

fn ty(t: &Type, cx: &RenderCtx) -> Rendered {
    match t {
        Type::TypeReference(r) => type_reference(r, cx),
        Type::SelfType => kw("Self"),
        Type::Primitive(p) => tyname(primitive(p)),
        Type::GenericParam(g) => tyname(&g.name),
        Type::Tuple(ts) if ts.is_empty() => tyname("()"),
        Type::Tuple(ts) => arglist("(", ts.iter().map(|t| ty(t, cx)).collect(), ")"),
        Type::Slice(inner) => punct("[") + ty(inner, cx) + punct("]"),
        Type::Array { r#type, length } => {
            punct("[") + ty(r#type, cx) + punct("; ") + tyname(&length.to_string()) + punct("]")
        }
        Type::BorrowedRef { lifetime, is_mutable, r#type } => {
            let lt = lifetime
                .as_ref()
                .map(|l| punct("'") + ident(l) + sp())
                .unwrap_or_else(Doc::nil);
            let m = if *is_mutable { kw("mut") + sp() } else { Doc::nil() };
            punct("&") + lt + m + ty(r#type, cx)
        }
        Type::RawPointer { is_mutable, r#type } => {
            let m = if *is_mutable { kw("mut") } else { kw("const") };
            punct("*") + m + sp() + ty(r#type, cx)
        }
        Type::Never => punct("!"),
        Type::Infer => tyname("_"),
        Type::Any => kw("dyn") + sp() + tyname("Any"),
        Type::Union(ts) => Doc::join(punct(" | "), ts.iter().map(|t| ty(t, cx))),
        Type::Intersection(ts) => Doc::join(punct(" + "), ts.iter().map(|t| ty(t, cx))),
        Type::Variadic(inner) => punct("...") + ty(inner, cx),
        Type::ImplTrait(_) => kw("impl") + sp() + tyname("_"),
        Type::RecordLiteral(rec) => match &rec.name {
            Some(n) => tyname(short_name(n, cx)),
            None => punct("{ ... }"),
        },
        other => tyname(&format!("/* {} */", type_tag(other))),
    }
}

fn type_reference(r: &TypeReference, cx: &RenderCtx) -> Rendered {
    let base = tyname(short_name(&r.identifier, cx));
    match &r.generic_args {
        Some(args) if !args.is_empty() => base + generic_args(args, cx),
        _ => base,
    }
}

fn generic_args(args: &[GenericArg], cx: &RenderCtx) -> Rendered {
    let items: Vec<Rendered> = args
        .iter()
        .filter_map(|a| match a {
            GenericArg::Type(t) => Some(ty(t, cx)),
            GenericArg::Lifetime(l) => Some(punct("'") + ident(l)),
            _ => None,
        })
        .collect();
    generic_list("<", items, ">")
}

fn primitive(p: &Primitive) -> &'static str {
    match p {
        Primitive::Int(w) => match w {
            Width::W8 => "i8",
            Width::W16 => "i16",
            Width::W32 => "i32",
            Width::W64 => "i64",
            Width::W128 => "i128",
            Width::Arch => "isize",
        },
        Primitive::UInt(w) => match w {
            Width::W8 => "u8",
            Width::W16 => "u16",
            Width::W32 => "u32",
            Width::W64 => "u64",
            Width::W128 => "u128",
            Width::Arch => "usize",
        },
        Primitive::Float(w) => match w {
            Width::W16 | Width::W8 => "f16",
            Width::W32 => "f32",
            Width::W128 => "f128",
            _ => "f64",
        },
        Primitive::Bool => "bool",
        Primitive::String => "String",
        Primitive::Char => "char",
        Primitive::Bytes => "Vec<u8>",
        Primitive::Date => "SystemTime",
        Primitive::Address => "*const c_void",
    }
}

/// A short tag for a `Type` variant with no core rendering, used in fallbacks.
fn type_tag(t: &Type) -> &'static str {
    match t {
        Type::DynTrait(_) => "dyn",
        Type::FunctionPointer(_) => "fn-ptr",
        Type::QualifiedPath(_) => "qualified-path",
        Type::Sum(_) => "inline-enum",
        Type::TypeOperator(_) => "type-operator",
        Type::Conditional(_) => "conditional",
        Type::Mapped(_) => "mapped",
        Type::Predicate(_) => "predicate",
        _ => "type",
    }
}
