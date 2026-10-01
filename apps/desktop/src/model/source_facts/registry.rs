//! The cargo registry on this machine: where a crate's source lives, when
//! each release was published (the sparse index cache's `pubtime`), and
//! which release satisfies a requirement. Everything here is a file read;
//! nothing touches the network.

use facet::folio::state::Standing;
use facet::marks::semver;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/// One published release of a crate, from the sparse index cache.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Published {
    /// The version, with build metadata as the registry spells it.
    pub version: String,
    /// `YYYY-MM-DD`, when the cache records a publish time.
    pub date: Option<String>,
    /// Whether its publisher withdrew it.
    pub standing: Standing,
    /// The archive's sha256 (lowercase hex) as the registry published it.
    pub checksum: Option<String>,
}

fn cargo_home() -> Option<PathBuf> {
    std::env::var_os("CARGO_HOME").map(PathBuf::from).or_else(|| std::env::var_os("HOME").map(|home| Path::new(&home).join(".cargo")))
}

fn glob_dirs(parent: &Path, prefix: &str) -> Vec<PathBuf> {
    let Ok(read) = fs::read_dir(parent) else { return Vec::new() };
    let mut out: Vec<PathBuf> = read
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().starts_with(prefix))
        .map(|e| e.path())
        .collect();
    out.sort();
    out
}

/// The registries' unpacked source directories (`~/.cargo/registry/src/index.crates.io-*`).
#[must_use]
pub fn source_dirs() -> Vec<PathBuf> {
    cargo_home().map(|home| glob_dirs(&home.join("registry").join("src"), "index.crates.io-")).unwrap_or_default()
}

fn cache_dirs() -> Vec<PathBuf> {
    cargo_home()
        .map(|home| glob_dirs(&home.join("registry").join("index"), "index.crates.io-").into_iter().map(|d| d.join(".cache")).collect())
        .unwrap_or_default()
}

/// Where the sparse index caches `name`: `1/x`, `2/xy`, `3/x/xyz`, `ab/cd/abcdef`.
fn cache_relative(name: &str) -> PathBuf {
    let n = name.to_lowercase();
    // Crate names are ASCII; anything else (or nothing) has no cache entry.
    if n.is_empty() || !n.is_ascii() {
        return PathBuf::new();
    }
    match n.len() {
        1 => Path::new("1").join(&n),
        2 => Path::new("2").join(&n),
        3 => Path::new("3").join(&n[..1]).join(&n),
        _ => Path::new(&n[..2]).join(&n[2..4]).join(&n),
    }
}

/// Every published release of `name` the index cache knows, oldest first.
#[must_use]
pub fn releases(name: &str) -> Vec<Published> {
    let relative = cache_relative(name);
    let Some(bytes) = cache_dirs().into_iter().find_map(|dir| fs::read(dir.join(&relative)).ok()) else {
        return Vec::new();
    };
    let mut out: Vec<Published> = Vec::new();
    // Framing: a few header bytes and an etag, then per release a NUL, its
    // version, a NUL, and its JSON.
    for part in bytes.split(|b| *b == 0) {
        if part.first() != Some(&b'{') {
            continue;
        }
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(part) else { continue };
        let Some(version) = value.get("vers").and_then(|v| v.as_str()) else { continue };
        out.push(Published {
            version: version.to_owned(),
            date: value.get("pubtime").and_then(|v| v.as_str()).and_then(|t| t.get(..10)).map(str::to_owned),
            standing: if value.get("yanked").and_then(serde_json::Value::as_bool).unwrap_or(false) { Standing::Yanked } else { Standing::Available },
            checksum: value.get("cksum").and_then(|v| v.as_str()).map(str::to_owned),
        });
    }
    out.sort_by(|a, b| semver::cmp(&a.version, &b.version));
    out.dedup_by(|a, b| a.version == b.version);
    out
}

/// The source directory of `name` at exactly `version`, when it is unpacked.
#[must_use]
pub fn source_of(name: &str, version: &str) -> Option<PathBuf> {
    source_dirs().into_iter().map(|dir| dir.join(format!("{name}-{version}"))).find(|dir| dir.is_dir())
}

/// Every version of `name` unpacked on this machine.
fn unpacked(name: &str) -> Vec<String> {
    let prefix = format!("{name}-");
    let mut out = Vec::new();
    for dir in source_dirs() {
        let Ok(read) = fs::read_dir(&dir) else { continue };
        for entry in read.filter_map(Result::ok) {
            let file = entry.file_name().to_string_lossy().into_owned();
            let Some(rest) = file.strip_prefix(&prefix) else { continue };
            // `foo-1.2.3`, never `foo-bar-1.2.3`.
            if rest.chars().next().is_some_and(|c| c.is_ascii_digit()) && rest.contains('.') {
                out.push(rest.to_owned());
            }
        }
    }
    out.sort_by(|a, b| semver::cmp(a, b));
    out.dedup();
    out
}

fn numbers(version: &str) -> (u64, u64, u64) {
    let v = semver::parse(version);
    (v.major, v.minor, v.patch)
}

/// Whether `version` satisfies one comparator (`^1.2`, `~1.2.3`, `>=1`, `=1.2.3`, `1.2`, `*`).
fn comparator(text: &str, version: &str) -> bool {
    let text = text.trim();
    if text.is_empty() || text == "*" {
        return true;
    }
    let (op, rest) = ["<=", ">=", "<", ">", "=", "^", "~"]
        .iter()
        .find_map(|op| text.strip_prefix(op).map(|rest| (*op, rest.trim())))
        .unwrap_or(("^", text));
    let parts: Vec<Option<u64>> = rest.split('-').next().unwrap_or(rest).split('+').next().unwrap_or(rest).split('.').map(|p| p.trim().parse().ok()).collect();
    let (major, minor, patch) = (parts.first().copied().flatten(), parts.get(1).copied().flatten(), parts.get(2).copied().flatten());
    let Some(major) = major else { return true };
    let v = numbers(version);
    let lower = (major, minor.unwrap_or(0), patch.unwrap_or(0));
    match op {
        "=" => v.0 == major && minor.is_none_or(|m| v.1 == m) && patch.is_none_or(|p| v.2 == p),
        ">=" => v >= lower,
        ">" => match (minor, patch) {
            (None, _) => v.0 > major,
            (Some(m), None) => (v.0, v.1) > (major, m),
            _ => v > lower,
        },
        "<=" => match (minor, patch) {
            (None, _) => v.0 <= major,
            (Some(m), None) => (v.0, v.1) <= (major, m),
            _ => v <= lower,
        },
        "<" => v < lower,
        "~" => v >= lower && v.0 == major && minor.is_none_or(|m| v.1 == m),
        _ => {
            // Caret: the leftmost non-zero component is the compatibility class.
            if v < lower {
                return false;
            }
            if major > 0 || minor.is_none() {
                v.0 == major
            } else if minor.unwrap_or(0) > 0 || patch.is_none() {
                v.0 == 0 && v.1 == minor.unwrap_or(0)
            } else {
                v.0 == 0 && v.1 == 0 && v.2 == patch.unwrap_or(0)
            }
        }
    }
}

/// Whether `version` satisfies every comparator of `requirement`.
#[must_use]
pub fn satisfies(requirement: &str, version: &str) -> bool {
    let pre = !semver::parse(version).pre.is_empty();
    if pre && !requirement.contains('-') {
        return false;
    }
    requirement.split(',').all(|c| comparator(c, version))
}

/// The release of `name` on disk that best matches `requirement`: the
/// highest that satisfies it, else the highest there is. `hint` is a
/// version a resolver already chose.
#[must_use]
pub fn pick(name: &str, requirement: &str, hint: Option<&str>) -> Option<String> {
    let have = unpacked(name);
    if let Some(hint) = hint.filter(|hint| have.iter().any(|v| v == hint)) {
        return Some(hint.to_owned());
    }
    have.iter().rev().find(|v| satisfies(requirement, v)).or_else(|| have.last()).cloned()
}

/// Lines of code per `(crate, version)`, kept for the process.
static SLOC: OnceLock<Mutex<HashMap<(String, String), Option<usize>>>> = OnceLock::new();

/// The lines of code of `name` at `version`, from its unpacked source
/// (`None` when it is not unpacked).
#[must_use]
pub fn sloc_of(name: &str, version: &str) -> Option<usize> {
    let cache = SLOC.get_or_init(Mutex::default);
    let key = (name.to_owned(), version.to_owned());
    if let Some(hit) = cache.lock().ok().and_then(|c| c.get(&key).copied()) {
        return hit;
    }
    let value = source_of(name, version).map(|dir| {
        let lib = super::manifest::read(&dir).map_or_else(|| "src/lib.rs".to_owned(), |m| m.lib);
        super::scan::sloc(&dir, &lib)
    });
    if let Ok(mut cache) = cache.lock() {
        cache.insert(key, value);
    }
    value
}

#[cfg(test)]
mod tests {
    use super::{cache_relative, satisfies};
    use std::path::Path;

    #[test]
    fn the_sparse_cache_lays_names_out_the_way_cargo_does() {
        assert_eq!(cache_relative("a"), Path::new("1/a"));
        assert_eq!(cache_relative("io"), Path::new("2/io"));
        assert_eq!(cache_relative("log"), Path::new("3/l/log"));
        assert_eq!(cache_relative("tokio"), Path::new("to/ki/tokio"));
        assert_eq!(cache_relative("Serde_Json"), Path::new("se/rd/serde_json"));
    }

    #[test]
    fn requirements_read_the_way_cargo_reads_them() {
        assert!(satisfies("1.2", "1.9.0") && !satisfies("1.2", "2.0.0") && !satisfies("1.2", "1.1.9"));
        assert!(satisfies("^0.8", "0.8.23") && !satisfies("^0.8", "0.9.0") && !satisfies("^0.8", "0.7.9"));
        assert!(satisfies("^0.0.3", "0.0.3") && !satisfies("^0.0.3", "0.0.4"));
        assert!(satisfies("~1.2.3", "1.2.9") && !satisfies("~1.2.3", "1.3.0"));
        assert!(satisfies(">=1.0, <2", "1.5.0") && !satisfies(">=1.0, <2", "2.0.0"));
        assert!(satisfies("=1.2.3", "1.2.3") && !satisfies("=1.2.3", "1.2.4"));
        assert!(satisfies("*", "9.9.9"));
        assert!(!satisfies("^1", "2.0.0-rc.1"), "a pre-release satisfies only a requirement that names one");
    }
}
