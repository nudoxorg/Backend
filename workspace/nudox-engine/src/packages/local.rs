//! A package that is already on this filesystem: what language it is, and
//! what it declares that it needs.
//!
//! # Why this module exists
//!
//! [`crate::packages::acquire`] answers "get me the bytes for this PURL". That
//! is the wrong question for a checkout: the bytes are already here, there is
//! no registry coordinate to resolve, and in a monorepo the interesting
//! dependency edges point at sibling directories that no registry has ever
//! heard of. A local root needs the *other* two facts instead — which producer
//! can read it, and what it declares — and neither was computed anywhere.
//!
//! # The honesty rule this module is built around
//!
//! Every function here returns a typed outcome rather than an empty
//! collection, because "this package declares no dependencies" and "nobody has
//! written a reader for this manifest format" are different facts that an
//! empty `Vec` renders identically. That distinction is the whole difference
//! between an agent that knows to look elsewhere and one that concludes a
//! package is standalone — the same class of defect as a reference graph that
//! was never populated reporting as a symbol with no callers.
//!
//! So [`DependencyScan`] has an [`Unsupported`](DependencyScan::Unsupported)
//! variant and callers are expected to surface it. A language whose reader
//! lands later changes one match arm here and nothing else.

use std::path::{Path, PathBuf};

use crate::ProducerLanguage;

// ---------------------------------------------------------------------------
// Language detection
// ---------------------------------------------------------------------------

/// The manifest filename each producer's package root is identified by.
///
/// Order matters where a directory could plausibly hold two: a Rust crate
/// vendored inside a Node package is a `Cargo.toml` package that happens to
/// sit under a `package.json`, and the reverse essentially does not occur.
/// The list is therefore most-specific-first rather than alphabetical, and the
/// pairing is kept in one table so "which manifest means which producer" has a
/// single answer — the same reason `descriptor_for` exists rather than an
/// inline match at each call site.
const MANIFESTS: &[(&str, ProducerLanguage)] = &[
    ("Cargo.toml", ProducerLanguage::Rust),
    ("go.mod", ProducerLanguage::Go),
    ("pom.xml", ProducerLanguage::Java),
    ("build.gradle", ProducerLanguage::Java),
    ("pyproject.toml", ProducerLanguage::Python),
    ("package.json", ProducerLanguage::TypeScript),
    ("compile_commands.json", ProducerLanguage::Cpp),
    ("CMakeLists.txt", ProducerLanguage::Cpp),
];

/// Which producer can read the package rooted at `root`, judged by which
/// manifest is present.
///
/// `None` means no manifest this workspace recognises — which is a real
/// answer, not a failure: it is what a caller gets for a path that is not a
/// package root at all, and it is why [`language_at`] is separate from the
/// error that names the path.
///
/// C# is deliberately absent from the fast path above and handled here: its
/// manifest is `*.csproj`, a *pattern* rather than a fixed name, so it costs a
/// directory read that the other seven do not.
pub fn language_at(root: &Path) -> Option<ProducerLanguage> {
    for (manifest, language) in MANIFESTS {
        if root.join(manifest).is_file() {
            return Some(*language);
        }
    }
    if has_csproj(root) {
        return Some(ProducerLanguage::CSharp);
    }
    None
}

/// Whether `root` directly contains a `*.csproj`.
///
/// Non-recursive on purpose: a solution directory containing several project
/// directories is not itself a package, and treating it as one would lower the
/// first project found while silently dropping its siblings.
fn has_csproj(root: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(root) else {
        return false;
    };
    entries.flatten().any(|entry| {
        entry
            .path()
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("csproj"))
    })
}

/// The best name to give the package rooted at `root` before its producer
/// reports the authoritative one.
///
/// Read from the manifest where that is cheap and unambiguous, and from the
/// directory name otherwise. Same caveat as
/// [`crate::PackageSpec::name`]: the oracle's answer wins once it arrives, so
/// a mismatch here is cosmetic and transient.
pub fn name_at(root: &Path, language: ProducerLanguage) -> String {
    let from_manifest = match language {
        ProducerLanguage::Rust => cargo_package_name(root),
        ProducerLanguage::TypeScript => npm_package_name(root),
        _ => None,
    };
    from_manifest.unwrap_or_else(|| {
        root.file_name()
            .and_then(|n| n.to_str())
            .filter(|n| !n.is_empty())
            .unwrap_or("package")
            .to_owned()
    })
}

fn cargo_package_name(root: &Path) -> Option<String> {
    let text = std::fs::read_to_string(root.join("Cargo.toml")).ok()?;
    let manifest: toml::Value = text.parse().ok()?;
    manifest
        .get("package")?
        .get("name")?
        .as_str()
        .map(str::to_owned)
}

fn npm_package_name(root: &Path) -> Option<String> {
    let text = std::fs::read_to_string(root.join("package.json")).ok()?;
    let manifest: serde_json::Value = serde_json::from_str(&text).ok()?;
    manifest.get("name")?.as_str().map(str::to_owned)
}

// ---------------------------------------------------------------------------
// Dependencies
// ---------------------------------------------------------------------------

/// One dependency edge read out of a package's own manifest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Dependency {
    /// Declared with a filesystem path: a cargo `path = "../lib"`, an npm
    /// `"file:../lib"`, or a workspace sibling resolved through the checkout.
    ///
    /// Indexable directly — no registry, no download, no version solving —
    /// which is what makes monorepo dependency following cheap enough to be
    /// worth offering at all.
    Local {
        /// The name the depending manifest uses for it.
        name: String,
        /// The directory the declaration points at, already made absolute.
        root: PathBuf,
    },
    /// Declared as a registry coordinate.
    ///
    /// Carries the requirement *as written* (`"^1.2"`, `"1.0.196"`,
    /// `"workspace:*"`) rather than a resolved version, because resolving one
    /// means talking to a registry and this module reads files. A caller that
    /// wants to index it has to resolve it first, and gets to decide whether
    /// that network call is worth making.
    Registry {
        /// The dependency's registry name.
        name: String,
        /// The version requirement exactly as the manifest spells it.
        requirement: String,
    },
}

impl Dependency {
    /// The name the manifest gave this dependency.
    pub fn name(&self) -> &str {
        match self {
            Self::Local { name, .. } | Self::Registry { name, .. } => name,
        }
    }
}

/// What reading a package's dependency declarations produced.
///
/// # Why this is not `Vec<Dependency>`
///
/// Three of these four outcomes would otherwise be an empty vector, and they
/// mean entirely different things: "declares nothing", "we cannot read this
/// format", "the file is corrupt", "there is no package here". Only the first
/// is a fact about the package. Collapsing the other three into it is how a
/// caller ends up reporting a complete dependency closure it never attempted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DependencyScan {
    /// The manifest was read. The vector may legitimately be empty.
    Read(Vec<Dependency>),
    /// No reader exists for this language's manifest format yet.
    ///
    /// An empty list here would claim the package is standalone. This says
    /// nobody looked, which is the only honest answer available.
    Unsupported {
        /// The language whose manifest went unread.
        language: ProducerLanguage,
        /// The file a future reader would parse.
        manifest: &'static str,
    },
    /// The manifest exists but could not be parsed.
    Malformed {
        /// The file that failed to parse.
        manifest: PathBuf,
        /// The parser's own message.
        reason: String,
    },
    /// `root` holds no manifest this workspace recognises.
    NoManifest {
        /// The directory that was probed.
        root: PathBuf,
    },
}

/// Read the dependencies the package at `root` declares.
///
/// `language` is taken rather than re-detected so a caller that already chose
/// a producer — because the user named one, or because the PURL implied one —
/// gets its dependencies read with the same manifest the producer will read,
/// instead of whichever one this module would have guessed.
pub fn declared_dependencies(root: &Path, language: ProducerLanguage) -> DependencyScan {
    match language {
        ProducerLanguage::Rust => cargo_dependencies(root),
        ProducerLanguage::TypeScript => npm_dependencies(root),
        // Each of these is one function away, and the manifest to parse is
        // already named. Reporting `Unsupported` rather than an empty list is
        // what keeps that gap visible instead of looking like a package with
        // no dependencies.
        ProducerLanguage::Go => unsupported(language, "go.mod"),
        ProducerLanguage::Java => unsupported(language, "pom.xml"),
        ProducerLanguage::CSharp => unsupported(language, "*.csproj"),
        ProducerLanguage::Python => unsupported(language, "pyproject.toml"),
        ProducerLanguage::Cpp => unsupported(language, "CMakeLists.txt"),
    }
}

fn unsupported(language: ProducerLanguage, manifest: &'static str) -> DependencyScan {
    DependencyScan::Unsupported { language, manifest }
}

// ── Cargo ───────────────────────────────────────────────────────────────────

/// Read `[dependencies]` out of a `Cargo.toml`.
///
/// Only the three dependency tables that describe what the *library* needs are
/// read. `[dev-dependencies]` is deliberately excluded: it is what the test
/// suite needs, it is frequently larger than the real dependency set, and
/// nothing in it can appear in the package's public API — so indexing it would
/// cost the most and answer the fewest questions.
fn cargo_dependencies(root: &Path) -> DependencyScan {
    let manifest_path = root.join("Cargo.toml");
    let text = match std::fs::read_to_string(&manifest_path) {
        Ok(text) => text,
        Err(_) => {
            return DependencyScan::NoManifest {
                root: root.to_path_buf(),
            };
        }
    };
    let manifest: toml::Value = match text.parse() {
        Ok(value) => value,
        Err(error) => {
            return DependencyScan::Malformed {
                manifest: manifest_path,
                reason: error.to_string(),
            };
        }
    };

    let mut found = Vec::new();
    for table in ["dependencies", "build-dependencies"] {
        let Some(entries) = manifest.get(table).and_then(toml::Value::as_table) else {
            continue;
        };
        for (name, value) in entries {
            found.push(cargo_one(root, name, value));
        }
    }
    DependencyScan::Read(found)
}

/// One `[dependencies]` entry, which is either a bare version string or a
/// table that may carry `path`.
fn cargo_one(root: &Path, name: &str, value: &toml::Value) -> Dependency {
    if let Some(requirement) = value.as_str() {
        return Dependency::Registry {
            name: name.to_owned(),
            requirement: requirement.to_owned(),
        };
    }

    if let Some(path) = value.get("path").and_then(toml::Value::as_str) {
        return Dependency::Local {
            name: name.to_owned(),
            root: absolutise(root, path),
        };
    }

    // A table with no `path`: `{ version = "1", features = [...] }`, or a git
    // dependency. Both are non-local; a git dependency has no registry
    // requirement to quote, so it reports the empty string rather than
    // inventing one.
    let requirement = value
        .get("version")
        .and_then(toml::Value::as_str)
        .unwrap_or_default()
        .to_owned();
    Dependency::Registry {
        name: name.to_owned(),
        requirement,
    }
}

// ── npm ─────────────────────────────────────────────────────────────────────

/// Read `dependencies` out of a `package.json`.
///
/// `devDependencies` is excluded for the same reason `[dev-dependencies]` is;
/// `peerDependencies` is excluded because it names what the *host* must
/// supply, so indexing it describes a package this checkout does not contain.
///
/// The two path-shaped requirement spellings are both handled, because a
/// monorepo uses one or the other and which one is a property of the package
/// manager rather than of the package:
///
/// * `"file:../lib"` — npm's own path protocol.
/// * `"workspace:*"` — pnpm/yarn workspaces, which do not name a path at all.
///   Resolved by searching the checkout upward for the sibling that declares
///   that name, which is what the package manager itself does.
fn npm_dependencies(root: &Path) -> DependencyScan {
    let manifest_path = root.join("package.json");
    let text = match std::fs::read_to_string(&manifest_path) {
        Ok(text) => text,
        Err(_) => {
            return DependencyScan::NoManifest {
                root: root.to_path_buf(),
            };
        }
    };
    let manifest: serde_json::Value = match serde_json::from_str(&text) {
        Ok(value) => value,
        Err(error) => {
            return DependencyScan::Malformed {
                manifest: manifest_path,
                reason: error.to_string(),
            };
        }
    };

    let Some(entries) = manifest.get("dependencies").and_then(|d| d.as_object()) else {
        return DependencyScan::Read(Vec::new());
    };

    let found = entries
        .iter()
        .map(|(name, value)| {
            let requirement = value.as_str().unwrap_or_default();
            npm_one(root, name, requirement)
        })
        .collect();
    DependencyScan::Read(found)
}

fn npm_one(root: &Path, name: &str, requirement: &str) -> Dependency {
    if let Some(path) = requirement.strip_prefix("file:") {
        return Dependency::Local {
            name: name.to_owned(),
            root: absolutise(root, path),
        };
    }

    if requirement.starts_with("workspace:") {
        if let Some(sibling) = find_workspace_sibling(root, name) {
            return Dependency::Local {
                name: name.to_owned(),
                root: sibling,
            };
        }
    }

    Dependency::Registry {
        name: name.to_owned(),
        requirement: requirement.to_owned(),
    }
}

/// Find the checkout directory whose `package.json` declares `name`.
///
/// Walks up from `root` and, at each level, scans one directory deep. That
/// covers the two layouts essentially every JS monorepo uses — `packages/*`
/// beside the depending package, and `packages/*` at the repository root —
/// without becoming a full-tree walk, which on a checkout with `node_modules`
/// would read hundreds of thousands of files to answer one question.
///
/// Bounded at four levels for the same reason: past that, a hit is more likely
/// to be an unrelated package that shares a name than the sibling meant.
fn find_workspace_sibling(root: &Path, name: &str) -> Option<PathBuf> {
    let mut here = root.parent();
    for _ in 0..4 {
        let level = here?;
        if let Some(found) = scan_one_level(level, name) {
            return Some(found);
        }
        here = level.parent();
    }
    None
}

fn scan_one_level(dir: &Path, name: &str) -> Option<PathBuf> {
    let entries = std::fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        // `node_modules` holds *installed* copies. Indexing one is not wrong,
        // but it is the published artifact rather than the checkout the user
        // is editing, and preferring it would make "go to definition" land in
        // a directory their edits never reach.
        if path.file_name().is_some_and(|n| n == "node_modules") {
            continue;
        }
        if npm_package_name(&path).as_deref() == Some(name) {
            return Some(path);
        }
        if let Some(found) = shallow_child_with_name(&path, name) {
            return Some(found);
        }
    }
    None
}

fn shallow_child_with_name(dir: &Path, name: &str) -> Option<PathBuf> {
    let entries = std::fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() && npm_package_name(&path).as_deref() == Some(name) {
            return Some(path);
        }
    }
    None
}

// ── Shared ──────────────────────────────────────────────────────────────────

/// Resolve `relative` against `base`, and normalise `..` textually.
///
/// `std::fs::canonicalize` is deliberately not used: it fails on a path that
/// does not exist, and a manifest that points at a missing sibling should
/// produce a dependency this module reports and the *caller* fails to index
/// with a message naming the path — not a silently dropped edge.
fn absolutise(base: &Path, relative: &str) -> PathBuf {
    let joined = base.join(relative);
    let mut out = PathBuf::new();
    for component in joined.components() {
        match component {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write(root: &Path, rel: &str, contents: &str) {
        let path = root.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create fixture dir");
        }
        fs::write(path, contents).expect("write fixture file");
    }

    #[test]
    fn a_cargo_manifest_names_rust() {
        let dir = tempfile::tempdir().expect("temp dir");
        write(dir.path(), "Cargo.toml", "[package]\nname = \"x\"\n");
        assert_eq!(language_at(dir.path()), Some(ProducerLanguage::Rust));
    }

    #[test]
    fn a_package_json_names_typescript() {
        let dir = tempfile::tempdir().expect("temp dir");
        write(dir.path(), "package.json", "{\"name\":\"x\"}");
        assert_eq!(language_at(dir.path()), Some(ProducerLanguage::TypeScript));
    }

    /// A csproj is found by extension, not by a fixed filename.
    #[test]
    fn a_csproj_names_csharp() {
        let dir = tempfile::tempdir().expect("temp dir");
        write(dir.path(), "Whatever.csproj", "<Project/>");
        assert_eq!(language_at(dir.path()), Some(ProducerLanguage::CSharp));
    }

    /// A directory that is not a package root is not an error.
    #[test]
    fn a_directory_with_no_manifest_has_no_language() {
        let dir = tempfile::tempdir().expect("temp dir");
        assert_eq!(language_at(dir.path()), None);
    }

    /// The point of the whole module: a path dependency is local.
    #[test]
    fn a_cargo_path_dependency_is_local() {
        let dir = tempfile::tempdir().expect("temp dir");
        write(
            dir.path(),
            "app/Cargo.toml",
            "[package]\nname = \"app\"\n\n[dependencies]\nlib = { path = \"../lib\" }\n",
        );
        let DependencyScan::Read(found) =
            declared_dependencies(&dir.path().join("app"), ProducerLanguage::Rust)
        else {
            panic!("a readable manifest must scan as Read");
        };
        assert_eq!(
            found,
            vec![Dependency::Local {
                name: "lib".to_owned(),
                root: dir.path().join("lib"),
            }],
            "`../lib` must resolve to a sibling of the depending package",
        );
    }

    #[test]
    fn a_cargo_registry_dependency_keeps_its_requirement_verbatim() {
        let dir = tempfile::tempdir().expect("temp dir");
        write(
            dir.path(),
            "Cargo.toml",
            "[package]\nname = \"app\"\n\n[dependencies]\nserde = \"1.0.196\"\n",
        );
        let DependencyScan::Read(found) = declared_dependencies(dir.path(), ProducerLanguage::Rust)
        else {
            panic!("a readable manifest must scan as Read");
        };
        assert_eq!(
            found,
            vec![Dependency::Registry {
                name: "serde".to_owned(),
                requirement: "1.0.196".to_owned(),
            }],
        );
    }

    /// dev-dependencies are not the package's API surface.
    #[test]
    fn dev_dependencies_are_not_read() {
        let dir = tempfile::tempdir().expect("temp dir");
        write(
            dir.path(),
            "Cargo.toml",
            "[package]\nname = \"app\"\n\n[dev-dependencies]\ncriterion = \"0.5\"\n",
        );
        let DependencyScan::Read(found) = declared_dependencies(dir.path(), ProducerLanguage::Rust)
        else {
            panic!("a readable manifest must scan as Read");
        };
        assert!(found.is_empty(), "got {found:?}");
    }

    #[test]
    fn an_npm_file_dependency_is_local() {
        let dir = tempfile::tempdir().expect("temp dir");
        write(
            dir.path(),
            "app/package.json",
            "{\"name\":\"app\",\"dependencies\":{\"lib\":\"file:../lib\"}}",
        );
        let DependencyScan::Read(found) =
            declared_dependencies(&dir.path().join("app"), ProducerLanguage::TypeScript)
        else {
            panic!("a readable manifest must scan as Read");
        };
        assert_eq!(
            found,
            vec![Dependency::Local {
                name: "lib".to_owned(),
                root: dir.path().join("lib"),
            }],
        );
    }

    /// `workspace:*` names no path, so the sibling has to be found.
    #[test]
    fn a_workspace_protocol_dependency_finds_its_sibling() {
        let dir = tempfile::tempdir().expect("temp dir");
        write(
            dir.path(),
            "packages/app/package.json",
            "{\"name\":\"app\",\"dependencies\":{\"@scope/lib\":\"workspace:*\"}}",
        );
        write(
            dir.path(),
            "packages/lib/package.json",
            "{\"name\":\"@scope/lib\"}",
        );

        let DependencyScan::Read(found) = declared_dependencies(
            &dir.path().join("packages/app"),
            ProducerLanguage::TypeScript,
        ) else {
            panic!("a readable manifest must scan as Read");
        };
        assert_eq!(
            found,
            vec![Dependency::Local {
                name: "@scope/lib".to_owned(),
                root: dir.path().join("packages/lib"),
            }],
            "the sibling declaring that name is the dependency, wherever it sits",
        );
    }

    /// An installed copy is not the checkout the user is editing.
    #[test]
    fn node_modules_is_never_preferred_to_a_sibling() {
        let dir = tempfile::tempdir().expect("temp dir");
        write(
            dir.path(),
            "packages/app/package.json",
            "{\"name\":\"app\",\"dependencies\":{\"lib\":\"workspace:*\"}}",
        );
        write(
            dir.path(),
            "packages/node_modules/lib/package.json",
            "{\"name\":\"lib\"}",
        );
        write(
            dir.path(),
            "packages/lib/package.json",
            "{\"name\":\"lib\"}",
        );

        let DependencyScan::Read(found) = declared_dependencies(
            &dir.path().join("packages/app"),
            ProducerLanguage::TypeScript,
        ) else {
            panic!("a readable manifest must scan as Read");
        };
        assert_eq!(
            found,
            vec![Dependency::Local {
                name: "lib".to_owned(),
                root: dir.path().join("packages/lib"),
            }],
        );
    }

    /// The honesty rule: an unread format must not look like "no dependencies".
    #[test]
    fn an_unread_manifest_format_says_so_rather_than_returning_nothing() {
        let dir = tempfile::tempdir().expect("temp dir");
        write(dir.path(), "go.mod", "module example.com/x\n");

        let scan = declared_dependencies(dir.path(), ProducerLanguage::Go);
        assert!(
            matches!(scan, DependencyScan::Unsupported { .. }),
            "an empty Read here would claim the package is standalone: {scan:?}",
        );
    }

    #[test]
    fn a_corrupt_manifest_is_reported_as_corrupt() {
        let dir = tempfile::tempdir().expect("temp dir");
        write(dir.path(), "Cargo.toml", "this is not toml [[[");

        let scan = declared_dependencies(dir.path(), ProducerLanguage::Rust);
        assert!(
            matches!(scan, DependencyScan::Malformed { .. }),
            "a parse failure must not read as an empty dependency set: {scan:?}",
        );
    }

    #[test]
    fn a_missing_manifest_is_reported_as_missing() {
        let dir = tempfile::tempdir().expect("temp dir");
        let scan = declared_dependencies(dir.path(), ProducerLanguage::Rust);
        assert!(
            matches!(scan, DependencyScan::NoManifest { .. }),
            "got {scan:?}",
        );
    }

    /// A dependency pointing at a directory that does not exist is still
    /// reported. The caller fails to index it, naming the path; dropping it
    /// here would make a typo in a manifest invisible.
    #[test]
    fn a_path_dependency_that_does_not_exist_is_still_reported() {
        let dir = tempfile::tempdir().expect("temp dir");
        write(
            dir.path(),
            "Cargo.toml",
            "[package]\nname = \"app\"\n\n[dependencies]\ngone = { path = \"../gone\" }\n",
        );
        let DependencyScan::Read(found) = declared_dependencies(dir.path(), ProducerLanguage::Rust)
        else {
            panic!("a readable manifest must scan as Read");
        };
        assert_eq!(found.len(), 1, "got {found:?}");
        assert!(matches!(found[0], Dependency::Local { .. }));
    }

    #[test]
    fn a_cargo_name_is_read_from_the_manifest_not_the_directory() {
        let dir = tempfile::tempdir().expect("temp dir");
        write(
            dir.path(),
            "checkout/Cargo.toml",
            "[package]\nname = \"real-name\"\n",
        );
        assert_eq!(
            name_at(&dir.path().join("checkout"), ProducerLanguage::Rust),
            "real-name",
        );
    }
}
