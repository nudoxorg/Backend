//! Resolving a Python package: local source-tree discovery into pyrefly modules.
//!
//! This module walks a package *root* (a `sys.path` entry — the directory that
//! contains the top-level packages/modules) and turns every importable
//! `.py`/`.pyi` file into a pyrefly [`Handle`].
//!
//! ## Qualname derivation
//! The dotted module name is computed from each file's path **relative to the
//! root** via pyrefly's own [`ModuleName::from_relative_path`], so we inherit
//! its exact rules:
//!   * `pkg/a.py`             → `pkg.a`
//!   * `pkg/sub/c.py`         → `pkg.sub.c`
//!   * `pkg/__init__.py`      → `pkg`        (the `__init__` component is dropped)
//!   * `pkg/sub/__init__.pyi` → `pkg.sub`
//! The `.py`/`.pyi` extension is stripped; only Python extensions are accepted.
//!
//! ## `.pyi` shadowing
//! A `.pyi` stub takes precedence over a sibling `.py` with the same module
//! name (an *interface* shadows its *executable* implementation). When both are
//! discovered we keep the interface and drop the executable.
//!
//! ## Namespace packages
//! Directories without an `__init__.py` are still traversed; their `.py` files
//! become dotted modules using the directory components. We do not emit a
//! synthetic module entry for the bare namespace directory — only real files
//! become handles (see the phase report's caveats).

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use pyrefly_build::handle::Handle;
use pyrefly_python::module_name::ModuleName;
use pyrefly_python::module_path::ModuleStyle;
use pyrefly_python::sys_info::SysInfo;

/// One importable module discovered on disk.
#[derive(Debug, Clone)]
pub struct DiscoveredModule {
    /// The dotted Python module name (e.g. `pkg.sub.c`).
    pub name:     ModuleName,
    /// The on-disk path to the backing `.py`/`.pyi` file.
    pub path:     PathBuf,
    /// Convenience copy of `name.as_str()` (e.g. `pkg.sub.c`).
    pub qualname: String,
}

impl DiscoveredModule {
    /// Build the pyrefly `Handle` for this module under the given environment.
    pub fn to_handle(&self, sys_info: &SysInfo) -> Handle {
        super::module::filesystem_handle(self.name, &self.path, sys_info)
    }

    /// `true` when the backing file is a `.pyi` stub (an interface module).
    pub fn is_interface(&self) -> bool {
        ModuleStyle::of_path(&self.path) == ModuleStyle::Interface
    }
}

/// Whether a path has a Python source extension we care about.
fn is_python_file(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("py") | Some("pyi")
    )
}

/// Recursively collect every `.py`/`.pyi` file under `dir`.
fn collect_python_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let file_type = match entry.file_type() {
            Ok(t) => t,
            Err(_) => continue,
        };
        if file_type.is_dir() {
            // Skip obvious non-source noise; keep namespace dirs.
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                if name == "__pycache__" || name.starts_with('.') {
                    continue;
                }
            }
            collect_python_files(&path, out);
        } else if file_type.is_file() && is_python_file(&path) {
            out.push(path);
        }
    }
}

/// Discover all importable modules under `root`.
///
/// `root` is treated as a `sys.path` entry (the import root): qualnames are
/// derived from each file's path **relative to `root`**. A `.pyi` stub shadows
/// the sibling `.py` with the same dotted name.
///
/// Returns the discovered modules sorted by qualname for determinism.
pub fn discover_modules(root: &Path) -> Vec<DiscoveredModule> {
    let mut files = Vec::new();
    collect_python_files(root, &mut files);

    // Map dotted-name → chosen module, applying `.pyi`-shadows-`.py`.
    let mut by_name: BTreeMap<String, DiscoveredModule> = BTreeMap::new();

    for path in files {
        let rel = match path.strip_prefix(root) {
            Ok(r) => r,
            Err(_) => continue,
        };
        // Use pyrefly's own relative-path → ModuleName logic so `__init__`
        // handling and extension stripping exactly match the type checker.
        let name = match ModuleName::from_relative_path(rel) {
            Ok(n) => n,
            Err(_) => continue,
        };
        let qualname = name.as_str().to_string();

        let candidate = DiscoveredModule {
            name,
            path: path.clone(),
            qualname: qualname.clone(),
        };

        match by_name.get(&qualname) {
            // Keep an already-chosen interface (`.pyi`) over any later sibling.
            Some(existing) if existing.is_interface() => {}
            // Replace an executable with an interface; otherwise keep the first.
            Some(_) if candidate.is_interface() => {
                by_name.insert(qualname, candidate);
            }
            Some(_) => {}
            None => {
                by_name.insert(qualname, candidate);
            }
        }
    }

    by_name.into_values().collect()
}

/// Discover modules and immediately resolve them to pyrefly handles.
pub fn discover_handles(root: &Path, sys_info: &SysInfo) -> Vec<(DiscoveredModule, Handle)> {
    discover_modules(root)
        .into_iter()
        .map(|m| {
            let h = m.to_handle(sys_info);
            (m, h)
        })
        .collect()
}
