use std::{collections::{HashMap, HashSet}, path::PathBuf, sync::Arc};

use deno_ast::swc::ast::{Accessibility, TruePlusMinus, VarDeclKind};
use deno_doc::{Declaration, DeclarationDef, Document, class::{ClassConstructorDef, ClassDef, ClassMethodDef}, r#enum::EnumDef, function::FunctionDef, interface::InterfaceDef, js_doc::JsDoc, node::{DeclarationKind, NamespaceDef, Symbol}, params::{ParamDef, ParamPatternDef}, ts_type::{CallSignatureDef, IndexSignatureDef, LiteralDef, LiteralDefKind, MethodDef, ThisOrIdent, TsTypeDef, TsTypeDefKind}, ts_type_param::TsTypeParamDef};
use ir::{entry::NudoxPath, function::{Attribute as FnAttribute, Function}, generics::*, kind::{Entry, Visibility}, parameter::{Parameter, ParameterAttribute, TypeParam, TypeParamOrigin}, primitives::{Primitive, Width}, protocols::*, record::*, ty::{ConditionalType, FunctionPointer, MappedType, ModifierPrefix, PredicateSubject, QualifiedPath, Type, TypeOperator, TypePredicate, TypeReference}};


use crate::core::parse_common::{output_parameters_from_type, parameter_link_key};

pub type Result<T> = std::result::Result<T, ParseError>;

#[derive(thiserror::Error, Debug)]
pub enum ParseError {
	#[error("symbol not found: {0}")]
	SymbolNotFound(String),

	#[error("declaration has no usable kind for symbol `{symbol}`: {reason}")]
	NoUsableDeclaration { symbol: String, reason: String },

	#[error("type resolution failed for `{type_name}`: {reason}")]
	TypeResolution { type_name: String, reason: String },

	#[error("invalid parameter shape in `{context}`: {reason}")]
	InvalidParameter { context: String, reason: String },

	#[error("generic constraint resolution failed: {reason}")]
	GenericConstraintResolution { reason: String },

	#[error("interface method parsing failed for `{name}`: {reason}")]
	InterfaceMethodParsing { name: String, reason: String },

	#[error("unsupported declaration kind: {0}")]
	UnsupportedDeclarationKind(String),

	#[error("circular dependency detected at path: {path}")]
	CircularDependency { path: String },
}

pub struct TsParseContext {
	documents: HashMap<String, Document>,

	path_to_id: HashMap<Vec<String>, i64>,

	type_name_to_id: HashMap<String, i64>,

	module_name_to_specifier: HashMap<String, String>,

	specifier_to_module_name: HashMap<String, String>,
}

#[derive(Default)]
pub struct TsParseState {
	visiting:    HashSet<Vec<String>>,
	entry_cache: HashMap<Vec<String>, Entry>,
}

pub struct TsDocParser {
	ctx:   TsParseContext,
	state: TsParseState,
}

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
	let raw_segments: Vec<String> = if let Ok(url) = url::Url::parse(clean) {
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

	let mut assignments = HashMap::with_capacity(segment_map.len());

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

fn extract_doc(js_doc: &JsDoc) -> Option<String> {
	if js_doc.is_empty() { None } else { js_doc.doc.as_deref().map(str::to_string) }
}

struct PropertyFieldMetadata<'a> {
	optional:      bool,
	readonly:      bool,
	is_static:     bool,
	visibility:    Option<Visibility>,
	documentation: Option<String>,
	decorators:    &'a [String],
}

fn pick_primary_declaration(declarations: &[Declaration]) -> &Declaration {
	let impl_decl = declarations.iter().rev().find(|d| match &d.def {
		DeclarationDef::Function(f) => f.has_body,
		DeclarationDef::Class(c) => c.constructors.iter().any(|c| c.has_body),
		_ => true,
	});
	impl_decl.unwrap_or_else(|| declarations.last().unwrap_or(&declarations[0]))
}

fn is_function_declaration(decl: &Declaration) -> bool {
	matches!(decl.def, DeclarationDef::Function(_))
}

fn modifier_prefix(value: Option<TruePlusMinus>) -> Option<ModifierPrefix> {
	match value {
		Some(TruePlusMinus::True) => Some(ModifierPrefix::Preserve),
		Some(TruePlusMinus::Plus) => Some(ModifierPrefix::Add),
		Some(TruePlusMinus::Minus) => Some(ModifierPrefix::Remove),
		None => None,
	}
}

fn declaration_kind_to_visibility(kind: DeclarationKind) -> Visibility {
	match kind {
		DeclarationKind::Export => Visibility::Public,
		DeclarationKind::Private => Visibility::Private,
		DeclarationKind::Declare => Visibility::Public,
	}
}

fn accessibility_to_visibility(acc: Option<Accessibility>) -> Visibility {
	match acc {
		Some(Accessibility::Public) | None => Visibility::Public,
		Some(Accessibility::Protected) => Visibility::Protected,
		Some(Accessibility::Private) => Visibility::Private,
	}
}

impl TsDocParser {
	/// Build a parser from the deno-doc document set.
	pub fn from_doc(input: HashMap<String, Document>) -> Result<Self> { Self::new(input) }

	/// Lower the loaded documents into a flat list of IR entries.
	pub fn parse(&mut self) -> Result<Vec<Entry>> { self.parse_documents() }
}

impl TsDocParser {
	pub fn new(documents: HashMap<String, Document>) -> Result<Self> {
		let mut ctx = TsParseContext {
			documents,
			path_to_id: HashMap::new(),
			type_name_to_id: HashMap::new(),
			module_name_to_specifier: HashMap::new(),
			specifier_to_module_name: HashMap::new(),
		};
		ctx.build_path_map();
		ctx.build_type_name_map();
		Ok(Self { ctx, state: TsParseState::default() })
	}

	pub fn parse_documents(&mut self) -> Result<Vec<Entry>> {
		let mut paths: Vec<Vec<String>> = self.ctx.path_to_id.keys().cloned().collect();
		paths.sort();

		let mut entries = Vec::new();

		for path in paths {
			let batch = self.parse_item_at_path(&path)?;
			entries.extend(batch);
		}

		Ok(entries)
	}
}

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

	fn module_name_for_specifier(&self, specifier: &str) -> String {
		self
			.specifier_to_module_name
			.get(specifier)
			.cloned()
			.unwrap_or_else(|| specifier_to_module_name(specifier))
	}

	fn specifier_for_module_name(&self, module_name: &str) -> Option<String> {
		self.module_name_to_specifier.get(module_name).cloned()
	}
}

impl TsDocParser {
	fn parse_item_at_path(&mut self, path: &[String]) -> Result<Vec<Entry>> {
		if let Some(cached) = self.state.entry_cache.get(path) {
			return Ok(vec![cached.clone()]);
		}

		if self.state.visiting.contains(path) {
			return Err(ParseError::CircularDependency { path: path.join("::") });
		}
		self.state.visiting.insert(path.to_vec());

		let result = self.dispatch_path(path);

		self.state.visiting.remove(path);

		if let Ok(ref batch) = result
			&& let Some(primary) = batch.first()
		{
			self.state.entry_cache.insert(path.to_vec(), primary.clone());
		}

		result
	}

	fn dispatch_path(&mut self, path: &[String]) -> Result<Vec<Entry>> {
		match path.len() {
			0 => Ok(vec![]),
			1 => self.parse_module_entry(&path[0]).map(|e| vec![e]),
			2 => self.parse_symbol_at_path(&path[0], &path[1]),
			3 => self.parse_member_at_path(&path[0], &path[1], &path[2]),
			_ => Ok(vec![]),
		}
	}

	fn parse_module_entry(&mut self, module_name: &str) -> Result<Entry> {
		let specifier = self.ctx.specifier_for_module_name(module_name);

		let specifier = specifier.unwrap_or_else(|| module_name.to_string());
		let doc = self.ctx.documents.get(&specifier).cloned();

		let documentation = doc.as_ref().and_then(|d| d.module_doc.doc.as_deref().map(str::to_string));

		let module_path = vec![module_name.to_string()];
		let _module_id = path_to_id(&module_path);

		let mut members = Vec::new();
		if let Some(doc) = &doc {
			for symbol in &doc.symbols {
				let sym_path = [module_name.to_string(), symbol.name.to_string()];
				members.push(NudoxPath::Local(std::path::PathBuf::from(sym_path.join("::"))));
			}
		}

		Ok(Entry::Module(ir::kind::Symbol {
			name: module_name.to_string(),
			path: NudoxPath::Local(std::path::PathBuf::from(module_path.join("::"))),
			aliases: None,
			visibility: Visibility::Public,
			documentation,
			inner: ir::module::Module { members: if members.is_empty() { None } else { Some(members) } },
		}))
	}

	fn parse_symbol_at_path(&mut self, module_name: &str, symbol_name: &str) -> Result<Vec<Entry>> {
		let specifier =
			self.ctx.specifier_for_module_name(module_name).unwrap_or_else(|| module_name.to_string());

		let symbol = self
			.ctx
			.documents
			.get(&specifier)
			.and_then(|doc| doc.symbols.iter().find(|s| s.name.as_ref() == symbol_name).cloned());

		let symbol =
			symbol.ok_or_else(|| ParseError::SymbolNotFound(format!("{module_name}::{symbol_name}")))?;

		self.parse_symbol(module_name, &symbol)
	}

	fn parse_symbol(&mut self, module_name: &str, symbol: &Symbol) -> Result<Vec<Entry>> {
		let path = [module_name.to_string(), symbol.name.to_string()];
		let decl = pick_primary_declaration(&symbol.declarations);
		let visibility = declaration_kind_to_visibility(decl.declaration_kind);
		let documentation = extract_doc(&decl.js_doc);

		let (kind, mut extra_entries, mut members) =
			self.parse_declaration(module_name, symbol.name.as_ref(), decl)?;
		let overloads = self.parse_function_overloads(&symbol.declarations, decl)?;

		for extra_decl in symbol.declarations.iter().filter(|candidate| !std::ptr::eq(*candidate, decl))
		{
			if matches!(extra_decl.def, DeclarationDef::Namespace(_)) {
				let (_, mut ns_entries, mut ns_members) =
					self.parse_declaration(module_name, symbol.name.as_ref(), extra_decl)?;
				extra_entries.append(&mut ns_entries);
				members.append(&mut ns_members);
			}
		}

		let symbol_template = ir::kind::Symbol {
			name: symbol.name.to_string(),
			path: NudoxPath::Local(std::path::PathBuf::from(path.join("::"))),
			aliases: None,
			visibility,
			documentation,
			inner: (),
		};

		let entry = match kind {
			Entry::Module(s) => {
				let mut inner = s.inner;
				inner.members = if members.is_empty() { None } else { Some(members) };
				Entry::Module(symbol_template.clone_with(inner))
			}
			Entry::RecordType(s) => {
				let mut inner = s.inner;
				inner.members = if members.is_empty() { None } else { Some(members) };
				Entry::RecordType(symbol_template.clone_with(inner))
			}
			Entry::Function(s) => {
				let mut inner = s.inner;
				inner.members = if members.is_empty() { None } else { Some(members) };
				inner.overloads = overloads;
				Entry::Function(symbol_template.clone_with(inner))
			}
			Entry::TraitDef(s) => {
				let mut inner = s.inner;
				inner.members = if members.is_empty() { None } else { Some(members) };
				Entry::TraitDef(symbol_template.clone_with(inner))
			}
			Entry::TraitImpl(s) => {
				let mut inner = s.inner;
				inner.members = if members.is_empty() { None } else { Some(members) };
				Entry::TraitImpl(symbol_template.clone_with(inner))
			}
			Entry::Constant(s) => Entry::Constant({
				let _: () = s.inner;
				symbol_template.clone_with(())
			}),
			Entry::Variable(s) => Entry::Variable({
				let _: () = s.inner;
				symbol_template.clone_with(())
			}),
			Entry::Macro(s) => Entry::Macro({
				let _: () = s.inner;
				symbol_template.clone_with(())
			}),
			Entry::PrimitiveType(s) => Entry::PrimitiveType({
				let _: () = s.inner;
				symbol_template.clone_with(())
			}),
			Entry::Field(s) => Entry::Field({
				let _: () = s.inner;
				symbol_template.clone_with(())
			}),
			Entry::Event(s) => Entry::Event({
				let _: () = s.inner;
				symbol_template.clone_with(())
			}),
			Entry::Info(s) => Entry::Info(symbol_template.clone_with(s.inner)),
			Entry::UnionType(s) => Entry::UnionType(symbol_template.clone_with(s.inner)),
			Entry::TypeAlias(s) => Entry::TypeAlias(symbol_template.clone_with(s.inner)),
			Entry::SumType(s) => Entry::SumType(symbol_template.clone_with(s.inner)),
		};

		extra_entries.insert(0, entry);
		Ok(extra_entries)
	}

	fn parse_declaration(
		&mut self,
		module_name: &str,
		symbol_name: &str,
		decl: &Declaration,
	) -> Result<(Entry, Vec<Entry>, Vec<NudoxPath>)> {
		match &decl.def {
			DeclarationDef::Function(f) => {
				let path = vec![module_name.to_string(), symbol_name.to_string()];
				let func = self.parse_function_def(symbol_name, f, &path)?;
				Ok((Entry::Function(ir::kind::Symbol::placeholder(func)), vec![], vec![]))
			}

			DeclarationDef::Variable(v) => {
				let entry = if v.kind == VarDeclKind::Const {
					Entry::Constant(ir::kind::Symbol::placeholder(()))
				} else {
					Entry::Variable(ir::kind::Symbol::placeholder(()))
				};
				Ok((entry, vec![], vec![]))
			}

			DeclarationDef::Enum(e) => {
				let variants = self.parse_enum_def(e)?;
				Ok((Entry::SumType(ir::kind::Symbol::placeholder(variants)), vec![], vec![]))
			}

			DeclarationDef::Class(cls) => {
				let (record, extra, member_refs) = self.parse_class_def(module_name, symbol_name, cls)?;
				Ok((Entry::RecordType(ir::kind::Symbol::placeholder(record)), extra, member_refs))
			}

			DeclarationDef::TypeAlias(ta) => {
				let ty = self.parse_ts_type(&ta.ts_type)?;
				Ok((Entry::TypeAlias(ir::kind::Symbol::placeholder(ty)), vec![], vec![]))
			}

			DeclarationDef::Namespace(_) => {
				let specifier = self
					.ctx
					.specifier_for_module_name(module_name)
					.unwrap_or_else(|| module_name.to_string());

				let elements: Vec<String> = self
					.ctx
					.documents
					.get(&specifier)
					.and_then(|doc| doc.symbols.iter().find(|s| s.name.as_ref() == symbol_name))
					.and_then(|sym| {
						sym.declarations.iter().find_map(|d| {
							if let DeclarationDef::Namespace(n) = &d.def {
								Some(n.elements.iter().map(|e| e.name.to_string()).collect())
							} else {
								None
							}
						})
					})
					.unwrap_or_default();

				let mut extra_entries = Vec::new();
				let mut member_refs = Vec::new();

				for elem_name in &elements {
					let elem_path = [module_name.to_string(), symbol_name.to_string(), elem_name.clone()];
					member_refs.push(NudoxPath::Local(std::path::PathBuf::from(elem_path.join("::"))));
				}

				let ns_elements: Vec<Arc<Symbol>> = self
					.ctx
					.documents
					.get(&specifier)
					.and_then(|doc| doc.symbols.iter().find(|s| s.name.as_ref() == symbol_name).cloned())
					.and_then(|sym| {
						sym.declarations.iter().find_map(|d| {
							if let DeclarationDef::Namespace(n) = &d.def {
								Some(n.elements.clone())
							} else {
								None
							}
						})
					})
					.unwrap_or_default();

				for elem in &ns_elements {
					let ns_module = format!("{}.{}", module_name, symbol_name);
					let mut batch = self.parse_symbol(&ns_module, elem)?;
					extra_entries.append(&mut batch);
				}

				Ok((
					Entry::Module(ir::kind::Symbol::placeholder(ir::module::Module { members: None })),
					extra_entries,
					member_refs,
				))
			}

			DeclarationDef::Interface(iface) => {
				let trait_def = self.parse_interface_def(symbol_name, iface)?;
				Ok((trait_def, vec![], vec![]))
			}

			DeclarationDef::Reference(_) => {
				Ok((Entry::Info(ir::kind::Symbol::placeholder(String::new())), vec![], vec![]))
			}
		}
	}

	fn parse_member_at_path(
		&mut self,
		module_name: &str,
		parent_name: &str,
		member_name: &str,
	) -> Result<Vec<Entry>> {
		let specifier =
			self.ctx.specifier_for_module_name(module_name).unwrap_or_else(|| module_name.to_string());

		let parent_symbol = self
			.ctx
			.documents
			.get(&specifier)
			.and_then(|doc| doc.symbols.iter().find(|s| s.name.as_ref() == parent_name).cloned());

		let parent_symbol = parent_symbol
			.ok_or_else(|| ParseError::SymbolNotFound(format!("{module_name}::{parent_name}")))?;

		for decl in &parent_symbol.declarations {
			if let DeclarationDef::Class(cls) = &decl.def {
				if member_name == "constructor" && !cls.constructors.is_empty() {
					let constructors: Vec<&ClassConstructorDef> = cls.constructors.iter().collect();
					return self
						.parse_constructor_group_entry(module_name, parent_name, &constructors)
						.map(|entry| vec![entry]);
				}
				let methods: Vec<&ClassMethodDef> =
					cls.methods.iter().filter(|m| m.name.as_ref() == member_name).collect();
				if !methods.is_empty() {
					return self
						.parse_method_group_entry(module_name, parent_name, &methods)
						.map(|entry| vec![entry]);
				}
			}

			if let DeclarationDef::Namespace(ns) = &decl.def
				&& let Some(element) =
					ns.elements.iter().find(|element| element.name.as_ref() == member_name)
			{
				let nested_module_name = format!("{module_name}::{parent_name}");
				return self.parse_symbol(&nested_module_name, element);
			}
		}

		Err(ParseError::SymbolNotFound(format!("{module_name}::{parent_name}::{member_name}")))
	}

	fn parse_method_entry(
		&mut self,
		module_name: &str,
		class_name: &str,
		method: &ClassMethodDef,
	) -> Result<Entry> {
		let path = vec![module_name.to_string(), class_name.to_string(), method.name.to_string()];
		let func = self.parse_function_def(method.name.as_ref(), &method.function_def, &path)?;
		let visibility = accessibility_to_visibility(method.accessibility);
		let documentation = extract_doc(&method.js_doc);

		Ok(Entry::Function(ir::kind::Symbol {
			name: method.name.to_string(),
			path: NudoxPath::Local(std::path::PathBuf::from(path.join("::"))),
			aliases: None,
			visibility,
			documentation,
			inner: func,
		}))
	}

	fn parse_method_group_entry(
		&mut self,
		module_name: &str,
		class_name: &str,
		methods: &[&ClassMethodDef],
	) -> Result<Entry> {
		let primary_idx = methods
			.iter()
			.rposition(|method| method.function_def.has_body)
			.unwrap_or(methods.len().saturating_sub(1));
		let primary = methods[primary_idx];
		let mut entry = self.parse_method_entry(module_name, class_name, primary)?;
		let overloads = methods
			.iter()
			.enumerate()
			.filter(|(idx, _)| *idx != primary_idx)
			.map(|(_, method)| self.parse_function_def(method.name.as_ref(), &method.function_def, &[]))
			.collect::<Result<Vec<_>>>()?;
		if let Entry::Function(symbol) = &mut entry {
			symbol.inner.overloads = if overloads.is_empty() { None } else { Some(overloads) };
		}
		Ok(entry)
	}

	fn parse_constructor_entry(
		&mut self,
		module_name: &str,
		class_name: &str,
		ctor: &ClassConstructorDef,
	) -> Result<Entry> {
		let path = [module_name.to_string(), class_name.to_string(), "constructor".to_string()];
		let func = self.parse_constructor_signature(ctor)?;

		Ok(Entry::Function(ir::kind::Symbol {
			name:          "constructor".to_string(),
			path:          NudoxPath::Local(std::path::PathBuf::from(path.join("::"))),
			aliases:       None,
			visibility:    accessibility_to_visibility(ctor.accessibility),
			documentation: extract_doc(&ctor.js_doc),
			inner:         func,
		}))
	}

	fn parse_constructor_group_entry(
		&mut self,
		module_name: &str,
		class_name: &str,
		constructors: &[&ClassConstructorDef],
	) -> Result<Entry> {
		let primary_idx = constructors
			.iter()
			.rposition(|ctor| ctor.has_body)
			.unwrap_or(constructors.len().saturating_sub(1));
		let primary = constructors[primary_idx];
		let mut entry = self.parse_constructor_entry(module_name, class_name, primary)?;
		let overloads = constructors
			.iter()
			.enumerate()
			.filter(|(idx, _)| *idx != primary_idx)
			.map(|(_, ctor)| self.parse_constructor_signature(ctor))
			.collect::<Result<Vec<_>>>()?;
		if let Entry::Function(symbol) = &mut entry {
			symbol.inner.overloads = if overloads.is_empty() { None } else { Some(overloads) };
		}
		Ok(entry)
	}
}

impl TsDocParser {
	fn parse_function_overloads(
		&mut self,
		declarations: &[Declaration],
		primary_decl: &Declaration,
	) -> Result<Option<Vec<Function>>> {
		if !is_function_declaration(primary_decl) {
			return Ok(None);
		}
		let overloads = declarations
			.iter()
			.filter(|decl| is_function_declaration(decl) && !std::ptr::eq(*decl, primary_decl))
			.map(|decl| match &decl.def {
				DeclarationDef::Function(function_def) => self.parse_function_def("", function_def, &[]),
				_ => unreachable!(),
			})
			.collect::<Result<Vec<_>>>()?;
		Ok(if overloads.is_empty() { None } else { Some(overloads) })
	}

	fn parse_params_with_receiver(
		&mut self,
		params: &[&ParamDef],
		default_receiver: Option<ReceiverKind>,
	) -> Result<(Option<ReceiverKind>, Option<Vec<Parameter>>)> {
		let mut receiver = default_receiver;
		let mut parsed = Vec::new();

		for (idx, param) in params.iter().enumerate() {
			if idx == 0
				&& matches!(&param.pattern, ParamPatternDef::Identifier { name, .. } if name == "this")
			{
				receiver = Some(ReceiverKind::SharedRef);
				continue;
			}
			parsed.push(self.parse_param(param)?);
		}

		let parsed = if parsed.is_empty() { None } else { Some(parsed) };
		Ok((receiver, parsed))
	}

	fn parse_constructor_signature(&mut self, ctor: &ClassConstructorDef) -> Result<Function> {
		let (_, input_parameters) = self.parse_params_with_receiver(
			&ctor.params.iter().map(|param| &param.param).collect::<Vec<_>>(),
			Some(ReceiverKind::Static),
		)?;
		let type_links = self.build_type_links_for_params(input_parameters.as_deref(), None);
		Ok(Function {
			input_parameters,
			output_parameters: None,
			type_links,
			attributes: None,
			generics: None,
			receiver: Some(ReceiverKind::Static),
			overloads: None,
			implemented: ctor.has_body,
			members: None,
			implemented_protocols: None,
			body: None,
		})
	}

	fn parse_constructor_signature_from_type_literal(
		&mut self,
		ctor: &deno_doc::ts_type::ConstructorDef,
	) -> Result<Function> {
		let (_, input_parameters) = self.parse_params_with_receiver(
			&ctor.params.iter().collect::<Vec<_>>(),
			Some(ReceiverKind::Static),
		)?;
		let output_parameters = ctor
			.return_type
			.as_ref()
			.map(|return_type| self.parse_ts_type(return_type))
			.transpose()?
			.and_then(output_parameters_from_type);
		let type_links =
			self.build_type_links_for_params(input_parameters.as_deref(), output_parameters.as_deref());
		let generics =
			if ctor.type_params.is_empty() { None } else { self.parse_type_params(&ctor.type_params)? };
		Ok(Function {
			input_parameters,
			output_parameters,
			type_links,
			attributes: None,
			generics,
			receiver: Some(ReceiverKind::Static),
			overloads: None,
			implemented: false,
			members: None,
			implemented_protocols: None,
			body: None,
		})
	}

	fn parse_method_signature_function(&mut self, method: &MethodDef) -> Result<Function> {
		let params = method.params.iter().collect::<Vec<_>>();
		let (receiver, input_parameters) = self.parse_params_with_receiver(&params, None)?;
		let output_parameters = method
			.return_type
			.as_ref()
			.map(|return_type| self.parse_ts_type(return_type))
			.transpose()?
			.and_then(output_parameters_from_type);
		let type_links =
			self.build_type_links_for_params(input_parameters.as_deref(), output_parameters.as_deref());
		let generics = if method.type_params.is_empty() {
			None
		} else {
			self.parse_type_params(&method.type_params)?
		};
		Ok(Function {
			input_parameters,
			output_parameters,
			type_links,
			attributes: None,
			generics,
			receiver,
			overloads: None,
			implemented: false,
			members: None,
			implemented_protocols: None,
			body: None,
		})
	}

	fn parse_call_signature_function(&mut self, sig: &CallSignatureDef) -> Result<Function> {
		let params = sig.params.iter().collect::<Vec<_>>();
		let (receiver, input_parameters) = self.parse_params_with_receiver(&params, None)?;
		let output_parameters = sig
			.ts_type
			.as_ref()
			.map(|return_type| self.parse_ts_type(return_type))
			.transpose()?
			.and_then(output_parameters_from_type);
		let type_links =
			self.build_type_links_for_params(input_parameters.as_deref(), output_parameters.as_deref());
		let generics =
			if sig.type_params.is_empty() { None } else { self.parse_type_params(&sig.type_params)? };
		Ok(Function {
			input_parameters,
			output_parameters,
			type_links,
			attributes: None,
			generics,
			receiver,
			overloads: None,
			implemented: false,
			members: None,
			implemented_protocols: None,
			body: None,
		})
	}

	fn parse_property_field(
		&mut self,
		name: &str,
		ts_type: Option<&TsTypeDef>,
		metadata: PropertyFieldMetadata<'_>,
	) -> Result<Field> {
		let ty = ts_type.map(|t| self.parse_ts_type(t).map(Box::new)).transpose()?;
		Ok(Field::Known(ir::record::KnownField {
			key:           ir::record::FieldKey::Ident(name.to_string()),
			r#type:        ty,
			default_value: None,
			attributes:    ir::record::FieldAttributes {
				is_mutable:  !metadata.readonly,
				is_optional: metadata.optional,
				decorators:  metadata.decorators.to_vec(),
				is_static:   metadata.is_static,
			},
			visibility:    metadata.visibility,
			documentation: metadata.documentation,
		}))
	}

	fn parse_index_signature(&mut self, sig: &IndexSignatureDef) -> Result<IndexSignature> {
		let key_param = sig.params.first().ok_or_else(|| ParseError::TypeResolution {
			type_name: "index_signature".to_string(),
			reason:    "missing key parameter".to_string(),
		})?;
		let key_type = key_param.ts_type.as_ref().ok_or_else(|| ParseError::TypeResolution {
			type_name: "index_signature".to_string(),
			reason:    "missing key type".to_string(),
		})?;
		let value_type = sig.ts_type.as_ref().ok_or_else(|| ParseError::TypeResolution {
			type_name: "index_signature".to_string(),
			reason:    "missing value type".to_string(),
		})?;
		Ok(IndexSignature {
			key_type:   Box::new(self.parse_ts_type(key_type)?),
			value_type: Box::new(self.parse_ts_type(value_type)?),
		})
	}

	fn parse_type_literal_record(
		&mut self,
		name: Option<String>,
		literal: &deno_doc::ts_type::TsTypeLiteralDef,
	) -> Result<Record> {
		let fields = literal
			.properties
			.iter()
			.map(|prop| {
				self.parse_property_field(&prop.name, prop.ts_type.as_ref(), PropertyFieldMetadata {
					optional:      prop.optional,
					readonly:      prop.readonly,
					is_static:     false,
					visibility:    None,
					documentation: extract_doc(&prop.js_doc),
					decorators:    &[],
				})
			})
			.collect::<Result<Vec<_>>>()?;
		let index_signatures = literal
			.index_signatures
			.iter()
			.map(|sig| self.parse_index_signature(sig))
			.collect::<Result<Vec<_>>>()?;
		let methods = literal
			.methods
			.iter()
			.map(|method| self.parse_method_signature_function(method))
			.collect::<Result<Vec<_>>>()?;
		let constructors = literal
			.constructors
			.iter()
			.map(|ctor| self.parse_constructor_signature_from_type_literal(ctor))
			.collect::<Result<Vec<_>>>()?;
		let call_signatures = literal
			.call_signatures
			.iter()
			.map(|sig| self.parse_call_signature_function(sig))
			.collect::<Result<Vec<_>>>()?;

		Ok(Record {
			name,
			generics: None,
			fields,
			call_signatures: if call_signatures.is_empty() { None } else { Some(call_signatures) },
			constructors: if constructors.is_empty() { None } else { Some(constructors) },
			methods: if methods.is_empty() { None } else { Some(methods) },
			index_signatures: if index_signatures.is_empty() { None } else { Some(index_signatures) },
			super_types: None,
			members: None,
			implemented_protocols: None,
		})
	}

	fn parse_class_def(
		&mut self,
		module_name: &str,
		class_name: &str,
		cls: &ClassDef,
	) -> Result<(Record, Vec<Entry>, Vec<NudoxPath>)> {
		let fields = cls
			.properties
			.iter()
			.map(|prop| {
				let mut decorators =
					prop.decorators.iter().map(|decorator| decorator.to_string()).collect::<Vec<_>>();
				if prop.is_abstract {
					decorators.push("abstract".to_string());
				}
				if prop.is_override {
					decorators.push("override".to_string());
				}
				self.parse_property_field(
					prop.name.as_ref(),
					prop.ts_type.as_ref(),
					PropertyFieldMetadata {
						optional:      prop.optional,
						readonly:      prop.readonly,
						is_static:     prop.is_static,
						visibility:    Some(accessibility_to_visibility(prop.accessibility)),
						documentation: extract_doc(&prop.js_doc),
						decorators:    &decorators,
					},
				)
			})
			.collect::<Result<Vec<_>>>()?;
		let index_signatures = cls
			.index_signatures
			.iter()
			.map(|sig| self.parse_index_signature(sig))
			.collect::<Result<Vec<_>>>()?;
		let mut super_types = Vec::new();
		if let Some(extends) = &cls.extends {
			super_types.push(Type::TypeReference(TypeReference {
				identifier:   extends.to_string(),
				generic_args: if cls.super_type_params.is_empty() {
					None
				} else {
					Some(
						cls
							.super_type_params
							.iter()
							.map(|ty| self.parse_ts_type(ty).map(GenericArg::Type))
							.collect::<Result<Vec<_>>>()?,
					)
				},
			}));
		}
		for implemented in &cls.implements {
			super_types.push(self.parse_ts_type(implemented)?);
		}

		let generics =
			if cls.type_params.is_empty() { None } else { self.parse_type_params(&cls.type_params)? };

		let record = Record {
			name: Some(class_name.to_string()),
			generics,
			fields,
			call_signatures: None,
			constructors: None,
			methods: None,
			index_signatures: if index_signatures.is_empty() { None } else { Some(index_signatures) },
			super_types: if super_types.is_empty() { None } else { Some(super_types) },
			implemented_protocols: None,
			members: None,
		};

		let mut extra_entries = Vec::new();
		let mut member_refs = Vec::new();

		if !cls.constructors.is_empty() {
			let constructors: Vec<&ClassConstructorDef> = cls.constructors.iter().collect();
			let ctor_entry =
				self.parse_constructor_group_entry(module_name, class_name, &constructors)?;
			member_refs.push(ctor_entry.path().clone());
			extra_entries.push(ctor_entry);
		}

		let mut seen_methods = HashSet::new();
		for method in cls.methods.iter().filter(|method| seen_methods.insert(method.name.to_string())) {
			let methods: Vec<&ClassMethodDef> =
				cls.methods.iter().filter(|candidate| candidate.name == method.name).collect();
			let method_entry = self.parse_method_group_entry(module_name, class_name, &methods)?;
			member_refs.push(method_entry.path().clone());
			extra_entries.push(method_entry);
		}

		Ok((record, extra_entries, member_refs))
	}
}

impl TsDocParser {
	fn parse_interface_def(&mut self, name: &str, iface: &InterfaceDef) -> Result<Entry> {
		let generics = if iface.type_params.is_empty() {
			None
		} else {
			self.parse_type_params(&iface.type_params)?
		};

		let super_traits: Option<Vec<TraitRef>> = {
			let v: Vec<TraitRef> =
				iface.extends.iter().map(|ty| self.ts_type_to_trait_ref(ty)).collect::<Result<Vec<_>>>()?;
			if v.is_empty() { None } else { Some(v) }
		};

		let mut required_methods: Vec<TraitMethod> = Vec::new();

		for m in &iface.methods {
			required_methods.push(self.parse_interface_method(m)?);
		}
		for (i, sig) in iface.call_signatures.iter().enumerate() {
			required_methods.push(self.parse_call_signature_as_method(sig, i)?);
		}
		for (i, sig) in iface.index_signatures.iter().enumerate() {
			required_methods.push(self.parse_index_signature_as_method(sig, i)?);
		}

		let properties = iface
			.properties
			.iter()
			.map(|prop| {
				let mut decorators = Vec::new();
				if prop.readonly {
					decorators.push("readonly".to_string());
				}
				self.parse_property_field(&prop.name, prop.ts_type.as_ref(), PropertyFieldMetadata {
					optional:      prop.optional,
					readonly:      prop.readonly,
					is_static:     false,
					visibility:    None,
					documentation: extract_doc(&prop.js_doc),
					decorators:    &decorators,
				})
			})
			.collect::<Result<Vec<_>>>()?;

		Ok(ir::entry::Entry::TraitDef(ir::kind::Symbol {
			name:          name.to_string(),
			path:          NudoxPath::Local(PathBuf::from("blank")),
			aliases:       None,
			visibility:    Visibility::Public,
			documentation: None,
			inner:         TraitDef {
				generics,
				super_traits,
				associated_types: None,
				properties: if properties.is_empty() { None } else { Some(properties) },
				required_methods: if required_methods.is_empty() { None } else { Some(required_methods) },
				provided_methods: None,
				required_constants: None,
				attributes: None,
				members: None,
			},
		}))
	}

	fn parse_interface_method(&mut self, m: &MethodDef) -> Result<TraitMethod> {
		let params = m.params.iter().collect::<Vec<_>>();
		let (receiver, parameters) =
			self.parse_params_with_receiver(&params, Some(ReceiverKind::SharedRef))?;
		let return_type =
			m.return_type.as_ref().map(|t| self.parse_ts_type(t).map(Box::new)).transpose()?;

		let generics =
			if m.type_params.is_empty() { None } else { self.parse_type_params(&m.type_params)? };

		Ok(TraitMethod {
			name: m.name.clone(),
			parameters,
			return_type,
			generics,
			attributes: None,
			documentation: extract_doc(&m.js_doc),
			receiver,
			has_default_implementation: false,
		})
	}

	fn parse_call_signature_as_method(
		&mut self,
		sig: &CallSignatureDef,
		idx: usize,
	) -> Result<TraitMethod> {
		let name = if idx == 0 { "__call".to_string() } else { format!("__call_{}", idx) };

		let params = sig.params.iter().collect::<Vec<_>>();
		let (receiver, parameters) =
			self.parse_params_with_receiver(&params, Some(ReceiverKind::SharedRef))?;
		let return_type =
			sig.ts_type.as_ref().map(|t| self.parse_ts_type(t).map(Box::new)).transpose()?;

		let generics =
			if sig.type_params.is_empty() { None } else { self.parse_type_params(&sig.type_params)? };

		Ok(TraitMethod {
			name,
			parameters,
			return_type,
			generics,
			attributes: None,
			documentation: extract_doc(&sig.js_doc),
			receiver,
			has_default_implementation: false,
		})
	}

	fn parse_index_signature_as_method(
		&mut self,
		sig: &IndexSignatureDef,
		idx: usize,
	) -> Result<TraitMethod> {
		let name = if idx == 0 { "__index".to_string() } else { format!("__index_{}", idx) };

		let params = sig.params.iter().collect::<Vec<_>>();
		let (receiver, parameters) =
			self.parse_params_with_receiver(&params, Some(ReceiverKind::SharedRef))?;
		let return_type =
			sig.ts_type.as_ref().map(|t| self.parse_ts_type(t).map(Box::new)).transpose()?;

		Ok(TraitMethod {
			name,
			parameters,
			return_type,
			generics: None,
			attributes: None,
			documentation: extract_doc(&sig.js_doc),
			receiver,
			has_default_implementation: false,
		})
	}
}

impl TsDocParser {
	fn parse_function_def(
		&mut self,
		_name: &str,
		func: &FunctionDef,
		_path: &[String],
	) -> Result<Function> {
		let params = func.params.iter().collect::<Vec<_>>();
		let (receiver, input_parameters) = self.parse_params_with_receiver(&params, None)?;
		let output_parameters = func
			.return_type
			.as_ref()
			.map(|rt| self.parse_ts_type(rt))
			.transpose()?
			.and_then(output_parameters_from_type);

		let type_links =
			self.build_type_links_for_params(input_parameters.as_deref(), output_parameters.as_deref());

		let attributes = self.parse_function_attributes(func);

		let generics =
			if func.type_params.is_empty() { None } else { self.parse_type_params(&func.type_params)? };

		Ok(Function {
			input_parameters,
			output_parameters,
			type_links,
			attributes,
			generics,
			receiver,
			overloads: None,
			implemented: func.has_body,
			members: None,
			implemented_protocols: None,
			body: None,
		})
	}

	fn parse_function_attributes(&self, func: &FunctionDef) -> Option<Vec<FnAttribute>> {
		let mut attrs = Vec::new();
		if func.is_async {
			attrs.push(FnAttribute::Async);
		}
		if func.is_generator {
			attrs.push(FnAttribute::Generator);
		}
		if attrs.is_empty() { None } else { Some(attrs) }
	}

	fn build_type_links_for_params(
		&self,
		inputs: Option<&[Parameter]>,
		outputs: Option<&[Parameter]>,
	) -> Option<HashMap<String, i64>> {
		let mut links = HashMap::new();

		if let Some(params) = inputs {
			for (idx, param) in params.iter().enumerate() {
				if let Parameter::Literal(l) = param
					&& let Some(ref ty) = l.r#type
					&& let Some(entry_id) = self.resolve_ir_type_to_entry_id(ty)
				{
					links.insert(parameter_link_key("in", idx, params.len(), &l.name), entry_id);
				}
			}
		}

		if let Some(params) = outputs {
			for (idx, param) in params.iter().enumerate() {
				if let Parameter::Literal(l) = param
					&& let Some(ref ty) = l.r#type
					&& let Some(entry_id) = self.resolve_ir_type_to_entry_id(ty)
				{
					links.insert(parameter_link_key("out", idx, params.len(), &l.name), entry_id);
				}
			}
		}

		if links.is_empty() { None } else { Some(links) }
	}
}

impl TsDocParser {
	fn parse_enum_def(&self, enum_def: &EnumDef) -> Result<Vec<SumVariant>> {
		Ok(
			enum_def
				.members
				.iter()
				.map(|m| SumVariant {
					name:          m.name.clone(),
					data:          None,
					documentation: None,
				})
				.collect(),
		)
	}
}

impl TsDocParser {
	fn parse_param(&mut self, param: &ParamDef) -> Result<Parameter> {
		match &param.pattern {
			ParamPatternDef::Identifier { name, optional } => {
				let ty = param.ts_type.as_ref().map(|t| self.parse_ts_type(t)).transpose()?;
				let attributes = if *optional { Some(vec![ParameterAttribute::Optional]) } else { None };
				Ok(Parameter::Literal(ir::parameter::LiteralParameter {
					name: name.clone(),
					r#type: ty,
					attributes,
					default_value: None,
					description: None,
				}))
			}

			ParamPatternDef::Rest { arg } => {
				let ty = param
					.ts_type
					.as_ref()
					.map(|t| self.parse_ts_type(t))
					.transpose()?
					.or(arg.ts_type.as_ref().map(|t| self.parse_ts_type(t)).transpose()?);
				Ok(Parameter::Literal(ir::parameter::LiteralParameter {
					name:          param.to_string(),
					r#type:        ty,
					attributes:    Some(vec![ParameterAttribute::Variadic]),
					default_value: None,
					description:   None,
				}))
			}

			ParamPatternDef::Assign { left, right } => {
				let mut inner = self.parse_param(left)?;
				if let Parameter::Literal(ref mut l) = inner {
					if l.r#type.is_none() {
						l.r#type = param.ts_type.as_ref().map(|t| self.parse_ts_type(t)).transpose()?;
					}
					l.default_value = Some(ConstExpr::Var(right.clone()));
				}
				Ok(inner)
			}

			ParamPatternDef::Array { optional, .. } => {
				let ty = param.ts_type.as_ref().map(|t| self.parse_ts_type(t)).transpose()?;
				let attributes = if *optional { Some(vec![ParameterAttribute::Optional]) } else { None };
				Ok(Parameter::Literal(ir::parameter::LiteralParameter {
					name: param.to_string(),
					r#type: ty,
					attributes,
					default_value: None,
					description: None,
				}))
			}

			ParamPatternDef::Object { optional, .. } => {
				let ty = param.ts_type.as_ref().map(|t| self.parse_ts_type(t)).transpose()?;
				let attributes = if *optional { Some(vec![ParameterAttribute::Optional]) } else { None };
				Ok(Parameter::Literal(ir::parameter::LiteralParameter {
					name: param.to_string(),
					r#type: ty,
					attributes,
					default_value: None,
					description: None,
				}))
			}
		}
	}
}

impl TsDocParser {
	fn parse_type_params(&mut self, params: &[TsTypeParamDef]) -> Result<Option<Generics>> {
		if params.is_empty() {
			return Ok(None);
		}

		let mut type_params = Vec::new();
		let mut constraints = Vec::new();

		for p in params {
			let default_type = p
				.default
				.as_ref()
				.map(|t| self.parse_ts_type(t))
				.transpose()?
				.map(|ty| self.parse_type_to_expr(&ty));

			type_params.push(Parameter::Type(TypeParam {
				name: Some(p.name.clone()),
				kind: ir::generics::Kind::Type,
				variance: Variance::Invariant,
				default_type,
				params: None,
				origin: TypeParamOrigin::Free,
			}));

			if let Some(constraint) = &p.constraint {
				let trait_ref = self.ts_type_to_trait_ref(constraint)?;
				constraints.push(Constraint::TraitBound { param: p.name.clone(), trait_ref });
			}
		}

		Ok(Some(Generics { params: type_params, constraints }))
	}

	fn parse_type_to_expr(&self, ty: &Type) -> TypeExpr {
		match ty {
			Type::TypeReference(tr) => {
				let args = tr
					.generic_args
					.as_ref()
					.map(|args| {
						args
							.iter()
							.filter_map(|arg| {
								if let GenericArg::Type(t) = arg { Some(self.parse_type_to_expr(t)) } else { None }
							})
							.collect()
					})
					.unwrap_or_default();
				TypeExpr { name: tr.identifier.clone(), args }
			}
			Type::SelfType => TypeExpr { name: "Self".to_string(), args: vec![] },
			_ => TypeExpr { name: format!("{:?}", ty), args: vec![] },
		}
	}

	fn ts_type_to_trait_ref(&mut self, ty: &TsTypeDef) -> Result<TraitRef> {
		match &ty.kind {
			TsTypeDefKind::TypeRef(type_ref) => {
				let args = type_ref
					.type_params
					.as_ref()
					.map(|tp| {
						tp.iter()
							.map(|t| self.parse_ts_type(t).map(|t| self.parse_type_to_expr(&t)))
							.collect::<Result<Vec<_>>>()
					})
					.transpose()?
					.unwrap_or_default();
				Ok(TraitRef { name: type_ref.type_name.clone(), args })
			}
			_ => {
				let parsed = self.parse_ts_type(ty)?;
				let expr = self.parse_type_to_expr(&parsed);
				Ok(TraitRef { name: expr.name, args: expr.args })
			}
		}
	}
}

impl TsDocParser {
	pub fn parse_ts_type(&mut self, ts_type: &TsTypeDef) -> Result<Type> {
		match &ts_type.kind {
			TsTypeDefKind::Keyword(value) => Ok(self.parse_keyword_type(value)),

			TsTypeDefKind::Literal(value) => Ok(self.parse_literal_type(value)),

			TsTypeDefKind::TypeRef(value) => {
				let generic_args = value
					.type_params
					.as_ref()
					.map(|tp| {
						tp.iter()
							.map(|t| self.parse_ts_type(t).map(GenericArg::Type))
							.collect::<Result<Vec<_>>>()
					})
					.transpose()?
					.and_then(|v| if v.is_empty() { None } else { Some(v) });

				Ok(Type::TypeReference(TypeReference { identifier: value.type_name.clone(), generic_args }))
			}

			TsTypeDefKind::Union(value) => {
				let types: Result<Vec<Type>> = value.iter().map(|t| self.parse_ts_type(t)).collect();
				Ok(Type::Union(types?))
			}

			TsTypeDefKind::Intersection(value) => {
				let types: Result<Vec<Type>> = value.iter().map(|t| self.parse_ts_type(t)).collect();
				Ok(Type::Intersection(types?))
			}

			TsTypeDefKind::Array(value) => Ok(Type::Slice(Box::new(self.parse_ts_type(value)?))),

			TsTypeDefKind::Tuple(value) => {
				let types: Result<Vec<Type>> = value.iter().map(|t| self.parse_ts_type(t)).collect();
				Ok(Type::Tuple(types?))
			}

			TsTypeDefKind::FnOrConstructor(value) => {
				let inputs: Result<Vec<Parameter>> =
					value.params.iter().map(|p| self.parse_param_type_only(p)).collect();

				let outputs = output_parameters_from_type(self.parse_ts_type(&value.ts_type)?);

				Ok(Type::FunctionPointer(FunctionPointer {
					inputs: Some(inputs?),
					outputs,
					attributes: None,
				}))
			}

			TsTypeDefKind::Parenthesized(value) => self.parse_ts_type(value),

			TsTypeDefKind::Rest(value) => Ok(Type::Variadic(Box::new(self.parse_ts_type(value)?))),

			TsTypeDefKind::Optional(value) => self.parse_ts_type(value),

			TsTypeDefKind::TypeQuery(value) => {
				Ok(Type::TypeReference(TypeReference { identifier: value.clone(), generic_args: None }))
			}

			TsTypeDefKind::This => Ok(Type::SelfType),

			TsTypeDefKind::Conditional(value) => Ok(Type::Conditional(ConditionalType {
				check_type:   Box::new(self.parse_ts_type(&value.check_type)?),
				extends_type: Box::new(self.parse_ts_type(&value.extends_type)?),
				true_type:    Box::new(self.parse_ts_type(&value.true_type)?),
				false_type:   Box::new(self.parse_ts_type(&value.false_type)?),
			})),

			TsTypeDefKind::Infer(_) => Ok(Type::Infer),

			TsTypeDefKind::IndexedAccess(value) => {
				let self_type = Box::new(self.parse_ts_type(&value.obj_type)?);
				let index_repr = format!("{:?}", self.parse_ts_type(&value.index_type)?);
				Ok(Type::QualifiedPath(QualifiedPath {
					name: index_repr,
					generic_arguments: None,
					self_type,
					tr: None,
				}))
			}

			TsTypeDefKind::TypeOperator(value) => Ok(Type::TypeOperator(TypeOperator {
				operator: value.operator.clone(),
				r#type:   Box::new(self.parse_ts_type(&value.ts_type)?),
			})),

			TsTypeDefKind::TypeLiteral(value) => {
				Ok(Type::RecordLiteral(Box::new(self.parse_type_literal_record(None, value)?)))
			}

			TsTypeDefKind::Mapped(value) => Ok(Type::Mapped(MappedType {
				readonly:    modifier_prefix(value.readonly),
				optional:    modifier_prefix(value.optional),
				parameter:   value.type_param.name.clone(),
				source_type: Box::new(self.parse_ts_type(
					value.type_param.constraint.as_ref().ok_or_else(|| ParseError::TypeResolution {
						type_name: "mapped_type".to_string(),
						reason:    "missing mapped type source constraint".to_string(),
					})?,
				)?),
				name_type:   value
					.name_type
					.as_ref()
					.map(|ty| self.parse_ts_type(ty).map(Box::new))
					.transpose()?,
				value_type:  value
					.ts_type
					.as_ref()
					.map(|ty| self.parse_ts_type(ty).map(Box::new))
					.transpose()?,
			})),

			TsTypeDefKind::ImportType(value) => {
				let name = value.qualifier.clone().unwrap_or_else(|| value.specifier.clone());
				let generic_args = value
					.type_params
					.as_ref()
					.map(|tp| {
						tp.iter()
							.map(|t| self.parse_ts_type(t).map(GenericArg::Type))
							.collect::<Result<Vec<_>>>()
					})
					.transpose()?
					.and_then(|v| if v.is_empty() { None } else { Some(v) });
				Ok(Type::TypeReference(TypeReference { identifier: name, generic_args }))
			}

			TsTypeDefKind::TypePredicate(value) => Ok(Type::Predicate(TypePredicate {
				asserts: value.asserts,
				subject: match &value.param {
					ThisOrIdent::This => PredicateSubject::This,
					ThisOrIdent::Identifier { name } => PredicateSubject::Identifier(name.clone()),
				},
				r#type:  value
					.r#type
					.as_ref()
					.map(|ty| self.parse_ts_type(ty).map(Box::new))
					.transpose()?,
			})),

			TsTypeDefKind::Unsupported => Ok(Type::Infer),
		}
	}

	fn parse_keyword_type(&self, keyword: &str) -> Type {
		match keyword {
			"string" => Type::Primitive(Primitive::String),
			"number" => Type::Primitive(Primitive::Float(Width::W64)),
			"boolean" => Type::Primitive(Primitive::Bool),
			"bigint" => Type::Primitive(Primitive::Int(Width::W128)),
			"null" | "undefined" | "void" => Type::Tuple(vec![]),
			"never" => Type::Never,
			"any" | "unknown" => Type::Any,
			"this" => Type::SelfType,
			"object" => Type::TypeReference(TypeReference {
				identifier:   "object".to_string(),
				generic_args: None,
			}),
			"symbol" | "unique symbol" => Type::TypeReference(TypeReference {
				identifier:   "Symbol".to_string(),
				generic_args: None,
			}),
			other => {
				Type::TypeReference(TypeReference { identifier: other.to_string(), generic_args: None })
			}
		}
	}

	fn parse_literal_type(&self, lit: &LiteralDef) -> Type {
		match lit.kind {
			LiteralDefKind::String => Type::Primitive(Primitive::String),
			LiteralDefKind::Number => Type::Primitive(Primitive::Float(Width::W64)),
			LiteralDefKind::Boolean => Type::Primitive(Primitive::Bool),
			LiteralDefKind::BigInt => Type::Primitive(Primitive::Int(Width::W128)),
			LiteralDefKind::Template => Type::Primitive(Primitive::String),
		}
	}

	fn parse_param_type_only(&mut self, param: &ParamDef) -> Result<Parameter> {
		let (name, attrs) = match &param.pattern {
			ParamPatternDef::Identifier { name, optional } => {
				let attrs = if *optional { Some(vec![ParameterAttribute::Optional]) } else { None };
				(name.clone(), attrs)
			}
			ParamPatternDef::Rest { .. } => (param.to_string(), Some(vec![ParameterAttribute::Variadic])),
			ParamPatternDef::Assign { left, .. } => {
				let inner_name = match &left.pattern {
					ParamPatternDef::Identifier { name, .. } => name.clone(),
					_ => left.to_string(),
				};
				(inner_name, None)
			}
			ParamPatternDef::Array { optional, .. } => {
				let attrs = if *optional { Some(vec![ParameterAttribute::Optional]) } else { None };
				(param.to_string(), attrs)
			}
			ParamPatternDef::Object { optional, .. } => {
				let attrs = if *optional { Some(vec![ParameterAttribute::Optional]) } else { None };
				(param.to_string(), attrs)
			}
		};

		let ty = param.ts_type.as_ref().map(|t| self.parse_ts_type(t)).transpose()?;
		Ok(Parameter::Literal(ir::parameter::LiteralParameter {
			name,
			r#type: ty,
			attributes: attrs,
			default_value: None,
			description: None,
		}))
	}
}

impl TsDocParser {
	fn resolve_ir_type_to_entry_id(&self, ty: &Type) -> Option<i64> {
		match ty {
			Type::TypeReference(tr) => {
				if let Some(&id) = self.ctx.type_name_to_id.get(tr.identifier.as_str()) {
					return Some(id);
				}
				for (k, &v) in &self.ctx.type_name_to_id {
					if k.ends_with(tr.identifier.as_str()) {
						return Some(v);
					}
				}
				None
			}
			_ => None,
		}
	}
}
