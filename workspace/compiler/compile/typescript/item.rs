//! Lowering deno-doc nodes into `ir::kind::Entry` (modules/namespaces, classes,
//! interfaces, enums, type aliases, variables, functions).

use std::{path::PathBuf, sync::Arc};

use deno_ast::swc::ast::VarDeclKind;
use deno_doc::{Declaration, DeclarationDef, class::{ClassConstructorDef, ClassDef, ClassMethodDef}, r#enum::EnumDef, interface::InterfaceDef, node::Symbol, ts_type::{CallSignatureDef, IndexSignatureDef, MethodDef}};
use ir::{entry::NudoxPath, generics::{GenericArg, TraitRef}, kind::{Entry, Visibility}, module::Module, protocols::{ReceiverKind, TraitDef, TraitMethod}, record::{Record, SumVariant}, ty::{Type, TypeReference}};
use rustc_hash::FxHashSet as HashSet;

use deno_ast::swc::ast::{Accessibility, VarDeclKind};
use deno_doc::class::{ClassConstructorDef, ClassDef, ClassMethodDef};
use deno_doc::r#enum::EnumDef;
use deno_doc::interface::InterfaceDef;
use deno_doc::js_doc::JsDoc;
use deno_doc::node::{DeclarationKind, NamespaceDef, Symbol};
use deno_doc::ts_type::{CallSignatureDef, IndexSignatureDef, MethodDef};
use deno_doc::{Declaration, DeclarationDef, Document};
use ir::entry::NudoxPath;
use ir::function::Function;
use ir::generics::{GenericArg, *};
use ir::kind::Entry;
use ir::kind::Visibility;
use ir::protocols::{ReceiverKind, TraitDef, TraitMethod};
use ir::record::{Record, SumVariant};
use ir::ty::{Type, TypeReference};

use super::{error::Parse, PropertyFieldMetadata, Result, TsDocParser};
use super::{accessibility_to_visibility, declaration_kind_to_visibility, extract_doc, path_to_id, pick_primary_declaration};
use crate::empty_to_none;

impl TsDocParser {
	pub(super) fn item_at_path(&mut self, path: &[String]) -> Result<Vec<Entry>> {
		if let Some(cached) = self.state.entry_cache.get(path) {
			return Ok(vec![cached.clone()]);
		}

		if self.state.visiting.contains(path) {
			return Err(Parse::CircularDependency { path: std::path::PathBuf::from(path.join("::")) });
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

	pub(super) fn dispatch_path(&mut self, path: &[String]) -> Result<Vec<Entry>> {
		match path.len() {
			0 => Ok(vec![]),
			1 => self.module_entry(&path[0]).map(|e| vec![e]),
			2 => self.symbol_at_path(&path[0], &path[1]),
			3 => self.member_at_path(&path[0], &path[1], &path[2]),
			_ => Ok(vec![]),
		}
	}

	pub(super) fn module_entry(&mut self, module_name: &str) -> Result<Entry> {
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
				members.push(NudoxPath::Local(PathBuf::from(sym_path.join("::"))));
			}
		}

		Ok(Entry::Module(ir::kind::Symbol {
			name: module_name.to_string(),
			path: NudoxPath::Local(PathBuf::from(module_path.join("::"))),
			aliases: None,
			visibility: Visibility::Public,
			documentation,
			inner: ir::module::Module { members: empty_to_none(members) },
		}))
	}

	pub(super) fn symbol_at_path(
		&mut self,
		module_name: &str,
		symbol_name: &str,
	) -> Result<Vec<Entry>> {
		let specifier =
			self.ctx.specifier_for_module_name(module_name).unwrap_or_else(|| module_name.to_string());

		let symbol = self
			.ctx
			.documents
			.get(&specifier)
			.and_then(|doc| doc.symbols.iter().find(|s| s.name.as_ref() == symbol_name).cloned());

		let symbol =
			symbol.ok_or_else(|| TsDeclarationError::SymbolNotFound { module: module_name.to_string(), symbol: symbol_name.to_string() })?;

		self.symbol(module_name, &symbol)
	}

	pub(super) fn symbol(&mut self, module_name: &str, symbol: &Symbol) -> Result<Vec<Entry>> {
		let path = [module_name.to_string(), symbol.name.to_string()];
		let decl = pick_primary_declaration(&symbol.declarations);
		let visibility = declaration_kind_to_visibility(decl.declaration_kind);
		let documentation = extract_doc(&decl.js_doc);

		let (kind, mut extra_entries, mut members) =
			self.declaration(module_name, symbol.name.as_ref(), decl)?;
		let overloads = self.function_overloads(&symbol.declarations, decl)?;

		for extra_decl in symbol.declarations.iter().filter(|candidate| !std::ptr::eq(*candidate, decl))
		{
			if matches!(extra_decl.def, DeclarationDef::Namespace(_)) {
				let (_, mut ns_entries, mut ns_members) =
					self.declaration(module_name, symbol.name.as_ref(), extra_decl)?;
				extra_entries.append(&mut ns_entries);
				members.append(&mut ns_members);
			}
		}

		let symbol_template = ir::kind::Symbol {
			name: symbol.name.to_string(),
			path: NudoxPath::Local(PathBuf::from(path.join("::"))),
			aliases: None,
			visibility,
			documentation,
			deprecation: None,
			doc_links: None,
			inner: (),
		};

		let entry = match kind {
			Entry::Module(s) => {
				let mut inner = s.inner;
				inner.members = empty_to_none(members);
				Entry::Module(symbol_template.clone_with(inner))
			}
			Entry::RecordType(s) => {
				let mut inner = s.inner;
				inner.members = empty_to_none(members);
				Entry::RecordType(symbol_template.clone_with(inner))
			}
			Entry::Function(s) => {
				let mut inner = s.inner;
				inner.members = empty_to_none(members);
				inner.overloads = overloads;
				Entry::Function(symbol_template.clone_with(inner))
			}
			Entry::TraitDef(s) => {
				let mut inner = s.inner;
				inner.members = empty_to_none(members);
				Entry::TraitDef(symbol_template.clone_with(inner))
			}
			Entry::TraitImpl(s) => {
				let mut inner = s.inner;
				inner.members = empty_to_none(members);
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

	pub(super) fn declaration(
		&mut self,
		module_name: &str,
		symbol_name: &str,
		decl: &Declaration,
	) -> Result<(Entry, Vec<Entry>, Vec<NudoxPath>)> {
		match &decl.def {
			DeclarationDef::Function(f) => {
				let path = vec![module_name.to_string(), symbol_name.to_string()];
				let func = self.function_def(symbol_name, f, &path)?;
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
				let variants = self.enum_def(e)?;
				Ok((Entry::SumType(ir::kind::Symbol::placeholder(variants)), vec![], vec![]))
			}

			DeclarationDef::Class(cls) => {
				let (record, extra, member_refs) = self.class_def(module_name, symbol_name, cls)?;
				Ok((Entry::RecordType(ir::kind::Symbol::placeholder(record)), extra, member_refs))
			}

			DeclarationDef::TypeAlias(ta) => {
				let ty = self.ts_type(&ta.ts_type)?;
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
					member_refs.push(NudoxPath::Local(PathBuf::from(elem_path.join("::"))));
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
					let mut batch = self.symbol(&ns_module, elem)?;
					extra_entries.append(&mut batch);
				}

				Ok((
					Entry::Module(ir::kind::Symbol::placeholder(Module { members: None })),
					extra_entries,
					member_refs,
				))
			}

			DeclarationDef::Interface(iface) => {
				let trait_def = self.interface_def(symbol_name, iface)?;
				Ok((trait_def, vec![], vec![]))
			}

			DeclarationDef::Reference(_) => {
				Ok((Entry::Info(ir::kind::Symbol::placeholder(String::new())), vec![], vec![]))
			}
		}
	}

	pub(super) fn member_at_path(
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
			.ok_or_else(|| TsDeclarationError::SymbolNotFound { module: module_name.to_string(), symbol: parent_name.to_string() })?;

		for decl in &parent_symbol.declarations {
			if let DeclarationDef::Class(cls) = &decl.def {
				if member_name == "constructor" && !cls.constructors.is_empty() {
					let constructors: Vec<&ClassConstructorDef> = cls.constructors.iter().collect();
					return self
						.constructor_group_entry(module_name, parent_name, &constructors)
						.map(|entry| vec![entry]);
				}
				let methods: Vec<&ClassMethodDef> =
					cls.methods.iter().filter(|m| m.name.as_ref() == member_name).collect();
				if !methods.is_empty() {
					return self
						.method_group_entry(module_name, parent_name, &methods)
						.map(|entry| vec![entry]);
				}
			}

			if let DeclarationDef::Namespace(ns) = &decl.def
				&& let Some(element) =
					ns.elements.iter().find(|element| element.name.as_ref() == member_name)
			{
				let nested_module_name = format!("{module_name}::{parent_name}");
				return self.symbol(&nested_module_name, element);
			}
		}

		Err(Parse::Declaration(TsDeclarationError::SymbolNotFound { module: module_name.to_string(), symbol: format!("{}::{}", parent_name, member_name) }))
	}

	pub(super) fn method_entry(
		&mut self,
		module_name: &str,
		class_name: &str,
		method: &ClassMethodDef,
	) -> Result<Entry> {
		let path = vec![module_name.to_string(), class_name.to_string(), method.name.to_string()];
		let func = self.function_def(method.name.as_ref(), &method.function_def, &path)?;
		let visibility = accessibility_to_visibility(method.accessibility);
		let documentation = extract_doc(&method.js_doc);

		Ok(Entry::Function(ir::kind::Symbol {
			name: method.name.to_string(),
			path: NudoxPath::Local(PathBuf::from(path.join("::"))),
			aliases: None,
			visibility,
			documentation,
			deprecation: None,
			doc_links: None,
			inner: func,
		}))
	}

	pub(super) fn method_group_entry(
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
		let mut entry = self.method_entry(module_name, class_name, primary)?;
		let overloads = methods
			.iter()
			.enumerate()
			.filter(|(idx, _)| *idx != primary_idx)
			.map(|(_, method)| self.function_def(method.name.as_ref(), &method.function_def, &[]))
			.collect::<Result<Vec<_>>>()?;
		if let Entry::Function(symbol) = &mut entry {
			symbol.inner.overloads = empty_to_none(overloads);
		}
		Ok(entry)
	}

	pub(super) fn constructor_entry(
		&mut self,
		module_name: &str,
		class_name: &str,
		ctor: &ClassConstructorDef,
	) -> Result<Entry> {
		let path = [module_name.to_string(), class_name.to_string(), "constructor".to_string()];
		let func = self.constructor_signature(ctor)?;

		Ok(Entry::Function(ir::kind::Symbol {
			name:          "constructor".to_string(),
			path:          NudoxPath::Local(PathBuf::from(path.join("::"))),
			aliases:       None,
			visibility:    accessibility_to_visibility(ctor.accessibility),
			documentation: extract_doc(&ctor.js_doc),
			deprecation:   None,
			doc_links:     None,
			inner:         func,
		}))
	}

	pub(super) fn constructor_group_entry(
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
		let mut entry = self.constructor_entry(module_name, class_name, primary)?;
		let overloads = constructors
			.iter()
			.enumerate()
			.filter(|(idx, _)| *idx != primary_idx)
			.map(|(_, ctor)| self.constructor_signature(ctor))
			.collect::<Result<Vec<_>>>()?;
		if let Entry::Function(symbol) = &mut entry {
			symbol.inner.overloads = empty_to_none(overloads);
		}
		Ok(entry)
	}

	pub(super) fn class_def(
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
				self.property_field(prop.name.as_ref(), prop.ts_type.as_ref(), PropertyFieldMetadata {
					optional:      prop.optional,
					readonly:      prop.readonly,
					is_static:     prop.is_static,
					visibility:    Some(accessibility_to_visibility(prop.accessibility)),
					documentation: extract_doc(&prop.js_doc),
					decorators:    &decorators,
				})
			})
			.collect::<Result<Vec<_>>>()?;
		let index_signatures = cls
			.index_signatures
			.iter()
			.map(|sig| self.index_signature(sig))
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
							.map(|ty| self.ts_type(ty).map(GenericArg::Type))
							.collect::<Result<Vec<_>>>()?,
					)
				},
			}));
		}
		for implemented in &cls.implements {
			super_types.push(self.ts_type(implemented)?);
		}

		let generics =
			if cls.type_params.is_empty() { None } else { self.type_params(&cls.type_params)? };

		let record = Record {
			name: Some(class_name.to_string()),
			generics,
			fields,
			call_signatures: None,
			constructors: None,
			methods: None,
			index_signatures: empty_to_none(index_signatures),
			super_types: empty_to_none(super_types),
			implemented_protocols: None,
			members: None,
		};

		let mut extra_entries = Vec::new();
		let mut member_refs = Vec::new();

		if !cls.constructors.is_empty() {
			let constructors: Vec<&ClassConstructorDef> = cls.constructors.iter().collect();
			let ctor_entry = self.constructor_group_entry(module_name, class_name, &constructors)?;
			member_refs.push(ctor_entry.path().clone());
			extra_entries.push(ctor_entry);
		}

		let mut seen_methods = HashSet::default();
		for method in cls.methods.iter().filter(|method| seen_methods.insert(method.name.to_string())) {
			let methods: Vec<&ClassMethodDef> =
				cls.methods.iter().filter(|candidate| candidate.name == method.name).collect();
			let method_entry = self.method_group_entry(module_name, class_name, &methods)?;
			member_refs.push(method_entry.path().clone());
			extra_entries.push(method_entry);
		}

		Ok((record, extra_entries, member_refs))
	}

	pub(super) fn interface_def(&mut self, name: &str, iface: &InterfaceDef) -> Result<Entry> {
		let generics =
			if iface.type_params.is_empty() { None } else { self.type_params(&iface.type_params)? };

		let super_traits: Option<Vec<TraitRef>> = {
			let v: Vec<TraitRef> =
				iface.extends.iter().map(|ty| self.ts_type_to_trait_ref(ty)).collect::<Result<Vec<_>>>()?;
			if v.is_empty() { None } else { Some(v) }
		};

		let mut required_methods: Vec<TraitMethod> = Vec::new();

		for m in &iface.methods {
			required_methods.push(self.interface_method(m)?);
		}
		for (i, sig) in iface.call_signatures.iter().enumerate() {
			required_methods.push(self.call_signature_as_method(sig, i)?);
		}
		for (i, sig) in iface.index_signatures.iter().enumerate() {
			required_methods.push(self.index_signature_as_method(sig, i)?);
		}

		let properties = iface
			.properties
			.iter()
			.map(|prop| {
				let mut decorators = Vec::new();
				if prop.readonly {
					decorators.push("readonly".to_string());
				}
				self.property_field(&prop.name, prop.ts_type.as_ref(), PropertyFieldMetadata {
					optional:      prop.optional,
					readonly:      prop.readonly,
					is_static:     false,
					visibility:    None,
					documentation: extract_doc(&prop.js_doc),
					decorators:    &decorators,
				})
			})
			.collect::<Result<Vec<_>>>()?;

		Ok(Entry::TraitDef(ir::kind::Symbol {
			name:          name.to_string(),
			path:          NudoxPath::Local(PathBuf::from("blank")),
			aliases:       None,
			visibility:    Visibility::Public,
			documentation: None,
			deprecation:   None,
			doc_links:     None,
			inner:         TraitDef {
				generics,
				super_traits,
				associated_types: None,
				properties: empty_to_none(properties),
				required_methods: empty_to_none(required_methods),
				provided_methods: None,
				required_constants: None,
				attributes: None,
				object_safe: None,
				sealed: None,
				cfg: None,
				members: None,
			},
		}))
	}

	pub(super) fn interface_method(&mut self, m: &MethodDef) -> Result<TraitMethod> {
		let params = m.params.iter().collect::<Vec<_>>();
		let (receiver, parameters) =
			self.params_with_receiver(&params, Some(ReceiverKind::SharedRef))?;
		let return_type = m.return_type.as_ref().map(|t| self.ts_type(t).map(Box::new)).transpose()?;

		let generics =
			if m.type_params.is_empty() { None } else { self.type_params(&m.type_params)? };

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

	pub(super) fn call_signature_as_method(
		&mut self,
		sig: &CallSignatureDef,
		idx: usize,
	) -> Result<TraitMethod> {
		let name = if idx == 0 { "__call".to_string() } else { format!("__call_{}", idx) };

		let params = sig.params.iter().collect::<Vec<_>>();
		let (receiver, parameters) =
			self.params_with_receiver(&params, Some(ReceiverKind::SharedRef))?;
		let return_type = sig.ts_type.as_ref().map(|t| self.ts_type(t).map(Box::new)).transpose()?;

		let generics =
			if sig.type_params.is_empty() { None } else { self.type_params(&sig.type_params)? };

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

	pub(super) fn index_signature_as_method(
		&mut self,
		sig: &IndexSignatureDef,
		idx: usize,
	) -> Result<TraitMethod> {
		let name = if idx == 0 { "__index".to_string() } else { format!("__index_{}", idx) };

		let params = sig.params.iter().collect::<Vec<_>>();
		let (receiver, parameters) =
			self.params_with_receiver(&params, Some(ReceiverKind::SharedRef))?;
		let return_type = sig.ts_type.as_ref().map(|t| self.ts_type(t).map(Box::new)).transpose()?;

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

	pub(super) fn enum_def(&self, enum_def: &EnumDef) -> Result<Vec<SumVariant>> {
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
