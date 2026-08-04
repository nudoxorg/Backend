//! Lowering Pyrefly's `Type` into `ir::ty::Type`.
//!
//! Maps pyrefly's rich Python type algebra (Union, Intersect, Literal,
//! Callable, TypedDict, TypeVar, ParamSpec, …) to the language-agnostic
//! IR type representation.

use pyrefly_types::callable::{Callable, DefaultValue, Function, Param, Params, Required};
use pyrefly_types::callable_residual::CallableResidualKind;
use pyrefly_types::class::ClassType;
use pyrefly_types::literal::Lit;
use pyrefly_types::quantified::{Quantified, QuantifiedKind};
use pyrefly_types::tuple::Tuple;
use pyrefly_types::type_alias::TypeAliasData;
use pyrefly_types::type_var::{PreInferenceVariance, Restriction};
use pyrefly_types::typed_dict::TypedDict;
use pyrefly_types::types::{BoundMethodType, NeverStyle, TParams, Type};

use ir::function::Attribute;
use ir::generics::{
    Constraint, ConstExpr, GenericArg, Generics, Kind, TraitRef, TypeExpr, Variance,
};
use ir::parameter::{
    LiteralParameter, Parameter as IrParameter, ParameterAttribute, TypeParam, TypeParamOrigin,
};
use ir::primitives::{Primitive, Width};
use ir::ty::{
    FunctionPointer, GenericParam, LiteralKind, LiteralValue, PredicateSubject, TypeOperator,
    TypePredicate, TypeReference,
};

/// Strip the trailing `@line:col-col` source-location suffix that pyrefly's
/// `QName` / `Class` `Display` appends to a fully-qualified name
/// (e.g. `builtins.int@418:7-10`), yielding a stable cross-reference
/// identifier (`builtins.int`).
///
/// A Python dotted name never contains `@`, so truncating at the first `@`
/// reliably drops only the location. Use this everywhere a
/// `TypeReference.identifier` is derived from a `qname()`.
pub fn strip_loc(ident: &str) -> String {
    match ident.split_once('@') {
        Some((head, _)) => head.to_string(),
        None => ident.to_string(),
    }
}

/// Lower a pyrefly `Type` into the language-agnostic `ir::ty::Type`.
///
/// IMPLEMENTATION NOTE: Python type system features that have no direct
/// IR counterpart are lossily mapped (e.g. `TypedDict` → `RecordLiteral`,
/// `TypeVar` → `GenericParam`). Review these mappings when the IR gains
/// Python-specific affordances.
pub fn lower_type(py_type: &Type) -> ir::ty::Type {
    match py_type {
        Type::Any(_) => ir::ty::Type::Any,
        Type::Never(style) => match style {
            NeverStyle::NoReturn | NeverStyle::Never => ir::ty::Type::Never,
        },
        Type::None => ir::ty::Type::TypeReference(TypeReference {
            identifier: "builtins.NoneType".into(),
            generic_args: None,
        }),
        Type::Union(u) => {
            let members: Vec<ir::ty::Type> = u.members.iter().map(lower_type).collect();
            ir::ty::Type::Union(members)
        }
        Type::Intersect(isect) => {
            let (members, _fallback) = &**isect;
            let lowered: Vec<ir::ty::Type> = members.iter().map(lower_type).collect();
            ir::ty::Type::Intersection(lowered)
        }
        Type::ClassType(ct) => lower_class_type(ct),
        Type::ClassDef(cls) => {
            // ClassDef is the definition; represent as `Type[ClassName]`.
            let qname = strip_loc(&format!("{}", cls.qname()));
            ir::ty::Type::TypeReference(TypeReference {
                identifier: "typing.Type".into(),
                generic_args: Some(vec![GenericArg::Type(ir::ty::Type::TypeReference(
                    TypeReference {
                        identifier: qname,
                        generic_args: None,
                    },
                ))]),
            })
        }
        Type::Function(func) => lower_function_type(func),
        Type::Callable(callable) => lower_callable_type(callable),
        Type::BoundMethod(bm) => lower_bound_method(&bm.func),
        // Forall is declaration-site quantification; the *body* type is what
        // consumers need in type positions. Declaration-site params are
        // extracted separately via [`lower_tparams`] / [`extract_forall_tparams`].
        Type::Forall(forall) => {
            let body = forall.body.clone().as_type();
            lower_type(&body)
        }
        Type::Overload(overload) => {
            // Pick the first overload signature; caller should inspect overloads.
            lower_type(&overload.signatures.first().as_type())
        }
        Type::Tuple(tuple) => lower_tuple_type(tuple),
        Type::TypedDict(td) => lower_typed_dict(td),
        Type::PartialTypedDict(td) => lower_typed_dict(td),
        Type::Literal(lit) => lower_literal(lit),
        Type::LiteralString(_) => ir::ty::Type::Primitive(Primitive::String),
        Type::TypeVar(tv) => ir::ty::Type::GenericParam(GenericParam {
            name: tv.qname().id().to_string(),
            kind: None,
        }),
        Type::ParamSpec(ps) => ir::ty::Type::GenericParam(GenericParam {
            name: ps.qname().id().to_string(),
            kind: None,
        }),
        Type::Quantified(q) | Type::QuantifiedValue(q) => lower_quantified(q),
        Type::TypeVarTuple(tvt) => ir::ty::Type::GenericParam(GenericParam {
            name: tvt.qname().id().to_string(),
            kind: None,
        }),
        Type::ElementOfTypeVarTuple(q) => ir::ty::Type::GenericParam(GenericParam {
            name: q.name.to_string(),
            kind: None,
        }),
        Type::Module(m) => ir::ty::Type::TypeReference(TypeReference {
            identifier: m.to_string(),
            generic_args: None,
        }),
        Type::TypeAlias(alias) | Type::UntypedAlias(alias) => lower_type_alias(alias),
        // TypeGuard / TypeIs are type predicates (PEP 647 / PEP 742).
        Type::TypeGuard(inner) => ir::ty::Type::Predicate(TypePredicate {
            asserts: false,
            subject: PredicateSubject::Identifier(String::new()),
            r#type: Some(Box::new(lower_type(inner))),
        }),
        Type::TypeIs(inner) => ir::ty::Type::Predicate(TypePredicate {
            asserts: false,
            subject: PredicateSubject::Identifier(String::new()),
            r#type: Some(Box::new(lower_type(inner))),
        }),
        // Annotated[T, meta...] — keep form via TypeOperator so the annotation
        // is not silently stripped to the bare inner type. Metadata items (the
        // `...` in `Annotated[T, ...]`) are preserved in the operator spelling
        // when present so consumers can still recover them.
        Type::Annotated(inner, meta) => {
            let operator = if meta.is_empty() {
                "Annotated".into()
            } else {
                let meta_spelling: Vec<String> = meta.iter().map(|m| format!("{m}")).collect();
                format!("Annotated[{}]", meta_spelling.join(", "))
            };
            ir::ty::Type::TypeOperator(TypeOperator {
                operator,
                r#type: Box::new(lower_type(inner)),
            })
        }
        Type::Unpack(inner) => ir::ty::Type::Variadic(Box::new(lower_type(inner))),
        Type::Concatenate(prefix, ps) => {
            // ParamSpec concatenation — lower as a generic function.
            let mut params = Vec::new();
            for p in prefix.iter() {
                params.push(IrParameter::Literal(LiteralParameter {
                    name: String::new(),
                    r#type: Some(lower_type(p.ty())),
                    attributes: None,
                    default_value: None,
                    description: None,
                }));
            }
            // The tail of a `Concatenate[...]` is itself a type (a ParamSpec,
            // `...`, or a nested concatenation); lower it as the trailing param.
            params.push(IrParameter::Literal(LiteralParameter {
                name: String::new(),
                r#type: Some(lower_type(ps)),
                attributes: None,
                default_value: None,
                description: None,
            }));
            ir::ty::Type::FunctionPointer(FunctionPointer {
                inputs: Some(params),
                outputs: None,
                attributes: None,
            })
        }
        Type::SelfType(_ct) => ir::ty::Type::SelfType,
        Type::CallableResidual(residual) => match &residual.kind {
            CallableResidualKind::Generic { quantified } => lower_quantified(quantified),
            CallableResidualKind::Overload { branches, .. } => {
                ir::ty::Type::Union(branches.iter().map(|b| lower_type(&b.ty)).collect())
            }
        },
        Type::SpecialForm(_) => ir::ty::Type::Any,
        Type::SuperInstance(super_inst) => {
            let (start_class, _obj) = &**super_inst;
            ir::ty::Type::TypeReference(TypeReference {
                identifier: strip_loc(&format!("{}", start_class.qname())),
                generic_args: None,
            })
        }
        Type::KwCall(call) => lower_type(&call.return_ty),
        Type::Var(_) => ir::ty::Type::Any,
        Type::ShapedArray(_) => ir::ty::Type::TypeReference(TypeReference {
            identifier: "torch.Tensor".into(),
            generic_args: None,
        }),
        Type::NNModule(mod_ty) => ir::ty::Type::TypeReference(TypeReference {
            identifier: strip_loc(&format!("{}", mod_ty.class.qname())),
            generic_args: Some(
                mod_ty
                    .class
                    .targs()
                    .as_slice()
                    .iter()
                    .map(|t| GenericArg::Type(lower_type(t)))
                    .collect(),
            ),
        }),
        Type::Size(_) => ir::ty::Type::Primitive(Primitive::Int(Width::W64)),
        Type::Dim(inner) => lower_type(inner),
        // The value-representation of a type, i.e. `type[X]`.
        Type::Type(inner) => ir::ty::Type::TypeReference(TypeReference {
            identifier: "typing.Type".into(),
            generic_args: Some(vec![GenericArg::Type(lower_type(inner))]),
        }),
        Type::TypeForm(inner) => lower_type(inner),
        Type::Materialization => ir::ty::Type::Infer,
        Type::Ellipsis => ir::ty::Type::Infer,
        Type::Args(q) | Type::ArgsValue(q) => ir::ty::Type::Variadic(Box::new(
            ir::ty::Type::GenericParam(GenericParam {
                name: q.name.to_string(),
                kind: None,
            }),
        )),
        Type::Kwargs(q) | Type::KwargsValue(q) => ir::ty::Type::TypeReference(TypeReference {
            identifier: "builtins.dict".into(),
            generic_args: Some(vec![
                GenericArg::Type(ir::ty::Type::Primitive(Primitive::String)),
                GenericArg::Type(ir::ty::Type::GenericParam(GenericParam {
                    name: q.name.to_string(),
                    kind: None,
                })),
            ]),
        }),
        Type::ParamSpecValue(params) => ir::ty::Type::FunctionPointer(FunctionPointer {
            inputs: Some(params.items().iter().map(lower_param).collect()),
            outputs: None,
            attributes: None,
        }),
        Type::Sentinel(_) => ir::ty::Type::Any,
    }
}

// ---------------------------------------------------------------------------
// Declaration-site generics
// ---------------------------------------------------------------------------

/// Lower a pyrefly `TParams` list into IR [`Generics`].
///
/// Each quantified becomes a `Parameter::Type` entry; bounds/constraints are
/// recorded on `Generics.constraints` so the parameter name is never lost
/// (unlike the historical path that collapsed bounded vars to their bound).
pub fn lower_tparams(tparams: &TParams) -> Option<Generics> {
    if tparams.is_empty() {
        return None;
    }

    let mut params = Vec::new();
    let mut constraints = Vec::new();

    for q in tparams.iter() {
        let name = q.name.to_string();
        let variance = match q.variance() {
            PreInferenceVariance::Covariant => Variance::Covariant,
            PreInferenceVariance::Contravariant => Variance::Contravariant,
            PreInferenceVariance::Invariant | PreInferenceVariance::Undefined => {
                Variance::Invariant
            }
        };
        let kind = match q.kind() {
            QuantifiedKind::TypeVar => Kind::Type,
            // ParamSpec is a higher-kinded callable-params constructor.
            QuantifiedKind::ParamSpec => {
                Kind::Arrow(Box::new(Kind::Type), Box::new(Kind::Constraint))
            }
            QuantifiedKind::TypeVarTuple => Kind::Type,
        };
        let default_type = q.default().map(|t| type_expr_of(&lower_type(t)));

        params.push(IrParameter::Type(TypeParam {
            name: Some(name.clone()),
            kind,
            variance,
            default_type,
            params: None,
            origin: TypeParamOrigin::Free,
        }));

        match q.restriction() {
            Restriction::Bound(bound) => {
                constraints.push(Constraint::TraitBound {
                    param: name,
                    trait_ref: trait_ref_of(&lower_type(bound)),
                });
            }
            Restriction::Constraints(cs) => {
                for c in cs {
                    constraints.push(Constraint::TraitBound {
                        param: name.clone(),
                        trait_ref: trait_ref_of(&lower_type(c)),
                    });
                }
            }
            Restriction::Unrestricted => {}
        }
    }

    Some(Generics { params, constraints })
}

/// Extract declaration-site type parameters from a callable-shaped type
/// (`Type::Forall` wrapping a function / bound method).
pub fn extract_forall_tparams(py_type: &Type) -> Option<Generics> {
    match py_type {
        Type::Forall(forall) => lower_tparams(&forall.tparams),
        Type::BoundMethod(bm) => match &bm.func {
            BoundMethodType::Forall(fa) => lower_tparams(&fa.tparams),
            _ => None,
        },
        _ => None,
    }
}

/// Synthesize a [`Generics`] list from free `GenericParam` names mentioned in
/// already-lowered parameter types. Used when a function is *not* wrapped in
/// `Type::Forall` but still references TypeVars in its signature.
pub fn generics_from_free_params(
    inputs: &[IrParameter],
    outputs: &Option<IrParameter>,
) -> Option<Generics> {
    let mut names: Vec<String> = Vec::new();
    let mut push = |ty: &ir::ty::Type| collect_generic_param_names(ty, &mut names);
    for p in inputs {
        if let IrParameter::Literal(lp) = p {
            if let Some(ty) = &lp.r#type {
                push(ty);
            }
        }
    }
    if let Some(IrParameter::Literal(lp)) = outputs {
        if let Some(ty) = &lp.r#type {
            push(ty);
        }
    }
    names.sort();
    names.dedup();
    if names.is_empty() {
        return None;
    }
    let params = names
        .into_iter()
        .map(|name| {
            IrParameter::Type(TypeParam {
                name: Some(name),
                kind: Kind::Type,
                variance: Variance::Invariant,
                default_type: None,
                params: None,
                origin: TypeParamOrigin::Free,
            })
        })
        .collect();
    Some(Generics {
        params,
        constraints: Vec::new(),
    })
}

fn collect_generic_param_names(ty: &ir::ty::Type, acc: &mut Vec<String>) {
    match ty {
        ir::ty::Type::GenericParam(g) => acc.push(g.name.clone()),
        ir::ty::Type::Union(v) | ir::ty::Type::Intersection(v) | ir::ty::Type::Tuple(v) => {
            v.iter().for_each(|t| collect_generic_param_names(t, acc));
        }
        ir::ty::Type::Slice(b) | ir::ty::Type::Variadic(b) => collect_generic_param_names(b, acc),
        ir::ty::Type::TypeReference(r) => {
            if let Some(args) = &r.generic_args {
                for a in args {
                    if let GenericArg::Type(t) = a {
                        collect_generic_param_names(t, acc);
                    }
                }
            }
        }
        ir::ty::Type::FunctionPointer(fp) => {
            for list in [&fp.inputs, &fp.outputs] {
                if let Some(params) = list {
                    for p in params {
                        if let IrParameter::Literal(lp) = p {
                            if let Some(t) = &lp.r#type {
                                collect_generic_param_names(t, acc);
                            }
                        }
                    }
                }
            }
        }
        ir::ty::Type::TypeOperator(op) => collect_generic_param_names(&op.r#type, acc),
        ir::ty::Type::Predicate(p) => {
            if let Some(t) = &p.r#type {
                collect_generic_param_names(t, acc);
            }
        }
        _ => {}
    }
}

fn type_expr_of(ty: &ir::ty::Type) -> TypeExpr {
    match ty {
        ir::ty::Type::TypeReference(r) => TypeExpr {
            name: r.identifier.clone(),
            args: r
                .generic_args
                .as_ref()
                .map(|args| {
                    args.iter()
                        .filter_map(|a| match a {
                            GenericArg::Type(t) => Some(type_expr_of(t)),
                            _ => None,
                        })
                        .collect()
                })
                .unwrap_or_default(),
        },
        ir::ty::Type::GenericParam(g) => TypeExpr {
            name: g.name.clone(),
            args: Vec::new(),
        },
        ir::ty::Type::Primitive(p) => TypeExpr {
            name: format!("{p:?}"),
            args: Vec::new(),
        },
        ir::ty::Type::Any => TypeExpr {
            name: "Any".into(),
            args: Vec::new(),
        },
        other => TypeExpr {
            name: format!("{other:?}"),
            args: Vec::new(),
        },
    }
}

fn trait_ref_of(ty: &ir::ty::Type) -> TraitRef {
    let expr = type_expr_of(ty);
    TraitRef {
        name: expr.name,
        args: expr.args,
    }
}

// ---------------------------------------------------------------------------
// Parameters
// ---------------------------------------------------------------------------

/// Lower a pyrefly [`Param`] into an IR value parameter, preserving:
/// - positional-only / keyword-only / `*args` / `**kwargs` distinction
/// - requiredness and default values
pub fn lower_param(param: &Param) -> IrParameter {
    fn opt_name<N: ToString>(n: &Option<N>) -> String {
        n.as_ref().map(|n| n.to_string()).unwrap_or_default()
    }

    let (name, ty, mut attrs, required) = match param {
        Param::PosOnly(name, ty, req) => (
            opt_name(name),
            ty,
            vec![ParameterAttribute::PositionalOnly],
            req,
        ),
        Param::Pos(name, ty, req) => (name.to_string(), ty, vec![], req),
        Param::Varargs(name, ty) => (
            opt_name(name),
            ty,
            vec![ParameterAttribute::Variadic],
            &Required::Required,
        ),
        Param::KwOnly(name, ty, req) => (
            name.to_string(),
            ty,
            vec![ParameterAttribute::KeywordOnly],
            req,
        ),
        Param::Kwargs(name, ty) => (
            opt_name(name),
            ty,
            vec![ParameterAttribute::KwVariadic],
            &Required::Required,
        ),
    };

    let (default_value, optional) = match required {
        Required::Required => (None, false),
        Required::Optional(dv) => (dv.as_ref().and_then(default_to_const_expr), true),
    };
    if optional {
        attrs.push(ParameterAttribute::Optional);
    }

    IrParameter::Literal(LiteralParameter {
        name,
        r#type: Some(lower_type(ty)),
        attributes: if attrs.is_empty() { None } else { Some(attrs) },
        default_value,
        description: None,
    })
}

/// Map a pyrefly optional-parameter default into an IR [`ConstExpr`].
pub fn default_to_const_expr(dv: &DefaultValue) -> Option<ConstExpr> {
    // Prefer the display string when present (floats etc. lose precision as types).
    if let Some(display) = &dv.display {
        return Some(ConstExpr::Var(display.clone()));
    }
    const_expr_from_type(&dv.ty)
}

/// Best-effort compile-time value extraction from a pyrefly type (literals).
pub fn const_expr_from_type(ty: &Type) -> Option<ConstExpr> {
    match ty {
        Type::Literal(lit) => match &lit.value {
            Lit::Int(i) => i.as_i64().map(ConstExpr::Int),
            Lit::Bool(b) => Some(ConstExpr::Bool(*b)),
            Lit::Str(s) => Some(ConstExpr::Str(s.to_string())),
            Lit::Bytes(b) => {
                // Represent bytes defaults as a Python-ish `b'...'` string form.
                Some(ConstExpr::Str(format!("b'{:?}'", b)))
            }
            Lit::Enum(e) => Some(ConstExpr::Var(format!(
                "{}.{}",
                e.class.name(),
                e.member
            ))),
        },
        Type::None => Some(ConstExpr::Var("None".into())),
        Type::ClassType(ct) if ct.class_object().is_builtin("bool") => {
            // Bare `bool` without a literal value — no recoverable constant.
            None
        }
        _ => {
            // Fall back to a stable display of the type as a Var name so the
            // default slot is still non-empty for consumers that only care
            // that *some* default exists. Prefer Display; Debug as last resort.
            let s = format!("{ty}");
            if s.is_empty() || s == "()" {
                None
            } else {
                Some(ConstExpr::Var(s))
            }
        }
    }
}

fn callable_param_types(params: &Params) -> Vec<IrParameter> {
    match params {
        Params::List(params) => params.items().iter().map(lower_param).collect(),
        _ => Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// Class / callable / tuple helpers
// ---------------------------------------------------------------------------

fn lower_class_type(ct: &ClassType) -> ir::ty::Type {
    let qname = strip_loc(&format!("{}", ct.qname()));
    let args: Vec<GenericArg> = ct
        .targs()
        .as_slice()
        .iter()
        .map(|t| GenericArg::Type(lower_type(t)))
        .collect();
    ir::ty::Type::TypeReference(TypeReference {
        identifier: qname,
        generic_args: if args.is_empty() { None } else { Some(args) },
    })
}

fn lower_function_type(func: &Function) -> ir::ty::Type {
    let inputs: Vec<IrParameter> = callable_param_types(&func.signature.params);
    let outputs = vec![IrParameter::Literal(LiteralParameter {
        name: String::new(),
        r#type: Some(lower_type(&func.signature.ret)),
        attributes: None,
        default_value: None,
        description: None,
    })];

    let mut attrs = Vec::new();
    if func.metadata.flags.is_async {
        attrs.push(Attribute::Async);
    }

    ir::ty::Type::FunctionPointer(FunctionPointer {
        inputs: Some(inputs),
        outputs: Some(outputs),
        attributes: if attrs.is_empty() { None } else { Some(attrs) },
    })
}

fn lower_callable_type(callable: &Callable) -> ir::ty::Type {
    let inputs: Vec<IrParameter> = callable_param_types(&callable.params);
    let outputs = vec![IrParameter::Literal(LiteralParameter {
        name: String::new(),
        r#type: Some(lower_type(&callable.ret)),
        attributes: None,
        default_value: None,
        description: None,
    })];

    ir::ty::Type::FunctionPointer(FunctionPointer {
        inputs: Some(inputs),
        outputs: Some(outputs),
        attributes: None,
    })
}

fn lower_bound_method(bm: &BoundMethodType) -> ir::ty::Type {
    match bm {
        BoundMethodType::Function(f) => lower_function_type(f),
        BoundMethodType::Forall(f) => lower_function_type(&f.body),
        BoundMethodType::Overload(o) => {
            let sigs: Vec<ir::ty::Type> =
                o.signatures.iter().map(|ot| lower_type(&ot.as_type())).collect();
            ir::ty::Type::Union(sigs)
        }
    }
}

fn lower_tuple_type(tuple: &Tuple) -> ir::ty::Type {
    match tuple {
        Tuple::Concrete(elements) => {
            ir::ty::Type::Tuple(elements.iter().map(lower_type).collect())
        }
        Tuple::Unbounded(element) => ir::ty::Type::Slice(Box::new(lower_type(element))),
        Tuple::Unpacked(unpacked) => {
            let (_prefix, middle, _suffix) = &**unpacked;
            ir::ty::Type::Union(vec![lower_type(middle)])
        }
    }
}

fn lower_typed_dict(td: &TypedDict) -> ir::ty::Type {
    match td {
        TypedDict::TypedDict(inner) => {
            let qname = strip_loc(&format!("{}", inner.qname()));
            let args: Vec<GenericArg> = inner
                .targs()
                .as_slice()
                .iter()
                .map(|t| GenericArg::Type(lower_type(t)))
                .collect();
            ir::ty::Type::TypeReference(TypeReference {
                identifier: qname,
                generic_args: if args.is_empty() { None } else { Some(args) },
            })
        }
        TypedDict::Anonymous(_) => ir::ty::Type::TypeReference(TypeReference {
            identifier: "TypedDict".into(),
            generic_args: None,
        }),
    }
}

/// Lower a pyrefly literal type, **preserving the value** as [`Type::Literal`].
fn lower_literal(lit: &pyrefly_types::literal::Literal) -> ir::ty::Type {
    match &lit.value {
        Lit::Int(i) => ir::ty::Type::Literal(LiteralValue {
            kind: LiteralKind::Number,
            value: i.to_string(),
        }),
        Lit::Bool(b) => ir::ty::Type::Literal(LiteralValue {
            kind: LiteralKind::Boolean,
            value: if *b { "True".into() } else { "False".into() },
        }),
        Lit::Str(s) => ir::ty::Type::Literal(LiteralValue {
            kind: LiteralKind::String,
            // Keep the unquoted content so consumers can re-quote as needed;
            // the source spelling is recoverable via Display of Lit when needed.
            value: s.to_string(),
        }),
        Lit::Bytes(bytes) => {
            // Bytes are not a first-class LiteralKind; surface as a string form.
            let mut escaped = String::from("b'");
            for &byte in bytes.iter() {
                match byte {
                    b'\t' => escaped.push_str("\\t"),
                    b'\n' => escaped.push_str("\\n"),
                    b'\r' => escaped.push_str("\\r"),
                    b'\\' => escaped.push_str("\\\\"),
                    b'\'' => escaped.push_str("\\'"),
                    0x20..=0x7e => escaped.push(byte as char),
                    _ => escaped.push_str(&format!("\\x{byte:02x}")),
                }
            }
            escaped.push('\'');
            ir::ty::Type::Literal(LiteralValue {
                kind: LiteralKind::String,
                value: escaped,
            })
        }
        Lit::Enum(enum_lit) => ir::ty::Type::TypeReference(TypeReference {
            identifier: format!(
                "{}.{}",
                strip_loc(&format!("{}", enum_lit.class.qname())),
                enum_lit.member
            ),
            generic_args: None,
        }),
    }
}

/// Lower a quantified type variable **always** as [`Type::GenericParam`],
/// never collapsing to its bound or `Any`. Bounds live on the enclosing
/// declaration's `generics` field (see [`lower_tparams`]).
fn lower_quantified(q: &Quantified) -> ir::ty::Type {
    ir::ty::Type::GenericParam(GenericParam {
        name: q.name.to_string(),
        kind: None,
    })
}

fn lower_type_alias(alias: &TypeAliasData) -> ir::ty::Type {
    match alias {
        TypeAliasData::Value(value) => lower_type(&value.as_type()),
        TypeAliasData::Ref(r#ref) => {
            let ident = format!("{}.{}", r#ref.module_name, r#ref.name);
            ir::ty::Type::TypeReference(TypeReference {
                identifier: ident,
                generic_args: r#ref.args.as_ref().map(|args| {
                    args.as_slice()
                        .iter()
                        .map(|t| GenericArg::Type(lower_type(t)))
                        .collect()
                }),
            })
        }
    }
}

