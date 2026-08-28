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

use oxc_ast::ast::{TSLiteral, TSType, TSTypeName, TSTypeParameterDeclaration};
use oxc_span::GetSpan;

use super::{
    AnonFieldOwned, FunctionBody, GenericParamOwned, LiteralOwned, ParamFact, ReceiverKind,
    TemplatePart, TypeOwned,
};

// ── Entry point ───────────────────────────────────────────────────────────────

/// Lower one `TSType` AST node into an owned `TypeOwned`.
///
/// Returns `TypeOwned::Unsupported` (rather than an error) for constructs that
/// are valid TS but have no IR representation, so that callers can continue.
/// The description string names the construct for diagnostic output.
///
/// # TypeVar vs Nominal — default is Nominal
///
/// A bare single-identifier `TSTypeReference` (`User`, `T`, `Foo`) is only
/// classified `TypeOwned::TypeVar` when the caller supplies an explicit
/// in-scope type-parameter set (via [`lower_ts_type_with_params`]) *and* the
/// name is a member of it. With no set supplied — this function, `None` — the
/// identifier defaults to `TypeOwned::Nominal`.
///
/// This is deliberately the opposite of the old heuristic ("no type args and
/// no set ⇒ TypeVar"), which misclassified *every* named type reference —
/// `owner: User`, `g: Foo`, a function parameter, an alias target — as a
/// generic type parameter the moment no type-param set was in scope, which in
/// practice was almost always (most call sites never threaded one). A
/// reference to a declared interface/class/alias is far more common in real
/// source than a stray, unresolvable type variable, so Nominal is the safer
/// default: an unrecognized Nominal degrades to `UnresolvedExternal` at the
/// `Lowering` sink (see `emit.rs::lower_nominal`), a named, honest gap. A
/// misclassified TypeVar silently discards the reference identity with no
/// gap recorded at all — that asymmetry is why the default flipped.
///
/// Callers that have an explicit set of in-scope type parameters — the
/// enclosing function/class/interface/alias's own declared generics — should
/// use [`lower_ts_type_with_params`] instead, so a genuine use of `T` in
/// `function f<T>(x: T)` still lowers to `TypeOwned::TypeVar("T")`.
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
        TSType::TSUnknownKeyword(_) | TSType::JSDocUnknownType(_) => TypeOwned::Unknown,
        TSType::TSVoidKeyword(_) => TypeOwned::Void,
        TSType::TSUndefinedKeyword(_) => TypeOwned::Undefined,
        TSType::TSNullKeyword(_) => TypeOwned::Null,
        TSType::TSBooleanKeyword(_) | TSType::TSTypePredicate(_) => TypeOwned::Bool,
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
            // Only an explicit, supplied in-scope set can classify this as a
            // TypeVar; no set (`None`) means "no generics known here" and
            // defaults to Nominal — see this module's doc comment above.
            let is_type_var = tr.type_arguments.is_none()
                && is_simple_id
                && type_params.is_some_and(|set| set.contains(&name));

            if is_type_var {
                TypeOwned::TypeVar(name)
            } else if let Some(args) = &tr.type_arguments {
                let lowered_args: Vec<TypeOwned> = args
                    .params
                    .iter()
                    .map(|p| lower_ts_type_impl(p, source, type_params))
                    .collect();
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
            let arms: Vec<TypeOwned> = u
                .types
                .iter()
                .map(|t| lower_ts_type_impl(t, source, type_params))
                .collect();
            TypeOwned::Union(arms)
        }
        TSType::TSIntersectionType(i) => {
            let arms: Vec<TypeOwned> = i
                .types
                .iter()
                .map(|t| lower_ts_type_impl(t, source, type_params))
                .collect();
            TypeOwned::Intersection(arms)
        }

        // ── Array / Tuple ──────────────────────────────────────────────────
        TSType::TSArrayType(arr) => TypeOwned::Array(Box::new(lower_ts_type_impl(
            &arr.element_type,
            source,
            type_params,
        ))),
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
            let return_type = Some(lower_ts_type_impl(
                &f.return_type.type_annotation,
                source,
                type_params,
            ));
            let generics = f
                .type_parameters
                .as_ref()
                .map(|tp| {
                    // Union the caller's in-scope set with this function
                    // type's own `<...>` list before lowering its bounds/
                    // defaults, same as every named-declaration call site of
                    // `lower_type_params` — so a bound referencing a sibling
                    // param (`<T, U extends T>(x: U) => void`) classifies as
                    // `TypeVar`, and an enclosing generic (e.g. a method's
                    // own `<T>` wrapping a function-typed parameter) stays
                    // visible too.
                    let mut fn_type_params = type_params.cloned().unwrap_or_default();
                    fn_type_params.extend(tp.params.iter().map(|p| p.name.name.to_string()));
                    lower_type_params(tp, source, &fn_type_params)
                })
                .unwrap_or_default();
            let span = f.span();
            TypeOwned::Function(Box::new(FunctionBody {
                generics,
                params,
                return_type,
                is_async: false,
                is_generator: false,
                has_body: false,
                receiver: ReceiverKind::None,
                span_start: span.start,
                span_end: span.end,
            }))
        }

        // ── Conditional type ───────────────────────────────────────────────
        // `infer U` inside `extends_type` introduces a type parameter that
        // TypeScript scopes to the true branch only (`then_ty`) — `type
        // ElementOf<T> = T extends Array<infer U> ? U : never` binds `U` for
        // the `? U` arm, not for `check_type`/`extends_type` themselves (a
        // direct `infer X` there already lowers straight to `TypeVar` via
        // the `TSInferType` arm below, independent of any threaded set) and
        // not for `else_ty` (TypeScript never binds an inferred name there).
        // `collect_infer_names` walks `extends_type` to find every such
        // name — it can nest arbitrarily deep, e.g. `Array<infer U>` — and
        // unions it into the set threaded into `then_ty` alone. Before this,
        // `then_ty` reused the caller's set unchanged, so a bare `U` there
        // fell through to `TypeOwned::Nominal("U")` and then to a sink-less
        // `UnresolvedExternal` at `emit.rs`, even though `U` is exactly the
        // kind of in-scope generic name `lower_ts_type_with_params` exists
        // to classify as `TypeVar`.
        TSType::TSConditionalType(c) => {
            let mut then_params = type_params.cloned().unwrap_or_default();
            collect_infer_names(&c.extends_type, &mut then_params);
            TypeOwned::Conditional {
                check: Box::new(lower_ts_type_impl(&c.check_type, source, type_params)),
                extends_ty: Box::new(lower_ts_type_impl(&c.extends_type, source, type_params)),
                then_ty: Box::new(lower_ts_type_impl(&c.true_type, source, Some(&then_params))),
                else_ty: Box::new(lower_ts_type_impl(&c.false_type, source, type_params)),
            }
        }

        // ── Mapped type ────────────────────────────────────────────────────
        TSType::TSMappedType(m) => {
            use nudox_ir::kinds::ty::MappedModifier;
            use oxc_ast::ast::TSMappedTypeModifierOperator;
            let key_var = m.key.name.to_string();
            let source_ty = Box::new(lower_ts_type_impl(&m.constraint, source, type_params));
            let value_ty = m.type_annotation.as_ref().map_or_else(
                || Box::new(TypeOwned::Any),
                |v| Box::new(lower_ts_type_impl(v, source, type_params)),
            );
            let readonly = match &m.readonly {
                Some(TSMappedTypeModifierOperator::True | TSMappedTypeModifierOperator::Plus) => {
                    MappedModifier::Add
                }
                Some(TSMappedTypeModifierOperator::Minus) => MappedModifier::Remove,
                None => MappedModifier::Absent,
            };
            let optional = match &m.optional {
                Some(TSMappedTypeModifierOperator::True | TSMappedTypeModifierOperator::Plus) => {
                    MappedModifier::Add
                }
                Some(TSMappedTypeModifierOperator::Minus) => MappedModifier::Remove,
                None => MappedModifier::Absent,
            };
            TypeOwned::Mapped {
                key_var,
                source: source_ty,
                value: value_ty,
                readonly,
                optional,
            }
        }

        // ── Template literal ───────────────────────────────────────────────
        TSType::TSTemplateLiteralType(tl) => {
            let mut parts: Vec<TemplatePart> = Vec::new();
            // TSTemplateLiteralType has `quasis: Vec<TemplateElement>` and `types: Vec<TSType>`
            // They alternate: quasi[0], type[0], quasi[1], type[1], ..., quasi[n]
            for (i, quasi) in tl.quasis.iter().enumerate() {
                let s = quasi.value.cooked.as_ref().map_or_else(
                    || quasi.value.raw.as_str().to_string(),
                    |s| s.as_str().to_string(),
                );
                if !s.is_empty() {
                    parts.push(TemplatePart::Literal(s));
                }
                if let Some(ty) = tl.types.get(i) {
                    parts.push(TemplatePart::Interpolated(Box::new(lower_ts_type_impl(
                        ty,
                        source,
                        type_params,
                    ))));
                }
            }
            TypeOwned::TemplateLiteral(parts)
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
        TSType::TSInferType(i) => TypeOwned::TypeVar(i.type_parameter.name.to_string()),

        // ── Type operators (keyof, typeof, readonly, unique) ───────────────
        TSType::TSTypeOperatorType(op) => {
            let inner = lower_ts_type_impl(&op.type_annotation, source, type_params);
            TypeOwned::Apply {
                base: Box::new(TypeOwned::Nominal(format!("{:?}", op.operator))),
                args: vec![inner],
            }
        }

        // ── Type query (typeof expr) ───────────────────────────────────────
        TSType::TSTypeQuery(q) => {
            let name = match &q.expr_name {
                oxc_ast::ast::TSTypeQueryExprName::IdentifierReference(id) => id.name.to_string(),
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
        // `import("mod").Foo` or `import("mod")` — a *dynamic* import type,
        // distinct from a static `import` statement (tracked in
        // `ImportFact`): the module specifier lives inline here, not in the
        // module's import table.
        //
        // This used to collapse straight to `TypeOwned::Nominal(name)`,
        // where `name` was *either* the qualifier (`"Foo"`) *or* the module
        // specifier (`imp.source.value`) — never both. The moment a
        // qualifier was present, the module specifier was discarded
        // entirely, so `import("pkg").Foo` and a same-named local `Foo`
        // became indistinguishable strings by the time `emit.rs` saw them,
        // and neither `lower_nominal`'s import-table lookup nor a
        // same-module declaration lookup had any specifier left to resolve
        // against — an unconditional `UnresolvedExternal`. `DynamicImport`
        // keeps the specifier and the qualifier as separate fields so
        // `emit.rs::lower_dynamic_import` can build the same npm
        // `ForeignKey` a static bare-specifier import gets.
        TSType::TSImportType(imp) => {
            let module_request = imp.source.value.as_str().to_string();
            let member = imp.qualifier.as_ref().map(import_type_qualifier_to_string);
            let base = TypeOwned::DynamicImport {
                module_request,
                member,
            };
            if let Some(args) = &imp.type_arguments {
                let lowered_args: Vec<TypeOwned> = args
                    .params
                    .iter()
                    .map(|p| lower_ts_type_impl(p, source, type_params))
                    .collect();
                TypeOwned::Apply {
                    base: Box::new(base),
                    args: lowered_args,
                }
            } else {
                base
            }
        }

        // ── Type literal (object shape) ────────────────────────────────────
        TSType::TSTypeLiteral(lit) => {
            use oxc_ast::ast::TSSignature;
            let mut members: Vec<AnonFieldOwned> = Vec::new();
            for member in &lit.members {
                match member {
                    TSSignature::TSPropertySignature(p) => {
                        let name = p
                            .key
                            .static_name()
                            .map_or_else(|| "__computed".to_string(), |s| s.to_string());
                        let ty = p.type_annotation.as_ref().map_or(TypeOwned::Any, |a| {
                            lower_ts_type_impl(&a.type_annotation, source, type_params)
                        });
                        members.push(AnonFieldOwned {
                            name,
                            ty,
                            optional: p.optional,
                            readonly: p.readonly,
                        });
                    }
                    TSSignature::TSMethodSignature(m) => {
                        let name = m
                            .key
                            .static_name()
                            .map_or_else(|| "__method".to_string(), |s| s.to_string());
                        let params: Vec<TypeOwned> = m
                            .params
                            .items
                            .iter()
                            .map(|p| {
                                p.type_annotation.as_ref().map_or(TypeOwned::Any, |a| {
                                    lower_ts_type_impl(&a.type_annotation, source, type_params)
                                })
                            })
                            .collect();
                        let ret = m
                            .return_type
                            .as_ref()
                            .map(|r| lower_ts_type_impl(&r.type_annotation, source, type_params));
                        // Anonymous method member inside an object-type literal
                        // (`{ foo(x: number): void }` as a type annotation, not
                        // a declaration): the per-parameter type is all that
                        // survived the earlier `params` map (it discarded each
                        // `FormalParameter` node), so there is no independent
                        // per-parameter span here. The whole `TSMethodSignature`
                        // node's span (`m.span()`) is used for both the
                        // synthetic `FunctionBody` and each synthetic
                        // `ParamFact` it wraps — real bytes that do contain the
                        // parameter, not a precise sub-span of it, and neither
                        // is a declared IR entry needing identity (this feeds
                        // `AnonFieldOwned`, never `emit.rs`'s `declare()`).
                        let span = m.span();
                        members.push(AnonFieldOwned {
                            name,
                            ty: TypeOwned::Function(Box::new(FunctionBody {
                                generics: vec![],
                                params: params
                                    .into_iter()
                                    .map(|ty| ParamFact {
                                        name: "_".to_string(),
                                        ty: Some(ty),
                                        is_optional: false,
                                        is_rest: false,
                                        is_readonly: false,
                                        span_start: span.start,
                                        span_end: span.end,
                                    })
                                    .collect(),
                                return_type: ret,
                                is_async: false,
                                is_generator: false,
                                has_body: false,
                                receiver: ReceiverKind::None,
                                span_start: span.start,
                                span_end: span.end,
                            })),
                            optional: m.optional,
                            readonly: false,
                        });
                    }
                    // Index signatures, call signatures, construct signatures — skip.
                    _ => {}
                }
            }
            TypeOwned::ObjectLiteral(members)
        }

        // ── Named tuple member (internal, shouldn't appear at top level) ───
        TSType::TSNamedTupleMember(m) => {
            lower_ts_tuple_element(&m.element_type, source, type_params)
        }

        // ── Parenthesized ─────────────────────────────────────────────────
        TSType::TSParenthesizedType(p) => {
            lower_ts_type_impl(&p.type_annotation, source, type_params)
        }

        // ── Constructor type ───────────────────────────────────────────────
        // `new () => T` — lowered to a function type (item 8).
        // The IR's `TypeOwned::Function` slot exists; using Unsupported here
        // would be the exact bug class we are eliminating.
        TSType::TSConstructorType(c) => {
            let params = lower_formal_params(&c.params, source, type_params);
            let return_type = Some(lower_ts_type_impl(
                &c.return_type.type_annotation,
                source,
                type_params,
            ));
            let generics = c
                .type_parameters
                .as_ref()
                .map(|tp| {
                    // See the identical union in the `TSFunctionType` arm above.
                    let mut ctor_type_params = type_params.cloned().unwrap_or_default();
                    ctor_type_params.extend(tp.params.iter().map(|p| p.name.name.to_string()));
                    lower_type_params(tp, source, &ctor_type_params)
                })
                .unwrap_or_default();
            let span = c.span();
            TypeOwned::Function(Box::new(FunctionBody {
                generics,
                params,
                return_type,
                is_async: false,
                is_generator: false,
                has_body: false,
                receiver: ReceiverKind::None,
                span_start: span.start,
                span_end: span.end,
            }))
        }

        // ── JS types (constructed types) ───────────────────────────────────
        TSType::JSDocNullableType(n) => lower_ts_type_impl(&n.type_annotation, source, type_params),
        TSType::JSDocNonNullableType(n) => {
            lower_ts_type_impl(&n.type_annotation, source, type_params)
        }
    }
}

/// Lower a type parameter list to owned generic params.
///
/// OXC 0.139.0 exposes TypeScript 4.7+ variance modifiers via
/// `TSTypeParameter::r#in` and `TSTypeParameter::out`.  We wire them into
/// `GenericParamOwned::variance` so `emit.rs` can forward them to
/// `GenericParam::Type::variance`.
///
/// `type_params` is the *full* in-scope generic-parameter set for this
/// declaration — every caller below already builds this union (its own
/// `<...>` list plus any outer enclosing scope) before calling this
/// function, precisely so a bound or default can reference a **sibling**
/// type parameter: `function f<T, U extends T = T>(x: U)` needs `U`'s own
/// bound and default to classify `T` as `TypeOwned::TypeVar`, not a nominal
/// reference to an undeclared type named `T`. Bounds/defaults used to lower
/// through the plain `lower_ts_type` (no type-params set at all), so this
/// was the one place in a declaration's own generic list that never learned
/// about its own type parameters even though every *use site* of them
/// (params, return type, property types, …) already did.
pub fn lower_type_params<'a>(
    tp: &TSTypeParameterDeclaration<'a>,
    source: &'a str,
    type_params: &std::collections::HashSet<String>,
) -> Vec<GenericParamOwned> {
    tp.params
        .iter()
        .map(|p| {
            let bounds = p
                .constraint
                .as_ref()
                .map(|c| vec![lower_ts_type_with_params(c, source, type_params)])
                .unwrap_or_default();
            let default = p
                .default
                .as_ref()
                .map(|d| lower_ts_type_with_params(d, source, type_params));
            // TS 4.7+ `in`/`out` variance annotations.
            // OXC: `p.r#in == true` → contravariant (`in T` means the type is
            // consumed / write-only), `p.out == true` → covariant (`out T` means
            // the type is produced / read-only).
            // When both or neither are set: None (no explicit annotation).
            let variance = match (p.r#in, p.out) {
                (true, false) => Some(nudox_ir::kinds::ty::Variance::Contravariant),
                (false, true) => Some(nudox_ir::kinds::ty::Variance::Covariant),
                _ => None,
            };
            GenericParamOwned {
                name: p.name.name.to_string(),
                bounds,
                default,
                variance,
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
                .map_or_else(|| b.value.as_str().to_string(), |s| s.as_str().to_string());
            TypeOwned::Literal(LiteralOwned::BigInt(repr))
        }
        // A template literal inside `TSLiteralType` — e.g. `` type X = `hello` ``.
        // Distinct from `TSType::TSTemplateLiteralType` (handled above), which is
        // the interpolating form `` `a-${T}` ``.
        //
        // A no-substitution template IS a template literal type with a single
        // fixed span, so it lowers to `TypeOwned::TemplateLiteral`. When
        // `expressions` is non-empty the interpolations are *value* expressions,
        // not types — that is not valid in a type position and has no `Type`
        // representation, so it stays `Unsupported` naming the construct.
        TSLiteral::TemplateLiteral(tl) => {
            if tl.expressions.is_empty() {
                let text = tl
                    .quasis
                    .iter()
                    .map(|q| {
                        q.value.cooked.as_ref().map_or_else(
                            || q.value.raw.to_string(),
                            std::string::ToString::to_string,
                        )
                    })
                    .collect::<String>();
                TypeOwned::TemplateLiteral(vec![TemplatePart::Literal(text)])
            } else {
                TypeOwned::Unsupported(
                    "template literal with value interpolation in type position".to_string(),
                )
            }
        }
        TSLiteral::UnaryExpression(u) => {
            // `-1` literal: render as number.
            let span_text = format!("{u:?}");
            TypeOwned::Literal(LiteralOwned::Number(span_text))
        }
    }
}

/// Collect every name bound by an `infer` clause reachable from `ty`,
/// recursing through the structural type-constructors an `infer` can
/// realistically nest inside: generic type arguments (`Array<infer U>`),
/// unions/intersections (`infer U | string`), arrays/tuples, parenthesized
/// types, type operators (`keyof infer U`), indexed access, a function/
/// constructor type's return position, and a nested conditional's own
/// check/extends arms.
///
/// Used only to extend the type-parameter set `TSConditionalType` lowering
/// threads into its true-branch (`then_ty`) — see that arm above for why
/// `infer`'s TypeScript scoping rule (bound in the true branch only) is what
/// this feeds.
fn collect_infer_names(ty: &TSType<'_>, out: &mut std::collections::HashSet<String>) {
    match ty {
        TSType::TSInferType(i) => {
            out.insert(i.type_parameter.name.to_string());
        }
        TSType::TSTypeReference(tr) => {
            if let Some(args) = &tr.type_arguments {
                for p in &args.params {
                    collect_infer_names(p, out);
                }
            }
        }
        TSType::TSUnionType(u) => {
            for t in &u.types {
                collect_infer_names(t, out);
            }
        }
        TSType::TSIntersectionType(i) => {
            for t in &i.types {
                collect_infer_names(t, out);
            }
        }
        TSType::TSArrayType(a) => collect_infer_names(&a.element_type, out),
        TSType::TSTupleType(t) => {
            for e in &t.element_types {
                if let Some(inner) = e.as_ts_type() {
                    collect_infer_names(inner, out);
                }
            }
        }
        TSType::TSParenthesizedType(p) => collect_infer_names(&p.type_annotation, out),
        TSType::TSTypeOperatorType(op) => collect_infer_names(&op.type_annotation, out),
        TSType::TSIndexedAccessType(ia) => {
            collect_infer_names(&ia.object_type, out);
            collect_infer_names(&ia.index_type, out);
        }
        TSType::TSFunctionType(f) => collect_infer_names(&f.return_type.type_annotation, out),
        TSType::TSConstructorType(c) => collect_infer_names(&c.return_type.type_annotation, out),
        TSType::TSConditionalType(c) => {
            collect_infer_names(&c.check_type, out);
            collect_infer_names(&c.extends_type, out);
        }
        _ => {}
    }
}

fn ts_type_name_to_string(name: &TSTypeName<'_>) -> String {
    match name {
        TSTypeName::IdentifierReference(id) => id.name.to_string(),
        TSTypeName::QualifiedName(q) => {
            format!("{}.{}", ts_type_name_to_string(&q.left), q.right.name)
        }
        TSTypeName::ThisExpression(_) => "this".to_string(),
    }
}

/// Flatten a `TSImportTypeQualifier` chain into a dotted string.
/// Used for `import("mod").Foo.Bar` → `"Foo.Bar"`.
fn import_type_qualifier_to_string(q: &oxc_ast::ast::TSImportTypeQualifier<'_>) -> String {
    use oxc_ast::ast::TSImportTypeQualifier;
    match q {
        TSImportTypeQualifier::Identifier(id) => id.name.as_str().to_string(),
        TSImportTypeQualifier::QualifiedName(qn) => {
            format!(
                "{}.{}",
                import_type_qualifier_to_string(&qn.left),
                qn.right.name.as_str()
            )
        }
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
        TSTupleElement::TSRestType(r) => TypeOwned::Array(Box::new(lower_ts_type_impl(
            &r.type_annotation,
            source,
            type_params,
        ))),
        // ── Named tuple member — preserve the label ───────────────────────
        // Previously the label was stripped, which is a silent data loss.
        // We emit TypeOwned::NamedTupleElem; emit.rs maps it to
        // TupleElement::Named { label, ty }, so the label reaches the IR.
        TSTupleElement::TSNamedTupleMember(m) => {
            let label = m.label.name.to_string();
            let ty = lower_ts_tuple_element(&m.element_type, source, type_params);
            TypeOwned::NamedTupleElem {
                label,
                ty: Box::new(ty),
            }
        }
        other => {
            // `as_ts_type()` returns `Option<&TSType<'a>>` for the transparent
            // pass-through variants (i.e. variants that ARE TSType variants).
            other.as_ts_type().map_or_else(
                || TypeOwned::Unsupported("tuple element".to_string()),
                |ty| lower_ts_type_impl(ty, source, type_params),
            )
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
        let span = param.span();
        out.push(ParamFact {
            name: name.unwrap_or_else(|| "_".to_string()),
            ty,
            is_optional: param.optional,
            is_rest: false,
            is_readonly: false,
            span_start: span.start,
            span_end: span.end,
        });
    }

    if let Some(rest) = &params.rest {
        let ty = rest
            .type_annotation
            .as_ref()
            .map(|ann| lower_ts_type_impl(&ann.type_annotation, source, type_params));
        let name = binding_pattern_name(&rest.rest.argument);
        let span = rest.span();
        out.push(ParamFact {
            name: name.unwrap_or_else(|| "...rest".to_string()),
            ty,
            is_optional: false,
            is_rest: true,
            is_readonly: false,
            span_start: span.start,
            span_end: span.end,
        });
    }

    out
}

fn binding_pattern_name(pat: &oxc_ast::ast::BindingPattern<'_>) -> Option<String> {
    use oxc_ast::ast::BindingPattern;
    match pat {
        BindingPattern::BindingIdentifier(id) => Some(id.name.to_string()),
        BindingPattern::AssignmentPattern(ap) => binding_pattern_name(&ap.left),
        BindingPattern::ObjectPattern(_) => Some("{...}".to_string()),
        BindingPattern::ArrayPattern(_) => Some("[...]".to_string()),
    }
}
