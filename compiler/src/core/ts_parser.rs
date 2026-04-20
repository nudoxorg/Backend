use std::{
	collections::{HashMap, HashSet},
	sync::Arc,
};

use deno_ast::swc::ast::{Accessibility, VarDeclKind};
use deno_doc::{
	Declaration, DeclarationDef, Document,
	class::{ClassConstructorDef, ClassDef, ClassMethodDef},
	r#enum::EnumDef,
	function::FunctionDef,
	interface::InterfaceDef,
	js_doc::JsDoc,
	node::{DeclarationKind, NamespaceDef, Symbol},
	params::{ParamDef, ParamPatternDef},
	ts_type::{
		CallSignatureDef, IndexSignatureDef, LiteralDef, LiteralDefKind, MethodDef, TsTypeDef,
		TsTypeDefKind,
	},
	ts_type_param::TsTypeParamDef,
	type_alias::TypeAliasDef,
	variable::VariableDef,
};

use ir::{
	entry::{Entry, EntryRef},
	function::{Attribute as FnAttribute, Function},
	generics::*,
	kind::{Kind, Visibility},
	parameter::{Parameter, ParameterAttribute},
	primitives::Primitive,
	protocols::*,
	record::*,
	ty::{FunctionPointer, Path as IrPath, QualifiedPath, Type},
};

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
}

#[derive(Default)]
pub struct TsParseState {
	visiting: HashSet<Vec<String>>,
	entry_cache: HashMap<Vec<String>, Entry>,
}

pub struct TsDocParser {
	ctx: TsParseContext,
	state: TsParseState,
}

pub fn path_to_id(path: &[String]) -> i64 {
	use std::{
		collections::hash_map::DefaultHasher,
		hash::{Hash, Hasher},
	};
	let mut hasher = DefaultHasher::new();
	path.hash(&mut hasher);
	hasher.finish() as i64
}

/// Convert a URL specifier or local file path to a short module name.
// TODO: DEFINE NATIVELY IN IR
pub fn specifier_to_module_name(specifier: &str) -> String {
	let clean =
		specifier.split('?').next().unwrap_or(specifier).split('#').next().unwrap_or(specifier);

	let last = clean.split('/').last().unwrap_or(clean);

	let name = last
		.trim_end_matches(".ts")
		.trim_end_matches(".tsx")
		.trim_end_matches(".mts")
		.trim_end_matches(".cts")
		.trim_end_matches(".js")
		.trim_end_matches(".mjs")
		.trim_end_matches(".cjs")
		.trim_end_matches(".jsx");

	if name.is_empty() { clean.to_string() } else { name.to_string() }
}

fn extract_doc(js_doc: &JsDoc) -> Option<String> {
	if js_doc.is_empty() { None } else { js_doc.doc.as_deref().map(str::to_string) }
}

fn pick_primary_declaration(declarations: &[Declaration]) -> &Declaration {
	let impl_decl = declarations.iter().rev().find(|d| match &d.def {
		DeclarationDef::Function(f) => f.has_body,
		DeclarationDef::Class(c) => c.constructors.iter().any(|c| c.has_body),
		_ => true,
	});
	impl_decl.unwrap_or_else(|| declarations.last().unwrap_or(&declarations[0]))
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
	pub fn new(documents: HashMap<String, Document>) -> Result<Self> {
		let mut ctx =
			TsParseContext { documents, path_to_id: HashMap::new(), type_name_to_id: HashMap::new() };
		ctx.build_path_map();
		ctx.build_type_name_map();
		Ok(Self { ctx, state: TsParseState::default() })
	}

	pub fn parse_documents(&mut self) -> Result<Vec<Entry>> {
		let mut paths: Vec<Vec<String>> = self.ctx.path_to_id.keys().cloned().collect();
		paths.sort();

		let mut entries = Vec::new();

		for path in paths {
			match self.parse_item_at_path(&path) {
				Ok(batch) => entries.extend(batch),
				Err(_) => {}
			}
		}

		Ok(entries)
	}
}

impl TsParseContext {
	fn build_path_map(&mut self) {
		let specifiers: Vec<String> = self.documents.keys().cloned().collect();

		for specifier in &specifiers {
			let module_name = specifier_to_module_name(specifier);
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

		if let Ok(ref batch) = result {
			if let Some(primary) = batch.first() {
				self.state.entry_cache.insert(path.to_vec(), primary.clone());
			}
		}

		result
	}

	fn dispatch_path(&mut self, path: &[String]) -> Result<Vec<Entry>> {
		match path.len() {
			0 => Ok(vec![]),
			1 => self.parse_module_entry(&path[0]).map(|e| vec![e]),
			2 => self.parse_symbol_at_path(&path[0], &path[1]),
			3 => self.parse_member_at_path(&path[0], &path[1], &path[2]).map(|e| vec![e]),
			_ => Ok(vec![]),
		}
	}

	fn parse_module_entry(&mut self, module_name: &str) -> Result<Entry> {
		let specifier =
			self.ctx.documents.keys().find(|s| specifier_to_module_name(s) == module_name).cloned();

		let specifier = specifier.unwrap_or_else(|| module_name.to_string());
		let doc = self.ctx.documents.get(&specifier).cloned();

		let documentation = doc.as_ref().and_then(|d| d.module_doc.doc.as_deref().map(str::to_string));

		let module_path = vec![module_name.to_string()];
		let module_id = path_to_id(&module_path);

		let mut members = Vec::new();
		if let Some(doc) = &doc {
			for symbol in &doc.symbols {
				let sym_path = vec![module_name.to_string(), symbol.name.to_string()];
				members.push(EntryRef { id: path_to_id(&sym_path), path: sym_path });
			}
		}

		Ok(Entry {
			name: module_name.to_string(),
			id: module_id,
			path: module_path,
			aliases: None,
			kind: Kind::Module,
			visibility: Some(Visibility::Public),
			documentation,
			members: if members.is_empty() { None } else { Some(members) },
		})
	}

	fn parse_symbol_at_path(&mut self, module_name: &str, symbol_name: &str) -> Result<Vec<Entry>> {
		let specifier = self
			.ctx
			.documents
			.keys()
			.find(|s| specifier_to_module_name(s) == module_name)
			.cloned()
			.unwrap_or_else(|| module_name.to_string());

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
		let path = vec![module_name.to_string(), symbol.name.to_string()];
		let decl = pick_primary_declaration(&symbol.declarations);

		let visibility = Some(declaration_kind_to_visibility(decl.declaration_kind));
		let documentation = extract_doc(&decl.js_doc);

		let (kind, mut extra_entries, members) =
			self.parse_declaration(module_name, symbol.name.as_ref(), decl)?;

		let entry = Entry {
			name: symbol.name.to_string(),
			id: path_to_id(&path),
			path,
			aliases: None,
			kind,
			visibility,
			documentation,
			members: if members.is_empty() { None } else { Some(members) },
		};

		extra_entries.insert(0, entry);
		Ok(extra_entries)
	}

	fn parse_declaration(
		&mut self,
		module_name: &str,
		symbol_name: &str,
		decl: &Declaration,
	) -> Result<(Kind, Vec<Entry>, Vec<EntryRef>)> {
		match &decl.def {
			DeclarationDef::Function(f) => {
				let path = vec![module_name.to_string(), symbol_name.to_string()];
				let func = self.parse_function_def(symbol_name, f, &path)?;
				Ok((Kind::Function(func), vec![], vec![]))
			}

			DeclarationDef::Variable(v) => {
				let kind = if v.kind == VarDeclKind::Const { Kind::Constant } else { Kind::Variable };
				Ok((kind, vec![], vec![]))
			}

			DeclarationDef::Enum(e) => {
				let variants = self.parse_enum_def(e)?;
				Ok((Kind::SumType(variants), vec![], vec![]))
			}

			DeclarationDef::Class(cls) => {
				let (record, extra, member_refs) = self.parse_class_def(module_name, symbol_name, cls)?;
				Ok((Kind::RecordType(record), extra, member_refs))
			}

			DeclarationDef::TypeAlias(ta) => {
				let ty = self.parse_ts_type(&ta.ts_type)?;
				Ok((Kind::TypeAlias(ty), vec![], vec![]))
			}

			DeclarationDef::Namespace(_) => {
				let specifier = self
					.ctx
					.documents
					.keys()
					.find(|s| specifier_to_module_name(s) == module_name)
					.cloned()
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
					let elem_path = vec![module_name.to_string(), symbol_name.to_string(), elem_name.clone()];
					member_refs.push(EntryRef { id: path_to_id(&elem_path), path: elem_path });
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
					if let Ok(mut batch) = self.parse_symbol(&ns_module, elem) {
						extra_entries.append(&mut batch);
					}
				}

				Ok((Kind::Module, extra_entries, member_refs))
			}

			DeclarationDef::Interface(iface) => {
				let trait_def = self.parse_interface_def(symbol_name, iface)?;
				Ok((Kind::TraitDef(trait_def), vec![], vec![]))
			}

			DeclarationDef::Reference(_) => Ok((Kind::Info, vec![], vec![])),
		}
	}

	fn parse_member_at_path(
		&mut self,
		module_name: &str,
		parent_name: &str,
		member_name: &str,
	) -> Result<Entry> {
		let specifier = self
			.ctx
			.documents
			.keys()
			.find(|s| specifier_to_module_name(s) == module_name)
			.cloned()
			.unwrap_or_else(|| module_name.to_string());

		let parent_symbol = self
			.ctx
			.documents
			.get(&specifier)
			.and_then(|doc| doc.symbols.iter().find(|s| s.name.as_ref() == parent_name).cloned());

		let parent_symbol = parent_symbol
			.ok_or_else(|| ParseError::SymbolNotFound(format!("{module_name}::{parent_name}")))?;

		for decl in &parent_symbol.declarations {
			if let DeclarationDef::Class(cls) = &decl.def {
				if member_name == "constructor" {
					if let Some(ctor) = cls.constructors.first() {
						return self.parse_constructor_entry(module_name, parent_name, ctor);
					}
				}
				if let Some(method) = cls.methods.iter().find(|m| m.name.as_ref() == member_name) {
					return self.parse_method_entry(module_name, parent_name, method);
				}
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
		let visibility = Some(accessibility_to_visibility(method.accessibility));
		let documentation = extract_doc(&method.js_doc);

		Ok(Entry {
			name: method.name.to_string(),
			id: path_to_id(&path),
			path,
			aliases: None,
			kind: Kind::Function(func),
			visibility,
			documentation,
			members: None,
		})
	}

	fn parse_constructor_entry(
		&mut self,
		module_name: &str,
		class_name: &str,
		ctor: &ClassConstructorDef,
	) -> Result<Entry> {
		let path = vec![module_name.to_string(), class_name.to_string(), "constructor".to_string()];

		let input_parameters: Option<Vec<Parameter>> = if ctor.params.is_empty() {
			None
		} else {
			let params: Result<Vec<Parameter>> =
				ctor.params.iter().map(|cp| self.parse_param(&cp.param)).collect();
			Some(params?)
		};

		let type_links = self.build_type_links_for_params(input_parameters.as_deref(), None);

		let func = Function {
			name: "constructor".to_string(),
			input_parameters,
			output_parameters: None,
			type_links,
			attributes: None,
			generics: None,
			implemented: ctor.has_body,
			visibility: Some(accessibility_to_visibility(ctor.accessibility)),
		};

		Ok(Entry {
			name: "constructor".to_string(),
			id: path_to_id(&path),
			path,
			aliases: None,
			kind: Kind::Function(func),
			visibility: Some(Visibility::Public),
			documentation: extract_doc(&ctor.js_doc),
			members: None,
		})
	}
}

impl TsDocParser {
	fn parse_class_def(
		&mut self,
		module_name: &str,
		class_name: &str,
		cls: &ClassDef,
	) -> Result<(Record, Vec<Entry>, Vec<EntryRef>)> {
		let fields: Option<Vec<Field>> = if cls.properties.is_empty() {
			None
		} else {
			Some(
				cls
					.properties
					.iter()
					.map(|prop| {
						let ty = prop.ts_type.as_ref().and_then(|t| self.parse_ts_type(t).ok()).map(Box::new);

						let attributes = if prop.optional { Some(FieldAttribute::Optional) } else { None };

						Field {
							name: Some(prop.name.to_string()),
							type_entry_id: prop
								.ts_type
								.as_ref()
								.and_then(|t| self.resolve_ts_type_to_entry_id(t)),
							ty,
							default_value: None,
							attributes,
							visibility: Some(accessibility_to_visibility(prop.accessibility)),
						}
					})
					.collect(),
			)
		};

		let record_kind = if fields.as_ref().map(|f| f.is_empty()).unwrap_or(true) {
			RecordKind::Unit
		} else {
			RecordKind::Named
		};

		let generics =
			if cls.type_params.is_empty() { None } else { self.parse_type_params(&cls.type_params) };

		let generic_args = generics
			.as_ref()
			.and_then(|g| self.generics_to_generic_args(g))
			.and_then(|v| if v.is_empty() { None } else { Some(v) });

		let record = Record {
			name: Some(class_name.to_string()),
			generics: generic_args,
			kind: record_kind,
			fields,
			visibility: None,
		};

		let mut extra_entries = Vec::new();
		let mut member_refs = Vec::new();

		if let Some(ctor) = cls.constructors.first() {
			let ctor_entry = self.parse_constructor_entry(module_name, class_name, ctor)?;
			member_refs.push(EntryRef { id: ctor_entry.id, path: ctor_entry.path.clone() });
			extra_entries.push(ctor_entry);
		}

		let methods_cloned: Vec<ClassMethodDef> = cls.methods.iter().cloned().collect();
		for method in &methods_cloned {
			let method_entry = self.parse_method_entry(module_name, class_name, method)?;
			member_refs.push(EntryRef { id: method_entry.id, path: method_entry.path.clone() });
			extra_entries.push(method_entry);
		}

		Ok((record, extra_entries, member_refs))
	}
}

impl TsDocParser {
	fn parse_interface_def(&mut self, name: &str, iface: &InterfaceDef) -> Result<TraitDef> {
		let generics =
			if iface.type_params.is_empty() { None } else { self.parse_type_params(&iface.type_params) };

		let super_traits: Option<Vec<TraitRef>> = {
			let v: Vec<TraitRef> =
				iface.extends.iter().filter_map(|ty| self.ts_type_to_trait_ref(ty)).collect();
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

		let associated_types: Option<Vec<AssociatedType>> = {
			let v: Vec<AssociatedType> = iface
				.properties
				.iter()
				.map(|p| AssociatedType {
					name: p.name.clone(),
					bounds: None,
					default_type: p.ts_type.as_ref().and_then(|t| self.parse_ts_type(t).ok()),
					docs: extract_doc(&p.js_doc),
				})
				.collect();
			if v.is_empty() { None } else { Some(v) }
		};

		Ok(TraitDef {
			name: name.to_string(),
			generics,
			super_traits,
			associated_types,
			required_methods: if required_methods.is_empty() { None } else { Some(required_methods) },
			provided_methods: None,
			required_constants: None,
			attributes: None,
			visibility: None,
			docs: None,
		})
	}

	fn parse_interface_method(&mut self, m: &MethodDef) -> Result<TraitMethod> {
		let parameters: Option<Vec<Parameter>> = if m.params.is_empty() {
			None
		} else {
			let ps: Result<Vec<Parameter>> = m.params.iter().map(|p| self.parse_param(p)).collect();
			Some(ps?)
		};

		let return_type = m.return_type.as_ref().and_then(|t| self.parse_ts_type(t).ok()).map(Box::new);

		let generics =
			if m.type_params.is_empty() { None } else { self.parse_type_params(&m.type_params) };

		Ok(TraitMethod {
			name: m.name.clone(),
			parameters,
			return_type,
			generics,
			attributes: None,
			receiver: Some(ReceiverKind::SharedRef),
			has_default_implementation: false,
			docs: extract_doc(&m.js_doc),
		})
	}

	fn parse_call_signature_as_method(
		&mut self,
		sig: &CallSignatureDef,
		idx: usize,
	) -> Result<TraitMethod> {
		let name = if idx == 0 { "__call".to_string() } else { format!("__call_{}", idx) };

		let parameters: Option<Vec<Parameter>> = if sig.params.is_empty() {
			None
		} else {
			let ps: Result<Vec<Parameter>> = sig.params.iter().map(|p| self.parse_param(p)).collect();
			Some(ps?)
		};

		let return_type = sig.ts_type.as_ref().and_then(|t| self.parse_ts_type(t).ok()).map(Box::new);

		let generics =
			if sig.type_params.is_empty() { None } else { self.parse_type_params(&sig.type_params) };

		Ok(TraitMethod {
			name,
			parameters,
			return_type,
			generics,
			attributes: None,
			receiver: Some(ReceiverKind::SharedRef),
			has_default_implementation: false,
			docs: extract_doc(&sig.js_doc),
		})
	}

	fn parse_index_signature_as_method(
		&mut self,
		sig: &IndexSignatureDef,
		idx: usize,
	) -> Result<TraitMethod> {
		let name = if idx == 0 { "__index".to_string() } else { format!("__index_{}", idx) };

		let parameters: Option<Vec<Parameter>> = if sig.params.is_empty() {
			None
		} else {
			let ps: Result<Vec<Parameter>> = sig.params.iter().map(|p| self.parse_param(p)).collect();
			Some(ps?)
		};

		let return_type = sig.ts_type.as_ref().and_then(|t| self.parse_ts_type(t).ok()).map(Box::new);

		Ok(TraitMethod {
			name,
			parameters,
			return_type,
			generics: None,
			attributes: None,
			receiver: Some(ReceiverKind::SharedRef),
			has_default_implementation: false,
			docs: None,
		})
	}
}

impl TsDocParser {
	fn parse_function_def(
		&mut self,
		name: &str,
		func: &FunctionDef,
		_path: &[String],
	) -> Result<Function> {
		let input_parameters: Option<Vec<Parameter>> = if func.params.is_empty() {
			None
		} else {
			let ps: Result<Vec<Parameter>> = func.params.iter().map(|p| self.parse_param(p)).collect();
			Some(ps?)
		};

		let output_parameters: Option<Vec<Parameter>> = func.return_type.as_ref().and_then(|rt| {
			self.parse_ts_type(rt).ok().map(|ty| {
				vec![Parameter {
					name: "return".to_string(),
					ty: Some(ty),
					attributes: None,
					default_value: None,
					description: None,
				}]
			})
		});

		let type_links =
			self.build_type_links_for_params(input_parameters.as_deref(), output_parameters.as_deref());

		let attributes = self.parse_function_attributes(func);

		let generics =
			if func.type_params.is_empty() { None } else { self.parse_type_params(&func.type_params) };

		Ok(Function {
			name: name.to_string(),
			input_parameters,
			output_parameters,
			type_links,
			attributes,
			generics,
			implemented: func.has_body,
			visibility: None,
		})
	}

	fn parse_function_attributes(&self, func: &FunctionDef) -> Option<Vec<FnAttribute>> {
		let mut attrs = Vec::new();
		if func.is_async {
			attrs.push(FnAttribute::Async);
		}
		if func.is_generator {
			attrs.push(FnAttribute::Variadic);
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
			for param in params {
				if let Some(ref ty) = param.ty {
					if let Some(entry_id) = self.resolve_ir_type_to_entry_id(ty) {
						links.insert(format!("in.{}", param.name), entry_id);
					}
				}
			}
		}

		if let Some(params) = outputs {
			for param in params {
				if let Some(ref ty) = param.ty {
					if let Some(entry_id) = self.resolve_ir_type_to_entry_id(ty) {
						links.insert(format!("out.{}", param.name), entry_id);
					}
				}
			}
		}

		if links.is_empty() { None } else { Some(links) }
	}
}

impl TsDocParser {
	fn parse_enum_def(&self, enum_def: &EnumDef) -> Result<Vec<SumVariant>> {
		Ok(enum_def.members.iter().map(|m| SumVariant { name: m.name.clone(), types: None }).collect())
	}
}

impl TsDocParser {
	fn parse_param(&mut self, param: &ParamDef) -> Result<Parameter> {
		match &param.pattern {
			ParamPatternDef::Identifier { name, optional } => {
				let ty = param.ts_type.as_ref().and_then(|t| self.parse_ts_type(t).ok());
				let attributes = if *optional { Some(vec![ParameterAttribute::Optional]) } else { None };
				Ok(Parameter { name: name.clone(), ty, attributes, default_value: None, description: None })
			}

			ParamPatternDef::Rest { arg } => {
				let ty = param
					.ts_type
					.as_ref()
					.and_then(|t| self.parse_ts_type(t).ok())
					.or_else(|| arg.ts_type.as_ref().and_then(|t| self.parse_ts_type(t).ok()));
				Ok(Parameter {
					name: "rest".to_string(),
					ty,
					attributes: Some(vec![ParameterAttribute::Variadic]),
					default_value: None,
					description: None,
				})
			}

			ParamPatternDef::Assign { left, right } => {
				let mut inner = self.parse_param(left)?;
				if inner.ty.is_none() {
					inner.ty = param.ts_type.as_ref().and_then(|t| self.parse_ts_type(t).ok());
				}
				inner.default_value = Some(ConstExpr { expr: right.clone() });
				Ok(inner)
			}

			ParamPatternDef::Array { optional, .. } => {
				let ty = param.ts_type.as_ref().and_then(|t| self.parse_ts_type(t).ok());
				let attributes = if *optional { Some(vec![ParameterAttribute::Optional]) } else { None };
				Ok(Parameter {
					name: "array".to_string(),
					ty,
					attributes,
					default_value: None,
					description: None,
				})
			}

			ParamPatternDef::Object { optional, .. } => {
				let ty = param.ts_type.as_ref().and_then(|t| self.parse_ts_type(t).ok());
				let attributes = if *optional { Some(vec![ParameterAttribute::Optional]) } else { None };
				Ok(Parameter {
					name: "object".to_string(),
					ty,
					attributes,
					default_value: None,
					description: None,
				})
			}
		}
	}
}

impl TsDocParser {
	fn parse_type_params(&self, params: &[TsTypeParamDef]) -> Option<Generics> {
		if params.is_empty() {
			return None;
		}

		let mut type_params = Vec::new();
		let mut constraints = Vec::new();

		for p in params {
			let default_type = p
				.default
				.as_ref()
				.and_then(|t| self.parse_ts_type(t).ok())
				.map(|ty| TypeExpr { name: format!("{:?}", ty), args: vec![] });

			type_params.push(TypeParam {
				name: p.name.clone(),
				kind: TypeKind::Type,
				variance: Variance::Invariant,
				default_type,
			});

			if let Some(constraint) = &p.constraint {
				if let Some(trait_ref) = self.ts_type_to_trait_ref(constraint) {
					constraints.push(Constraint::TraitBound { param: p.name.clone(), trait_ref });
				}
			}
		}

		Some(Generics { type_params, const_params: vec![], lifetime_params: vec![], constraints })
	}

	fn generics_to_generic_args(&self, generics: &Generics) -> Option<Vec<GenericArg>> {
		let args: Vec<GenericArg> = generics
			.type_params
			.iter()
			.map(|tp| GenericArg::Type(Type::GenericParam(tp.name.clone())))
			.collect();

		if args.is_empty() { None } else { Some(args) }
	}

	fn ts_type_to_trait_ref(&self, ty: &TsTypeDef) -> Option<TraitRef> {
		match &ty.kind {
			TsTypeDefKind::TypeRef(type_ref) => {
				let args = type_ref
					.type_params
					.as_ref()
					.map(|tp| {
						tp.iter()
							.filter_map(|t| self.parse_ts_type(t).ok())
							.map(|t| TypeExpr { name: format!("{:?}", t), args: vec![] })
							.collect::<Vec<_>>()
					})
					.unwrap_or_default();
				Some(TraitRef { name: type_ref.type_name.clone(), args })
			}
			_ => None,
		}
	}
}

impl TsDocParser {
	pub fn parse_ts_type(&self, ts_type: &TsTypeDef) -> Result<Type> {
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

				Ok(Type::ResolvedPath(IrPath { path: value.type_name.clone(), generic_args }))
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

				let outputs = Some(vec![Parameter {
					name: "return".to_string(),
					ty: Some(self.parse_ts_type(&value.ts_type)?),
					attributes: None,
					default_value: None,
					description: None,
				}]);

				let generic_params = if value.type_params.is_empty() {
					None
				} else {
					self.parse_type_params(&value.type_params).map(|g| g.type_params)
				};

				Ok(Type::FunctionPointer(FunctionPointer {
					inputs: Some(inputs?),
					outputs,
					generic_params,
					attributes: None,
				}))
			}

			TsTypeDefKind::Parenthesized(value) => self.parse_ts_type(value),

			TsTypeDefKind::Rest(value) => self.parse_ts_type(value),

			TsTypeDefKind::Optional(value) => self.parse_ts_type(value),

			TsTypeDefKind::TypeQuery(value) => {
				Ok(Type::ResolvedPath(IrPath { path: value.clone(), generic_args: None }))
			}

			TsTypeDefKind::This => Ok(Type::GenericParam("this".to_string())),

			TsTypeDefKind::Conditional(value) => {
				Ok(Type::Pattern { ty: Box::new(self.parse_ts_type(&value.check_type)?) })
			}

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

			TsTypeDefKind::TypeOperator(value) => {
				Ok(Type::Pattern { ty: Box::new(self.parse_ts_type(&value.ts_type)?) })
			}

			TsTypeDefKind::TypeLiteral(_) => {
				Ok(Type::ResolvedPath(IrPath { path: "[object]".to_string(), generic_args: None }))
			}

			TsTypeDefKind::Mapped(_) => {
				Ok(Type::ResolvedPath(IrPath { path: "[mapped]".to_string(), generic_args: None }))
			}

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
				Ok(Type::ResolvedPath(IrPath { path: name, generic_args }))
			}

			TsTypeDefKind::TypePredicate(_) => Ok(Type::Primitive(Primitive::Bool(None))),

			TsTypeDefKind::Unsupported => Ok(Type::Infer),
		}
	}

	fn parse_keyword_type(&self, keyword: &str) -> Type {
		match keyword {
			"string" => Type::Primitive(Primitive::String(None)),
			"number" => Type::Primitive(Primitive::Double(None)),
			"boolean" => Type::Primitive(Primitive::Bool(None)),
			"bigint" => Type::Primitive(Primitive::Int128(None)),
			"null" | "undefined" | "never" | "void" => Type::Primitive(Primitive::Null),
			"any" | "unknown" => Type::Infer,
			"this" => Type::GenericParam("this".to_string()),
			"object" => Type::ResolvedPath(IrPath { path: "object".to_string(), generic_args: None }),
			"symbol" | "unique symbol" => {
				Type::ResolvedPath(IrPath { path: "Symbol".to_string(), generic_args: None })
			}
			other => Type::ResolvedPath(IrPath { path: other.to_string(), generic_args: None }),
		}
	}

	fn parse_literal_type(&self, lit: &LiteralDef) -> Type {
		match lit.kind {
			LiteralDefKind::String => Type::Primitive(Primitive::String(lit.string.clone())),
			LiteralDefKind::Number => Type::Primitive(Primitive::Double(lit.number)),
			LiteralDefKind::Boolean => Type::Primitive(Primitive::Bool(lit.boolean)),
			LiteralDefKind::BigInt => Type::Primitive(Primitive::Int128(None)),
			LiteralDefKind::Template => Type::Primitive(Primitive::String(None)),
		}
	}

	fn parse_param_type_only(&self, param: &ParamDef) -> Result<Parameter> {
		let (name, attrs) = match &param.pattern {
			ParamPatternDef::Identifier { name, optional } => {
				let attrs = if *optional { Some(vec![ParameterAttribute::Optional]) } else { None };
				(name.clone(), attrs)
			}
			ParamPatternDef::Rest { .. } => {
				("rest".to_string(), Some(vec![ParameterAttribute::Variadic]))
			}
			ParamPatternDef::Assign { left, .. } => {
				let inner_name = match &left.pattern {
					ParamPatternDef::Identifier { name, .. } => name.clone(),
					_ => "assign".to_string(),
				};
				(inner_name, None)
			}
			ParamPatternDef::Array { optional, .. } => {
				let attrs = if *optional { Some(vec![ParameterAttribute::Optional]) } else { None };
				("array".to_string(), attrs)
			}
			ParamPatternDef::Object { optional, .. } => {
				let attrs = if *optional { Some(vec![ParameterAttribute::Optional]) } else { None };
				("object".to_string(), attrs)
			}
		};

		let ty = param.ts_type.as_ref().and_then(|t| self.parse_ts_type(t).ok());
		Ok(Parameter { name, ty, attributes: attrs, default_value: None, description: None })
	}
}

impl TsDocParser {
	fn resolve_ts_type_to_entry_id(&self, ts_type: &TsTypeDef) -> Option<i64> {
		match &ts_type.kind {
			TsTypeDefKind::TypeRef(type_ref) => {
				let name = &type_ref.type_name;
				if let Some(&id) = self.ctx.type_name_to_id.get(name.as_str()) {
					return Some(id);
				}
				for (k, &v) in &self.ctx.type_name_to_id {
					if k.ends_with(name.as_str()) {
						return Some(v);
					}
				}
				None
			}
			_ => None,
		}
	}

	fn resolve_ir_type_to_entry_id(&self, ty: &Type) -> Option<i64> {
		match ty {
			Type::ResolvedPath(path) => {
				if let Some(&id) = self.ctx.type_name_to_id.get(path.path.as_str()) {
					return Some(id);
				}
				for (k, &v) in &self.ctx.type_name_to_id {
					if k.ends_with(path.path.as_str()) {
						return Some(v);
					}
				}
				None
			}
			_ => None,
		}
	}
}
