//! Lowering methods, constructors, operators, and conversions into
//! `ir::function::Function` / `ir::protocols::TraitMethod` (CSHARP-PLAN §3.7).
//!
//! Signature semantics preserved here (Java-precedent reused verbatim where it
//! fits C#):
//!
//! * **Parameters** — real source names, structural types, `<param>`
//!   descriptions. `ref` → `Inout`; `out` → `Inout` (a `(out)` note keeps the
//!   distinction — Track A); `in` / `ref readonly` → `Borrowing`; optional →
//!   `Optional` + `default_value`; `params` → `Variadic` + `Attribute::Variadic`.
//! * **Returns** — the return type becomes a single unnamed output carrying the
//!   `<returns>` text; `void` returns and constructors have no output.
//! * **Exceptions** — each `<exception cref>` becomes an additional output
//!   named `throws`, marked `Optional`, with its text as description (exact
//!   Java throws encoding; C# exceptions are docs-only).
//! * **Receivers** — `static` → `Static`; instance → `SharedRef`. Constructors
//!   carry no receiver and live in the record's `constructors` slot.
//! * **Async / iterators** — `Attribute::Async` / `Attribute::Generator`.
//! * **Operators** — name = metadata `op_*`; implicit/explicit/checked-ness
//!   rides a `Declared:` doc note (Track A; `Attribute` is a closed enum).
//! * **Overloads** — grouping same-named methods into one `Function` with
//!   `overloads` happens in `item`; this module lowers one signature.

use ir::function::{Attribute, Function};
use ir::parameter::{LiteralParameter, Parameter as IrParameter, ParameterAttribute};
use ir::protocols::{ReceiverKind, TraitMethod};
use ir::ty::Type as IrType;

use super::schema;
use super::types;
use super::xmldoc::ParsedDoc;

/// Lower one method/operator/conversion into an IR [`Function`].
pub fn lower_method(m: &schema::Method) -> Function {
	let parsed = super::xmldoc::parse_opt(m.doc.as_deref());
	build_function(m, parsed.as_ref(), receiver_of(m))
}

/// Lower one constructor into an IR [`Function`] (no receiver, no return
/// output — the produced instance is implied by the constructor slot).
pub fn lower_constructor(m: &schema::Method) -> Function {
	let parsed = super::xmldoc::parse_opt(m.doc.as_deref());
	build_function(m, parsed.as_ref(), None)
}

/// `static` → `Static`; instance → `SharedRef` (matching the Java/Python `self`
/// convention — mutability is a property of the object, not the binding).
fn receiver_of(m: &schema::Method) -> Option<ReceiverKind> {
	if m.is_static {
		Some(ReceiverKind::Static)
	} else {
		Some(ReceiverKind::SharedRef)
	}
}

/// Assemble the [`Function`] shared by methods and constructors.
fn build_function(
	m: &schema::Method,
	parsed: Option<&ParsedDoc>,
	receiver: Option<ReceiverKind>,
) -> Function {
	let has_params = m.parameters.iter().any(|p| p.is_params);
	let inputs = lower_params_ext(&m.parameters, parsed, m.is_extension_method);
	let outputs = lower_outputs(m, parsed);

	let mut attributes: Vec<Attribute> = Vec::new();
	if m.is_async {
		attributes.push(Attribute::Async);
	}
	if m.is_iterator {
		attributes.push(Attribute::Generator);
	}
	if has_params {
		attributes.push(Attribute::Variadic);
	}
	// `unsafe`: no symbol flag — inferred from a pointer in the signature.
	if signature_is_unsafe(m) {
		attributes.push(Attribute::Unsafe);
	}

	Function {
		input_parameters: if inputs.is_empty() { None } else { Some(inputs) },
		output_parameters: if outputs.is_empty() { None } else { Some(outputs) },
		type_links: None,
		attributes: if attributes.is_empty() { None } else { Some(attributes) },
		generics: types::lower_type_params(&m.type_params),
		receiver,
		// Same-named siblings are folded in by `item`.
		overloads: None,
		implemented: !m.is_abstract,
		members: None,
		implemented_protocols: None,
	}
}

/// Whether any pointer / function-pointer occurs in the signature (→ `unsafe`).
fn signature_is_unsafe(m: &schema::Method) -> bool {
	fn ty_has_pointer(t: &schema::TypeSig) -> bool {
		match t {
			schema::TypeSig::Pointer { .. } | schema::TypeSig::FuncPtr { .. } => true,
			schema::TypeSig::Named { args, .. } => args.iter().any(ty_has_pointer),
			schema::TypeSig::Array { element, .. } => ty_has_pointer(element),
			schema::TypeSig::NullableValue { inner } => ty_has_pointer(inner),
			schema::TypeSig::Tuple { elements, .. } => elements.iter().any(|e| ty_has_pointer(&e.ty)),
			_ => false,
		}
	}
	m.parameters.iter().any(|p| ty_has_pointer(&p.ty))
		|| m.return_type.as_ref().is_some_and(ty_has_pointer)
}

/// Lower formal parameters, attaching modifiers and `<param>` descriptions.
///
/// When `extension_receiver` is true the first parameter is the C# `this`
/// extension receiver and is stamped with an `(extension)` description note.
pub fn lower_params(params: &[schema::Param], parsed: Option<&ParsedDoc>) -> Vec<IrParameter> {
	lower_params_ext(params, parsed, false)
}

/// Like [`lower_params`], with an explicit extension-method flag.
pub fn lower_params_ext(
	params: &[schema::Param],
	parsed: Option<&ParsedDoc>,
	is_extension: bool,
) -> Vec<IrParameter> {
	params
		.iter()
		.enumerate()
		.map(|(i, p)| {
			let mut attrs: Vec<ParameterAttribute> = Vec::new();
			let mut note: Option<&str> = None;
			match p.ref_kind.as_str() {
				"ref" => attrs.push(ParameterAttribute::Inout),
				"out" => {
					attrs.push(ParameterAttribute::Inout);
					note = Some("out");
				}
				"in" => attrs.push(ParameterAttribute::Borrowing),
				"refReadonly" => {
					attrs.push(ParameterAttribute::Borrowing);
					note = Some("ref readonly");
				}
				_ => {}
			}
			if p.is_params {
				attrs.push(ParameterAttribute::Variadic);
			}
			if p.has_default {
				attrs.push(ParameterAttribute::Optional);
			}

			let mut description = parsed.and_then(|d| d.params.get(&p.name).cloned());
			// Extension receiver: first formal of a classic extension method.
			if is_extension && i == 0 {
				description = Some(match description {
					Some(d) => format!("(extension) {d}"),
					None => "(extension)".to_string(),
				});
			}
			if let Some(n) = note {
				description = Some(match description {
					Some(d) => format!("({n}) {d}"),
					None => format!("({n})"),
				});
			}

			IrParameter::Literal(LiteralParameter {
				name:          p.name.clone(),
				r#type:        Some(types::lower_type(&p.ty)),
				attributes:    if attrs.is_empty() { None } else { Some(attrs) },
				default_value: p.default.as_deref().map(types::lower_constant),
				description,
			})
		})
		.collect()
}

/// Lower the output side: the return value (first, unnamed) followed by one
/// `throws` output per documented `<exception>`.
///
/// `ref` / `ref readonly` returns wrap the type in [`IrType::BorrowedRef`]
/// (mutable for `ref`, immutable for `ref readonly`) so the return-by-ref
/// flag is structural, not only a doc note (H8).
fn lower_outputs(m: &schema::Method, parsed: Option<&ParsedDoc>) -> Vec<IrParameter> {
	let mut outputs = Vec::new();

	if let Some(ret) = &m.return_type {
		if !is_void(ret) {
			let base = types::lower_type(ret);
			let r#type = if m.returns_by_ref {
				// `ref T` → mutable borrow; `ref readonly T` → immutable.
				IrType::BorrowedRef {
					lifetime:   None,
					is_mutable: !m.returns_by_ref_readonly,
					r#type:     Box::new(base),
				}
			} else {
				base
			};
			outputs.push(IrParameter::Literal(LiteralParameter {
				name:          String::new(),
				r#type:        Some(r#type),
				attributes:    None,
				default_value: None,
				description:   parsed.and_then(|d| d.returns.clone()),
			}));
		}
	}

	if let Some(parsed) = parsed {
		for (cref, text) in &parsed.exceptions {
			outputs.push(IrParameter::Literal(LiteralParameter {
				name:          "throws".to_string(),
				r#type:        Some(IrType::TypeReference(ir::ty::TypeReference {
					identifier:   cref.clone(),
					generic_args: None,
				})),
				attributes:    Some(vec![ParameterAttribute::Optional]),
				default_value: None,
				description:   if text.is_empty() { None } else { Some(text.clone()) },
			}));
		}
	}

	outputs
}

/// Whether a return type is `void` (the unit type).
fn is_void(t: &schema::TypeSig) -> bool {
	matches!(t, schema::TypeSig::Named { name, .. } if name == "System.Void")
}

/// Declaration notes for a method symbol's documentation: non-access modifiers
/// and operator/conversion kind have no structural slot on `Function` (its
/// `Attribute` enum is closed), so they ride as labelled text.
pub fn declaration_notes(m: &schema::Method) -> Vec<String> {
	let mut notes = Vec::new();

	let mut modifiers: Vec<&str> = Vec::new();
	if m.is_static {
		modifiers.push("static");
	}
	if m.is_abstract {
		modifiers.push("abstract");
	}
	if m.is_virtual {
		modifiers.push("virtual");
	}
	if m.is_override {
		modifiers.push("override");
	}
	if m.is_sealed {
		modifiers.push("sealed");
	}
	if m.is_extern {
		modifiers.push("extern");
	}
	if m.is_readonly {
		modifiers.push("readonly");
	}
	if m.is_extension_method {
		// Classic extension method (`this` receiver) — C4.
		modifiers.push("extension");
	}
	if m.returns_by_ref {
		if m.returns_by_ref_readonly {
			modifiers.push("ref readonly return");
		} else {
			modifiers.push("ref return");
		}
	}
	if signature_is_unsafe(m) {
		modifiers.push("unsafe");
	}
	// Conversion kind (implicit / explicit / checked).
	match m.operator_kind.as_str() {
		"implicit" => modifiers.push("implicit operator"),
		"explicit" => modifiers.push("explicit operator"),
		"checked" => modifiers.push("checked operator"),
		_ => {}
	}
	if !modifiers.is_empty() {
		notes.push(format!("Declared: `{}`", modifiers.join(" ")));
	}

	if let Some(iface) = &m.explicit_interface {
		notes.push(format!("Explicit implementation of `{iface}` (callable via the interface only)."));
	}

	let interesting: Vec<String> = m
		.attributes
		.iter()
		.map(types::render_attribute)
		.filter(|s| !s.is_empty())
		.collect();
	if !interesting.is_empty() {
		notes.push(format!("Attributes: {}", interesting.join(", ")));
	}

	notes
}

/// Project a method onto the trait/interface vocabulary. `provided` (a default
/// interface member — a body-bearing interface method) drives
/// `has_default_implementation`. The `throws` contract cannot ride a bare
/// return type, so exceptions are appended to the documentation.
pub fn trait_method(m: &schema::Method, provided: bool) -> TraitMethod {
	let parsed = super::xmldoc::parse_opt(m.doc.as_deref());

	let parameters = lower_params_ext(&m.parameters, parsed.as_ref(), m.is_extension_method);
	let return_type: Option<Box<IrType>> = m.return_type.as_ref().and_then(|ret| {
		if is_void(ret) {
			None
		} else {
			let base = types::lower_type(ret);
			let ty = if m.returns_by_ref {
				IrType::BorrowedRef {
					lifetime:   None,
					is_mutable: !m.returns_by_ref_readonly,
					r#type:     Box::new(base),
				}
			} else {
				base
			};
			Some(Box::new(ty))
		}
	});

	let mut doc_sections: Vec<String> = Vec::new();
	if let Some(text) = parsed.as_ref().and_then(ParsedDoc::documentation) {
		doc_sections.push(text);
	}
	if let Some(returns) = parsed.as_ref().and_then(|p| p.returns.clone()) {
		doc_sections.push(format!("Returns: {returns}"));
	}
	if let Some(parsed) = &parsed {
		if !parsed.exceptions.is_empty() {
			let mut lines = Vec::new();
			for (cref, text) in &parsed.exceptions {
				if text.is_empty() {
					lines.push(format!("- `{cref}`"));
				} else {
					lines.push(format!("- `{cref}` — {text}"));
				}
			}
			doc_sections.push(format!("Throws:\n{}", lines.join("\n")));
		}
	}
	doc_sections.extend(declaration_notes(m));

	let mut attributes: Vec<Attribute> = Vec::new();
	if m.is_async {
		attributes.push(Attribute::Async);
	}
	if m.is_iterator {
		attributes.push(Attribute::Generator);
	}
	if m.parameters.iter().any(|p| p.is_params) {
		attributes.push(Attribute::Variadic);
	}

	TraitMethod {
		name: method_display_name(m),
		parameters: if parameters.is_empty() { None } else { Some(parameters) },
		return_type,
		generics: types::lower_type_params(&m.type_params),
		attributes: if attributes.is_empty() { None } else { Some(attributes) },
		documentation: if doc_sections.is_empty() {
			None
		} else {
			Some(doc_sections.join("\n\n"))
		},
		receiver: receiver_of(m),
		has_default_implementation: provided,
	}
}

/// The display name for a method group key: the metadata name for ordinary
/// methods; for explicit interface implementations the dotted `IFace.Member`
/// form (kept unique + doc-id-aligned).
pub fn method_display_name(m: &schema::Method) -> String {
	if let Some(iface) = &m.explicit_interface {
		return format!("{}.{}", types::simple_name(iface), m.name);
	}
	m.name.clone()
}

/// Fold a group of same-named signatures into one primary [`Function`] whose
/// `overloads` carries every branch (including the primary).
pub fn fold_overloads(mut group: Vec<Function>) -> Function {
	if group.len() == 1 {
		return group.pop().expect("len checked");
	}
	let mut primary = group.first().cloned().expect("non-empty overload group");
	primary.overloads = Some(group);
	primary
}
