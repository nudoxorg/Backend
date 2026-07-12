//! `TSType` → `ir::ty::Type` lowering (OXC-PLAN §5.3), plus the record/field,
//! index-signature, and generic-parameter helpers built on top of it.
//!
//! LEAF FILE — all `todo!()` stubs replaced with real implementations.
//! Binds to `Extractor` state and `ir::*` target shapes; signatures match the
//! stub declarations exactly.

use oxc_ast::ast::{
    BindingPattern, TSIndexSignature, TSLiteral, TSMappedTypeModifierOperator, TSSignature,
    TSType, TSTypeLiteral, TSTypeName, TSTypeParameterDeclaration, TSTypePredicateName,
    TSTupleElement,
};
use oxc_span::GetSpan;

use ir::{
    generics::{Constraint, GenericArg, Generics, Kind, TraitRef, TypeExpr, Variance},
    kind::Visibility,
    parameter::{LiteralParameter, Parameter, ParameterAttribute, TypeParam, TypeParamOrigin},
    pipeline::output_parameters_from_type,
    primitives::{Primitive, Width},
    record::{Field, FieldAttributes, FieldKey, IndexSignature, KnownField, Record},
    ty::{
        ConditionalType, FunctionPointer, LiteralKind, LiteralValue, MappedType, ModifierPrefix,
        PredicateSubject, QualifiedPath, TemplateLiteralType, TupleMember, Type, TypeOperator,
        TypePredicate, TypeQuery, TypeReference,
    },
};

use super::{Extractor, Result};

// ---------------------------------------------------------------------------
// Public metadata type
// ---------------------------------------------------------------------------

/// Metadata carried alongside a property when lowering it to an `ir` [`Field`]
/// (shared by class/interface/type-literal lowering).
pub struct PropertyFieldMetadata<'m> {
    pub optional: bool,
    pub readonly: bool,
    pub is_static: bool,
    pub visibility: Option<Visibility>,
    pub documentation: Option<String>,
    pub decorators: &'m [String],
}

// ---------------------------------------------------------------------------
// Internal helpers (free functions)
// ---------------------------------------------------------------------------

/// Map an `Option<TSMappedTypeModifierOperator>` onto `Option<ModifierPrefix>`.
fn modifier_prefix(op: Option<TSMappedTypeModifierOperator>) -> Option<ModifierPrefix> {
    match op {
        Some(TSMappedTypeModifierOperator::True) => Some(ModifierPrefix::Preserve),
        Some(TSMappedTypeModifierOperator::Plus) => Some(ModifierPrefix::Add),
        Some(TSMappedTypeModifierOperator::Minus) => Some(ModifierPrefix::Remove),
        None => None,
    }
}

/// Flatten a `TSTypeName` into a dotted string (`Foo` or `Foo.Bar.Baz`).
fn ts_type_name_to_string(name: &TSTypeName<'_>) -> String {
    match name {
        TSTypeName::IdentifierReference(id) => id.name.to_string(),
        TSTypeName::QualifiedName(qn) => {
            format!("{}.{}", ts_type_name_to_string(&qn.left), qn.right.name)
        }
        // `this` in a TSTypeName (rare; appears in heritage positions like `this.Prop`)
        TSTypeName::ThisExpression(_) => "this".to_string(),
    }
}

/// Flatten a `TSImportTypeQualifier` chain into a dotted string.
fn import_qualifier_to_string(q: &oxc_ast::ast::TSImportTypeQualifier<'_>) -> String {
    use oxc_ast::ast::TSImportTypeQualifier;
    match q {
        TSImportTypeQualifier::Identifier(id) => id.name.to_string(),
        TSImportTypeQualifier::QualifiedName(qn) => {
            format!("{}.{}", import_qualifier_to_string(&qn.left), qn.right.name)
        }
    }
}

/// Extract a static string key from a `PropertyKey` (computed keys → `None`).
fn property_key_name<'a>(
    key: &'a oxc_ast::ast::PropertyKey<'_>,
) -> Option<String> {
    key.static_name().map(|s| s.into_owned())
}

/// Derive a readable name for an indexed-access index type (`T[K]`), replacing
/// deno's `format!("{:?}")` stringification. The structured index type is kept
/// separately in `QualifiedPath.generic_arguments`; this is only the label.
fn index_access_name(index: &Type) -> String {
    match index {
        Type::TypeReference(tr) => tr.identifier.clone(),
        Type::Primitive(p) => format!("{p:?}"),
        Type::TypeOperator(op) => op.operator.clone(),
        _ => "index".to_string(),
    }
}

// ---------------------------------------------------------------------------
// `impl<'a> Extractor<'a>` — types block
// ---------------------------------------------------------------------------

impl<'a> Extractor<'a> {
    // -----------------------------------------------------------------------
    // Primary dispatch
    // -----------------------------------------------------------------------

    /// Lower a `TSType` node into `ir::ty::Type`. Dispatches over all ~37 oxc
    /// `TSType` variants per OXC-PLAN §5.3.
    ///
    /// **TSParenthesizedType**: `preserve_parens: false` is set at parse time so
    /// this variant cannot appear in practice. An unwrap branch is included for
    /// safety.
    ///
    /// **type_ref_scratch**: NOT populated here per Phase-3.4 design. That is
    /// func.rs's responsibility when building parameter/return type-link keys.
    pub(crate) fn lower_ts_type(&mut self, ty: &TSType<'a>) -> Result<Type> {
        match ty {
            // ----------------------------------------------------------------
            // Keyword types — each maps to a fixed ir::Type per §5.3.
            // ----------------------------------------------------------------
            TSType::TSAnyKeyword(_) => Ok(self.keyword_type("any")),
            TSType::TSBigIntKeyword(_) => Ok(self.keyword_type("bigint")),
            TSType::TSBooleanKeyword(_) => Ok(self.keyword_type("boolean")),
            TSType::TSIntrinsicKeyword(_) => Ok(self.keyword_type("intrinsic")),
            TSType::TSNeverKeyword(_) => Ok(self.keyword_type("never")),
            TSType::TSNullKeyword(_) => Ok(self.keyword_type("null")),
            TSType::TSNumberKeyword(_) => Ok(self.keyword_type("number")),
            TSType::TSObjectKeyword(_) => Ok(self.keyword_type("object")),
            TSType::TSStringKeyword(_) => Ok(self.keyword_type("string")),
            TSType::TSSymbolKeyword(_) => Ok(self.keyword_type("symbol")),
            TSType::TSUndefinedKeyword(_) => Ok(self.keyword_type("undefined")),
            TSType::TSUnknownKeyword(_) => Ok(self.keyword_type("unknown")),
            TSType::TSVoidKeyword(_) => Ok(self.keyword_type("void")),
            TSType::TSThisType(_) => Ok(Type::SelfType),

            // ----------------------------------------------------------------
            // TSTypeReference — Foo / Foo.Bar / Foo<T>
            // ----------------------------------------------------------------
            TSType::TSTypeReference(tr) => {
                let identifier = ts_type_name_to_string(&tr.type_name);
                let generic_args = tr
                    .type_arguments
                    .as_ref()
                    .map(|tp| {
                        tp.params
                            .iter()
                            .map(|t| self.lower_ts_type(t).map(GenericArg::Type))
                            .collect::<Result<Vec<_>>>()
                    })
                    .transpose()?
                    .filter(|v| !v.is_empty());
                Ok(Type::TypeReference(TypeReference { identifier, generic_args }))
            }

            // ----------------------------------------------------------------
            // Union / Intersection
            // ----------------------------------------------------------------
            TSType::TSUnionType(u) => {
                let types =
                    u.types.iter().map(|t| self.lower_ts_type(t)).collect::<Result<Vec<_>>>()?;
                Ok(Type::Union(types))
            }
            TSType::TSIntersectionType(i) => {
                let types =
                    i.types.iter().map(|t| self.lower_ts_type(t)).collect::<Result<Vec<_>>>()?;
                Ok(Type::Intersection(types))
            }

            // ----------------------------------------------------------------
            // Array / Tuple
            // ----------------------------------------------------------------
            TSType::TSArrayType(arr) => {
                Ok(Type::Slice(Box::new(self.lower_ts_type(&arr.element_type)?)))
            }
            TSType::TSTupleType(tup) => {
                // If any element is labelled, preserve labels via NamedTuple
                // (deno dropped them); otherwise a plain positional Tuple.
                let has_labels = tup
                    .element_types
                    .iter()
                    .any(|e| matches!(e, TSTupleElement::TSNamedTupleMember(_)));
                if has_labels {
                    let members = tup
                        .element_types
                        .iter()
                        .map(|elem| self.lower_tuple_member(elem))
                        .collect::<Result<Vec<_>>>()?;
                    Ok(Type::NamedTuple(members))
                } else {
                    let types = tup
                        .element_types
                        .iter()
                        .map(|elem| self.lower_tuple_element(elem))
                        .collect::<Result<Vec<_>>>()?;
                    Ok(Type::Tuple(types))
                }
            }

            // ----------------------------------------------------------------
            // Function / Constructor types
            // ----------------------------------------------------------------
            TSType::TSFunctionType(f) => {
                let input_params = self.lower_fn_params(&f.params)?;
                let return_ty = self.lower_ts_type(&f.return_type.type_annotation)?;
                let outputs = output_parameters_from_type(return_ty);
                Ok(Type::FunctionPointer(FunctionPointer {
                    inputs: if input_params.is_empty() { None } else { Some(input_params) },
                    outputs,
                    attributes: None,
                }))
            }
            TSType::TSConstructorType(c) => {
                let input_params = self.lower_fn_params(&c.params)?;
                let return_ty = self.lower_ts_type(&c.return_type.type_annotation)?;
                let outputs = output_parameters_from_type(return_ty);
                Ok(Type::FunctionPointer(FunctionPointer {
                    inputs: if input_params.is_empty() { None } else { Some(input_params) },
                    outputs,
                    attributes: None,
                }))
            }

            // ----------------------------------------------------------------
            // TSParenthesizedType — unwrap (should not appear; preserve_parens=false)
            // ----------------------------------------------------------------
            TSType::TSParenthesizedType(p) => self.lower_ts_type(&p.type_annotation),

            // ----------------------------------------------------------------
            // Type operators: keyof / unique / readonly
            // ----------------------------------------------------------------
            TSType::TSTypeOperatorType(op) => {
                use oxc_ast::ast::TSTypeOperatorOperator;
                let operator = match op.operator {
                    TSTypeOperatorOperator::Keyof => "keyof",
                    TSTypeOperatorOperator::Unique => "unique",
                    TSTypeOperatorOperator::Readonly => "readonly",
                }
                .to_string();
                Ok(Type::TypeOperator(TypeOperator {
                    operator,
                    r#type: Box::new(self.lower_ts_type(&op.type_annotation)?),
                }))
            }

            // ----------------------------------------------------------------
            // typeof: TypeQuery { expr_name }
            // Tier A: TypeReference(name). Tier B will add a structured form.
            // TSTypeQueryExprName inherits from TSTypeName, so the expanded
            // variants are: IdentifierReference, QualifiedName, ThisExpression
            // (from TSTypeName) + TSImportType (own variant).
            // ----------------------------------------------------------------
            TSType::TSTypeQuery(q) => {
                use oxc_ast::ast::TSTypeQueryExprName;
                let name = match &q.expr_name {
                    TSTypeQueryExprName::IdentifierReference(id) => id.name.to_string(),
                    TSTypeQueryExprName::QualifiedName(qn) => {
                        // Reconstruct the dotted name from TSQualifiedName
                        fn qualified_to_string(name: &TSTypeName<'_>) -> String {
                            ts_type_name_to_string(name)
                        }
                        format!(
                            "{}.{}",
                            qualified_to_string(&qn.left),
                            qn.right.name
                        )
                    }
                    TSTypeQueryExprName::ThisExpression(_) => "this".to_string(),
                    TSTypeQueryExprName::TSImportType(import) => import
                        .qualifier
                        .as_ref()
                        .map(|q| import_qualifier_to_string(q))
                        .unwrap_or_else(|| import.source.value.to_string()),
                };
                // `typeof x` is a first-class type-level *query* (deno flattened
                // it to a bare name string). Preserve the queried binding name
                // plus any explicit type arguments.
                let generic_args = q
                    .type_arguments
                    .as_ref()
                    .map(|ta| {
                        ta.params
                            .iter()
                            .map(|t| self.lower_ts_type(t).map(GenericArg::Type))
                            .collect::<Result<Vec<_>>>()
                    })
                    .transpose()?
                    .filter(|v| !v.is_empty());
                Ok(Type::TypeQuery(TypeQuery { name, generic_args }))
            }

            // ----------------------------------------------------------------
            // Conditional: T extends U ? X : Y
            // ----------------------------------------------------------------
            TSType::TSConditionalType(c) => Ok(Type::Conditional(ConditionalType {
                check_type: Box::new(self.lower_ts_type(&c.check_type)?),
                extends_type: Box::new(self.lower_ts_type(&c.extends_type)?),
                true_type: Box::new(self.lower_ts_type(&c.true_type)?),
                false_type: Box::new(self.lower_ts_type(&c.false_type)?),
            })),

            // ----------------------------------------------------------------
            // infer T
            // ----------------------------------------------------------------
            TSType::TSInferType(_) => Ok(Type::Infer),

            // ----------------------------------------------------------------
            // Indexed access: T[K]
            // Tier A: format!("{:?}", lowered index) as name (matches old
            // types.rs:243 for Tier-A parity). Tier B will produce a real
            // QualifiedPath lowering.
            // ----------------------------------------------------------------
            TSType::TSIndexedAccessType(ia) => {
                let self_type = Box::new(self.lower_ts_type(&ia.object_type)?);
                let index_ty = self.lower_ts_type(&ia.index_type)?;
                // Tier B: a real lowering of `T[K]`. deno stringified the index
                // via `format!("{:?}")`; instead we derive a readable name and
                // preserve the *structured* index type in `generic_arguments`
                // so consumers can recover it exactly.
                let name = index_access_name(&index_ty);
                Ok(Type::QualifiedPath(QualifiedPath {
                    name,
                    generic_arguments: Some(vec![GenericArg::Type(index_ty)]),
                    self_type,
                    tr: None,
                }))
            }

            // ----------------------------------------------------------------
            // Type literal: { k: V; m(): R }
            // ----------------------------------------------------------------
            TSType::TSTypeLiteral(lit) => {
                Ok(Type::RecordLiteral(Box::new(self.type_literal_record(None, lit)?)))
            }

            // ----------------------------------------------------------------
            // Mapped type: { [P in keyof T]?: T[P] }
            // constraint is non-optional in oxc (deno's MissingMappedTypeConstraint
            // error path disappears).
            // ----------------------------------------------------------------
            TSType::TSMappedType(m) => {
                let source_type = Box::new(self.lower_ts_type(&m.constraint)?);
                Ok(Type::Mapped(MappedType {
                    readonly: modifier_prefix(m.readonly),
                    optional: modifier_prefix(m.optional),
                    parameter: m.key.name.to_string(),
                    source_type,
                    name_type: m
                        .name_type
                        .as_ref()
                        .map(|t| self.lower_ts_type(t).map(Box::new))
                        .transpose()?,
                    value_type: m
                        .type_annotation
                        .as_ref()
                        .map(|t| self.lower_ts_type(t).map(Box::new))
                        .transpose()?,
                }))
            }

            // ----------------------------------------------------------------
            // Import type: import("mod") / import("mod").Qualifier
            // Tier A: TypeReference(qualifier name or specifier).
            // ----------------------------------------------------------------
            TSType::TSImportType(imp) => {
                let name = imp
                    .qualifier
                    .as_ref()
                    .map(|q| import_qualifier_to_string(q))
                    .unwrap_or_else(|| imp.source.value.to_string());
                let generic_args = imp
                    .type_arguments
                    .as_ref()
                    .map(|ta| {
                        ta.params
                            .iter()
                            .map(|t| self.lower_ts_type(t).map(GenericArg::Type))
                            .collect::<Result<Vec<_>>>()
                    })
                    .transpose()?
                    .filter(|v| !v.is_empty());
                Ok(Type::TypeReference(TypeReference { identifier: name, generic_args }))
            }

            // ----------------------------------------------------------------
            // Type predicate: x is T / asserts x is T / asserts x
            // ----------------------------------------------------------------
            TSType::TSTypePredicate(pred) => {
                let subject = match &pred.parameter_name {
                    TSTypePredicateName::This(_) => PredicateSubject::This,
                    TSTypePredicateName::Identifier(id) => {
                        PredicateSubject::Identifier(id.name.to_string())
                    }
                };
                let r#type = pred
                    .type_annotation
                    .as_ref()
                    .map(|ann| self.lower_ts_type(&ann.type_annotation).map(Box::new))
                    .transpose()?;
                Ok(Type::Predicate(TypePredicate { asserts: pred.asserts, subject, r#type }))
            }

            // ----------------------------------------------------------------
            // Literal type: "string", 42, true, -1n, etc.
            // ----------------------------------------------------------------
            TSType::TSLiteralType(l) => Ok(self.literal_type(&l.literal)),

            // ----------------------------------------------------------------
            // Template literal type: `foo-${T}` — first-class structured form.
            // deno flattened this to `string`; instead we keep the interleaved
            // literal chunks (`quasis`) AND the embedded interpolated `types`.
            // ----------------------------------------------------------------
            TSType::TSTemplateLiteralType(t) => {
                let quasis =
                    t.quasis.iter().map(|q| q.value.raw.to_string()).collect::<Vec<_>>();
                let types = t
                    .types
                    .iter()
                    .map(|ty| self.lower_ts_type(ty))
                    .collect::<Result<Vec<_>>>()?;
                Ok(Type::TemplateLiteral(TemplateLiteralType { quasis, types }))
            }

            // ----------------------------------------------------------------
            // Named tuple member — only valid inside TSTupleType; if somehow
            // encountered at top level, lower the element type.
            // ----------------------------------------------------------------
            TSType::TSNamedTupleMember(n) => self.lower_tuple_element(&n.element_type),

            // ----------------------------------------------------------------
            // JSDoc types
            // ----------------------------------------------------------------
            TSType::JSDocNullableType(n) => self.lower_ts_type(&n.type_annotation),
            TSType::JSDocNonNullableType(n) => self.lower_ts_type(&n.type_annotation),
            TSType::JSDocUnknownType(_) => Ok(Type::Any),
        }
    }

    // -----------------------------------------------------------------------
    // keyword_type — §5.3 keyword-to-ir mapping
    // -----------------------------------------------------------------------

    /// Map a keyword spelling to its `ir` primitive / reference.
    pub(crate) fn keyword_type(&self, keyword: &str) -> Type {
        match keyword {
            "string" => Type::Primitive(Primitive::String),
            "number" => Type::Primitive(Primitive::Float(Width::W64)),
            "boolean" => Type::Primitive(Primitive::Bool),
            "bigint" => Type::Primitive(Primitive::Int(Width::W128)),
            "null" | "undefined" | "void" => Type::Tuple(vec![]),
            "never" => Type::Never,
            "any" | "unknown" => Type::Any,
            "this" => Type::SelfType,
            "object" => Type::TypeReference(TypeReference {
                identifier: "object".to_string(),
                generic_args: None,
            }),
            "symbol" | "unique symbol" => Type::TypeReference(TypeReference {
                identifier: "Symbol".to_string(),
                generic_args: None,
            }),
            "intrinsic" => Type::TypeReference(TypeReference {
                identifier: "intrinsic".to_string(),
                generic_args: None,
            }),
            other => Type::TypeReference(TypeReference {
                identifier: other.to_string(),
                generic_args: None,
            }),
        }
    }

    // -----------------------------------------------------------------------
    // literal_type — TSLiteralType → primitive (Tier A)
    // -----------------------------------------------------------------------

    /// Lower a `TSLiteralType`'s literal node into a first-class
    /// [`Type::Literal`], preserving the exact **value** deno_doc dropped (it
    /// kept only the kind → `string`/`number`/`boolean`). `"foo"`, `42`, `true`,
    /// and `-42n` each survive with their source spelling and a typed kind.
    pub(crate) fn literal_type(&mut self, lit: &TSLiteral<'a>) -> Type {
        let value = lit.span().source_text(self.source).to_string();
        let kind = match lit {
            TSLiteral::BooleanLiteral(_) => LiteralKind::Boolean,
            TSLiteral::NumericLiteral(_) => LiteralKind::Number,
            TSLiteral::BigIntLiteral(_) => LiteralKind::BigInt,
            TSLiteral::StringLiteral(_) => LiteralKind::String,
            TSLiteral::TemplateLiteral(_) => LiteralKind::String,
            // `-42` / `-42n` etc.: infer bigint from the `n` suffix.
            TSLiteral::UnaryExpression(_) => {
                if value.ends_with('n') { LiteralKind::BigInt } else { LiteralKind::Number }
            }
            // Null / RegExp / any future literal node: fall back to its spelling.
            _ => LiteralKind::String,
        };
        Type::Literal(LiteralValue { kind, value })
    }

    // -----------------------------------------------------------------------
    // lower_type_params — TSTypeParameterDeclaration → Generics
    // -----------------------------------------------------------------------

    /// Lower a `TSTypeParameterDeclaration` into `ir::generics::Generics`.
    /// Returns `None` when the parameter list is empty.
    pub(crate) fn lower_type_params(
        &mut self,
        params: &TSTypeParameterDeclaration<'a>,
    ) -> Result<Option<Generics>> {
        if params.params.is_empty() {
            return Ok(None);
        }

        let mut type_params = Vec::new();
        let mut constraints = Vec::new();

        for p in &params.params {
            let default_type = p
                .default
                .as_ref()
                .map(|t| self.lower_ts_type(t))
                .transpose()?
                .map(|ty| self.type_to_expr(&ty));

            // Tier B: honour explicit `in`/`out` variance annotations
            // (`type Foo<in out T>`), which deno hardcoded to `Invariant`.
            let variance = match (p.r#in, p.r#out) {
                (true, true) => Variance::Invariant,
                (true, false) => Variance::Contravariant,
                (false, true) => Variance::Covariant,
                (false, false) => Variance::Invariant,
            };

            type_params.push(Parameter::Type(TypeParam {
                name: Some(p.name.name.to_string()),
                kind: Kind::Type,
                variance,
                default_type,
                params: None,
                origin: TypeParamOrigin::Free,
            }));

            if let Some(constraint) = &p.constraint {
                let trait_ref = self.ts_type_to_trait_ref(constraint)?;
                constraints.push(Constraint::TraitBound {
                    param: p.name.name.to_string(),
                    trait_ref,
                });
            }
        }

        Ok(Some(Generics { params: type_params, constraints }))
    }

    // -----------------------------------------------------------------------
    // type_to_expr — Type → TypeExpr
    // -----------------------------------------------------------------------

    /// Flatten an `ir::ty::Type` into a `TypeExpr` (name + args) for use as
    /// a generic default or trait-ref argument.
    pub(crate) fn type_to_expr(&self, ty: &Type) -> TypeExpr {
        match ty {
            Type::TypeReference(tr) => {
                let args = tr
                    .generic_args
                    .as_ref()
                    .map(|args| {
                        args.iter()
                            .filter_map(|arg| {
                                if let GenericArg::Type(t) = arg { Some(self.type_to_expr(t)) } else { None }
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                TypeExpr { name: tr.identifier.clone(), args }
            }
            Type::SelfType => TypeExpr { name: "Self".to_string(), args: vec![] },
            _ => TypeExpr { name: format!("{:?}", ty), args: vec![] },
        }
    }

    // -----------------------------------------------------------------------
    // ts_type_to_trait_ref — TSType (heritage) → TraitRef
    // -----------------------------------------------------------------------

    /// Lower a type used in an `extends` / heritage position into a `TraitRef`.
    pub(crate) fn ts_type_to_trait_ref(&mut self, ty: &TSType<'a>) -> Result<TraitRef> {
        match ty {
            TSType::TSTypeReference(tr) => {
                let name = ts_type_name_to_string(&tr.type_name);
                let args = tr
                    .type_arguments
                    .as_ref()
                    .map(|tp| {
                        tp.params
                            .iter()
                            .map(|t| self.lower_ts_type(t).map(|t| self.type_to_expr(&t)))
                            .collect::<Result<Vec<_>>>()
                    })
                    .transpose()?
                    .unwrap_or_default();
                Ok(TraitRef { name, args })
            }
            _ => {
                let parsed = self.lower_ts_type(ty)?;
                let expr = self.type_to_expr(&parsed);
                Ok(TraitRef { name: expr.name, args: expr.args })
            }
        }
    }

    // -----------------------------------------------------------------------
    // index_signature — TSIndexSignature → IndexSignature
    // -----------------------------------------------------------------------

    /// Lower a `TSIndexSignature` into an `ir::record::IndexSignature`.
    pub(crate) fn index_signature(
        &mut self,
        sig: &TSIndexSignature<'a>,
    ) -> Result<IndexSignature> {
        use super::super::error::TsTypeError;

        let key_param = sig
            .parameters
            .first()
            .ok_or(TsTypeError::MissingIndexSignatureKeyParameter)?;
        let key_type = self.lower_ts_type(&key_param.type_annotation.type_annotation)?;
        let value_type = self.lower_ts_type(&sig.type_annotation.type_annotation)?;

        Ok(IndexSignature {
            key_type: Box::new(key_type),
            value_type: Box::new(value_type),
        })
    }

    // -----------------------------------------------------------------------
    // type_literal_record — TSTypeLiteral → Record
    // -----------------------------------------------------------------------

    /// Lower a `TSTypeLiteral` (`{ k: V; m(): R }`) into an anonymous `Record`
    /// with its five member kinds (props / methods / call / construct / index).
    pub(crate) fn type_literal_record(
        &mut self,
        name: Option<String>,
        lit: &TSTypeLiteral<'a>,
    ) -> Result<Record> {
        let mut fields = Vec::new();
        let mut index_signatures = Vec::new();
        let mut methods = Vec::new();
        let mut call_signatures = Vec::new();
        let mut constructors = Vec::new();

        for sig in &lit.members {
            match sig {
                TSSignature::TSPropertySignature(prop) => {
                    if let Some(key_name) = property_key_name(&prop.key) {
                        let ty = prop
                            .type_annotation
                            .as_ref()
                            .map(|ann| self.lower_ts_type(&ann.type_annotation))
                            .transpose()?;
                        fields.push(Field::Known(KnownField {
                            key: FieldKey::Ident(key_name),
                            r#type: ty.map(Box::new),
                            default_value: None,
                            attributes: FieldAttributes {
                                is_mutable: !prop.readonly,
                                is_optional: prop.optional,
                                decorators: vec![],
                                is_static: false,
                            },
                            visibility: None,
                            documentation: None,
                        }));
                    }
                    // Computed property keys are skipped (cannot produce a
                    // stable FieldKey without a checker).
                }
                TSSignature::TSIndexSignature(idx) => {
                    index_signatures.push(self.index_signature(idx)?);
                }
                TSSignature::TSMethodSignature(method) => {
                    methods.push(self.method_signature_function(method)?);
                }
                TSSignature::TSCallSignatureDeclaration(call) => {
                    call_signatures.push(self.call_signature_function(call)?);
                }
                TSSignature::TSConstructSignatureDeclaration(ctor) => {
                    constructors.push(self.construct_signature_function(ctor)?);
                }
            }
        }

        Ok(Record {
            name,
            generics: None,
            fields,
            call_signatures: if call_signatures.is_empty() { None } else { Some(call_signatures) },
            constructors: if constructors.is_empty() { None } else { Some(constructors) },
            methods: if methods.is_empty() { None } else { Some(methods) },
            index_signatures: if index_signatures.is_empty() {
                None
            } else {
                Some(index_signatures)
            },
            super_types: None,
            members: None,
            implemented_protocols: None,
        })
    }

    // -----------------------------------------------------------------------
    // property_field — named property → Field::Known
    // -----------------------------------------------------------------------

    /// Lower a single property into an `ir` [`Field::Known`].
    pub(crate) fn property_field(
        &mut self,
        name: &str,
        ty: Option<&TSType<'a>>,
        metadata: PropertyFieldMetadata<'_>,
    ) -> Result<Field> {
        let lowered_ty =
            ty.map(|t| self.lower_ts_type(t).map(Box::new)).transpose()?;
        Ok(Field::Known(KnownField {
            key: FieldKey::Ident(name.to_string()),
            r#type: lowered_ty,
            default_value: None,
            attributes: FieldAttributes {
                is_mutable: !metadata.readonly,
                is_optional: metadata.optional,
                decorators: metadata.decorators.to_vec(),
                is_static: metadata.is_static,
            },
            visibility: metadata.visibility,
            documentation: metadata.documentation,
        }))
    }

    // -----------------------------------------------------------------------
    // Private helpers
    // -----------------------------------------------------------------------

    /// Lower a `TSTupleElement` into an `ir::ty::Type`.
    ///
    /// - `TSOptionalType` → passthrough inner type (deno parity)
    /// - `TSRestType` → `Type::Variadic(inner)`
    /// - `TSNamedTupleMember` → lower `element_type` (label dropped in Tier A)
    /// - All inherited `TSType` variants → delegate to `lower_ts_type` via
    ///   the generated `as_ts_type()` cast (zero-cost transmute; safe per oxc
    ///   discriminant layout guarantee).
    fn lower_tuple_element(&mut self, elem: &TSTupleElement<'a>) -> Result<Type> {
        match elem {
            TSTupleElement::TSRestType(r) => {
                Ok(Type::Variadic(Box::new(self.lower_ts_type(&r.type_annotation)?)))
            }
            TSTupleElement::TSOptionalType(o) => {
                // Optional tuple element: lower inner, no wrapper in Tier A.
                self.lower_ts_type(&o.type_annotation)
            }
            TSTupleElement::TSNamedTupleMember(n) => {
                // Named member: label dropped in Tier A; lower element_type.
                self.lower_tuple_element(&n.element_type)
            }
            // All remaining variants are `TSType` variants inherited via
            // the `#[ast] INHERIT(TSType<'a>)` mechanism. The generated
            // `as_ts_type()` converts `&TSTupleElement<'a>` → `&TSType<'a>`
            // via a checked transmute (discriminants are aligned).
            other => {
                let ts_ty = other.as_ts_type().expect(
                    "TSTupleElement variant that is not TSOptionalType/TSRestType/TSNamedTupleMember \
                     must be a TSType variant (as_ts_type() cannot return None here)",
                );
                self.lower_ts_type(ts_ty)
            }
        }
    }

    /// Lower a `TSTupleElement` into a labelled [`TupleMember`], preserving the
    /// `TSNamedTupleMember` label that deno_doc discarded.
    fn lower_tuple_member(&mut self, elem: &TSTupleElement<'a>) -> Result<TupleMember> {
        match elem {
            TSTupleElement::TSNamedTupleMember(n) => Ok(TupleMember {
                label:  Some(n.label.name.to_string()),
                r#type: self.lower_tuple_element(&n.element_type)?,
            }),
            other => Ok(TupleMember { label: None, r#type: self.lower_tuple_element(other)? }),
        }
    }

    /// Lower a `FormalParameters` block into a flat `Vec<Parameter>` — type
    /// only (used for function-type and constructor-type params where we need
    /// parameter types but not full receiver logic).
    fn lower_fn_params(
        &mut self,
        params: &oxc_ast::ast::FormalParameters<'a>,
    ) -> Result<Vec<Parameter>> {
        let mut result: Vec<Parameter> = params
            .items
            .iter()
            .map(|p| self.lower_formal_param_type_only(p))
            .collect::<Result<_>>()?;

        if let Some(rest) = &params.rest {
            // `FormalParameters.rest` is a `FormalParameterRest` which wraps a
            // `BindingRestElement` (`.rest.argument`) and its type annotation
            // (`.type_annotation`).
            let ty = rest
                .type_annotation
                .as_ref()
                .map(|ann| self.lower_ts_type(&ann.type_annotation))
                .transpose()?;
            let rest_name = rest
                .rest
                .argument
                .get_binding_identifier()
                .map(|id| id.name.to_string())
                .unwrap_or_default();
            result.push(Parameter::Literal(LiteralParameter {
                name: rest_name,
                r#type: ty,
                attributes: Some(vec![ParameterAttribute::Variadic]),
                default_value: None,
                description: None,
            }));
        }

        Ok(result)
    }

    /// Lower one `FormalParameter` to a bare `Parameter` — type only, no
    /// full receiver/name inference (used for function-pointer params and
    /// construct/call signatures inside type literals).
    fn lower_formal_param_type_only(
        &mut self,
        param: &oxc_ast::ast::FormalParameter<'a>,
    ) -> Result<Parameter> {
        let (name, attrs) = match &param.pattern {
            BindingPattern::BindingIdentifier(id) => {
                let attrs =
                    if param.optional { Some(vec![ParameterAttribute::Optional]) } else { None };
                (id.name.to_string(), attrs)
            }
            BindingPattern::AssignmentPattern(ap) => {
                // Default parameter: optional = true, use inner binding name.
                let inner_name = match &ap.left {
                    BindingPattern::BindingIdentifier(id) => id.name.to_string(),
                    _ => String::new(),
                };
                (inner_name, None)
            }
            _ => {
                // Array / Object destructure — fall back to the source span.
                let attrs =
                    if param.optional { Some(vec![ParameterAttribute::Optional]) } else { None };
                (param.span.source_text(self.source).to_string(), attrs)
            }
        };

        let ty = param
            .type_annotation
            .as_ref()
            .map(|ann| self.lower_ts_type(&ann.type_annotation))
            .transpose()?;

        Ok(Parameter::Literal(LiteralParameter {
            name,
            r#type: ty,
            attributes: attrs,
            default_value: None,
            description: None,
        }))
    }

    /// Lower a `TSConstructSignatureDeclaration` into an `ir::function::Function`
    /// (used when a `new(params): R` signature appears inside a type literal).
    fn construct_signature_function(
        &mut self,
        ctor: &oxc_ast::ast::TSConstructSignatureDeclaration<'a>,
    ) -> Result<ir::function::Function> {
        use ir::protocols::ReceiverKind;

        let input_params = self.lower_fn_params(&ctor.params)?;
        let outputs = ctor
            .return_type
            .as_ref()
            .map(|ann| self.lower_ts_type(&ann.type_annotation))
            .transpose()?
            .and_then(output_parameters_from_type);
        let generics = ctor
            .type_parameters
            .as_ref()
            .map(|tp| self.lower_type_params(tp))
            .transpose()?
            .flatten();

        Ok(ir::function::Function {
            input_parameters: if input_params.is_empty() { None } else { Some(input_params) },
            output_parameters: outputs,
            type_links: None,
            attributes: None,
            generics,
            receiver: Some(ReceiverKind::Static),
            overloads: None,
            implemented: false,
            members: None,
            implemented_protocols: None,
            body: None,
        })
    }
}
