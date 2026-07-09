//! Lowering static lambda information into `ir::function::Function`.
//!
//! The Nix static layer knows:
//!
//! * The *name* and *default expression* of each formal (from the CST).
//! * Whether the outermost pattern has an ellipsis (`...`).
//! * Per-formal `# argname` doc comments (legacy convention).
//! * The optional `::` type signature extracted from the doc block.
//!
//! What it does NOT know at this layer:
//!
//! * Which formals are actually *required* at runtime (no evaluation).
//! * The real return type (unless the doc sig provides it).
//! * Whether the function is pure, strict, lazy, etc.
//!
//! The dynamic layer (fusion) can override the resulting [`Function`]
//! in place; the fields intentionally leave room for that.

use ir::function::Function;
use ir::generics::ConstExpr;
use ir::parameter::{LiteralParameter, Parameter, ParameterAttribute};
use ir::ty::Type;

use super::docs::ParsedDoc;
use super::sig::{self, Signature};
use super::syntax::{LambdaInfo, ParamKind};

// ──────────────────────────────────────────────────────────────────────────
// Public API
// ──────────────────────────────────────────────────────────────────────────

/// Lower a static [`LambdaInfo`] into an IR [`Function`].
///
/// Parameters are built from `lam.params` in declaration order.  When a
/// `sig` is supplied the corresponding positional type (`sig.params[i]`)
/// is attached; otherwise the parameter type is left `None`.
///
/// An ellipsis (`...`) on the outermost pattern appends a synthetic
/// variadic parameter named `"..."` with [`ParameterAttribute::Variadic`].
///
/// The output parameter list is built from `sig.ret` when a signature is
/// present; otherwise `output_parameters` is `None`.
pub fn lower_lambda(
	lam:  &LambdaInfo,
	doc:  &ParsedDoc,
	sig:  Option<&Signature>,
) -> Function {
	let mut inputs: Vec<Parameter> = Vec::new();

	for (i, param) in lam.params.iter().enumerate() {
		// Type: positional from sig, or None.
		let ty: Option<Type> = sig
			.and_then(|s| s.params.get(i))
			.cloned();

		// Default: raw source text kept verbatim as a `ConstExpr::Str`.
		let default_value: Option<ConstExpr> = param
			.default
			.as_ref()
			.map(|src| ConstExpr::Str(src.clone()));

		// Description: formal-level doc first, then the doc-block arg table.
		let description: Option<String> = param
			.doc
			.clone()
			.or_else(|| doc.arg_docs.get(&param.name).cloned());

		// Optional flag: a default makes the formal optional at call-sites.
		let attributes: Option<Vec<ParameterAttribute>> =
			if default_value.is_some() {
				Some(vec![ParameterAttribute::Optional])
			} else {
				None
			};

		inputs.push(Parameter::Literal(LiteralParameter {
			name: param.name.clone(),
			r#type: ty,
			attributes,
			default_value,
			description,
		}));
	}

	// Ellipsis: add a synthetic `...` parameter for the open pattern.
	if lam.ellipsis {
		inputs.push(Parameter::Literal(LiteralParameter {
			name:          "...".to_string(),
			r#type:        None,
			attributes:    Some(vec![ParameterAttribute::Variadic]),
			default_value: None,
			description:   Some(
				"Additional attributes accepted via pattern ellipsis.".to_string(),
			),
		}));
	}

	// Output: the declared return type from the signature.
	let output_parameters: Option<Vec<Parameter>> = sig.map(|s| {
		vec![Parameter::Literal(LiteralParameter {
			name:          String::new(),
			r#type:        Some(s.ret.clone()),
			attributes:    None,
			default_value: None,
			description:   None,
		})]
	});

	Function {
		input_parameters:      if inputs.is_empty() { None } else { Some(inputs) },
		output_parameters,
		type_links:            None,
		attributes:            None,
		generics:              None,
		receiver:              None,
		overloads:             None,
		implemented:           true,
		members:               None,
		implemented_protocols: None,
		body:                  None,
	}
}

/// Try to parse a `::` type signature from the `type_sig` field of a
/// [`ParsedDoc`], returning `None` on any parse failure.
///
/// `item::lower_static` calls this to obtain the optional `sig` argument
/// for [`lower_lambda`].
pub fn signature_from_doc(doc: &ParsedDoc) -> Option<Signature> {
	let raw = doc.type_sig.as_deref()?;
	sig::parse(raw)
}
