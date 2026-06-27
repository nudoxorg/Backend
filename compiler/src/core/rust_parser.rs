use std::collections::{HashMap, HashSet, VecDeque};

use rustdoc_types::{Crate, Id, Item, ItemEnum};

pub type Result<T> = std::result::Result<T, ParseError>;

use ir::{entry::NudoxPath, function::{Attribute as FnAttribute, Function}, generics::{Term, *}, kind::{Entry, Visibility}, parameter::{ConstParam, LifetimeParam, Parameter, TypeParam, TypeParamOrigin}, primitives::{Primitive, Width}, protocols::*, record::*, ty::{DynTrait, FunctionPointer, PolyTrait, QualifiedPath, Type, TypeReference}};


use crate::core::{parse_common::{output_parameters_from_type, parameter_link_key}, rust::ParseError};

/// Immutable context.
pub struct ParseContext {
	krate:       Crate,
	/// Maps rustdoc IDs to resolved paths
	id_to_paths: HashMap<Id, HashSet<Vec<String>>>,

	/// Primitive name to ID mapping (for Genealogy resolution)
	primitive_map: HashMap<String, Id>,
	path_to_id:    HashMap<String, Id>,
}

/// Mutable state that only changes during `parse_crate`.
#[derive(Default)]
pub struct ParseState {
	/// Tracks visited items to detect circular dependencies
	visiting: HashSet<Id>,

	/// Cache of parsed entries
	entry_cache: HashMap<Id, Entry>,
}

pub struct RustdocParser {
	ctx:   ParseContext,
	state: ParseState,
}

fn add_path(id_to_paths: &mut HashMap<Id, HashSet<Vec<String>>>, id: &Id, path: Vec<String>) {
	id_to_paths
		.entry(*id)
		.and_modify(|hash_set| {
			hash_set.insert(path.clone());
		})
		.or_insert_with(|| {
			let mut hs = HashSet::default();
			hs.insert(path);
			hs
		});
}

fn queue_child(
	index: &HashMap<Id, Item>,
	id_to_paths: &mut HashMap<Id, HashSet<Vec<String>>>,
	queue: &mut VecDeque<(Id, Vec<String>)>,
	id: &Id,
	parent_path: &[String],
) {
	if let Some(item) = index.get(id)
		&& let Some(name) = &item.name
	{
		let mut path = parent_path.to_vec();
		path.push(name.clone());
		add_path(id_to_paths, id, path.clone());
		queue.push_back((*id, path));
	}
}

fn queue_impls(
	_index: &HashMap<Id, Item>,
	_id_to_paths: &mut HashMap<Id, HashSet<Vec<String>>>,
	_queue: &mut VecDeque<(Id, Vec<String>)>,
	_impls: &[Id],
	_parent_path: &[String],
) {
}

impl RustdocParser {
	/// Build a parser from a rustdoc `Crate`.
	pub fn from_doc(input: Crate) -> Result<Self> { Self::new(input) }

	/// Lower the loaded crate into a flat list of IR entries.
	pub fn parse(&mut self) -> Result<Vec<Entry>> { self.parse_crate() }
}

impl RustdocParser {
	pub fn new(krate: Crate) -> Result<Self> {
		let mut ctx = ParseContext {
			krate,
			id_to_paths: HashMap::new(),
			primitive_map: HashMap::new(),
			path_to_id: HashMap::new(),
		};
		ctx.scan_primitives();
		ctx.build_path_map()?;
		Ok(Self { ctx, state: ParseState::default() })
	}

	pub fn parse_crate(&mut self) -> Result<Vec<Entry>> {
		let mut all_ids: Vec<_> = self.ctx.id_to_paths.keys().cloned().collect();

		all_ids.sort_by(|a, b| {
			let path_a = &self.ctx.id_to_paths[a];
			let path_b = &self.ctx.id_to_paths[b];
			path_a.iter().cmp(path_b)
		});

		let mut entries = Vec::new();

		for id in all_ids {
			match self.ctx.parse_item(&mut self.state, &id) {
				Ok(entry) => {
					entries.push(entry);
				}
				Err(e) => {
					// We should probably decide if we want to fail hard here or just skip and log.
					// For now, let's at least log it if it's not a circular dependency (which is
					// handled).
					if let ParseError::CircularDependency { .. } = e {
						// Circular dependencies are expected in some cases and handled by
						// returning Err.
					} else {
						// This is a silent failure point that we should probably address.
						// todo!("Failed to parse item {}: {}. Decide on error handling
						// policy.", id.0, e);
					}
				}
			}
		}

		Ok(entries)
	}
}

impl ParseContext {
	fn scan_primitives(&mut self) {
		for (id, item) in &self.krate.index {
			if let ItemEnum::Primitive(p) = &item.inner {
				self.primitive_map.insert(p.name.clone(), *id);
				add_path(&mut self.id_to_paths, id, vec![p.name.clone()]);
			}
		}
	}

	fn build_path_map(&mut self) -> Result<()> {
		let root_id = self.krate.root;

		let mut queue = VecDeque::new();
		let mut visited = HashSet::new();

		if let Some(root_item) = self.krate.index.get(&root_id) {
			let root_name = root_item.name.clone().unwrap_or_else(|| "crate".to_string());
			let root_path = vec![root_name];
			add_path(&mut self.id_to_paths, &root_id, root_path.clone());
			queue.push_back((root_id, root_path));
		}

		while let Some((id, current_path)) = queue.pop_front() {
			if visited.contains(&id) {
				continue;
			}
			visited.insert(id);

			let item = match self.krate.index.get(&id) {
				Some(i) => i,
				None => continue,
			};

			match &item.inner {
				ItemEnum::Module(m) => {
					for child_id in &m.items {
						if let Some(child_item) = self.krate.index.get(child_id) {
							if let Some(name) = &child_item.name {
								let mut new_path = current_path.clone();
								new_path.push(name.clone());

								if !visited.contains(child_id) {
									add_path(&mut self.id_to_paths, child_id, new_path.clone());
									queue.push_back((*child_id, new_path));
								} else {
									add_path(&mut self.id_to_paths, child_id, new_path);
								}
							} else {
								if !visited.contains(child_id) {
									add_path(&mut self.id_to_paths, child_id, current_path.clone());
									queue.push_back((*child_id, current_path.clone()));
								} else {
									add_path(&mut self.id_to_paths, child_id, current_path.clone());
								}
							}
						}
					}
				}
				ItemEnum::Use(import) => {
					if let Some(target_id) = &import.id {
						let mut new_path = current_path.clone();
						new_path.push(import.name.clone());
						add_path(&mut self.id_to_paths, target_id, new_path.clone());

						if !visited.contains(target_id) {
							queue.push_back((*target_id, new_path));
						}
					} else if import.is_glob {
						// Glob imports: requires resolving the path string to an ID
						todo!("Glob imports resolution not yet implemented");
					} else {
						// This can happen for some re-exports of items from other crates
						// that are not inlined.
						todo!("Import with no target ID and not a glob: {:?}", import);
					}
				}
				ItemEnum::Struct(s) => {
					queue_impls(
						&self.krate.index,
						&mut self.id_to_paths,
						&mut queue,
						&s.impls,
						&current_path,
					);
				}
				ItemEnum::Enum(e) => {
					queue_impls(
						&self.krate.index,
						&mut self.id_to_paths,
						&mut queue,
						&e.impls,
						&current_path,
					);
				}
				ItemEnum::Trait(t) => {
					for item_id in &t.items {
						if self
							.krate
							.index
							.get(item_id)
							.is_some_and(|trait_item| matches!(trait_item.inner, ItemEnum::Function(_)))
						{
							queue_child(
								&self.krate.index,
								&mut self.id_to_paths,
								&mut queue,
								item_id,
								&current_path,
							);
						}
					}
				}
				ItemEnum::Function(_)
				| ItemEnum::Constant { .. }
				| ItemEnum::Static(_)
				| ItemEnum::TypeAlias(_)
				| ItemEnum::Macro(_)
				| ItemEnum::ProcMacro(_)
				| ItemEnum::Union(_)
				| ItemEnum::Primitive(_)
				| ItemEnum::AssocType { .. }
				| ItemEnum::AssocConst { .. }
				| ItemEnum::ExternCrate { .. } => {

					// These are terminal items in the path map traversal (for now)
				}
				_ => {
					todo!("Unhandled item type in build_path_map: {:?}", item.inner);
				}
			}
		}
		Ok(())
	}
}

impl ParseContext {
	fn parse_item(&self, state: &mut ParseState, id: &Id) -> Result<Entry> {
		if let Some(cached) = state.entry_cache.get(id) {
			return Ok(cached.clone());
		}

		if state.visiting.contains(id) {
			return Err(ParseError::CircularDependency { path: self.get_path(id)?.join("::") });
		}

		state.visiting.insert(*id);

		let item = self.krate.index.get(id).ok_or(ParseError::ItemNotFound(id.0))?;

		let entry = self.convert_item(state, id, item)?;

		state.visiting.remove(id);
		state.entry_cache.insert(*id, entry.clone());

		Ok(entry)
	}

	fn get_paths(&self, id: &Id) -> Option<&HashSet<Vec<String>>> { self.id_to_paths.get(id) }

	fn get_primary_path(&self, id: &Id) -> Result<Vec<String>> {
		self
			.get_paths(id)
			.and_then(|paths| paths.iter().min_by(|a, b| a.cmp(b)))
			.cloned()
			.ok_or_else(|| ParseError::PathParsing(format!("No path found for ID: {}", id.0)))
	}

	fn get_path(&self, id: &Id) -> Result<Vec<String>> { self.get_primary_path(id) }

	fn nudox_path_for_rustdoc_path(&self, path: &rustdoc_types::Path) -> NudoxPath {
		if let Ok(local_path) = self.get_primary_path(&path.id) {
			return NudoxPath::Local(std::path::PathBuf::from(local_path.join("::")));
		}

		let mut segments = path.path.split("::");
		let Some(dependency) = segments.next() else {
			return NudoxPath::Local(std::path::PathBuf::new());
		};
		let relative_path = segments.collect::<Vec<_>>().join("::");

		NudoxPath::External {
			dependency: dependency.to_string(),
			path:       std::path::PathBuf::from(relative_path),
		}
	}

	fn collect_implemented_protocols(&self, impl_ids: &[Id]) -> Vec<NudoxPath> {
		let mut implemented_protocols = Vec::new();
		let mut seen = HashSet::new();

		for impl_id in impl_ids {
			let Some(impl_item) = self.krate.index.get(impl_id) else {
				continue;
			};

			let ItemEnum::Impl(imp) = &impl_item.inner else {
				continue;
			};

			if imp.is_negative {
				continue;
			}

			let Some(trait_path) = &imp.trait_ else {
				continue;
			};

			let nudox_path = self.nudox_path_for_rustdoc_path(trait_path);
			if seen.insert(nudox_path.clone()) {
				implemented_protocols.push(nudox_path);
			}
		}

		implemented_protocols
	}

	fn collect_record_methods(&self, impl_ids: &[Id]) -> Result<Vec<Function>> {
		let mut methods = Vec::new();

		for impl_id in impl_ids {
			let Some(impl_item) = self.krate.index.get(impl_id) else {
				continue;
			};

			let ItemEnum::Impl(imp) = &impl_item.inner else {
				continue;
			};

			if imp.is_negative {
				continue;
			}

			let implemented_protocol =
				imp.trait_.as_ref().map(|path| self.nudox_path_for_rustdoc_path(path));

			for assoc_item_id in &imp.items {
				let Some(assoc_item) = self.krate.index.get(assoc_item_id) else {
					continue;
				};

				let ItemEnum::Function(function_item) = &assoc_item.inner else {
					continue;
				};

				let mut function = self.parse_function(assoc_item_id, function_item)?;
				function.implemented_protocols = implemented_protocol.clone().map(|path| vec![path]);
				methods.push(function);
			}
		}

		Ok(methods)
	}

	fn convert_item(&self, state: &mut ParseState, id: &Id, item: &Item) -> Result<Entry> {
		let name = item.name.clone().unwrap_or_default();
		let path = self.get_primary_path(id).unwrap_or_else(|_| vec![name.clone()]);
		let aliases = self
			.get_paths(id)
			.map(|paths| {
				let mut hs = paths.clone();
				hs.remove(&path);
				hs
			})
			.and_then(|hs| if hs.is_empty() { None } else { Some(hs) });
		let visibility = self.parse_visibility(&item.visibility);
		let documentation = item.docs.clone();
		let kind = self.parse_item_kind(state, id, &item.inner)?;

		let members = match &item.inner {
			ItemEnum::Trait(t) => {
				let mut trait_members = Vec::new();
				for method_id in &t.items {
					let Some(trait_item) = self.krate.index.get(method_id) else {
						continue;
					};

					if !matches!(trait_item.inner, ItemEnum::Function(_)) {
						continue;
					}

					match self.parse_item(state, method_id) {
						Ok(entry) => {
							trait_members.push(entry.path().clone());
						}
						Err(e) => {
							todo!(
								"Failed to parse trait member {}: {}. Decide on error handling policy.",
								method_id.0,
								e
							);
						}
					}
				}
				Some(trait_members)
			}
			ItemEnum::Module(m) => {
				let mut children = Vec::new();
				for item_id in &m.items {
					if let Ok(child) = self.parse_item(state, item_id) {
						children.push(child.path().clone());
					}
				}
				if children.is_empty() { None } else { Some(children) }
			}
			_ => None,
		};

		let symbol_template = ir::kind::Symbol {
			name,
			path: NudoxPath::Local(std::path::PathBuf::from(path.join("::"))),
			aliases,
			visibility,
			documentation,
			inner: (),
		};

		let entry = match kind {
			Entry::Module(s) => {
				let mut inner = s.inner;
				inner.members = members;
				Entry::Module(symbol_template.clone_with(inner))
			}
			Entry::RecordType(s) => {
				let mut inner = s.inner;
				inner.members = members;
				let methods = match &item.inner {
					ItemEnum::Struct(struct_item) => self.collect_record_methods(&struct_item.impls)?,
					_ => Vec::new(),
				};
				inner.methods = if methods.is_empty() { None } else { Some(methods) };
				let implemented_protocols = match &item.inner {
					ItemEnum::Struct(struct_item) => self.collect_implemented_protocols(&struct_item.impls),
					_ => Vec::new(),
				};
				inner.implemented_protocols =
					if implemented_protocols.is_empty() { None } else { Some(implemented_protocols) };
				Entry::RecordType(symbol_template.clone_with(inner))
			}
			Entry::Function(s) => {
				let mut inner = s.inner;
				inner.members = members;
				Entry::Function(symbol_template.clone_with(inner))
			}
			Entry::TraitDef(s) => {
				let mut inner = s.inner;
				inner.members = members;
				Entry::TraitDef(symbol_template.clone_with(inner))
			}
			Entry::TraitImpl(s) => {
				let mut inner = s.inner;
				inner.members = members;
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

		Ok(entry)
	}

	fn parse_item_kind(&self, state: &mut ParseState, id: &Id, inner: &ItemEnum) -> Result<Entry> {
		match inner {
			ItemEnum::Module(_) => {
				Ok(Entry::Module(ir::kind::Symbol::placeholder(ir::module::Module { members: None })))
			}

			ItemEnum::Struct(s) => {
				let record = self.parse_struct(id, s)?;
				Ok(Entry::RecordType(ir::kind::Symbol::placeholder(record)))
			}

			ItemEnum::Enum(e) => {
				let variants = self.parse_enum_variants(e)?;
				Ok(Entry::SumType(ir::kind::Symbol::placeholder(variants)))
			}

			ItemEnum::Function(f) => {
				let function = self.parse_function(id, f)?;
				Ok(Entry::Function(ir::kind::Symbol::placeholder(function)))
			}

			ItemEnum::Trait(t) => {
				let trait_def = self.parse_trait(state, id, t)?;
				Ok(Entry::TraitDef(ir::kind::Symbol::placeholder(trait_def)))
			}

			ItemEnum::Impl(i) => {
				let trait_impl = self.parse_impl(state, id, i)?;
				Ok(Entry::TraitImpl(ir::kind::Symbol::placeholder(trait_impl)))
			}

			ItemEnum::TypeAlias(alias) => {
				Ok(Entry::TypeAlias(ir::kind::Symbol::placeholder(self.parse_type(&alias.type_)?)))
			}

			ItemEnum::Constant { .. } => Ok(Entry::Constant(ir::kind::Symbol::placeholder(()))),

			ItemEnum::Static(_) => Ok(Entry::Variable(ir::kind::Symbol::placeholder(()))),

			ItemEnum::Macro(_) => Ok(Entry::Macro(ir::kind::Symbol::placeholder(()))),

			ItemEnum::ProcMacro(_) => Ok(Entry::Macro(ir::kind::Symbol::placeholder(()))),

			ItemEnum::Union(u) => {
				let types = self.parse_union_fields(u)?;
				Ok(Entry::UnionType(ir::kind::Symbol::placeholder(types)))
			}

			ItemEnum::StructField(_)
			| ItemEnum::AssocType { .. }
			| ItemEnum::AssocConst { .. }
			| ItemEnum::Variant(_)
			| ItemEnum::ExternCrate { .. } => Err(ParseError::UnsupportedItemType(format!("{inner:?}"))),

			_ => Err(ParseError::UnsupportedItemType(format!("{inner:?}"))),
		}
	}

	fn parse_struct(&self, id: &Id, s: &rustdoc_types::Struct) -> Result<Record> {
		let item = self.krate.index.get(id).ok_or(ParseError::ItemNotFound(id.0))?;
		let generics =
			s.generics.params.is_empty().then(|| self.parse_generic_params(&s.generics)).flatten();

		let fields = match &s.kind {
			rustdoc_types::StructKind::Unit => vec![],
			rustdoc_types::StructKind::Tuple(field_ids) => self.parse_tuple_fields(field_ids)?,
			rustdoc_types::StructKind::Plain { fields: field_ids, .. } => {
				self.parse_named_fields(field_ids)?
			}
		};

		let _visibility = self.parse_visibility(&item.visibility);

		Ok(Record {
			name: item.name.clone(),
			generics,
			fields,
			call_signatures: None,
			constructors: None,
			methods: None,
			index_signatures: None,
			super_types: None,
			implemented_protocols: None,
			members: None,
		})
	}

	fn parse_tuple_fields(&self, field_ids: &[Option<Id>]) -> Result<Vec<Field>> {
		field_ids
			.iter()
			.enumerate()
			.filter_map(|(idx, opt_id)| {
				opt_id.as_ref().map(|id| {
					let item = self.krate.index.get(id)?;
					if let ItemEnum::StructField(ty) = &item.inner {
						let parsed_ty = self.parse_type(ty).ok()?;

						Some(Field::Known(KnownField {
							key:           FieldKey::Index(idx),
							r#type:        Some(Box::new(parsed_ty)),
							default_value: None,
							attributes:    FieldAttributes {
								decorators:  vec![],
								is_mutable:  false,
								is_optional: false,
								is_static:   false,
							},
							visibility:    Some(self.parse_visibility(&item.visibility)),
							documentation: item.docs.clone(),
						}))
					} else {
						None
					}
				})
			})
			.collect::<Option<Vec<_>>>()
			.ok_or_else(|| ParseError::MissingField {
				field:   "tuple fields".to_string(),
				context: "struct".to_string(),
			})
	}

	fn parse_named_fields(&self, field_ids: &[Id]) -> Result<Vec<Field>> {
		field_ids
			.iter()
			.map(|id| {
				let item = self.krate.index.get(id).ok_or(ParseError::ItemNotFound(id.0))?;

				if let ItemEnum::StructField(ty) = &item.inner {
					let parsed_ty = self.parse_type(ty)?;

					Ok(Field::Known(KnownField {
						key:           FieldKey::Ident(item.name.clone().unwrap_or_default()),
						r#type:        Some(Box::new(parsed_ty)),
						default_value: None,
						attributes:    FieldAttributes {
							decorators:  vec![],
							is_mutable:  false,
							is_optional: false,
							is_static:   false,
						},
						visibility:    Some(self.parse_visibility(&item.visibility)),
						documentation: item.docs.clone(),
					}))
				} else {
					Err(ParseError::InvalidItemKind {
						id:       id.0.to_string(),
						expected: "StructField".to_string(),
						actual:   format!("{:?}", item.inner),
					})
				}
			})
			.collect()
	}

	fn parse_enum_variants(&self, e: &rustdoc_types::Enum) -> Result<Vec<SumVariant>> {
		e.variants
			.iter()
			.map(|variant_id| {
				let item =
					self.krate.index.get(variant_id).ok_or(ParseError::ItemNotFound(variant_id.0))?;

				let name = item.name.clone().unwrap_or_default();

				if let ItemEnum::Variant(v) = &item.inner {
					let data = match &v.kind {
						rustdoc_types::VariantKind::Plain => None,
						rustdoc_types::VariantKind::Tuple(fields) => {
							let types: Result<Vec<Type>> = fields
								.iter()
								.filter_map(|opt_id| opt_id.as_ref())
								.map(|id| {
									let field_item =
										self.krate.index.get(id).ok_or(ParseError::ItemNotFound(id.0))?;
									if let ItemEnum::StructField(ty) = &field_item.inner {
										self.parse_type(ty)
									} else {
										Err(ParseError::InvalidItemKind {
											id:       id.0.to_string(),
											expected: "StructField".to_string(),
											actual:   format!("{:?}", field_item.inner),
										})
									}
								})
								.collect();
							Some(ir::record::SumField::Tuple(types?))
						}
						rustdoc_types::VariantKind::Struct { fields, .. } => {
							let types: Result<Vec<Field>> = fields
								.iter()
								.map(|id| {
									let field_item =
										self.krate.index.get(id).ok_or(ParseError::ItemNotFound(id.0))?;
									if let ItemEnum::StructField(ty) = &field_item.inner {
										Ok(Field::Known(ir::record::KnownField {
											key:           ir::record::FieldKey::Ident(
												field_item.name.clone().unwrap_or_default(),
											),
											r#type:        Some(Box::new(self.parse_type(ty)?)),
											default_value: None,
											attributes:    ir::record::FieldAttributes {
												is_mutable:  false,
												is_optional: false,
												decorators:  vec![],
												is_static:   false,
											},
											visibility:    Some(self.parse_visibility(&field_item.visibility)),
											documentation: field_item.docs.clone(),
										}))
									} else {
										Err(ParseError::InvalidItemKind {
											id:       id.0.to_string(),
											expected: "StructField".to_string(),
											actual:   format!("{:?}", field_item.inner),
										})
									}
								})
								.collect();
							Some(ir::record::SumField::StructLike(types?))
						}
					};

					Ok(SumVariant { name, data, documentation: item.docs.clone() })
				} else {
					Err(ParseError::InvalidItemKind {
						id:       variant_id.0.to_string(),
						expected: "Variant".to_string(),
						actual:   format!("{:?}", item.inner),
					})
				}
			})
			.collect()
	}

	fn parse_function(&self, id: &Id, f: &rustdoc_types::Function) -> Result<Function> {
		let item = self.krate.index.get(id).ok_or(ParseError::ItemNotFound(id.0))?;
		let vis = item.visibility.clone();
		let (receiver, input_parameters) = self.parse_function_inputs(&f.sig.inputs)?;

		let output_parameters = f
			.sig
			.output
			.as_ref()
			.map(|output_ty| self.parse_type(output_ty))
			.transpose()?
			.and_then(output_parameters_from_type);

		let attributes = self.parse_function_attributes(f);

		let generics =
			if f.generics.params.is_empty() { None } else { self.parse_generic_params(&f.generics) };

		let _visibility = Some(self.parse_visibility(&vis));

		let type_links = {
			let mut links = HashMap::new();

			if let Some(ref params) = input_parameters {
				for (idx, param) in params.iter().enumerate() {
					if let Parameter::Literal(l) = param
						&& let Some(ref ty) = l.r#type
						&& let Some(entry_id) = self.resolve_type_to_entry_id(ty)
					{
						links.insert(parameter_link_key("in", idx, params.len(), &l.name), entry_id);
					}
				}
			}

			if let Some(ref params) = output_parameters {
				for (idx, param) in params.iter().enumerate() {
					if let Parameter::Literal(l) = param
						&& let Some(ref ty) = l.r#type
						&& let Some(entry_id) = self.resolve_type_to_entry_id(ty)
					{
						links.insert(parameter_link_key("out", idx, params.len(), &l.name), entry_id);
					}
				}
			}

			if !links.is_empty() { Some(links) } else { None }
		};

		Ok(Function {
			input_parameters,
			output_parameters,
			attributes,
			generics,
			receiver: match receiver {
				Some(ReceiverKind::Static) | None => None,
				other => other,
			},
			overloads: None,
			implemented: true,
			type_links,
			implemented_protocols: None,
			members: None,
			body: None,
		})
	}

	fn parse_function_inputs(
		&self,
		inputs: &[(String, rustdoc_types::Type)],
	) -> Result<(Option<ReceiverKind>, Option<Vec<Parameter>>)> {
		let receiver = Self::determine_receiver(inputs);
		let params = inputs
			.iter()
			.enumerate()
			.filter(|(idx, (name, _))| !(*idx == 0 && name == "self"))
			.map(|(_, (name, ty))| {
				let parsed_ty = self.parse_type(ty)?;
				Ok(Parameter::Literal(ir::parameter::LiteralParameter {
					name:          name.clone(),
					r#type:        Some(parsed_ty),
					attributes:    None,
					default_value: None,
					description:   None,
				}))
			})
			.collect::<Result<Vec<_>>>()?;
		Ok((receiver, if params.is_empty() { None } else { Some(params) }))
	}

	fn parse_function_attributes(&self, f: &rustdoc_types::Function) -> Option<Vec<FnAttribute>> {
		let mut attrs = Vec::new();

		let header = &f.header;
		if header.is_const {
			attrs.push(FnAttribute::Const);
		}
		if header.is_unsafe {
			attrs.push(FnAttribute::Unsafe);
		}
		if header.is_async {
			attrs.push(FnAttribute::Async);
		}

		if attrs.is_empty() { None } else { Some(attrs) }
	}

	fn parse_trait(
		&self,
		_state: &mut ParseState,
		_id: &Id,
		t: &rustdoc_types::Trait,
	) -> Result<TraitDef> {
		let generics =
			if t.generics.params.is_empty() { None } else { self.parse_generic_params(&t.generics) };

		let super_traits =
			if t.bounds.is_empty() { None } else { Some(self.parse_trait_bounds(&t.bounds)?) };

		let mut required_methods = Vec::new();
		let mut provided_methods = Vec::new();
		let mut associated_types = Vec::new();
		let mut required_constants = Vec::new();

		for item_id in &t.items {
			let trait_item = self.krate.index.get(item_id).ok_or(ParseError::ItemNotFound(item_id.0))?;

			match &trait_item.inner {
				ItemEnum::Function(f) => {
					let method = self.parse_trait_method(item_id, f)?;
					if f.has_body {
						provided_methods.push(method);
					} else {
						required_methods.push(method);
					}
				}
				ItemEnum::AssocType { generics: _, bounds, type_ } => {
					let assoc_type = AssociatedType {
						name:         trait_item.name.clone().unwrap_or_default(),
						bounds:       if bounds.is_empty() {
							None
						} else {
							Some(self.parse_generic_bounds(bounds)?)
						},
						default_type: if let Some(ty) = type_ { Some(self.parse_type(ty)?) } else { None },
					};
					associated_types.push(assoc_type);
				}
				ItemEnum::AssocConst { type_, value } => {
					let constant = TraitConstant {
						name:          trait_item.name.clone().unwrap_or_default(),
						r#type:        Box::new(self.parse_type(type_)?),
						default_value: value.as_ref().map(|d| ConstExpr::Var(d.clone())),
					};
					required_constants.push(constant);
				}
				_ => {}
			}
		}

		let attributes = if t.is_auto {
			Some(vec![TraitAttribute::Auto])
		} else if t.is_unsafe {
			Some(vec![TraitAttribute::Unsafe])
		} else {
			None
		};

		Ok(TraitDef {
			generics,
			super_traits,
			associated_types: if associated_types.is_empty() { None } else { Some(associated_types) },
			properties: None,
			required_methods: if required_methods.is_empty() { None } else { Some(required_methods) },
			provided_methods: if provided_methods.is_empty() { None } else { Some(provided_methods) },
			required_constants: if required_constants.is_empty() {
				None
			} else {
				Some(required_constants)
			},
			attributes,
			members: None,
		})
	}

	fn parse_trait_method(&self, id: &Id, f: &rustdoc_types::Function) -> Result<TraitMethod> {
		let item = self.krate.index.get(id).ok_or(ParseError::ItemNotFound(id.0))?;

		let (receiver, parameters) = self.parse_function_inputs(&f.sig.inputs)?;

		let return_type =
			f.sig.output.as_ref().map(|ty| self.parse_type(ty).map(Box::new)).transpose()?;

		let generics =
			if f.generics.params.is_empty() { None } else { self.parse_generic_params(&f.generics) };

		let attributes = self.parse_function_attributes(f);

		Ok(TraitMethod {
			name: item.name.clone().unwrap_or_default(),
			parameters,
			return_type,
			generics,
			attributes,
			documentation: item.docs.clone(),
			receiver,
			has_default_implementation: f.has_body,
		})
	}

	fn determine_receiver(inputs: &[(String, rustdoc_types::Type)]) -> Option<ReceiverKind> {
		if let Some((name, ty)) = inputs.first() {
			if name != "self" {
				return Some(ReceiverKind::Static);
			}
			match ty {
				rustdoc_types::Type::BorrowedRef { is_mutable, .. } => {
					if *is_mutable {
						Some(ReceiverKind::MutRef)
					} else {
						Some(ReceiverKind::SharedRef)
					}
				}
				rustdoc_types::Type::Generic(name) if name == "Self" => Some(ReceiverKind::Owned),
				rustdoc_types::Type::ResolvedPath(path) if path.path == "Self" => Some(ReceiverKind::Owned),
				_ => Some(ReceiverKind::Arbitrary),
			}
		} else {
			Some(ReceiverKind::Static)
		}
	}

	fn parse_impl(
		&self,
		_state: &mut ParseState,
		_id: &Id,
		i: &rustdoc_types::Impl,
	) -> Result<TraitImpl> {
		let tr =
			i.trait_.as_ref().map(|path| self.parse_path_to_trait_ref(path)).transpose()?.ok_or_else(
				|| ParseError::ImplBlockParsing {
					reason: "Inherent impl blocks not supported as TraitImpl".to_string(),
				},
			)?;

		let for_type = Box::new(self.parse_type(&i.for_)?);

		let generics =
			if i.generics.params.is_empty() { None } else { self.parse_generic_params(&i.generics) };

		let where_constraints = if i.generics.where_predicates.is_empty() {
			None
		} else {
			Some(self.parse_where_predicates(&i.generics.where_predicates)?)
		};

		let mut methods = Vec::new();
		let mut associated_types = Vec::new();
		let mut associated_constants = Vec::new();

		for item_id in &i.items {
			let impl_item = self.krate.index.get(item_id).ok_or(ParseError::ItemNotFound(item_id.0))?;

			match &impl_item.inner {
				ItemEnum::Function(f) => {
					let function = self.parse_function(item_id, f)?;
					methods.push(function);
				}
				ItemEnum::AssocType { type_: Some(ty), .. } => {
					let assoc_type_impl = AssociatedTypeImpl {
						name:   impl_item.name.clone().unwrap_or_default(),
						r#type: Box::new(self.parse_type(ty)?),
					};
					associated_types.push(assoc_type_impl);
				}
				ItemEnum::AssocType { type_: None, .. } => {}
				ItemEnum::AssocConst { type_, value } => {
					let constant = TraitConstant {
						name:          impl_item.name.clone().unwrap_or_default(),
						r#type:        Box::new(self.parse_type(type_)?),
						default_value: value.as_ref().map(|d| ConstExpr::Var(d.clone())),
					};
					associated_constants.push(constant);
				}
				_ => {}
			}
		}

		Ok(TraitImpl {
			tr,
			for_type,
			generics,
			where_constraints,
			methods: if methods.is_empty() { None } else { Some(methods) },
			associated_types: if associated_types.is_empty() { None } else { Some(associated_types) },
			associated_constants: if associated_constants.is_empty() {
				None
			} else {
				Some(associated_constants)
			},
			is_negative: i.is_negative,
			is_blanket: i.blanket_impl.is_some(),
			is_unsafe: i.is_unsafe,
			members: None,
		})
	}

	fn parse_union_fields(&self, u: &rustdoc_types::Union) -> Result<Vec<Type>> {
		u.fields
			.iter()
			.map(|id| {
				let item = self.krate.index.get(id).ok_or(ParseError::ItemNotFound(id.0))?;
				if let ItemEnum::StructField(ty) = &item.inner {
					self.parse_type(ty)
				} else {
					Err(ParseError::InvalidItemKind {
						id:       id.0.to_string(),
						expected: "StructField".to_string(),
						actual:   format!("{:?}", item.inner),
					})
				}
			})
			.collect()
	}

	fn resolve_path_to_id(&self, path: &str) -> Option<&Id> {
		if let Some(id) = self.path_to_id.get(path) {
			return Some(id);
		}

		for (key, id) in &self.path_to_id {
			if key.ends_with(&format!("::{}", path)) || key == path {
				return Some(id);
			}
		}

		None
	}

	fn resolve_type_to_entry_id(&self, ty: &Type) -> Option<i64> {
		match ty {
			Type::TypeReference(tr) => {
				self.resolve_path_to_id(&tr.identifier).map(|id| self.id_to_number(id))
			}
			Type::Primitive(prim) => {
				let name = match prim {
					Primitive::Int(Width::W8) => "i8",
					Primitive::Int(Width::W16) => "i16",
					Primitive::Int(Width::Arch) => "isize",
					Primitive::Int(Width::W64) => "i64",
					Primitive::Int(Width::W128) => "i128",
					Primitive::UInt(Width::W8) => "u8",
					Primitive::UInt(Width::W16) => "u16",
					Primitive::UInt(Width::Arch) => "usize",
					Primitive::UInt(Width::W64) => "u64",
					Primitive::UInt(Width::W128) => "u128",
					Primitive::Float(Width::W16) => "f16",
					Primitive::Float(Width::W32) => "f32",
					Primitive::Float(Width::W64) => "f64",
					Primitive::Bool => "bool",
					Primitive::String => "str",
					Primitive::Char => "char",
					_ => return None,
				};
				self.primitive_map.get(name).map(|id| self.id_to_number(id))
			}
			_ => None,
		}
	}

	fn id_to_number(&self, id: &Id) -> i64 {
		use std::{collections::hash_map::DefaultHasher, hash::{Hash, Hasher}};

		let mut hasher = DefaultHasher::new();
		id.0.hash(&mut hasher);
		hasher.finish() as i64
	}

	fn parse_visibility(&self, vis: &rustdoc_types::Visibility) -> Visibility {
		match vis {
			rustdoc_types::Visibility::Public => Visibility::Public,
			rustdoc_types::Visibility::Default => Visibility::Private,
			rustdoc_types::Visibility::Crate => Visibility::Internal,
			rustdoc_types::Visibility::Restricted { .. } => Visibility::Package,
		}
	}

	fn parse_type(&self, ty: &rustdoc_types::Type) -> Result<Type> {
		match ty {
			rustdoc_types::Type::ResolvedPath(path) => {
				if path.path == "Self" {
					return Ok(Type::SelfType);
				}
				let ir_path = self.parse_resolved_path(path)?;
				Ok(Type::TypeReference(ir_path))
			}

			rustdoc_types::Type::DynTrait(dyn_trait) => {
				let traits = dyn_trait
					.traits
					.iter()
					.map(|pt| self.parse_poly_trait(pt))
					.collect::<Result<Vec<_>>>()?;

				Ok(Type::DynTrait(DynTrait { traits, lifetime: dyn_trait.lifetime.clone() }))
			}

			rustdoc_types::Type::Generic(name) => {
				if name == "Self" {
					Ok(Type::SelfType)
				} else {
					Ok(Type::GenericParam(ir::ty::GenericParam { name: name.clone(), kind: None }))
				}
			}

			rustdoc_types::Type::Primitive(prim) => {
				if prim == "never" || prim == "!" {
					Ok(Type::Never)
				} else {
					Ok(Type::Primitive(self.parse_primitive(prim)?))
				}
			}

			rustdoc_types::Type::FunctionPointer(fp) => {
				let function_pointer = self.parse_function_pointer(fp)?;
				Ok(Type::FunctionPointer(function_pointer))
			}

			rustdoc_types::Type::Tuple(types) => {
				let parsed_types = types.iter().map(|t| self.parse_type(t)).collect::<Result<Vec<_>>>()?;
				Ok(Type::Tuple(parsed_types))
			}

			rustdoc_types::Type::Slice(inner) => {
				let parsed_inner = Box::new(self.parse_type(inner)?);
				Ok(Type::Slice(parsed_inner))
			}

			rustdoc_types::Type::Array { type_, len } => {
				let parsed_ty = Box::new(self.parse_type(type_)?);
				let length = len.parse::<usize>().map_err(|_| ParseError::TypeResolution {
					type_name: "array".to_string(),
					reason:    format!("Invalid array length: {}", len),
				})?;
				Ok(Type::Array { r#type: parsed_ty, length })
			}

			rustdoc_types::Type::Pat { .. } => Ok(Type::Infer),

			rustdoc_types::Type::ImplTrait(bounds) => {
				let generic_bounds = self.parse_generic_bounds(bounds)?;
				Ok(Type::ImplTrait(generic_bounds))
			}

			rustdoc_types::Type::Infer => Ok(Type::Infer),

			rustdoc_types::Type::RawPointer { is_mutable, type_ } => {
				let parsed_ty = Box::new(self.parse_type(type_)?);
				Ok(Type::RawPointer { is_mutable: *is_mutable, r#type: parsed_ty })
			}

			rustdoc_types::Type::BorrowedRef { lifetime, is_mutable, type_ } => {
				let parsed_ty = Box::new(self.parse_type(type_)?);
				Ok(Type::BorrowedRef {
					lifetime:   lifetime.clone(),
					is_mutable: *is_mutable,
					r#type:     parsed_ty,
				})
			}

			rustdoc_types::Type::QualifiedPath { name, args, self_type, trait_ } => {
				let parsed_self_type = Box::new(self.parse_type(self_type)?);
				let parsed_trait =
					trait_.as_ref().map(|path| self.parse_resolved_path(path)).transpose()?;
				let generic_args = args
					.as_ref()
					.map(|ga| self.parse_generic_args(ga))
					.transpose()?
					.and_then(|v| if v.is_empty() { None } else { Some(v) });

				Ok(Type::QualifiedPath(QualifiedPath {
					name:              name.clone(),
					generic_arguments: generic_args,
					self_type:         parsed_self_type,
					tr:                parsed_trait,
				}))
			}
		}
	}

	fn parse_resolved_path(&self, path: &rustdoc_types::Path) -> Result<TypeReference> {
		let path_str = &path.path;
		let generic_args = path
			.args
			.as_ref()
			.map(|ga| self.parse_generic_args(ga))
			.transpose()?
			.and_then(|v| if v.is_empty() { None } else { Some(v) });

		Ok(TypeReference { identifier: path_str.clone(), generic_args })
	}

	fn parse_poly_trait(&self, pt: &rustdoc_types::PolyTrait) -> Result<PolyTrait> {
		let trait_ref = self.parse_path_to_trait_ref(&pt.trait_)?;
		Ok(PolyTrait {
			trait_ref,
			lifetimes: pt.generic_params.iter().map(|gp| gp.name.clone()).collect(),
		})
	}

	fn parse_path_to_trait_ref(&self, path: &rustdoc_types::Path) -> Result<TraitRef> {
		let args = path
			.args
			.as_ref()
			.map(|ga| {
				self.parse_generic_args(ga).and_then(|args| {
					args
						.into_iter()
						.map(|arg| match arg {
							GenericArg::Type(ty) => Ok(TypeExpr { name: format!("{:?}", ty), args: vec![] }),
							GenericArg::ConstExpr(ce) => Ok(TypeExpr { name: format!("{:?}", ce), args: vec![] }),
							GenericArg::Lifetime(lt) => Ok(TypeExpr { name: lt, args: vec![] }),
							GenericArg::Constraint(_) => Ok(TypeExpr { name: String::new(), args: vec![] }),
							GenericArg::Module(_) => Ok(TypeExpr { name: String::new(), args: vec![] }),
						})
						.collect()
				})
			})
			.transpose()?
			.unwrap_or_default();

		Ok(TraitRef { name: path.path.clone(), args })
	}

	fn parse_primitive(&self, prim: &str) -> Result<Primitive> {
		match prim {
			"i8" => Ok(Primitive::Int(Width::W8)),
			"i16" => Ok(Primitive::Int(Width::W16)),
			"i32" | "isize" => Ok(Primitive::Int(Width::Arch)),
			"i64" => Ok(Primitive::Int(Width::W64)),
			"i128" => Ok(Primitive::Int(Width::W128)),
			"u8" => Ok(Primitive::UInt(Width::W8)),
			"u16" => Ok(Primitive::UInt(Width::W16)),
			"u32" | "usize" => Ok(Primitive::UInt(Width::Arch)),
			"u64" => Ok(Primitive::UInt(Width::W64)),
			"u128" => Ok(Primitive::UInt(Width::W128)),
			"f16" => Ok(Primitive::Float(Width::W16)),
			"f32" => Ok(Primitive::Float(Width::W32)),
			"f64" | "f128" => Ok(Primitive::Float(Width::W64)),
			"bool" => Ok(Primitive::Bool),
			"str" => Ok(Primitive::String),
			"char" => Ok(Primitive::Char),
			_ => Err(ParseError::InvalidPrimitive(prim.to_string())),
		}
	}

	fn parse_function_pointer(&self, fp: &rustdoc_types::FunctionPointer) -> Result<FunctionPointer> {
		let (_, inputs) = self.parse_function_inputs(&fp.sig.inputs)?;

		let outputs = fp.sig.output.as_ref().map(|ty| self.parse_type(ty)).transpose()?;

		let mut attributes = Vec::new();
		if fp.header.is_const {
			attributes.push(FnAttribute::Const);
		}
		if fp.header.is_unsafe {
			attributes.push(FnAttribute::Unsafe);
		}
		if fp.header.is_async {
			attributes.push(FnAttribute::Async);
		}

		Ok(FunctionPointer {
			inputs,
			outputs: outputs.and_then(output_parameters_from_type),
			attributes: if attributes.is_empty() { None } else { Some(attributes) },
		})
	}

	fn parse_generic_params(&self, generics: &rustdoc_types::Generics) -> Option<Generics> {
		if generics.params.is_empty() && generics.where_predicates.is_empty() {
			return None;
		}

		let mut params = Vec::new();

		for param in &generics.params {
			match &param.kind {
				rustdoc_types::GenericParamDefKind::Type { bounds: _, default, is_synthetic } => {
					if !is_synthetic {
						params.push(Parameter::Type(TypeParam {
							name:         Some(param.name.clone()),
							kind:         ir::generics::Kind::Type,
							variance:     Variance::Invariant,
							default_type: default
								.as_ref()
								.and_then(|ty| self.parse_type(ty).ok())
								.map(|ty| TypeExpr { name: format!("{:?}", ty), args: vec![] }),
							params:       None,
							origin:       TypeParamOrigin::Free,
						}));
					}
				}
				rustdoc_types::GenericParamDefKind::Const { type_, default } => {
					if let Ok(ty) = self.parse_type(type_) {
						params.push(Parameter::Const(ConstParam {
							name:          param.name.clone(),
							r#type:        TypeExpr { name: format!("{:?}", ty), args: vec![] },
							default_value: default.as_ref().map(|d| ConstExpr::Var(d.clone())),
						}));
					}
				}
				rustdoc_types::GenericParamDefKind::Lifetime { outlives: _ } => {
					params.push(Parameter::Lifetime(LifetimeParam {
						name:     param.name.clone(),
						variance: Variance::Invariant,
					}));
				}
			}
		}

		let constraints = self.parse_where_predicates(&generics.where_predicates).unwrap_or_default();

		Some(Generics { params, constraints })
	}

	fn parse_where_predicates(
		&self,
		predicates: &[rustdoc_types::WherePredicate],
	) -> Result<Vec<Constraint>> {
		predicates
			.iter()
			.map(|pred| match pred {
				rustdoc_types::WherePredicate::BoundPredicate { type_, bounds, generic_params: _ } => {
					let param_name = match type_ {
						rustdoc_types::Type::Generic(name) => name.clone(),
						_ => format!("{:?}", type_),
					};

					bounds
						.iter()
						.map(|bound| self.parse_generic_bound_to_constraint(&param_name, bound))
						.collect::<Result<Vec<_>>>()
				}
				rustdoc_types::WherePredicate::EqPredicate { lhs, rhs } => {
					let lhs_str = format!("{:?}", lhs);
					Ok(vec![Constraint::AssociatedTypeBound {
						param:      lhs_str.clone(),
						assoc_name: lhs_str,
						bound:      TypeExpr { name: format!("{:?}", rhs), args: vec![] },
					}])
				}
				rustdoc_types::WherePredicate::LifetimePredicate { lifetime, outlives } => Ok(
					outlives
						.iter()
						.map(|o| Constraint::LifetimeBound { shorter: lifetime.clone(), longer: o.clone() })
						.collect(),
				),
			})
			.collect::<Result<Vec<Vec<_>>>>()
			.map(|v| v.into_iter().flatten().collect())
	}

	fn parse_generic_bound_to_constraint(
		&self,
		param: &str,
		bound: &rustdoc_types::GenericBound,
	) -> Result<Constraint> {
		match bound {
			rustdoc_types::GenericBound::TraitBound { trait_, generic_params: _, modifier: _ } => {
				let trait_ref = self.parse_path_to_trait_ref(trait_)?;
				Ok(Constraint::TraitBound { param: param.to_string(), trait_ref })
			}
			rustdoc_types::GenericBound::Use(_) => Ok(Constraint::TraitBound {
				param:     param.to_string(),
				trait_ref: TraitRef { name: "Use".to_string(), args: vec![] },
			}),
			rustdoc_types::GenericBound::Outlives(lifetime) => {
				Ok(Constraint::LifetimeBound { shorter: param.to_string(), longer: lifetime.clone() })
			}
		}
	}

	fn parse_trait_bounds(&self, bounds: &[rustdoc_types::GenericBound]) -> Result<Vec<TraitRef>> {
		bounds
			.iter()
			.filter_map(|bound| match bound {
				rustdoc_types::GenericBound::TraitBound { trait_, .. } => {
					Some(self.parse_path_to_trait_ref(trait_))
				}
				_ => None,
			})
			.collect()
	}

	fn parse_generic_bounds(
		&self,
		bounds: &[rustdoc_types::GenericBound],
	) -> Result<Vec<GenericBound>> {
		bounds
			.iter()
			.map(|bound| match bound {
				rustdoc_types::GenericBound::TraitBound { trait_, .. } => {
					let trait_ref = self.parse_path_to_trait_ref(trait_)?;
					Ok(GenericBound::Trait(trait_ref))
				}
				rustdoc_types::GenericBound::Outlives(lifetime) => {
					Ok(GenericBound::Lifetime(lifetime.clone()))
				}
				rustdoc_types::GenericBound::Use(_) => {
					Ok(GenericBound::Trait(TraitRef { name: "Use".into(), args: vec![] }))
				}
			})
			.collect()
	}

	fn map_rustdoc_term(&self, term: rustdoc_types::Term) -> Term {
		match term {
			rustdoc_types::Term::Type(typer) => {
				Term::Equality(Box::new(self.parse_type(&typer).unwrap()))
			}
			rustdoc_types::Term::Constant(_constant) => Term::Bound(vec![]),
		}
	}

	fn parse_generic_args(&self, args: &rustdoc_types::GenericArgs) -> Result<Vec<GenericArg>> {
		match args {
			rustdoc_types::GenericArgs::AngleBracketed { args, constraints } => {
				let mut result = Vec::new();

				for constraint in constraints {
					result.push(GenericArg::Constraint(Constraint::AssociatedItem {
						name: constraint.name.clone(),
						args: constraint.args.as_ref().map(|a| self.parse_generic_args(&a.clone()).unwrap()),
						term: match &constraint.binding {
							rustdoc_types::AssocItemConstraintKind::Equality(term) => {
								self.map_rustdoc_term(term.clone())
							}
							rustdoc_types::AssocItemConstraintKind::Constraint(bounds) => Term::Bound(
								bounds
									.iter()
									.flat_map(|t| {
										let parsed = self.parse_generic_bounds(std::slice::from_ref(t)).unwrap();
										parsed
											.into_iter()
											.map(|b| match b {
												GenericBound::Trait(tr) => {
													Constraint::TraitBound { param: String::new(), trait_ref: tr }
												}
												GenericBound::Lifetime(lt) => {
													Constraint::LifetimeBound { shorter: String::new(), longer: lt }
												}
											})
											.collect::<Vec<_>>()
									})
									.collect(),
							),
						},
					}));
				}

				for arg in args {
					match arg {
						rustdoc_types::GenericArg::Lifetime(lt) => {
							result.push(GenericArg::Lifetime(lt.clone()));
						}
						rustdoc_types::GenericArg::Type(ty) => {
							let parsed_ty = self.parse_type(ty)?;
							result.push(GenericArg::Type(parsed_ty));
						}
						rustdoc_types::GenericArg::Const(c) => {
							result.push(GenericArg::ConstExpr(ConstExpr::Var(c.expr.clone())));
						}
						rustdoc_types::GenericArg::Infer => {
							result.push(GenericArg::Type(Type::Infer));
						}
					}
				}

				Ok(result)
			}
			rustdoc_types::GenericArgs::Parenthesized { inputs, output } => {
				let mut result = Vec::new();

				for input in inputs {
					let parsed_ty = self.parse_type(input)?;
					result.push(GenericArg::Type(parsed_ty));
				}

				if let Some(output) = output {
					let parsed_output = self.parse_type(output)?;
					result.push(GenericArg::Type(parsed_output));
				}

				Ok(result)
			}
			rustdoc_types::GenericArgs::ReturnTypeNotation => Ok(vec![]),
		}
	}
}
