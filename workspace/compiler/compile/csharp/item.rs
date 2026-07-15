//! Lowering type declarations into `ir::kind::Entry` values (CSHARP-PLAN §3.6).
//!
//! Shape decisions (C# form → IR form):
//!
//! * **class / struct / record / record struct** → `Entry::RecordType`.
//!   Fields and properties (accessor asymmetry as decorators) stay inline
//!   `Record.fields`; events become standalone `Entry::Event` symbols (delegate
//!   type on `TypedBinding.ty`); constructors fill `Record.constructors`;
//!   methods / operators / conversions become standalone `Entry::Function`
//!   symbols (overloads folded) listed in `Record.members` alongside nested
//!   types and events. Base type + interfaces land in `super_types`; interfaces
//!   resolving in-extraction also wire `implemented_protocols`. The C# form
//!   keyword (class/struct/record/record struct) plus sealed / abstract /
//!   static / readonly / ref ride a `Declared:` doc note (no structural slot).
//! * **interface** → `Entry::TraitDef`. Abstract members → `required_methods`;
//!   default interface members → `provided_methods`; properties →
//!   `TraitDef.properties`; variance + constraints → `Generics`; base
//!   interfaces → `super_traits`.
//! * **enum** → `Entry::SumType` over its members (constant value in the
//!   variant doc); `[Flags]` + underlying type ride the type doc.
//! * **delegate** → `Entry::TypeAlias(Type::FunctionPointer)`.
//! * **forwarded types** → full emit with a `Forwarded` doc marker.

use std::collections::BTreeMap;

use ir::entry::{Entry, NudoxPath};
use ir::function::Function;
use ir::generics::TraitRef;
use ir::kind::{Deprecation, Symbol, TypedBinding};
use ir::parameter::Parameter as IrParameter;
use ir::protocols::{TraitAttribute, TraitDef};
use ir::record::{Field, FieldAttributes, FieldKey, KnownField, Record, SumVariant};
use ir::ty::{FunctionPointer as IrFunctionPointer, Type as IrType};
use rustc_hash::FxHashSet as HashSet;

use super::context::Lowering;
use super::function;
use super::schema;
use super::types;
use super::xmldoc::{self, ParsedDoc};

/// Lower one type declaration into `(path, entry)` pairs: the type's own entry
/// plus its standalone member entries (method groups).
pub fn lower_type_decl(ctx: &Lowering<'_>, decl: &schema::TypeDecl) -> Vec<(NudoxPath, Entry)> {
	match decl.kind.as_str() {
		"INTERFACE" => lower_interface(ctx, decl),
		"ENUM" => lower_enum(ctx, decl),
		"DELEGATE" => lower_delegate(ctx, decl),
		// CLASS / STRUCT / RECORD / RECORD_STRUCT share the record shape.
		_ => lower_class_like(ctx, decl),
	}
}

// ---------------------------------------------------------------------------
// Shared symbol plumbing
// ---------------------------------------------------------------------------

/// The `docId` of a symbol, stored as its sole alias so cref `doc_links` and
/// occurrence work can join on it.
fn doc_id_alias(doc_id: &str) -> Option<HashSet<Vec<String>>> {
	if doc_id.is_empty() {
		return None;
	}
	let mut set = HashSet::default();
	set.insert(vec![doc_id.to_string()]);
	Some(set)
}

/// A deprecation section from an `[Obsolete]` marker (prose for docs).
fn deprecation_section(dep: Option<&schema::Deprecated>) -> Option<String> {
	let dep = dep?;
	let label = if dep.is_error { "Deprecated (error)" } else { "Deprecated" };
	match &dep.message {
		Some(msg) if !msg.is_empty() => Some(format!("{label}: {msg}")),
		_ => Some(format!("{label}.")),
	}
}

/// Map oracle `[Obsolete]` onto the IR [`Deprecation`] field.
///
/// There is no structural slot for `is_error`, so an error-level Obsolete
/// prefixes the note with `"error: "` when a message is present, or sets the
/// note to `"error"` alone when bare.
pub fn deprecation_of(dep: Option<&schema::Deprecated>) -> Option<Deprecation> {
	let dep = dep?;
	let note = match (&dep.message, dep.is_error) {
		(Some(msg), true) if !msg.is_empty() => Some(format!("error: {msg}")),
		(Some(msg), false) if !msg.is_empty() => Some(msg.clone()),
		(_, true) => Some("error".to_string()),
		(_, false) => None,
	};
	Some(Deprecation { note, since: None })
}

/// C# form keyword recovered from the oracle `kind` token.
fn form_keyword(kind: &str) -> &'static str {
	match kind {
		"STRUCT" => "struct",
		"RECORD" => "record class",
		"RECORD_STRUCT" => "record struct",
		"INTERFACE" => "interface",
		"ENUM" => "enum",
		"DELEGATE" => "delegate",
		// CLASS and unknown → class
		_ => "class",
	}
}

/// The form + non-access modifier note for a type
/// (`Declared: sealed abstract class` / `Declared: readonly struct`).
///
/// Always includes the C# form keyword so CLASS/STRUCT/RECORD/RECORD_STRUCT
/// remain recoverable (they all lower to `Entry::RecordType`).
fn declared_note(kind: &str, modifiers: &[String]) -> Option<String> {
	const INTERESTING: &[&str] =
		&["static", "sealed", "abstract", "readonly", "ref", "partial", "unsafe", "new"];
	let mut parts: Vec<&str> = modifiers
		.iter()
		.map(String::as_str)
		.filter(|m| INTERESTING.contains(m))
		.collect();
	parts.push(form_keyword(kind));
	Some(format!("Declared: `{}`", parts.join(" ")))
}

/// Source accessibility as a `Declared:` note so collapsed visibility pairs
/// (`protectedInternal` → Protected, `privateProtected` → Package) stay
/// recoverable from documentation.
fn accessibility_declared_note(accessibility: &str) -> Option<String> {
	if accessibility.is_empty() {
		return None;
	}
	// Always stamp the original oracle token (camelCase) for fidelity.
	Some(format!("Declared: `{accessibility}`"))
}

/// Assemble a type entry's documentation: prose, declaration modifiers,
/// attributes, forwarding, hidden/deprecation markers.
fn type_documentation(
	decl: &schema::TypeDecl,
	parsed: Option<&ParsedDoc>,
	extra: &[String],
) -> Option<String> {
	let mut sections: Vec<String> = Vec::new();

	if let Some(parsed) = parsed {
		if let Some(text) = parsed.documentation() {
			sections.push(text);
		}
		if !parsed.type_params.is_empty() {
			let mut lines: Vec<String> = parsed
				.type_params
				.iter()
				.map(|(name, desc)| format!("- `{name}` — {desc}"))
				.collect();
			lines.sort();
			sections.push(format!("Type parameters:\n{}", lines.join("\n")));
		}
		if !parsed.see_also.is_empty() {
			let refs: Vec<String> = parsed.see_also.iter().map(|s| format!("`{s}`")).collect();
			sections.push(format!("See also: {}", refs.join(", ")));
		}
	}

	sections.extend(extra.iter().cloned());

	if let Some(note) = declared_note(&decl.kind, &decl.modifiers) {
		sections.push(note);
	}
	// Always stamp original accessibility so collapsed pairs stay recoverable.
	if let Some(note) = accessibility_declared_note(&type_accessibility(decl)) {
		sections.push(note);
	}
	if !decl.attributes.is_empty() {
		let rendered: Vec<String> =
			decl.attributes.iter().map(|a| format!("`{}`", types::render_attribute(a))).collect();
		sections.push(format!("Attributes: {}", rendered.join(", ")));
	}
	if decl.forwarded {
		sections.push("Forwarded type (re-exported from a dependency).".to_string());
	}
	if decl.hidden {
		sections.push("Hidden (`EditorBrowsable(Never)`).".to_string());
	}
	if let Some(section) = deprecation_section(decl.deprecated.as_ref()) {
		sections.push(section);
	}

	if sections.is_empty() { None } else { Some(sections.join("\n\n")) }
}

/// Build the resolved `doc_links` map from the oracle's `cref → docId` links.
fn make_doc_links(
	ctx: &Lowering<'_>,
	links: &Option<BTreeMap<String, String>>,
) -> Option<rustc_hash::FxHashMap<String, NudoxPath>> {
	let links = links.as_ref()?;
	if links.is_empty() {
		return None;
	}
	let mut out: rustc_hash::FxHashMap<String, NudoxPath> = Default::default();
	for (cref, doc_id) in links {
		out.insert(cref.clone(), ctx.doc_id_to_path(doc_id));
	}
	Some(out)
}

// ---------------------------------------------------------------------------
// Fields, properties, events (inline Record fields)
// ---------------------------------------------------------------------------

/// Lower a field declaration into an inline record field.
fn lower_field(f: &schema::Field) -> Field {
	let parsed = xmldoc::parse_opt(f.doc.as_deref());

	let mut decorators: Vec<String> =
		f.attributes.iter().map(types::render_attribute).collect();
	if f.is_const {
		decorators.push("const".to_string());
	}
	if f.is_readonly {
		decorators.push("readonly".to_string());
	}
	if f.is_volatile {
		decorators.push("volatile".to_string());
	}
	if f.is_required {
		decorators.push("required".to_string());
	}
	// Stamp original accessibility so collapsed pairs stay recoverable.
	if !f.accessibility.is_empty() {
		decorators.push(format!("accessibility:{}", f.accessibility));
	}

	Field::Known(KnownField {
		key:           FieldKey::Ident(f.name.clone()),
		r#type:        Some(Box::new(types::lower_type(&f.ty))),
		default_value: f.constant.as_deref().map(types::lower_constant),
		attributes:    FieldAttributes {
			decorators,
			// `const`/`readonly` fields are immutable; everything else is mutable.
			is_mutable:  !(f.is_const || f.is_readonly),
			is_optional: false,
			is_static:   f.is_static || f.is_const,
		},
		visibility:    Some(types::visibility(&f.accessibility)),
		documentation: field_doc(
			parsed.as_ref(),
			f.deprecated.as_ref(),
			f.hidden,
			Some(&f.accessibility),
		),
	})
}

/// Lower a property (or indexer) into an inline record field, encoding accessor
/// asymmetry and init/required/static as decorators (Track A).
fn lower_property(p: &schema::Property) -> Field {
	let parsed = xmldoc::parse_opt(p.doc.as_deref());

	let mut decorators: Vec<String> =
		p.attributes.iter().map(types::render_attribute).collect();
	decorators.push(accessor_decorator(p));
	if p.is_required {
		decorators.push("required".to_string());
	}
	if p.is_indexer {
		let params: Vec<String> = p
			.parameters
			.iter()
			.map(|param| {
				format!("{} {}", types::type_display(&param.ty), param.name)
			})
			.collect();
		decorators.push(format!("indexer[{}]", params.join(", ")));
	}
	if p.returns_by_ref {
		decorators.push(if p.returns_by_ref_readonly { "ref readonly".to_string() } else { "ref".to_string() });
	}
	if !p.accessibility.is_empty() {
		decorators.push(format!("accessibility:{}", p.accessibility));
	}

	// A property with no setter is immutable from the outside.
	let is_mutable = p.set_kind != "none";

	Field::Known(KnownField {
		key:           FieldKey::Ident(p.name.clone()),
		r#type:        Some(Box::new(types::lower_type(&p.ty))),
		default_value: None,
		attributes:    FieldAttributes {
			decorators,
			is_mutable,
			is_optional: false,
			is_static: p.is_static,
		},
		visibility:    Some(types::visibility(&p.accessibility)),
		documentation: property_doc(parsed.as_ref(), p),
	})
}

/// The accessor decorator for a property: `get`, `set`, `init`, and asymmetric
/// forms like `get;private set`.
fn accessor_decorator(p: &schema::Property) -> String {
	let mut parts: Vec<String> = Vec::new();
	// Getter: emit its own accessibility when it differs (asymmetric accessors).
	match &p.get_accessibility {
		Some(vis) if vis != &p.accessibility => parts.push(format!("{vis} get")),
		Some(_) => parts.push("get".to_string()),
		// No explicit getter accessibility → assume the common case (a getter).
		None => parts.push("get".to_string()),
	}
	// Setter / init.
	match p.set_kind.as_str() {
		"set" => match &p.set_accessibility {
			Some(vis) if vis != &p.accessibility => parts.push(format!("{vis} set")),
			_ => parts.push("set".to_string()),
		},
		"init" => match &p.set_accessibility {
			Some(vis) if vis != &p.accessibility => parts.push(format!("{vis} init")),
			_ => parts.push("init".to_string()),
		},
		_ => {}
	}
	parts.join(";")
}

/// Lower an event into a standalone `Entry::Event` whose `TypedBinding.ty`
/// carries the delegate type (C5). Returns the minted path for the parent
/// `members` list.
fn push_event_entry(
	ctx: &Lowering<'_>,
	decl: &schema::TypeDecl,
	e: &schema::Event,
	entries: &mut Vec<(NudoxPath, Entry)>,
) -> NudoxPath {
	let path = ctx.member_key(decl, &e.name);
	let parsed = xmldoc::parse_opt(e.doc.as_deref());
	let mut doc_sections: Vec<String> = Vec::new();
	if let Some(text) = parsed.as_ref().and_then(ParsedDoc::documentation) {
		doc_sections.push(text);
	}
	doc_sections.push("Declared: `event`".to_string());
	if e.is_static {
		doc_sections.push("Declared: `static`".to_string());
	}
	if let Some(note) = accessibility_declared_note(&e.accessibility) {
		doc_sections.push(note);
	}
	if !e.attributes.is_empty() {
		let rendered: Vec<String> =
			e.attributes.iter().map(|a| format!("`{}`", types::render_attribute(a))).collect();
		doc_sections.push(format!("Attributes: {}", rendered.join(", ")));
	}
	if e.hidden {
		doc_sections.push("Hidden (`EditorBrowsable(Never)`).".to_string());
	}
	if let Some(section) = deprecation_section(e.deprecated.as_ref()) {
		doc_sections.push(section);
	}

	entries.push((
		path.clone(),
		Entry::Event(Symbol {
			name:          e.name.clone(),
			path:          path.clone(),
			aliases:       doc_id_alias(&e.doc_id),
			visibility:    types::visibility(&e.accessibility),
			documentation: if doc_sections.is_empty() {
				None
			} else {
				Some(doc_sections.join("\n\n"))
			},
			deprecation:   deprecation_of(e.deprecated.as_ref()),
			doc_links:     make_doc_links(ctx, &e.doc_links),
			inner:         TypedBinding {
				ty:      Some(types::lower_type(&e.ty)),
				value:   None,
				mutable: Some(true),
			},
		}),
	));
	path
}

/// Documentation for a field: prose + accessibility + hidden/deprecation.
fn field_doc(
	parsed: Option<&ParsedDoc>,
	deprecated: Option<&schema::Deprecated>,
	hidden: bool,
	accessibility: Option<&str>,
) -> Option<String> {
	let mut sections: Vec<String> = Vec::new();
	if let Some(text) = parsed.and_then(ParsedDoc::documentation) {
		sections.push(text);
	}
	if let Some(acc) = accessibility {
		if let Some(note) = accessibility_declared_note(acc) {
			sections.push(note);
		}
	}
	if hidden {
		sections.push("Hidden (`EditorBrowsable(Never)`).".to_string());
	}
	if let Some(section) = deprecation_section(deprecated) {
		sections.push(section);
	}
	if sections.is_empty() { None } else { Some(sections.join("\n\n")) }
}

/// Documentation for a property: prose (+ `<value>`) + accessibility + hidden/deprecation.
fn property_doc(parsed: Option<&ParsedDoc>, p: &schema::Property) -> Option<String> {
	let mut sections: Vec<String> = Vec::new();
	if let Some(text) = parsed.and_then(ParsedDoc::documentation) {
		sections.push(text);
	}
	if let Some(value) = parsed.and_then(|d| d.value.clone()) {
		sections.push(format!("Value: {value}"));
	}
	if let Some(note) = accessibility_declared_note(&p.accessibility) {
		sections.push(note);
	}
	if p.hidden {
		sections.push("Hidden (`EditorBrowsable(Never)`).".to_string());
	}
	if let Some(section) = deprecation_section(p.deprecated.as_ref()) {
		sections.push(section);
	}
	if sections.is_empty() { None } else { Some(sections.join("\n\n")) }
}

// ---------------------------------------------------------------------------
// Methods (standalone entries)
// ---------------------------------------------------------------------------

/// Group methods by display name in first-appearance order.
fn method_groups(methods: &[schema::Method]) -> Vec<(String, Vec<&schema::Method>)> {
	let mut groups: Vec<(String, Vec<&schema::Method>)> = Vec::new();
	for m in methods {
		let key = function::method_display_name(m);
		match groups.iter_mut().find(|(name, _)| *name == key) {
			Some((_, group)) => group.push(m),
			None => groups.push((key, vec![m])),
		}
	}
	groups
}

/// Emit one standalone `Entry::Function` per method-name group (overloads
/// folded), returning the minted paths for the parent's `members`.
fn push_method_entries(
	ctx: &Lowering<'_>,
	decl: &schema::TypeDecl,
	methods: &[schema::Method],
	entries: &mut Vec<(NudoxPath, Entry)>,
) -> Vec<NudoxPath> {
	let mut paths = Vec::new();

	for (name, group) in method_groups(methods) {
		let path = ctx.member_key(decl, &name);
		paths.push(path.clone());

		let primary = group[0];
		let parsed = xmldoc::parse_opt(primary.doc.as_deref());
		let mut doc_sections: Vec<String> = Vec::new();
		if let Some(text) = parsed.as_ref().and_then(ParsedDoc::documentation) {
			doc_sections.push(text);
		}
		if let Some(parsed) = &parsed {
			if !parsed.type_params.is_empty() {
				let mut lines: Vec<String> = parsed
					.type_params
					.iter()
					.map(|(n, d)| format!("- `{n}` — {d}"))
					.collect();
				lines.sort();
				doc_sections.push(format!("Type parameters:\n{}", lines.join("\n")));
			}
		}
		doc_sections.extend(function::declaration_notes(primary));
		if let Some(note) = accessibility_declared_note(&primary.accessibility) {
			doc_sections.push(note);
		}
		if primary.hidden {
			doc_sections.push("Hidden (`EditorBrowsable(Never)`).".to_string());
		}
		if let Some(section) = deprecation_section(primary.deprecated.as_ref()) {
			doc_sections.push(section);
		}

		let lowered: Vec<Function> =
			group.iter().map(|m| function::lower_method(m)).collect();

		entries.push((
			path.clone(),
			Entry::Function(Symbol {
				name:          name.clone(),
				path:          path.clone(),
				aliases:       doc_id_alias(&primary.doc_id),
				visibility:    types::visibility(&primary.accessibility),
				documentation: if doc_sections.is_empty() {
					None
				} else {
					Some(doc_sections.join("\n\n"))
				},
				deprecation:   deprecation_of(primary.deprecated.as_ref()),
				doc_links:     make_doc_links(ctx, &primary.doc_links),
				inner:         function::fold_overloads(lowered),
			}),
		));
	}

	paths
}

/// The nested member types that survive lowering, as paths for `members`.
///
/// Oracle `members.nested` is a list of **doc-ids** (`T:Ns.Outer+Inner`), so
/// resolution tries `types_by_doc_id` first and falls back to qualified name.
fn nested_paths(ctx: &Lowering<'_>, decl: &schema::TypeDecl) -> Vec<NudoxPath> {
	decl.members
		.nested
		.iter()
		.filter_map(|key| ctx.resolve_type_ref(key))
		.map(|nested| ctx.type_key(nested))
		.collect()
}

// ---------------------------------------------------------------------------
// Classes, structs, records
// ---------------------------------------------------------------------------

/// Base types that are structural noise rather than information.
const IMPLICIT_BASES: &[&str] = &[
	"System.Object",
	"System.ValueType",
	"System.Enum",
	"System.Delegate",
	"System.MulticastDelegate",
];

fn lower_class_like(ctx: &Lowering<'_>, decl: &schema::TypeDecl) -> Vec<(NudoxPath, Entry)> {
	let mut entries = Vec::new();
	let path = ctx.type_key(decl);
	let parsed = xmldoc::parse_opt(decl.doc.as_deref());

	// --- Fields, properties, indexers → inline fields. ---------------------
	let mut fields: Vec<Field> = Vec::new();
	for f in &decl.members.fields {
		fields.push(lower_field(f));
	}
	for p in &decl.members.properties {
		fields.push(lower_property(p));
	}
	for idx in &decl.members.indexers {
		fields.push(lower_property(idx));
	}

	// --- Constructors. -----------------------------------------------------
	let constructors: Vec<Function> = decl
		.members
		.constructors
		.iter()
		.map(function::lower_constructor)
		.collect();

	// --- Methods + operators + conversions → standalone members. -----------
	let mut all_methods: Vec<schema::Method> = Vec::new();
	all_methods.extend(decl.members.methods.iter().cloned());
	all_methods.extend(decl.members.operators.iter().cloned());
	all_methods.extend(decl.members.conversions.iter().cloned());
	let mut members = push_method_entries(ctx, decl, &all_methods, &mut entries);

	// --- Events → standalone Entry::Event members (delegate type on ty). ---
	for e in &decl.members.events {
		members.push(push_event_entry(ctx, decl, e, &mut entries));
	}

	// Nested types (oracle emits doc-ids).
	members.extend(nested_paths(ctx, decl));

	// --- Supertypes. -------------------------------------------------------
	let mut super_types: Vec<IrType> = Vec::new();
	if let Some(base) = &decl.base_type {
		let implicit = base.named_name().is_some_and(|n| IMPLICIT_BASES.contains(&n));
		if !implicit {
			super_types.push(types::lower_type(base));
		}
	}
	for interface in &decl.interfaces {
		super_types.push(types::lower_type(interface));
	}

	let implemented_protocols: Vec<NudoxPath> = decl
		.interfaces
		.iter()
		.filter_map(|i| i.named_name())
		.filter_map(|name| ctx.decl(name))
		.filter(|target| target.kind == "INTERFACE")
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

	// Extension-block receiver, when this is a C# 14 extension container.
	let mut extra: Vec<String> = Vec::new();
	if let Some(receiver) = &decl.extension_receiver {
		extra.push(format!("Extension block for `{}`.", types::type_display(receiver)));
	}

	entries.push((
		path.clone(),
		Entry::RecordType(Symbol {
			name: decl.simple_name.clone(),
			path,
			aliases: doc_id_alias(&decl.doc_id),
			visibility: types::visibility(&type_accessibility(decl)),
			documentation: type_documentation(decl, parsed.as_ref(), &extra),
			deprecation: deprecation_of(decl.deprecated.as_ref()),
			doc_links: make_doc_links(ctx, &decl.doc_links),
			inner: record,
		}),
	));

	entries
}

/// A type declaration's accessibility: the first access modifier in the list,
/// defaulting to `internal` (the C# top-level / nested-in-namespace default).
fn type_accessibility(decl: &schema::TypeDecl) -> String {
	for m in &decl.modifiers {
		if matches!(
			m.as_str(),
			"public" | "protected" | "internal" | "protectedInternal" | "privateProtected"
				| "private"
		) {
			return m.clone();
		}
	}
	"internal".to_string()
}

// ---------------------------------------------------------------------------
// Interfaces
// ---------------------------------------------------------------------------

fn lower_interface(ctx: &Lowering<'_>, decl: &schema::TypeDecl) -> Vec<(NudoxPath, Entry)> {
	let mut entries = Vec::new();
	let path = ctx.type_key(decl);
	let parsed = xmldoc::parse_opt(decl.doc.as_deref());

	// Abstract members → required; body-bearing (default interface members,
	// non-abstract static) → provided.
	let mut required = Vec::new();
	let mut provided = Vec::new();
	let mut all_methods: Vec<&schema::Method> = Vec::new();
	all_methods.extend(decl.members.methods.iter());
	all_methods.extend(decl.members.operators.iter());
	for m in all_methods {
		let has_body = !m.is_abstract;
		let lowered = function::trait_method(m, has_body);
		if has_body {
			provided.push(lowered);
		} else {
			required.push(lowered);
		}
	}

	// Properties + indexers → property requirements.
	let mut properties: Vec<Field> = Vec::new();
	for p in &decl.members.properties {
		properties.push(lower_property(p));
	}
	for idx in &decl.members.indexers {
		properties.push(lower_property(idx));
	}

	let super_traits: Vec<TraitRef> = decl.interfaces.iter().map(types::trait_ref).collect();

	let mut attributes: Vec<TraitAttribute> = Vec::new();
	for a in &decl.attributes {
		let rendered = types::render_attribute(a);
		attributes.push(TraitAttribute::Custom { name: rendered, args: None });
	}

	// Events as standalone members; nested types via doc-id resolution.
	let mut members = Vec::new();
	for e in &decl.members.events {
		members.push(push_event_entry(ctx, decl, e, &mut entries));
	}
	members.extend(nested_paths(ctx, decl));

	let trait_def = TraitDef {
		generics: types::lower_type_params(&decl.type_params),
		super_traits: if super_traits.is_empty() { None } else { Some(super_traits) },
		associated_types: None,
		properties: if properties.is_empty() { None } else { Some(properties) },
		required_methods: if required.is_empty() { None } else { Some(required) },
		provided_methods: if provided.is_empty() { None } else { Some(provided) },
		required_constants: None,
		attributes: if attributes.is_empty() { None } else { Some(attributes) },
		object_safe: None,
		sealed: None,
		cfg: None,
		members: if members.is_empty() { None } else { Some(members) },
	};

	entries.push((
		path.clone(),
		Entry::TraitDef(Symbol {
			name: decl.simple_name.clone(),
			path,
			aliases: doc_id_alias(&decl.doc_id),
			visibility: types::visibility(&type_accessibility(decl)),
			documentation: type_documentation(decl, parsed.as_ref(), &[]),
			deprecation: deprecation_of(decl.deprecated.as_ref()),
			doc_links: make_doc_links(ctx, &decl.doc_links),
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
	let parsed = xmldoc::parse_opt(decl.doc.as_deref());

	let variants: Vec<SumVariant> = decl
		.members
		.fields
		.iter()
		.filter(|f| f.is_const || f.constant.is_some())
		.map(lower_enum_member)
		.collect();

	// Dual-emit methods and hang them on the SumType container.
	let mut all_methods: Vec<schema::Method> = Vec::new();
	all_methods.extend(decl.members.methods.iter().cloned());
	let method_paths = push_method_entries(ctx, decl, &all_methods, &mut entries);

	// Underlying type + [Flags] still noted in docs; also structural on SumType.
	let mut extra: Vec<String> = Vec::new();
	let underlying = decl.enum_underlying.as_ref().map(types::lower_type);
	if let Some(underlying_sig) = &decl.enum_underlying {
		let name = types::type_display(underlying_sig);
		if name != "System.Int32" && name != "Int32" {
			extra.push(format!("Underlying type: `{name}`."));
		}
	}
	if decl.attributes.iter().any(|a| a.ty.ends_with("FlagsAttribute")) {
		extra.push("`[Flags]` — a bit-field enumeration.".to_string());
	}

	let mut sum = ir::record::SumType::from_variants(variants);
	sum.underlying = underlying;
	sum.members = if method_paths.is_empty() {
		None
	} else {
		Some(method_paths)
	};

	entries.push((
		path.clone(),
		Entry::SumType(Symbol {
			name: decl.simple_name.clone(),
			path,
			aliases: doc_id_alias(&decl.doc_id),
			visibility: types::visibility(&type_accessibility(decl)),
			documentation: type_documentation(decl, parsed.as_ref(), &extra),
			deprecation: deprecation_of(decl.deprecated.as_ref()),
			doc_links: make_doc_links(ctx, &decl.doc_links),
			inner: sum,
		}),
	));

	entries
}

/// One enum member → `SumVariant`; its constant value rides the variant doc.
fn lower_enum_member(f: &schema::Field) -> SumVariant {
	let parsed = xmldoc::parse_opt(f.doc.as_deref());
	let mut sections: Vec<String> = Vec::new();
	if let Some(text) = parsed.as_ref().and_then(ParsedDoc::documentation) {
		sections.push(text);
	}
	if let Some(value) = &f.constant {
		sections.push(format!("Value: `{value}`"));
	}
	if let Some(section) = deprecation_section(f.deprecated.as_ref()) {
		sections.push(section);
	}

	let discriminant = f.constant.as_ref().and_then(|v| {
		// Prefer structured int/bool/string; else keep the spelling as Var.
		if let Ok(n) = v.replace('_', "").parse::<i64>() {
			return Some(ir::generics::ConstExpr::Int(n));
		}
		if v == "true" {
			return Some(ir::generics::ConstExpr::Bool(true));
		}
		if v == "false" {
			return Some(ir::generics::ConstExpr::Bool(false));
		}
		if (v.starts_with('"') && v.ends_with('"')) || (v.starts_with('\'') && v.ends_with('\'')) {
			return Some(ir::generics::ConstExpr::Str(v[1..v.len() - 1].to_string()));
		}
		Some(ir::generics::ConstExpr::Var(v.clone()))
	});

	SumVariant {
		name:          f.name.clone(),
		data:          None,
		documentation: if sections.is_empty() { None } else { Some(sections.join("\n\n")) },
		discriminant,
	}
}

// ---------------------------------------------------------------------------
// Delegates
// ---------------------------------------------------------------------------

fn lower_delegate(ctx: &Lowering<'_>, decl: &schema::TypeDecl) -> Vec<(NudoxPath, Entry)> {
	let path = ctx.type_key(decl);
	let parsed = xmldoc::parse_opt(decl.doc.as_deref());

	let (inputs, outputs) = match &decl.delegate_sig {
		Some(sig) => {
			let inputs = function::lower_params(&sig.params, parsed.as_ref());
			let outputs = sig.return_type.as_ref().and_then(|ret| {
				if matches!(ret, schema::TypeSig::Named { name, .. } if name == "System.Void") {
					None
				} else {
					Some(vec![IrParameter::Literal(ir::parameter::LiteralParameter {
						name:          String::new(),
						r#type:        Some(types::lower_type(ret)),
						attributes:    None,
						default_value: None,
						description:   parsed.as_ref().and_then(|d| d.returns.clone()),
					})])
				}
			});
			(if inputs.is_empty() { None } else { Some(inputs) }, outputs)
		}
		None => (None, None),
	};

	let fnptr = IrType::FunctionPointer(IrFunctionPointer {
		inputs,
		outputs,
		attributes: None,
	});

	vec![(
		path.clone(),
		Entry::TypeAlias(Symbol {
			name: decl.simple_name.clone(),
			path,
			aliases: doc_id_alias(&decl.doc_id),
			visibility: types::visibility(&type_accessibility(decl)),
			documentation: type_documentation(decl, parsed.as_ref(), &["A `delegate` type.".to_string()]),
			deprecation: deprecation_of(decl.deprecated.as_ref()),
			doc_links: make_doc_links(ctx, &decl.doc_links),
			inner: ir::kind::TypeAliasBody::plain(fnptr),
		}),
	)]
}
