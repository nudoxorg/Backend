//! Lowering TypeScript into the surface IR via deno-doc.
//!
//! Discovers the declaration roots for an entry point (package.json
//! types/typings/module/main/exports, triple-slash references, ...) and lowers
//! the resulting documentation graph into an `ir::Index`.

use std::path::Path;

use deno_ast::swc::ast::{Accessibility, TruePlusMinus};
use deno_doc::{Declaration, DeclarationDef, Document, class::ClassDef, js_doc::JsDoc, node::{DeclarationKind, NamespaceDef}};
use deno_graph::ModuleSpecifier;
use ir::{entry::Index, kind::{Entry, Visibility}, ty::ModifierPrefix};
use rustc_hash::{FxHashMap as HashMap, FxHashSet as HashSet};

pub mod entry_point;
pub mod error;
pub mod function;
pub mod item;
pub mod package;
pub mod producer;
pub mod traversal;
pub mod types;

pub use self::{
	error::{Package, Parse, TsDeclarationError, TsInterfaceError, TsTypeError},
	package::TypescriptPackage,
	producer::TypescriptProducer,
};

pub type Result<T> = std::result::Result<T, Parse>;

/// Convert an empty vec to `None`, wrapping a non-empty vec in `Some`.
pub(crate) fn empty_to_none<T>(v: Vec<T>) -> Option<Vec<T>> {
	if v.is_empty() { None } else { Some(v) }
}

/// Lower a materialized TypeScript package at `root` into the indexed API
/// surface.
///
/// `name` is the package name as its manifest reports it; the entry point and
/// declaration roots are discovered from the package's `package.json`.
pub fn generate_ir(root: &Path, name: &str) -> std::result::Result<Index, Package> {
	let package = TypescriptPackage { name: name.to_string() };
	Ok(package.generate_ir(root)?.index().into_index())
}

// ============================================================================
// Core parse types
// ============================================================================

/// The immutable side of a parse: the deno-doc document set plus the maps
/// derived from it (paths → ids, module-name ⇄ specifier).
pub(super) struct TsParseContext {
	documents: HashMap<String, Document>,

	path_to_id: HashMap<Vec<String>, i64>,

	type_name_to_id: HashMap<String, i64>,

	module_name_to_specifier: HashMap<String, String>,

	specifier_to_module_name: HashMap<String, String>,
}

/// The mutable side of a parse: the cycle guard and the entry memo cache.
#[derive(Default)]
pub(super) struct TsParseState {
	visiting:    HashSet<Vec<String>>,
	entry_cache: HashMap<Vec<String>, Entry>,
}

/// Lowers a deno-doc document set into a flat list of IR entries.
pub struct TsDocParser {
	pub(super) ctx:   TsParseContext,
	pub(super) state: TsParseState,
}

// ============================================================================
// Public helpers
// ============================================================================

/// A stable 64-bit id for an entry path, used for `type_links`.
pub fn path_to_id(path: &[String]) -> i64 {
	use std::{collections::hash_map::DefaultHasher, hash::{Hash, Hasher}};
	let mut hasher = DefaultHasher::new();
	path.hash(&mut hasher);
	hasher.finish() as i64
}

/// Convert a URL specifier or local file path to a short module name.
// TODO: DEFINE NATIVELY IN IR
pub fn specifier_to_module_name(specifier: &str) -> String {
	let clean =
		specifier.split('?').next().unwrap_or(specifier).split('#').next().unwrap_or(specifier);

	let last = clean.split('/').next_back().unwrap_or(clean);

	let name = strip_typescript_like_suffix(last);

	if name.is_empty() { clean.to_string() } else { name.to_string() }
}

// ============================================================================
// Internal helpers
// ============================================================================

fn strip_typescript_like_suffix(value: &str) -> &str {
	value
		.trim_end_matches(".d.ts")
		.trim_end_matches(".d.tsx")
		.trim_end_matches(".d.mts")
		.trim_end_matches(".d.cts")
		.trim_end_matches(".ts")
		.trim_end_matches(".tsx")
		.trim_end_matches(".mts")
		.trim_end_matches(".cts")
		.trim_end_matches(".js")
		.trim_end_matches(".mjs")
		.trim_end_matches(".cjs")
		.trim_end_matches(".jsx")
}

fn specifier_to_module_segments(specifier: &str) -> Vec<String> {
	let clean =
		specifier.split('?').next().unwrap_or(specifier).split('#').next().unwrap_or(specifier);
	let raw_segments: Vec<String> = if let Ok(url) = ModuleSpecifier::parse(clean) {
		url
			.path_segments()
			.map(|segments| {
				segments.filter(|segment| !segment.is_empty()).map(str::to_string).collect::<Vec<_>>()
			})
			.unwrap_or_default()
	} else {
		clean.split('/').filter(|segment| !segment.is_empty()).map(str::to_string).collect::<Vec<_>>()
	};

	if raw_segments.is_empty() {
		return vec![clean.to_string()];
	}

	let last_index = raw_segments.len() - 1;
	raw_segments
		.into_iter()
		.enumerate()
		.map(|(idx, segment)| {
			if idx == last_index { strip_typescript_like_suffix(&segment).to_string() } else { segment }
		})
		.filter(|segment| !segment.is_empty() && segment != ".")
		.collect()
}

fn assign_unique_module_names(specifiers: &[String]) -> HashMap<String, String> {
	let mut segment_map: Vec<(String, Vec<String>)> = specifiers
		.iter()
		.cloned()
		.map(|specifier| {
			let segments = specifier_to_module_segments(&specifier);
			(specifier, if segments.is_empty() { vec!["module".to_string()] } else { segments })
		})
		.collect();

	let shared_prefix_len = shared_segment_prefix_len(
		&segment_map.iter().map(|(_, segments)| segments.as_slice()).collect::<Vec<_>>(),
	);
	if shared_prefix_len > 0 {
		for (_, segments) in &mut segment_map {
			if segments.len() > shared_prefix_len {
				segments.drain(..shared_prefix_len);
			}
		}
	}

	let mut assignments = HashMap::with_capacity_and_hasher(segment_map.len(), Default::default());

	for (specifier, segments) in &segment_map {
		let mut chosen = segments.join(".");
		for suffix_len in 1..=segments.len() {
			let candidate = segments[segments.len() - suffix_len..].join(".");
			let duplicate = segment_map.iter().any(|(other_specifier, other_segments)| {
				if other_specifier == specifier {
					return false;
				}
				other_segments.len() >= suffix_len
					&& other_segments[other_segments.len() - suffix_len..].join(".") == candidate
			});
			if !duplicate {
				chosen = candidate;
				break;
			}
		}
		assignments.insert(specifier.clone(), chosen);
	}

	assignments
}

fn shared_segment_prefix_len(segment_sets: &[&[String]]) -> usize {
	let Some(first) = segment_sets.first() else {
		return 0;
	};

	let mut prefix_len = 0;
	while prefix_len < first.len() {
		let candidate = &first[prefix_len];
		if segment_sets
			.iter()
			.all(|segments| segments.len() > prefix_len && segments[prefix_len] == *candidate)
		{
			prefix_len += 1;
		} else {
			break;
		}
	}

	prefix_len
}

pub(super) fn extract_doc(js_doc: &JsDoc) -> Option<String> {
	if js_doc.is_empty() { None } else { js_doc.doc.as_deref().map(str::to_string) }
}

pub(super) struct PropertyFieldMetadata<'a> {
	pub(super) optional:      bool,
	pub(super) readonly:      bool,
	pub(super) is_static:     bool,
	pub(super) visibility:    Option<Visibility>,
	pub(super) documentation: Option<String>,
	pub(super) decorators:    &'a [String],
}

pub(super) fn pick_primary_declaration(declarations: &[Declaration]) -> &Declaration {
	let impl_decl = declarations.iter().rev().find(|d| match &d.def {
		DeclarationDef::Function(f) => f.has_body,
		DeclarationDef::Class(c) => c.constructors.iter().any(|c| c.has_body),
		_ => true,
	});
	impl_decl.unwrap_or_else(|| declarations.last().unwrap_or(&declarations[0]))
}

pub(super) fn is_function_declaration(decl: &Declaration) -> bool {
	matches!(decl.def, DeclarationDef::Function(_))
}

pub(super) fn modifier_prefix(value: Option<TruePlusMinus>) -> Option<ModifierPrefix> {
	match value {
		Some(TruePlusMinus::True) => Some(ModifierPrefix::Preserve),
		Some(TruePlusMinus::Plus) => Some(ModifierPrefix::Add),
		Some(TruePlusMinus::Minus) => Some(ModifierPrefix::Remove),
		None => None,
	}
}

pub(super) fn declaration_kind_to_visibility(kind: DeclarationKind) -> Visibility {
	match kind {
		DeclarationKind::Export => Visibility::Public,
		DeclarationKind::Private => Visibility::Private,
		DeclarationKind::Declare => Visibility::Public,
	}
}

pub(super) fn accessibility_to_visibility(acc: Option<Accessibility>) -> Visibility {
	match acc {
		Some(Accessibility::Public) | None => Visibility::Public,
		Some(Accessibility::Protected) => Visibility::Protected,
		Some(Accessibility::Private) => Visibility::Private,
	}
}

// ============================================================================
// TsDocParser
// ============================================================================

impl TsDocParser {
	/// Build a parser from the deno-doc document set.
	pub fn from_doc(input: HashMap<String, Document>) -> Result<Self> { Self::new(input) }

	/// Build a parser from the deno-doc document set.
	pub fn new(documents: HashMap<String, Document>) -> Result<Self> {
		let mut ctx = TsParseContext {
			documents,
			path_to_id: HashMap::default(),
			type_name_to_id: HashMap::default(),
			module_name_to_specifier: HashMap::default(),
			specifier_to_module_name: HashMap::default(),
		};
		ctx.build_path_map();
		ctx.build_type_name_map();
		Ok(Self { ctx, state: TsParseState::default() })
	}

	/// Lower the loaded documents into a flat list of IR entries.
	pub fn parse(&mut self) -> Result<Vec<Entry>> { self.documents() }

	/// Lower every registered path (modules, symbols, members) in order.
	pub fn documents(&mut self) -> Result<Vec<Entry>> {
		let mut paths: Vec<Vec<String>> = self.ctx.path_to_id.keys().cloned().collect();
		paths.sort();

		let mut entries = Vec::new();

		for path in paths {
			let batch = self.item_at_path(&path)?;
			entries.extend(batch);
		}

		Ok(entries)
	}
}

// ============================================================================
// TsParseContext
// ============================================================================

impl TsParseContext {
	fn build_path_map(&mut self) {
		let specifiers: Vec<String> = self.documents.keys().cloned().collect();
		self.specifier_to_module_name = assign_unique_module_names(&specifiers);
		self.module_name_to_specifier = self
			.specifier_to_module_name
			.iter()
			.map(|(specifier, module_name)| (module_name.clone(), specifier.clone()))
			.collect();

		for specifier in &specifiers {
			let module_name = self.module_name_for_specifier(specifier);
			let module_path = vec![module_name.clone()];
			self.path_to_id.insert(module_path.clone(), path_to_id(&module_path));

			let doc = match self.documents.get(specifier.as_str()) {
				Some(d) => d.clone(),
				None => continue,
			};

			for symbol in &doc.symbols {
				let sym_path = vec![module_name.clone(), symbol.name.to_string()];
				self.path_to_id.insert(sym_path.clone(), path_to_id(&sym_path));

				for decl in &symbol.declarations {
					match &decl.def {
						DeclarationDef::Class(cls) => {
							self.register_class_members(&module_name, symbol.name.as_ref(), cls);
						}
						DeclarationDef::Namespace(ns) => {
							self.register_namespace_elements(&module_name, symbol.name.as_ref(), ns);
						}
						_ => {}
					}
				}
			}
		}
	}

	fn register_class_members(&mut self, module_name: &str, class_name: &str, cls: &ClassDef) {
		for method in cls.methods.iter() {
			let p = vec![module_name.to_string(), class_name.to_string(), method.name.to_string()];
			self.path_to_id.insert(p.clone(), path_to_id(&p));
		}
		if !cls.constructors.is_empty() {
			let p = vec![module_name.to_string(), class_name.to_string(), "constructor".to_string()];
			self.path_to_id.insert(p.clone(), path_to_id(&p));
		}
	}

	fn register_namespace_elements(&mut self, module_name: &str, ns_name: &str, ns: &NamespaceDef) {
		for element in &ns.elements {
			let p = vec![module_name.to_string(), ns_name.to_string(), element.name.to_string()];
			self.path_to_id.insert(p.clone(), path_to_id(&p));
		}
	}

	fn build_type_name_map(&mut self) {
		for (path, &id) in &self.path_to_id {
			if let Some(name) = path.last() {
				self.type_name_to_id.entry(name.clone()).or_insert(id);
			}
			let full = path.join(".");
			self.type_name_to_id.entry(full).or_insert(id);
		}
	}

	pub(super) fn module_name_for_specifier(&self, specifier: &str) -> String {
		self
			.specifier_to_module_name
			.get(specifier)
			.cloned()
			.unwrap_or_else(|| specifier_to_module_name(specifier))
	}

	pub(super) fn specifier_for_module_name(&self, module_name: &str) -> Option<String> {
		self.module_name_to_specifier.get(module_name).cloned()
	}
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
	use super::{assign_unique_module_names, specifier_to_module_name};

	#[test]
	fn specifier_to_module_name_strips_declaration_suffixes() {
		assert_eq!(specifier_to_module_name("file:///tmp/index.d.ts"), "index");
		assert_eq!(specifier_to_module_name("file:///tmp/v3/index.d.cts"), "index");
	}

	#[test]
	fn assign_unique_module_names_disambiguates_nested_indexes() {
		let specifiers = vec![
			"file:///tmp/src/index.ts".to_string(),
			"file:///tmp/src/v3/index.ts".to_string(),
			"file:///tmp/src/v4/index.ts".to_string(),
		];

		let names = assign_unique_module_names(&specifiers);
		assert_eq!(names["file:///tmp/src/index.ts"], "index");
		assert_eq!(names["file:///tmp/src/v3/index.ts"], "v3.index");
		assert_eq!(names["file:///tmp/src/v4/index.ts"], "v4.index");
	}
}
