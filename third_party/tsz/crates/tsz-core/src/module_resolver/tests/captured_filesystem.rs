//! Captured-provider controls: no ambient lookup, negative-cache epochs,
//! symlink classification, boundary walks, and fail-closed provider errors.

use super::super::{ImportKind, ModuleResolver, ResolutionFailure};
use crate::config::ResolvedCompilerOptions;
use crate::resolution::filesystem::{
    ResolutionDirEntry, ResolutionFileKind, ResolutionFileSystem,
};
use crate::span::Span;
use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::RwLock;

#[derive(Default)]
struct MemoryFileSystem {
    boundary: PathBuf,
    files: RwLock<HashMap<PathBuf, Vec<u8>>>,
    directories: RwLock<HashSet<PathBuf>>,
    symlinks: RwLock<HashSet<PathBuf>>,
    denied: RwLock<HashSet<PathBuf>>,
    epoch: AtomicU64,
    file_probes: AtomicUsize,
    directory_probes: AtomicUsize,
    file_reads: AtomicUsize,
    symlink_probes: AtomicUsize,
    probed_paths: RwLock<Vec<PathBuf>>,
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

    fn add_file(&self, path: impl Into<PathBuf>, text: &str) {
        self.files
            .write()
            .unwrap()
            .insert(path.into(), text.as_bytes().to_vec());
    }

    fn deny(&self, path: impl Into<PathBuf>) {
        self.denied.write().unwrap().insert(path.into());
    }

    fn bump_epoch(&self) {
        self.epoch.fetch_add(1, Ordering::SeqCst);
    }

    fn check_denied(&self, path: &Path) -> io::Result<()> {
        if self.denied.read().unwrap().contains(path) {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("captured path denied: {}", path.display()),
            ))
        } else {
            Ok(())
        }
    }
}

impl ResolutionFileSystem for MemoryFileSystem {
    fn cache_epoch(&self) -> u64 {
        self.epoch.load(Ordering::SeqCst)
    }

    fn resolution_boundary(&self) -> Option<&Path> {
        Some(&self.boundary)
    }

    fn read_file(&self, path: &Path) -> io::Result<Option<Vec<u8>>> {
        self.check_denied(path)?;
        self.file_reads.fetch_add(1, Ordering::Relaxed);
        Ok(self.files.read().unwrap().get(path).cloned())
    }

    fn is_file(&self, path: &Path) -> io::Result<bool> {
        self.check_denied(path)?;
        self.file_probes.fetch_add(1, Ordering::Relaxed);
        self.probed_paths.write().unwrap().push(path.to_path_buf());
        Ok(self.files.read().unwrap().contains_key(path))
    }

    fn is_dir(&self, path: &Path) -> io::Result<bool> {
        self.check_denied(path)?;
        self.directory_probes.fetch_add(1, Ordering::Relaxed);
        self.probed_paths.write().unwrap().push(path.to_path_buf());
        Ok(self.directories.read().unwrap().contains(path))
    }

    fn read_dir(&self, path: &Path) -> io::Result<Option<Vec<ResolutionDirEntry>>> {
        self.check_denied(path)?;
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
        for symlink in self.symlinks.read().unwrap().iter() {
            if symlink.parent() == Some(path) {
                if let Some(name) = symlink.file_name() {
                    entries.insert(name.to_os_string(), ResolutionFileKind::Symlink);
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
        self.check_denied(path)?;
        let exists = self.files.read().unwrap().contains_key(path)
            || self.directories.read().unwrap().contains(path)
            || self.symlinks.read().unwrap().contains(path);
        Ok(exists.then(|| path.to_path_buf()))
    }

    fn symlink_metadata(&self, path: &Path) -> io::Result<Option<ResolutionFileKind>> {
        self.check_denied(path)?;
        self.symlink_probes.fetch_add(1, Ordering::Relaxed);
        if self.symlinks.read().unwrap().contains(path) {
            return Ok(Some(ResolutionFileKind::Symlink));
        }
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
fn resolver_uses_only_captured_files_and_stops_at_its_boundary() {
    let fs = MemoryFileSystem::new("/virtual/workspace");
    fs.add_dir("/virtual/workspace/src");
    fs.add_file("/virtual/workspace/src/dep.ts", "export {};");
    let importer = Path::new("/virtual/workspace/src/main.ts");
    let dependency = Path::new("/virtual/workspace/src/dep.ts");
    assert!(!crate::resolution::helpers::cached_is_file(dependency));
    let mut resolver = ModuleResolver::with_file_system(&ResolvedCompilerOptions::default(), &fs);

    let result = resolver
        .resolve_with_kind_checked("./dep", importer, Span::new(0, 5), ImportKind::EsmImport)
        .expect("captured provider should permit resolution")
        .expect("dependency should resolve");

    assert_eq!(result.resolved_path, PathBuf::from("/virtual/workspace/src/dep.ts"));
    assert!(fs.file_probes.load(Ordering::Relaxed) > 0);
    assert!(fs.file_reads.load(Ordering::Relaxed) > 0);
    assert!(fs
        .probed_paths
        .read()
        .unwrap()
        .iter()
        .all(|path| path.starts_with("/virtual/workspace")));
}

#[test]
fn injected_resolver_retries_negative_results_after_provider_epoch_changes() {
    let fs = MemoryFileSystem::new("/virtual/workspace");
    fs.add_dir("/virtual/workspace/src");
    let importer = Path::new("/virtual/workspace/src/main.ts");
    let mut resolver = ModuleResolver::with_file_system(&ResolvedCompilerOptions::default(), &fs);

    let first = resolver
        .resolve_with_kind_checked("./later", importer, Span::new(0, 7), ImportKind::EsmImport)
        .expect("missing file is a normal negative lookup");
    assert!(matches!(first, Err(ResolutionFailure::NotFound { .. })));

    fs.add_file("/virtual/workspace/src/later.ts", "export {};");
    fs.bump_epoch();
    let second = resolver
        .resolve_with_kind_checked("./later", importer, Span::new(0, 7), ImportKind::EsmImport)
        .expect("new provider epoch invalidates negative caches")
        .expect("file added in the new epoch should resolve");
    assert_eq!(second.resolved_path, PathBuf::from("/virtual/workspace/src/later.ts"));
}

#[test]
fn captured_provider_refusals_are_not_reported_as_module_not_found() {
    let fs = MemoryFileSystem::new("/virtual/workspace");
    fs.add_dir("/virtual/workspace/src");
    fs.deny("/virtual/workspace/src/secret.ts");
    let importer = Path::new("/virtual/workspace/src/main.ts");
    let mut resolver = ModuleResolver::with_file_system(&ResolvedCompilerOptions::default(), &fs);

    let error = resolver
        .resolve_with_kind_checked("./secret", importer, Span::new(0, 8), ImportKind::EsmImport)
        .expect_err("provider denial must escape as a host error");
    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
}

#[test]
fn captured_provider_preserves_symlink_entry_kind() {
    let fs = MemoryFileSystem::new("/virtual/workspace");
    let package = PathBuf::from("/virtual/workspace/node_modules/pkg");
    fs.symlinks.write().unwrap().insert(package.clone());
    let resolver = ModuleResolver::with_file_system(&ResolvedCompilerOptions::default(), &fs);

    assert_eq!(
        resolver.symlink_metadata(&package),
        Some(ResolutionFileKind::Symlink)
    );
    assert_eq!(fs.symlink_probes.load(Ordering::Relaxed), 1);
}
