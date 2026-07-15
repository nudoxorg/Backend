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
//!   their embedding origin recorded. Interfaces satisfied in-package
//!   land on `Record.implemented_protocols` and as `Entry::TraitImpl`.
//! * **interface type** → `Entry::TraitDef`. Embedded named interfaces
//!   become `super_traits`; `required_methods` carries the FULL expanded
//!   method set, inherited methods annotated with their declaring
//!   package (provenance). Constraint type sets lower structurally as a
//!   `properties` field of type `Union`/`TypeOperator("~")` (not only a
//!   stringified custom attribute).
//! * **iota enum convention** — a defined non-struct, non-interface type
//!   with at least one same-package `const` block of that type using
//!   `iota` → `Entry::SumType`; the underlying type is recorded in the
//!   sum's documentation; variant values prefer structured notes with
//!   parsed `ConstExpr` spellings (the IR `SumVariant` has no value slot).
//! * **other defined types** (`type Celsius float64`, `type Handler
//!   func(...)`) → a single-field *newtype* `Entry::RecordType` (field
//!   key `Index(0)`, Rust-tuple-struct style), which preserves
//!   defined-type identity — a `TypeAlias` entry would erase the
//!   alias/defined distinction Go draws.
//! * **true alias** (`type A = B`) → `Entry::TypeAlias` with
//!   [`TypeAliasBody`] (generics preserved when the oracle supplies them).
//! * **const/var** → `Entry::Constant` / `Entry::Variable` with
//!   [`TypedBinding`] (`ty` + parsed `value` + mutability).
//!
//! ## Path scheme
//!
//! | Kind   | Canonical path                         | Alias spellings (symtab) |
//! |--------|----------------------------------------|---------------------------|
//! | Module | `import/path`                          | — |
//! | Item   | `import/path::Name`                    | — |
//! | Method | `import/path::Type.Method`             | `import/path::Type::Method`, `Type.Method`, `Type::Method` |
//!
//! Treesitter (`treesitter/go.rs`) emits method frames with parent
//! `Type` + child bare method name; the resolver rebuilds
//! `Type.Method` / `Type::Method` via segment join. Symtab
//! [`path_segments`](crate::graph::symtab::path_segments) splits `::`
//! and dots inside components (but **not** `/` in Go import paths —
//! those stay filesystem path components). Method entries therefore
//! register both dotted and double-colon alias segmentations so
//! `resolve_exact` succeeds under either spelling.
//!
//! Go import paths use `/` as the segment separator at the `PathBuf`
//! layer (`example.com/m` → components `example.com`, `m`); do not
//! invent slash-splitting on top of that.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use ir::entry::NudoxPath;
use ir::generics::{ConstExpr, TraitRef};
use ir::kind::{Deprecation, Entry, Symbol, TypeAliasBody, TypedBinding, Visibility};
use ir::module::Module;
use ir::protocols::{TraitAttribute, TraitDef, TraitImpl};
use ir::record::{Field, FieldAttributes, FieldKey, KnownField, Record, SumField, SumVariant};
use ir::ty::{Type as IrType, TypeReference};
use rustc_hash::FxHashMap;
use rustc_hash::FxHashSet;

use super::docstring::{self, GoDoc};
use super::function;
use super::oracle::{self, DeclKind, TypeKind};
use super::types;

/// Lower one oracle package into `(path, entry)` pairs: the package's
/// `Entry::Module` plus every declaration. Module `members` and index
/// `root_ids` are wired afterwards by `context`.
pub fn lower_package(pkg: &oracle::Package) -> Vec<(NudoxPath, Entry)> {
	let mut entries: Vec<(NudoxPath, Entry)> = Vec::new();

	let module_key = package_key(&pkg.import_path);
	let pkg_doc = parse_docs(&pkg.doc);
	entries.push((
		module_key.clone(),
		Entry::Module(Symbol {
			name:          pkg.import_path.clone(),
			path:          module_key,
			aliases:       None,
			visibility:    Visibility::Public,
			documentation: pkg_doc.documentation(),
			deprecation:   deprecation_of(&pkg_doc),
			doc_links:     doc_links_of(&pkg_doc),
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

/// A method is keyed `import/path::Type.Method` (dotted receiver form).
///
/// See module docs for the alias spellings registered alongside this
/// canonical key so symtab / treesitter can `resolve_exact` either form.
pub fn method_key(import_path: &str, type_name: &str, method: &str) -> NudoxPath {
	NudoxPath::Local(PathBuf::from(format!("{import_path}::{type_name}.{method}")))
}

/// Symtab alias segmentations for a method so both dotted and
/// double-colon spellings resolve.
///
/// Produces:
/// * `[import/path, Type, Method]` → `import/path::Type::Method`
/// * `[Type, Method]` → `Type::Method` (module-relative / treesitter)
/// * `[Type.Method]` → `Type.Method`
/// * `[import/path, Type.Method]` → `import/path::Type.Method`
fn method_aliases(import_path: &str, type_name: &str, method: &str) -> FxHashSet<Vec<String>> {
	let mut set = FxHashSet::default();
	set.insert(vec![
		import_path.to_string(),
		type_name.to_string(),
		method.to_string(),
	]);
	set.insert(vec![type_name.to_string(), method.to_string()]);
	set.insert(vec![format!("{type_name}.{method}")]);
	set.insert(vec![import_path.to_string(), format!("{type_name}.{method}")]);
	set
}

// ---------------------------------------------------------------------------
// Docs
// ---------------------------------------------------------------------------

fn parse_docs(raw: &str) -> GoDoc {
	if raw.trim().is_empty() {
		GoDoc::default()
	} else {
		docstring::parse(raw)
	}
}

/// Parse a raw oracle doc comment into `Symbol.documentation` text.
fn doc_of(raw: &str) -> Option<String> {
	if raw.trim().is_empty() {
		return None;
	}
	docstring::parse(raw).documentation()
}

fn deprecation_of(doc: &GoDoc) -> Option<Deprecation> {
	doc.deprecated.as_ref().map(|note| Deprecation {
		since: None,
		note:  Some(note.clone()),
	})
}

/// Map GoDoc link definitions (`[Name]: URL`) onto `Symbol.doc_links`.
///
/// External URLs become [`NudoxPath::External`] with the URL as the
/// dependency key (no path segments); bare identifiers that look like
/// import paths become local paths so in-package mentions can resolve.
fn doc_links_of(doc: &GoDoc) -> Option<FxHashMap<String, NudoxPath>> {
	if doc.links.is_empty() {
		return None;
	}
	let mut out = FxHashMap::default();
	for (name, url) in &doc.links {
		let path = if url.starts_with("http://") || url.starts_with("https://") {
			NudoxPath::External {
				dependency: url.clone(),
				path:       PathBuf::new(),
			}
		} else {
			NudoxPath::Local(PathBuf::from(url))
		};
		out.insert(name.clone(), path);
	}
	Some(out)
}

/// Shared Symbol scaffolding from a declaration's doc comment.
struct DocMeta {
	documentation: Option<String>,
	deprecation:   Option<Deprecation>,
	doc_links:     Option<FxHashMap<String, NudoxPath>>,
}

impl DocMeta {
	fn from_raw(raw: &str) -> Self {
		let doc = parse_docs(raw);
		Self {
			documentation: doc.documentation(),
			deprecation:   deprecation_of(&doc),
			doc_links:     doc_links_of(&doc),
		}
	}

	fn from_go_doc(doc: GoDoc) -> Self {
		Self {
			documentation: doc.documentation(),
			deprecation:   deprecation_of(&doc),
			doc_links:     doc_links_of(&doc),
		}
	}
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
	let docs = DocMeta::from_raw(&decl.doc);

	if let Some(variants) = enums.variants_by_type.get(&decl.name) {
		let underlying_note = decl
			.underlying
			.as_ref()
			.map(|u| format!("Underlying: `{}`", types::type_expr(u).name));
		let documentation = match (docs.documentation, underlying_note) {
			(Some(text), Some(note)) => Some(format!("{text}\n\n{note}")),
			(None, Some(note)) => Some(note),
			(doc, None) => doc,
		};
		// Dual-emit methods first so their paths can hang on the sum container.
		let method_paths = push_method_entries(pkg, decl, entries);
		let mut sum = ir::record::SumType::from_variants(
			variants.iter().map(|c| sum_variant(c)).collect(),
		);
		sum.underlying = decl.underlying.as_ref().map(types::lower_type);
		sum.members = if method_paths.is_empty() {
			None
		} else {
			Some(method_paths)
		};
		entries.push((
			path.clone(),
			Entry::SumType(Symbol {
				name:          decl.name.clone(),
				path,
				aliases:       None,
				visibility:    types::visibility(decl.exported),
				documentation,
				deprecation:   docs.deprecation,
				doc_links:     docs.doc_links,
				inner:         sum,
			}),
		));
		return;
	}

	match decl.underlying.as_ref().map(|u| u.kind) {
		Some(TypeKind::Struct) => {
			let method_paths = push_method_entries(pkg, decl, entries);
			let (entry, impls) = struct_entry(pkg, decl, path, method_paths, docs);
			entries.push(entry);
			entries.extend(impls);
		}
		Some(TypeKind::Interface) => {
			entries.push(interface_entry(decl, path, docs));
		}
		_ => {
			let method_paths = push_method_entries(pkg, decl, entries);
			let (entry, impls) = newtype_entry(pkg, decl, path, method_paths, docs);
			entries.push(entry);
			entries.extend(impls);
		}
	}
}

/// A variant constant → `SumVariant`. Values ride in documentation
/// (parsed spelling) and, when a simple typed value is available, as a
/// single-element tuple of a literal type so structured consumers can
/// recover the constant.
fn sum_variant(decl: &oracle::Decl) -> SumVariant {
	let value_expr = types::parse_const_value(&decl.value);
	let documentation = with_value_note(doc_of(&decl.doc), &decl.value);
	let data = value_expr.as_ref().and_then(|expr| match expr {
		ConstExpr::Int(n) => Some(SumField::Tuple(vec![IrType::Literal(ir::ty::LiteralValue {
			kind:  ir::ty::LiteralKind::Number,
			value: n.to_string(),
		})])),
		ConstExpr::Bool(b) => Some(SumField::Tuple(vec![IrType::Literal(ir::ty::LiteralValue {
			kind:  ir::ty::LiteralKind::Boolean,
			value: b.to_string(),
		})])),
		ConstExpr::Str(s) => Some(SumField::Tuple(vec![IrType::Literal(ir::ty::LiteralValue {
			kind:  ir::ty::LiteralKind::String,
			value: s.clone(),
		})])),
		_ => None,
	});
	SumVariant {
		name:          decl.name.clone(),
		data,
		documentation,
		discriminant:  value_expr,
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
		let docs = DocMeta::from_raw(&method.doc);
		entries.push((
			path.clone(),
			Entry::Function(Symbol {
				name:          method.name.clone(),
				path,
				aliases:       Some(method_aliases(&pkg.import_path, &decl.name, &method.name)),
				visibility:    types::visibility(method.exported),
				documentation: docs.documentation,
				deprecation:   docs.deprecation,
				doc_links:     docs.doc_links,
				inner:         function::lower_method(method),
			}),
		));
	}

	for method in &decl.promoted_methods {
		let path = method_key(&pkg.import_path, &decl.name, &method.name);
		paths.push(path.clone());
		let mut docs = DocMeta::from_raw(&method.doc);
		docs.documentation = promoted_doc(method);
		entries.push((
			path.clone(),
			Entry::Function(Symbol {
				name:          method.name.clone(),
				path,
				aliases:       Some(method_aliases(&pkg.import_path, &decl.name, &method.name)),
				visibility:    types::visibility(method.exported),
				documentation: docs.documentation,
				deprecation:   docs.deprecation,
				doc_links:     docs.doc_links,
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

/// Resolve oracle `implements` entries into protocol paths (same package
/// preferred; fully-qualified named types accepted).
fn protocol_paths(pkg: &oracle::Package, implements: &[oracle::Type]) -> Vec<NudoxPath> {
	implements
		.iter()
		.filter(|iface| matches!(iface.kind, TypeKind::Named | TypeKind::Alias))
		.map(|iface| {
			let import = if iface.pkg.is_empty() {
				pkg.import_path.as_str()
			} else {
				iface.pkg.as_str()
			};
			item_key(import, &iface.name)
		})
		.collect()
}

/// Build `Entry::TraitImpl` symbols for each satisfied interface.
fn trait_impl_entries(
	pkg: &oracle::Package,
	decl: &oracle::Decl,
	implements: &[oracle::Type],
) -> Vec<(NudoxPath, Entry)> {
	let for_type = IrType::TypeReference(TypeReference {
		identifier:   types::qualify(&pkg.import_path, &decl.name),
		generic_args: None,
	});
	implements
		.iter()
		.filter(|iface| matches!(iface.kind, TypeKind::Named | TypeKind::Alias))
		.map(|iface| {
			let iface_pkg = if iface.pkg.is_empty() {
				pkg.import_path.as_str()
			} else {
				iface.pkg.as_str()
			};
			// Path: import/path::Type:Iface (colon avoids clashing with methods).
			let path = NudoxPath::Local(PathBuf::from(format!(
				"{}::{}:{}",
				pkg.import_path, decl.name, iface.name
			)));
			(
				path.clone(),
				Entry::TraitImpl(Symbol {
					name:          format!("{}:{}", decl.name, iface.name),
					path,
					aliases:       None,
					visibility:    types::visibility(decl.exported),
					documentation: None,
					deprecation:   None,
					doc_links:     None,
					inner:         TraitImpl {
						tr: TraitRef {
							name: types::qualify(iface_pkg, &iface.name),
							args: iface.type_args.iter().map(types::type_expr).collect(),
						},
						for_type:             Box::new(for_type.clone()),
						generics:             types::lower_generics(&decl.type_params),
						where_constraints:    None,
						methods:              None,
						associated_types:     None,
						associated_constants: None,
						is_negative:          false,
						is_blanket:           false,
						is_unsafe:            false,
						members:              None,
					},
				}),
			)
		})
		.collect()
}

/// `type T struct { ... }` → `Entry::RecordType` (+ TraitImpls).
fn struct_entry(
	pkg: &oracle::Package,
	decl: &oracle::Decl,
	path: NudoxPath,
	method_paths: Vec<NudoxPath>,
	docs: DocMeta,
) -> ((NudoxPath, Entry), Vec<(NudoxPath, Entry)>) {
	let fields = decl
		.underlying
		.as_ref()
		.map(|u| types::lower_struct_fields(&u.fields, Some(&decl.field_docs)))
		.unwrap_or_default();

	let protocols = protocol_paths(pkg, &decl.implements);
	let impls = trait_impl_entries(pkg, decl, &decl.implements);

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
		implemented_protocols: if protocols.is_empty() { None } else { Some(protocols) },
	};

	(
		(
			path.clone(),
			Entry::RecordType(Symbol {
				name:          decl.name.clone(),
				path,
				aliases:       None,
				visibility:    types::visibility(decl.exported),
				documentation: docs.documentation,
				deprecation:   docs.deprecation,
				doc_links:     docs.doc_links,
				inner:         record,
			}),
		),
		impls,
	)
}

/// `type T interface { ... }` → `Entry::TraitDef`.
fn interface_entry(decl: &oracle::Decl, path: NudoxPath, docs: DocMeta) -> (NudoxPath, Entry) {
	let underlying = decl.underlying.as_ref();

	let mut super_traits: Vec<TraitRef> = Vec::new();
	let mut attributes: Vec<TraitAttribute> = Vec::new();
	let mut properties: Vec<Field> = Vec::new();
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
				// Constraint type set: keep a structural Union on the
				// trait as a synthetic property, and a compact custom
				// attribute for name-only consumers.
				TypeKind::Union => {
					let structural = types::lower_type(embedded);
					properties.push(Field::Known(KnownField {
						key:           FieldKey::Ident("type_set".to_string()),
						r#type:        Some(Box::new(structural)),
						default_value: None,
						attributes:    FieldAttributes {
							decorators:  vec!["go:type_set".to_string()],
							is_mutable:  false,
							is_optional: false,
							is_static:   true,
						},
						visibility:    Some(types::visibility(decl.exported)),
						documentation: Some(
							"Constraint type set (structural Union / ~ terms).".to_string(),
						),
					}));
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
				decl.method_docs.get(&sig.name).and_then(|raw| doc_of(raw))
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
		properties:         if properties.is_empty() { None } else { Some(properties) },
		required_methods:   if required_methods.is_empty() { None } else { Some(required_methods) },
		// Go interfaces cannot provide default bodies.
		provided_methods:   None,
		required_constants: None,
		attributes:         if attributes.is_empty() { None } else { Some(attributes) },
		object_safe:        None,
		sealed:             None,
		cfg:                None,
		members:            None,
	};

	(
		path.clone(),
		Entry::TraitDef(Symbol {
			name:          decl.name.clone(),
			path,
			aliases:       None,
			visibility:    types::visibility(decl.exported),
			documentation: docs.documentation,
			deprecation:   docs.deprecation,
			doc_links:     docs.doc_links,
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
	pkg: &oracle::Package,
	decl: &oracle::Decl,
	path: NudoxPath,
	method_paths: Vec<NudoxPath>,
	docs: DocMeta,
) -> ((NudoxPath, Entry), Vec<(NudoxPath, Entry)>) {
	let underlying_type = decl
		.underlying
		.as_ref()
		.map(types::lower_type)
		.unwrap_or(IrType::Infer);

	let protocols = protocol_paths(pkg, &decl.implements);
	let impls = trait_impl_entries(pkg, decl, &decl.implements);

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
		implemented_protocols: if protocols.is_empty() { None } else { Some(protocols) },
	};

	(
		(
			path.clone(),
			Entry::RecordType(Symbol {
				name:          decl.name.clone(),
				path,
				aliases:       None,
				visibility:    types::visibility(decl.exported),
				documentation: docs.documentation,
				deprecation:   docs.deprecation,
				doc_links:     docs.doc_links,
				inner:         record,
			}),
		),
		impls,
	)
}

/// `type A = B` → `Entry::TypeAlias` with optional generics.
fn lower_alias(pkg: &oracle::Package, decl: &oracle::Decl) -> (NudoxPath, Entry) {
	let path = item_key(&pkg.import_path, &decl.name);
	let target = decl.target.as_ref().map(types::lower_type).unwrap_or(IrType::Infer);
	let generics = types::lower_generics(&decl.type_params);
	let docs = DocMeta::from_raw(&decl.doc);
	(
		path.clone(),
		Entry::TypeAlias(Symbol {
			name:          decl.name.clone(),
			path,
			aliases:       None,
			visibility:    types::visibility(decl.exported),
			documentation: docs.documentation,
			deprecation:   docs.deprecation,
			doc_links:     docs.doc_links,
			inner:         TypeAliasBody::with_generics(generics, target),
		}),
	)
}

/// A package-level `func` → `Entry::Function`.
fn lower_func(pkg: &oracle::Package, decl: &oracle::Decl) -> (NudoxPath, Entry) {
	let path = item_key(&pkg.import_path, &decl.name);
	let docs = DocMeta::from_raw(&decl.doc);
	(
		path.clone(),
		Entry::Function(Symbol {
			name:          decl.name.clone(),
			path,
			aliases:       None,
			visibility:    types::visibility(decl.exported),
			documentation: docs.documentation,
			deprecation:   docs.deprecation,
			doc_links:     docs.doc_links,
			inner:         function::lower_func_decl(decl),
		}),
	)
}

/// A non-variant constant → `Entry::Constant` with typed binding payload.
/// The exact value also remains in documentation for consumers that only
/// read prose.
fn lower_const(pkg: &oracle::Package, decl: &oracle::Decl) -> (NudoxPath, Entry) {
	let path = item_key(&pkg.import_path, &decl.name);
	let ty = decl.r#type.as_ref().map(types::lower_type);
	let value = types::parse_const_value(&decl.value);
	let parsed = parse_docs(&decl.doc);
	let mut docs = DocMeta::from_go_doc(parsed);
	docs.documentation = with_value_note(docs.documentation, &decl.value);
	(
		path.clone(),
		Entry::Constant(Symbol {
			name:          decl.name.clone(),
			path,
			aliases:       None,
			visibility:    types::visibility(decl.exported),
			documentation: docs.documentation,
			deprecation:   docs.deprecation,
			doc_links:     docs.doc_links,
			inner: TypedBinding {
				ty,
				value,
				mutable: Some(false),
			},
		}),
	)
}

/// A package-level `var` → `Entry::Variable` with typed binding payload.
fn lower_var(pkg: &oracle::Package, decl: &oracle::Decl) -> (NudoxPath, Entry) {
	let path = item_key(&pkg.import_path, &decl.name);
	let ty = decl.r#type.as_ref().map(types::lower_type);
	let docs = DocMeta::from_raw(&decl.doc);
	(
		path.clone(),
		Entry::Variable(Symbol {
			name:          decl.name.clone(),
			path,
			aliases:       None,
			visibility:    types::visibility(decl.exported),
			documentation: docs.documentation,
			deprecation:   docs.deprecation,
			doc_links:     docs.doc_links,
			inner: TypedBinding {
				ty,
				value: None,
				mutable: Some(true),
			},
		}),
	)
}
