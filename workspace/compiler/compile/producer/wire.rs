//! Shared module-membership wiring over language path conventions.

use ir::entry::{Index, NudoxPath};
use ir::kind::Entry;
use ir::module::Module;
use rustc_hash::FxHashMap as HashMap;

/// How a language spells parent/child relationships on [`NudoxPath`].
pub trait PathParent {
	/// Parent path for membership, if any (skip roots / externals).
	fn parent_of(&self, path: &NudoxPath) -> Option<NudoxPath>;
}

/// Filesystem-style parents (`lib/attrsets/mapAttrs` → `lib/attrsets`).
///
/// Used by Nix attrpaths and any Local path that nests with `/`.
#[derive(Debug, Default, Clone, Copy)]
pub struct FsPathParent;

impl PathParent for FsPathParent {
	fn parent_of(&self, path: &NudoxPath) -> Option<NudoxPath> {
		match path {
			NudoxPath::Local(p) => {
				// `Path::parent` of a single-segment path is `Some("")` — the
				// flake/package root module. Do not treat empty as "no parent".
				let parent = p.parent()?;
				Some(NudoxPath::Local(parent.to_path_buf()))
			}
			NudoxPath::External { .. } => None,
		}
	}
}

/// Build parent → children from a path-parent convention.
pub fn members_by_parent<P: PathParent>(
	paths: impl Iterator<Item = NudoxPath>,
	split: &P,
	known: &HashMap<NudoxPath, Entry>,
) -> HashMap<NudoxPath, Vec<NudoxPath>> {
	let mut members: HashMap<NudoxPath, Vec<NudoxPath>> = HashMap::default();
	for child in paths {
		if let Some(parent) = split.parent_of(&child) {
			if known.contains_key(&parent) {
				members.entry(parent).or_default().push(child);
			}
		}
	}
	members
}

/// Assign `members` onto each `Entry::Module` in `by_path`.
pub fn apply_members(
	by_path: &mut HashMap<NudoxPath, Entry>,
	members: HashMap<NudoxPath, Vec<NudoxPath>>,
) {
	for (parent, mut children) in members {
		children.sort_by(|a, b| path_str(a).cmp(&path_str(b)));
		if let Some(Entry::Module(sym)) = by_path.get_mut(&parent) {
			sym.inner = Module {
				members: Some(children),
			};
		}
	}
}

/// One-shot: wire module members using a path-parent convention.
pub fn wire_members<P: PathParent>(by_path: &mut HashMap<NudoxPath, Entry>, split: &P) {
	let paths: Vec<NudoxPath> = by_path.keys().cloned().collect();
	let members = members_by_parent(paths.into_iter(), split, by_path);
	apply_members(by_path, members);
}

/// Wire members on an [`Index`] (same as [`wire_members`] on `entries_by_path`).
pub fn wire_index_members<P: PathParent>(index: &mut Index, split: &P) {
	wire_members(&mut index.entries_by_path, split);
}

/// Apply a precomputed module-key → children map (Python / Go style).
///
/// Accepts any iterator so callers may use `std` or `Fx` hash maps.
pub fn apply_members_to_index(
	index: &mut Index,
	members: impl IntoIterator<Item = (NudoxPath, Vec<NudoxPath>)>,
) {
	for (module_key, mut children) in members {
		children.sort_by(|a, b| path_str(a).cmp(&path_str(b)));
		if let Some(Entry::Module(symbol)) = index.entries_by_path.get_mut(&module_key) {
			symbol.inner.members = Some(children);
		}
	}
}

fn path_str(path: &NudoxPath) -> String {
	match path {
		NudoxPath::Local(p) => p.display().to_string(),
		NudoxPath::External { path, dependency } => {
			format!("{dependency}:{}", path.display())
		}
	}
}
