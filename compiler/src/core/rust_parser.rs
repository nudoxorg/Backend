use std::collections::{HashMap, HashSet, VecDeque};

use rustdoc_types::{Crate, Id, Item, ItemEnum};

pub type Result<T> = std::result::Result<T, ParseError>;

use ir::{entry::{Entry, EntryRef}, function::{Attribute as FnAttribute, Function}, generics::*, kind::{Kind, Visibility}, parameter::Parameter, primitives::Primitive, protocols::*, record::*, ty::{DynTrait, FunctionPointer, Path as IRPath, PolyTrait, QualifiedPath, Type}};

use crate::{core::rust::ParseError, error::NewDocsError};

impl From<crate::core::rust::ParseError> for NewDocsError {
	fn from(err: crate::core::rust::ParseError) -> Self { NewDocsError::ParseError(err.to_string()) }
}

pub struct RustdocParser {
	krate:         Crate,
	/// Maps rustdoc IDs to resolved paths
	id_to_paths:   HashMap<Id, HashSet<Vec<String>>>, // HashSet tracks path of re-exports on same ID
	/// Tracks visited items to detect circular dependencies
	visiting:      HashSet<Id>,
	/// Cache of parsed entries
	entry_cache:   HashMap<Id, Entry>,
	/// Primitive name to ID mapping (for Genealogy resolution)
	primitive_map: HashMap<String, Id>,
	path_to_id:    HashMap<String, Id>,
}

impl RustdocParser {
	fn resolve_path_to_id(&self, path: &str) -> Option<Id> {
		// Try exact match first
		if let Some(id) = self.path_to_id.get(path) {
			return Some(id.clone());
		}

		// Try without crate prefix (e.g., "Affine" instead of "xilem::Affine")
		for (key, id) in &self.path_to_id {
			if key.ends_with(&format!("::{}", path)) || key == path {
				return Some(id.clone());
			}
		}

		None
	}

	fn resolve_type_to_entry_id(&self, ty: &Type) -> Option<i64> {
		match ty {
			Type::ResolvedPath(ir_path) => {
				self.resolve_path_to_id(&ir_path.path).map(|id| self.id_to_number(&id))
			}
			Type::Primitive(prim) => {
				// Look up in primitive_map
				let name = match prim {
					Primitive::Int8(_) => "i8",
					Primitive::Int16(_) => "i16",
					Primitive::Int(_) => "isize",
					Primitive::Int64(_) => "i64",
					Primitive::Int128(_) => "i128",
					Primitive::UInt8(_) => "u8",
					Primitive::UInt16(_) => "u16",
					Primitive::UInt(_) => "usize",
					Primitive::UInt64(_) => "u64",
					Primitive::UInt128(_) => "u128",
					Primitive::Float(_) => "f32",
					Primitive::Double(_) => "f64",
					Primitive::Bool(_) => "bool",
					Primitive::String(_) => "str",
					Primitive::Char(_) => "char",
					_ => return None,
				};
				self.primitive_map.get(name).map(|id| self.id_to_number(id))
			}
			_ => None,
		}
	}

	pub fn new(krate: Crate) -> Result<Self> {
		let mut parser = Self {
			krate,
			id_to_paths: HashMap::new(),
			visiting: HashSet::new(),
			entry_cache: HashMap::new(),
			primitive_map: HashMap::new(),
			path_to_id: HashMap::new(),
		};
		// Pre-scan primitives to handle them like the "old" code
		parser.scan_primitives();
		parser.build_path_map()?;
		Ok(parser)
	}

	fn scan_primitives(&mut self) {
		for (id, item) in &self.krate.index {
			if let ItemEnum::Primitive(p) = &item.inner {
				self.primitive_map.insert(p.name.clone(), id.clone());
				// Primitives don't always have a path from root, so we ensure they exist in the
				// map potentially, multiple paths (primary + aliases/re-exports) can exist
				// per id
				self
					.id_to_paths
					.entry(id.clone())
					.and_modify(|hash_set| {
						hash_set.insert(vec![p.name.clone()]);
					})
					.or_insert_with(|| {
						// build hashset and add initial path
						let mut hs = HashSet::default();
						hs.insert(vec![p.name.clone()]);
						hs
					});
			}
		}
	}

	fn collect_impl_members(&mut self, impl_ids: &[Id]) -> Result<Vec<EntryRef>> {
		let mut members = Vec::new();
		let index = self.krate.index.clone();

		for impl_id in impl_ids {
			let impl_item = match index.get(impl_id) {
				Some(i) => i,
				None => continue,
			};

			if let ItemEnum::Impl(imp) = &impl_item.inner {
				// You can filter here: e.g., only inherent impls (where trait_ is None)
				// or include trait methods too.
				// The "Genealogy" code usually flattens everything.

				for assoc_item_id in &imp.items {
					// Check circular dep/cache for the method
					if let Ok(entry) = self.parse_item(assoc_item_id) {
						// EntryRef as a pointer for member instead of nested duplication
						members.push(EntryRef { id: entry.id, path: entry.path });
					}
				}
			}
		}

		Ok(members)
	}

	fn build_path_map(&mut self) -> Result<()> {
		let root_id = self.krate.root.clone();

		// BFS queue: (ID, current_path)
		let mut queue = VecDeque::new();
		let mut visited = HashSet::new();

		// Initialize with root
		if let Some(root_item) = self.krate.index.get(&root_id) {
			let root_name = root_item.name.clone().unwrap_or_else(|| "crate".to_string());
			let root_path = vec![root_name];
			// Allow possibility of multiple paths in a HashSet on ID for re-exports
			self.add_path(&root_id, root_path.clone());
			queue.push_back((root_id, root_path));
		}

		while let Some((id, current_path)) = queue.pop_front() {
			if visited.contains(&id) {
				continue;
			}
			visited.insert(id.clone());
			let index = self.krate.index.clone();

			let item = match index.get(&id) {
				Some(i) => i,
				None => continue, // Skip broken links
			};

			match &item.inner {
				ItemEnum::Module(m) => {
					for child_id in &m.items {
						// For module children, append their name to the path
						if let Some(child_item) = self.krate.index.get(child_id) {
							if let Some(name) = &child_item.name {
								let mut new_path = current_path.clone();
								new_path.push(name.clone());

								// Record the path for this ID; BFS traversal uses the first-seen path.
								if !visited.contains(child_id) {
									self.add_path(child_id, new_path.clone());
									queue.push_back((child_id.clone(), new_path));
								} else {
									self.add_path(child_id, new_path);
								}
							} else {
								// I believe this is the path that items that are inlined take, usually
								// re-exports. Only continue down paths not visited
								if !visited.contains(child_id) {
									self.add_path(child_id, current_path.clone());
									queue.push_back((child_id.clone(), current_path.clone()));
								} else {
									self.add_path(child_id, current_path.clone());
								}
							}
						}
					}
				}
				ItemEnum::Use(import) => {
					// "Deep Path Resolution": Follow the import
					if let Some(target_id) = &import.id {
						// If we are importing something, we give it a path at this location.
						// e.g., "crate::foo::Bar" might point to "crate::internal::Bar"
						// We treat this as an alias path for the target ID.
						self.add_path(target_id, current_path.clone());

						// Continue BFS from the target item, using this new public path
						if !visited.contains(target_id) {
							queue.push_back((target_id.clone(), current_path));
						}
					} else if import.is_glob {
						// Handle Glob imports if necessary: read the module and bring all
						// children up. (Simplified for this snippet: requires
						// resolving the path string to an ID if not explicit)
					}
				}
				ItemEnum::Struct(s) => self.queue_impls(&mut queue, &s.impls, &current_path),
				ItemEnum::Enum(e) => self.queue_impls(&mut queue, &e.impls, &current_path),
				ItemEnum::Trait(t) => {
					// Trait items (required methods) are children
					for item_id in &t.items {
						self.queue_child(&mut queue, item_id, &current_path);
					}
				}
				_ => {}
			}
		}
		Ok(())
	}

	/// Helper for HashSet insertion on new id_to_paths
	fn add_path(&mut self, id: &Id, path: Vec<String>) {
		self
			.id_to_paths
			.entry(id.clone())
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
		&mut self,
		queue: &mut VecDeque<(Id, Vec<String>)>,
		id: &Id,
		parent_path: &[String],
	) {
		if let Some(item) = self.krate.index.get(id) {
			if let Some(name) = &item.name {
				let mut path = parent_path.to_vec();
				path.push(name.clone());
				// Record additional paths for this ID (associated items can be reachable in
				// multiple places)
				self.add_path(id, path.clone());
				queue.push_back((id.clone(), path));
			}
		}
	}

	// Helper to traverse into impls during path building to ensure associated
	// types/methods get paths
	fn queue_impls(
		&mut self,
		queue: &mut VecDeque<(Id, Vec<String>)>,
		impls: &[Id],
		parent_path: &[String],
	) {
		for impl_id in impls {
			if let Some(item) = self.krate.index.clone().get(impl_id) {
				if let ItemEnum::Impl(i) = &item.inner {
					for item_id in &i.items {
						self.queue_child(queue, item_id, parent_path);
					}
				}
			}
		}
	}

	pub fn parse_crate(&mut self) -> Result<Vec<Entry>> {
		// 1. Gather all IDs we found paths for (this includes deep items, re-exports,
		//    etc.)
		let mut all_ids: Vec<_> = self.id_to_paths.keys().cloned().collect();

		// 2. Sort them by path for deterministic output order (e.g. "axum::body::Body"
		//    comes before "axum::extract::Json")
		all_ids.sort_by(|a, b| {
			let path_a = &self.id_to_paths[a];
			let path_b = &self.id_to_paths[b];
			path_a.into_iter().cmp(path_b)
		});

		let mut entries = Vec::new();

		for id in all_ids {
			// We parse every reachable item.
			// The `entry_cache` inside `parse_item` ensures we don't duplicate work
			// if an item was already visited during a previous recursion.
			if let Ok(entry) = self.parse_item(&id) {
				// Optional: Filter out items you don't want top-level entries for.
				// For example, if you don't want standalone entries for methods (because
				// they are already inside the Struct's `members`), you could check `kind`.
				// For a raw search index, keeping them is usually fine/better.
				entries.push(entry);
			}
		}

		Ok(entries)
	}

	fn parse_item(&mut self, id: &Id) -> Result<Entry> {
		// Check cache first
		if let Some(cached) = self.entry_cache.get(id) {
			return Ok(cached.clone());
		}

		// Detect circular dependencies
		if self.visiting.contains(id) {
			return Err(ParseError::CircularDependency { path: self.get_path(id)?.join("::") });
		}

		self.visiting.insert(id.clone());

		let index = self.krate.index.clone();

		let item = index.get(id).ok_or_else(|| ParseError::ItemNotFound(id.0))?;

		let entry = self.convert_item(id, item)?;

		self.visiting.remove(id);
		self.entry_cache.insert(id.clone(), entry.clone());

		Ok(entry)
	}

	fn get_paths(&self, id: &Id) -> Option<&HashSet<Vec<String>>> { self.id_to_paths.get(id) }

	/// Resolve Primary path in HashSet, currently returns shortest
	fn get_primary_path(&self, id: &Id) -> Result<Vec<String>> {
		// TODO validate primary vs alias selection logic
		// prefer a path that matches the public modules
		self
			.get_paths(id)
			.and_then(|paths| paths.iter().min_by(|a, b| a.cmp(b)))
			.cloned()
			.ok_or_else(|| ParseError::PathParsing(format!("No path found for ID: {}", id.0)))
	}

	fn convert_item(&mut self, id: &Id, item: &Item) -> Result<Entry> {
		let name = item.name.clone().unwrap_or_default();
		let path = self.get_primary_path(id).unwrap_or_else(|_| vec![name.clone()]);
		// check aliases, find where None
		let aliases = self
			.get_paths(id)
			.map(|paths| {
				let mut hs = paths.clone();
				hs.remove(&path);
				hs
			})
			.and_then(|hs| if hs.is_empty() { None } else { Some(hs) });
		let visibility = Some(self.parse_visibility(&item.visibility));
		let documentation = item.docs.clone();
		let kind = self.parse_item_kind(id, &item.inner)?;
		let id_num = self.id_to_number(id);

		// Identify items that have impls and collect their children
		let members = match &item.inner {
			ItemEnum::Struct(s) => Some(self.collect_impl_members(&s.impls)?),
			ItemEnum::Enum(e) => Some(self.collect_impl_members(&e.impls)?),
			ItemEnum::Union(u) => Some(self.collect_impl_members(&u.impls)?),
			ItemEnum::Primitive(p) => Some(self.collect_impl_members(&p.impls)?),
			// Trait definitions already contain their required/provided methods in `parse_trait`
			// but if we want them in `members` as well:
			// change to entry ref -> might break traits
			ItemEnum::Trait(t) => {
				let mut trait_members = Vec::new();
				for method_id in &t.items {
					if let Ok(entry) = self.parse_item(method_id) {
						trait_members.push(EntryRef { id: entry.id, path: entry.path });
					}
				}
				Some(trait_members)
			}
			ItemEnum::Module(m) => {
				let mut children = Vec::new();
				for item_id in &m.items {
					// Only add if we can resolve it
					if let Ok(child) = self.parse_item(item_id) {
						children.push(EntryRef { id: child.id, path: child.path });
					}
				}
				if children.is_empty() { None } else { Some(children) }
			}
			_ => None,
		};

		Ok(Entry { name, id: id_num, path, aliases, kind, visibility, documentation, members })
	}

	fn parse_item_kind(&mut self, id: &Id, inner: &ItemEnum) -> Result<Kind> {
		match inner {
			ItemEnum::Module(_) => Ok(Kind::Module),

			ItemEnum::Struct(s) => {
				let record = self.parse_struct(id, s)?;
				Ok(Kind::RecordType(record))
			}

			ItemEnum::Enum(e) => {
				let variants = self.parse_enum_variants(e)?;
				Ok(Kind::SumType(variants))
			}

			ItemEnum::Function(f) => {
				let function = self.parse_function(id, f)?;
				Ok(Kind::Function(function))
			}

			ItemEnum::Trait(t) => {
				let trait_def = self.parse_trait(id, t)?;
				Ok(Kind::TraitDef(trait_def))
			}

			ItemEnum::Impl(i) => {
				let trait_impl = self.parse_impl(id, i)?;
				Ok(Kind::TraitImpl(trait_impl))
			}

			// TODO: Process generics here if applicable, make sure display impl is ther
			ItemEnum::TypeAlias(alias) => Ok(Kind::TypeAlias(self.parse_type(&alias.type_)?)),

			ItemEnum::Constant { .. } => Ok(Kind::Constant),

			ItemEnum::Static(_) => Ok(Kind::Variable),

			ItemEnum::Macro(_) => Ok(Kind::Macro),

			ItemEnum::ProcMacro(_) => Ok(Kind::Macro),

			// ItemEnum::Primitive(_) => Ok(Kind::Primitive),

			// These are useful only when linked to their parent struct, so ignore the internal type,
			// which is dealt with during the struct field resolution
			ItemEnum::StructField(ty) => Ok(Kind::Field),

			ItemEnum::Union(u) => {
				let types = self.parse_union_fields(u)?;
				Ok(Kind::UnionType(types))
			}

			_ => Err(ParseError::UnsupportedItemType(format!("{:?}", inner))),
		}
	}

	fn parse_struct(&mut self, id: &Id, s: &rustdoc_types::Struct) -> Result<Record> {
		let index = self.krate.index.clone();

		let item = index.get(id).unwrap();
		let generics =
			s.generics.params.is_empty().then(|| self.parse_generic_params(&s.generics)).flatten();

		let generic_args = generics.map(|g| self.generics_to_args(&g)).flatten();

		let (kind, fields) = match &s.kind {
			rustdoc_types::StructKind::Unit => (RecordKind::Unit, None),
			rustdoc_types::StructKind::Tuple(field_ids) => {
				let fields = self.parse_tuple_fields(field_ids)?;
				(RecordKind::Tuple, Some(fields))
			}
			rustdoc_types::StructKind::Plain { fields: field_ids, .. } => {
				let fields = self.parse_named_fields(field_ids)?;
				(RecordKind::Named, Some(fields))
			}
		};

		let visibility = Some(self.parse_visibility(&item.visibility));

		Ok(Record { name: item.name.clone(), generics: generic_args, kind, fields, visibility })
	}

	fn parse_tuple_fields(&mut self, field_ids: &[Option<Id>]) -> Result<Vec<Field>> {
		field_ids
			.iter()
			.enumerate()
			.filter_map(|(idx, opt_id)| {
				opt_id.as_ref().map(|id| {
					let index = self.krate.index.clone();
					let item = index.get(id)?;
					if let ItemEnum::StructField(ty) = &item.inner {
						let parsed_ty = self.parse_type(ty).ok()?;
						let type_entry_id = self.resolve_type_to_entry_id(&parsed_ty); // ← NEW

						Some(Field {
							name: Some(idx.to_string()),
							ty: Some(Box::new(parsed_ty)),
							default_value: None,
							attributes: None,
							visibility: Some(self.parse_visibility(&item.visibility)),
							type_entry_id, // ← NEW
						})
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

	fn parse_named_fields(&mut self, field_ids: &[Id]) -> Result<Vec<Field>> {
		field_ids
			.iter()
			.map(|id| {
				let item = self.krate.index.get(id).ok_or_else(|| ParseError::ItemNotFound(id.0))?.clone();

				if let ItemEnum::StructField(ty) = &item.inner {
					let parsed_ty = self.parse_type(&ty)?;
					let type_entry_id = self.resolve_type_to_entry_id(&parsed_ty); // ← NEW

					Ok(Field {
						name: item.name.clone(),
						ty: Some(Box::new(parsed_ty)),
						default_value: None,
						attributes: None,
						visibility: Some(self.parse_visibility(&item.visibility)),
						type_entry_id, // ← NEW
					})
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

	fn parse_enum_variants(&mut self, e: &rustdoc_types::Enum) -> Result<Vec<SumVariant>> {
		e.variants
			.iter()
			.map(|variant_id| {
				let index = self.krate.index.clone();
				let item =
					index.get(variant_id).ok_or_else(|| ParseError::ItemNotFound(variant_id.0.clone()))?;

				let name = item.name.clone().unwrap_or_default();

				if let ItemEnum::Variant(v) = &item.inner {
					let types = match &v.kind {
						rustdoc_types::VariantKind::Plain => None,
						rustdoc_types::VariantKind::Tuple(fields) => {
							let types: Result<Vec<Type>> = fields
								.iter()
								.filter_map(|opt_id| opt_id.as_ref())
								.map(|id| {
									let field_item =
										index.get(id).ok_or_else(|| ParseError::ItemNotFound(id.0.clone()))?;
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
							Some(types?)
						}
						rustdoc_types::VariantKind::Struct { fields, .. } => {
							let types: Result<Vec<Type>> = fields
								.iter()
								.map(|id| {
									let field_item =
										index.get(id).ok_or_else(|| ParseError::ItemNotFound(id.0.clone()))?;
									if let ItemEnum::StructField(ty) = &field_item.inner {
										self.parse_type(&ty)
									} else {
										Err(ParseError::InvalidItemKind {
											id:       id.0.to_string(),
											expected: "StructField".to_string(),
											actual:   format!("{:?}", field_item.inner),
										})
									}
								})
								.collect();
							Some(types?)
						}
					};

					Ok(SumVariant { name, types })
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

	fn parse_function(&mut self, id: &Id, f: &rustdoc_types::Function) -> Result<Function> {
		let item = self.krate.index.get(id).unwrap();
		let visibility_clone = item.visibility.clone();
		let name = item.name.clone().unwrap_or_default();

		let input_parameters =
			if f.sig.inputs.is_empty() { None } else { Some(self.parse_function_inputs(&f.sig.inputs)?) };

		let output_parameters = if let Some(output_ty) = &f.sig.output {
			Some(vec![Parameter {
				name:          "return".to_string(),
				ty:            Some(self.parse_type(output_ty)?),
				attributes:    None,
				default_value: None,
				description:   None,
			}])
		} else {
			None
		};

		let attributes = self.parse_function_attributes(f);

		let generics =
			if f.generics.params.is_empty() { None } else { self.parse_generic_params(&f.generics) };

		let visibility = Some(self.parse_visibility(&visibility_clone));

		// NEW: Compute type_links for function parameters
		let type_links = {
			let mut links = HashMap::new();

			// Link input parameters to their type definitions
			if let Some(ref params) = input_parameters {
				for param in params {
					if let Some(ref ty) = param.ty {
						if let Some(entry_id) = self.resolve_type_to_entry_id(ty) {
							// Use "in." prefix to distinguish input params
							links.insert(format!("in.{}", param.name), entry_id);
						}
					}
				}
			}

			// Link output parameters to their type definitions
			if let Some(ref params) = output_parameters {
				for param in params {
					if let Some(ref ty) = param.ty {
						if let Some(entry_id) = self.resolve_type_to_entry_id(ty) {
							// Use "out." prefix for return type
							links.insert(format!("out.{}", param.name), entry_id);
						}
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
			name,
			implemented: true,
			visibility,
			type_links, // ← NEW
		})
	}

	fn parse_function_inputs(
		&mut self,
		inputs: &[(String, rustdoc_types::Type)],
	) -> Result<Vec<Parameter>> {
		inputs
			.iter()
			.map(|(name, ty)| {
				let parsed_ty = self.parse_type(ty)?;
				Ok(Parameter {
					name:          name.clone(),
					ty:            Some(parsed_ty),
					attributes:    None,
					default_value: None,
					description:   None,
				})
			})
			.collect()
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

	fn parse_trait(&mut self, id: &Id, t: &rustdoc_types::Trait) -> Result<TraitDef> {
		let item = self.krate.index.get(id).unwrap();
		let visibility_clone = item.visibility.clone();
		let name = item.name.clone().unwrap_or_default();
		let docs = item.docs.clone();

		let generics =
			if t.generics.params.is_empty() { None } else { self.parse_generic_params(&t.generics) };

		let super_traits =
			if t.bounds.is_empty() { None } else { Some(self.parse_trait_bounds(&t.bounds)?) };

		let mut required_methods = Vec::new();
		let mut provided_methods = Vec::new();
		let mut associated_types = Vec::new();
		let mut required_constants = Vec::new();

		// Clone items to avoid borrow issues
		let trait_items: Vec<Id> = t.items.clone();

		for item_id in trait_items {
			let trait_item =
				self.krate.index.get(&item_id).ok_or_else(|| ParseError::ItemNotFound(item_id.0))?.clone();

			match &trait_item.inner {
				ItemEnum::Function(f) => {
					let method = self.parse_trait_method(&item_id, f)?;
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
						docs:         trait_item.docs.clone(),
					};
					associated_types.push(assoc_type);
				}
				ItemEnum::AssocConst { type_, value } => {
					let constant = TraitConstant {
						name:          trait_item.name.clone().unwrap_or_default(),
						ty:            Box::new(self.parse_type(type_)?),
						default_value: value.as_ref().map(|d| ConstExpr { expr: d.clone() }),
						docs:          trait_item.docs.clone(),
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

		let visibility = Some(self.parse_visibility(&visibility_clone));

		Ok(TraitDef {
			name,
			generics,
			super_traits,
			associated_types: if associated_types.is_empty() { None } else { Some(associated_types) },
			required_methods: if required_methods.is_empty() { None } else { Some(required_methods) },
			provided_methods: if provided_methods.is_empty() { None } else { Some(provided_methods) },
			required_constants: if required_constants.is_empty() {
				None
			} else {
				Some(required_constants)
			},
			attributes,
			visibility,
			docs,
		})
	}

	fn parse_trait_method(&mut self, id: &Id, f: &rustdoc_types::Function) -> Result<TraitMethod> {
		let index = self.krate.index.clone();
		let item = index.get(id).unwrap();

		let parameters =
			if f.sig.inputs.is_empty() { None } else { Some(self.parse_function_inputs(&f.sig.inputs)?) };

		let return_type =
			f.sig.output.as_ref().map(|ty| self.parse_type(ty).map(Box::new)).transpose()?;

		let generics =
			if f.generics.params.is_empty() { None } else { self.parse_generic_params(&f.generics) };

		let attributes = self.parse_function_attributes(f);

		let receiver = Self::determine_receiver(&f.sig.inputs);

		Ok(TraitMethod {
			name: item.name.clone().unwrap_or_default(),
			parameters,
			return_type,
			generics,
			attributes,
			receiver,
			has_default_implementation: f.has_body,
			docs: item.docs.clone(),
		})
	}

	fn determine_receiver(inputs: &[(String, rustdoc_types::Type)]) -> Option<ReceiverKind> {
		if let Some((name, ty)) = inputs.first() {
			if name == "self" {
				return Some(ReceiverKind::Owned);
			}
			match ty {
				rustdoc_types::Type::BorrowedRef { is_mutable, .. } => {
					if *is_mutable {
						Some(ReceiverKind::MutRef)
					} else {
						Some(ReceiverKind::SharedRef)
					}
				}
				_ => Some(ReceiverKind::Static),
			}
		} else {
			Some(ReceiverKind::Static)
		}
	}

	fn parse_impl(&mut self, id: &Id, i: &rustdoc_types::Impl) -> Result<TraitImpl> {
		let index = self.krate.index.clone();
		let item = index.get(id).unwrap();

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
			let index = self.krate.index.clone();

			let impl_item =
				index.get(item_id).ok_or_else(|| ParseError::ItemNotFound(item_id.0.clone()))?;

			match &impl_item.inner {
				ItemEnum::Function(f) => {
					let function = self.parse_function(item_id, f)?;
					methods.push(function);
				}
				ItemEnum::AssocType { type_, .. } => {
					if let Some(ty) = type_ {
						let assoc_type_impl = AssociatedTypeImpl {
							name: impl_item.name.clone().unwrap_or_default(),
							ty:   Box::new(self.parse_type(ty)?),
						};
						associated_types.push(assoc_type_impl);
					}
				}
				ItemEnum::AssocConst { type_, value } => {
					let constant = TraitConstant {
						name:          impl_item.name.clone().unwrap_or_default(),
						ty:            Box::new(self.parse_type(type_)?),
						default_value: value.as_ref().map(|d| ConstExpr { expr: d.clone() }),
						docs:          impl_item.docs.clone(),
					};
					associated_constants.push(constant);
				}
				_ => {}
			}
		}

		let visibility = Some(self.parse_visibility(&item.visibility));

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
			visibility,
			docs: item.docs.clone(),
		})
	}

	fn parse_union_fields(&mut self, u: &rustdoc_types::Union) -> Result<Vec<Type>> {
		u.fields
			.iter()
			.map(|id| {
				let index = self.krate.index.clone();

				let item = index.get(id).ok_or_else(|| ParseError::ItemNotFound(id.0.clone()))?;
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

	fn parse_type(&mut self, ty: &rustdoc_types::Type) -> Result<Type> {
		match ty {
			rustdoc_types::Type::ResolvedPath(path) => {
				let ir_path = self.parse_resolved_path(path)?;
				Ok(Type::ResolvedPath(ir_path))
			}

			rustdoc_types::Type::DynTrait(dyn_trait) => {
				let traits = dyn_trait
					.traits
					.iter()
					.map(|pt| self.parse_poly_trait(pt))
					.collect::<Result<Vec<_>>>()?;

				Ok(Type::DynTrait(DynTrait { traits, lifetime: dyn_trait.lifetime.clone() }))
			}

			rustdoc_types::Type::Generic(name) => Ok(Type::GenericParam(name.clone())),

			rustdoc_types::Type::Primitive(prim) => Ok(Type::Primitive(self.parse_primitive(prim)?)),

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
				Ok(Type::Array { ty: parsed_ty, length })
			}

			rustdoc_types::Type::Pat { type_, .. } => {
				let parsed_ty = Box::new(self.parse_type(type_)?);
				Ok(Type::Pattern { ty: parsed_ty })
			}

			rustdoc_types::Type::ImplTrait(bounds) => {
				let generic_bounds = self.parse_generic_bounds(bounds)?;
				Ok(Type::ImplTrait(generic_bounds))
			}

			rustdoc_types::Type::Infer => Ok(Type::Infer),

			rustdoc_types::Type::RawPointer { is_mutable, type_ } => {
				let parsed_ty = Box::new(self.parse_type(type_)?);
				Ok(Type::RawPointer { is_mutable: *is_mutable, ty: parsed_ty })
			}

			rustdoc_types::Type::BorrowedRef { lifetime, is_mutable, type_ } => {
				let parsed_ty = Box::new(self.parse_type(type_)?);
				Ok(Type::BorrowedRef {
					lifetime:   lifetime.clone(),
					is_mutable: *is_mutable,
					ty:         parsed_ty,
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

	fn parse_resolved_path(&mut self, path: &rustdoc_types::Path) -> Result<IRPath> {
		let path_str = path.path.clone();
		let generic_args = path
			.args
			.as_ref()
			.map(|ga| self.parse_generic_args(ga))
			.transpose()?
			.and_then(|v| if v.is_empty() { None } else { Some(v) });

		Ok(IRPath { path: path_str, generic_args })
	}

	fn parse_poly_trait(&mut self, pt: &rustdoc_types::PolyTrait) -> Result<PolyTrait> {
		let trait_ref = self.parse_path_to_trait_ref(&pt.trait_)?;
		Ok(PolyTrait {
			tr:        trait_ref,
			lifetimes: pt.generic_params.iter().map(|gp| gp.name.clone()).collect(),
		})
	}

	fn parse_path_to_trait_ref(&mut self, path: &rustdoc_types::Path) -> Result<TraitRef> {
		let args = path
			.args
			.as_ref()
			.map(|ga| {
				self.parse_generic_args(ga).and_then(|args| {
					args
						.into_iter()
						.map(|arg| match arg {
							GenericArg::Type(ty) => Ok(TypeExpr { name: format!("{:?}", ty), args: vec![] }),
							GenericArg::ConstExpr(ce) => Ok(TypeExpr { name: ce.expr, args: vec![] }),
							GenericArg::Lifetime(lt) => Ok(TypeExpr { name: lt, args: vec![] }),
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
			"i8" => Ok(Primitive::Int8(None)),
			"i16" => Ok(Primitive::Int16(None)),
			"isize" => Ok(Primitive::Int(None)),
			"i64" => Ok(Primitive::Int64(None)),
			"i128" => Ok(Primitive::Int128(None)),
			"u8" => Ok(Primitive::UInt8(None)),
			"u16" => Ok(Primitive::UInt16(None)),
			"usize" => Ok(Primitive::UInt(None)),
			"u64" => Ok(Primitive::UInt64(None)),
			"u128" => Ok(Primitive::UInt128(None)),
			"f32" => Ok(Primitive::Float(None)),
			"f64" => Ok(Primitive::Double(None)),
			"bool" => Ok(Primitive::Bool(None)),
			"str" => Ok(Primitive::String(None)),
			"char" => Ok(Primitive::Char(None)),
			_ => Err(ParseError::InvalidPrimitive(prim.to_string())),
		}
	}

	fn parse_function_pointer(
		&mut self,
		fp: &rustdoc_types::FunctionPointer,
	) -> Result<FunctionPointer> {
		let inputs = if fp.sig.inputs.is_empty() {
			None
		} else {
			Some(self.parse_function_inputs(&fp.sig.inputs)?)
		};

		let outputs = fp
			.sig
			.output
			.as_ref()
			.map(|ty| {
				self.parse_type(ty).map(|t| {
					vec![Parameter {
						name:          "return".to_string(),
						ty:            Some(t),
						attributes:    None,
						default_value: None,
						description:   None,
					}]
				})
			})
			.transpose()?;

		let generic_params = if fp.generic_params.is_empty() {
			None
		} else {
			Some(
				fp.generic_params
					.iter()
					.map(|gp| TypeParam {
						name:         gp.name.clone(),
						kind:         TypeKind::Type,
						variance:     Variance::Invariant,
						default_type: None,
					})
					.collect(),
			)
		};

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
			outputs,
			generic_params,
			attributes: if attributes.is_empty() { None } else { Some(attributes) },
		})
	}

	fn parse_generic_params(&mut self, generics: &rustdoc_types::Generics) -> Option<Generics> {
		if generics.params.is_empty() && generics.where_predicates.is_empty() {
			return None;
		}

		let mut type_params = Vec::new();
		let mut const_params = Vec::new();
		let mut lifetime_params = Vec::new();

		for param in &generics.params {
			match &param.kind {
				rustdoc_types::GenericParamDefKind::Type { bounds, default, is_synthetic } => {
					if !is_synthetic {
						type_params.push(TypeParam {
							name:         param.name.clone(),
							kind:         TypeKind::Type,
							variance:     Variance::Invariant,
							default_type: default
								.as_ref()
								.and_then(|ty| self.parse_type(ty).ok())
								.map(|ty| TypeExpr { name: format!("{:?}", ty), args: vec![] }),
						});
					}
				}
				rustdoc_types::GenericParamDefKind::Const { type_, default } => {
					if let Ok(ty) = self.parse_type(type_) {
						const_params.push(ConstParam {
							name:          param.name.clone(),
							ty:            TypeExpr { name: format!("{:?}", ty), args: vec![] },
							default_value: default.as_ref().map(|d| ConstExpr { expr: d.clone() }),
						});
					}
				}
				rustdoc_types::GenericParamDefKind::Lifetime { outlives } => {
					lifetime_params
						.push(LifetimeParam { name: param.name.clone(), variance: Variance::Invariant });
				}
			}
		}

		let constraints = self.parse_where_predicates(&generics.where_predicates).unwrap_or_default();

		Some(Generics { type_params, const_params, lifetime_params, constraints })
	}

	fn parse_where_predicates(
		&mut self,
		predicates: &[rustdoc_types::WherePredicate],
	) -> Result<Vec<Constraint>> {
		predicates
			.iter()
			.map(|pred| match pred {
				rustdoc_types::WherePredicate::BoundPredicate { type_, bounds, generic_params } => {
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
				rustdoc_types::WherePredicate::LifetimePredicate { lifetime, outlives } => todo!(),
			})
			.collect::<Result<Vec<Vec<_>>>>()
			.map(|v| v.into_iter().flatten().collect())
	}

	fn parse_generic_bound_to_constraint(
		&mut self,
		param: &str,
		bound: &rustdoc_types::GenericBound,
	) -> Result<Constraint> {
		match bound {
			rustdoc_types::GenericBound::TraitBound { trait_, generic_params, modifier } => {
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

	fn parse_trait_bounds(
		&mut self,
		bounds: &[rustdoc_types::GenericBound],
	) -> Result<Vec<TraitRef>> {
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
		&mut self,
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
				rustdoc_types::GenericBound::Use(precise_capturing_args) => todo!(),
			})
			.collect()
	}

	fn parse_generic_args(&mut self, args: &rustdoc_types::GenericArgs) -> Result<Vec<GenericArg>> {
		match args {
			rustdoc_types::GenericArgs::AngleBracketed { args, constraints } => {
				let mut result = Vec::new();

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
							result.push(GenericArg::ConstExpr(ConstExpr { expr: c.expr.clone() }));
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

	fn parse_visibility(&self, vis: &rustdoc_types::Visibility) -> Visibility {
		match vis {
			rustdoc_types::Visibility::Public => Visibility::Public,
			rustdoc_types::Visibility::Default => Visibility::Private,
			rustdoc_types::Visibility::Crate => Visibility::Internal,
			rustdoc_types::Visibility::Restricted { .. } => Visibility::Package,
		}
	}

	fn get_path(&self, id: &Id) -> Result<Vec<String>> {
		// Backwards-compatible shim for any call sites still expecting a single path.
		self.get_primary_path(id)
	}

	fn id_to_number(&self, id: &Id) -> i64 {
		// Simple hash of the ID string to generate a consistent number
		use std::{collections::hash_map::DefaultHasher, hash::{Hash, Hasher}};

		let mut hasher = DefaultHasher::new();
		id.0.hash(&mut hasher);
		hasher.finish() as i64
	}

	fn generics_to_args(&self, generics: &Generics) -> Option<Vec<GenericArg>> {
		let mut args = Vec::new();

		for type_param in &generics.type_params {
			if let Some(default) = &type_param.default_type {
				args.push(GenericArg::Type(Type::GenericParam(default.name.clone())));
			} else {
				args.push(GenericArg::Type(Type::GenericParam(type_param.name.clone())));
			}
		}

		for const_param in &generics.const_params {
			args.push(GenericArg::ConstExpr(ConstExpr { expr: const_param.name.clone() }));
		}

		for lifetime_param in &generics.lifetime_params {
			args.push(GenericArg::Lifetime(lifetime_param.name.clone()));
		}

		if args.is_empty() { None } else { Some(args) }
	}
}
