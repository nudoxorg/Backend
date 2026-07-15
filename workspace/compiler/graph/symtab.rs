//! The package symbol table: fq-name → path resolution over one [`Index`].
//!
//! This is the one place name-resolution semantics live. The graph
//! [`Linker`](super::link::Linker) consumes it and encodes results to symbol
//! IRIs; the occurrence resolver (`generate::resolve`) consumes the same table
//! and stays in [`NudoxPath`]. Building it once means emit-time and
//! generate-time resolution can never drift (REFERENCES-PLAN §4.2).

use std::collections::hash_map::Entry;

use ir::entry::{Index, NudoxPath};
use rustc_hash::{FxHashMap as HashMap, FxHashSet as HashSet};

/// The path components of a [`NudoxPath`], as owned strings.
///
/// Mirrors the graph linker's coordinate derivation: for both `Local` and
/// `External` paths the fq spelling is the `PathBuf` components joined with
/// `::`; the owning package lives elsewhere in the `NudoxPath` and does not
/// enter the fq key.
///
/// Many producers mint single-component paths that already embed `::`
/// separators (`"crate::Type::method"`, `"com.example::Foo.bar"`,
/// `"pkg.mod::name"`). Those are expanded so suffix resolution and
/// module-member tables see real leaf segments. A component that mixes
/// language module dots with a trailing `::name` is split as
/// `a.b::c` → `["a", "b", "c"]` so treesitter module paths align.
pub fn path_segments(path: &NudoxPath) -> Vec<String> {
	let pb = match path {
		NudoxPath::Local(pb) => pb,
		NudoxPath::External { path, .. } => path,
	};
	let mut out = Vec::new();
	for c in pb.iter() {
		let s = c.to_string_lossy();
		if s.contains("::") {
			for part in s.split("::") {
				if part.is_empty() {
					continue;
				}
				// Module segments often use `.` (Python/Java/C#/Go import paths).
				if part.contains('.') && !part.contains('/') {
					out.extend(part.split('.').filter(|p| !p.is_empty()).map(str::to_owned));
				} else {
					out.push(part.to_owned());
				}
			}
		} else {
			out.push(s.into_owned());
		}
	}
	out
}

/// Name-resolution index over one package's [`Index`].
///
/// - `exact`: every fq spelling (canonical *and* alias) → the entry's path.
/// - `suffix`: last segment → the entry's path, iff unambiguous across the
///   index (`None` marks a collision).
/// - `by_module`: module path → the leaf names declared directly under it, for
///   glob-import and sibling-scope resolution.
#[derive(Debug, Clone, Default)]
pub struct SymbolTable {
	exact: HashMap<String, NudoxPath>,
	suffix: HashMap<String, Option<NudoxPath>>,
	by_module: HashMap<Vec<String>, HashSet<String>>,
}

impl SymbolTable {
	/// Build the table from a package index, recording every canonical path and
	/// alias spelling.
	pub fn build(index: &Index) -> Self {
		let mut table = SymbolTable::default();
		for (path, entry) in &index.entries_by_path {
			table.insert_spelling(&path_segments(path), path);
			if let Some(aliases) = entry.aliases() {
				for alias in aliases {
					table.insert_spelling(alias, path);
				}
			}
		}
		table
	}

	fn insert_spelling(&mut self, segments: &[String], path: &NudoxPath) {
		self.exact.insert(segments.join("::"), path.clone());
		if let Some((last, parent)) = segments.split_last() {
			match self.suffix.entry(last.clone()) {
				Entry::Vacant(v) => {
					v.insert(Some(path.clone()));
				}
				Entry::Occupied(mut o) => {
					if o.get().as_ref() != Some(path) {
						*o.get_mut() = None; // ambiguous suffix
					}
				}
			}
			self.by_module.entry(parent.to_vec()).or_default().insert(last.clone());
		}
	}

	/// Exact fq (or alias) lookup.
	pub fn resolve_exact(&self, fq: &str) -> Option<&NudoxPath> {
		self.exact.get(fq)
	}

	/// Unique last-segment lookup; `None` if the leaf is absent or ambiguous.
	pub fn resolve_suffix(&self, leaf: &str) -> Option<&NudoxPath> {
		self.suffix.get(leaf).and_then(Option::as_ref)
	}

	/// The leaf names declared directly under `module`, if any.
	pub fn module_members(&self, module: &[String]) -> Option<&HashSet<String>> {
		self.by_module.get(module)
	}
}
