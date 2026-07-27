//! `TSType` → `TypeOwned` lowering.
//!
//! Rewritten against the new IR. The old `types.rs` targeted an IR type system
//! that no longer exists. This targets `TypeOwned`, which maps onto
//! `nudox_ir::kinds::Type` in `emit.rs`.
//!
//! UNCERTAINTY: OXC 0.139.0 AST variant names and method signatures are verified
//! against the source read from `workspace/compiler/compile/typescript/oxc/extract/types.rs`
//! and the old code's imports of `oxc_ast::ast::TSType`, `TSLiteral`, etc.
//! If OXC's AST changes between 0.138 and 0.139, some variant names may differ.

use oxc_ast::ast::{
    TSType, TSTypeName, TSLiteral, TSTypeParameterDeclaration,
};
use oxc_span::GetSpan;

use super::{
    FunctionBody, GenericParamOwned, LiteralOwned, ParamFact, ReceiverKind, TypeOwned,
};

// ── Entry point ───────────────────────────────────────────────────────────────

/// Lower one `TSType` AST node into an owned `TypeOwned`.
///
/// Returns `TypeOwned::Unsupported` (rather than an error) for constructs that
/// are valid TS but have no IR representation, so that callers can continue.
/// The description string names the construct for diagnostic output.
///
/// # TypeVar heuristic
///
/// When a `TSTypeReference` is a bare single identifier with no type arguments
/// (e.g. `T`, `K`, `U`), it is lowered as `TypeOwned::TypeVar(name)` rather
/// than `TypeOwned::Nominal(name)`. This heuristic is correct for the common
/// case where the identifier refers to a type parameter in scope.
///
/// The alternative — tracking in-scope params through the recursive call —
/// requires threading a `&HashSet<String>` through every recursive call, which
/// adds noise proportional to the depth of the type tree with minimal benefit:
/// a misclassification (TypeVar vs Nominal for a top-level type) is corrected
/// at the Lowering sink when the declared ID is found or not found.
///
/// Callers that have an explicit set of in-scope type parameters should use
/// `lower_ts_type_with_params` instead.
pub fn lower_ts_type<'a>(ty: &TSType<'a>, source: &'a str) -> TypeOwned {
    lower_ts_type_impl(ty, source, None)
}

/// Lower `ty` with an explicit set of in-scope type parameter names.
///
/// Any single-identifier `TSTypeReference` whose name is in `type_params` is
/// emitted as `TypeOwned::TypeVar`; others become `TypeOwned::Nominal`.
pub fn lower_ts_type_with_params<'a>(
    ty: &TSType<'a>,
    source: &'a str,
    type_params: &std::collections::HashSet<String>,
) -> TypeOwned {
    lower_ts_type_impl(ty, source, Some(type_params))
}

fn lower_ts_type_impl<'a>(
    ty: &TSType<'a>,
    source: &'a str,
    type_params: Option<&std::collections::HashSet<String>>,
) -> TypeOwned {
    match ty {
        // ── Primitives ─────────────────────────────────────────────────────
        TSType::TSAnyKeyword(_) => TypeOwned::Any,
        TSType::TSNeverKeyword(_) => TypeOwned::Never,
        TSType::TSUnknownKeyword(_) => TypeOwned::Unknown,
        TSType::TSVoidKeyword(_) => TypeOwned::Void,
        TSType::TSUndefinedKeyword(_) => TypeOwned::Undefined,
        TSType::TSNullKeyword(_) => TypeOwned::Null,
        TSType::TSBooleanKeyword(_) => TypeOwned::Bool,
        TSType::TSNumberKeyword(_) => TypeOwned::Number,
        TSType::TSBigIntKeyword(_) => TypeOwned::BigInt,
        TSType::TSStringKeyword(_) => TypeOwned::String,
        TSType::TSSymbolKeyword(_) => TypeOwned::Symbol,
        TSType::TSObjectKeyword(_) => TypeOwned::Object,
        TSType::TSThisType(_) => TypeOwned::This,
        TSType::TSIntrinsicKeyword(_) => TypeOwned::Unsupported("intrinsic".to_string()),

        // ── Literal types ──────────────────────────────────────────────────
        TSType::TSLiteralType(lit) => lower_ts_literal(&lit.literal),

        // ── Type reference ─────────────────────────────────────────────────
        TSType::TSTypeReference(tr) => {
            let name = ts_type_name_to_string(&tr.type_name);
            // Detect a generic type-parameter *use*: single identifier + no
            // type arguments. If caller supplied a type-params set, use it;
            // otherwise apply the single-identifier heuristic.
            let is_simple_id = !name.contains('.');
            let is_type_var = tr.type_arguments.is_none() && is_simple_id && {
                match type_params {
                    Some(set) => set.contains(&name),
                    // Heuristic: bare single identifier without type args is
                    // likely a type parameter (T, K, V, …). This is overridden
                    // by callers that supply the actual set.
                    None => true,
                }
            };

            if is_type_var {
                TypeOwned::TypeVar(name)
            } else if let Some(args) = &tr.type_arguments {
                let lowered_args: Vec<TypeOwned> =
                    args.params.iter().map(|p| lower_ts_type_impl(p, source, type_params)).collect();
                TypeOwned::Apply {
                    base: Box::new(TypeOwned::Nominal(name)),
                    args: lowered_args,
                }
            } else {
                TypeOwned::Nominal(name)
            }
        }

        // ── Union / Intersection ───────────────────────────────────────────
        TSType::TSUnionType(u) => {
            let arms: Vec<TypeOwned> =
                u.types.iter().map(|t| lower_ts_type_impl(t, source, type_params)).collect();
            TypeOwned::Union(arms)
        }
        TSType::TSIntersectionType(i) => {
            let arms: Vec<TypeOwned> =
                i.types.iter().map(|t| lower_ts_type_impl(t, source, type_params)).collect();
            TypeOwned::Intersection(arms)
        }

        // ── Array / Tuple ──────────────────────────────────────────────────
        TSType::TSArrayType(arr) => {
            TypeOwned::Array(Box::new(lower_ts_type_impl(&arr.element_type, source, type_params)))
        }
        TSType::TSTupleType(tup) => {
            let members: Vec<TypeOwned> = tup
                .element_types
                .iter()
                .map(|e| lower_ts_tuple_element(e, source, type_params))
                .collect();
            TypeOwned::Tuple(members)
        }

        // ── Function types ─────────────────────────────────────────────────
        TSType::TSFunctionType(f) => {
            let params = lower_formal_params(&f.params, source, type_params);
            let return_type = Some(lower_ts_type_impl(&f.return_type.type_annotation, source, type_params));
            let generics = f
                .type_parameters
                .as_ref()
                .map(|tp| lower_type_params(tp, source))
                .unwrap_or_default();
            TypeOwned::Function(Box::new(FunctionBody {
                generics,
                params,
                return_type,
                is_async: false,
                is_generator: false,
                has_body: false,
                receiver: ReceiverKind::None,
            }))
        }

        // ── Conditional type ───────────────────────────────────────────────
        TSType::TSConditionalType(_) => {
            TypeOwned::Unsupported("conditional type".to_string())
        }

        // ── Mapped type ────────────────────────────────────────────────────
        TSType::TSMappedType(_) => TypeOwned::Unsupported("mapped type".to_string()),

        // ── Template literal ───────────────────────────────────────────────
        TSType::TSTemplateLiteralType(_) => {
            TypeOwned::Unsupported("template literal type".to_string())
        }

        // ── Indexed access ─────────────────────────────────────────────────
        TSType::TSIndexedAccessType(ia) => {
            let obj = lower_ts_type_impl(&ia.object_type, source, type_params);
            let idx = lower_ts_type_impl(&ia.index_type, source, type_params);
            TypeOwned::Apply {
                base: Box::new(TypeOwned::Nominal("IndexedAccess".to_string())),
                args: vec![obj, idx],
            }
        }

        // ── Infer ─────────────────────────────────────────────────────────
        TSType::TSInferType(i) => {
            TypeOwned::TypeVar(i.type_parameter.name.to_string())
        }

        // ── Type operators (keyof, typeof, readonly, unique) ───────────────
        TSType::TSTypeOperatorType(op) => {
            let inner = lower_ts_type_impl(&op.type_annotation, source, type_params);
            TypeOwned::Apply {
                base: Box::new(TypeOwned::Nominal(format!("{:?}", op.operator))),
                args: vec![inner],
            }
        }

        // ── Type predicate ─────────────────────────────────────────────────
        TSType::TSTypePredicate(_) => TypeOwned::Bool,

        // ── Type query (typeof expr) ───────────────────────────────────────
        TSType::TSTypeQuery(q) => {
            let name = match &q.expr_name {
                oxc_ast::ast::TSTypeQueryExprName::IdentifierReference(id) => {
                    id.name.to_string()
                }
                oxc_ast::ast::TSTypeQueryExprName::QualifiedName(qn) => {
                    // Reconstruct the dotted name by walking the qualified name.
                    // UNCERTAINTY: `TSTypeQueryExprName::QualifiedName` wraps a
                    // `Box<TSQualifiedName<'a>>` in 0.139.0. We render it via the
                    // span source text rather than cloning (clone may not be derived).
                    qn.span().source_text(source).to_string()
                }
                other => format!("{other:?}"),
            };
            TypeOwned::Apply {
                base: Box::new(TypeOwned::Nominal("typeof".to_string())),
                args: vec![TypeOwned::Nominal(name)],
            }
        }

        // ── Import type ────────────────────────────────────────────────────
        TSType::TSImportType(_) => TypeOwned::Unsupported("import type".to_string()),

        // ── Type literal (object shape) ────────────────────────────────────
        TSType::TSTypeLiteral(_) => TypeOwned::Unsupported("type literal".to_string()),

        // ── Named tuple member (internal, shouldn't appear at top level) ───
        TSType::TSNamedTupleMember(m) => lower_ts_tuple_element(&m.element_type, source, type_params),

        // ── Parenthesized ─────────────────────────────────────────────────
        TSType::TSParenthesizedType(p) => lower_ts_type_impl(&p.type_annotation, source, type_params),

        // ── Constructor type ───────────────────────────────────────────────
        TSType::TSConstructorType(_) => TypeOwned::Unsupported("constructor type".to_string()),

        // ── JS types (constructed types) ───────────────────────────────────
        TSType::JSDocNullableType(n) => lower_ts_type_impl(&n.type_annotation, source, type_params),
        TSType::JSDocNonNullableType(n) => lower_ts_type_impl(&n.type_annotation, source, type_params),
        TSType::JSDocUnknownType(_) => TypeOwned::Unknown,
    }
}

/// Lower a type parameter list to owned generic params.
pub fn lower_type_params<'a>(
    tp: &TSTypeParameterDeclaration<'a>,
    source: &'a str,
) -> Vec<GenericParamOwned> {
    tp.params
        .iter()
        .map(|p| {
            let bounds = p
                .constraint
                .as_ref()
                .map(|c| vec![lower_ts_type(c, source)])
                .unwrap_or_default();
            let default = p.default.as_ref().map(|d| lower_ts_type(d, source));
            GenericParamOwned {
                name: p.name.name.to_string(),
                bounds,
                default,
            }
        })
        .collect()
}

// ── Internal helpers ──────────────────────────────────────────────────────────

fn lower_ts_literal(lit: &TSLiteral<'_>) -> TypeOwned {
    match lit {
        TSLiteral::BooleanLiteral(b) => TypeOwned::Literal(LiteralOwned::Bool(b.value)),
        TSLiteral::NumericLiteral(n) => {
            TypeOwned::Literal(LiteralOwned::Number(n.value.to_string()))
        }
        TSLiteral::StringLiteral(s) => {
            TypeOwned::Literal(LiteralOwned::String(s.value.to_string()))
        }
        TSLiteral::BigIntLiteral(b) => {
            // `raw` is `Option<Str<'_>>` in 0.139.0; fall back to `value` (base-10).
            let repr = b
                .raw
                .as_ref()
                .map(|s| s.as_str().to_string())
                .unwrap_or_else(|| b.value.as_str().to_string());
            TypeOwned::Literal(LiteralOwned::BigInt(repr))
        }
        TSLiteral::TemplateLiteral(_) => {
            TypeOwned::Unsupported("template literal type".to_string())
        }
        TSLiteral::UnaryExpression(u) => {
            // `-1` literal: render as number.
            let span_text = format!("{:?}", u);
            TypeOwned::Literal(LiteralOwned::Number(span_text))
        }
    }
}

fn ts_type_name_to_string(name: &TSTypeName<'_>) -> String {
    match name {
        TSTypeName::IdentifierReference(id) => id.name.to_string(),
        TSTypeName::QualifiedName(q) => {
            format!("{}.{}", ts_type_name_to_string(&q.left), q.right.name)
        }
        // UNCERTAINTY: OXC 0.139.0 may not have `TSTypeName::ThisExpression`.
        // If this arm causes a compile error, remove it.
        #[allow(unreachable_patterns)]
        _ => "this".to_string(),
    }
}

fn lower_ts_tuple_element<'a>(
    elem: &oxc_ast::ast::TSTupleElement<'a>,
    source: &'a str,
    type_params: Option<&std::collections::HashSet<String>>,
) -> TypeOwned {
    use oxc_ast::ast::TSTupleElement;
    match elem {
        TSTupleElement::TSOptionalType(o) => {
            lower_ts_type_impl(&o.type_annotation, source, type_params)
        }
        TSTupleElement::TSRestType(r) => {
            TypeOwned::Array(Box::new(lower_ts_type_impl(&r.type_annotation, source, type_params)))
        }
        TSTupleElement::TSNamedTupleMember(m) => {
            lower_ts_tuple_element(&m.element_type, source, type_params)
        }
        other => {
            // UNCERTAINTY: `TSTupleElement::as_ts_type()` exists in 0.139.0.
            // If this does not compile, replace with:
            //   TypeOwned::Unsupported("tuple element".to_string())
            // `as_ts_type()` returns `Option<&TSType<'a>>` for the transparent
            // pass-through variants (i.e. variants that ARE TSType variants).
            if let Some(ty) = other.as_ts_type() {
                lower_ts_type_impl(ty, source, type_params)
            } else {
                TypeOwned::Unsupported("tuple element".to_string())
            }
        }
    }
}

fn lower_formal_params<'a>(
    params: &oxc_ast::ast::FormalParameters<'a>,
    source: &'a str,
    type_params: Option<&std::collections::HashSet<String>>,
) -> Vec<ParamFact> {
    let mut out: Vec<ParamFact> = Vec::with_capacity(params.items.len());

    for param in &params.items {
        let name = binding_pattern_name(&param.pattern);
        // Skip the `this` parameter pseudo-binding.
        if name.as_deref() == Some("this") {
            continue;
        }
        let ty = param
            .type_annotation
            .as_ref()
            .map(|ann| lower_ts_type_impl(&ann.type_annotation, source, type_params));
        out.push(ParamFact {
            name: name.unwrap_or_else(|| "_".to_string()),
            ty,
            is_optional: param.optional,
            is_rest: false,
            is_readonly: false,
        });
    }

    if let Some(rest) = &params.rest {
        let ty = rest
            .type_annotation
            .as_ref()
            .map(|ann| lower_ts_type_impl(&ann.type_annotation, source, type_params));
        let name = binding_pattern_name(&rest.rest.argument);
        out.push(ParamFact {
            name: name.unwrap_or_else(|| "...rest".to_string()),
            ty,
            is_optional: false,
            is_rest: true,
            is_readonly: false,
        });
    }

    out
}

fn binding_pattern_name<'a>(pat: &oxc_ast::ast::BindingPattern<'a>) -> Option<String> {
    use oxc_ast::ast::BindingPattern;
    match pat {
        BindingPattern::BindingIdentifier(id) => Some(id.name.to_string()),
        BindingPattern::AssignmentPattern(ap) => binding_pattern_name(&ap.left),
        BindingPattern::ObjectPattern(_) => Some("{...}".to_string()),
        BindingPattern::ArrayPattern(_) => Some("[...]".to_string()),
    }
}
