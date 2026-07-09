//! Lowering methods and constructors into `ir::function::Function` /
//! `ir::protocols::TraitMethod`.
//!
//! Signature semantics preserved here:
//!
//! * **Parameters** — real source names (the oracle reads source, so no
//!   `-parameters` caveats apply), types fully structural, `@param`
//!   descriptions wired in from the parsed javadoc. A varargs method's final parameter keeps its array type (as
//!   the element model reports it) and is marked
//!   `ParameterAttribute::Variadic`; the function itself carries
//!   `Attribute::Variadic`.
//! * **Returns** — the return type becomes a single unnamed output
//!   parameter carrying the `@return` description; `void` methods and
//!   constructors have no return output.
//! * **`throws` (checked exceptions)** — every declared thrown type becomes
//!   an *additional output parameter* named `throws`, marked
//!   `ParameterAttribute::Optional` (raised only sometimes), with the
//!   matching `@throws` javadoc as its description. This is the structural
//!   home: it mirrors Go's error-position outputs, keeps the exception type
//!   fully structural, and is unambiguous — a `throws` output is
//!   distinguished from the return output by its name and Optional mark
//!   (the return output is always first and unnamed). Checked vs unchecked
//!   is not distinguished by the `throws` clause itself (Java allows
//!   declaring unchecked exceptions); both are preserved as declared.
//! * **Receivers** — `static` methods → `ReceiverKind::Static`; instance
//!   methods → `ReceiverKind::SharedRef` (matching the Python producer's
//!   `self` convention; Java receivers are shared object references —
//!   mutability is a property of the object, not the receiver binding).
//!   Constructors carry no receiver (`None`) and live in the record's
//!   `constructors` slot, so no marker is needed.
//! * **Generic methods** — method-level type parameters (with bounds)
//!   lower via `types::lower_type_params` into `Function::generics`.
//! * **Modifiers and annotations** — the IR's `function::Attribute` enum is
//!   closed (no custom variant), so `abstract`/`final`/`synchronized`/
//!   `native`/`strictfp`/`default` and annotation uses are preserved as a
//!   labelled documentation section (`Declared:` / `Annotations:`) — a
//!   documented lossy-slot placement, consistent with the Go producer's
//!   value notes. `abstract` additionally maps to `implemented: false`
//!   (`native` methods count as implemented — the body exists, elsewhere).
//! * **Overloads** — grouping same-named methods into one `Function` with
//!   `overloads` happens in `item`; this module lowers one signature.

use ir::function::{Attribute, Function};
use ir::parameter::{LiteralParameter, Parameter as IrParameter, ParameterAttribute};
use ir::protocols::{ReceiverKind, TraitMethod};
use ir::ty::Type as IrType;

use super::context::Lowering;
use super::javadoc::ParsedJavadoc;
use super::schema;
use super::types;

/// Lower one method declaration into an IR [`Function`].
///
/// `self_type` is the qualified name of the declaring type (for javadoc
/// reference resolution).
pub fn lower_method(ctx: &Lowering<'_>, self_type: &str, m: &schema::Method) -> Function {
	let parsed = ctx.parse_doc(m.doc.as_deref(), m.doc_kind.as_deref(), Some(self_type));
	build_function(m, parsed.as_ref(), receiver_of(m))
}

/// Lower one constructor declaration into an IR [`Function`] (no receiver,
/// no return output — the produced instance is implied by the constructor
/// slot it is stored in).
pub fn lower_constructor(
	ctx: &Lowering<'_>,
	self_type: &str,
	m: &schema::Method,
) -> Function {
	let parsed = ctx.parse_doc(m.doc.as_deref(), m.doc_kind.as_deref(), Some(self_type));
	build_function(m, parsed.as_ref(), None)
}

/// `static` → `Static`; instance → `SharedRef` (see the module doc).
fn receiver_of(m: &schema::Method) -> Option<ReceiverKind> {
	if types::has_modifier(&m.modifiers, "static") {
		Some(ReceiverKind::Static)
	} else {
		Some(ReceiverKind::SharedRef)
	}
}

/// Assemble the [`Function`] shared by methods and constructors.
fn build_function(
	m: &schema::Method,
	parsed: Option<&ParsedJavadoc>,
	receiver: Option<ReceiverKind>,
) -> Function {
	let inputs = lower_params(&m.params, m.varargs, parsed);
	let outputs = lower_outputs(m, parsed);

	let attributes =
		if m.varargs { Some(vec![Attribute::Variadic]) } else { None };

	Function {
		input_parameters: if inputs.is_empty() { None } else { Some(inputs) },
		output_parameters: if outputs.is_empty() { None } else { Some(outputs) },
		type_links: None,
		attributes,
		generics: types::lower_type_params(&m.type_params),
		receiver,
		// Same-named siblings are folded in by `item`.
		overloads: None,
		implemented: !types::has_modifier(&m.modifiers, "abstract"),
		members: None,
		implemented_protocols: None,
		body: None,
	}
}

/// Lower formal parameters, marking the final one variadic when the method
/// is, and attaching `@param` descriptions.
pub fn lower_params(
	params: &[schema::Param],
	varargs: bool,
	parsed: Option<&ParsedJavadoc>,
) -> Vec<IrParameter> {
	let last = params.len().saturating_sub(1);
	params
		.iter()
		.enumerate()
		.map(|(idx, p)| {
			let is_variadic = varargs && idx == last;
			IrParameter::Literal(LiteralParameter {
				name:          p.name.clone(),
				r#type:        Some(types::lower_type(&p.ty)),
				attributes:    if is_variadic {
					Some(vec![ParameterAttribute::Variadic])
				} else {
					None
				},
				default_value: None,
				description:   parsed
					.and_then(|doc| doc.params.get(&p.name))
					.cloned(),
			})
		})
		.collect()
}

/// Lower the output side: the return value (first, unnamed) followed by one
/// `throws` output per declared thrown type (see the module doc).
fn lower_outputs(m: &schema::Method, parsed: Option<&ParsedJavadoc>) -> Vec<IrParameter> {
	let mut outputs = Vec::new();

	if let Some(ret) = &m.return_type {
		if !matches!(ret, schema::TypeMirror::Void) {
			outputs.push(IrParameter::Literal(LiteralParameter {
				name:          String::new(),
				r#type:        Some(types::lower_type(ret)),
				attributes:    None,
				default_value: None,
				description:   parsed.and_then(|doc| doc.returns.clone()),
			}));
		}
	}

	for thrown in &m.thrown {
		outputs.push(IrParameter::Literal(LiteralParameter {
			name:          "throws".to_string(),
			r#type:        Some(types::lower_type(thrown)),
			attributes:    Some(vec![ParameterAttribute::Optional]),
			default_value: None,
			description:   throws_description(thrown, parsed),
		}));
	}

	outputs
}

/// Match a thrown type against the parsed `@throws` entries. The javadoc
/// side resolves references to qualified names where possible, so we
/// compare on the qualified name first and fall back to the simple name.
fn throws_description(
	thrown: &schema::TypeMirror,
	parsed: Option<&ParsedJavadoc>,
) -> Option<String> {
	let parsed = parsed?;
	let qualified = thrown.declared_name()?;
	let simple = types::simple_name(qualified);
	parsed
		.throws
		.iter()
		.find(|(reference, _)| {
			let ref_type = reference.split('#').next().unwrap_or(reference);
			ref_type == qualified || types::simple_name(ref_type) == simple
		})
		.map(|(_, text)| text.clone())
		.filter(|text| !text.is_empty())
}

/// The declaration notes appended to a method symbol's documentation:
/// non-access modifiers and annotation uses have no structural IR slot on
/// `Function` (its `Attribute` enum is closed), so they ride as labelled
/// text — the documented lossy placement.
pub fn declaration_notes(m: &schema::Method) -> Vec<String> {
	let mut notes = Vec::new();

	let interesting: Vec<&str> = m
		.modifiers
		.iter()
		.map(String::as_str)
		.filter(|m| {
			matches!(
				*m,
				"static" | "final" | "abstract" | "synchronized" | "native"
					| "strictfp" | "default"
			)
		})
		.collect();
	if !interesting.is_empty() {
		notes.push(format!("Declared: `{}`", interesting.join(" ")));
	}

	if !m.annotations.is_empty() {
		let rendered: Vec<String> =
			m.annotations.iter().map(|a| format!("`{}`", types::render_annotation(a))).collect();
		notes.push(format!("Annotations: {}", rendered.join(", ")));
	}

	if let Some(default) = &m.annotation_default {
		notes.push(format!("Default: `{}`", types::render_value(default)));
	}

	notes
}

/// A labelled section for `@throws` entries that name types *not* in the
/// declared `throws` clause (unchecked exceptions documented but not
/// declared). Declared thrown types already carry their descriptions on
/// the structural `throws` outputs; without this section the
/// javadoc-only entries would be lost.
pub fn undeclared_throws_section(
	m: &schema::Method,
	parsed: Option<&ParsedJavadoc>,
) -> Option<String> {
	let parsed = parsed?;
	let declared: Vec<&str> = m.thrown.iter().filter_map(|t| t.declared_name()).collect();

	let mut lines = Vec::new();
	for (reference, text) in &parsed.throws {
		let ref_type = reference.split('#').next().unwrap_or(reference);
		let is_declared = declared.iter().any(|d| {
			*d == ref_type || types::simple_name(d) == types::simple_name(ref_type)
		});
		if is_declared {
			continue;
		}
		if text.is_empty() {
			lines.push(format!("- `{ref_type}`"));
		} else {
			lines.push(format!("- `{ref_type}` — {text}"));
		}
	}

	if lines.is_empty() {
		None
	} else {
		Some(format!("Throws (undeclared):\n{}", lines.join("\n")))
	}
}

/// Project a method onto the trait/interface vocabulary.
///
/// `provided` marks methods that ship a body (interface `default`, `static`,
/// and `private` methods); it drives `has_default_implementation`. The
/// return slot is the bare return type (`None` for `void`); the `throws`
/// contract cannot ride a bare type, so for trait methods it is appended to
/// the method documentation (`Throws:` note) — the full structural form
/// remains available wherever the same method lowers as a `Function`.
pub fn trait_method(
	ctx: &Lowering<'_>,
	self_type: &str,
	m: &schema::Method,
	provided: bool,
) -> TraitMethod {
	let parsed = ctx.parse_doc(m.doc.as_deref(), m.doc_kind.as_deref(), Some(self_type));

	let parameters = lower_params(&m.params, m.varargs, parsed.as_ref());
	let return_type: Option<Box<IrType>> = m.return_type.as_ref().and_then(|ret| {
		if matches!(ret, schema::TypeMirror::Void) {
			None
		} else {
			Some(Box::new(types::lower_type(ret)))
		}
	});

	let mut doc_sections: Vec<String> = Vec::new();
	if let Some(text) = parsed.as_ref().and_then(ParsedJavadoc::documentation) {
		doc_sections.push(text);
	}
	// A trait method's return slot is a bare type; keep the `@return` prose.
	if let Some(returns) = parsed.as_ref().and_then(|p| p.returns.clone()) {
		doc_sections.push(format!("Returns: {returns}"));
	}
	if let Some(section) = parsed.as_ref().and_then(ParsedJavadoc::type_params_section) {
		doc_sections.push(section);
	}
	if !m.thrown.is_empty() {
		let mut lines = Vec::new();
		for thrown in &m.thrown {
			let expr = types::type_expr(thrown);
			match throws_description(thrown, parsed.as_ref()) {
				Some(text) => lines.push(format!("- `{}` — {text}", expr.name)),
				None => lines.push(format!("- `{}`", expr.name)),
			}
		}
		doc_sections.push(format!("Throws:\n{}", lines.join("\n")));
	}
	if let Some(section) = undeclared_throws_section(m, parsed.as_ref()) {
		doc_sections.push(section);
	}
	doc_sections.extend(declaration_notes(m));

	TraitMethod {
		name: m.name.clone(),
		parameters: if parameters.is_empty() { None } else { Some(parameters) },
		return_type,
		generics: types::lower_type_params(&m.type_params),
		attributes: if m.varargs { Some(vec![Attribute::Variadic]) } else { None },
		documentation: if doc_sections.is_empty() {
			None
		} else {
			Some(doc_sections.join("\n\n"))
		},
		receiver: receiver_of(m),
		has_default_implementation: provided,
	}
}

/// Fold a group of same-named signatures into one primary [`Function`]
/// whose `overloads` carries every branch (including the primary), mirroring
/// the Python producer's overload shape.
pub fn fold_overloads(mut group: Vec<Function>) -> Function {
	if group.len() == 1 {
		return group.pop().expect("len checked");
	}
	let mut primary = group.first().cloned().expect("non-empty overload group");
	primary.overloads = Some(group);
	primary
}
