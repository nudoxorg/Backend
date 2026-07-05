//! IR → Rust surface syntax.
//!
//! Renders individual IR entries back into rustfmt-like Rust declaration
//! snippets: 4-space indentation inside rendered items, one named field per
//! line, single-line unit/empty items, deterministic output. All renderers
//! are infallible and total over their input vocabulary — every IR variant
//! produces *some* canonical surface form.
//!
//! # Path shortening
//!
//! Identifiers in the IR are fully-qualified (`std::vec::Vec`). By default
//! the last path segment is rendered (`Vec`);
//! [`RenderOptions::qualified_paths`] switches to full paths. Shortening is
//! only applied to strings that look like plain paths (identifier characters
//! and `::`), so producer artifacts such as debug-formatted type blobs pass
//! through untouched.
//!
//! # Where-clause policy
//!
//! Bounds are attached inline on the generic parameter when they are simple:
//! a `TraitBound` whose trait reference carries no arguments (`T: Clone`),
//! or a `LifetimeBound` whose left side is a declared type/lifetime
//! parameter (`T: 'a`, `'a: 'b`). Everything else — trait bounds with
//! arguments or associated-type bindings (`T: Iterator<Item = u8>`),
//! associated-type bounds, HRTB-carrying constructs, const-expression
//! bounds, logical predicates, and bounds naming non-parameters — moves to a
//! `where` clause, one constraint per line.
//!
//! # Canonical choices for target-ambiguous / target-foreign IR
//!
//! The IR is a superset of Rust; constructs with no Rust surface render in
//! the least-surprising, structure-preserving notation:
//!
//! - `Visibility`: `Public` → `pub`, `Private` → nothing (the Rust default),
//!   `Internal`/`Protected` → `pub(crate)` (Rust has no `protected`),
//!   `Package` → `pub(super)` (the restriction path is not carried).
//! - `Primitive`: `Int/UInt(Arch)` → `isize`/`usize`; `String` → `str`
//!   (the borrowed primitive, matching the Rust producer's lowering);
//!   `Bytes` → `[u8]`; `Date` → `SystemTime` (`std::time::SystemTime`
//!   qualified); `Address` → `*const c_void`; `Float(W8)` clamps to `f16`
//!   and `Float(Arch)` to `f64` (Rust has no such float widths).
//! - `Type::Any` → `dyn Any` (`dyn core::any::Any` qualified): the closest
//!   named Rust rendering of a top type.
//! - `Type::Union` → members joined with ` | ` (matches Rust's or-pattern
//!   syntax; anonymous untagged unions have no Rust type surface).
//! - `Type::Intersection` → members joined with ` + ` (bound syntax).
//! - `Type::Sum` in type position → inline `enum { A, B(T) }` notation.
//! - `Type::RecordLiteral`: a named record renders as its name; an
//!   anonymous all-index record as a tuple `(A, B)`; an anonymous named-field
//!   record as inline braced notation `{ a: T }`.
//! - `Type::Variadic` → `...T`; a `Variadic` parameter attribute renders
//!   as `name: ...` (Rust's unstable c-variadic form); a function-level
//!   `Variadic` attribute appends a bare `...` argument.
//! - `Type::QualifiedPath` without a trait → `<T>::Name` (with a trait,
//!   the canonical `<T as Trait>::Name`).
//! - TypeScript-origin constructs (`TypeOperator`, `Conditional`, `Mapped`,
//!   `Predicate`) render their structure in TS-style notation verbatim —
//!   there is no honest Rust spelling, and inventing one would be lossier.
//! - Function pointers: HRTB binders are reconstructed from
//!   `Parameter::Lifetime` entries in `FunctionPointer::inputs`
//!   (`for<'a> fn(&'a str)`); the IR does not carry an extern ABI, so no
//!   `extern "…"` is ever emitted; `Const`/`Async` attributes render as
//!   `const fn(…)`/`async fn(…)` prefixes even though Rust pointers cannot
//!   spell them today (information preservation beats grammar pedantry
//!   here); `Pure` has no Rust surface and is dropped; `Generator` renders
//!   as a `gen` qualifier.
//! - Receivers: `Owned` → `self`, `SharedRef` → `&self`, `MutRef` →
//!   `&mut self`, `Static`/absent → no receiver, `Arbitrary` → `self: _`
//!   (the IR does not carry the arbitrary self type).
//! - Struct-field `default_value` renders as `field: Ty = expr` (Rust's
//!   default-field-values syntax). Parameter defaults are *not*
//!   representable in Rust function signatures and are omitted.
//! - Records: `methods`/`constructors`/`call_signatures`/`super_types` are
//!   not part of a Rust struct declaration (methods live in `impl` blocks,
//!   rendered separately) and are ignored; `index_signatures` and
//!   `Field::Pattern` render as `[K]: V` lines, `Field::Unknown` as `..`.
//! - An empty `fields` vector renders as a unit struct (`struct Name;`) —
//!   the IR cannot distinguish `struct S;` from `struct S {}`.
//! - Traits: `Marker` renders as `#[marker]`, `Auto`/`Unsafe` as the
//!   `auto`/`unsafe` keywords, `Custom` as an attribute;
//!   `ObjectSafe`/`Sealed`/`Functional` are semantic properties without
//!   Rust syntax and are dropped. Provided methods (those with a default
//!   implementation) end in ` { ... }`, required methods in `;`.
//!   Trait `properties` (interface-origin) render in field notation
//!   (`name: Ty;`).
//! - Generic parameters with no Rust analogue render structurally:
//!   `Dependent` as `name: Ty`, `Module` as `name: Signature`,
//!   higher-kinded `TypeParam::kind`s are not renderable and are ignored;
//!   `HigherKindedBound` where-items render kinds in `* -> *` notation.
//! - `Constraint::LogicalPredicate` renders with `&&`/`||`/`!(…)` around
//!   its atoms, `FunctionalDependency` as `a b -> c` — diagnostic, not
//!   Rust, and documented as such.
//! - Anonymous names fall back to `_` (unnamed type params, unnamed
//!   records, unnamed value parameters).

use ir::function::{Attribute, Function};
use ir::generics::{BinOp, ConstExpr, Constraint, GenericArg, Generics, Kind, Predicate, Term, TraitRef, TypeExpr, UnaryOp};
use ir::kind::Visibility;
use ir::parameter::{Parameter, ParameterAttribute};
use ir::primitives::{Primitive, Width};
use ir::protocols::{GenericBound, ReceiverKind, TraitAttribute, TraitDef, TraitMethod};
use ir::record::{Field, FieldKey, IndexSignature, Record, SumField, SumVariant};
use ir::ty::{DynTrait, FunctionPointer, MappedType, ModifierPrefix, PolyTrait, PredicateSubject, QualifiedPath, Type, TypeOperator, TypePredicate, TypeReference};

use super::RenderOptions;

/// Indentation used *inside* rendered snippets (rustfmt convention), as
/// opposed to the tabs used by this source file itself.
const INDENT: &str = "    ";

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Render a [`Record`] as a Rust `struct` declaration.
///
/// Field shape selects the struct form: all-`FieldKey::Index` fields render
/// as a tuple struct, no fields as a unit struct, anything else as a named
/// struct with one field per line. Enums are *not* records in this IR — see
/// [`render_sum_type`] for `Entry::SumType` payloads.
pub fn render_record(record: &Record, visibility: &Visibility, opts: &RenderOptions) -> String {
	let name = record.name.as_deref().unwrap_or("_");
	let parts = generics_parts(record.generics.as_ref(), opts);

	let mut out = String::new();
	out.push_str(vis_prefix(visibility));
	out.push_str("struct ");
	out.push_str(name);
	out.push_str(&parts.decl);

	let is_tuple = !record.fields.is_empty()
		&& record.fields.iter().all(
			|f| matches!(f, Field::Known(kf) if matches!(kf.key, FieldKey::Index(_))),
		);

	let has_body_fields =
		!record.fields.is_empty() || record.index_signatures.as_ref().is_some_and(|ix| !ix.is_empty());

	if !has_body_fields {
		// Unit struct. `struct S;` and `struct S {}` are indistinguishable
		// in the IR; `;` is the canonical form.
		if parts.where_items.is_empty() {
			out.push(';');
		} else {
			push_where(&mut out, &parts.where_items, false);
			out.push(';');
		}
	} else if is_tuple && record.index_signatures.is_none() {
		let fields: Vec<String> =
			record.fields.iter().map(|f| tuple_field_str(f, opts)).collect();
		out.push('(');
		out.push_str(&fields.join(", "));
		out.push(')');
		if parts.where_items.is_empty() {
			out.push(';');
		} else {
			push_where(&mut out, &parts.where_items, false);
			out.push(';');
		}
	} else {
		if parts.where_items.is_empty() {
			out.push_str(" {\n");
		} else {
			push_where(&mut out, &parts.where_items, true);
			out.push_str("\n{\n");
		}
		for field in &record.fields {
			push_named_field(&mut out, field, opts);
		}
		if let Some(index_signatures) = &record.index_signatures {
			for ix in index_signatures {
				out.push_str(INDENT);
				out.push_str(&index_signature_str(ix, opts));
				out.push_str(",\n");
			}
		}
		out.push('}');
	}
	out
}

/// Render a sum type (`Entry::SumType` payload) as a Rust `enum`.
///
/// The `Symbol<Vec<SumVariant>>` entry shape carries no generics of its own;
/// callers that know better may pass them explicitly.
pub fn render_sum_type(
	name: &str,
	generics: Option<&Generics>,
	variants: &[SumVariant],
	visibility: &Visibility,
	opts: &RenderOptions,
) -> String {
	let parts = generics_parts(generics, opts);

	let mut out = String::new();
	out.push_str(vis_prefix(visibility));
	out.push_str("enum ");
	out.push_str(name);
	out.push_str(&parts.decl);

	if variants.is_empty() {
		if parts.where_items.is_empty() {
			out.push_str(" {}");
		} else {
			push_where(&mut out, &parts.where_items, true);
			out.push_str("\n{}");
		}
		return out;
	}

	if parts.where_items.is_empty() {
		out.push_str(" {\n");
	} else {
		push_where(&mut out, &parts.where_items, true);
		out.push_str("\n{\n");
	}
	for variant in variants {
		if opts.show_docs {
			if let Some(doc) = &variant.documentation {
				push_doc_lines(&mut out, doc, INDENT);
			}
		}
		out.push_str(INDENT);
		out.push_str(&sum_variant_inline(variant, opts));
		out.push_str(",\n");
	}
	out.push('}');
	out
}

/// Render a [`Function`] as a Rust `fn` signature ending in `;`.
///
/// The name lives on the surrounding `Symbol`, not on the payload, and is
/// passed explicitly. Item-level docs are likewise the caller's concern.
pub fn render_function(
	name: &str,
	function: &Function,
	visibility: &Visibility,
	opts: &RenderOptions,
) -> String {
	let parts = generics_parts(function.generics.as_ref(), opts);
	let attributes = function.attributes.as_deref().unwrap_or(&[]);

	let mut out = String::new();
	out.push_str(vis_prefix(visibility));
	out.push_str(&fn_qualifiers(attributes));
	out.push_str("fn ");
	out.push_str(name);
	out.push_str(&parts.decl);
	out.push('(');
	out.push_str(&fn_params_str(
		function.receiver.as_ref(),
		function.input_parameters.as_deref(),
		attributes,
		opts,
	));
	out.push(')');
	out.push_str(&return_suffix(function.output_parameters.as_deref(), opts));
	if parts.where_items.is_empty() {
		out.push(';');
	} else {
		push_where(&mut out, &parts.where_items, false);
		out.push(';');
	}
	out
}

/// Render a [`TraitMethod`] as a Rust `fn` signature.
///
/// Required methods end in `;`; methods carrying a default implementation
/// end in ` { ... }` (the body itself is not part of the surface IR).
pub fn render_trait_method(method: &TraitMethod, opts: &RenderOptions) -> String {
	let parts = generics_parts(method.generics.as_ref(), opts);
	let attributes = method.attributes.as_deref().unwrap_or(&[]);

	let mut out = String::new();
	if opts.show_docs {
		if let Some(doc) = &method.documentation {
			push_doc_lines(&mut out, doc, "");
		}
	}
	out.push_str(&fn_qualifiers(attributes));
	out.push_str("fn ");
	out.push_str(&method.name);
	out.push_str(&parts.decl);
	out.push('(');
	out.push_str(&fn_params_str(
		method.receiver.as_ref(),
		method.parameters.as_deref(),
		attributes,
		opts,
	));
	out.push(')');
	if let Some(return_type) = &method.return_type {
		out.push_str(" -> ");
		out.push_str(&render_type(return_type, opts));
	}
	if parts.where_items.is_empty() {
		out.push_str(if method.has_default_implementation { " { ... }" } else { ";" });
	} else {
		push_where(&mut out, &parts.where_items, method.has_default_implementation);
		out.push_str(if method.has_default_implementation { "\n{ ... }" } else { ";" });
	}
	out
}

/// Render a [`TraitDef`] as a Rust `trait` declaration.
///
/// Body items are emitted in a fixed order: associated types, constants,
/// properties, required methods, provided methods.
pub fn render_trait(
	name: &str,
	def: &TraitDef,
	visibility: &Visibility,
	opts: &RenderOptions,
) -> String {
	let parts = generics_parts(def.generics.as_ref(), opts);

	let mut out = String::new();
	let mut is_unsafe = false;
	let mut is_auto = false;
	if let Some(attributes) = &def.attributes {
		for attribute in attributes {
			match attribute {
				TraitAttribute::Marker => out.push_str("#[marker]\n"),
				TraitAttribute::Auto => is_auto = true,
				TraitAttribute::Unsafe => is_unsafe = true,
				TraitAttribute::Custom { name, args } => {
					out.push_str("#[");
					out.push_str(name);
					if let Some(args) = args {
						out.push('(');
						out.push_str(&args.join(", "));
						out.push(')');
					}
					out.push_str("]\n");
				}
				// Semantic properties with no Rust spelling.
				TraitAttribute::ObjectSafe
				| TraitAttribute::Sealed
				| TraitAttribute::Functional => {}
			}
		}
	}
	out.push_str(vis_prefix(visibility));
	if is_unsafe {
		out.push_str("unsafe ");
	}
	if is_auto {
		out.push_str("auto ");
	}
	out.push_str("trait ");
	out.push_str(name);
	out.push_str(&parts.decl);
	if let Some(super_traits) = &def.super_traits {
		if !super_traits.is_empty() {
			out.push_str(": ");
			let supers: Vec<String> =
				super_traits.iter().map(|tr| trait_ref_str(tr, opts)).collect();
			out.push_str(&supers.join(" + "));
		}
	}

	let mut items: Vec<String> = Vec::new();
	if let Some(associated_types) = &def.associated_types {
		for at in associated_types {
			let mut item = format!("type {}", at.name);
			if let Some(bounds) = &at.bounds {
				if !bounds.is_empty() {
					let rendered: Vec<String> =
						bounds.iter().map(|b| generic_bound_str(b, opts)).collect();
					item.push_str(": ");
					item.push_str(&rendered.join(" + "));
				}
			}
			if let Some(default_type) = &at.default_type {
				item.push_str(" = ");
				item.push_str(&render_type(default_type, opts));
			}
			item.push(';');
			items.push(item);
		}
	}
	if let Some(constants) = &def.required_constants {
		for constant in constants {
			let mut item =
				format!("const {}: {}", constant.name, render_type(&constant.r#type, opts));
			if let Some(default_value) = &constant.default_value {
				item.push_str(" = ");
				item.push_str(&const_expr_str(default_value, opts));
			}
			item.push(';');
			items.push(item);
		}
	}
	if let Some(properties) = &def.properties {
		for property in properties {
			// Interface-origin construct: Rust traits have no fields.
			items.push(format!("{};", inline_field_str(property, opts)));
		}
	}
	if let Some(methods) = &def.required_methods {
		for method in methods {
			items.push(render_trait_method(method, opts));
		}
	}
	if let Some(methods) = &def.provided_methods {
		for method in methods {
			items.push(render_trait_method(method, opts));
		}
	}

	if items.is_empty() {
		if parts.where_items.is_empty() {
			out.push_str(" {}");
		} else {
			push_where(&mut out, &parts.where_items, true);
			out.push_str("\n{}");
		}
		return out;
	}

	if parts.where_items.is_empty() {
		out.push_str(" {\n");
	} else {
		push_where(&mut out, &parts.where_items, true);
		out.push_str("\n{\n");
	}
	for item in &items {
		for line in item.lines() {
			out.push_str(INDENT);
			out.push_str(line);
			out.push('\n');
		}
	}
	out.push('}');
	out
}

/// Render an [`ir::ty::Type`] in Rust type-position syntax.
///
/// Total over the type algebra; see the module docs for the canonical forms
/// chosen for constructs without a native Rust spelling.
pub fn render_type(ty: &Type, opts: &RenderOptions) -> String {
	match ty {
		Type::TypeReference(reference) => type_reference_str(reference, opts),
		Type::SelfType => "Self".to_string(),
		Type::DynTrait(dyn_trait) => dyn_trait_str(dyn_trait, opts),
		// Higher-kinded `kind` shapes have no Rust surface; the bare
		// parameter name is the only faithful rendering.
		Type::GenericParam(param) => param.name.clone(),
		Type::Primitive(primitive) => primitive_str(primitive, opts),
		Type::FunctionPointer(pointer) => function_pointer_str(pointer, opts),
		Type::Tuple(types) => match types.as_slice() {
			[] => "()".to_string(),
			[single] => format!("({},)", render_type(single, opts)),
			many => {
				let rendered: Vec<String> = many.iter().map(|t| render_type(t, opts)).collect();
				format!("({})", rendered.join(", "))
			}
		},
		Type::RecordLiteral(record) => record_literal_str(record, opts),
		Type::Slice(inner) => format!("[{}]", render_type(inner, opts)),
		Type::Array { r#type, length } => format!("[{}; {}]", render_type(r#type, opts), length),
		Type::ImplTrait(bounds) => {
			let rendered: Vec<String> = bounds.iter().map(|b| generic_bound_str(b, opts)).collect();
			format!("impl {}", rendered.join(" + "))
		}
		Type::Infer => "_".to_string(),
		Type::Never => "!".to_string(),
		// Top type: `dyn Any` is the closest named Rust rendering.
		Type::Any => {
			if opts.qualified_paths { "dyn core::any::Any".to_string() } else { "dyn Any".to_string() }
		}
		Type::RawPointer { is_mutable, r#type } => {
			format!("*{} {}", if *is_mutable { "mut" } else { "const" }, render_type(r#type, opts))
		}
		Type::BorrowedRef { lifetime, is_mutable, r#type } => {
			let mut out = String::from("&");
			if let Some(lt) = lifetime {
				out.push_str(&lifetime_token(lt));
				out.push(' ');
			}
			if *is_mutable {
				out.push_str("mut ");
			}
			out.push_str(&render_type(r#type, opts));
			out
		}
		// Untagged unions have no Rust type surface; ` | ` preserves the
		// structure in or-pattern notation.
		Type::Union(types) => {
			let rendered: Vec<String> = types.iter().map(|t| render_type(t, opts)).collect();
			rendered.join(" | ")
		}
		Type::Intersection(types) => {
			let rendered: Vec<String> = types.iter().map(|t| render_type(t, opts)).collect();
			rendered.join(" + ")
		}
		// Anonymous sum in type position: inline enum notation.
		Type::Sum(variants) => {
			let rendered: Vec<String> =
				variants.iter().map(|v| sum_variant_inline(v, opts)).collect();
			format!("enum {{ {} }}", rendered.join(", "))
		}
		Type::QualifiedPath(path) => qualified_path_str(path, opts),
		Type::Variadic(inner) => format!("...{}", render_type(inner, opts)),
		// TS-origin constructs below: rendered structurally, TS notation.
		Type::TypeOperator(TypeOperator { operator, r#type }) => {
			format!("{} {}", operator, render_type(r#type, opts))
		}
		Type::Conditional(conditional) => format!(
			"{} extends {} ? {} : {}",
			render_type(&conditional.check_type, opts),
			render_type(&conditional.extends_type, opts),
			render_type(&conditional.true_type, opts),
			render_type(&conditional.false_type, opts)
		),
		Type::Mapped(mapped) => mapped_str(mapped, opts),
		Type::Predicate(predicate) => type_predicate_str(predicate, opts),
	}
}

// ---------------------------------------------------------------------------
// Names, paths, lifetimes
// ---------------------------------------------------------------------------

fn vis_prefix(visibility: &Visibility) -> &'static str {
	match visibility {
		Visibility::Public => "pub ",
		Visibility::Private => "",
		// No `protected` in Rust; crate visibility is the closest cage.
		Visibility::Protected | Visibility::Internal => "pub(crate) ",
		// The restriction path is not carried by the IR.
		Visibility::Package => "pub(super) ",
	}
}

/// Last path segment unless fully-qualified output is requested. Only
/// strings made of identifier characters and `::` are shortened, so
/// producer debug blobs survive intact.
fn short_path<'a>(path: &'a str, opts: &RenderOptions) -> &'a str {
	if opts.qualified_paths {
		return path;
	}
	if path.chars().all(|c| c.is_alphanumeric() || c == '_' || c == ':') {
		path.rsplit("::").next().unwrap_or(path)
	} else {
		path
	}
}

/// Lifetimes arrive from producers with their leading `'` (rustdoc keeps
/// it); normalize defensively for producers that strip it.
fn lifetime_token(name: &str) -> String {
	if name.starts_with('\'') { name.to_string() } else { format!("'{name}") }
}

// ---------------------------------------------------------------------------
// Constant expressions
// ---------------------------------------------------------------------------

fn const_expr_str(expr: &ConstExpr, opts: &RenderOptions) -> String {
	match expr {
		ConstExpr::Int(value) => value.to_string(),
		ConstExpr::Float(value) => {
			let rendered = value.to_string();
			if rendered.contains(['.', 'e', 'E']) || rendered.contains("inf") || rendered.contains("NaN")
			{
				rendered
			} else {
				format!("{rendered}.0")
			}
		}
		ConstExpr::Bool(value) => value.to_string(),
		ConstExpr::Str(value) => format!("{value:?}"),
		ConstExpr::Var(name) => name.clone(),
		ConstExpr::BinOp { op, lhs, rhs } => format!(
			"{} {} {}",
			const_operand_str(lhs, opts),
			bin_op_token(op),
			const_operand_str(rhs, opts)
		),
		ConstExpr::UnaryOp { op, operand } => {
			format!("{}{}", unary_op_token(op), const_operand_str(operand, opts))
		}
		ConstExpr::Call { func, args } => {
			let rendered: Vec<String> = args.iter().map(|a| const_expr_str(a, opts)).collect();
			format!("{}({})", func, rendered.join(", "))
		}
		// Rust has no stable type ascription; parenthesized form.
		ConstExpr::Ascription { expr, ty } => {
			format!("({}: {})", const_expr_str(expr, opts), type_expr_str(ty, opts))
		}
	}
}

/// Sub-expressions of operators get parenthesized when composite so the
/// output never depends on precedence reconstruction.
fn const_operand_str(expr: &ConstExpr, opts: &RenderOptions) -> String {
	match expr {
		ConstExpr::BinOp { .. } | ConstExpr::UnaryOp { .. } => {
			format!("({})", const_expr_str(expr, opts))
		}
		_ => const_expr_str(expr, opts),
	}
}

/// Const generic-argument position: literals and plain names go bare
/// (`Foo<4>`, `Foo<N>`), everything else is braced (`Foo<{ N + 1 }>`).
fn const_arg_str(expr: &ConstExpr, opts: &RenderOptions) -> String {
	match expr {
		ConstExpr::Int(_)
		| ConstExpr::Float(_)
		| ConstExpr::Bool(_)
		| ConstExpr::Str(_)
		| ConstExpr::Var(_) => const_expr_str(expr, opts),
		_ => format!("{{ {} }}", const_expr_str(expr, opts)),
	}
}

fn bin_op_token(op: &BinOp) -> &'static str {
	match op {
		BinOp::Add => "+",
		BinOp::Sub => "-",
		BinOp::Mul => "*",
		BinOp::Div => "/",
		BinOp::Rem => "%",
		BinOp::BitAnd => "&",
		BinOp::BitOr => "|",
		BinOp::BitXor => "^",
		BinOp::Shl => "<<",
		BinOp::Shr => ">>",
		BinOp::Eq => "==",
		BinOp::Ne => "!=",
		BinOp::Lt => "<",
		BinOp::Le => "<=",
		BinOp::Gt => ">",
		BinOp::Ge => ">=",
		BinOp::And => "&&",
		BinOp::Or => "||",
	}
}

fn unary_op_token(op: &UnaryOp) -> &'static str {
	match op {
		UnaryOp::Neg => "-",
		UnaryOp::Not => "!",
		UnaryOp::Ref => "&",
		UnaryOp::Deref => "*",
	}
}

// ---------------------------------------------------------------------------
// Type expressions, trait references, kinds
// ---------------------------------------------------------------------------

fn type_expr_str(expr: &TypeExpr, opts: &RenderOptions) -> String {
	let name = short_path(&expr.name, opts);
	if expr.args.is_empty() {
		name.to_string()
	} else {
		let args: Vec<String> = expr.args.iter().map(|a| type_expr_str(a, opts)).collect();
		format!("{}<{}>", name, args.join(", "))
	}
}

fn trait_ref_str(trait_ref: &TraitRef, opts: &RenderOptions) -> String {
	let name = short_path(&trait_ref.name, opts);
	if trait_ref.args.is_empty() {
		name.to_string()
	} else {
		let args: Vec<String> = trait_ref.args.iter().map(|a| type_expr_str(a, opts)).collect();
		format!("{}<{}>", name, args.join(", "))
	}
}

/// Kinds render in classic `* -> *` notation — Rust has no kind syntax.
fn kind_str(kind: &Kind) -> String {
	match kind {
		Kind::Type => "*".to_string(),
		Kind::Constraint => "Constraint".to_string(),
		Kind::Row => "Row".to_string(),
		Kind::Arrow(from, to) => format!("{} -> {}", kind_str(from), kind_str(to)),
		Kind::Var(name) => name.clone(),
	}
}

// ---------------------------------------------------------------------------
// Generic arguments (usage position)
// ---------------------------------------------------------------------------

/// Render an argument list, reordered into legal Rust surface order:
/// lifetimes first, types/consts/modules next (relative order preserved),
/// associated-item bindings last.
fn generic_args_str(args: &[GenericArg], opts: &RenderOptions) -> String {
	let mut lifetimes: Vec<String> = Vec::new();
	let mut middles: Vec<String> = Vec::new();
	let mut bindings: Vec<String> = Vec::new();
	for arg in args {
		match arg {
			GenericArg::Lifetime(_) => lifetimes.push(generic_arg_str(arg, opts)),
			GenericArg::Constraint(_) => bindings.push(generic_arg_str(arg, opts)),
			_ => middles.push(generic_arg_str(arg, opts)),
		}
	}
	lifetimes.extend(middles);
	lifetimes.extend(bindings);
	lifetimes.join(", ")
}

fn generic_arg_str(arg: &GenericArg, opts: &RenderOptions) -> String {
	match arg {
		GenericArg::Type(ty) => render_type(ty, opts),
		GenericArg::ConstExpr(expr) => const_arg_str(expr, opts),
		GenericArg::Lifetime(lt) => lifetime_token(lt),
		GenericArg::Constraint(constraint) => match constraint {
			Constraint::AssociatedItem { name, args, term } => {
				assoc_item_str(name, args.as_deref(), term, opts)
			}
			other => where_item_str(other, opts),
		},
		GenericArg::Module(path) => path.clone(),
	}
}

/// `Item = u8` / `Item: Bound + 'a` associated-item bindings.
fn assoc_item_str(
	name: &str,
	args: Option<&[GenericArg]>,
	term: &Term,
	opts: &RenderOptions,
) -> String {
	let mut out = name.to_string();
	if let Some(args) = args {
		if !args.is_empty() {
			out.push('<');
			out.push_str(&generic_args_str(args, opts));
			out.push('>');
		}
	}
	match term {
		Term::Equality(ty) => {
			out.push_str(" = ");
			out.push_str(&render_type(ty, opts));
		}
		Term::Bound(constraints) => {
			if !constraints.is_empty() {
				let rendered: Vec<String> =
					constraints.iter().map(|c| bound_side_str(c, opts)).collect();
				out.push_str(": ");
				out.push_str(&rendered.join(" + "));
			}
		}
	}
	out
}

/// The right-hand side of a bound, with the constrained parameter elided
/// (used where the subject is implied by position).
fn bound_side_str(constraint: &Constraint, opts: &RenderOptions) -> String {
	match constraint {
		Constraint::TraitBound { trait_ref, .. } | Constraint::ImplicitBound { trait_ref, .. } => {
			trait_ref_str(trait_ref, opts)
		}
		Constraint::LifetimeBound { longer, .. } => lifetime_token(longer),
		other => where_item_str(other, opts),
	}
}

// ---------------------------------------------------------------------------
// Constraints and where clauses
// ---------------------------------------------------------------------------

fn where_item_str(constraint: &Constraint, opts: &RenderOptions) -> String {
	match constraint {
		Constraint::TraitBound { param, trait_ref } => {
			format!("{}: {}", param, trait_ref_str(trait_ref, opts))
		}
		Constraint::AssociatedTypeBound { param, assoc_name, bound } => {
			// The Rust producer encodes equality predicates with
			// `param == assoc_name`; render those as an equality.
			if param == assoc_name {
				format!("{} = {}", param, type_expr_str(bound, opts))
			} else {
				format!("{}::{}: {}", param, assoc_name, type_expr_str(bound, opts))
			}
		}
		Constraint::HigherKindedBound { param, kind } => {
			// No Rust HKT syntax; kind notation preserves the content.
			format!("{}: {}", param, kind_str(kind))
		}
		Constraint::AssociatedItem { name, args, term } => {
			assoc_item_str(name, args.as_deref(), term, opts)
		}
		// Either side may be a lifetime or a type parameter; both arrive
		// verbatim from producers, only the right side is definitely a
		// lifetime.
		Constraint::LifetimeBound { shorter, longer } => {
			format!("{}: {}", shorter, lifetime_token(longer))
		}
		// Aspirational const-generics bound (`where N > 0`); no Rust
		// surface exists yet.
		Constraint::ConstExprBound { param: _, expr } => const_expr_str(expr, opts),
		Constraint::LogicalPredicate { pred } => predicate_str(pred, opts),
		// Haskell functional dependency; diagnostic notation.
		Constraint::FunctionalDependency { sources, determined } => {
			format!("{} -> {}", sources.join(" "), determined.join(" "))
		}
		// Implicit evidence: the closest Rust reading is a plain bound.
		Constraint::ImplicitBound { param, trait_ref } => {
			format!("{}: {}", param, trait_ref_str(trait_ref, opts))
		}
	}
}

fn predicate_str(predicate: &Predicate, opts: &RenderOptions) -> String {
	match predicate {
		Predicate::Atom(constraint) => where_item_str(constraint, opts),
		Predicate::And(parts) => {
			let rendered: Vec<String> = parts.iter().map(|p| predicate_atom_str(p, opts)).collect();
			rendered.join(" && ")
		}
		Predicate::Or(parts) => {
			let rendered: Vec<String> = parts.iter().map(|p| predicate_atom_str(p, opts)).collect();
			rendered.join(" || ")
		}
		Predicate::Not(inner) => format!("!({})", predicate_str(inner, opts)),
	}
}

fn predicate_atom_str(predicate: &Predicate, opts: &RenderOptions) -> String {
	match predicate {
		Predicate::Atom(constraint) => where_item_str(constraint, opts),
		composite => format!("({})", predicate_str(composite, opts)),
	}
}

// ---------------------------------------------------------------------------
// Generics (declaration position)
// ---------------------------------------------------------------------------

struct GenericsParts {
	/// `<...>` including brackets, or empty.
	decl: String,
	/// Constraints demoted to a `where` clause, pre-rendered.
	where_items: Vec<String>,
}

impl GenericsParts {
	fn empty() -> Self {
		Self { decl: String::new(), where_items: Vec::new() }
	}
}

/// Split a generics scope into an angle-bracket parameter list (with simple
/// bounds inlined) and residual where-clause items. See the module docs for
/// the inline-vs-where policy.
fn generics_parts(generics: Option<&Generics>, opts: &RenderOptions) -> GenericsParts {
	let Some(generics) = generics else {
		return GenericsParts::empty();
	};
	if generics.params.is_empty() && generics.constraints.is_empty() {
		return GenericsParts::empty();
	}

	// Slots for inline bounds, in declaration order. Only type and
	// lifetime parameters are eligible targets.
	let mut inline: Vec<(String, Vec<String>)> = generics
		.params
		.iter()
		.filter_map(|param| match param {
			Parameter::Type(tp) => tp.name.clone(),
			Parameter::Lifetime(lp) => Some(lifetime_token(&lp.name)),
			_ => None,
		})
		.map(|name| (name, Vec::new()))
		.collect();

	let mut where_items: Vec<String> = Vec::new();
	for constraint in &generics.constraints {
		let inlined = match constraint {
			Constraint::TraitBound { param, trait_ref } if trait_ref.args.is_empty() => {
				try_inline(&mut inline, param, trait_ref_str(trait_ref, opts))
			}
			Constraint::LifetimeBound { shorter, longer } => {
				try_inline(&mut inline, shorter, lifetime_token(longer))
			}
			_ => false,
		};
		if !inlined {
			where_items.push(where_item_str(constraint, opts));
		}
	}

	let decl = if generics.params.is_empty() {
		String::new()
	} else {
		let rendered: Vec<String> =
			generics.params.iter().map(|p| generic_param_decl(p, &inline, opts)).collect();
		format!("<{}>", rendered.join(", "))
	};

	GenericsParts { decl, where_items }
}

fn try_inline(inline: &mut [(String, Vec<String>)], param: &str, bound: String) -> bool {
	let normalized = lifetime_token(param);
	if let Some(slot) =
		inline.iter_mut().find(|(name, _)| name == param || *name == normalized)
	{
		slot.1.push(bound);
		true
	} else {
		false
	}
}

fn generic_param_decl(
	param: &Parameter,
	inline: &[(String, Vec<String>)],
	opts: &RenderOptions,
) -> String {
	let bounds_for = |name: &str| -> Option<&Vec<String>> {
		inline.iter().find(|(n, _)| n == name).map(|(_, b)| b).filter(|b| !b.is_empty())
	};
	match param {
		Parameter::Lifetime(lp) => {
			let name = lifetime_token(&lp.name);
			match bounds_for(&name) {
				Some(bounds) => format!("{}: {}", name, bounds.join(" + ")),
				None => name,
			}
		}
		Parameter::Type(tp) => {
			let mut out = tp.name.clone().unwrap_or_else(|| "_".to_string());
			if let Some(bounds) = bounds_for(&out) {
				out.push_str(": ");
				out.push_str(&bounds.join(" + "));
			}
			if let Some(default_type) = &tp.default_type {
				out.push_str(" = ");
				out.push_str(&type_expr_str(default_type, opts));
			}
			out
		}
		Parameter::Const(cp) => {
			let mut out = format!("const {}: {}", cp.name, type_expr_str(&cp.r#type, opts));
			if let Some(default_value) = &cp.default_value {
				out.push_str(" = ");
				out.push_str(&const_expr_str(default_value, opts));
			}
			out
		}
		// Dependent/module parameters have no Rust analogue; structural
		// `name: signature` notation.
		Parameter::Dependent(dp) => format!("{}: {}", dp.name, type_expr_str(&dp.r#type, opts)),
		Parameter::Module(mp) => match &mp.signature {
			Some(signature) => format!("{}: {}", mp.name, type_expr_str(signature, opts)),
			None => mp.name.clone(),
		},
		// A value parameter in a generic list should not occur; render it
		// like a function parameter for visibility rather than dropping it.
		Parameter::Literal(_) => value_param_str(param, opts),
	}
}

/// Append `\nwhere\n    item,\n    item` to `out`. With `trailing_comma`
/// the last item also gets a comma (rustfmt style before a `{` block);
/// without it the caller appends `;` directly after the last item.
fn push_where(out: &mut String, items: &[String], trailing_comma: bool) {
	out.push_str("\nwhere");
	for (index, item) in items.iter().enumerate() {
		out.push('\n');
		out.push_str(INDENT);
		out.push_str(item);
		if trailing_comma || index + 1 < items.len() {
			out.push(',');
		}
	}
}

// ---------------------------------------------------------------------------
// Value parameters, receivers, function signatures
// ---------------------------------------------------------------------------

/// Canonical Rust qualifier order: `const async gen unsafe`.
/// `Pure` has no Rust spelling and is dropped; `Variadic` is handled in the
/// parameter list.
fn fn_qualifiers(attributes: &[Attribute]) -> String {
	let mut out = String::new();
	if attributes.contains(&Attribute::Const) {
		out.push_str("const ");
	}
	if attributes.contains(&Attribute::Async) {
		out.push_str("async ");
	}
	if attributes.contains(&Attribute::Generator) {
		out.push_str("gen ");
	}
	if attributes.contains(&Attribute::Unsafe) {
		out.push_str("unsafe ");
	}
	out
}

fn fn_params_str(
	receiver: Option<&ReceiverKind>,
	parameters: Option<&[Parameter]>,
	attributes: &[Attribute],
	opts: &RenderOptions,
) -> String {
	let mut params: Vec<String> = Vec::new();
	match receiver {
		Some(ReceiverKind::Owned) => params.push("self".to_string()),
		Some(ReceiverKind::SharedRef) => params.push("&self".to_string()),
		Some(ReceiverKind::MutRef) => params.push("&mut self".to_string()),
		// The arbitrary self *type* is not carried by the IR.
		Some(ReceiverKind::Arbitrary) => params.push("self: _".to_string()),
		Some(ReceiverKind::Static) | None => {}
	}
	if let Some(parameters) = parameters {
		for parameter in parameters {
			params.push(value_param_str(parameter, opts));
		}
	}
	if attributes.contains(&Attribute::Variadic) {
		params.push("...".to_string());
	}
	params.join(", ")
}

/// A parameter in value position (`name: Type`). Parameter defaults are not
/// representable in Rust signatures and are omitted; the `Mutable` attribute
/// renders as a `mut` binding, `Variadic` as the c-variadic `name: ...`.
/// Remaining calling-convention attributes (`Inout`, `Consuming`, …) have no
/// Rust syntax and are dropped.
fn value_param_str(param: &Parameter, opts: &RenderOptions) -> String {
	match param {
		Parameter::Literal(lp) => {
			let attributes = lp.attributes.as_deref().unwrap_or(&[]);
			let name = if lp.name.is_empty() { "_" } else { lp.name.as_str() };
			if attributes.contains(&ParameterAttribute::Variadic) {
				return format!("{name}: ...");
			}
			let mut out = String::new();
			if attributes.contains(&ParameterAttribute::Mutable) {
				out.push_str("mut ");
			}
			out.push_str(name);
			out.push_str(": ");
			match &lp.r#type {
				Some(ty) => out.push_str(&render_type(ty, opts)),
				None => out.push('_'),
			}
			out
		}
		Parameter::Type(tp) => tp.name.clone().unwrap_or_else(|| "_".to_string()),
		Parameter::Const(cp) => cp.name.clone(),
		Parameter::Lifetime(lp) => lifetime_token(&lp.name),
		Parameter::Dependent(dp) => format!("{}: {}", dp.name, type_expr_str(&dp.r#type, opts)),
		Parameter::Module(mp) => mp.name.clone(),
	}
}

/// Function-pointer arguments: unnamed literals render as a bare type
/// (`fn(u8)`), named ones as `name: Type`.
fn fn_ptr_param_str(param: &Parameter, opts: &RenderOptions) -> String {
	if let Parameter::Literal(lp) = param {
		if lp.name.is_empty() {
			return match &lp.r#type {
				Some(ty) => render_type(ty, opts),
				None => "_".to_string(),
			};
		}
	}
	value_param_str(param, opts)
}

/// ` -> T` for a single output, ` -> (A, B)` for several, nothing for unit.
fn return_suffix(outputs: Option<&[Parameter]>, opts: &RenderOptions) -> String {
	let Some(outputs) = outputs else {
		return String::new();
	};
	if outputs.is_empty() {
		return String::new();
	}
	let types: Vec<String> = outputs.iter().map(|p| output_type_str(p, opts)).collect();
	if types.len() == 1 {
		format!(" -> {}", types[0])
	} else {
		format!(" -> ({})", types.join(", "))
	}
}

fn output_type_str(param: &Parameter, opts: &RenderOptions) -> String {
	match param {
		Parameter::Literal(lp) => match &lp.r#type {
			Some(ty) => render_type(ty, opts),
			None => "_".to_string(),
		},
		other => value_param_str(other, opts),
	}
}

// ---------------------------------------------------------------------------
// Type-level building blocks
// ---------------------------------------------------------------------------

fn type_reference_str(reference: &TypeReference, opts: &RenderOptions) -> String {
	let name = short_path(&reference.identifier, opts);
	match &reference.generic_args {
		Some(args) if !args.is_empty() => format!("{}<{}>", name, generic_args_str(args, opts)),
		_ => name.to_string(),
	}
}

fn primitive_str(primitive: &Primitive, opts: &RenderOptions) -> String {
	match primitive {
		Primitive::Int(width) => match width {
			Width::W8 => "i8",
			Width::W16 => "i16",
			Width::W32 => "i32",
			Width::W64 => "i64",
			Width::W128 => "i128",
			Width::Arch => "isize",
		}
		.to_string(),
		Primitive::UInt(width) => match width {
			Width::W8 => "u8",
			Width::W16 => "u16",
			Width::W32 => "u32",
			Width::W64 => "u64",
			Width::W128 => "u128",
			Width::Arch => "usize",
		}
		.to_string(),
		Primitive::Float(width) => match width {
			// Rust has no 8-bit or arch-sized floats; clamp to the nearest
			// legal width rather than invent syntax.
			Width::W8 | Width::W16 => "f16",
			Width::W32 => "f32",
			Width::W64 | Width::Arch => "f64",
			Width::W128 => "f128",
		}
		.to_string(),
		Primitive::Bool => "bool".to_string(),
		// The borrowed string primitive, matching the Rust producer.
		Primitive::String => "str".to_string(),
		Primitive::Char => "char".to_string(),
		Primitive::Bytes => "[u8]".to_string(),
		Primitive::Date => {
			if opts.qualified_paths {
				"std::time::SystemTime".to_string()
			} else {
				"SystemTime".to_string()
			}
		}
		Primitive::Address => {
			if opts.qualified_paths {
				"*const core::ffi::c_void".to_string()
			} else {
				"*const c_void".to_string()
			}
		}
	}
}

fn dyn_trait_str(dyn_trait: &DynTrait, opts: &RenderOptions) -> String {
	let mut parts: Vec<String> =
		dyn_trait.traits.iter().map(|pt| poly_trait_str(pt, opts)).collect();
	if let Some(lt) = &dyn_trait.lifetime {
		parts.push(lifetime_token(lt));
	}
	format!("dyn {}", parts.join(" + "))
}

fn poly_trait_str(poly: &PolyTrait, opts: &RenderOptions) -> String {
	if poly.lifetimes.is_empty() {
		trait_ref_str(&poly.trait_ref, opts)
	} else {
		let lifetimes: Vec<String> = poly.lifetimes.iter().map(|lt| lifetime_token(lt)).collect();
		format!("for<{}> {}", lifetimes.join(", "), trait_ref_str(&poly.trait_ref, opts))
	}
}

fn generic_bound_str(bound: &GenericBound, opts: &RenderOptions) -> String {
	match bound {
		GenericBound::Trait(trait_ref) => trait_ref_str(trait_ref, opts),
		GenericBound::Lifetime(lt) => lifetime_token(lt),
	}
}

fn function_pointer_str(pointer: &FunctionPointer, opts: &RenderOptions) -> String {
	let attributes = pointer.attributes.as_deref().unwrap_or(&[]);

	// HRTB binder: lifetime parameters among the inputs introduce a
	// `for<...>` scope; the remaining parameters are the arguments.
	let mut binder: Vec<String> = Vec::new();
	let mut args: Vec<String> = Vec::new();
	if let Some(inputs) = &pointer.inputs {
		for input in inputs {
			match input {
				Parameter::Lifetime(lp) => binder.push(lifetime_token(&lp.name)),
				other => args.push(fn_ptr_param_str(other, opts)),
			}
		}
	}

	let mut out = String::new();
	if !binder.is_empty() {
		out.push_str("for<");
		out.push_str(&binder.join(", "));
		out.push_str("> ");
	}
	out.push_str(&fn_qualifiers(attributes));
	out.push_str("fn(");
	out.push_str(&args.join(", "));
	if attributes.contains(&Attribute::Variadic) {
		if !args.is_empty() {
			out.push_str(", ");
		}
		out.push_str("...");
	}
	out.push(')');
	out.push_str(&return_suffix(pointer.outputs.as_deref(), opts));
	out
}

fn qualified_path_str(path: &QualifiedPath, opts: &RenderOptions) -> String {
	let self_type = render_type(&path.self_type, opts);
	let args = match &path.generic_arguments {
		Some(args) if !args.is_empty() => format!("<{}>", generic_args_str(args, opts)),
		_ => String::new(),
	};
	match &path.tr {
		Some(trait_reference) => format!(
			"<{} as {}>::{}{}",
			self_type,
			type_reference_str(trait_reference, opts),
			path.name,
			args
		),
		None => format!("<{}>::{}{}", self_type, path.name, args),
	}
}

fn record_literal_str(record: &Record, opts: &RenderOptions) -> String {
	if let Some(name) = &record.name {
		return short_path(name, opts).to_string();
	}
	if record.fields.is_empty() {
		return "{}".to_string();
	}
	let is_tuple = record
		.fields
		.iter()
		.all(|f| matches!(f, Field::Known(kf) if matches!(kf.key, FieldKey::Index(_))));
	if is_tuple {
		let types: Vec<String> = record
			.fields
			.iter()
			.map(|f| match f {
				Field::Known(kf) => match &kf.r#type {
					Some(ty) => render_type(ty, opts),
					None => "_".to_string(),
				},
				_ => "_".to_string(),
			})
			.collect();
		format!("({})", types.join(", "))
	} else {
		let fields: Vec<String> =
			record.fields.iter().map(|f| inline_field_str(f, opts)).collect();
		format!("{{ {} }}", fields.join(", "))
	}
}

fn mapped_str(mapped: &MappedType, opts: &RenderOptions) -> String {
	let readonly = match &mapped.readonly {
		None => "",
		Some(ModifierPrefix::Preserve) => "readonly ",
		Some(ModifierPrefix::Add) => "+readonly ",
		Some(ModifierPrefix::Remove) => "-readonly ",
	};
	let optional = match &mapped.optional {
		None => "",
		Some(ModifierPrefix::Preserve) => "?",
		Some(ModifierPrefix::Add) => "+?",
		Some(ModifierPrefix::Remove) => "-?",
	};
	let name = match &mapped.name_type {
		Some(name_type) => format!(" as {}", render_type(name_type, opts)),
		None => String::new(),
	};
	let value = match &mapped.value_type {
		Some(value_type) => format!(": {}", render_type(value_type, opts)),
		None => String::new(),
	};
	format!(
		"{{ {}[{} in {}{}]{}{} }}",
		readonly,
		mapped.parameter,
		render_type(&mapped.source_type, opts),
		name,
		optional,
		value
	)
}

fn type_predicate_str(predicate: &TypePredicate, opts: &RenderOptions) -> String {
	let mut out = String::new();
	if predicate.asserts {
		out.push_str("asserts ");
	}
	match &predicate.subject {
		PredicateSubject::This => out.push_str("this"),
		PredicateSubject::Identifier(name) => out.push_str(name),
	}
	if let Some(ty) = &predicate.r#type {
		out.push_str(" is ");
		out.push_str(&render_type(ty, opts));
	}
	out
}

// ---------------------------------------------------------------------------
// Fields and variants
// ---------------------------------------------------------------------------

fn field_key_str(key: &FieldKey, opts: &RenderOptions) -> String {
	match key {
		FieldKey::Ident(name) => name.clone(),
		FieldKey::Index(index) => index.to_string(),
		// Computed keys are a dynamic-language construct.
		FieldKey::Computed(expr) => format!("[{}]", const_expr_str(expr, opts)),
	}
}

fn index_signature_str(signature: &IndexSignature, opts: &RenderOptions) -> String {
	format!(
		"[{}]: {}",
		render_type(&signature.key_type, opts),
		render_type(&signature.value_type, opts)
	)
}

/// Compact `key: Type` form used in enum struct variants, record literals,
/// and trait properties. Visibility, docs, and decorators are intentionally
/// not part of this form.
fn inline_field_str(field: &Field, opts: &RenderOptions) -> String {
	match field {
		Field::Known(kf) => {
			let ty = match &kf.r#type {
				Some(ty) => render_type(ty, opts),
				None => "_".to_string(),
			};
			format!("{}: {}", field_key_str(&kf.key, opts), ty)
		}
		Field::Pattern(signature) => index_signature_str(signature, opts),
		Field::Unknown => "..".to_string(),
	}
}

fn tuple_field_str(field: &Field, opts: &RenderOptions) -> String {
	match field {
		Field::Known(kf) => {
			let vis = kf.visibility.as_ref().map(vis_prefix).unwrap_or("");
			let ty = match &kf.r#type {
				Some(ty) => render_type(ty, opts),
				None => "_".to_string(),
			};
			format!("{vis}{ty}")
		}
		// Unreachable for tuple-classified records (all fields Known).
		Field::Pattern(signature) => index_signature_str(signature, opts),
		Field::Unknown => "..".to_string(),
	}
}

/// One named-struct field, indented one level, trailing comma and newline.
fn push_named_field(out: &mut String, field: &Field, opts: &RenderOptions) {
	match field {
		Field::Known(kf) => {
			if opts.show_docs {
				if let Some(doc) = &kf.documentation {
					push_doc_lines(out, doc, INDENT);
				}
			}
			for decorator in &kf.attributes.decorators {
				out.push_str(INDENT);
				if decorator.starts_with('#') {
					out.push_str(decorator);
				} else {
					out.push_str("#[");
					out.push_str(decorator);
					out.push(']');
				}
				out.push('\n');
			}
			out.push_str(INDENT);
			if let Some(visibility) = &kf.visibility {
				out.push_str(vis_prefix(visibility));
			}
			out.push_str(&field_key_str(&kf.key, opts));
			out.push_str(": ");
			match &kf.r#type {
				Some(ty) => out.push_str(&render_type(ty, opts)),
				None => out.push('_'),
			}
			// Default-field-values syntax; Rust producers leave this None
			// (Default impls live elsewhere), other languages may not.
			if let Some(default_value) = &kf.default_value {
				out.push_str(" = ");
				out.push_str(&const_expr_str(default_value, opts));
			}
			out.push_str(",\n");
		}
		Field::Pattern(signature) => {
			out.push_str(INDENT);
			out.push_str(&index_signature_str(signature, opts));
			out.push_str(",\n");
		}
		Field::Unknown => {
			out.push_str(INDENT);
			out.push_str("..\n");
		}
	}
}

fn sum_variant_inline(variant: &SumVariant, opts: &RenderOptions) -> String {
	match &variant.data {
		None => variant.name.clone(),
		Some(SumField::Tuple(types)) => {
			let rendered: Vec<String> = types.iter().map(|t| render_type(t, opts)).collect();
			format!("{}({})", variant.name, rendered.join(", "))
		}
		Some(SumField::StructLike(fields)) => {
			let rendered: Vec<String> = fields.iter().map(|f| inline_field_str(f, opts)).collect();
			format!("{} {{ {} }}", variant.name, rendered.join(", "))
		}
	}
}

// ---------------------------------------------------------------------------
// Docs
// ---------------------------------------------------------------------------

fn push_doc_lines(out: &mut String, doc: &str, indent: &str) {
	for line in doc.lines() {
		out.push_str(indent);
		out.push_str("///");
		if !line.is_empty() {
			out.push(' ');
			out.push_str(line);
		}
		out.push('\n');
	}
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
	use ir::generics::Variance;
	use ir::parameter::{ConstParam, LifetimeParam, LiteralParameter, TypeParam, TypeParamOrigin};
	use ir::protocols::{AssociatedType, TraitConstant};
	use ir::record::{FieldAttributes, KnownField};
	use ir::ty::GenericParam;

	use super::*;

	fn opts() -> RenderOptions {
		RenderOptions::default()
	}

	fn ty_ref(identifier: &str) -> Type {
		Type::TypeReference(TypeReference { identifier: identifier.to_string(), generic_args: None })
	}

	fn ty_ref_args(identifier: &str, args: Vec<GenericArg>) -> Type {
		Type::TypeReference(TypeReference {
			identifier:   identifier.to_string(),
			generic_args: Some(args),
		})
	}

	fn ty_param(name: &str) -> Type {
		Type::GenericParam(GenericParam { name: name.to_string(), kind: None })
	}

	fn no_field_attributes() -> FieldAttributes {
		FieldAttributes { decorators: vec![], is_mutable: false, is_optional: false, is_static: false }
	}

	fn known_field(name: &str, ty: Type, visibility: Visibility, documentation: Option<&str>) -> Field {
		Field::Known(KnownField {
			key:           FieldKey::Ident(name.to_string()),
			r#type:        Some(Box::new(ty)),
			default_value: None,
			attributes:    no_field_attributes(),
			visibility:    Some(visibility),
			documentation: documentation.map(str::to_string),
		})
	}

	fn tuple_field(index: usize, ty: Type, visibility: Visibility) -> Field {
		Field::Known(KnownField {
			key:           FieldKey::Index(index),
			r#type:        Some(Box::new(ty)),
			default_value: None,
			attributes:    no_field_attributes(),
			visibility:    Some(visibility),
			documentation: None,
		})
	}

	fn record(name: &str, generics: Option<Generics>, fields: Vec<Field>) -> Record {
		Record {
			name: Some(name.to_string()),
			generics,
			fields,
			call_signatures: None,
			constructors: None,
			methods: None,
			index_signatures: None,
			super_types: None,
			members: None,
			implemented_protocols: None,
		}
	}

	fn type_param(name: &str, default_type: Option<TypeExpr>) -> Parameter {
		Parameter::Type(TypeParam {
			name: Some(name.to_string()),
			kind: Kind::Type,
			variance: Variance::Invariant,
			default_type,
			params: None,
			origin: TypeParamOrigin::Free,
		})
	}

	fn lifetime_param(name: &str) -> Parameter {
		Parameter::Lifetime(LifetimeParam { name: name.to_string(), variance: Variance::Invariant })
	}

	fn literal_param(name: &str, ty: Type) -> Parameter {
		Parameter::Literal(LiteralParameter {
			name:          name.to_string(),
			r#type:        Some(ty),
			attributes:    None,
			default_value: None,
			description:   None,
		})
	}

	fn output(ty: Type) -> Vec<Parameter> {
		vec![Parameter::Literal(LiteralParameter {
			name:          String::new(),
			r#type:        Some(ty),
			attributes:    None,
			default_value: None,
			description:   None,
		})]
	}

	fn trait_ref(name: &str) -> TraitRef {
		TraitRef { name: name.to_string(), args: vec![] }
	}

	#[test]
	fn plain_struct() {
		let rec = record("Point", None, vec![
			known_field("x", Type::Primitive(Primitive::Float(Width::W64)), Visibility::Public, None),
			known_field("y", Type::Primitive(Primitive::Float(Width::W64)), Visibility::Public, None),
		]);
		assert_eq!(
			render_record(&rec, &Visibility::Public, &opts()),
			"pub struct Point {\n    pub x: f64,\n    pub y: f64,\n}"
		);
	}

	#[test]
	fn plain_struct_with_field_docs() {
		let rec = record("Point", None, vec![known_field(
			"x",
			Type::Primitive(Primitive::Float(Width::W64)),
			Visibility::Public,
			Some("The x coordinate."),
		)]);
		let opts = RenderOptions { show_docs: true, ..RenderOptions::default() };
		assert_eq!(
			render_record(&rec, &Visibility::Public, &opts),
			"pub struct Point {\n    /// The x coordinate.\n    pub x: f64,\n}"
		);
	}

	#[test]
	fn generic_struct_with_where_clause() {
		// `T: Iterator<Item = u8>` is complex (carries arguments) → where
		// clause; `T: 'a` is simple → inline.
		let generics = Generics {
			params:      vec![lifetime_param("'a"), type_param("T", None)],
			constraints: vec![
				Constraint::TraitBound {
					param:     "T".to_string(),
					trait_ref: TraitRef {
						name: "Iterator".to_string(),
						args: vec![TypeExpr { name: "Item = u8".to_string(), args: vec![] }],
					},
				},
				Constraint::LifetimeBound { shorter: "T".to_string(), longer: "'a".to_string() },
			],
		};
		let rec = record("Wrap", Some(generics), vec![known_field(
			"inner",
			Type::BorrowedRef {
				lifetime:   Some("'a".to_string()),
				is_mutable: false,
				r#type:     Box::new(ty_param("T")),
			},
			Visibility::Public,
			None,
		)]);
		assert_eq!(
			render_record(&rec, &Visibility::Public, &opts()),
			"pub struct Wrap<'a, T: 'a>\nwhere\n    T: Iterator<Item = u8>,\n{\n    pub inner: &'a T,\n}"
		);
	}

	#[test]
	fn struct_with_default_and_const_params() {
		let generics = Generics {
			params:      vec![
				type_param("T", Some(TypeExpr { name: "u8".to_string(), args: vec![] })),
				Parameter::Const(ConstParam {
					name:          "N".to_string(),
					r#type:        TypeExpr { name: "usize".to_string(), args: vec![] },
					default_value: None,
				}),
			],
			constraints: vec![],
		};
		let rec = record("Buf", Some(generics), vec![known_field(
			"items",
			Type::Array { r#type: Box::new(ty_param("T")), length: 8 },
			Visibility::Public,
			None,
		)]);
		assert_eq!(
			render_record(&rec, &Visibility::Public, &opts()),
			"pub struct Buf<T = u8, const N: usize> {\n    pub items: [T; 8],\n}"
		);
	}

	#[test]
	fn tuple_struct() {
		let rec = record("Pair", None, vec![
			tuple_field(0, Type::Primitive(Primitive::UInt(Width::W8)), Visibility::Public),
			tuple_field(1, Type::Primitive(Primitive::Bool), Visibility::Private),
		]);
		assert_eq!(render_record(&rec, &Visibility::Public, &opts()), "pub struct Pair(pub u8, bool);");
	}

	#[test]
	fn unit_struct() {
		let rec = record("Marker", None, vec![]);
		assert_eq!(render_record(&rec, &Visibility::Public, &opts()), "pub struct Marker;");
	}

	#[test]
	fn enum_with_all_variant_shapes() {
		let variants = vec![
			SumVariant { name: "Unit".to_string(), data: None, documentation: None },
			SumVariant {
				name:          "Tup".to_string(),
				data:          Some(SumField::Tuple(vec![
					Type::Primitive(Primitive::UInt(Width::W8)),
					ty_ref("alloc::string::String"),
				])),
				documentation: None,
			},
			SumVariant {
				name:          "Rec".to_string(),
				data:          Some(SumField::StructLike(vec![known_field(
					"id",
					Type::Primitive(Primitive::UInt(Width::W64)),
					Visibility::Public,
					None,
				)])),
				documentation: None,
			},
		];
		assert_eq!(
			render_sum_type("Shape", None, &variants, &Visibility::Public, &opts()),
			"pub enum Shape {\n    Unit,\n    Tup(u8, String),\n    Rec { id: u64 },\n}"
		);
	}

	#[test]
	fn async_method_with_receiver_and_generics() {
		let function = Function {
			input_parameters:      Some(vec![literal_param(
				"limit",
				Type::Primitive(Primitive::UInt(Width::Arch)),
			)]),
			output_parameters:     Some(output(ty_ref_args("alloc::vec::Vec", vec![
				GenericArg::Type(ty_param("T")),
			]))),
			type_links:            None,
			attributes:            Some(vec![Attribute::Async]),
			generics:              Some(Generics {
				params:      vec![type_param("T", None)],
				constraints: vec![Constraint::TraitBound {
					param:     "T".to_string(),
					trait_ref: trait_ref("core::clone::Clone"),
				}],
			}),
			receiver:              Some(ReceiverKind::MutRef),
			overloads:             None,
			implemented:           true,
			members:               None,
			implemented_protocols: None,
			body:                  None,
		};
		assert_eq!(
			render_function("fetch", &function, &Visibility::Public, &opts()),
			"pub async fn fetch<T: Clone>(&mut self, limit: usize) -> Vec<T>;"
		);
	}

	#[test]
	fn trait_with_associated_type_and_const() {
		let def = TraitDef {
			generics:           None,
			super_traits:       Some(vec![trait_ref("core::marker::Send")]),
			associated_types:   Some(vec![AssociatedType {
				name:         "Item".to_string(),
				bounds:       Some(vec![GenericBound::Trait(trait_ref("core::clone::Clone"))]),
				default_type: None,
			}]),
			properties:         None,
			required_methods:   Some(vec![TraitMethod {
				name: "next".to_string(),
				parameters: None,
				return_type: Some(Box::new(ty_ref_args("core::option::Option", vec![
					GenericArg::Type(Type::Primitive(Primitive::UInt(Width::W8))),
				]))),
				generics: None,
				attributes: None,
				documentation: None,
				receiver: Some(ReceiverKind::MutRef),
				has_default_implementation: false,
			}]),
			provided_methods:   Some(vec![TraitMethod {
				name: "reset".to_string(),
				parameters: None,
				return_type: None,
				generics: None,
				attributes: None,
				documentation: None,
				receiver: Some(ReceiverKind::MutRef),
				has_default_implementation: true,
			}]),
			required_constants: Some(vec![TraitConstant {
				name:          "MAX".to_string(),
				r#type:        Box::new(Type::Primitive(Primitive::UInt(Width::Arch))),
				default_value: None,
			}]),
			attributes:         None,
			members:            None,
		};
		assert_eq!(
			render_trait("Source", &def, &Visibility::Public, &opts()),
			"pub trait Source: Send {\n    type Item: Clone;\n    const MAX: usize;\n    fn next(&mut self) -> Option<u8>;\n    fn reset(&mut self) { ... }\n}"
		);
	}

	#[test]
	fn gnarly_nested_type() {
		// Option<Result<&'a mut [T; 4], Box<dyn Error + Send + 'static>>>
		let dyn_error = Type::DynTrait(DynTrait {
			traits:   vec![
				PolyTrait { trait_ref: trait_ref("std::error::Error"), lifetimes: vec![] },
				PolyTrait { trait_ref: trait_ref("core::marker::Send"), lifetimes: vec![] },
			],
			lifetime: Some("'static".to_string()),
		});
		let boxed = ty_ref_args("alloc::boxed::Box", vec![GenericArg::Type(dyn_error)]);
		let array = Type::Array { r#type: Box::new(ty_param("T")), length: 4 };
		let borrow = Type::BorrowedRef {
			lifetime:   Some("'a".to_string()),
			is_mutable: true,
			r#type:     Box::new(array),
		};
		let result = ty_ref_args("core::result::Result", vec![
			GenericArg::Type(borrow),
			GenericArg::Type(boxed),
		]);
		let option = ty_ref_args("core::option::Option", vec![GenericArg::Type(result)]);
		assert_eq!(
			render_type(&option, &opts()),
			"Option<Result<&'a mut [T; 4], Box<dyn Error + Send + 'static>>>"
		);
	}

	#[test]
	fn function_pointer_with_hrtb() {
		let pointer = Type::FunctionPointer(FunctionPointer {
			inputs:     Some(vec![
				lifetime_param("'a"),
				Parameter::Literal(LiteralParameter {
					name:          String::new(),
					r#type:        Some(Type::BorrowedRef {
						lifetime:   Some("'a".to_string()),
						is_mutable: false,
						r#type:     Box::new(Type::Primitive(Primitive::String)),
					}),
					attributes:    None,
					default_value: None,
					description:   None,
				}),
			]),
			outputs:    Some(output(Type::Primitive(Primitive::Bool))),
			attributes: Some(vec![Attribute::Unsafe]),
		});
		assert_eq!(render_type(&pointer, &opts()), "for<'a> unsafe fn(&'a str) -> bool");
	}

	#[test]
	fn qualified_path() {
		let path = Type::QualifiedPath(QualifiedPath {
			name:              "Output".to_string(),
			generic_arguments: None,
			self_type:         Box::new(ty_param("T")),
			tr:                Some(TypeReference {
				identifier:   "core::ops::Add".to_string(),
				generic_args: Some(vec![GenericArg::Type(Type::Primitive(Primitive::UInt(
					Width::W8,
				)))]),
			}),
		});
		assert_eq!(render_type(&path, &opts()), "<T as Add<u8>>::Output");
	}

	#[test]
	fn qualified_paths_flag() {
		let ty = ty_ref("std::vec::Vec");
		assert_eq!(render_type(&ty, &opts()), "Vec");
		let qualified = RenderOptions { qualified_paths: true, ..RenderOptions::default() };
		assert_eq!(render_type(&ty, &qualified), "std::vec::Vec");
	}
}
