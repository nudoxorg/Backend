//! IR → Rust surface syntax.

use ir::function::{Attribute, Function};
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
        let lines = text
            .lines()
            .map(|l| txt("/// ").annotate(Annotation::Comment) + txt(l));
        Doc::join(Doc::hardline(), lines).annotate(Annotation::Comment)
    }

    fn ty(&self, t: &Type, cx: &RenderCtx) -> Rendered {
        ty(t, cx)
    }

    fn record(&self, name: &str, vis: &Visibility, rec: &Record, cx: &RenderCtx) -> Rendered {
        let info = analyze_generics(rec.generics.as_ref());
        let header = vis_prefix(vis) + kw("struct") + sp() + tyname(name) + generics_decl(&info);

        let fields: Vec<Rendered> = rec.fields.iter().filter_map(|f| field(f, cx)).collect();

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
        let quals = fn_qualifiers(f.attributes.as_deref());
        let params = signature_params(f.receiver.as_ref(), f.input_parameters.as_deref(), cx);
        let sig = vis_prefix(vis)
            + quals
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

/// Canonical Rust qualifier order: `const async gen unsafe`.
fn fn_qualifiers(attrs: Option<&[Attribute]>) -> Rendered {
    let mut out = Doc::nil();
    let attrs = attrs.unwrap_or(&[]);
    if attrs.contains(&Attribute::Const) {
        out = out + kw("const") + sp();
    }
    if attrs.contains(&Attribute::Async) {
        out = out + kw("async") + sp();
    }
    if attrs.contains(&Attribute::Generator) {
        out = out + kw("gen") + sp();
    }
    if attrs.contains(&Attribute::Unsafe) {
        out = out + kw("unsafe") + sp();
    }
    out
}

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
        kf.visibility
            .as_ref()
            .map(vis_prefix)
            .unwrap_or_else(Doc::nil)
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
        Some(SumField::Tuple(tys)) => arglist("(", tys.iter().map(|t| ty(t, cx)).collect(), ")"),
        Some(SumField::StructLike(fields)) => {
            let fs: Vec<Rendered> = fields
                .iter()
                .filter_map(|f| field_with(f, false, cx))
                .collect();
            sp() + block("{", fs, "}")
        }
    };
    head + body + punct(",")
}

fn trait_method(m: &TraitMethod, provided: bool, cx: &RenderCtx) -> Rendered {
    let info = analyze_generics(m.generics.as_ref());
    let quals = fn_qualifiers(m.attributes.as_deref());
    let params = signature_params(m.receiver.as_ref(), m.parameters.as_deref(), cx);
    let ret = m
        .return_type
        .as_ref()
        .map(|t| punct(" -> ") + ty(t, cx))
        .unwrap_or_else(Doc::nil);
    let doc = doc_prefix(m.documentation.as_deref(), cx);
    let sig = doc
        + quals
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
        Type::BorrowedRef {
            lifetime,
            is_mutable,
            r#type,
        } => {
            let lt = lifetime
                .as_ref()
                .map(|l| punct("'") + ident(l) + sp())
                .unwrap_or_else(Doc::nil);
            let m = if *is_mutable {
                kw("mut") + sp()
            } else {
                Doc::nil()
            };
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
    let sn = short_name(&r.identifier, cx);
    // Pre-render type args for the known_type check.
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
    if let Some(known) = super::known_type(sn, &type_args, Language::Rust) {
        return known;
    }
    let base = tyname(sn);
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

#[cfg(test)]
mod tests {
    use ir::function::{Attribute, Function};
    use ir::generics::{Constraint, GenericArg, Generics, TraitRef};
    use ir::generics::{Kind, Variance};
    use ir::kind::Visibility;
    use ir::parameter::TypeParamOrigin;
    use ir::parameter::{LifetimeParam, LiteralParameter, Parameter, TypeParam};
    use ir::primitives::{Primitive, Width};
    use ir::protocols::{ReceiverKind, TraitDef, TraitMethod};
    use ir::record::{Field, FieldAttributes, FieldKey, KnownField, Record, SumField, SumVariant};
    use ir::ty::{GenericParam, Type, TypeReference};

    use super::super::super::backend::Backend;
    use super::super::super::backend::{Language, RenderCtx, RenderOptions};
    use super::Rust;

    fn cx() -> RenderCtx {
        RenderCtx::new(Language::Rust)
    }

    fn cx_with_docs() -> RenderCtx {
        RenderCtx::new(Language::Rust).with_docs(true)
    }

    fn ty_ref(identifier: &str) -> Type {
        Type::TypeReference(TypeReference {
            identifier: identifier.to_string(),
            generic_args: None,
        })
    }

    fn ty_ref_args(identifier: &str, args: Vec<GenericArg>) -> Type {
        Type::TypeReference(TypeReference {
            identifier: identifier.to_string(),
            generic_args: Some(args),
        })
    }

    fn ty_param(name: &str) -> Type {
        Type::GenericParam(GenericParam {
            name: name.to_string(),
            kind: None,
        })
    }

    fn no_field_attributes() -> FieldAttributes {
        FieldAttributes {
            decorators: vec![],
            is_mutable: false,
            is_optional: false,
            is_static: false,
        }
    }

    fn known_field(
        name: &str,
        ty: Type,
        visibility: Visibility,
        documentation: Option<&str>,
    ) -> Field {
        Field::Known(KnownField {
            key: FieldKey::Ident(name.to_string()),
            r#type: Some(Box::new(ty)),
            default_value: None,
            attributes: no_field_attributes(),
            visibility: Some(visibility),
            documentation: documentation.map(str::to_string),
        })
    }

    fn tuple_field(index: usize, ty: Type, visibility: Visibility) -> Field {
        Field::Known(KnownField {
            key: FieldKey::Index(index),
            r#type: Some(Box::new(ty)),
            default_value: None,
            attributes: no_field_attributes(),
            visibility: Some(visibility),
            documentation: None,
        })
    }

    fn record(name: &str, generics: Option<Generics>, fields: Vec<Field>) -> Record {
        Record {
            name: Some(name.to_string()),
            generics,
            fields,
            call_signatures: None,
            constructors: None,
            methods: None,
            index_signatures: None,
            super_types: None,
            members: None,
            implemented_protocols: None,
        }
    }

    fn type_param_p(name: &str) -> Parameter {
        Parameter::Type(TypeParam {
            name: Some(name.to_string()),
            kind: Kind::Type,
            variance: Variance::Invariant,
            default_type: None,
            params: None,
            origin: TypeParamOrigin::Free,
        })
    }

    fn lifetime_param_p(name: &str) -> Parameter {
        Parameter::Lifetime(LifetimeParam {
            name: name.to_string(),
            variance: Variance::Invariant,
        })
    }

    fn literal_param(name: &str, ty: Type) -> Parameter {
        Parameter::Literal(LiteralParameter {
            name: name.to_string(),
            r#type: Some(ty),
            attributes: None,
            default_value: None,
            description: None,
        })
    }

    fn output(ty: Type) -> Vec<Parameter> {
        vec![Parameter::Literal(LiteralParameter {
            name: String::new(),
            r#type: Some(ty),
            attributes: None,
            default_value: None,
            description: None,
        })]
    }

    fn trait_ref_val(name: &str) -> TraitRef {
        TraitRef {
            name: name.to_string(),
            args: vec![],
        }
    }

    #[test]
    fn plain_struct() {
        let rec = record(
            "Point",
            None,
            vec![
                known_field(
                    "x",
                    Type::Primitive(Primitive::Float(Width::W64)),
                    Visibility::Public,
                    None,
                ),
                known_field(
                    "y",
                    Type::Primitive(Primitive::Float(Width::W64)),
                    Visibility::Public,
                    None,
                ),
            ],
        );
        let result = Rust
            .record("Point", &Visibility::Public, &rec, &cx())
            .render(80);
        assert_eq!(
            result,
            "pub struct Point {\n    pub x: f64,\n    pub y: f64,\n}"
        );
    }

    #[test]
    fn plain_struct_with_field_docs() {
        let rec = record(
            "Point",
            None,
            vec![known_field(
                "x",
                Type::Primitive(Primitive::Float(Width::W64)),
                Visibility::Public,
                Some("The x coordinate."),
            )],
        );
        let result = Rust
            .record("Point", &Visibility::Public, &rec, &cx_with_docs())
            .render(80);
        assert_eq!(
            result,
            "pub struct Point {\n    /// The x coordinate.\n    pub x: f64,\n}"
        );
    }

    #[test]
    fn generic_struct_simple_bounds_only() {
        let generics = Generics {
            params: vec![lifetime_param_p("'a"), type_param_p("T")],
            constraints: vec![
                Constraint::TraitBound {
                    param: "T".to_string(),
                    trait_ref: TraitRef {
                        name: "Iterator".to_string(),
                        args: vec![ir::generics::TypeExpr {
                            name: "Item = u8".to_string(),
                            args: vec![],
                        }],
                    },
                },
                Constraint::LifetimeBound {
                    shorter: "T".to_string(),
                    longer: "'a".to_string(),
                },
            ],
        };
        let rec = record(
            "Wrap",
            Some(generics),
            vec![known_field(
                "inner",
                Type::BorrowedRef {
                    lifetime: Some("'a".to_string()),
                    is_mutable: false,
                    r#type: Box::new(ty_param("T")),
                },
                Visibility::Public,
                None,
            )],
        );
        // New backend: no where clause, no lifetime bounds inline → <'a, T>
        let result = Rust
            .record("Wrap", &Visibility::Public, &rec, &cx())
            .render(80);
        assert_eq!(result, "pub struct Wrap<'a, T> {\n    pub inner: &'a T,\n}");
    }

    #[test]
    fn unit_struct() {
        let rec = record("Marker", None, vec![]);
        let result = Rust
            .record("Marker", &Visibility::Public, &rec, &cx())
            .render(80);
        assert_eq!(result, "pub struct Marker;");
    }

    #[test]
    fn tuple_struct_as_named() {
        let rec = record(
            "Pair",
            None,
            vec![
                tuple_field(
                    0,
                    Type::Primitive(Primitive::UInt(Width::W8)),
                    Visibility::Public,
                ),
                tuple_field(1, Type::Primitive(Primitive::Bool), Visibility::Private),
            ],
        );
        let result = Rust
            .record("Pair", &Visibility::Public, &rec, &cx())
            .render(80);
        assert_eq!(result, "pub struct Pair {\n    pub 0: u8,\n    1: bool,\n}");
    }

    #[test]
    fn enum_with_all_variant_shapes() {
        let variants = vec![
            SumVariant {
                name: "Unit".to_string(),
                data: None,
                documentation: None,
            },
            SumVariant {
                name: "Tup".to_string(),
                data: Some(SumField::Tuple(vec![
                    Type::Primitive(Primitive::UInt(Width::W8)),
                    ty_ref("alloc::string::String"),
                ])),
                documentation: None,
            },
            SumVariant {
                name: "Rec".to_string(),
                data: Some(SumField::StructLike(vec![known_field(
                    "id",
                    Type::Primitive(Primitive::UInt(Width::W64)),
                    Visibility::Public,
                    None,
                )])),
                documentation: None,
            },
        ];
        let result = Rust
            .sum("Shape", None, &variants, &Visibility::Public, &cx())
            .render(80);
        // Struct-like variants use block{} which always breaks; different from legacy.
        assert_eq!(
            result,
            "pub enum Shape {\n    Unit,\n    Tup(u8, String),\n    Rec {\n        id: u64,\n    },\n}"
        );
    }

    #[test]
    fn async_method_with_receiver_and_generics() {
        let function = Function {
            input_parameters: Some(vec![literal_param(
                "limit",
                Type::Primitive(Primitive::UInt(Width::Arch)),
            )]),
            output_parameters: Some(output(ty_ref_args(
                "alloc::vec::Vec",
                vec![GenericArg::Type(ty_param("T"))],
            ))),
            type_links: None,
            attributes: Some(vec![Attribute::Async]),
            generics: Some(Generics {
                params: vec![type_param_p("T")],
                constraints: vec![Constraint::TraitBound {
                    param: "T".to_string(),
                    trait_ref: trait_ref_val("core::clone::Clone"),
                }],
            }),
            receiver: Some(ReceiverKind::MutRef),
            overloads: None,
            implemented: true,
            members: None,
            implemented_protocols: None,
            body: None,
        };
        let result = Rust
            .function("fetch", &function, &Visibility::Public, &cx())
            .render(80);
        assert_eq!(
            result,
            "pub async fn fetch<T: Clone>(&mut self, limit: usize) -> Vec<T>;"
        );
    }

    #[test]
    fn trait_with_methods() {
        let def = TraitDef {
            generics: None,
            super_traits: None,
            associated_types: None,
            properties: None,
            required_methods: Some(vec![TraitMethod {
                name: "next".to_string(),
                parameters: None,
                return_type: Some(Box::new(ty_ref_args(
                    "core::option::Option",
                    vec![GenericArg::Type(Type::Primitive(Primitive::UInt(
                        Width::W8,
                    )))],
                ))),
                generics: None,
                attributes: None,
                documentation: None,
                receiver: Some(ReceiverKind::MutRef),
                has_default_implementation: false,
            }]),
            provided_methods: Some(vec![TraitMethod {
                name: "reset".to_string(),
                parameters: None,
                return_type: None,
                generics: None,
                attributes: None,
                documentation: None,
                receiver: Some(ReceiverKind::MutRef),
                has_default_implementation: true,
            }]),
            required_constants: None,
            attributes: None,
            members: None,
        };
        let result = Rust
            .interface("Source", &def, &Visibility::Public, &cx())
            .render(80);
        assert_eq!(
            result,
            "pub trait Source {\n    fn next(&mut self) -> Option<u8>;\n    fn reset(&mut self) { ... }\n}"
        );
    }

    #[test]
    fn qualified_paths_flag() {
        let ty_val = ty_ref("std::vec::Vec");
        let default_result = Rust.ty(&ty_val, &cx()).render(80);
        assert_eq!(default_result, "Vec");
        // qualified_paths=true requires constructing RenderCtx differently:
        let qual_cx = RenderCtx {
            language: Language::Rust,
            options: RenderOptions {
                qualified_paths: true,
                show_docs: false,
            },
            width: 80,
        };
        let qualified_result = Rust.ty(&ty_val, &qual_cx).render(80);
        assert_eq!(qualified_result, "std::vec::Vec");
    }
}
