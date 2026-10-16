//! Captured config loading and embedded-library controls.

use super::super::*;
use crate::resolution::filesystem::{
    ResolutionDirEntry, ResolutionFileKind, ResolutionFileSystem,
};
use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::RwLock;

#[derive(Default)]
struct MemoryFileSystem {
    boundary: PathBuf,
    files: RwLock<HashMap<PathBuf, Vec<u8>>>,
    directories: RwLock<HashSet<PathBuf>>,
    denied: RwLock<HashSet<PathBuf>>,
    accesses: RwLock<Vec<PathBuf>>,
}

impl MemoryFileSystem {
    fn new(boundary: &str) -> Self {
        let mut this = Self::default();
        this.boundary = PathBuf::from(boundary);
        this.directories.write().unwrap().insert(this.boundary.clone());
        this
    }

    fn add_dir(&self, path: impl Into<PathBuf>) {
        self.directories.write().unwrap().insert(path.into());
    }

    fn add_file(&self, path: impl Into<PathBuf>, source: &str) {
        self.files
            .write()
            .unwrap()
            .insert(path.into(), source.as_bytes().to_vec());
    }

    fn deny(&self, path: impl Into<PathBuf>) {
        self.denied.write().unwrap().insert(path.into());
    }

    fn check(&self, path: &Path) -> io::Result<()> {
        self.accesses.write().unwrap().push(path.to_path_buf());
        if !path.starts_with(&self.boundary) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("captured path outside boundary: {}", path.display()),
            ));
        }
        if self.denied.read().unwrap().contains(path) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("captured path denied: {}", path.display()),
            ));
        }
        Ok(())
    }

    fn exists(&self, path: &Path) -> bool {
        self.files.read().unwrap().contains_key(path)
            || self.directories.read().unwrap().contains(path)
    }
}

impl ResolutionFileSystem for MemoryFileSystem {
    fn resolution_boundary(&self) -> Option<&Path> {
        Some(&self.boundary)
    }

    fn read_file(&self, path: &Path) -> io::Result<Option<Vec<u8>>> {
        self.check(path)?;
        Ok(self.files.read().unwrap().get(path).cloned())
    }

    fn is_file(&self, path: &Path) -> io::Result<bool> {
        self.check(path)?;
        Ok(self.files.read().unwrap().contains_key(path))
    }

    fn is_dir(&self, path: &Path) -> io::Result<bool> {
        self.check(path)?;
        Ok(self.directories.read().unwrap().contains(path))
    }

    fn read_dir(&self, path: &Path) -> io::Result<Option<Vec<ResolutionDirEntry>>> {
        self.check(path)?;
        if !self.directories.read().unwrap().contains(path) {
            return Ok(None);
        }
        let mut entries = HashMap::<OsString, ResolutionFileKind>::new();
        for file in self.files.read().unwrap().keys() {
            if file.parent() == Some(path) {
                if let Some(name) = file.file_name() {
                    entries.insert(name.to_os_string(), ResolutionFileKind::File);
                }
            }
        }
        for directory in self.directories.read().unwrap().iter() {
            if directory.parent() == Some(path) {
                if let Some(name) = directory.file_name() {
                    entries.insert(name.to_os_string(), ResolutionFileKind::Directory);
                }
            }
        }
        Ok(Some(
            entries
                .into_iter()
                .map(|(name, kind)| ResolutionDirEntry { name, kind })
                .collect(),
        ))
    }

    fn canonicalize(&self, path: &Path) -> io::Result<Option<PathBuf>> {
        self.check(path)?;
        Ok(self.exists(path).then(|| path.to_path_buf()))
    }

    fn symlink_metadata(&self, path: &Path) -> io::Result<Option<ResolutionFileKind>> {
        self.check(path)?;
        if self.files.read().unwrap().contains_key(path) {
            return Ok(Some(ResolutionFileKind::File));
        }
        if self.directories.read().unwrap().contains(path) {
            return Ok(Some(ResolutionFileKind::Directory));
        }
        Ok(None)
    }
}

#[test]
fn captured_config_loader_resolves_relative_and_exported_package_extends() {
    let fs = MemoryFileSystem::new("/virtual/workspace");
    for directory in [
        "/virtual/workspace/project",
        "/virtual/workspace/project/node_modules",
        "/virtual/workspace/project/node_modules/shared",
    ] {
        fs.add_dir(directory);
    }
    fs.add_file(
        "/virtual/workspace/project/tsconfig.json",
        r#"{"extends":["./base","shared"],"compilerOptions":{"strict":true}}"#,
    );
    fs.add_file(
        "/virtual/workspace/project/base.json",
        r#"{"compilerOptions":{"target":"ES2020"}}"#,
    );
    fs.add_file(
        "/virtual/workspace/project/node_modules/shared/package.json",
        r#"{"exports":{".":"./config.json"}}"#,
    );
    fs.add_file(
        "/virtual/workspace/project/node_modules/shared/config.json",
        r#"{"compilerOptions":{"target":"ES2022"}}"#,
    );

    let config = load_tsconfig_with_file_system(
        Path::new("/virtual/workspace/project/tsconfig.json"),
        &fs,
    )
    .expect("captured config and both bases should load");
    let options = config.compiler_options.expect("merged compiler options");

    assert_eq!(options.target.as_deref(), Some("ES2022"));
    assert_eq!(options.strict, Some(true));
    assert!(fs
        .accesses
        .read()
        .unwrap()
        .iter()
        .all(|path| path.starts_with("/virtual/workspace")));
}

#[test]
fn captured_config_loader_propagates_provider_refusals() {
    let fs = MemoryFileSystem::new("/virtual/workspace");
    fs.add_dir("/virtual/workspace/project");
    fs.add_file(
        "/virtual/workspace/project/tsconfig.json",
        r#"{"extends":"./base"}"#,
    );
    fs.add_file(
        "/virtual/workspace/project/base.json",
        r#"{"compilerOptions":{"target":"ES2020"}}"#,
    );
    fs.deny("/virtual/workspace/project/base.json");

    let error =
        load_tsconfig_with_file_system(Path::new("/virtual/workspace/project/tsconfig.json"), &fs)
            .expect_err("a provider refusal cannot become an absent extends target");
    assert!(
        error
            .downcast_ref::<io::Error>()
            .is_some_and(|error| error.kind() == io::ErrorKind::PermissionDenied)
    );

    let error = load_tsconfig_with_diagnostics_and_file_system(
        Path::new("/virtual/workspace/project/tsconfig.json"),
        &fs,
    )
    .err()
    .expect("diagnostic recovery cannot hide a provider refusal");
    assert!(
        error
            .downcast_ref::<io::Error>()
            .is_some_and(|error| error.kind() == io::ErrorKind::PermissionDenied)
    );
}

#[test]
fn captured_config_diagnostics_keep_base_selector_anchors_and_valid_inherited_roots() {
    let fs = MemoryFileSystem::new("/virtual/workspace");
    fs.add_dir("/virtual/workspace/project");
    fs.add_dir("/virtual/workspace/project/base");
    let base_source = r#"{/* π */ "include":["bad/**","valid/**/*"]}"#;
    fs.add_file("/virtual/workspace/project/base/options.json", base_source);
    fs.add_file(
        "/virtual/workspace/project/tsconfig.json",
        r#"{"extends":"./base/options.json","compilerOptions":{"noLib":true}}"#,
    );
    let parsed = load_tsconfig_with_diagnostics_and_file_system(
        Path::new("/virtual/workspace/project/tsconfig.json"),
        &fs,
    )
    .unwrap();
    assert_eq!(
        parsed.config.include,
        Some(vec!["/virtual/workspace/project/base/valid/**/*".into()])
    );
    assert_eq!(parsed.diagnostics.len(), 1);
    let diagnostic = &parsed.diagnostics[0];
    assert_eq!(diagnostic.code, 5010);
    assert_eq!(
        diagnostic.file,
        "/virtual/workspace/project/base/options.json"
    );
    assert_eq!(
        diagnostic.start as usize,
        base_source.find("\"bad/**\"").unwrap()
    );
    assert_eq!(diagnostic.length, "\"bad/**\"".len() as u32);
    assert!(
        fs.accesses
            .read()
            .unwrap()
            .iter()
            .all(|path| path.starts_with("/virtual/workspace"))
    );
}

#[test]
fn embedded_option_resolution_uses_only_linked_standard_libraries() {
    let defaults = resolve_compiler_options_embedded(None).expect("embedded defaults resolve");
    assert!(defaults.lib_is_default);
    assert!(!defaults.lib_files.is_empty());
    assert!(defaults
        .lib_files
        .iter()
        .all(|path| path.starts_with("/embedded-lib")));

    let explicit = CompilerOptions {
        lib: Some(vec!["es2020".to_owned()]),
        ..Default::default()
    };
    let explicit = resolve_compiler_options_embedded(Some(&explicit))
        .expect("embedded explicit lib resolves");
    assert!(!explicit.lib_is_default);
    assert!(explicit.lib_files.iter().any(|path| {
        path.file_name()
            .is_some_and(|name| name.to_string_lossy() == "lib.es2020.d.ts")
    }));
    assert!(explicit
        .lib_files
        .iter()
        .all(|path| path.starts_with("/embedded-lib")));
}
