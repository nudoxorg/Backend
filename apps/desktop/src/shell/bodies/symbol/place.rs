//! Where a declaration's file is on this machine: a local project's own
//! directory, or the unpacked registry release. The page opens a place in
//! the editor and reads the line a use names from these paths; a package
//! whose source is not on disk has no place to open.

use crate::model::pages::PackageRef;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

type Key = (String, String);

fn cache() -> &'static Mutex<HashMap<Key, Option<PathBuf>>> {
    static CACHE: OnceLock<Mutex<HashMap<Key, Option<PathBuf>>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// `~/.cargo/registry/src/<index>/<name>-<version>`, when it is unpacked.
fn registry_source(name: &str, version: &str) -> Option<PathBuf> {
    let home = std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cargo")))?;
    let src = home.join("registry").join("src");
    std::fs::read_dir(src)
        .ok()?
        .filter_map(Result::ok)
        .map(|index| index.path().join(format!("{name}-{version}")))
        .find(|dir| dir.is_dir())
}

/// The directory `package`'s relative paths are under.
pub(super) fn root(package: &PackageRef) -> Option<PathBuf> {
    if package.is_local() {
        let path = PathBuf::from(package.as_str());
        // A test's package is a path that is not on disk; its files are given.
        return (cfg!(test) || path.is_dir()).then_some(path);
    }
    let key = (package.as_str().to_owned(), String::new());
    if let Some(found) = cache()
        .lock()
        .ok()
        .and_then(|cache| cache.get(&key).cloned())
    {
        return found;
    }
    let text = package.as_str().strip_prefix("pkg:cargo/")?;
    let (name, version) = text.split_once('@')?;
    let version = version.split(['?', '#']).next().unwrap_or(version);
    let found = registry_source(name.rsplit('/').next().unwrap_or(name), version);
    if let Ok(mut cache) = cache().lock() {
        cache.insert(key, found.clone());
    }
    found
}

/// The absolute path of `relative` in `package`, when its source is here.
pub(super) fn absolute(package: &PackageRef, relative: &str) -> Option<String> {
    let path = root(package)?.join(relative);
    (cfg!(test) || path.exists()).then(|| path.to_string_lossy().into_owned())
}
