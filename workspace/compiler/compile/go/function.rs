//! Lowering Go function and method signatures into `ir::function::Function`.
//!
//! Signature semantics preserved here:
//!
//! * **Multiple returns** — Go's result tuple maps one-to-one onto the
//!   IR's `output_parameters` list; *named* results keep their names.
//! * **Variadics** — `func f(xs ...T)` marks the final parameter with
//!   `ParameterAttribute::Variadic` (its type stays the `[]T` go/types
//!   reports) and the function with `Attribute::Variadic`.
//! * **Receivers** — a pointer receiver (`func (t *T)`) can mutate the
//!   receiver and lowers to `ReceiverKind::MutRef`; a value receiver
//!   (`func (t T)`) operates on its own copy, which is ownership
//!   semantics → `ReceiverKind::Owned`.
//! * **Generics** — a generic function's type parameters (with their
//!   constraint type sets) lower via `types::lower_generics`. Go methods
//!   cannot declare their own type parameters; a method on a generic
//!   type re-binds the *receiver's* parameters (`func (l *List[T])`),
//!   which are carried as unconstrained params here — their constraints
//!   live on the type declaration.

use ir::function::{Attribute, Function};
use ir::generics::{Generics, Kind, Variance};
use ir::parameter::{Parameter as IrParameter, TypeParam, TypeParamOrigin};
use ir::protocols::{ReceiverKind, TraitMethod};

use super::oracle;
use super::types;

/// Lower a package-level `func` declaration into an IR [`Function`].
pub fn lower_func_decl(decl: &oracle::Decl) -> Function {
	let signature = decl.signature.as_ref();
	build_function(signature, types::lower_generics(&decl.type_params), None)
}

/// Lower a method (declared or promoted) into an IR [`Function`] with
/// its receiver kind.
pub fn lower_method(method: &oracle::Method) -> Function {
	build_function(
		method.signature.as_ref(),
		recv_generics(&method.recv_type_params),
		Some(receiver_kind(method.pointer_recv)),
	)
}

/// Value receivers copy (ownership), pointer receivers mutate in place.
pub fn receiver_kind(pointer_recv: bool) -> ReceiverKind {
	if pointer_recv { ReceiverKind::MutRef } else { ReceiverKind::Owned }
}

/// Assemble a [`Function`] from an oracle func-type node.
fn build_function(
	signature: Option<&oracle::Type>,
	generics: Option<Generics>,
	receiver: Option<ReceiverKind>,
) -> Function {
	let (inputs, outputs, variadic) = match signature {
		Some(sig) => {
			let inputs = types::lower_fn_params(&sig.params, sig.variadic);
			let outputs = types::lower_fn_results(&sig.results);
			(inputs, outputs, sig.variadic)
		}
		None => (Vec::new(), Vec::new(), false),
	};

	Function {
		input_parameters:      if inputs.is_empty() { None } else { Some(inputs) },
		output_parameters:     if outputs.is_empty() { None } else { Some(outputs) },
		type_links:            None,
		attributes:            if variadic { Some(vec![Attribute::Variadic]) } else { None },
		generics,
		receiver,
		// Go has no overloading.
		overloads:             None,
		// A Go signature in a compiled package always has a body
		// (assembly-backed stubs are indistinguishable here and equally
		// "implemented" from the caller's perspective).
		implemented:           true,
		members:               None,
		implemented_protocols: None,
	}
}

/// A method's receiver type parameters, re-bound by name.
///
/// Constraints are intentionally absent: they belong to (and are lowered
/// with) the generic *type's* declaration.
fn recv_generics(names: &[String]) -> Option<Generics> {
	if names.is_empty() {
		return None;
	}
	let params = names
		.iter()
		.map(|name| {
			IrParameter::Type(TypeParam {
				name:         Some(name.clone()),
				kind:         Kind::Type,
				variance:     Variance::Invariant,
				default_type: None,
				params:       None,
				origin:       TypeParamOrigin::Free,
			})
		})
		.collect();
	Some(Generics { params, constraints: Vec::new() })
}

/// Project an interface method signature into the protocol vocabulary.
///
/// Interface methods have no receiver mutability in Go (the dynamic
/// value satisfies the set however it likes), so `receiver` is left
/// unset; `has_default_implementation` is always `false` — Go interfaces
/// cannot provide bodies.
pub fn trait_method(sig: &oracle::MethodSig, documentation: Option<String>) -> TraitMethod {
	let (parameters, return_type) = match &sig.signature {
		Some(func) => {
			let params = types::lower_fn_params(&func.params, func.variadic);
			let outputs = types::lower_fn_results(&func.results);
			let return_type = outputs_as_return(outputs);
			(if params.is_empty() { None } else { Some(params) }, return_type)
		}
		None => (None, None),
	};

	TraitMethod {
		name: sig.name.clone(),
		parameters,
		return_type,
		generics: None,
		attributes: None,
		documentation,
		receiver: None,
		has_default_implementation: false,
	}
}

/// Collapse a result list into `TraitMethod`'s single `return_type` slot:
/// one result passes through; multiple results become an IR `Tuple`
/// (positional, names dropped — the slot is a bare type, and full-shape
/// signatures remain available on `Function`-lowered forms).
fn outputs_as_return(outputs: Vec<IrParameter>) -> Option<Box<ir::ty::Type>> {
	let mut result_types: Vec<ir::ty::Type> = outputs
		.into_iter()
		.filter_map(|p| match p {
			IrParameter::Literal(lp) => lp.r#type,
			_ => None,
		})
		.collect();

	match result_types.len() {
		0 => None,
		1 => Some(Box::new(result_types.remove(0))),
		_ => Some(Box::new(ir::ty::Type::Tuple(result_types))),
	}
}
