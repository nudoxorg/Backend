//! Lowering oracle declarations into `ir::kind::Entry` values.
//!
//! Shape decisions (Go form → IR form):
//!
//! * **struct type** → `Entry::RecordType`. Embedded fields stay marked
//!   fields (an `"embedded"` decorator + the implicit name) rather than
//!   being flattened — see `types::lower_struct_fields`. Methods become
//!   standalone `Entry::Function` symbols listed in `Record::members`
//!   (the inline `Record::methods` slot holds nameless `Function`s and
//!   would drop the method names); promoted methods are included with
//!   their embedding origin recorded.
//! * **interface type** → `Entry::TraitDef`. Embedded named interfaces
//!   become `super_traits`; `required_methods` carries the FULL expanded
//!   method set, inherited methods annotated with their declaring
//!   package (provenance). Constraint-only content (type-set unions,
//!   comparability) is preserved as `TraitAttribute::Custom` entries.
//! * **iota enum convention** — a defined non-struct, non-interface type
//!   with at least one same-package `const` block of that type using
//!   `iota` → `Entry::SumType`; the participating constants become
//!   `SumVariant`s (value recorded in the variant documentation, the IR
//!   variant having no value slot) instead of `Entry::Constant`s.
//! * **other defined types** (`type Celsius float64`, `type Handler
//!   func(...)`) → a single-field *newtype* `Entry::RecordType` (field
//!   key `Index(0)`, Rust-tuple-struct style), which preserves
//!   defined-type identity — a `TypeAlias` entry would erase the
//!   alias/defined distinction Go draws.
//! * **true alias** (`type A = B`) → `Entry::TypeAlias`.
//! * **const/var** → `Entry::Constant` / `Entry::Variable`. The IR
//!   payload for both is `Symbol<()>`; the exact constant value (which
//!   has no structural slot) is appended to the documentation.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use ir::entry::NudoxPath;
use ir::generics::TraitRef;
use ir::kind::{Entry, Symbol, Visibility};
use ir::module::Module;
use ir::protocols::{TraitAttribute, TraitDef};
use ir::record::{Field, FieldAttributes, FieldKey, KnownField, Record, SumVariant};
use ir::ty::Type as IrType;

use super::docstring;
use super::function;
use super::oracle::{self, DeclKind, TypeKind};
use super::types;

/// Lower one oracle package into `(path, entry)` pairs: the package's
/// `Entry::Module` plus every declaration. Module `members` and index
/// `root_ids` are wired afterwards by `context`.
pub fn lower_package(pkg: &oracle::Package) -> Vec<(NudoxPath, Entry)> {
	let mut entries: Vec<(NudoxPath, Entry)> = Vec::new();

	let module_key = package_key(&pkg.import_path);
	entries.push((
		module_key.clone(),
		Entry::Module(Symbol {
			name:          pkg.import_path.clone(),
			path:          module_key,
			aliases:       None,
			visibility:    Visibility::Public,
			documentation: doc_of(&pkg.doc),
			inner:         Module { members: None },
		}),
	));

	let enums = detect_enums(pkg);

	for decl in &pkg.decls {
		match decl.kind {
			DeclKind::Type => lower_type_decl(pkg, decl, &enums, &mut entries),
			DeclKind::Alias => entries.push(lower_alias(pkg, decl)),
			DeclKind::Func => entries.push(lower_func(pkg, decl)),
			DeclKind::Const => {
				// Enum variants were consumed by their SumType.
				if !enums.variant_names.contains(&decl.name) {
					entries.push(lower_const(pkg, decl));
				}
			}
			DeclKind::Var => entries.push(lower_var(pkg, decl)),
		}
	}

	entries
}

// ---------------------------------------------------------------------------
// Path scheme
// ---------------------------------------------------------------------------

/// A package's `Entry::Module` is keyed by its import path.
pub fn package_key(import_path: &str) -> NudoxPath {
	NudoxPath::Local(PathBuf::from(import_path))
}

/// A package-level item is keyed `import/path::Name`.
pub fn item_key(import_path: &str, name: &str) -> NudoxPath {
	NudoxPath::Local(PathBuf::from(format!("{import_path}::{name}")))
}

/// A method is keyed `import/path::Type.Method`.
pub fn method_key(import_path: &str, type_name: &str, method: &str) -> NudoxPath {
	NudoxPath::Local(PathBuf::from(format!("{import_path}::{type_name}.{method}")))
}

// ---------------------------------------------------------------------------
// Docs
// ---------------------------------------------------------------------------

/// Parse a raw oracle doc comment into `Symbol.documentation` text.
fn doc_of(raw: &str) -> Option<String> {
	if raw.trim().is_empty() {
		return None;
	}
	docstring::parse(raw).documentation()
}

// ---------------------------------------------------------------------------
// The iota enum convention
// ---------------------------------------------------------------------------

/// The detected enum conventions of a package.
struct EnumInfo<'a> {
	/// Defined type name → its variant constants, in source order.
	variants_by_type: HashMap<String, Vec<&'a oracle::Decl>>,
	/// Every constant name consumed as a variant.
	variant_names: HashSet<String>,
}

/// Detect Go's enum convention: a defined type `T` (non-struct,
/// non-interface underlying) together with same-package `const` block(s)
/// of type `T` that use `iota`. Constants of type `T` in non-iota groups
/// (e.g. a lone `const Freezing Celsius = 0`) do NOT make `T` an enum.
fn detect_enums(pkg: &oracle::Package) -> EnumInfo<'_> {
	let defined: HashMap<&str, &oracle::Decl> = pkg
		.decls
		.iter()
		.filter(|d| d.kind == DeclKind::Type)
		.map(|d| (d.name.as_str(), d))
		.collect();

	let mut variants_by_type: HashMap<String, Vec<&oracle::Decl>> = HashMap::new();

	for decl in &pkg.decls {
		if decl.kind != DeclKind::Const || !decl.group_has_iota {
			continue;
		}
		let Some(const_type) = &decl.r#type else {
			continue;
		};
		// The constant's type must be a defined type of THIS package.
		if !matches!(const_type.kind, TypeKind::Named | TypeKind::Alias)
			|| const_type.pkg != pkg.import_path
		{
			continue;
		}
		let Some(type_decl) = defined.get(const_type.name.as_str()) else {
			continue;
		};
		let sum_like = type_decl
			.underlying
			.as_ref()
			.map(|u| !matches!(u.kind, TypeKind::Struct | TypeKind::Interface))
			.unwrap_or(false);
		if sum_like {
			variants_by_type.entry(const_type.name.clone()).or_default().push(decl);
		}
	}

	// Variants in source order (decls arrive name-sorted from the oracle).
	for variants in variants_by_type.values_mut() {
		variants.sort_by_key(|d| {
			d.pos.as_ref().map(|p| (p.file.clone(), p.line)).unwrap_or_default()
		});
	}

	let variant_names =
		variants_by_type.values().flatten().map(|d| d.name.clone()).collect();

	EnumInfo { variants_by_type, variant_names }
}

// ---------------------------------------------------------------------------
// Declarations
// ---------------------------------------------------------------------------

/// Lower a defined type: iota enum → SumType, struct → Record,
/// interface → TraitDef, anything else → newtype Record.
fn lower_type_decl(
	pkg: &oracle::Package,
	decl: &oracle::Decl,
	enums: &EnumInfo<'_>,
	entries: &mut Vec<(NudoxPath, Entry)>,
) {
	let path = item_key(&pkg.import_path, &decl.name);

	if let Some(variants) = enums.variants_by_type.get(&decl.name) {
		entries.push((
			path.clone(),
			Entry::SumType(Symbol {
				name:          decl.name.clone(),
				path,
				aliases:       None,
				visibility:    types::visibility(decl.exported),
				documentation: doc_of(&decl.doc),
				inner:         variants.iter().map(|c| sum_variant(c)).collect(),
			}),
		));
		// SumType has no member/method slots; the type's methods become
		// standalone entries alongside it.
		push_method_entries(pkg, decl, entries);
		return;
	}

	match decl.underlying.as_ref().map(|u| u.kind) {
		Some(TypeKind::Struct) => {
			let method_paths = push_method_entries(pkg, decl, entries);
			entries.push(struct_entry(decl, path, method_paths));
		}
		Some(TypeKind::Interface) => {
			entries.push(interface_entry(decl, path));
		}
		_ => {
			let method_paths = push_method_entries(pkg, decl, entries);
			entries.push(newtype_entry(decl, path, method_paths));
		}
	}
}

/// A variant constant → `SumVariant`. The IR variant has no value slot,
/// so the exact constant value is recorded in the documentation.
fn sum_variant(decl: &oracle::Decl) -> SumVariant {
	SumVariant {
		name:          decl.name.clone(),
		data:          None,
		documentation: with_value_note(doc_of(&decl.doc), &decl.value),
	}
}

/// Append a `Value:` note to a documentation string.
fn with_value_note(doc: Option<String>, value: &str) -> Option<String> {
	if value.is_empty() {
		return doc;
	}
	let note = format!("Value: `{value}`");
	Some(match doc {
		Some(text) => format!("{text}\n\n{note}"),
		None => note,
	})
}

/// Emit standalone `Entry::Function` symbols for a type's declared and
/// promoted methods, returning their paths (for `Record::members`).
fn push_method_entries(
	pkg: &oracle::Package,
	decl: &oracle::Decl,
	entries: &mut Vec<(NudoxPath, Entry)>,
) -> Vec<NudoxPath> {
	let mut paths = Vec::new();

	for method in &decl.methods {
		let path = method_key(&pkg.import_path, &decl.name, &method.name);
		paths.push(path.clone());
		entries.push((
			path.clone(),
			Entry::Function(Symbol {
				name:          method.name.clone(),
				path,
				aliases:       None,
				visibility:    types::visibility(method.exported),
				documentation: doc_of(&method.doc),
				inner:         function::lower_method(method),
			}),
		));
	}

	for method in &decl.promoted_methods {
		let path = method_key(&pkg.import_path, &decl.name, &method.name);
		paths.push(path.clone());
		entries.push((
			path.clone(),
			Entry::Function(Symbol {
				name:          method.name.clone(),
				path,
				aliases:       None,
				visibility:    types::visibility(method.exported),
				documentation: promoted_doc(method),
				inner:         function::lower_method(method),
			}),
		));
	}

	paths
}

/// Provenance-preserving documentation for a promoted method.
fn promoted_doc(method: &oracle::Method) -> Option<String> {
	let provenance = if method.origin.is_empty() {
		"Promoted from an embedded field.".to_string()
	} else {
		format!("Promoted from embedded `{}`.", method.origin)
	};
	Some(match doc_of(&method.doc) {
		Some(text) => format!("{text}\n\n{provenance}"),
		None => provenance,
	})
}

/// `type T struct { ... }` → `Entry::RecordType`.
fn struct_entry(
	decl: &oracle::Decl,
	path: NudoxPath,
	method_paths: Vec<NudoxPath>,
) -> (NudoxPath, Entry) {
	let fields = decl
		.underlying
		.as_ref()
		.map(|u| types::lower_struct_fields(&u.fields, Some(&decl.field_docs)))
		.unwrap_or_default();

	let record = Record {
		name: Some(decl.name.clone()),
		generics: types::lower_generics(&decl.type_params),
		fields,
		call_signatures: None,
		constructors: None,
		methods: None,
		index_signatures: None,
		super_types: None,
		members: if method_paths.is_empty() { None } else { Some(method_paths) },
		implemented_protocols: None,
	};

	(
		path.clone(),
		Entry::RecordType(Symbol {
			name:          decl.name.clone(),
			path,
			aliases:       None,
			visibility:    types::visibility(decl.exported),
			documentation: doc_of(&decl.doc),
			inner:         record,
		}),
	)
}

/// `type T interface { ... }` → `Entry::TraitDef`.
fn interface_entry(decl: &oracle::Decl, path: NudoxPath) -> (NudoxPath, Entry) {
	let underlying = decl.underlying.as_ref();

	let mut super_traits: Vec<TraitRef> = Vec::new();
	let mut attributes: Vec<TraitAttribute> = Vec::new();
	let mut explicit_names: HashSet<&str> = HashSet::new();
	let mut required_methods = Vec::new();

	if let Some(iface) = underlying {
		for method in &iface.explicit_methods {
			explicit_names.insert(method.name.as_str());
		}

		for embedded in &iface.embeddeds {
			match embedded.kind {
				// Embedded named interfaces are supertraits.
				TypeKind::Named | TypeKind::Alias => {
					let expr = types::type_expr(embedded);
					super_traits.push(TraitRef { name: expr.name, args: expr.args });
				}
				// A constraint type set: the IR trait vocabulary has no
				// type-set slot, so it is preserved as a custom
				// attribute over the term names (`~` marking
				// approximation) — the terms themselves also remain
				// fully structural in `types::lower_type` positions.
				TypeKind::Union => {
					let terms: Vec<String> = embedded
						.terms
						.iter()
						.map(|term| {
							let name = term
								.r#type
								.as_ref()
								.map(|t| types::type_expr(t).name)
								.unwrap_or_else(|| "<invalid>".to_string());
							if term.tilde { format!("~{name}") } else { name }
						})
						.collect();
					attributes.push(TraitAttribute::Custom {
						name: "type_set".to_string(),
						args: Some(terms),
					});
				}
				// Inline anonymous interfaces (rare): fold their
				// supertrait-ness into a custom marker; their methods
				// already surface through `all_methods`.
				_ => {
					attributes.push(TraitAttribute::Custom {
						name: "embedded_anonymous".to_string(),
						args: None,
					});
				}
			}
		}

		if iface.is_comparable {
			attributes.push(TraitAttribute::Custom { name: "comparable".to_string(), args: None });
		}

		// The FULL expanded method set: explicit methods carry their
		// harvested docs; inherited methods carry provenance.
		for sig in &iface.all_methods {
			let documentation = if explicit_names.contains(sig.name.as_str()) {
				decl.method_docs.get(&sig.name).map(|raw| doc_of(raw)).flatten()
			} else {
				Some(inherited_note(sig))
			};
			required_methods.push(function::trait_method(sig, documentation));
		}
	}

	let trait_def = TraitDef {
		generics:           types::lower_generics(&decl.type_params),
		super_traits:       if super_traits.is_empty() { None } else { Some(super_traits) },
		associated_types:   None,
		properties:         None,
		required_methods:   if required_methods.is_empty() { None } else { Some(required_methods) },
		// Go interfaces cannot provide default bodies.
		provided_methods:   None,
		required_constants: None,
		attributes:         if attributes.is_empty() { None } else { Some(attributes) },
		members:            None,
	};

	(
		path.clone(),
		Entry::TraitDef(Symbol {
			name:          decl.name.clone(),
			path,
			aliases:       None,
			visibility:    types::visibility(decl.exported),
			documentation: doc_of(&decl.doc),
			inner:         trait_def,
		}),
	)
}

/// Provenance note for a method obtained via interface embedding.
fn inherited_note(sig: &oracle::MethodSig) -> String {
	if sig.pkg.is_empty() {
		"Inherited via an embedded interface.".to_string()
	} else {
		format!("Inherited via an embedded interface (declared in `{}`).", sig.pkg)
	}
}

/// A defined type over a non-struct, non-interface underlying —
/// `type Celsius float64`, `type Handler func(...)` — lowers as a
/// single-field newtype record (field key `Index(0)`), preserving the
/// defined-type identity that separates it from a mere alias.
fn newtype_entry(
	decl: &oracle::Decl,
	path: NudoxPath,
	method_paths: Vec<NudoxPath>,
) -> (NudoxPath, Entry) {
	let underlying_type = decl
		.underlying
		.as_ref()
		.map(types::lower_type)
		.unwrap_or(IrType::Infer);

	let record = Record {
		name: Some(decl.name.clone()),
		generics: types::lower_generics(&decl.type_params),
		fields: vec![Field::Known(KnownField {
			key:           FieldKey::Index(0),
			r#type:        Some(Box::new(underlying_type)),
			default_value: None,
			attributes:    FieldAttributes {
				decorators:  vec!["underlying".to_string()],
				is_mutable:  true,
				is_optional: false,
				is_static:   false,
			},
			visibility:    Some(types::visibility(decl.exported)),
			documentation: None,
		})],
		call_signatures: None,
		constructors: None,
		methods: None,
		index_signatures: None,
		super_types: None,
		members: if method_paths.is_empty() { None } else { Some(method_paths) },
		implemented_protocols: None,
	};

	(
		path.clone(),
		Entry::RecordType(Symbol {
			name:          decl.name.clone(),
			path,
			aliases:       None,
			visibility:    types::visibility(decl.exported),
			documentation: doc_of(&decl.doc),
			inner:         record,
		}),
	)
}

/// `type A = B` → `Entry::TypeAlias`.
fn lower_alias(pkg: &oracle::Package, decl: &oracle::Decl) -> (NudoxPath, Entry) {
	let path = item_key(&pkg.import_path, &decl.name);
	let target = decl.target.as_ref().map(types::lower_type).unwrap_or(IrType::Infer);
	(
		path.clone(),
		Entry::TypeAlias(Symbol {
			name:          decl.name.clone(),
			path,
			aliases:       None,
			visibility:    types::visibility(decl.exported),
			documentation: doc_of(&decl.doc),
			inner:         target,
		}),
	)
}

/// A package-level `func` → `Entry::Function`.
fn lower_func(pkg: &oracle::Package, decl: &oracle::Decl) -> (NudoxPath, Entry) {
	let path = item_key(&pkg.import_path, &decl.name);
	(
		path.clone(),
		Entry::Function(Symbol {
			name:          decl.name.clone(),
			path,
			aliases:       None,
			visibility:    types::visibility(decl.exported),
			documentation: doc_of(&decl.doc),
			inner:         function::lower_func_decl(decl),
		}),
	)
}

/// A non-variant constant → `Entry::Constant`. `Symbol<()>` has no
/// type/value payload, so the exact value is preserved in the
/// documentation (a documented IR limitation).
fn lower_const(pkg: &oracle::Package, decl: &oracle::Decl) -> (NudoxPath, Entry) {
	let path = item_key(&pkg.import_path, &decl.name);
	(
		path.clone(),
		Entry::Constant(Symbol {
			name:          decl.name.clone(),
			path,
			aliases:       None,
			visibility:    types::visibility(decl.exported),
			documentation: with_value_note(doc_of(&decl.doc), &decl.value),
			inner:         (),
		}),
	)
}

/// A package-level `var` → `Entry::Variable`.
fn lower_var(pkg: &oracle::Package, decl: &oracle::Decl) -> (NudoxPath, Entry) {
	let path = item_key(&pkg.import_path, &decl.name);
	(
		path.clone(),
		Entry::Variable(Symbol {
			name:          decl.name.clone(),
			path,
			aliases:       None,
			visibility:    types::visibility(decl.exported),
			documentation: doc_of(&decl.doc),
			inner:         (),
		}),
	)
}
