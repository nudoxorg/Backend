//! The rustdoc parse context: the index of ids → paths, primitive map, and the
//! cycle-guarded memoization state used while lowering items.

use std::collections::VecDeque;

use ir::{entry::NudoxPath, kind::Entry};
use rustc_hash::{FxHashMap as HashMap, FxHashSet as HashSet};
use rustdoc_types::{Crate, Id, Item, ItemEnum};

use super::{Result, error::{ItemError, Parse}};

pub(crate) struct ParseContext {
	pub(crate) krate:         Crate,
	pub(crate) id_to_paths:   HashMap<Id, HashSet<Vec<String>>>,
	pub(crate) primitive_map: HashMap<String, Id>,
	pub(crate) path_to_id:    HashMap<String, Id>,
}

#[derive(Default)]
pub(crate) struct ParseState {
	pub(crate) visiting:    HashSet<Id>,
	pub(crate) entry_cache: HashMap<Id, Entry>,
}

pub struct RustdocParser {
	pub(crate) ctx:   ParseContext,
	pub(crate) state: ParseState,
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

impl RustdocParser {
	pub fn from_doc(input: Crate) -> Result<Self> { Self::new(input) }

	fn new(krate: Crate) -> Result<Self> {
		let mut ctx = ParseContext {
			krate,
			id_to_paths: HashMap::default(),
			primitive_map: HashMap::default(),
			path_to_id: HashMap::default(),
		};
		ctx.scan_primitives();
		ctx.build_path_map()?;
		// The reverse map backs type → entry-id resolution (`type_links`).
		for (id, paths) in &ctx.id_to_paths {
			for path in paths {
				ctx.path_to_id.insert(path.join("::"), *id);
			}
		}
		Ok(Self { ctx, state: ParseState::default() })
	}

	pub fn parse(&mut self) -> Result<Vec<Entry>> {
		let mut all_ids: Vec<_> = self.ctx.id_to_paths.keys().cloned().collect();

		all_ids.sort_by(|a, b| {
			let path_a = &self.ctx.id_to_paths[a];
			let path_b = &self.ctx.id_to_paths[b];
			path_a.iter().cmp(path_b)
		});

		let mut entries = Vec::new();

		// Lenient by design: an entry that fails to lower is dropped, not
		// fatal — the surface should degrade, never disappear.
		for id in all_ids {
			if let Ok(entry) = self.ctx.item(&mut self.state, &id) {
				entries.push(entry);
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
						let Some(child_item) = self.krate.index.get(child_id) else {
							continue;
						};
						// Unnamed children (impls, re-export groups) inherit
						// the parent path.
						let child_path = match &child_item.name {
							Some(name) => {
								let mut new_path = current_path.clone();
								new_path.push(name.clone());
								new_path
							}
							None => current_path.clone(),
						};
						add_path(&mut self.id_to_paths, child_id, child_path.clone());
						if !visited.contains(child_id) {
							queue.push_back((*child_id, child_path));
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
					}
					// Glob re-exports (and unresolved imports) are not yet
					// resolved; skip and continue.
				}
				ItemEnum::Trait(t) => {
					for item_id in &t.items {
						if let Some(trait_item) = self.krate.index.get(item_id)
							&& matches!(trait_item.inner, ItemEnum::Function(_))
							&& let Some(name) = &trait_item.name
						{
							let mut path = current_path.clone();
							path.push(name.clone());
							add_path(&mut self.id_to_paths, item_id, path.clone());
							queue.push_back((*item_id, path));
						}
					}
				}
				// Struct/enum impl contents are reached through their record
				// entries (`collect_record_methods`), not path-mapped here.
				_ => {}
			}
		}
		Ok(())
	}

	pub(crate) fn item(&self, state: &mut ParseState, id: &Id) -> Result<Entry> {
		if let Some(cached) = state.entry_cache.get(id) {
			return Ok(cached.clone());
		}

		if state.visiting.contains(id) {
			return Err(Parse::Item(ItemError::CircularDependency { path: self.get_path(id)?.join("::") }));
		}

		state.visiting.insert(*id);

		let item = self.krate.index.get(id).ok_or(Parse::Item(ItemError::NotFound(id.0)));
		let entry = item.and_then(|item| self.convert_item(state, id, item));

		// Unwind the visiting marker on *both* paths — leaving it behind on
		// error would poison later lookups into spurious cycle reports.
		state.visiting.remove(id);

		let entry = entry?;
		state.entry_cache.insert(*id, entry.clone());

		Ok(entry)
	}

	pub(crate) fn get_paths(&self, id: &Id) -> Option<&HashSet<Vec<String>>> {
		self.id_to_paths.get(id)
	}

	pub(crate) fn get_primary_path(&self, id: &Id) -> Result<Vec<String>> {
		self
			.get_paths(id)
			.and_then(|paths| paths.iter().min_by(|a, b| a.cmp(b)))
			.cloned()
			.ok_or_else(|| Parse::Item(ItemError::NoPrimaryPath(id.0)))
	}

	pub(crate) fn get_path(&self, id: &Id) -> Result<Vec<String>> { self.get_primary_path(id) }

	pub(crate) fn nudox_path_for_rustdoc_path(&self, path: &rustdoc_types::Path) -> NudoxPath {
		// Local items we already path-mapped during the module walk.
		if let Ok(local_path) = self.get_primary_path(&path.id) {
			return NudoxPath::Local(std::path::PathBuf::from(local_path.join("::")));
		}

		// Prefer rustdoc's `paths` table: it carries the defining crate id and
		// the full path segments. The display `path.path` string is only the
		// *use-site* spelling (often a bare name like `Marker` or `Send`),
		// which would otherwise mint `External { dependency: "Marker", .. }`.
		if let Some(summary) = self.krate.paths.get(&path.id) {
			if summary.crate_id == 0 {
				// Same crate, but not in our walk (e.g. a private item we
				// never queued). Still a local coordinate.
				return NudoxPath::Local(std::path::PathBuf::from(summary.path.join("::")));
			}
			let dependency = self
				.krate
				.external_crates
				.get(&summary.crate_id)
				.map(|c| c.name.clone())
				.or_else(|| summary.path.first().cloned())
				.unwrap_or_default();
			// Drop a leading segment that restates the crate name so the
			// relative path is crate-relative (`helper::Marker` → `Marker`).
			let relative = match summary.path.as_slice() {
				[head, rest @ ..] if *head == dependency => rest.join("::"),
				other => other.join("::"),
			};
			return NudoxPath::External {
				dependency,
				path: std::path::PathBuf::from(relative),
			};
		}

		// Last resort: parse the use-site path string.
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

	pub(crate) fn convert_item(&self, state: &mut ParseState, id: &Id, item: &Item) -> Result<Entry> {
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
						// A member that fails to lower degrades the trait's
						// member list, not the whole trait — the same lenient
						// policy the top-level parse loop applies.
						Err(e) => {
							tracing::warn!(member = method_id.0, error = %e, "skipping unlowerable trait member");
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
}
