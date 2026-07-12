use std::collections::VecDeque;
use rustc_hash::FxHashMap as HashMap;
use rustc_hash::FxHashSet as HashSet;

use rustdoc_types::{Crate, Id, Item, ItemEnum};

use ir::entry::NudoxPath;
use ir::kind::Entry;

use super::error::Parse;
use super::Result;
use crate::{DocParser, VisibilityMap};

pub(super) struct ParseContext {
	pub(super) krate:          Crate,
	pub(super) id_to_paths:    HashMap<Id, HashSet<Vec<String>>>,
	pub(super) primitive_map:  HashMap<String, Id>,
	pub(super) path_to_id:     HashMap<String, Id>,
}

#[derive(Default)]
pub(super) struct ParseState {
	pub(super) visiting:     HashSet<Id>,
	pub(super) entry_cache:  HashMap<Id, Entry>,
}

pub struct RustdocParser {
	pub(super) ctx:   ParseContext,
	pub(super) state: ParseState,
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
	index: &std::collections::HashMap<Id, Item>,
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
	_index: &std::collections::HashMap<Id, Item>,
	_id_to_paths: &mut HashMap<Id, HashSet<Vec<String>>>,
	_queue: &mut VecDeque<(Id, Vec<String>)>,
	_impls: &[Id],
	_parent_path: &[String],
) {
}

impl RustdocParser {
	pub fn from_doc(input: Crate) -> Result<Self> { Self::new(input) }

	pub fn parse(&mut self) -> Result<Vec<Entry>> { <Self as DocParser>::parse(self) }

	fn new(krate: Crate) -> Result<Self> {
		let mut ctx = ParseContext {
			krate,
			id_to_paths: HashMap::default(),
			primitive_map: HashMap::default(),
			path_to_id: HashMap::default(),
		};
		ctx.scan_primitives();
		ctx.build_path_map()?;
		Ok(Self { ctx, state: ParseState::default() })
	}
}

impl DocParser for RustdocParser {
	type Doc = Crate;
	type Error = Parse;

	fn from_doc(input: Crate) -> Result<Self> { Self::new(input) }

	fn parse(&mut self) -> Result<Vec<Entry>> {
		let mut all_ids: Vec<_> = self.ctx.id_to_paths.keys().cloned().collect();

		all_ids.sort_by(|a, b| {
			let path_a = &self.ctx.id_to_paths[a];
			let path_b = &self.ctx.id_to_paths[b];
			path_a.iter().cmp(path_b)
		});

		let mut entries = Vec::new();

		for id in all_ids {
			match self.ctx.item(&mut self.state, &id) {
				Ok(entry) => entries.push(entry),
				Err(Parse::CircularDependency { .. }) => {}
				Err(_) => {}
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
		let mut visited = HashSet::default();

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
						// Glob re-exports are not yet resolved; skip and continue.
					} else {
						// Import with no resolved target and not a glob — skip.
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
				}
				_ => {
					// Unknown item variant — not traversed, path already recorded above.
				}
			}
		}
		Ok(())
	}

	pub(super) fn item(&self, state: &mut ParseState, id: &Id) -> Result<Entry> {
		if let Some(cached) = state.entry_cache.get(id) {
			return Ok(cached.clone());
		}

		if state.visiting.contains(id) {
			return Err(Parse::CircularDependency { path: self.get_path(id)?.join("::") });
		}

		state.visiting.insert(*id);

		let item = self.krate.index.get(id).ok_or(Parse::ItemNotFound(id.0))?;

		let entry = self.convert_item(state, id, item)?;

		state.visiting.remove(id);
		state.entry_cache.insert(*id, entry.clone());

		Ok(entry)
	}

	pub(super) fn get_paths(&self, id: &Id) -> Option<&HashSet<Vec<String>>> {
		self.id_to_paths.get(id)
	}

	pub(super) fn get_primary_path(&self, id: &Id) -> Result<Vec<String>> {
		self
			.get_paths(id)
			.and_then(|paths| paths.iter().min_by(|a, b| a.cmp(b)))
			.cloned()
			.ok_or_else(|| Parse::PathParsing(format!("No path found for ID: {}", id.0)))
	}

	pub(super) fn get_path(&self, id: &Id) -> Result<Vec<String>> { self.get_primary_path(id) }

	pub(super) fn nudox_path_for_rustdoc_path(&self, path: &rustdoc_types::Path) -> NudoxPath {
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

	pub(super) fn convert_item(&self, state: &mut ParseState, id: &Id, item: &Item) -> Result<Entry> {
		let name = item.name.clone().unwrap_or_default();
		let path = self.get_primary_path(id).unwrap_or_else(|_| vec![name.clone()]);
		let aliases = self
			.get_paths(id)
			.map(|paths| {
				let mut hs = paths.clone();
				hs.remove(&path);
				hs
			})
			.filter(|hs| !hs.is_empty());
		let visibility = self.visibility(&item.visibility);
		let documentation = item.docs.clone();
		let kind = self.item_kind(state, id, &item.inner)?;

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

					match self.item(state, method_id) {
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
					if let Ok(child) = self.item(state, item_id) {
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
					ItemEnum::Struct(struct_item) => {
						self.collect_implemented_protocols(&struct_item.impls)
					}
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
}
