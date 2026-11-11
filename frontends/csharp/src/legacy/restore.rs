//! Bounded reader for a restored C# `project.assets.json`.
//!
//! This deliberately reads the restore result only. It never runs MSBuild,
//! evaluates package targets, or scans a NuGet directory: every assembly is an
//! explicit compile asset selected by the restore graph.
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{fs, path::{Path, PathBuf}};

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
#[derive(Debug, Clone)]
pub struct RestoreClosure {
    pub files: Vec<PathBuf>,
    pub digest: [u8; 32],
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
    let metadata = fs::metadata(assets_path).map_err(|e| RestoreError::Unavailable(format!("{} ({e})", assets_path.display())))?;
    if !metadata.is_file() || metadata.len() > MAX_BYTES { return Err(RestoreError::Limit); }
    let bytes = fs::read(assets_path).map_err(|e| RestoreError::Unavailable(e.to_string()))?;
    let root = fs::canonicalize(project_root).map_err(|e| RestoreError::Unavailable(format!("{} ({e})", project_root.display())))?;
    let value: Value = serde_json::from_slice(&bytes)?;
    let folders = value.get("packageFolders").and_then(Value::as_object).ok_or(RestoreError::Missing("packageFolders"))?;
    let targets = value.get("targets").and_then(Value::as_object).ok_or(RestoreError::Missing("targets"))?;
    let target_name = targets.keys().find(|name| name.split('/').next() == Some(target_framework)).ok_or(RestoreError::Missing("targets for target framework"))?;
    let target = targets.get(target_name).and_then(Value::as_object).ok_or(RestoreError::Missing("target graph"))?;
    let libraries = value.get("libraries").and_then(Value::as_object).ok_or(RestoreError::Missing("libraries"))?;
    let mut files = Vec::new();
    for (name, version) in expected_packages {
        let found = target.keys().any(|identity| identity.split_once('/').is_some_and(|(n, v)| n.eq_ignore_ascii_case(name) && v == *version));
        if !found { return Err(RestoreError::Unavailable(format!("package reference {name}@{version} is absent from restore graph"))); }
    }
    for project in expected_projects {
        let found = target.iter().any(|(identity, item)| {
            item.get("compile").is_some() && libraries.get(identity).and_then(Value::as_object).and_then(|lib| lib.get("type")).and_then(Value::as_str) == Some("project") && libraries.get(identity).and_then(Value::as_object).and_then(|lib| lib.get("path")).and_then(Value::as_str).is_some_and(|path| path == *project || path.ends_with(project))
        });
        if !found { return Err(RestoreError::Unavailable(format!("project reference {project} is absent from restore graph"))); }
    }
    for (identity, item) in target {
        let compile = item.get("compile").and_then(Value::as_object);
        let Some(compile) = compile else { continue };
        let library = libraries.get(identity).and_then(Value::as_object).ok_or_else(|| RestoreError::Unavailable(format!("library {identity}")))?;
        let kind = library.get("type").and_then(Value::as_str).unwrap_or("package");
        let relative_base = library.get("path").and_then(Value::as_str).ok_or_else(|| RestoreError::Unavailable(format!("path for {identity}")))?;
        for (relative, present) in compile {
            if present.is_null() { continue; }
            if files.len() >= MAX_FILES { return Err(RestoreError::Limit); }
            let path = if kind == "project" {
                // Project libraries are rooted at the restore root. Their
                // compile asset is generally bin/<config>/<tfm>/<name>.dll.
                root.join(relative_base).join(relative)
            } else {
                let folder = folders.keys().next().ok_or(RestoreError::Missing("package folder"))?;
                let folder = checked_path(folder)?;
                PathBuf::from(folder).join(relative_base).join(relative)
            };
            let canonical = fs::canonicalize(&path).map_err(|e| RestoreError::Unavailable(format!("{} ({e})", path.display())))?;
            if !canonical.starts_with(&root) && kind == "project" { return Err(RestoreError::OutsideRoot(path.display().to_string())); }
            if !canonical.is_file() { return Err(RestoreError::Unavailable(canonical.display().to_string())); }
            files.push(canonical);
        }
    }
    files.sort();
    files.dedup();
    let mut digest = Sha256::new();
    for file in &files {
        let rel = file.strip_prefix(&root).unwrap_or(file);
        digest.update(rel.to_string_lossy().as_bytes());
        digest.update([0]);
        let content = fs::read(file).map_err(|e| RestoreError::Unavailable(e.to_string()))?;
        if content.len() as u64 > MAX_BYTES { return Err(RestoreError::Limit); }
        digest.update((content.len() as u64).to_le_bytes());
        digest.update(content);
    }
    Ok(RestoreClosure { files, digest: digest.finalize().into() })
}

fn checked_path(path: &str) -> Result<&Path, RestoreError> {
    let p = Path::new(path);
    if path.is_empty() || path.contains(':') || path.contains('\\') {
        return Err(RestoreError::ForeignPath(path.to_owned()));
    }
    Ok(p)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    #[test]
    fn rejects_windows_package_folder() {
        let dir = tempfile::tempdir().unwrap();
        let assets = dir.path().join("project.assets.json");
        fs::write(&assets, r#"{"packageFolders":{"C:\\Users\\me\\.nuget\\packages\\":""},"targets":{"net10.0":{"P/1.0":{"compile":{"x.dll":{}}}}},"libraries":{"P/1.0":{"type":"package","path":"p/1.0"}}}"#).unwrap();
        assert!(matches!(resolve_project_assets(&assets, dir.path(), "net10.0"), Err(RestoreError::ForeignPath(_))));
    }
}
