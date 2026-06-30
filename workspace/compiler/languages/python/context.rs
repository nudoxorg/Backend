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
use super::types;
use ir::entry::{Index, NudoxPath};

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
    pub fn check_snippet(&self, module_name: &str, source: &str) -> Result<Handle, String> {
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

        Ok(handle)
    }

    /// Type-check a file on disk.
    ///
    /// `module_name` — the dotted Python module name.
    /// `path` — the filesystem path to the `.py` file.
    ///
    /// Returns a `Handle` that can be used with `lower_handle`.
    pub fn check_file(&self, module_name: &str, path: &Path) -> Result<Handle, String> {
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

        Ok(handle)
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
