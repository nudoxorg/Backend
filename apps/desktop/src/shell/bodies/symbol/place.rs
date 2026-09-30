//! Where a declaration's file is on this machine: a local project's own
//! directory, or the unpacked registry release. The page opens a place in
//! the editor and reads the line a use names from these paths; a package
//! whose source is not on disk has no place to open.

use crate::model::pages::PackageRef;
use std::path::PathBuf;

/// The directory `package`'s relative paths are under.
pub(super) fn root(package: &PackageRef) -> Option<PathBuf> {
    if package.is_local() {
        let path = PathBuf::from(package.as_str());
        // A test's package is a path that is not on disk; its files are given.
        return (cfg!(test) || path.is_dir()).then_some(path);
    }
    let text = package.as_str().strip_prefix("pkg:cargo/")?;
    let (name, version) = text.split_once('@')?;
    let version = version.split(['?', '#']).next().unwrap_or(version);
    let release =
        crate::host::registry::Release::new(name.rsplit('/').next().unwrap_or(name), version)
            .ok()?;
    let composition = crate::host::registry::composed()?;
    composition
        .source
        .resolve(&release)
        .ok()
        .map(|tree| tree.root)
}

/// The absolute path of `relative` in `package`, when its source is here.
pub(super) fn absolute(package: &PackageRef, relative: &str) -> Option<String> {
    let path = root(package)?.join(relative);
    (cfg!(test) || path.exists()).then(|| path.to_string_lossy().into_owned())
}
