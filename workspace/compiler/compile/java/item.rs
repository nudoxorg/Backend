//! Lowering type declarations into `ir::kind::Entry` values.
//!
//! Shape decisions (Java form → IR form):
//!
//! * **class** → `Entry::RecordType`. Fields stay inline `Record.fields`
//!   (annotations + non-access modifiers as decorators, `final` →
//!   immutable, compile-time constants as `default_value`); constructors
//!   fill `Record.constructors`; methods become standalone
//!   `Entry::Function` symbols (same-named overloads folded into one
//!   `Function` with `overloads`) listed in `Record.members` alongside the
//!   nested types — nesting provenance is the dotted key spine
//!   (`pkg::Outer.Inner`). Extends/implements land in `super_types`
//!   (structural), and interfaces that resolve within the extraction are
//!   additionally wired into `implemented_protocols` by path.
//! * **interface** → `Entry::TraitDef`. `abstract` methods →
//!   `required_methods`; `default`, `static`, and `private` methods (all
//!   body-bearing) → `provided_methods` with
//!   `has_default_implementation: true` — the receiver distinguishes
//!   `static` from `default`. Interface constants → `properties`
//!   (`Field::Known`, static + immutable, value in `default_value`) rather
//!   than `required_constants`, which has no documentation slot — Java
//!   interface constants are *provided*, and this keeps their docs,
//!   annotations, and values. Super-interfaces → `super_traits`.
//! * **enum** → `Entry::SumType` over its constants. Constructor arguments
//!   and constant class bodies exist only in source; the oracle ships a
//!   raw declaration window per constant and [`parse_constant_source`]
//!   recovers the balanced `(args)` group and `{` body opener — both are
//!   recorded on the variant documentation (`Arguments:` / body note),
//!   since `SumVariant` has no payload slot for either. `SumType` carries
//!   no member slots, so the enum's methods and fields become standalone
//!   entries alongside it (`Entry::Function` / `Entry::Field`); enum
//!   constructors are deliberately dropped — they are uncallable outside
//!   the constant declarations, whose arguments are already captured.
//! * **record** → `Entry::RecordType` with one immutable field per record
//!   component (component docs fall back to the class-level `@param`);
//!   the private backing fields (same names) are elided as duplicates.
//!   Canonical/compact and secondary constructors fill
//!   `Record.constructors`; accessors and other methods are standalone
//!   functions like class methods.
//! * **annotation type** (`@interface`) → `Entry::TraitDef` tagged
//!   `TraitAttribute::Custom("annotation_interface")`; elements become
//!   `required_methods`, an element default marking
//!   `has_default_implementation` (and noted as `Default:` text — the
//!   trait vocabulary has no default-value slot for methods).
//! * **sealed** — a sealed *interface* is fully structural:
//!   `TraitAttribute::Sealed` plus a `Custom("permits", [...])` attribute
//!   carrying the qualified permitted subtypes. A sealed *class/record*
//!   has no attribute slot on `Record`, so its permitted subtypes ride the
//!   documentation (`Sealed; permits: ...`) — a documented lossy spot.
//! * **type-level annotations** → structural `TraitAttribute::Custom` on
//!   traits; a labelled `Annotations:` documentation section on records
//!   (no slot). `@FunctionalInterface` → `TraitAttribute::Functional`.
//! * **visibility** — `public`/`protected`/`private`/none →
//!   `Public`/`Protected`/`Private`/`Package` (see `types::visibility`).
//!
//! `SYNTHETIC`-origin members (compiler artifacts) are dropped; `MANDATED`
//! members (default constructors, enum `values`/`valueOf`, record members)
//! are kept — they are real, callable API.

use ir::entry::NudoxPath;
use ir::generics::TraitRef;
use ir::kind::{Entry, Symbol, Visibility};
use ir::protocols::{TraitAttribute, TraitDef};
use ir::record::{Field, FieldAttributes, FieldKey, KnownField, Record, SumVariant};

use super::context::Lowering;
use super::function;
use super::javadoc::ParsedJavadoc;
use super::schema;
use super::types;

/// Lower one type declaration into `(path, entry)` pairs: the type's own
/// entry plus its standalone member entries (method groups, enum fields).
pub fn lower_type_decl(
	ctx: &Lowering<'_>,
	decl: &schema::TypeDecl,
) -> Vec<(NudoxPath, Entry)> {
	match decl.kind.as_str() {
		"INTERFACE" => lower_interface(ctx, decl, false),
		"ANNOTATION_TYPE" => lower_interface(ctx, decl, true),
		"ENUM" => lower_enum(ctx, decl),
		// CLASS and RECORD share the record shape.
		_ => lower_class_like(ctx, decl),
	}
}

// ---------------------------------------------------------------------------
// Shared symbol plumbing
// ---------------------------------------------------------------------------

/// Parse a declaration's own doc comment against the extraction resolver.
fn parse_decl_doc(ctx: &Lowering<'_>, decl: &schema::TypeDecl) -> Option<ParsedJavadoc> {
	ctx.parse_doc(decl.doc.as_deref(), decl.doc_kind.as_deref(), Some(&decl.qualified_name))
}

/// Assemble a type entry's documentation: prose, tag sections preserved
/// from the parse (`@since`/`@author`/`@version`/`@see`), deprecation, and
/// — where the entry kind has no structural slot — annotations and sealing.
fn type_documentation(
	decl: &schema::TypeDecl,
	parsed: Option<&ParsedJavadoc>,
	include_annotations: bool,
	include_permits: bool,
) -> Option<String> {
	let mut sections: Vec<String> = Vec::new();

	if let Some(parsed) = parsed {
		if let Some(text) = parsed.documentation() {
			sections.push(text);
		}
		// `@param <T>` on a generic type has no `Generics` slot.
		if let Some(section) = parsed.type_params_section() {
			sections.push(section);
		}
		sections.extend(tag_sections(parsed));
	}

	if let Some(section) = deprecation_section(decl.deprecated, parsed) {
		sections.push(section);
	}

	if include_annotations && !decl.annotations.is_empty() {
		let rendered: Vec<String> = decl
			.annotations
			.iter()
			.map(|a| format!("`{}`", types::render_annotation(a)))
			.collect();
		sections.push(format!("Annotations: {}", rendered.join(", ")));
	}

	if include_permits && !decl.permits.is_empty() {
		let names: Vec<String> = decl
			.permits
			.iter()
			.map(|p| format!("`{}`", types::type_expr(p).name))
			.collect();
		sections.push(format!("Sealed; permitted subtypes: {}", names.join(", ")));
	}

	if sections.is_empty() { None } else { Some(sections.join("\n\n")) }
}

/// `@since` / `@version` / `@author` / `@see` have no structural `Symbol`
/// slot; preserve them as labelled sections.
pub(crate) fn tag_sections(parsed: &ParsedJavadoc) -> Vec<String> {
	let mut sections = Vec::new();
	if let Some(since) = &parsed.since {
		sections.push(format!("Since: {since}"));
	}
	if let Some(version) = &parsed.version {
		sections.push(format!("Version: {version}"));
	}
	if !parsed.authors.is_empty() {
		sections.push(format!("Authors: {}", parsed.authors.join(", ")));
	}
	if !parsed.see.is_empty() {
		let refs: Vec<String> = parsed.see.iter().map(|s| format!("`{s}`")).collect();
		sections.push(format!("See also: {}", refs.join(", ")));
	}
	sections
}

/// A deprecation section from the element flag and/or the `@deprecated` tag.
fn deprecation_section(flagged: bool, parsed: Option<&ParsedJavadoc>) -> Option<String> {
	let text = parsed.and_then(|p| p.deprecated.clone());
	match (flagged, text) {
		(_, Some(text)) if !text.is_empty() => Some(format!("Deprecated: {text}")),
		(true, _) | (_, Some(_)) => Some("Deprecated.".to_string()),
		(false, None) => None,
	}
}

/// Whether a member origin marks a compiler artifact to drop.
fn is_synthetic(origin: Option<&str>) -> bool {
	origin == Some("SYNTHETIC")
}

// ---------------------------------------------------------------------------
// Fields
// ---------------------------------------------------------------------------

/// Lower one field declaration into an inline record field.
fn lower_field(
	ctx: &Lowering<'_>,
	decl: &schema::TypeDecl,
	f: &schema::Field,
) -> Field {
	let parsed =
		ctx.parse_doc(f.doc.as_deref(), f.doc_kind.as_deref(), Some(&decl.qualified_name));

	let mut decorators: Vec<String> =
		f.annotations.iter().map(types::render_annotation).collect();
	for m in &f.modifiers {
		if matches!(m.as_str(), "static" | "final" | "transient" | "volatile") {
			decorators.push(m.clone());
		}
	}

	let mut doc_sections: Vec<String> = Vec::new();
	if let Some(text) = parsed.as_ref().and_then(ParsedJavadoc::documentation) {
		doc_sections.push(text);
	}
	if let Some(section) = deprecation_section(f.deprecated, parsed.as_ref()) {
		doc_sections.push(section);
	}

	Field::Known(KnownField {
		key:           FieldKey::Ident(f.name.clone()),
		r#type:        Some(Box::new(types::lower_type(&f.ty))),
		default_value: f.constant.as_ref().map(types::lower_value),
		attributes:    FieldAttributes {
			decorators,
			is_mutable: !types::has_modifier(&f.modifiers, "final"),
			is_optional: false,
			is_static: types::has_modifier(&f.modifiers, "static"),
		},
		visibility:    Some(types::visibility(&f.modifiers)),
		documentation: if doc_sections.is_empty() {
			None
		} else {
			Some(doc_sections.join("\n\n"))
		},
	})
}

/// Lower a record component into an immutable inline field. Component-site
/// docs win; the class-level `@param <component>` text is the fallback
/// (records are conventionally documented at the class level).
fn lower_record_component(
	ctx: &Lowering<'_>,
	decl: &schema::TypeDecl,
	component: &schema::RecordComponent,
	class_doc: Option<&ParsedJavadoc>,
) -> Field {
	let own = ctx.parse_doc(
		component.doc.as_deref(),
		component.doc_kind.as_deref(),
		Some(&decl.qualified_name),
	);
	let documentation = own
		.as_ref()
		.and_then(ParsedJavadoc::documentation)
		.or_else(|| class_doc.and_then(|p| p.params.get(&component.name).cloned()));

	Field::Known(KnownField {
		key:           FieldKey::Ident(component.name.clone()),
		r#type:        Some(Box::new(types::lower_type(&component.ty))),
		default_value: None,
		attributes:    FieldAttributes {
			decorators:  component
				.annotations
				.iter()
				.map(types::render_annotation)
				.collect(),
			// Record components are final by construction.
			is_mutable:  false,
			is_optional: false,
			is_static:   false,
		},
		visibility:    Some(Visibility::Public),
		documentation,
	})
}

// ---------------------------------------------------------------------------
// Methods (standalone entries)
// ---------------------------------------------------------------------------

/// Group methods by name in first-appearance order, dropping synthetics.
fn method_groups<'m>(methods: &'m [schema::Method]) -> Vec<(&'m str, Vec<&'m schema::Method>)> {
	let mut groups: Vec<(&str, Vec<&schema::Method>)> = Vec::new();
	for m in methods {
		if is_synthetic(m.origin.as_deref()) {
			continue;
		}
		match groups.iter_mut().find(|(name, _)| *name == m.name.as_str()) {
			Some((_, group)) => group.push(m),
			None => groups.push((m.name.as_str(), vec![m])),
		}
	}
	groups
}

/// Emit one standalone `Entry::Function` per method-name group (overloads
/// folded), returning the minted paths for the parent's `members`.
fn push_method_entries(
	ctx: &Lowering<'_>,
	decl: &schema::TypeDecl,
	entries: &mut Vec<(NudoxPath, Entry)>,
) -> Vec<NudoxPath> {
	let mut paths = Vec::new();

	for (name, group) in method_groups(&decl.methods) {
		let path = ctx.member_key(decl, name);
		paths.push(path.clone());

		// The primary signature also names the symbol's docs/visibility.
		let primary = group[0];
		let parsed = ctx.parse_doc(
			primary.doc.as_deref(),
			primary.doc_kind.as_deref(),
			Some(&decl.qualified_name),
		);
		let mut doc_sections: Vec<String> = Vec::new();
		if let Some(text) = parsed.as_ref().and_then(ParsedJavadoc::documentation) {
			doc_sections.push(text);
		}
		if let Some(parsed) = &parsed {
			// `@param <T>` descriptions have no slot on `Generics`.
			if let Some(section) = parsed.type_params_section() {
				doc_sections.push(section);
			}
			doc_sections.extend(tag_sections(parsed));
		}
		// `@throws` prose for types outside the declared `throws` clause.
		if let Some(section) =
			function::undeclared_throws_section(primary, parsed.as_ref())
		{
			doc_sections.push(section);
		}
		if let Some(section) = deprecation_section(primary.deprecated, parsed.as_ref()) {
			doc_sections.push(section);
		}
		doc_sections.extend(function::declaration_notes(primary));

		let lowered: Vec<ir::function::Function> = group
			.iter()
			.map(|m| function::lower_method(ctx, &decl.qualified_name, m))
			.collect();

		entries.push((
			path.clone(),
			Entry::Function(Symbol {
				name:          name.to_string(),
				path,
				aliases:       None,
				visibility:    types::visibility(&primary.modifiers),
				documentation: if doc_sections.is_empty() {
					None
				} else {
					Some(doc_sections.join("\n\n"))
				},
				inner:         function::fold_overloads(lowered),
			}),
		));
	}

	paths
}

/// The nested member types that survive lowering, as paths for `members`.
fn nested_paths(ctx: &Lowering<'_>, decl: &schema::TypeDecl) -> Vec<NudoxPath> {
	decl.nested
		.iter()
		.filter_map(|qualified| ctx.decl(qualified))
		.filter(|nested| matches!(nested.nesting.as_str(), "MEMBER"))
		.map(|nested| ctx.type_key(nested))
		.collect()
}

// ---------------------------------------------------------------------------
// Classes and records
// ---------------------------------------------------------------------------

/// Superclass names that are structural noise rather than information.
const IMPLICIT_SUPERCLASSES: &[&str] =
	&["java.lang.Object", "java.lang.Enum", "java.lang.Record"];

fn lower_class_like(
	ctx: &Lowering<'_>,
	decl: &schema::TypeDecl,
) -> Vec<(NudoxPath, Entry)> {
	let mut entries = Vec::new();
	let path = ctx.type_key(decl);
	let parsed = parse_decl_doc(ctx, decl);
	let is_record = decl.kind == "RECORD";

	// --- Fields. -----------------------------------------------------------
	let mut fields: Vec<Field> = Vec::new();
	if is_record {
		for component in &decl.record_components {
			fields.push(lower_record_component(ctx, decl, component, parsed.as_ref()));
		}
	}
	let component_names: Vec<&str> =
		decl.record_components.iter().map(|c| c.name.as_str()).collect();
	for f in &decl.fields {
		if is_synthetic(f.origin.as_deref()) {
			continue;
		}
		// A record's private backing fields duplicate its components.
		if is_record && component_names.contains(&f.name.as_str()) {
			continue;
		}
		fields.push(lower_field(ctx, decl, f));
	}

	// --- Constructors. -------------------------------------------------------
	let constructors: Vec<ir::function::Function> = decl
		.constructors
		.iter()
		.filter(|c| !is_synthetic(c.origin.as_deref()))
		.map(|c| function::lower_constructor(ctx, &decl.qualified_name, c))
		.collect();

	// --- Methods + nested members. -------------------------------------------
	let mut members = push_method_entries(ctx, decl, &mut entries);
	members.extend(nested_paths(ctx, decl));

	// --- Supertypes. -----------------------------------------------------------
	let mut super_types: Vec<ir::ty::Type> = Vec::new();
	if let Some(superclass) = &decl.superclass {
		let implicit = superclass
			.declared_name()
			.is_some_and(|name| IMPLICIT_SUPERCLASSES.contains(&name));
		if !implicit {
			super_types.push(types::lower_type(superclass));
		}
	}
	for interface in &decl.interfaces {
		super_types.push(types::lower_type(interface));
	}

	// Interfaces resolving inside the extraction are also protocol links.
	let implemented_protocols: Vec<NudoxPath> = decl
		.interfaces
		.iter()
		.filter_map(|i| i.declared_name())
		.filter_map(|name| ctx.decl(name))
		.filter(|target| {
			matches!(target.kind.as_str(), "INTERFACE" | "ANNOTATION_TYPE")
		})
		.map(|target| ctx.type_key(target))
		.collect();

	let record = Record {
		name: Some(decl.simple_name.clone()),
		generics: types::lower_type_params(&decl.type_params),
		fields,
		call_signatures: None,
		constructors: if constructors.is_empty() { None } else { Some(constructors) },
		methods: None,
		index_signatures: None,
		super_types: if super_types.is_empty() { None } else { Some(super_types) },
		members: if members.is_empty() { None } else { Some(members) },
		implemented_protocols: if implemented_protocols.is_empty() {
			None
		} else {
			Some(implemented_protocols)
		},
	};

	entries.push((
		path.clone(),
		Entry::RecordType(Symbol {
			name: decl.simple_name.clone(),
			path,
			aliases: None,
			visibility: types::visibility(&decl.modifiers),
			// Record has no annotation/sealing slots → both ride the docs.
			documentation: type_documentation(decl, parsed.as_ref(), true, true),
			inner: record,
		}),
	));

	entries
}

// ---------------------------------------------------------------------------
// Interfaces and annotation types
// ---------------------------------------------------------------------------

fn lower_interface(
	ctx: &Lowering<'_>,
	decl: &schema::TypeDecl,
	is_annotation: bool,
) -> Vec<(NudoxPath, Entry)> {
	let mut entries = Vec::new();
	let path = ctx.type_key(decl);
	let parsed = parse_decl_doc(ctx, decl);

	// --- Methods: abstract = required; body-bearing = provided. -------------
	let mut required = Vec::new();
	let mut provided = Vec::new();
	for m in &decl.methods {
		if is_synthetic(m.origin.as_deref()) {
			continue;
		}
		// Annotation elements with a declared default count as provided.
		let has_body = !types::has_modifier(&m.modifiers, "abstract")
			|| (is_annotation && m.annotation_default.is_some());
		let lowered = function::trait_method(ctx, &decl.qualified_name, m, has_body);
		if has_body {
			provided.push(lowered);
		} else {
			required.push(lowered);
		}
	}

	// --- Constants → properties (see the module doc). -----------------------
	let properties: Vec<Field> = decl
		.fields
		.iter()
		.filter(|f| !is_synthetic(f.origin.as_deref()))
		.map(|f| lower_field(ctx, decl, f))
		.collect();

	// --- Super-interfaces. -----------------------------------------------------
	let super_traits: Vec<TraitRef> =
		decl.interfaces.iter().map(types::trait_ref).collect();

	// --- Attributes: sealing, functional-ness, annotations. --------------------
	let mut attributes: Vec<TraitAttribute> = Vec::new();
	if is_annotation {
		attributes.push(TraitAttribute::Custom {
			name: "annotation_interface".to_string(),
			args: None,
		});
	}
	if types::has_modifier(&decl.modifiers, "sealed") {
		attributes.push(TraitAttribute::Sealed);
		let permitted: Vec<String> =
			decl.permits.iter().map(|p| types::type_expr(p).name).collect();
		if !permitted.is_empty() {
			attributes.push(TraitAttribute::Custom {
				name: "permits".to_string(),
				args: Some(permitted),
			});
		}
	}
	for annotation in &decl.annotations {
		if annotation.ty == "java.lang.FunctionalInterface" {
			attributes.push(TraitAttribute::Functional);
			continue;
		}
		let args: Vec<String> = annotation
			.values
			.iter()
			.map(|(k, v)| format!("{k} = {}", types::render_value(v)))
			.collect();
		attributes.push(TraitAttribute::Custom {
			name: annotation.ty.clone(),
			args: if args.is_empty() { None } else { Some(args) },
		});
	}

	// --- Nested member types. ----------------------------------------------------
	let members = nested_paths(ctx, decl);

	let trait_def = TraitDef {
		generics: types::lower_type_params(&decl.type_params),
		super_traits: if super_traits.is_empty() { None } else { Some(super_traits) },
		associated_types: None,
		properties: if properties.is_empty() { None } else { Some(properties) },
		required_methods: if required.is_empty() { None } else { Some(required) },
		provided_methods: if provided.is_empty() { None } else { Some(provided) },
		required_constants: None,
		attributes: if attributes.is_empty() { None } else { Some(attributes) },
		members: if members.is_empty() { None } else { Some(members) },
	};

	entries.push((
		path.clone(),
		Entry::TraitDef(Symbol {
			name: decl.simple_name.clone(),
			path,
			aliases: None,
			visibility: types::visibility(&decl.modifiers),
			// Annotations and sealing are structural attributes here, so the
			// docs carry only prose + tags + deprecation.
			documentation: type_documentation(decl, parsed.as_ref(), false, false),
			inner: trait_def,
		}),
	));

	entries
}

// ---------------------------------------------------------------------------
// Enums
// ---------------------------------------------------------------------------

fn lower_enum(ctx: &Lowering<'_>, decl: &schema::TypeDecl) -> Vec<(NudoxPath, Entry)> {
	let mut entries = Vec::new();
	let path = ctx.type_key(decl);
	let parsed = parse_decl_doc(ctx, decl);

	let variants: Vec<SumVariant> = decl
		.enum_constants
		.iter()
		.map(|c| lower_enum_constant(ctx, decl, c))
		.collect();

	// SumType has no member slots: methods and (non-constant) fields become
	// standalone entries alongside the sum.
	push_method_entries(ctx, decl, &mut entries);
	for f in &decl.fields {
		if is_synthetic(f.origin.as_deref()) {
			continue;
		}
		entries.push(enum_field_entry(ctx, decl, f));
	}

	entries.push((
		path.clone(),
		Entry::SumType(Symbol {
			name: decl.simple_name.clone(),
			path,
			aliases: None,
			visibility: types::visibility(&decl.modifiers),
			documentation: type_documentation(decl, parsed.as_ref(), true, false),
			inner: variants,
		}),
	));

	entries
}

/// One enum constant → `SumVariant`; constructor arguments and constant
/// bodies (source-only facts) ride the variant documentation.
fn lower_enum_constant(
	ctx: &Lowering<'_>,
	decl: &schema::TypeDecl,
	c: &schema::EnumConstant,
) -> SumVariant {
	let parsed =
		ctx.parse_doc(c.doc.as_deref(), c.doc_kind.as_deref(), Some(&decl.qualified_name));

	let mut sections: Vec<String> = Vec::new();
	if let Some(text) = parsed.as_ref().and_then(ParsedJavadoc::documentation) {
		sections.push(text);
	}
	if let Some(section) = deprecation_section(c.deprecated, parsed.as_ref()) {
		sections.push(section);
	}
	if !c.annotations.is_empty() {
		let rendered: Vec<String> = c
			.annotations
			.iter()
			.map(|a| format!("`{}`", types::render_annotation(a)))
			.collect();
		sections.push(format!("Annotations: {}", rendered.join(", ")));
	}

	if let Some(window) = &c.source {
		let source = parse_constant_source(window, &c.name);
		if let Some(args) = source.args {
			sections.push(format!("Arguments: `({args})`"));
		}
		if source.has_body {
			sections.push("Declares a constant class body.".to_string());
		}
	}

	SumVariant {
		name:          c.name.clone(),
		data:          None,
		documentation: if sections.is_empty() { None } else { Some(sections.join("\n\n")) },
	}
}

/// An enum's field, emitted standalone (`Entry::Field` carries no payload,
/// so the type is preserved as a `Type:` documentation note).
fn enum_field_entry(
	ctx: &Lowering<'_>,
	decl: &schema::TypeDecl,
	f: &schema::Field,
) -> (NudoxPath, Entry) {
	let path = ctx.member_key(decl, &f.name);
	let parsed =
		ctx.parse_doc(f.doc.as_deref(), f.doc_kind.as_deref(), Some(&decl.qualified_name));

	let mut sections: Vec<String> = Vec::new();
	if let Some(text) = parsed.as_ref().and_then(ParsedJavadoc::documentation) {
		sections.push(text);
	}
	sections.push(format!("Type: `{}`", types::type_display(&f.ty)));
	if let Some(constant) = &f.constant {
		sections.push(format!("Value: `{}`", types::render_value(constant)));
	}
	if let Some(section) = deprecation_section(f.deprecated, parsed.as_ref()) {
		sections.push(section);
	}

	(
		path.clone(),
		Entry::Field(Symbol {
			name: f.name.clone(),
			path,
			aliases: None,
			visibility: types::visibility(&f.modifiers),
			documentation: Some(sections.join("\n\n")),
			inner: (),
		}),
	)
}

// ---------------------------------------------------------------------------
// Enum-constant source parsing
// ---------------------------------------------------------------------------

/// What the raw source window for one enum constant reveals.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EnumConstSource {
	/// The text inside the constructor-argument parens, when present.
	pub args:     Option<String>,
	/// Whether the constant declares a `{ ... }` class body.
	pub has_body: bool,
}

/// Parse an enum-constant declaration window: skip leading annotations
/// (`@Name` / `@Name(...)` — the window starts at the declaration, which
/// includes them), expect the constant name, then read the balanced
/// `(args)` group and check for a `{` body opener. String, char, and
/// comment contents are skipped correctly while balancing; a window that
/// truncates mid-args yields `None` args (never a half group).
pub fn parse_constant_source(window: &str, name: &str) -> EnumConstSource {
	let bytes = window.as_bytes();
	let mut i = 0usize;

	// --- Skip annotations + trivia before the constant name. --------------
	loop {
		i = skip_trivia(window, i);
		if bytes.get(i) == Some(&b'@') {
			i += 1;
			// Dotted annotation name.
			while i < bytes.len()
				&& (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_' || bytes[i] == b'$' || bytes[i] == b'.')
			{
				i += 1;
			}
			let after = skip_trivia(window, i);
			if bytes.get(after) == Some(&b'(') {
				match skip_balanced(window, after) {
					Some(end) => i = end,
					None => return EnumConstSource::default(),
				}
			} else {
				i = after;
			}
			continue;
		}
		break;
	}

	// --- The constant name itself (with an identifier boundary after it,
	// so `LOW` never matches inside `LOWER`). ---------------------------------
	if !window[i..].starts_with(name) {
		return EnumConstSource::default();
	}
	let boundary = bytes.get(i + name.len());
	if boundary.is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_' || *b == b'$') {
		return EnumConstSource::default();
	}
	i += name.len();
	i = skip_trivia(window, i);

	// --- `(args)`. --------------------------------------------------------------
	let mut out = EnumConstSource::default();
	if bytes.get(i) == Some(&b'(') {
		match skip_balanced(window, i) {
			Some(end) => {
				out.args = Some(window[i + 1..end - 1].trim().to_string());
				i = end;
			}
			// Truncated window: report nothing rather than a half group.
			None => return out,
		}
	}
	i = skip_trivia(window, i);

	// --- `{` body opener. ----------------------------------------------------------
	out.has_body = bytes.get(i) == Some(&b'{');
	out
}

/// Advance past whitespace and `/* */` / `//` comments.
fn skip_trivia(text: &str, mut i: usize) -> usize {
	let bytes = text.as_bytes();
	loop {
		while i < bytes.len() && bytes[i].is_ascii_whitespace() {
			i += 1;
		}
		if text[i..].starts_with("//") {
			match text[i..].find('\n') {
				Some(offset) => i += offset + 1,
				None => return bytes.len(),
			}
		} else if text[i..].starts_with("/*") {
			match text[i + 2..].find("*/") {
				Some(offset) => i += 2 + offset + 2,
				None => return bytes.len(),
			}
		} else {
			return i;
		}
	}
}

/// From an opening `(`, return the index just past its matching `)`. String
/// and char literals (with escapes) and comments are opaque. `None` when
/// the window ends before the group closes.
fn skip_balanced(text: &str, open: usize) -> Option<usize> {
	let bytes = text.as_bytes();
	debug_assert_eq!(bytes.get(open), Some(&b'('));
	let mut depth = 0usize;
	let mut i = open;

	while i < bytes.len() {
		match bytes[i] {
			b'(' => {
				depth += 1;
				i += 1;
			}
			b')' => {
				depth -= 1;
				i += 1;
				if depth == 0 {
					return Some(i);
				}
			}
			b'"' | b'\'' => {
				let quote = bytes[i];
				i += 1;
				while i < bytes.len() && bytes[i] != quote {
					if bytes[i] == b'\\' {
						i += 1;
					}
					i += 1;
				}
				i += 1; // Past the closing quote (or end).
			}
			b'/' if text[i..].starts_with("//") || text[i..].starts_with("/*") => {
				i = skip_trivia(text, i);
			}
			_ => i += 1,
		}
	}
	None
}

// ---------------------------------------------------------------------------
// Tests (pure — no oracle required)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn plain_constant() {
		let parsed = parse_constant_source("LOW(1, \"low\"),\n/** next */ MID(5)", "LOW");
		assert_eq!(parsed.args.as_deref(), Some("1, \"low\""));
		assert!(!parsed.has_body);
	}

	#[test]
	fn annotated_constant() {
		let parsed =
			parse_constant_source("@Deprecated\n\tMID(5, \"mid\"),\n\tHIGH(9)", "MID");
		assert_eq!(parsed.args.as_deref(), Some("5, \"mid\""));
		assert!(!parsed.has_body);
	}

	#[test]
	fn annotation_with_args_and_body() {
		let window = "@SuppressWarnings(\"x(\")\nHIGH(9, \"high\") {\n\t@Override\n";
		let parsed = parse_constant_source(window, "HIGH");
		assert_eq!(parsed.args.as_deref(), Some("9, \"high\""));
		assert!(parsed.has_body);
	}

	#[test]
	fn bare_constant() {
		let parsed = parse_constant_source("NORTH,\nSOUTH", "NORTH");
		assert_eq!(parsed.args, None);
		assert!(!parsed.has_body);
	}

	#[test]
	fn tricky_string_args() {
		let parsed =
			parse_constant_source("X(\"a)b\", ')', nested(1, 2)) {", "X");
		assert_eq!(parsed.args.as_deref(), Some("\"a)b\", ')', nested(1, 2)"));
		assert!(parsed.has_body);
	}

	#[test]
	fn truncated_window_degrades() {
		let parsed = parse_constant_source("LONG(1, \"unterminated", "LONG");
		assert_eq!(parsed.args, None);
		assert!(!parsed.has_body);
	}

	#[test]
	fn name_mismatch_is_empty() {
		assert_eq!(
			parse_constant_source("OTHER(1)", "LOW"),
			EnumConstSource::default()
		);
	}
}
