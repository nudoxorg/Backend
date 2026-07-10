//! The lowering context for a Go module: oracle output → `ir::entry::Index`.
//!
//! This is the producer's public entry point, mirroring the shape of
//! `python::context::PythonContext`: construct a context over a resolved
//! source tree, then lower it into one [`Index`].
//!
//! Where the Python context wraps a live type-checker (`pyrefly::State`),
//! the Go context holds the *finished* facts: the `go.mod`-derived module
//! metadata and the oracle's exhaustive JSON document. All type resolution
//! already happened inside the oracle (`go/types`), so lowering here is a
//! pure function over that document. No memoization or cycle guards are
//! needed at this layer — the oracle emits named types by *reference*
//! (never inlined), so recursive Go types cannot recurse the lowering
//! (see `oracle`/`types` module docs).
//!
//! ## Wiring
//! [`item::lower_package`] mints `(NudoxPath, Entry)` pairs per package;
//! this module assembles them into the index and then wires the tree
//! structure the flat pairs cannot see on their own:
//!
//! * every entry minted for a package (items AND standalone method
//!   entries) becomes a `members` child of that package's
//!   `Entry::Module` — the path scheme (`import/path::Name`,
//!   `import/path::Type.Method`) makes ownership derivable by splitting
//!   on the first `::`, which never occurs in an import path;
//! * each package module becomes a `members` child of its *nearest
//!   discovered ancestor* package (Go trees routinely skip directory
//!   levels that contain no `.go` files);
//! * packages with no discovered ancestor become the index `root_ids`.

use std::collections::HashMap;
use std::path::Path;

use ir::entry::{Index, NudoxPath};
use ir::kind::Entry;

use super::error::{GoError, Result};
use super::item;
use super::oracle;
use super::package;

/// The lowering context for one Go module.
pub struct GoContext {
	/// go.mod-derived metadata for the module on disk.
	pub module: package::GoModule,

	/// The oracle's exhaustive document over that module.
	pub output: oracle::Output,
}

impl GoContext {
	/// Resolve the module at (or above) `start` and run the vendored
	/// oracle over it.
	///
	/// Load diagnostics that still produced a document are carried in
	/// [`oracle::Output::errors`] and surfaced as warnings; they do not
	/// fail the build (the oracle extracts best-effort).
	pub fn load(ctx: &dyn crate::compile::producer::ForgeContext, start: &Path) -> Result<Self> {
		let module = package::discover_module(start)?;
		let output = package::run_oracle(ctx, &module.root)
			.map_err(|source| GoError::RunOracle {
				root:   module.root.clone(),
				source: Box::new(source),
			})?;

		for error in &output.errors {
			tracing::warn!(module = %module.module_path, "go oracle diagnostic: {error}");
		}

		Ok(GoContext { module, output })
	}

	/// Lower every package of the module into a single [`Index`].
	pub fn lower_package(&self) -> Index {
		let mut index = Index {
			root_ids:        Vec::new(),
			entries_by_path: Default::default(),
		};

		// Package import path -> the NudoxPath its `Entry::Module` is
		// stored under, used for member wiring below.
		let mut module_keys: HashMap<String, NudoxPath> = HashMap::new();
		// Module key -> member entry paths, in mint order (the oracle
		// sorts decls by name and packages by import path, so this is
		// deterministic).
		let mut members: HashMap<NudoxPath, Vec<NudoxPath>> = HashMap::new();

		for pkg in &self.output.packages {
			let module_key = item::package_key(&pkg.import_path);
			module_keys.insert(pkg.import_path.clone(), module_key.clone());

			for (path, entry) in item::lower_package(pkg) {
				if path != module_key {
					members.entry(module_key.clone()).or_default().push(path.clone());
				}
				index.entries_by_path.insert(path, entry);
			}
		}

		// Nest each package under its nearest discovered ancestor; the
		// orphans are the module's roots.
		for pkg in &self.output.packages {
			let module_key = &module_keys[&pkg.import_path];
			match nearest_ancestor(&pkg.import_path, &module_keys) {
				Some(parent_key) => {
					members.entry(parent_key).or_default().push(module_key.clone());
				}
				None => index.root_ids.push(module_key.clone()),
			}
		}

		for (module_key, children) in members {
			if let Some(Entry::Module(symbol)) = index.entries_by_path.get_mut(&module_key) {
				symbol.inner.members = Some(children);
			}
		}

		index
	}
}

/// Discover, extract, and lower the Go module rooted at (or above)
/// `root` in one call — the producer's one-shot entry point.
pub fn lower_package(
	ctx: &dyn crate::compile::producer::ForgeContext,
	root: &Path,
) -> Result<Index> {
	Ok(GoContext::load(ctx, root)?.lower_package())
}

/// The module key of `import_path`'s nearest discovered proper ancestor
/// package (`a/b/c` → `a/b`, else `a`, …), if any.
fn nearest_ancestor(
	import_path: &str,
	module_keys: &HashMap<String, NudoxPath>,
) -> Option<NudoxPath> {
	let mut prefix = import_path;
	while let Some((head, _)) = prefix.rsplit_once('/') {
		if let Some(key) = module_keys.get(head) {
			return Some(key.clone());
		}
		prefix = head;
	}
	None
}

// ---------------------------------------------------------------------------
// Tests (pure wiring over a canned oracle document — no Go toolchain)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
	use super::*;

	/// A minimal two-package document: a root package with one struct
	/// (with a method) and a nested package two path levels down.
	const DOC: &str = r#"{
		"module": {"path": "example.com/m", "dir": "/tmp/m", "goVersion": "1.23"},
		"packages": [
			{
				"importPath": "example.com/m",
				"name": "m",
				"decls": [
					{
						"kind": "type",
						"name": "Server",
						"exported": true,
						"underlying": {"kind": "struct", "fields": [
							{"name": "Addr", "type": {"kind": "basic", "name": "string"}, "exported": true}
						]},
						"methods": [
							{"name": "Close", "exported": true, "pointerRecv": true,
							 "signature": {"kind": "func", "results": [
								{"type": {"kind": "named", "name": "error"}}
							 ]}}
						]
					}
				]
			},
			{
				"importPath": "example.com/m/internal/deep",
				"name": "deep",
				"decls": [
					{"kind": "func", "name": "Helper", "exported": true,
					 "signature": {"kind": "func"}}
				]
			}
		]
	}"#;

	fn context() -> GoContext {
		GoContext {
			module: package::GoModule {
				root:        std::path::PathBuf::from("/tmp/m"),
				module_path: "example.com/m".to_string(),
				go_version:  Some("1.23".to_string()),
			},
			output: serde_json::from_str(DOC).expect("canned document parses"),
		}
	}

	#[test]
	fn wires_members_and_roots() {
		let index = context().lower_package();

		// The root package is the single root (the nested package's
		// ancestor `example.com/m/internal` was never discovered, so it
		// nests under `example.com/m` directly).
		let root_key = item::package_key("example.com/m");
		assert_eq!(index.root_ids, vec![root_key.clone()]);

		let Some(Entry::Module(root)) = index.entries_by_path.get(&root_key) else {
			panic!("root module entry missing");
		};
		let members = root.inner.members.as_ref().expect("root has members");
		assert!(members.contains(&item::item_key("example.com/m", "Server")));
		assert!(members.contains(&item::method_key("example.com/m", "Server", "Close")));
		assert!(members.contains(&item::package_key("example.com/m/internal/deep")));

		// The nested package owns its function.
		let deep_key = item::package_key("example.com/m/internal/deep");
		let Some(Entry::Module(deep)) = index.entries_by_path.get(&deep_key) else {
			panic!("nested module entry missing");
		};
		let deep_members = deep.inner.members.as_ref().expect("nested has members");
		assert_eq!(
			deep_members,
			&vec![item::item_key("example.com/m/internal/deep", "Helper")]
		);
	}

	#[test]
	fn struct_and_method_entries_land() {
		let index = context().lower_package();

		let server = index
			.entries_by_path
			.get(&item::item_key("example.com/m", "Server"))
			.expect("Server entry");
		let Entry::RecordType(record) = server else {
			panic!("Server should lower to a RecordType");
		};
		assert_eq!(record.inner.fields.len(), 1);
		let members = record.inner.members.as_ref().expect("record members");
		assert_eq!(members, &vec![item::method_key("example.com/m", "Server", "Close")]);

		let close = index
			.entries_by_path
			.get(&item::method_key("example.com/m", "Server", "Close"))
			.expect("Close entry");
		let Entry::Function(func) = close else {
			panic!("Close should lower to a Function");
		};
		assert_eq!(
			func.inner.receiver,
			Some(ir::protocols::ReceiverKind::MutRef)
		);
	}
}
