use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use pyrefly::commands::config_finder::default_config_finder;
use pyrefly::state::load::FileContents;
use pyrefly::state::require::Require;
use pyrefly::state::state::State;
use pyrefly::state::state::Transaction;
use pyrefly_build::handle::Handle;
use pyrefly_python::module::Module;
use pyrefly_python::module_name::ModuleName;
use pyrefly_python::module_path::ModulePath;
use pyrefly_python::sys_info::SysInfo;
use pyrefly_util::thread_pool::ThreadCount;

use super::item;
use super::package;
use super::types;
use ir::entry::{Index, NudoxPath};
use ir::kind::Entry;

/// The lowering context for a Python package.
///
/// Holds the pyrefly `State` (the type-checker's database) and provides
/// a clean interface for loading, checking, and lowering Python source
/// into the compiler IR.
///
/// # Lifecycle
///
/// 1. `PythonContext::new()` — initialise a pyrefly `State` with default
///    config discovery.
/// 2. `check_file()` / `check_snippet()` — load a source file or string
///    into the checker and run type inference. Returns a `Handle` key.
/// 3. `lower_handle()` — extract bindings, answers, and class info from
///    the checker for a given handle and lower everything into an
///    `ir::Index`.
///
/// Currently only single-snippet / single-file mode is surfaced. Batch
/// mode (whole-package checking) will use `State::new_committable_transaction`.
pub struct PythonContext {
    state: State,
}

impl PythonContext {
    pub fn new() -> Self {
        let config_finder = default_config_finder(None);
        let thread_count = ThreadCount::default();
        let state = State::new(config_finder, thread_count);
        PythonContext { state }
    }

    /// Type-check a source string as a virtual module.
    ///
    /// `module_name` — the dotted Python module name (e.g. `"my_module"`).
    /// `source` — the Python source code to check.
    ///
    /// Returns a `Handle` that can be used with `lower_handle`.
    pub fn check_snippet(&self, module_name: &str, source: &str) -> Handle {
        let module_name = ModuleName::from_str(module_name);
        let module_path = ModulePath::memory(PathBuf::from(format!("{}.py", module_name)));
        let sys_info = self.resolve_sys_info();
        let handle = Handle::new(module_name, module_path, sys_info);

        // Run inference inside a *committable* transaction and commit it, so the
        // resolved bindings/answers persist in the shared `State`. A throwaway
        // `state.transaction()` would discard its `updated_modules` on drop,
        // leaving `lower_handle` to read an empty (Any-only) view.
        let mut tx = self
            .state
            .new_committable_transaction(Require::Everything, None);
        tx.as_mut().set_memory(vec![(
            handle.path().as_path().to_path_buf(),
            Some(Arc::new(FileContents::from_source(source.to_string()))),
        )]);
        tx.as_mut().run(&[handle.clone()], Require::Everything, None);
        self.state.commit_transaction(tx, None);

        handle
    }

    /// Type-check a file on disk.
    ///
    /// `module_name` — the dotted Python module name.
    /// `path` — the filesystem path to the `.py` file.
    ///
    /// Returns a `Handle` that can be used with `lower_handle`.
    pub fn check_file(&self, module_name: &str, path: &Path) -> Handle {
        let module_name = ModuleName::from_str(module_name);
        let module_path = ModulePath::filesystem(path.to_path_buf());
        let sys_info = self.resolve_sys_info();
        let handle = Handle::new(module_name, module_path, sys_info);

        // Same committed-transaction discipline as `check_snippet` (see there).
        let mut tx = self
            .state
            .new_committable_transaction(Require::Everything, None);
        tx.as_mut().run(&[handle.clone()], Require::Everything, None);
        self.state.commit_transaction(tx, None);

        handle
    }

    /// Lower a previously-checked handle into an `ir::Index`.
    ///
    /// Runs a fresh transaction to read bindings, answers, and errors.
    pub fn lower_handle(&self, handle: &Handle) -> Index {
        let tx = self.state.transaction();

        let module_info = tx.get_module_info(handle);

        let mut index = Index {
            root_ids: Vec::new(),
            entries_by_path: Default::default(),
        };

        if let Some(module_info) = module_info {
            let path = module_path_to_nudox(&module_info);
            let module_entry = item::lower_module(handle, &tx, &path);

            if let Some(module_nudox) = module_info_path(&module_info) {
                index.root_ids.push(module_nudox);
            }
            for (child_path, entry) in module_entry {
                index.entries_by_path.insert(child_path, entry);
            }
        }

        index
    }

    /// Discover, type-check, and lower an entire Python package rooted at
    /// `root` into a single [`Index`].
    ///
    /// `root` is treated as a `sys.path` entry (the import root). Every
    /// `.py`/`.pyi` file under it becomes a module (see [`package::discover_modules`]
    /// for the qualname/`.pyi`-shadowing rules).
    ///
    /// # Cross-module inference
    /// All discovered handles are loaded into ONE *committable* transaction and
    /// run together, then committed. Running them together is what lets imports
    /// resolve across modules (e.g. `b.py`'s `from pkg.a import Foo`); committing
    /// is what makes the solved types visible to the *fresh* read transaction
    /// `lower_handle`-style lowering opens below. (A throwaway `transaction()`
    /// would drop its `updated_modules` on `drop`, leaving an `Any`-only view.)
    ///
    /// After lowering each module via [`item::lower_module`] and merging the
    /// results, a post-process pass wires every `Entry::Module`'s `members`
    /// from the `Local("qualname::name")` child paths, and sets the package's
    /// top-level modules as `root_ids`.
    pub fn lower_package(&self, root: &Path) -> Index {
        let sys_info = self.resolve_sys_info();
        let discovered = package::discover_handles(root, &sys_info);

        let mut index = Index {
            root_ids:        Vec::new(),
            entries_by_path: Default::default(),
        };
        if discovered.is_empty() {
            return index;
        }

        // --- 1. Load every module into ONE committable transaction. ---------
        let handles: Vec<Handle> = discovered.iter().map(|(_, h)| h.clone()).collect();
        let mut tx = self
            .state
            .new_committable_transaction(Require::Everything, None);
        tx.as_mut().run(&handles, Require::Everything, None);
        self.state.commit_transaction(tx, None);

        // --- 2. Lower each module against a fresh (committed) read tx. -------
        let read_tx = self.state.transaction();

        // qualname -> the NudoxPath key under which that module's `Entry::Module`
        // is stored (the module file path), used to wire `members` afterwards.
        let mut module_key_by_qualname: HashMap<String, NudoxPath> = HashMap::new();

        for (disc, handle) in &discovered {
            let module_info = match read_tx.get_module_info(handle) {
                Some(m) => m,
                None => continue,
            };
            let module_path = module_path_to_nudox(&module_info);
            let lowered = item::lower_module(handle, &read_tx, &module_path);

            for (child_path, entry) in lowered {
                if matches!(entry, Entry::Module(_)) {
                    module_key_by_qualname.insert(disc.qualname.clone(), child_path.clone());
                }
                index.entries_by_path.insert(child_path, entry);
            }
        }

        // --- 3. Post-process: wire module->children `members` + `root_ids`. --
        wire_members(&mut index, &module_key_by_qualname);
        index.root_ids = compute_root_ids(&module_key_by_qualname);

        index
    }

    /// Resolve the Python environment's `SysInfo`.
    ///
    /// IMPLEMENTATION NOTE: In the CLI path this uses the config finder's
    /// environment detection. For full control, pass an explicit `SysInfo`.
    fn resolve_sys_info(&self) -> SysInfo {
        SysInfo::default()
    }
}

impl Default for PythonContext {
    fn default() -> Self {
        Self::new()
    }
}

fn module_info_path(module_info: &Module) -> Option<NudoxPath> {
    let path = module_info.path();
    match path.as_path().to_str() {
        Some(s) => Some(NudoxPath::Local(std::path::PathBuf::from(s))),
        None => None,
    }
}

fn module_path_to_nudox(module_info: &Module) -> NudoxPath {
    let path = module_info.path();
    NudoxPath::Local(path.as_path().to_path_buf())
}

/// Extract the owning module qualname from a child entry's `Local("qualname::name")`
/// path. Returns `None` for paths that don't follow the `module::name` scheme
/// (e.g. module entries, which are keyed by file path).
fn owning_module_qualname(path: &NudoxPath) -> Option<String> {
    let s = match path {
        NudoxPath::Local(p) => p.to_str()?,
        NudoxPath::External { .. } => return None,
    };
    // Split once on the FIRST `::`. `item.rs` mints module-level children as
    // `format!("{}::{}", module_qualname, name)`, and Python qualnames use `.`
    // (never `::`) so the segment before the first `::` is the owning module.
    s.split_once("::").map(|(module, _)| module.to_string())
}

/// Populate each `Entry::Module`'s `members` with the paths of the child
/// entries that belong to it, derived purely from the `Local("qualname::name")`
/// child-path scheme (see [`owning_module_qualname`]).
fn wire_members(index: &mut Index, module_key_by_qualname: &HashMap<String, NudoxPath>) {
    // Group child paths under their owning module's storage key.
    let mut members_by_module: HashMap<NudoxPath, Vec<NudoxPath>> = HashMap::new();
    for path in index.entries_by_path.keys() {
        if let Some(qualname) = owning_module_qualname(path) {
            if let Some(module_key) = module_key_by_qualname.get(&qualname) {
                members_by_module
                    .entry(module_key.clone())
                    .or_default()
                    .push(path.clone());
            }
        }
    }

    for (module_key, mut members) in members_by_module {
        // Deterministic ordering for stable output/tests.
        members.sort_by(|a, b| format!("{a:?}").cmp(&format!("{b:?}")));
        if let Some(Entry::Module(symbol)) = index.entries_by_path.get_mut(&module_key) {
            symbol.inner.members = Some(members);
        }
    }
}

/// The package's top-level modules become `root_ids`: a module is top-level
/// when no *other discovered module* is its parent package. The parent of
/// `pkg.sub.c` is `pkg.sub`; a module with no `.` (e.g. `pkg`) has no parent
/// and is always a root. A submodule under a missing namespace parent (parent
/// not discovered) is also treated as a root.
fn compute_root_ids(module_key_by_qualname: &HashMap<String, NudoxPath>) -> Vec<NudoxPath> {
    let mut roots: Vec<NudoxPath> = module_key_by_qualname
        .iter()
        .filter(|(qualname, _)| match qualname.rsplit_once('.') {
            None => true,
            Some((parent, _)) => !module_key_by_qualname.contains_key(parent),
        })
        .map(|(_, key)| key.clone())
        .collect();
    roots.sort_by(|a, b| format!("{a:?}").cmp(&format!("{b:?}")));
    roots
}
