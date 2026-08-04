//! Functions, methods, constructors, call/construct signatures, parameters,
//! receivers and overloads → `ir::function::Function` (OXC-PORT-SPEC §1.3/§1.4).
//!
//! LEAF FILE — implements all `todo!()` bodies originally stubbed.
//!
//! ## Design notes
//!
//! * `type_links` is always `None` here. Pass 2 (`link.rs`) fills `type_links`
//!   by walking `FactEntry.type_refs` — we must NOT populate it in pass 1.
//! * `overloads` is always `None` here. `decl::function_overloads` folds the
//!   non-primary declarations into the primary `IrFunction` after this call.
//! * `body` is always `None` — the IR never carries parsed bodies at this stage.
//! * `implemented` = `func.body.is_some()` (overload signatures have no body).
//!
//! ### Receiver detection
//!
//! A leading `FormalParameter` whose pattern is `BindingPattern::BindingIdentifier`
//! named `"this"` → `ReceiverKind::SharedRef`, skipped from the list.
//! Constructor default receiver is `ReceiverKind::Static`.
//!
//! ### Parameter patterns
//!
//! `BindingPattern` in OXC 0.139.0 is the *outer* enum (no wrapper struct):
//! `BindingPattern::{BindingIdentifier, ObjectPattern, ArrayPattern, AssignmentPattern}`.
//!
//! Important: `FormalParameter` does **not** use `AssignmentPattern` in its
//! `pattern` for defaults — it uses `FormalParameter.initializer` instead.
//! `AssignmentPattern` only appears inside destructuring contexts (e.g.
//! `{x = 1}` inside an `ObjectPattern`). We still match and handle the
//! `AssignmentPattern` arm defensively, but the primary default-value path for
//! function params is `param.initializer`.
//!
//! ### Type annotation
//!
//! `FormalParameter.type_annotation` is the explicit annotation (`x: T`);
//! `FormalParameter.pattern.type_annotation()` is not used — the outer field
//! is the canonical one for parameters.
//!
//! ### Rest parameters
//!
//! `FormalParameters.rest: Option<Box<FormalParameterRest>>` where
//! `FormalParameterRest.rest: BindingRestElement`, and
//! `BindingRestElement.argument: BindingPattern`. The type annotation lives on
//! `FormalParameterRest.type_annotation`.
//!
//! ### Constructor parameter properties
//!
//! A `FormalParameter` with `accessibility.is_some() || readonly` is a
//! parameter property. We still lower it as a regular parameter; `decl.rs`
//! decides whether to also synthesise a class field.

use oxc_ast::ast::{
    BindingPattern, FormalParameter, FormalParameters, Function,
    TSCallSignatureDeclaration, TSMethodSignature,
};
use oxc_span::GetSpan as _;

use ir::{
    function::{Attribute, Function as IrFunction},
    generics::ConstExpr,
    parameter::{LiteralParameter, Parameter, ParameterAttribute},
    pipeline::output_parameters_from_type,
    protocols::ReceiverKind,
};

use super::{Extractor, Result};

impl<'a> Extractor<'a> {
    /// Lower a `Function` (declaration/expression) into `ir::function::Function`.
    ///
    /// `implemented` ← `func.body.is_some()`. Overloads are attached by the
    /// caller (`decl::function_overloads`). `type_links` is left `None` for
    /// pass 2.
    pub(crate) fn lower_function(&mut self, func: &Function<'a>) -> Result<IrFunction> {
        let (receiver, input_parameters) =
            self.lower_params_with_receiver(&func.params, None)?;

        let output_parameters = func
            .return_type
            .as_ref()
            .map(|ann| self.lower_ts_type(&ann.type_annotation))
            .transpose()?
            .and_then(output_parameters_from_type);

        let attributes = self.function_attributes(func);

        let generics = func
            .type_parameters
            .as_ref()
            .map(|tp| self.lower_type_params(tp))
            .transpose()?
            .flatten();

        Ok(IrFunction {
            input_parameters,
            output_parameters,
            type_links: None,
            attributes,
            generics,
            receiver,
            overloads: None,
            implemented: func.body.is_some(),
            members: None,
            implemented_protocols: None,
        })
    }

    /// Lower a parameter list, detecting a leading `this` pseudo-parameter as a
    /// receiver (skipped from the list). Mirrors `params_with_receiver`.
    ///
    /// `default_receiver` is used as the receiver when no `this` param is found
    /// (e.g. `Some(ReceiverKind::Static)` for constructors).
    pub(crate) fn lower_params_with_receiver(
        &mut self,
        params: &FormalParameters<'a>,
        default_receiver: Option<ReceiverKind>,
    ) -> Result<(Option<ReceiverKind>, Option<Vec<Parameter>>)> {
        let mut receiver = default_receiver;
        let mut parsed: Vec<Parameter> = Vec::new();

        for (idx, param) in params.items.iter().enumerate() {
            // First param named `this` → receiver, skip from list.
            if idx == 0 {
                if let BindingPattern::BindingIdentifier(id) = &param.pattern {
                    if id.name.as_str() == "this" {
                        receiver = Some(ReceiverKind::SharedRef);
                        continue;
                    }
                }
            }
            parsed.push(self.lower_param(param)?);
        }

        // Handle rest element (e.g. `...args: T[]`).
        if let Some(rest_param) = &params.rest {
            // Type annotation lives on FormalParameterRest, not on the inner pattern.
            let ty = rest_param
                .type_annotation
                .as_ref()
                .map(|ann| self.lower_ts_type(&ann.type_annotation))
                .transpose()?;

            // Name: extract from the inner BindingRestElement.argument.
            let name = match &rest_param.rest.argument {
                BindingPattern::BindingIdentifier(id) => id.name.to_string(),
                other => other.span().source_text(self.source).to_string(),
            };

            parsed.push(Parameter::Literal(LiteralParameter {
                name,
                r#type: ty,
                attributes: Some(vec![ParameterAttribute::Variadic]),
                default_value: None,
                description: None,
            }));
        }

        let out = if parsed.is_empty() { None } else { Some(parsed) };
        Ok((receiver, out))
    }

    /// Lower one `FormalParameter` into an `ir` [`Parameter`].
    ///
    /// Handles all `BindingPattern` variants:
    /// - `BindingIdentifier` → named literal param; `param.optional` → Optional.
    /// - `ArrayPattern` / `ObjectPattern` → span source text as name.
    /// - `AssignmentPattern` → optional + `default_value` (destructuring default;
    ///   function parameter defaults use `param.initializer` instead).
    ///
    /// If `param.initializer` is set (function parameter default value), the
    /// parameter is optional and `default_value` = `ConstExpr::Var(<rhs text>)`.
    ///
    /// Note: rest params are handled by `lower_params_with_receiver` via
    /// `FormalParameters.rest`; this method is only called for `items` entries.
    pub(crate) fn lower_param(&mut self, param: &FormalParameter<'a>) -> Result<Parameter> {
        // Type annotation: the outer FormalParameter.type_annotation is canonical
        // for function parameters (pattern.type_annotation is for non-param contexts).
        let ty = param
            .type_annotation
            .as_ref()
            .map(|ann| self.lower_ts_type(&ann.type_annotation))
            .transpose()?;

        // Default value from the initializer (function parameter default `f(x = 1)`).
        let (has_initializer, default_value) = if let Some(init) = &param.initializer {
            let text = init.span().source_text(self.source).to_string();
            (true, Some(ConstExpr::Var(text)))
        } else {
            (false, None)
        };

        match &param.pattern {
            BindingPattern::BindingIdentifier(id) => {
                let is_optional = param.optional || has_initializer;
                let attributes =
                    if is_optional { Some(vec![ParameterAttribute::Optional]) } else { None };
                Ok(Parameter::Literal(LiteralParameter {
                    name: id.name.to_string(),
                    r#type: ty,
                    attributes,
                    default_value,
                    description: None,
                }))
            }

            BindingPattern::AssignmentPattern(assign) => {
                // Destructuring default (e.g. `{x = 1}` inside an ObjectPattern).
                // This arm is *not* used for function param defaults in practice
                // (those use `initializer`), but we handle it defensively.
                let inner_name = match &assign.left {
                    BindingPattern::BindingIdentifier(id) => id.name.to_string(),
                    other => other.span().source_text(self.source).to_string(),
                };
                // If we already extracted a type from the outer annotation, use it;
                // otherwise fall through to the inner pattern's annotation.
                let inner_ty = if ty.is_some() {
                    ty
                } else {
                    // Try the left-hand side's annotation.
                    match &assign.left {
                        BindingPattern::BindingIdentifier(id) => {
                            // BindingIdentifier carries no type annotation inline;
                            // the FormalParameter.type_annotation above is the source.
                            let _ = id;
                            None
                        }
                        _ => None,
                    }
                };
                let assign_default = Some(ConstExpr::Var(
                    assign.right.span().source_text(self.source).to_string(),
                ));
                Ok(Parameter::Literal(LiteralParameter {
                    name: inner_name,
                    r#type: inner_ty,
                    attributes: Some(vec![ParameterAttribute::Optional]),
                    default_value: assign_default,
                    description: None,
                }))
            }

            BindingPattern::ArrayPattern(_) => {
                let name = param.pattern.span().source_text(self.source).to_string();
                let is_optional = param.optional || has_initializer;
                let attributes =
                    if is_optional { Some(vec![ParameterAttribute::Optional]) } else { None };
                Ok(Parameter::Literal(LiteralParameter {
                    name,
                    r#type: ty,
                    attributes,
                    default_value,
                    description: None,
                }))
            }

            BindingPattern::ObjectPattern(_) => {
                let name = param.pattern.span().source_text(self.source).to_string();
                let is_optional = param.optional || has_initializer;
                let attributes =
                    if is_optional { Some(vec![ParameterAttribute::Optional]) } else { None };
                Ok(Parameter::Literal(LiteralParameter {
                    name,
                    r#type: ty,
                    attributes,
                    default_value,
                    description: None,
                }))
            }
        }
    }

    /// Compute `[Async, Generator]` attributes from a function's flags.
    ///
    /// Returns `None` when neither flag is set.
    pub(crate) fn function_attributes(&self, func: &Function<'a>) -> Option<Vec<Attribute>> {
        let mut attrs = Vec::new();
        if func.r#async {
            attrs.push(Attribute::Async);
        }
        if func.generator {
            attrs.push(Attribute::Generator);
        }
        if attrs.is_empty() { None } else { Some(attrs) }
    }

    /// Lower a constructor (the `Function` value inside a `Constructor`
    /// `MethodDefinition`) into `ir::function::Function`, with
    /// `receiver = Static`.
    ///
    /// Constructor parameter-properties (`accessibility.is_some() || readonly`)
    /// are noted by the caller in `decl.rs`; we still lower each param normally
    /// so the parameter shape is preserved — match deno's behaviour: constructor-
    /// only, no synthetic class field.
    pub(crate) fn constructor_signature(&mut self, ctor: &Function<'a>) -> Result<IrFunction> {
        // Force default receiver = Static; the `this`-detection still runs in
        // lower_params_with_receiver but would be very unusual for a ctor.
        let (_, input_parameters) =
            self.lower_params_with_receiver(&ctor.params, Some(ReceiverKind::Static))?;

        Ok(IrFunction {
            input_parameters,
            output_parameters: None,
            type_links: None,
            attributes: None,
            generics: None,
            receiver: Some(ReceiverKind::Static),
            overloads: None,
            implemented: ctor.body.is_some(),
            members: None,
            implemented_protocols: None,
        })
    }

    /// Lower an interface / type-literal call signature `(params): R` into
    /// `ir::function::Function`.
    pub(crate) fn call_signature_function(
        &mut self,
        sig: &TSCallSignatureDeclaration<'a>,
    ) -> Result<IrFunction> {
        let (receiver, input_parameters) =
            self.lower_params_with_receiver(&sig.params, None)?;

        let output_parameters = sig
            .return_type
            .as_ref()
            .map(|ann| self.lower_ts_type(&ann.type_annotation))
            .transpose()?
            .and_then(output_parameters_from_type);

        let generics = sig
            .type_parameters
            .as_ref()
            .map(|tp| self.lower_type_params(tp))
            .transpose()?
            .flatten();

        Ok(IrFunction {
            input_parameters,
            output_parameters,
            type_links: None,
            attributes: None,
            generics,
            receiver,
            overloads: None,
            implemented: false,
            members: None,
            implemented_protocols: None,
        })
    }

    /// Lower a `TSMethodSignature` (`key(params): R` / get / set) into
    /// `ir::function::Function`.
    pub(crate) fn method_signature_function(
        &mut self,
        sig: &TSMethodSignature<'a>,
    ) -> Result<IrFunction> {
        let (receiver, input_parameters) =
            self.lower_params_with_receiver(&sig.params, None)?;

        let output_parameters = sig
            .return_type
            .as_ref()
            .map(|ann| self.lower_ts_type(&ann.type_annotation))
            .transpose()?
            .and_then(output_parameters_from_type);

        let generics = sig
            .type_parameters
            .as_ref()
            .map(|tp| self.lower_type_params(tp))
            .transpose()?
            .flatten();

        Ok(IrFunction {
            input_parameters,
            output_parameters,
            type_links: None,
            attributes: None,
            generics,
            receiver,
            overloads: None,
            implemented: false,
            members: None,
            implemented_protocols: None,
        })
    }
}
