//! Lowering Pyrefly's `Type` into `ir::ty::Type`.
//!
//! Maps pyrefly's rich Python type algebra (Union, Intersect, Literal,
//! Callable, TypedDict, TypeVar, ParamSpec, …) to the language-agnostic
//! IR type representation.

use pyrefly_types::callable::{Callable, FuncMetadata, Function, FunctionKind, Param, ParamList, Params};
use pyrefly_types::callable_residual::CallableResidualKind;
use pyrefly_types::class::ClassType;
use pyrefly_types::literal::Lit;
use pyrefly_types::quantified::{Quantified, QuantifiedKind};
use pyrefly_types::tuple::Tuple;
use pyrefly_types::type_alias::TypeAliasData;
use pyrefly_types::type_var::Restriction;
use pyrefly_types::typed_dict::TypedDict;
use pyrefly_types::types::{BoundMethodType, Forallable, NeverStyle, Type, Union};

use ir::function::Attribute;
use ir::generics::GenericArg;
use ir::parameter::{LiteralParameter, Parameter as IrParameter, ParameterAttribute};
use ir::primitives::{Primitive, Width};
use ir::ty::{GenericParam, FunctionPointer, TypeReference};

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
                identifier: format!("typing.Type"),
                generic_args: Some(vec![GenericArg::Type(
                    ir::ty::Type::TypeReference(TypeReference {
                        identifier: qname,
                        generic_args: None,
                    }),
                )]),
            })
        }
        Type::Function(func) => lower_function_type(func),
        Type::Callable(callable) => lower_callable_type(callable),
        Type::BoundMethod(bm) => lower_bound_method(&bm.func),
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
        Type::TypeGuard(inner) => lower_type(inner),
        Type::TypeIs(inner) => lower_type(inner),
        Type::Annotated(inner, _) => lower_type(inner),
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
        Type::SelfType(ct) => ir::ty::Type::SelfType,
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
            inputs: Some(params.items().iter().map(lower_param_to_ir).collect()),
            outputs: None,
            attributes: None,
        }),
        Type::Sentinel(_) => ir::ty::Type::Any,
    }
}

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

fn lower_literal(lit: &pyrefly_types::literal::Literal) -> ir::ty::Type {
    match &lit.value {
        Lit::Int(i) => ir::ty::Type::Primitive(Primitive::Int(Width::W64)),
        Lit::Bool(b) => ir::ty::Type::Primitive(Primitive::Bool),
        Lit::Str(_) => ir::ty::Type::Primitive(Primitive::String),
        Lit::Bytes(_) => ir::ty::Type::Primitive(Primitive::Bytes),
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

fn lower_quantified(q: &Quantified) -> ir::ty::Type {
    match &q.restriction {
        Restriction::Bound(bound) => lower_type(bound),
        Restriction::Constraints(constraints) => {
            ir::ty::Type::Union(constraints.iter().map(lower_type).collect())
        }
        Restriction::Unrestricted => ir::ty::Type::Any,
    }
}

fn lower_type_alias(alias: &TypeAliasData) -> ir::ty::Type {
    match alias {
        TypeAliasData::Value(value) => lower_type(&value.as_type()),
        TypeAliasData::Ref(r#ref) => {
            let mut ident = format!("{}.{}", r#ref.module_name, r#ref.name);
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

fn lower_param_to_ir(param: &Param) -> IrParameter {
    fn opt_name<N: ToString>(n: &Option<N>) -> String {
        n.as_ref().map(|n| n.to_string()).unwrap_or_default()
    }
    let (name, ty, attrs) = match param {
        Param::PosOnly(name, ty, _) => (opt_name(name), ty, vec![]),
        Param::Pos(name, ty, _) => (name.to_string(), ty, vec![]),
        Param::Varargs(name, ty) => (
            opt_name(name),
            ty,
            vec![ParameterAttribute::Variadic],
        ),
        Param::KwOnly(name, ty, _) => (name.to_string(), ty, vec![]),
        Param::Kwargs(name, ty) => (
            opt_name(name),
            ty,
            vec![ParameterAttribute::Variadic],
        ),
    };

    IrParameter::Literal(LiteralParameter {
        name,
        r#type: Some(lower_type(ty)),
        attributes: if attrs.is_empty() { None } else { Some(attrs) },
        default_value: None,
        description: None,
    })
}

fn callable_param_types(params: &Params) -> Vec<IrParameter> {
    match params {
        Params::List(params) => params.items().iter().map(lower_param_to_ir).collect(),
        _ => Vec::new(),
    }
}
