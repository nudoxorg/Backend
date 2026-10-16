//! Captured-provider controls: no ambient lookup, negative-cache epochs,
//! symlink classification, boundary walks, and fail-closed provider errors.

use super::super::{ImportKind, ImportingModuleKind, ModuleResolver, ResolutionFailure};
use crate::config::{ModuleResolutionKind, ResolvedCompilerOptions};
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

fn resolve_dotted(
    fs: &MemoryFileSystem,
    mode: ModuleResolutionKind,
    importer: &str,
    kind: ImportKind,
    specifier: &str,
    allow_arbitrary_extensions: bool,
) -> io::Result<Result<super::super::ResolvedModule, ResolutionFailure>> {
    let options = ResolvedCompilerOptions {
        module_resolution: Some(mode),
        allow_arbitrary_extensions,
        module_suffixes: vec![String::new()],
        ..Default::default()
    };
    ModuleResolver::with_file_system(&options, fs).resolve_with_kind_checked(
        specifier,
        &Path::new("/virtual/workspace/src").join(importer),
        Span::new(0, specifier.len() as u32),
        kind,
    )
}

#[test]
fn dotted_basenames_append_source_extensions_in_implicit_resolution_modes() {
    let fs = MemoryFileSystem::new("/virtual/workspace");
    fs.add_dir("/virtual/workspace/src");
    fs.add_file(
        "/virtual/workspace/src/app.component.ts",
        "export class AppComponent {}",
    );
    fs.add_file(
        "/virtual/workspace/src/app.config.ts",
        "export const appConfig = {};",
    );
    fs.add_file(
        "/virtual/workspace/src/article-list-config.model.ts",
        "export interface Config {}",
    );
    for (mode, importer) in [
        (ModuleResolutionKind::Node, "main.ts"),
        (ModuleResolutionKind::Bundler, "main.ts"),
        (ModuleResolutionKind::Classic, "main.ts"),
        (ModuleResolutionKind::Node16, "main.cts"),
        (ModuleResolutionKind::NodeNext, "main.cts"),
    ] {
        for stem in ["app.component", "app.config", "article-list-config.model"] {
            let resolved = resolve_dotted(
                &fs,
                mode,
                importer,
                ImportKind::EsmImport,
                &format!("./{stem}"),
                false,
            )
            .expect("captured filesystem")
            .expect("complete dotted basename resolves");
            assert_eq!(
                resolved.resolved_path,
                PathBuf::from(format!("/virtual/workspace/src/{stem}.ts"))
            );
        }
    }
}

#[test]
fn dotted_basenames_keep_esm_refusal_and_arbitrary_declaration_precedence() {
    let fs = MemoryFileSystem::new("/virtual/workspace");
    fs.add_dir("/virtual/workspace/src");
    fs.add_file(
        "/virtual/workspace/src/app.component.ts",
        "export class AppComponent {}",
    );
    for mode in [ModuleResolutionKind::Node16, ModuleResolutionKind::NodeNext] {
        for (importer, kind) in [
            ("main.mts", ImportKind::EsmImport),
            ("main.cts", ImportKind::DynamicImport),
        ] {
            assert!(
                resolve_dotted(&fs, mode, importer, kind, "./app.component", false)
                    .expect("captured filesystem")
                    .is_err(),
                "ESM cannot add an implicit .ts suffix"
            );
        }
        assert!(
            resolve_dotted(
                &fs,
                mode,
                "main.cts",
                ImportKind::CjsRequire,
                "./app.component",
                false
            )
            .expect("captured filesystem")
            .is_ok()
        );
    }
    fs.add_file(
        "/virtual/workspace/src/app.d.component.ts",
        "export declare class AppComponent {}",
    );
    for mode in [
        ModuleResolutionKind::Node,
        ModuleResolutionKind::Bundler,
        ModuleResolutionKind::NodeNext,
    ] {
        for allow in [false, true] {
            let resolved = resolve_dotted(
                &fs,
                mode,
                "main.mts",
                ImportKind::EsmImport,
                "./app.component",
                allow,
            )
            .expect("captured filesystem")
            .expect("declaration companion still comes first");
            assert_eq!(
                resolved.resolved_path,
                PathBuf::from("/virtual/workspace/src/app.d.component.ts")
            );
        }
    }
}

#[test]
fn dotted_probe_preserves_known_extensions_assets_and_captured_failures() {
    let fs = MemoryFileSystem::new("/virtual/workspace");
    fs.add_dir("/virtual/workspace/src");
    fs.add_file(
        "/virtual/workspace/src/app.ts",
        "export const unrelated = true;",
    );
    fs.add_file("/virtual/workspace/src/style.css", "body {}");
    for allow in [false, true] {
        assert!(
            resolve_dotted(
                &fs,
                ModuleResolutionKind::Bundler,
                "main.ts",
                ImportKind::EsmImport,
                "./app.component",
                allow
            )
            .expect("captured filesystem")
            .is_err(),
            "never replace .component with .ts"
        );
        assert!(
            resolve_dotted(
                &fs,
                ModuleResolutionKind::Bundler,
                "main.ts",
                ImportKind::EsmImport,
                "./style.css",
                allow
            )
            .expect("captured filesystem")
            .is_err(),
            "raw assets never become source modules"
        );
    }
    fs.add_file(
        "/virtual/workspace/src/style.d.css.ts",
        "declare const css: string; export default css;",
    );
    fs.add_file(
        "/virtual/workspace/src/style.css.ts",
        "export const implementation = true;",
    );
    for allow in [false, true] {
        let resolved = resolve_dotted(
            &fs,
            ModuleResolutionKind::Bundler,
            "main.ts",
            ImportKind::EsmImport,
            "./style.css",
            allow,
        )
        .expect("captured filesystem")
        .expect("asset declaration stays first");
        assert_eq!(
            resolved.resolved_path,
            PathBuf::from("/virtual/workspace/src/style.d.css.ts")
        );
    }
    fs.add_file("/virtual/workspace/src/data.json", "{}");
    fs.add_file(
        "/virtual/workspace/src/data.json.ts",
        "export const notJson = true;",
    );
    assert!(
        matches!(
            resolve_dotted(
                &fs,
                ModuleResolutionKind::Node,
                "main.ts",
                ImportKind::EsmImport,
                "./data.json",
                false
            )
            .expect("captured filesystem"),
            Err(ResolutionFailure::JsonModuleWithoutResolveJsonModule { .. })
        ),
        "known JSON suffix keeps its own resolution policy"
    );
    fs.add_file(
        "/virtual/workspace/src/app.component.d.ts",
        "export declare const declaration: true;",
    );
    fs.add_file(
        "/virtual/workspace/src/app.component.js",
        "export const runtime = true;",
    );
    let resolved = resolve_dotted(
        &fs,
        ModuleResolutionKind::Bundler,
        "main.ts",
        ImportKind::EsmImport,
        "./app.component.js",
        false,
    )
    .expect("captured filesystem")
    .expect("known JS declaration substitution");
    assert_eq!(
        resolved.resolved_path,
        PathBuf::from("/virtual/workspace/src/app.component.d.ts")
    );
    fs.deny("/virtual/workspace/src/app.component.ts");
    assert_eq!(
        resolve_dotted(
            &fs,
            ModuleResolutionKind::Bundler,
            "main.ts",
            ImportKind::EsmImport,
            "./app.component",
            false
        )
        .expect_err("a failed captured probe cannot become ordinary NotFound")
        .kind(),
        io::ErrorKind::PermissionDenied
    );
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

#[test]
fn type_reference_prefers_at_types_and_uses_native_package_metadata() {
    let fs = MemoryFileSystem::new("/virtual/workspace");
    fs.add_dir("/virtual/workspace/src");
    fs.add_dir("/virtual/workspace/node_modules");
    fs.add_dir("/virtual/workspace/node_modules/tool");
    fs.add_file(
        "/virtual/workspace/node_modules/tool/index.d.ts",
        "export declare const runtime: string;",
    );
    fs.add_dir("/virtual/workspace/node_modules/@types");
    fs.add_dir("/virtual/workspace/node_modules/@types/tool");
    fs.add_file(
        "/virtual/workspace/node_modules/@types/tool/package.json",
        r#"{"name":"@types/tool","types":"./types/index.d.ts"}"#,
    );
    fs.add_dir("/virtual/workspace/node_modules/@types/tool/types");
    fs.add_file(
        "/virtual/workspace/node_modules/@types/tool/types/index.d.ts",
        "export declare const declaration: number;",
    );
    let importer = Path::new("/virtual/workspace/src/main.ts");
    let mut resolver =
        ModuleResolver::with_file_system(&ResolvedCompilerOptions::default(), &fs);

    let resolved = resolver
        .resolve_type_reference_directive_checked(
            "tool",
            importer,
            None,
            Some(ImportingModuleKind::Esm),
        )
        .expect("captured provider should permit type resolution")
        .expect("matching @types package should resolve");

    assert_eq!(
        resolved.resolved_path,
        PathBuf::from("/virtual/workspace/node_modules/@types/tool/types/index.d.ts")
    );
}

#[test]
fn explicit_type_roots_precede_default_at_types_roots() {
    let fs = MemoryFileSystem::new("/virtual/workspace");
    fs.add_dir("/virtual/workspace/src");
    fs.add_dir("/virtual/workspace/custom-types");
    fs.add_dir("/virtual/workspace/custom-types/pkg");
    fs.add_file(
        "/virtual/workspace/custom-types/pkg/index.d.ts",
        "export declare const custom: true;",
    );
    fs.add_dir("/virtual/workspace/node_modules");
    fs.add_dir("/virtual/workspace/node_modules/@types");
    fs.add_dir("/virtual/workspace/node_modules/@types/pkg");
    fs.add_file(
        "/virtual/workspace/node_modules/@types/pkg/index.d.ts",
        "export declare const defaultRoot: true;",
    );
    let options = ResolvedCompilerOptions {
        type_roots: Some(vec![PathBuf::from("/virtual/workspace/custom-types")]),
        ..Default::default()
    };
    let mut resolver = ModuleResolver::with_file_system(&options, &fs);

    let resolved = resolver
        .resolve_type_reference_directive_checked(
            "pkg",
            Path::new("/virtual/workspace/src/main.ts"),
            None,
            None,
        )
        .expect("captured provider should permit type resolution")
        .expect("explicit type root package should resolve");

    assert_eq!(
        resolved.resolved_path,
        PathBuf::from("/virtual/workspace/custom-types/pkg/index.d.ts")
    );
}

#[test]
fn type_reference_uses_exports_and_types_versions_from_package_json() {
    let fs = MemoryFileSystem::new("/virtual/workspace");
    fs.add_dir("/virtual/workspace/src");
    fs.add_dir("/virtual/workspace/custom-types");
    fs.add_dir("/virtual/workspace/custom-types/exports-pkg");
    fs.add_file(
        "/virtual/workspace/custom-types/exports-pkg/package.json",
        r#"{"name":"exports-pkg","exports":{".":{"types":"./modern.d.ts","default":"./runtime.js"}}}"#,
    );
    fs.add_file(
        "/virtual/workspace/custom-types/exports-pkg/modern.d.ts",
        "export {};",
    );
    fs.add_file(
        "/virtual/workspace/custom-types/exports-pkg/runtime.js",
        "module.exports = {};",
    );
    fs.add_dir("/virtual/workspace/custom-types/versions-pkg");
    fs.add_file(
        "/virtual/workspace/custom-types/versions-pkg/package.json",
        r#"{"name":"versions-pkg","typesVersions":{"*":{"*":["v5/*"]}}}"#,
    );
    fs.add_dir("/virtual/workspace/custom-types/versions-pkg/v5");
    fs.add_file(
        "/virtual/workspace/custom-types/versions-pkg/v5/index.d.ts",
        "export {};",
    );
    let options = ResolvedCompilerOptions {
        module_resolution: Some(ModuleResolutionKind::NodeNext),
        resolve_package_json_exports: true,
        types_versions_compiler_version: Some("5.4.0".to_owned()),
        type_roots: Some(vec![PathBuf::from("/virtual/workspace/custom-types")]),
        ..Default::default()
    };
    let mut resolver = ModuleResolver::with_file_system(&options, &fs);
    let roots = [PathBuf::from("/virtual/workspace/custom-types")];

    let exported = resolver
        .resolve_type_reference_directive_checked(
            "exports-pkg",
            Path::new("/virtual/workspace/src/main.mts"),
            Some(&roots),
            Some(ImportingModuleKind::Esm),
        )
        .expect("exports package metadata should be readable")
        .expect("types export should resolve");
    let versioned = resolver
        .resolve_type_reference_directive_checked(
            "versions-pkg",
            Path::new("/virtual/workspace/src/main.mts"),
            Some(&roots),
            Some(ImportingModuleKind::Esm),
        )
        .expect("typesVersions package metadata should be readable")
        .expect("typesVersions entry should resolve");

    assert_eq!(
        exported.resolved_path,
        PathBuf::from("/virtual/workspace/custom-types/exports-pkg/modern.d.ts")
    );
    assert_eq!(
        versioned.resolved_path,
        PathBuf::from("/virtual/workspace/custom-types/versions-pkg/v5/index.d.ts")
    );
}

#[test]
fn source_type_reference_falls_back_to_bare_package_declarations() {
    let fs = MemoryFileSystem::new("/virtual/workspace");
    fs.add_dir("/virtual/workspace/src");
    fs.add_dir("/virtual/workspace/node_modules");
    fs.add_dir("/virtual/workspace/node_modules/vite");
    fs.add_file(
        "/virtual/workspace/node_modules/vite/package.json",
        r#"{"name":"vite","exports":{"./client":"./client.d.ts"}}"#,
    );
    fs.add_file(
        "/virtual/workspace/node_modules/vite/client.d.ts",
        "interface ImportMetaEnv { readonly DEV: boolean; }",
    );
    let options = ResolvedCompilerOptions {
        module_resolution: Some(ModuleResolutionKind::NodeNext),
        resolve_package_json_exports: true,
        ..Default::default()
    };
    let mut resolver = ModuleResolver::with_file_system(&options, &fs);

    let resolved = resolver
        .resolve_type_reference_directive_checked(
            "vite/client",
            Path::new("/virtual/workspace/src/main.ts"),
            None,
            Some(ImportingModuleKind::Esm),
        )
        .expect("captured provider should permit the secondary package lookup")
        .expect("Vite's package declaration should resolve as a source type reference");

    assert_eq!(
        resolved.resolved_path,
        PathBuf::from("/virtual/workspace/node_modules/vite/client.d.ts")
    );
}

#[test]
fn program_type_roots_suppress_bare_package_fallback_but_source_references_keep_it() {
    let fs = MemoryFileSystem::new("/virtual/workspace");
    fs.add_dir("/virtual/workspace/src");
    fs.add_dir("/virtual/workspace/custom-types");
    fs.add_dir("/virtual/workspace/node_modules");
    fs.add_dir("/virtual/workspace/node_modules/vite");
    fs.add_file(
        "/virtual/workspace/node_modules/vite/package.json",
        r#"{"name":"vite","exports":{"./client":"./client.d.ts"}}"#,
    );
    fs.add_file(
        "/virtual/workspace/node_modules/vite/client.d.ts",
        "interface ImportMetaEnv { readonly DEV: boolean; }",
    );
    let options = ResolvedCompilerOptions {
        module_resolution: Some(ModuleResolutionKind::NodeNext),
        resolve_package_json_exports: true,
        type_roots: Some(vec![PathBuf::from("/virtual/workspace/custom-types")]),
        ..Default::default()
    };
    let roots = [PathBuf::from("/virtual/workspace/custom-types")];
    let importer = Path::new("/virtual/workspace/src/main.ts");
    let mut source_resolver = ModuleResolver::with_file_system(&options, &fs);
    let source_reference = source_resolver
        .resolve_type_reference_directive_checked(
            "vite/client",
            importer,
            Some(&roots),
            Some(ImportingModuleKind::Esm),
        )
        .expect("source type references permit secondary package resolution")
        .expect("Vite's declaration should remain available to source references");

    let mut program_resolver = ModuleResolver::with_file_system(&options, &fs);
    let program_type = program_resolver
        .resolve_program_type_reference_directive_checked(
            "vite/client",
            importer,
            Some(&roots),
            Some(ImportingModuleKind::Esm),
        )
        .expect("captured custom type root should be queryable");

    assert_eq!(
        source_reference.resolved_path,
        PathBuf::from("/virtual/workspace/node_modules/vite/client.d.ts")
    );
    assert!(matches!(program_type, Err(ResolutionFailure::NotFound { .. })));
}

#[test]
fn automatic_type_names_skip_explicit_null_and_propagate_provider_refusals() {
    let fs = MemoryFileSystem::new("/virtual/workspace");
    let root = PathBuf::from("/virtual/workspace/node_modules/@types");
    fs.add_dir(&root);
    fs.add_dir(root.join("visible"));
    fs.add_dir(root.join("disabled"));
    fs.add_file(root.join("disabled/package.json"), r#"{"types":null}"#);
    fs.add_dir(root.join(".hidden"));
    fs.add_file(root.join(".hidden/package.json"), r#"{"types":"./index.d.ts"}"#);
    fs.add_dir(root.join("@scope"));
    fs.add_dir(root.join("@scope/package"));
    fs.add_file(
        root.join("@scope/package/package.json"),
        r#"{"typings":null}"#,
    );
    let mut resolver =
        ModuleResolver::with_file_system(&ResolvedCompilerOptions::default(), &fs);

    let names = resolver
        .automatic_type_directive_names_checked(std::slice::from_ref(&root))
        .expect("captured type root should be enumerable");
    assert_eq!(names, vec!["visible".to_owned()]);

    let denied_fs = MemoryFileSystem::new("/virtual/workspace");
    denied_fs.add_dir(&root);
    denied_fs.add_dir(root.join("blocked"));
    denied_fs.deny(root.join("blocked/package.json"));
    let mut denied_resolver =
        ModuleResolver::with_file_system(&ResolvedCompilerOptions::default(), &denied_fs);
    let error = denied_resolver
        .automatic_type_directive_names_checked(std::slice::from_ref(&root))
        .expect_err("provider refusals must not silently become absent directives");
    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
}
