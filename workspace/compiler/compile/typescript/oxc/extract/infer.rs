//! Syntactic expression inference (OXC-PLAN §1.2) — the closed table deno_doc's
//! `infer_ts_type_from_expr` implemented, ported onto `oxc_ast::Expression`.
//!
//! LEAF FILE — implements the `todo!()` bodies from the stub. This is a total,
//! closed dispatch over the inference-relevant `Expression` variants; unhandled
//! expressions return `None` (no inference), matching deno_doc.

use oxc_ast::ast::{
    ArrowFunctionExpression, ArrayExpressionElement, BinaryOperator, Expression, Function,
    ObjectPropertyKind, Statement,
};

use ir::{
    generics::GenericArg,
    parameter::{LiteralParameter, Parameter, ParameterAttribute},
    primitives::{Primitive, Width},
    record::{Field, FieldAttributes, FieldKey, KnownField, Record},
    ty::{FunctionPointer, Type, TypeReference},
};

use super::Extractor;

// ────────────────────────────────────────────────────────────────────────────
// Convenience constructors
// ────────────────────────────────────────────────────────────────────────────

#[inline]
fn type_ref(name: &str) -> Type {
    Type::TypeReference(TypeReference { identifier: name.to_owned(), generic_args: None })
}

#[inline]
fn primitive(p: Primitive) -> Type {
    Type::Primitive(p)
}

/// `Promise<void>` — used by `infer_return_type_fallback` for async functions.
fn promise_void() -> Type {
    Type::TypeReference(TypeReference {
        identifier: "Promise".to_owned(),
        generic_args: Some(vec![GenericArg::Type(void_type())]),
    })
}

/// `void` ≡ unit tuple in the IR (matches `TSVoidKeyword` → `Tuple([])`).
#[inline]
fn void_type() -> Type {
    Type::Tuple(vec![])
}

// ────────────────────────────────────────────────────────────────────────────
// Statement walker helpers (for `infer_return_type_fallback`)
// ────────────────────────────────────────────────────────────────────────────

/// Returns `true` if `stmts` contain a `return <expr>` (non-void return)
/// anywhere, recursively descending into nested blocks and control-flow bodies.
fn has_value_return(stmts: &[Statement<'_>]) -> bool {
    stmts.iter().any(stmt_has_value_return)
}

fn stmt_has_value_return(stmt: &Statement<'_>) -> bool {
    match stmt {
        // `return expr;` — has a value
        Statement::ReturnStatement(ret) => ret.argument.is_some(),

        // Block statement
        Statement::BlockStatement(block) => has_value_return(&block.body),

        // if / else
        Statement::IfStatement(if_stmt) => {
            stmt_has_value_return(&if_stmt.consequent)
                || if_stmt.alternate.as_ref().is_some_and(|alt| stmt_has_value_return(alt))
        }

        // Loops — body may contain a return
        Statement::WhileStatement(w)   => stmt_has_value_return(&w.body),
        Statement::DoWhileStatement(d) => stmt_has_value_return(&d.body),
        Statement::ForStatement(f)     => stmt_has_value_return(&f.body),
        Statement::ForInStatement(f)   => stmt_has_value_return(&f.body),
        Statement::ForOfStatement(f)   => stmt_has_value_return(&f.body),

        // switch
        Statement::SwitchStatement(sw) => {
            sw.cases.iter().any(|case| has_value_return(&case.consequent))
        }

        // try / catch / finally
        Statement::TryStatement(t) => {
            has_value_return(&t.block.body)
                || t.handler.as_ref().is_some_and(|h| has_value_return(&h.body.body))
                || t.finalizer.as_ref().is_some_and(|f| has_value_return(&f.body))
        }

        // labeled
        Statement::LabeledStatement(l) => stmt_has_value_return(&l.body),

        // with
        Statement::WithStatement(w) => stmt_has_value_return(&w.body),

        // Everything else (ExpressionStatement, Declaration, …) carries no return.
        _ => false,
    }
}

// ────────────────────────────────────────────────────────────────────────────
// Extractor impl
// ────────────────────────────────────────────────────────────────────────────

impl<'a> Extractor<'a> {
    /// Infer a type from an initializer/default expression (OXC-PLAN §1.2).
    ///
    /// `is_const` widens literal handling (`const x = 1` → literal `1` type vs
    /// `number`; `x as const` re-infers with `is_const = true`). Returns `None`
    /// for calls / bare identifiers / member exprs (no syntactic inference).
    pub(crate) fn infer_type_from_expr(
        &mut self,
        expr: &Expression<'a>,
        is_const: bool,
    ) -> Option<Type> {
        match expr {
            // ── Literals ──────────────────────────────────────────────────────

            // number literal → `number`
            // Tier A: always lower to keyword; Tier B will carry the literal value.
            Expression::NumericLiteral(_) => {
                Some(primitive(Primitive::Float(Width::W64)))
            }

            // string literal → `string`
            Expression::StringLiteral(_) => Some(primitive(Primitive::String)),

            // boolean literal → `boolean`
            Expression::BooleanLiteral(_) => Some(primitive(Primitive::Bool)),

            // bigint literal → `bigint`
            Expression::BigIntLiteral(_) => Some(primitive(Primitive::Int(Width::W128))),

            // null literal → `null`  (unit tuple in the IR = TSNullKeyword)
            Expression::NullLiteral(_) => Some(Type::Tuple(vec![])),

            // regex literal → TypeRef("RegExp")
            Expression::RegExpLiteral(_) => Some(type_ref("RegExp")),

            // template literal → `string`  (Tier A; structured in Tier B)
            Expression::TemplateLiteral(_) => Some(primitive(Primitive::String)),

            // ── Type-cast expressions ─────────────────────────────────────────

            // `x as const` — re-infer inner expression with is_const=true.
            // OXC represents `as const` as a TSAsExpression whose type_annotation
            // is a TSTypeReference with identifier "const".
            Expression::TSAsExpression(as_expr) => {
                if is_as_const(&as_expr.type_annotation) {
                    // `x as const` — re-infer the inner expression with is_const=true
                    self.infer_type_from_expr(&as_expr.expression, true)
                } else {
                    // `x as T` → lower T
                    self.lower_ts_type(&as_expr.type_annotation).ok()
                }
            }

            // `<T>x` (TSTypeAssertion — legacy angle-bracket cast)
            Expression::TSTypeAssertion(assertion) => {
                self.lower_ts_type(&assertion.type_annotation).ok()
            }

            // `x satisfies T` → T
            Expression::TSSatisfiesExpression(sat) => {
                self.lower_ts_type(&sat.type_annotation).ok()
            }

            // `x!` (non-null assertion) → infer inner.
            // Tier A: return the inner inference unchanged (deno_doc parity).
            // Tier B: strip null/undefined from a union result.
            Expression::TSNonNullExpression(nn) => {
                self.infer_type_from_expr(&nn.expression, is_const)
            }

            // ── Function / Arrow expressions ──────────────────────────────────

            Expression::ArrowFunctionExpression(arrow) => {
                self.infer_arrow_function(arrow, is_const)
            }

            Expression::FunctionExpression(func) => {
                self.infer_function_expr(func, is_const)
            }

            // ── `new X(...)` → TypeRef("X") ───────────────────────────────────

            Expression::NewExpression(new_expr) => {
                let name = callee_name(&new_expr.callee)?;
                // TODO (Tier B): lower new_expr.type_arguments as generic_args
                Some(type_ref(&name))
            }

            // ── Object literal → RecordLiteral ───────────────────────────────

            Expression::ObjectExpression(obj) => {
                let mut fields: Vec<Field> = Vec::new();
                for prop in &obj.properties {
                    match prop {
                        ObjectPropertyKind::ObjectProperty(p) => {
                            let val_ty =
                                self.infer_type_from_expr(&p.value, is_const).map(Box::new);
                            let key = property_key_name(&p.key);
                            fields.push(Field::Known(KnownField {
                                key:           FieldKey::Ident(key),
                                r#type:        val_ty,
                                default_value: None,
                                attributes:    FieldAttributes {
                                    decorators: vec![],
                                    is_mutable: true,
                                    is_optional: false,
                                    is_static:  false,
                                },
                                visibility:    None,
                                documentation: None,
                            }));
                        }
                        // SpreadProperty carries no static key — skip at Tier A
                        ObjectPropertyKind::SpreadProperty(_) => {}
                    }
                }
                Some(Type::RecordLiteral(Box::new(Record {
                    name:                  None,
                    generics:              None,
                    fields,
                    call_signatures:       None,
                    constructors:          None,
                    methods:               None,
                    index_signatures:      None,
                    super_types:           None,
                    members:               None,
                    implemented_protocols: None,
                })))
            }

            // ── Array literal → union of element inferences; `any[]` fallback ─

            Expression::ArrayExpression(arr) => {
                let mut element_types: Vec<Type> = Vec::new();
                for elem in &arr.elements {
                    match elem {
                        ArrayExpressionElement::SpreadElement(spread) => {
                            if let Some(t) =
                                self.infer_type_from_expr(&spread.argument, is_const)
                            {
                                element_types.push(t);
                            }
                        }
                        ArrayExpressionElement::Elision(_) => {
                            // array hole — skip
                        }
                        // All Expression-inherited variants
                        other => {
                            if let Some(inner_expr) = other.as_expression() {
                                if let Some(t) =
                                    self.infer_type_from_expr(inner_expr, is_const)
                                {
                                    element_types.push(t);
                                }
                            }
                        }
                    }
                }
                if element_types.is_empty() {
                    // `any[]` fallback
                    Some(Type::Slice(Box::new(Type::Any)))
                } else {
                    Some(Type::Union(element_types))
                }
            }

            // ── Ternary → union of both branch inferences ─────────────────────

            Expression::ConditionalExpression(cond) => {
                let t = self.infer_type_from_expr(&cond.consequent, is_const);
                let f = self.infer_type_from_expr(&cond.alternate, is_const);
                match (t, f) {
                    (Some(a), Some(b)) if a == b => Some(a),
                    (Some(a), Some(b))            => Some(Type::Union(vec![a, b])),
                    (Some(a), None)               => Some(a),
                    (None,    Some(b))            => Some(b),
                    (None,    None)               => None,
                }
            }

            // ── Unary expressions ─────────────────────────────────────────────

            Expression::UnaryExpression(unary) => {
                use oxc_ast::ast::UnaryOperator;
                match unary.operator {
                    // `typeof x` → string
                    UnaryOperator::Typeof => Some(primitive(Primitive::String)),
                    // `void x` → void (unit)
                    UnaryOperator::Void => Some(void_type()),
                    // arithmetic unary: `-x`, `+x` → number
                    UnaryOperator::UnaryNegation | UnaryOperator::UnaryPlus => {
                        Some(primitive(Primitive::Float(Width::W64)))
                    }
                    // `!x` → boolean
                    UnaryOperator::LogicalNot => Some(primitive(Primitive::Bool)),
                    // `~x` (bitwise NOT) → number
                    UnaryOperator::BitwiseNot => Some(primitive(Primitive::Float(Width::W64))),
                    // `delete x` → boolean
                    UnaryOperator::Delete => Some(primitive(Primitive::Bool)),
                    #[allow(unreachable_patterns)]
                    _ => None,
                }
            }

            // ── Update expressions: `x++`, `++x`, `x--`, `--x` → number ──────

            Expression::UpdateExpression(_) => {
                Some(primitive(Primitive::Float(Width::W64)))
            }

            // ── Binary expressions ────────────────────────────────────────────

            Expression::BinaryExpression(bin) => Some(infer_binary_op(bin.operator)),

            // ── Logical expressions (`&&`, `||`, `??`) ───────────────────────
            // deno_doc doesn't special-case these — return None.
            Expression::LogicalExpression(_) => None,

            // ── Await expressions → unwrap Promise<T> of inner inference ──────

            Expression::AwaitExpression(aw) => {
                let inner = self.infer_type_from_expr(&aw.argument, is_const)?;
                Some(unwrap_promise(inner))
            }

            // ── Sequence expressions: take last expression ────────────────────

            Expression::SequenceExpression(seq) => seq
                .expressions
                .last()
                .and_then(|e| self.infer_type_from_expr(e, is_const)),

            // ── Parenthesized expression (only seen when preserve_parens=true,
            //    which we don't set — but handle defensively) ──────────────────
            Expression::ParenthesizedExpression(p) => {
                self.infer_type_from_expr(&p.expression, is_const)
            }

            // ── No syntactic inference for: calls / identifiers / members ─────
            Expression::CallExpression(_)
            | Expression::Identifier(_)
            | Expression::StaticMemberExpression(_)
            | Expression::ComputedMemberExpression(_)
            | Expression::PrivateFieldExpression(_)
            | Expression::ChainExpression(_)
            | Expression::AssignmentExpression(_)
            | Expression::YieldExpression(_)
            | Expression::TaggedTemplateExpression(_)
            | Expression::ImportExpression(_)
            | Expression::MetaProperty(_)
            | Expression::Super(_)
            | Expression::ThisExpression(_)
            | Expression::JSXElement(_)
            | Expression::JSXFragment(_)
            | Expression::V8IntrinsicExpression(_) => None,

            // Catch-all — oxc is pre-1.0; future variants must not cause hard failures
            #[allow(unreachable_patterns)]
            _ => None,
        }
    }

    // ── Arrow / function expression helpers ───────────────────────────────────

    fn infer_arrow_function(
        &mut self,
        arrow: &ArrowFunctionExpression<'a>,
        _is_const: bool,
    ) -> Option<Type> {
        let inputs = self.fp_inputs_from_params(&arrow.params);

        // Return type: explicit annotation wins; else infer from body.
        let outputs = if let Some(ret_ann) = &arrow.return_type {
            self.lower_ts_type(&ret_ann.type_annotation)
                .ok()
                .map(|ty| vec![unnamed_output(ty)])
        } else if arrow.expression {
            // Expression-body arrow: `() => expr`.
            // OXC stores the expression as a single ExpressionStatement in body.statements.
            if let Some(Statement::ExpressionStatement(expr_stmt)) =
                arrow.body.statements.first()
            {
                self.infer_type_from_expr(&expr_stmt.expression, false)
                    .map(|ty| vec![unnamed_output(ty)])
            } else {
                None
            }
        } else {
            // Block-body arrow without annotation — use return-type fallback.
            let is_void = !has_value_return(&arrow.body.statements);
            let ret_ty = if is_void {
                if arrow.r#async { promise_void() } else { void_type() }
            } else {
                // Has a value return — can't infer type statically at Tier A
                return None;
            };
            Some(vec![unnamed_output(ret_ty)])
        };

        let attributes = if arrow.r#async { Some(vec![ir::function::Attribute::Async]) } else { None };

        Some(Type::FunctionPointer(FunctionPointer { inputs, outputs, attributes }))
    }

    fn infer_function_expr(
        &mut self,
        func: &Function<'a>,
        _is_const: bool,
    ) -> Option<Type> {
        let inputs = self.fp_inputs_from_params(&func.params);

        let outputs = if let Some(ret_ann) = &func.return_type {
            self.lower_ts_type(&ret_ann.type_annotation)
                .ok()
                .map(|ty| vec![unnamed_output(ty)])
        } else {
            // Use return-type fallback
            self.infer_return_type_fallback(func).map(|ty| vec![unnamed_output(ty)])
        };

        let mut attrs: Vec<ir::function::Attribute> = Vec::new();
        if func.r#async    { attrs.push(ir::function::Attribute::Async); }
        if func.generator  { attrs.push(ir::function::Attribute::Generator); }
        let attributes = if attrs.is_empty() { None } else { Some(attrs) };

        Some(Type::FunctionPointer(FunctionPointer { inputs, outputs, attributes }))
    }

    /// Build the `inputs` side of a `FunctionPointer` from a `FormalParameters`
    /// node. Each parameter's type annotation (on `FormalParameter`) is lowered;
    /// parameters without annotations contribute `r#type: None` at Tier A.
    fn fp_inputs_from_params(
        &mut self,
        params: &oxc_ast::ast::FormalParameters<'a>,
    ) -> Option<Vec<Parameter>> {
        let mut result: Vec<Parameter> = Vec::new();

        for p in &params.items {
            let name = pattern_name(&p.pattern);
            // Type annotation lives on FormalParameter, not BindingPattern
            let ty = p
                .type_annotation
                .as_ref()
                .and_then(|ann| self.lower_ts_type(&ann.type_annotation).ok());
            let mut pattrs: Vec<ParameterAttribute> = Vec::new();
            if p.optional { pattrs.push(ParameterAttribute::Optional); }
            // Default-value initializer → also marks the parameter as optional
            if p.initializer.is_some() && !p.optional {
                pattrs.push(ParameterAttribute::Optional);
            }
            let attributes = if pattrs.is_empty() { None } else { Some(pattrs) };
            result.push(Parameter::Literal(LiteralParameter {
                name,
                r#type:        ty,
                attributes,
                default_value: None,
                description:   None,
            }));
        }

        // Rest element: type annotation is on FormalParameterRest (not BindingRestElement)
        if let Some(rest) = &params.rest {
            let name = pattern_name(&rest.rest.argument);
            let ty = rest
                .type_annotation
                .as_ref()
                .and_then(|ann| self.lower_ts_type(&ann.type_annotation).ok());
            result.push(Parameter::Literal(LiteralParameter {
                name,
                r#type:        ty,
                attributes:    Some(vec![ParameterAttribute::Variadic]),
                default_value: None,
                description:   None,
            }));
        }

        if result.is_empty() { None } else { Some(result) }
    }

    // ── Return-type fallback ──────────────────────────────────────────────────

    /// Return-type fallback: a function with no annotation and no `return <expr>`
    /// anywhere in its body infers `void` (sync) / `Promise<void>` (async).
    /// Recursively walks body statements.
    pub(crate) fn infer_return_type_fallback(&self, func: &Function<'a>) -> Option<Type> {
        let body = func.body.as_ref()?;
        if has_value_return(&body.statements) {
            // Has a value-return somewhere — cannot infer statically at Tier A
            None
        } else if func.r#async {
            Some(promise_void())
        } else {
            Some(void_type())
        }
    }
}

// ────────────────────────────────────────────────────────────────────────────
// Free helpers
// ────────────────────────────────────────────────────────────────────────────

/// Map a binary operator to its inferred result type (§1.2 "binary ops" row).
fn infer_binary_op(op: BinaryOperator) -> Type {
    match op {
        // Comparison → boolean
        BinaryOperator::Equality
        | BinaryOperator::Inequality
        | BinaryOperator::StrictEquality
        | BinaryOperator::StrictInequality
        | BinaryOperator::LessThan
        | BinaryOperator::LessEqualThan
        | BinaryOperator::GreaterThan
        | BinaryOperator::GreaterEqualThan
        | BinaryOperator::Instanceof
        | BinaryOperator::In => primitive(Primitive::Bool),

        // Arithmetic → number.
        // Note: `+` is conservatively number here. Detecting string concatenation
        // requires knowing operand types (checker). deno_doc does the same.
        BinaryOperator::Addition
        | BinaryOperator::Subtraction
        | BinaryOperator::Multiplication
        | BinaryOperator::Division
        | BinaryOperator::Remainder
        | BinaryOperator::Exponential => primitive(Primitive::Float(Width::W64)),

        // Bitwise → number
        BinaryOperator::BitwiseAnd
        | BinaryOperator::BitwiseOR
        | BinaryOperator::BitwiseXOR
        | BinaryOperator::ShiftLeft
        | BinaryOperator::ShiftRight
        | BinaryOperator::ShiftRightZeroFill => primitive(Primitive::Float(Width::W64)),

        #[allow(unreachable_patterns)]
        _ => primitive(Primitive::Float(Width::W64)),
    }
}

/// Try to extract a plain name string from a callee expression (for `new X()`).
fn callee_name(expr: &Expression<'_>) -> Option<String> {
    match expr {
        Expression::Identifier(id) => Some(id.name.to_string()),
        Expression::StaticMemberExpression(mem) => {
            // `new a.B()` → "a.B"  (best-effort; Tier B does full resolution)
            let obj = callee_name(&mem.object)?;
            Some(format!("{}.{}", obj, mem.property.name))
        }
        _ => None,
    }
}

/// Best-effort property name from a `PropertyKey`.
fn property_key_name(key: &oxc_ast::ast::PropertyKey<'_>) -> String {
    match key {
        oxc_ast::ast::PropertyKey::StaticIdentifier(id)  => id.name.to_string(),
        oxc_ast::ast::PropertyKey::PrivateIdentifier(id) => format!("#{}", id.name),
        // Inherited Expression variants: try string/number literals
        other => {
            if let Some(expr) = other.as_expression() {
                match expr {
                    Expression::StringLiteral(s)  => return s.value.to_string(),
                    Expression::NumericLiteral(n) => return n.value.to_string(),
                    _ => {}
                }
            }
            // Computed key — use a placeholder name
            "[computed]".to_owned()
        }
    }
}

/// Detect `as const`: a `TSAsExpression` whose annotation is `TSTypeReference`
/// with the single identifier `"const"` (OXC 0.139.0 encoding for `as const`).
fn is_as_const(ann: &oxc_ast::ast::TSType<'_>) -> bool {
    use oxc_ast::ast::{TSType, TSTypeName};
    if let TSType::TSTypeReference(tr) = ann {
        if tr.type_arguments.is_none() {
            if let TSTypeName::IdentifierReference(id) = &tr.type_name {
                return id.name == "const";
            }
        }
    }
    false
}

/// Unwrap one `Promise<T>` layer. If the type is `TypeReference("Promise", [T])`,
/// return `T`. Otherwise return the type unchanged (deno_doc parity; no checker).
fn unwrap_promise(ty: Type) -> Type {
    if let Type::TypeReference(ref tr) = ty {
        if tr.identifier == "Promise" {
            if let Some(ref args) = tr.generic_args {
                if let Some(GenericArg::Type(inner)) = args.first() {
                    return inner.clone();
                }
            }
        }
    }
    ty
}

/// Construct a single unnamed output `Parameter::Literal` wrapping a `Type`.
/// Used to build the `outputs` slot of a `FunctionPointer`.
fn unnamed_output(ty: Type) -> Parameter {
    Parameter::Literal(LiteralParameter {
        name:          String::new(),
        r#type:        Some(ty),
        attributes:    None,
        default_value: None,
        description:   None,
    })
}

/// Extract a best-effort name string from a `BindingPattern` for use in
/// anonymous arrow/function parameters.
fn pattern_name(pat: &oxc_ast::ast::BindingPattern<'_>) -> String {
    match pat {
        oxc_ast::ast::BindingPattern::BindingIdentifier(id) => id.name.to_string(),
        oxc_ast::ast::BindingPattern::ObjectPattern(_)      => "{…}".to_owned(),
        oxc_ast::ast::BindingPattern::ArrayPattern(_)       => "[…]".to_owned(),
        oxc_ast::ast::BindingPattern::AssignmentPattern(ap) => pattern_name(&ap.left),
    }
}

// ────────────────────────────────────────────────────────────────────────────
// `as const` detection note
// ────────────────────────────────────────────────────────────────────────────
//
// In OXC 0.139.0, `expr as const` is represented as:
//   Expression::TSAsExpression {
//       expression: <inner>,
//       type_annotation: TSType::TSTypeReference(TSTypeReference {
//           type_name: TSTypeName::IdentifierReference(IdentifierReference { name: "const" }),
//           type_arguments: None,
//       }),
//   }
//
// `is_as_const()` above detects exactly this shape, causing the `TSAsExpression`
// arm to re-infer the inner expression with `is_const = true` instead of lowering
// the annotation as a TypeReference("const"). This matches deno_doc's intent of
// treating `x as const` as a widening-prevention hint.
