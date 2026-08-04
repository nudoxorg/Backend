use std::path::Path;

use pyrefly_build::handle::Handle;
use pyrefly_python::module_name::ModuleName;
use pyrefly_python::module_path::ModulePath;
use pyrefly_python::sys_info::SysInfo;

/// Resolve a Python module name + file path into a pyrefly `Handle`.
///
/// This is the bridge between the compiler's file-discovery layer and
/// pyrefly's module representation.
pub fn resolve_handle(
    module_name: &str,
    file_path: &Path,
    sys_info: &SysInfo,
) -> Handle {
    let name = ModuleName::from_str(module_name);
    let path = ModulePath::filesystem(file_path.to_path_buf());
    Handle::new(name, path, sys_info.clone())
}

/// Resolve a Python module from a memory/snippet source into a `Handle`.
pub fn resolve_memory_handle(
    module_name: &str,
    virtual_path: &str,
    sys_info: &SysInfo,
) -> Handle {
    let name = ModuleName::from_str(module_name);
    let path = ModulePath::memory(std::path::PathBuf::from(virtual_path));
    Handle::new(name, path, sys_info.clone())
}

/// Build a filesystem `Handle` from an already-resolved [`ModuleName`].
///
/// Used by package discovery, where the dotted module name has been computed
/// from a file's path relative to the import root (so we keep the precise
/// `ModuleName` rather than round-tripping through a string).
pub fn filesystem_handle(name: ModuleName, file_path: &Path, sys_info: &SysInfo) -> Handle {
    Handle::new(
        name,
        ModulePath::filesystem(file_path.to_path_buf()),
        sys_info.clone(),
    )
}
