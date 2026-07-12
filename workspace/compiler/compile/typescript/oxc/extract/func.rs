//! Functions, methods, constructors, call/construct signatures, parameters,
//! receivers and overloads → `ir::function::Function` (OXC-PORT-SPEC §1.3/§1.4).
//!
//! LEAF FILE — fill the `todo!()` bodies. Every parameter/return type lowered
//! here must push its resolvable references into `self.type_ref_scratch` so the
//! driver can attach them to the produced `FactEntry` for `type_links`.

use oxc_ast::ast::{
	FormalParameter, FormalParameters, Function, TSCallSignatureDeclaration, TSMethodSignature,
};

use ir::{
	function::{Attribute, Function as IrFunction},
	parameter::Parameter,
	protocols::ReceiverKind,
};

use super::{Extractor, Result};

impl<'a> Extractor<'a> {
	/// Lower a `Function` (declaration/expression) into `ir::function::Function`.
	/// `implemented` ← `func.body.is_some()`. Overloads are attached by the
	/// caller (`decl::function_overloads`).
	pub(crate) fn lower_function(&mut self, func: &Function<'a>) -> Result<IrFunction> {
		let _ = func;
		todo!("func.rs: Function → ir::function::Function")
	}

	/// Lower a parameter list, detecting a leading `this` pseudo-parameter as a
	/// receiver (skipped from the list). Mirrors `params_with_receiver`.
	pub(crate) fn lower_params_with_receiver(
		&mut self,
		params: &FormalParameters<'a>,
		default_receiver: Option<ReceiverKind>,
	) -> Result<(Option<ReceiverKind>, Option<Vec<Parameter>>)> {
		let _ = (params, default_receiver);
		todo!("func.rs: FormalParameters → (receiver, params)")
	}

	/// Lower one `FormalParameter` (identifier / rest / assign-default /
	/// array-pattern / object-pattern) into an `ir` [`Parameter`].
	pub(crate) fn lower_param(&mut self, param: &FormalParameter<'a>) -> Result<Parameter> {
		let _ = param;
		todo!("func.rs: FormalParameter → Parameter")
	}

	/// Compute `[Async, Generator]` attributes from a function's flags.
	pub(crate) fn function_attributes(&self, func: &Function<'a>) -> Option<Vec<Attribute>> {
		let _ = func;
		todo!("func.rs: Function flags → attributes")
	}

	/// Lower a constructor (the `Function` value of a `Constructor`
	/// `MethodDefinition`) into `ir::function::Function`, with
	/// `receiver = Static`. Parameter-property params (`accessibility`/
	/// `readonly`) are handled by the class lowering, not synthesized here.
	pub(crate) fn constructor_signature(&mut self, ctor: &Function<'a>) -> Result<IrFunction> {
		let _ = ctor;
		todo!("func.rs: constructor Function → ir::function::Function")
	}

	/// Lower an interface/type-literal call signature `(params): R`.
	pub(crate) fn call_signature_function(
		&mut self,
		sig: &TSCallSignatureDeclaration<'a>,
	) -> Result<IrFunction> {
		let _ = sig;
		todo!("func.rs: TSCallSignatureDeclaration → ir::function::Function")
	}

	/// Lower a `TSMethodSignature` (`key(params): R` / get / set) into
	/// `ir::function::Function`.
	pub(crate) fn method_signature_function(
		&mut self,
		sig: &TSMethodSignature<'a>,
	) -> Result<IrFunction> {
		let _ = sig;
		todo!("func.rs: TSMethodSignature → ir::function::Function")
	}
}
