//! Bounded reader for a restored C# `project.assets.json`.
//!
//! This deliberately reads the restore result only. It never runs MSBuild,
//! evaluates package targets, or scans a NuGet directory: every assembly is an
//! explicit compile asset selected by the restore graph.
use backend_platform::durable::read_regular_bounded_stable;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeSet, HashSet},
    fs,
    path::{Component, Path, PathBuf},
};

const MAX_BYTES: u64 = 64 * 1024 * 1024;
const MAX_FILES: usize = 4096;

#[derive(Debug, thiserror::Error)]
pub enum RestoreError {
    #[error("project.assets.json is not valid JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("project.assets.json is missing {0}")]
    Missing(&'static str),
    #[error("project.assets.json contains a path from another operating system: {0}")]
    ForeignPath(String),
    #[error("restored reference path escapes the package root: {0}")]
    OutsideRoot(String),
    #[error("restored reference is unavailable: {0}")]
    Unavailable(String),
    #[error("project.assets.json exceeds the bounded reference budget")]
    Limit,
}

/// Exact compile assemblies selected by one restore graph. The digest covers
/// each relative path and its bytes in sorted order and is safe to include in
/// an attempt-local compiler identity.
#[derive(Debug)]
pub struct RestoreClosure {
    pub files: Vec<PathBuf>,
    pub digest: [u8; 32],
    directory: tempfile::TempDir,
}

impl RestoreClosure {
    /// Private flattened directory containing exactly the admitted assemblies.
    /// Keeping this closure alive keeps the snapshot alive through compilation.
    pub fn reference_directory(&self) -> &Path {
        self.directory.path()
    }
}

pub fn resolve_project_assets(
    assets_path: &Path,
    project_root: &Path,
    target_framework: &str,
) -> Result<RestoreClosure, RestoreError> {
    resolve_project_assets_checked(assets_path, project_root, target_framework, &[], &[])
}

/// Variant used by the project capture path. Literal dependency declarations
/// are checked against the selected restore target so a stale assets file
/// cannot silently stand in for the current project.
pub fn resolve_project_assets_checked(
    assets_path: &Path,
    project_root: &Path,
    target_framework: &str,
    expected_packages: &[(&str, &str)],
    expected_projects: &[&str],
) -> Result<RestoreClosure, RestoreError> {
    resolve_project_assets_with_metadata(
        assets_path,
        project_root,
        target_framework,
        expected_packages,
        expected_projects,
        &[],
    )
}

/// Checked resolver with authored IncludeAssets declarations. A package that
/// excludes `compile` must never contribute a compile assembly from stale
/// restore output.
pub fn resolve_project_assets_with_metadata(
    assets_path: &Path,
    project_root: &Path,
    target_framework: &str,
    expected_packages: &[(&str, &str)],
    expected_projects: &[&str],
    expected_include_assets: &[(&str, &str)],
) -> Result<RestoreClosure, RestoreError> {
    let metadata = fs::metadata(assets_path)
        .map_err(|e| RestoreError::Unavailable(format!("{} ({e})", assets_path.display())))?;
    if !metadata.is_file() || metadata.len() > MAX_BYTES {
        return Err(RestoreError::Limit);
    }
    let bytes = read_bounded(assets_path, MAX_BYTES)?;
    let root = fs::canonicalize(project_root)
        .map_err(|e| RestoreError::Unavailable(format!("{} ({e})", project_root.display())))?;
    let project_dir = assets_path
        .parent()
        .and_then(Path::parent)
        .map(fs::canonicalize)
        .transpose()
        .map_err(|e| RestoreError::Unavailable(e.to_string()))?
        .unwrap_or_else(|| root.clone());
    let value: Value = serde_json::from_slice(&bytes)?;
    let folders = value
        .get("packageFolders")
        .and_then(Value::as_object)
        .ok_or(RestoreError::Missing("packageFolders"))?;
    let targets = value
        .get("targets")
        .and_then(Value::as_object)
        .ok_or(RestoreError::Missing("targets"))?;
    // The RID-independent compile graph is required; choosing the first RID
    // graph would silently change a project's compile inputs.
    let target = targets
        .get(target_framework)
        .and_then(Value::as_object)
        .ok_or(RestoreError::Missing("target graph"))?;
    let libraries = value
        .get("libraries")
        .and_then(Value::as_object)
        .ok_or(RestoreError::Missing("libraries"))?;
    let mut files = Vec::new();
    if !expected_packages.is_empty() || !expected_projects.is_empty() {
        let dependencies = value
            .get("project")
            .and_then(|project| project.get("frameworks"))
            .and_then(|frameworks| frameworks.get(target_framework))
            .and_then(|framework| framework.get("dependencies"))
            .and_then(Value::as_object);
        let actual = dependencies
            .map(|dependencies| {
                dependencies
                    .keys()
                    .map(|name| name.to_ascii_lowercase())
                    .collect::<BTreeSet<_>>()
            })
            .unwrap_or_default();
        let expected = expected_packages
            .iter()
            .map(|(name, _)| name.to_ascii_lowercase())
            .collect::<BTreeSet<_>>();
        if actual != expected {
            return Err(RestoreError::Unavailable("direct package dependencies changed; run dotnet restore for the current project on this operating system".to_owned()));
        }
        for (name, _) in expected_packages {
            let dependency = dependencies
                .and_then(|dependencies| {
                    dependencies
                        .iter()
                        .find(|(key, _)| key.eq_ignore_ascii_case(name))
                        .map(|(_, value)| value)
                })
                .ok_or(RestoreError::Missing("direct package metadata"))?;
            let authored = expected_include_assets
                .iter()
                .find(|(id, _)| id.eq_ignore_ascii_case(name))
                .map(|(_, include)| *include)
                .unwrap_or("all");
            let restored = dependency
                .get("include")
                .and_then(Value::as_str)
                .unwrap_or("all");
            if asset_tokens(authored) != asset_tokens(restored) {
                return Err(RestoreError::Unavailable(format!(
                    "package {name} IncludeAssets changed; run dotnet restore for the current project"
                )));
            }
        }
    }
    for (name, version) in expected_packages {
        let found = target.keys().any(|identity| {
            identity
                .split_once('/')
                .is_some_and(|(n, v)| n.eq_ignore_ascii_case(name) && v == *version)
        });
        if !found {
            return Err(RestoreError::Unavailable(format!(
                "package reference {name}@{version} is absent from restore graph"
            )));
        }
    }
    for (name, include) in expected_include_assets {
        let excludes_compile = !include.split(';').any(|token| {
            matches!(
                token.trim().to_ascii_lowercase().as_str(),
                "compile" | "all"
            )
        });
        if excludes_compile {
            let has_compile = target.iter().any(|(identity, item)| {
                identity
                    .split_once('/')
                    .is_some_and(|(n, _)| n.eq_ignore_ascii_case(name))
                    && item.get("compile").is_some_and(|v| {
                        v.as_object().is_some_and(|o| {
                            o.keys()
                                .any(|path| !path.ends_with("/_._") && path != "_._")
                        })
                    })
            });
            if has_compile {
                return Err(RestoreError::Unavailable(format!(
                    "package {name} has compile assets despite IncludeAssets={include}"
                )));
            }
        }
    }
    for project in expected_projects {
        let found = target.iter().any(|(identity, item)| {
            item.get("compile").is_some()
                && libraries
                    .get(identity)
                    .and_then(Value::as_object)
                    .and_then(|lib| lib.get("type"))
                    .and_then(Value::as_str)
                    == Some("project")
                && libraries
                    .get(identity)
                    .and_then(Value::as_object)
                    .and_then(|lib| lib.get("path"))
                    .and_then(Value::as_str)
                    .is_some_and(|path| project_ref_matches(path, project))
        });
        if !found {
            return Err(RestoreError::Unavailable(format!(
                "project reference {project} is absent from restore graph"
            )));
        }
    }
    for (identity, item) in target {
        let compile = item.get("compile").and_then(Value::as_object);
        let Some(compile) = compile else { continue };
        let library = libraries
            .get(identity)
            .and_then(Value::as_object)
            .ok_or_else(|| RestoreError::Unavailable(format!("library {identity}")))?;
        let kind = library
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("package");
        let relative_base = library
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| RestoreError::Unavailable(format!("path for {identity}")))?;
        for (relative, present) in compile {
            if present.is_null()
                || Path::new(relative)
                    .file_name()
                    .is_some_and(|name| name == "_._")
            {
                continue;
            }
            if files.len() >= MAX_FILES {
                return Err(RestoreError::Limit);
            }
            checked_relative(relative)?;
            if kind != "project" {
                checked_relative(relative_base)?;
            }
            let path = if kind == "project" {
                // Project libraries are rooted at the restore root. Their
                // compile asset is generally bin/<config>/<tfm>/<name>.dll.
                let project_base = if relative_base.ends_with(".csproj") {
                    Path::new(relative_base).parent().unwrap_or(Path::new(""))
                } else {
                    Path::new(relative_base)
                };
                let referenced_project = project_dir.join(relative_base);
                let canonical_project = fs::canonicalize(&referenced_project).map_err(|e| {
                    RestoreError::Unavailable(format!(
                        "referenced project {} ({e})",
                        referenced_project.display()
                    ))
                })?;
                if !canonical_project.starts_with(&root) {
                    return Err(RestoreError::OutsideRoot(
                        referenced_project.display().to_string(),
                    ));
                }
                let base = project_dir.join(project_base);
                if relative.starts_with("bin/placeholder/") {
                    let name = Path::new(relative)
                        .file_name()
                        .ok_or(RestoreError::Missing("project reference assembly filename"))?;
                    let framework = item
                        .get("framework")
                        .and_then(Value::as_str)
                        .unwrap_or(target_framework);
                    checked_relative(framework)?;
                    let reference = base
                        .join("obj/Debug")
                        .join(framework)
                        .join("ref")
                        .join(name);
                    let implementation = base.join("bin/Debug").join(framework).join(name);
                    if reference.is_file() {
                        reference
                    } else if implementation.is_file() {
                        implementation
                    } else {
                        return Err(RestoreError::Unavailable(format!(
                            "project reference {} has restore metadata but no built Debug reference assembly; build the referenced project with the selected dotnet SDK before indexing",
                            referenced_project.display()
                        )));
                    }
                } else {
                    base.join(relative)
                }
            } else {
                let mut found = None;
                for folder in folders.keys() {
                    let folder = checked_path(folder)?;
                    let candidate = PathBuf::from(folder).join(relative_base).join(relative);
                    if candidate.is_file() {
                        let folder = fs::canonicalize(folder)
                            .map_err(|e| RestoreError::Unavailable(e.to_string()))?;
                        let canonical = fs::canonicalize(&candidate)
                            .map_err(|e| RestoreError::Unavailable(e.to_string()))?;
                        if !canonical.starts_with(&folder) {
                            return Err(RestoreError::OutsideRoot(candidate.display().to_string()));
                        }
                        found = Some(canonical);
                        break;
                    }
                }
                found.ok_or_else(|| {
                    RestoreError::Unavailable(format!("package asset {identity}/{relative}"))
                })?
            };
            let canonical = fs::canonicalize(&path)
                .map_err(|e| RestoreError::Unavailable(format!("{} ({e})", path.display())))?;
            if !canonical.starts_with(&root) && kind == "project" {
                return Err(RestoreError::OutsideRoot(path.display().to_string()));
            }
            if !canonical.is_file() {
                return Err(RestoreError::Unavailable(canonical.display().to_string()));
            }
            files.push(canonical);
        }
    }
    files.sort();
    files.dedup();
    let directory = tempfile::tempdir().map_err(|e| RestoreError::Unavailable(e.to_string()))?;
    let mut names = HashSet::new();
    let mut snapshot = Vec::with_capacity(files.len());
    let mut total_bytes = bytes.len() as u64;
    for source in &files {
        let name = source
            .file_name()
            .ok_or_else(|| RestoreError::Unavailable(source.display().to_string()))?
            .to_owned();
        if !names.insert(name.to_string_lossy().to_ascii_lowercase()) {
            return Err(RestoreError::Unavailable(format!(
                "duplicate assembly filename {}",
                name.to_string_lossy()
            )));
        }
        let destination = directory.path().join(&name);
        let content = read_bounded(source, 32 * 1024 * 1024)?;
        total_bytes = total_bytes
            .checked_add(content.len() as u64)
            .ok_or(RestoreError::Limit)?;
        if total_bytes > 256 * 1024 * 1024 {
            return Err(RestoreError::Limit);
        }
        fs::write(&destination, &content).map_err(|e| RestoreError::Unavailable(e.to_string()))?;
        snapshot.push(destination);
    }
    files = snapshot;
    let mut digest = Sha256::new();
    digest.update((bytes.len() as u64).to_le_bytes());
    digest.update(&bytes);
    for file in &files {
        let rel = file.file_name().unwrap_or_default();
        digest.update(rel.to_string_lossy().as_bytes());
        digest.update([0]);
        let content = read_bounded(file, MAX_BYTES)?;
        if content.len() as u64 > MAX_BYTES {
            return Err(RestoreError::Limit);
        }
        digest.update((content.len() as u64).to_le_bytes());
        digest.update(content);
    }
    Ok(RestoreClosure {
        files,
        digest: digest.finalize().into(),
        directory,
    })
}

fn asset_tokens(value: &str) -> BTreeSet<String> {
    let tokens = value
        .split([',', ';'])
        .map(|token| token.trim().to_ascii_lowercase())
        .filter(|token| !token.is_empty() && token != "none")
        .collect::<BTreeSet<_>>();
    if tokens.contains("all") {
        [
            "runtime",
            "compile",
            "build",
            "native",
            "contentfiles",
            "analyzers",
            "buildtransitive",
            "buildmultitargeting",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect()
    } else {
        tokens
    }
}

fn checked_path(path: &str) -> Result<&Path, RestoreError> {
    let p = Path::new(path);
    if path.is_empty()
        || !p.is_absolute()
        || (!cfg!(windows) && (path.contains(':') || path.contains('\\')))
    {
        return Err(RestoreError::ForeignPath(path.to_owned()));
    }
    Ok(p)
}

fn checked_relative(path: &str) -> Result<(), RestoreError> {
    let p = Path::new(path);
    if p.is_absolute()
        || p.components().any(|c| {
            matches!(
                c,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(RestoreError::OutsideRoot(path.to_owned()));
    }
    Ok(())
}

fn project_ref_matches(path: &str, expected: &str) -> bool {
    let normalize = |value: &str| {
        value
            .replace('\\', "/")
            .trim_end_matches(".csproj")
            .trim_matches('/')
            .to_owned()
    };
    let actual = normalize(path);
    let expected = normalize(expected);
    actual == expected || actual.ends_with(&format!("/{expected}"))
}

fn read_bounded(path: &Path, limit: u64) -> Result<Vec<u8>, RestoreError> {
    read_regular_bounded_stable(path, limit as usize)
        .map_err(|e| RestoreError::Unavailable(format!("{} ({e})", path.display())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    #[cfg(not(windows))]
    #[test]
    fn rejects_windows_package_folder() {
        let dir = tempfile::tempdir().unwrap();
        let assets = dir.path().join("project.assets.json");
        fs::write(&assets, r#"{"packageFolders":{"C:\\Users\\me\\.nuget\\packages\\":""},"targets":{"net10.0":{"P/1.0":{"compile":{"x.dll":{}}}}},"libraries":{"P/1.0":{"type":"package","path":"p/1.0"}}}"#).unwrap();
        assert!(matches!(
            resolve_project_assets(&assets, dir.path(), "net10.0"),
            Err(RestoreError::ForeignPath(_))
        ));
    }

    #[test]
    fn snapshots_package_assets_with_stable_digest() {
        let dir = tempfile::tempdir().unwrap();
        let package = dir.path().join("packages/p/1.0");
        fs::create_dir_all(&package).unwrap();
        fs::write(package.join("p.dll"), b"assembly").unwrap();
        let assets = dir.path().join("project.assets.json");
        fs::write(&assets, format!(r#"{{"packageFolders":{{"{}":{{}}}},"targets":{{"net10.0":{{"P/1.0":{{"compile":{{"p.dll":{{}}}}}}}}}},"libraries":{{"P/1.0":{{"type":"package","path":"p/1.0"}}}}}}"#, dir.path().join("packages").display())).unwrap();
        let first = resolve_project_assets(&assets, dir.path(), "net10.0").unwrap();
        let second = resolve_project_assets(&assets, dir.path(), "net10.0").unwrap();
        assert_eq!(first.digest, second.digest);
        assert_eq!(
            fs::read(first.reference_directory().join("p.dll")).unwrap(),
            b"assembly"
        );
        fs::write(package.join("p.dll"), b"changed").unwrap();
        assert_eq!(
            fs::read(first.reference_directory().join("p.dll")).unwrap(),
            b"assembly"
        );
    }

    #[test]
    fn rejects_compile_traversal() {
        let dir = tempfile::tempdir().unwrap();
        let assets = dir.path().join("project.assets.json");
        fs::write(&assets, r#"{"packageFolders":{"/tmp":""},"targets":{"net10.0":{"P/1.0":{"compile":{"../x.dll":{}}}}},"libraries":{"P/1.0":{"type":"package","path":"p/1.0"}}}"#).unwrap();
        assert!(matches!(
            resolve_project_assets(&assets, dir.path(), "net10.0"),
            Err(RestoreError::OutsideRoot(_))
        ));
    }
}

#[cfg(test)]
mod closure_regressions {
    use super::*;
    use serde_json::json;

    fn package_fixture(root: &Path) -> PathBuf {
        let package = root.join("packages/p/1.0/ref/net10.0");
        fs::create_dir_all(&package).unwrap();
        fs::write(package.join("P.dll"), b"reference bytes").unwrap();
        let assets = root.join("App/obj/project.assets.json");
        fs::create_dir_all(assets.parent().unwrap()).unwrap();
        let graph = json!({
            "packageFolders": { root.join("packages").to_str().unwrap(): {} },
            "targets": { "net10.0": { "P/1.0": {"compile": {"ref/net10.0/P.dll": {}}}}},
            "libraries": {"P/1.0": {"type":"package", "path":"p/1.0"}},
            "project": {"frameworks": {"net10.0": {"dependencies": {"P":{"target":"Package", "version":"[1.0, )"}}}}}
        });
        fs::write(&assets, serde_json::to_vec(&graph).unwrap()).unwrap();
        assets
    }

    #[test]
    fn exact_current_dependencies_are_required_before_binding() {
        let root = tempfile::tempdir().unwrap();
        let assets = package_fixture(root.path());
        let closure =
            resolve_project_assets_checked(&assets, root.path(), "net10.0", &[("P", "1.0")], &[])
                .unwrap();
        assert_eq!(
            fs::read(closure.reference_directory().join("P.dll")).unwrap(),
            b"reference bytes"
        );
        assert!(
            resolve_project_assets_checked(&assets, root.path(), "net10.0", &[("P", "2.0")], &[])
                .is_err()
        );
        assert!(
            resolve_project_assets_with_metadata(
                &assets,
                root.path(),
                "net10.0",
                &[("P", "1.0")],
                &[],
                &[("P", "runtime")]
            )
            .is_err()
        );
        assert!(
            resolve_project_assets_checked(&assets, root.path(), "net9.0", &[("P", "1.0")], &[])
                .is_err()
        );
    }

    #[test]
    fn project_restore_placeholder_requires_real_reference_output() {
        let root = tempfile::tempdir().unwrap();
        let assets = root.path().join("Tests/obj/project.assets.json");
        fs::create_dir_all(assets.parent().unwrap()).unwrap();
        let api = root.path().join("Api");
        fs::create_dir_all(api.join("obj/Debug/net10.0/ref")).unwrap();
        fs::write(api.join("Api.csproj"), "<Project/>").unwrap();
        let graph = json!({
            "packageFolders": {},
            "targets": {"net10.0": {"Api/1.0": {"type":"project", "framework":"net10.0", "compile":{"bin/placeholder/Api.dll":{}}}}},
            "libraries": {"Api/1.0":{"type":"project", "path":"../Api/Api.csproj"}}
        });
        fs::write(&assets, serde_json::to_vec(&graph).unwrap()).unwrap();
        let failure = resolve_project_assets_checked(
            &assets,
            root.path(),
            "net10.0",
            &[],
            &["../Api/Api.csproj"],
        )
        .unwrap_err();
        assert!(
            failure
                .to_string()
                .contains("no built Debug reference assembly"),
            "{failure}"
        );
        fs::write(
            api.join("obj/Debug/net10.0/ref/Api.dll"),
            b"project reference",
        )
        .unwrap();
        let closure = resolve_project_assets_checked(
            &assets,
            root.path(),
            "net10.0",
            &[],
            &["../Api/Api.csproj"],
        )
        .unwrap();
        assert_eq!(
            fs::read(closure.reference_directory().join("Api.dll")).unwrap(),
            b"project reference"
        );
    }
}
