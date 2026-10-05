//! Request-scoped admission for a project's installed TypeScript compiler.
//!
//! Project discovery is deliberately rooted at the package resolver's selected path. It never
//! searches `PATH`, downloads packages, or executes a package-manager wrapper. A local `tsc`
//! entry is resolved to the `typescript` package's JavaScript file and probed through the exact
//! admitted Node executable in a closed environment.

use std::{
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
};

use backend_frontend_typescript::legacy::{Checker, ExplicitTypeScriptChecker};
use backend_semantic::vocabulary::NativeTool;
use blake3::Hasher;
use thiserror::Error;

use crate::application::{ToolchainProbeError, ToolchainProbeLimits};
use crate::driver::ToolchainResolutionError;

const MAX_PROJECT_ANCESTORS: usize = 32;
const MAX_PACKAGE_MANIFEST_BYTES: usize = 64 * 1024;

/// Closed host inputs used to admit TypeScript projects after package-root selection.
#[derive(Clone, Debug)]
pub struct TypeScriptProjectHost {
    explicit_compiler: Option<Box<Path>>,
    node: Option<Box<Path>>,
    explicit_module_root: Option<Box<Path>>,
    report_program: Option<Box<Path>>,
    probe_limits: ToolchainProbeLimits,
}

impl TypeScriptProjectHost {
    pub(crate) fn new(
        explicit_compiler: Option<PathBuf>,
        node: Option<PathBuf>,
        explicit_module_root: Option<PathBuf>,
        report_program: Option<PathBuf>,
        probe_limits: ToolchainProbeLimits,
    ) -> Self {
        Self {
            explicit_compiler: explicit_compiler.map(PathBuf::into_boxed_path),
            node: node.map(PathBuf::into_boxed_path),
            explicit_module_root: explicit_module_root.map(PathBuf::into_boxed_path),
            report_program: report_program.map(PathBuf::into_boxed_path),
            probe_limits,
        }
    }

    /// Resolves and admits the TypeScript installation selected by one exact package root.
    ///
    /// `Ok(None)` means no project-local installation exists, allowing the caller to preserve an
    /// already configured host-wide checker. Every present-but-invalid installation is a typed
    /// refusal and must not silently fall back to another project's or the host's compiler.
    pub(crate) fn admit(
        &self,
        package_root: &Path,
    ) -> Result<Option<AdmittedTypeScriptProject>, TypeScriptProjectHostError> {
        if !package_root.is_absolute() {
            return Err(TypeScriptProjectHostError::RelativePackageRoot {
                package_root: package_root.to_path_buf().into_boxed_path(),
            });
        }
        let project_root = fs::canonicalize(package_root).map_err(|source| {
            TypeScriptProjectHostError::PackageRoot {
                package_root: package_root.to_path_buf().into_boxed_path(),
                source,
            }
        })?;

        let project = match find_project_typescript(&project_root)? {
            ProjectTypeScriptSearch::Found(project) => Some(project),
            ProjectTypeScriptSearch::NotFound => None,
            ProjectTypeScriptSearch::Pnp(marker) => {
                return Err(TypeScriptProjectHostError::YarnPnpUnsupported {
                    marker: marker.into_boxed_path(),
                });
            }
        };

        let Some(project) = project else {
            return Ok(None);
        };

        let compiler = self
            .explicit_compiler
            .as_deref()
            .unwrap_or(project.compiler.as_path());
        let node =
            self.node
                .as_deref()
                .ok_or_else(|| TypeScriptProjectHostError::NodeUnavailable {
                    package_root: project_root.clone().into_boxed_path(),
                })?;
        let node_version = crate::application::toolchain_probe::probe_command(
            NativeTool::TypeScriptCompiler,
            node,
            &["--version"],
            self.probe_limits,
        )
        .map_err(|source| TypeScriptProjectHostError::NodeProbe {
            node: node.to_path_buf().into_boxed_path(),
            source,
        })?;

        let (module_root, expected_version) = match self.explicit_module_root.as_deref() {
            Some(root) => read_typescript_module(root)?,
            None if self.explicit_compiler.is_some() => {
                let root = find_module_root_for_compiler(compiler)?
                    .unwrap_or_else(|| project.module_root.clone());
                read_typescript_module(&root)?
            }
            None => (project.module_root.clone(), project.version.clone()),
        };

        let version = if is_module_tsc_script(compiler, &module_root) {
            crate::application::toolchain_probe::probe_typescript_script_with_node(
                compiler,
                node,
                self.probe_limits,
            )
            .map_err(|source| TypeScriptProjectHostError::CompilerProbe {
                compiler: compiler.to_path_buf().into_boxed_path(),
                node: node.to_path_buf().into_boxed_path(),
                source,
            })?
        } else {
            crate::application::toolchain_probe::probe_version(
                NativeTool::TypeScriptCompiler,
                compiler,
                self.probe_limits,
            )
            .map_err(|source| TypeScriptProjectHostError::CompilerProbe {
                compiler: compiler.to_path_buf().into_boxed_path(),
                node: node.to_path_buf().into_boxed_path(),
                source,
            })?
        };
        let observed_version = parse_tsc_version(&version).ok_or_else(|| {
            TypeScriptProjectHostError::InvalidCompilerVersion {
                compiler: compiler.to_path_buf().into_boxed_path(),
                output: bounded_text(&version),
            }
        })?;
        if observed_version != expected_version {
            return Err(TypeScriptProjectHostError::VersionMismatch {
                compiler: compiler.to_path_buf().into_boxed_path(),
                expected: expected_version.into_boxed_str(),
                observed: observed_version.into_boxed_str(),
            });
        }

        let checker = match self.report_program.as_deref() {
            Some(program) => Checker::default().with_program(program.to_path_buf()),
            None => Checker::default().with_node(node.to_path_buf(), module_root.clone()),
        }
        .map_err(|source| TypeScriptProjectHostError::CheckerConfiguration {
            node: node.to_path_buf().into_boxed_path(),
            module_root: module_root.clone().into_boxed_path(),
            message: source.to_string().into_boxed_str(),
        })?;

        let fingerprint = project_fingerprint(
            &project_root,
            compiler,
            node,
            &module_root,
            &expected_version,
            &node_version,
            checker.local_configuration_fingerprint(),
        );
        Ok(Some(AdmittedTypeScriptProject {
            checker,
            compiler: compiler.to_path_buf().into_boxed_path(),
            compiler_version: version,
            fingerprint,
        }))
    }
}

/// One project-local checker and the exact toolchain facts admitted for it.
#[derive(Debug)]
pub(crate) struct AdmittedTypeScriptProject {
    pub(crate) checker: ExplicitTypeScriptChecker,
    pub(crate) compiler: Box<Path>,
    pub(crate) compiler_version: Box<[u8]>,
    pub(crate) fingerprint: [u8; 32],
}

#[derive(Debug)]
struct ProjectTypeScript {
    module_root: PathBuf,
    compiler: PathBuf,
    version: String,
}

enum ProjectTypeScriptSearch {
    Found(ProjectTypeScript),
    NotFound,
    Pnp(PathBuf),
}

fn find_project_typescript(
    root: &Path,
) -> Result<ProjectTypeScriptSearch, TypeScriptProjectHostError> {
    let mut pnp = None;
    for ancestor in root.ancestors().take(MAX_PROJECT_ANCESTORS) {
        let node_modules = ancestor.join("node_modules");
        let package = node_modules.join("typescript");
        match fs::symlink_metadata(&package) {
            Ok(_) => return inspect_project_package(&node_modules, &package),
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(TypeScriptProjectHostError::PackagePath {
                    path: package.into_boxed_path(),
                    source,
                });
            }
        }
        let marker = ancestor.join(".pnp.cjs");
        if marker.is_file() && pnp.is_none() {
            pnp = Some(marker);
        }
    }
    Ok(match pnp {
        Some(marker) => ProjectTypeScriptSearch::Pnp(marker),
        None => ProjectTypeScriptSearch::NotFound,
    })
}

fn inspect_project_package(
    node_modules: &Path,
    package: &Path,
) -> Result<ProjectTypeScriptSearch, TypeScriptProjectHostError> {
    let node_modules = fs::canonicalize(node_modules).map_err(|source| {
        TypeScriptProjectHostError::PackagePath {
            path: node_modules.to_path_buf().into_boxed_path(),
            source,
        }
    })?;
    let package =
        fs::canonicalize(package).map_err(|source| TypeScriptProjectHostError::PackagePath {
            path: package.to_path_buf().into_boxed_path(),
            source,
        })?;
    if !package.starts_with(&node_modules) {
        return Err(TypeScriptProjectHostError::PackageEscapesNodeModules {
            package: package.into_boxed_path(),
            node_modules: node_modules.into_boxed_path(),
        });
    }
    let manifest = package.join("package.json");
    let (name, version) = read_package_manifest(&manifest)?;
    if name != "typescript" {
        return Err(TypeScriptProjectHostError::InvalidPackageName {
            manifest: manifest.into_boxed_path(),
            name: name.into_boxed_str(),
        });
    }
    validate_semver(&version).ok_or_else(|| TypeScriptProjectHostError::InvalidPackageVersion {
        manifest: manifest.clone().into_boxed_path(),
        version: version.clone().into_boxed_str(),
    })?;
    let compiler = package.join("bin/tsc");
    let compiler =
        fs::canonicalize(&compiler).map_err(|source| TypeScriptProjectHostError::PackagePath {
            path: compiler.clone().into_boxed_path(),
            source,
        })?;
    if !compiler.starts_with(&package) || !compiler.is_file() {
        return Err(TypeScriptProjectHostError::CompilerEscapesPackage {
            compiler: compiler.into_boxed_path(),
            package: package.into_boxed_path(),
        });
    }
    let bin_link = node_modules.join(".bin/tsc");
    match fs::symlink_metadata(&bin_link) {
        Ok(metadata) => {
            if !metadata.file_type().is_symlink() {
                return Err(TypeScriptProjectHostError::CompilerShimRejected {
                    shim: bin_link.into_boxed_path(),
                });
            }
            let target = fs::canonicalize(&bin_link).map_err(|source| {
                TypeScriptProjectHostError::PackagePath {
                    path: bin_link.clone().into_boxed_path(),
                    source,
                }
            })?;
            if target != compiler {
                return Err(TypeScriptProjectHostError::CompilerLinkMismatch {
                    shim: bin_link.into_boxed_path(),
                    target: target.into_boxed_path(),
                    expected: compiler.into_boxed_path(),
                });
            }
        }
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {}
        Err(source) => {
            return Err(TypeScriptProjectHostError::PackagePath {
                path: bin_link.into_boxed_path(),
                source,
            });
        }
    }
    Ok(ProjectTypeScriptSearch::Found(ProjectTypeScript {
        module_root: node_modules,
        compiler,
        version,
    }))
}

fn find_module_root_for_compiler(
    compiler: &Path,
) -> Result<Option<PathBuf>, TypeScriptProjectHostError> {
    for ancestor in compiler.ancestors().take(MAX_PROJECT_ANCESTORS) {
        if ancestor
            .file_name()
            .is_some_and(|name| name == "node_modules")
        {
            if ancestor.join("typescript/package.json").is_file() {
                return Ok(Some(ancestor.to_path_buf()));
            }
        }
    }
    Ok(None)
}

fn is_module_tsc_script(compiler: &Path, module_root: &Path) -> bool {
    let expected = module_root.join("typescript/bin/tsc");
    matches!(
        (fs::canonicalize(compiler), fs::canonicalize(expected)),
        (Ok(compiler), Ok(expected)) if compiler == expected
    )
}

fn read_typescript_module(root: &Path) -> Result<(PathBuf, String), TypeScriptProjectHostError> {
    let root =
        fs::canonicalize(root).map_err(|source| TypeScriptProjectHostError::PackagePath {
            path: root.to_path_buf().into_boxed_path(),
            source,
        })?;
    let manifest = root.join("typescript/package.json");
    let (name, version) = read_package_manifest(&manifest)?;
    if name != "typescript" {
        return Err(TypeScriptProjectHostError::InvalidPackageName {
            manifest: manifest.into_boxed_path(),
            name: name.into_boxed_str(),
        });
    }
    validate_semver(&version).ok_or_else(|| TypeScriptProjectHostError::InvalidPackageVersion {
        manifest: manifest.clone().into_boxed_path(),
        version: version.clone().into_boxed_str(),
    })?;
    Ok((root, version))
}

fn read_package_manifest(path: &Path) -> Result<(String, String), TypeScriptProjectHostError> {
    let file = File::open(path).map_err(|source| TypeScriptProjectHostError::PackagePath {
        path: path.to_path_buf().into_boxed_path(),
        source,
    })?;
    let mut bytes = Vec::new();
    file.take(u64::try_from(MAX_PACKAGE_MANIFEST_BYTES + 1).unwrap_or(u64::MAX))
        .read_to_end(&mut bytes)
        .map_err(|source| TypeScriptProjectHostError::PackagePath {
            path: path.to_path_buf().into_boxed_path(),
            source,
        })?;
    if bytes.len() > MAX_PACKAGE_MANIFEST_BYTES {
        return Err(TypeScriptProjectHostError::ManifestTooLarge {
            manifest: path.to_path_buf().into_boxed_path(),
            maximum: MAX_PACKAGE_MANIFEST_BYTES,
        });
    }
    let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(|source| {
        TypeScriptProjectHostError::ManifestInvalid {
            manifest: path.to_path_buf().into_boxed_path(),
            message: source.to_string().into_boxed_str(),
        }
    })?;
    let name = value.get("name").and_then(serde_json::Value::as_str);
    let version = value.get("version").and_then(serde_json::Value::as_str);
    match (name, version) {
        (Some(name), Some(version)) => Ok((name.to_owned(), version.to_owned())),
        _ => Err(TypeScriptProjectHostError::ManifestInvalid {
            manifest: path.to_path_buf().into_boxed_path(),
            message: "package.json must contain string name and version fields".into(),
        }),
    }
}

fn parse_tsc_version(bytes: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(bytes).ok()?.trim();
    let version = text.strip_prefix("Version ").unwrap_or(text).trim();
    validate_semver(version).map(|()| version.to_owned())
}

fn validate_semver(version: &str) -> Option<()> {
    if version.is_empty()
        || version.len() > 128
        || version.bytes().any(|byte| byte.is_ascii_whitespace())
    {
        return None;
    }
    let (without_build, build) = version
        .split_once('+')
        .map_or((version, None), |(a, b)| (a, Some(b)));
    if build.is_some_and(|value| !valid_identifiers(value)) {
        return None;
    }
    let (core, prerelease) = without_build
        .split_once('-')
        .map_or((without_build, None), |(a, b)| (a, Some(b)));
    if prerelease.is_some_and(|value| !valid_identifiers(value)) {
        return None;
    }
    let mut parts = core.split('.');
    for _ in 0..3 {
        let part = parts.next()?;
        if part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        if part.len() > 1 && part.starts_with('0') {
            return None;
        }
        if part.parse::<u64>().is_err() {
            return None;
        }
    }
    parts.next().is_none().then_some(())
}

fn valid_identifiers(value: &str) -> bool {
    !value.is_empty()
        && value.split('.').all(|identifier| {
            !identifier.is_empty()
                && identifier
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        })
}

fn bounded_text(bytes: &[u8]) -> Box<str> {
    let shown = &bytes[..bytes.len().min(512)];
    String::from_utf8_lossy(shown).into_owned().into_boxed_str()
}

fn project_fingerprint(
    package_root: &Path,
    compiler: &Path,
    node: &Path,
    module_root: &Path,
    package_version: &str,
    node_version: &[u8],
    checker_fingerprint: [u8; 32],
) -> [u8; 32] {
    let mut digest = Hasher::new();
    digest.update(b"compiler.typescript.project-admission.v1\0");
    for path in [package_root, compiler, node, module_root] {
        let bytes = path.as_os_str().as_encoded_bytes();
        digest.update(&(bytes.len() as u64).to_be_bytes());
        digest.update(bytes);
    }
    digest.update(&(package_version.len() as u64).to_be_bytes());
    digest.update(package_version.as_bytes());
    digest.update(&(node_version.len() as u64).to_be_bytes());
    digest.update(node_version);
    digest.update(&checker_fingerprint);
    *digest.finalize().as_bytes()
}

/// Typed terminal from resolving or probing one project-owned TypeScript installation.
///
/// Each variant's display text identifies the selected path or the bounded process failure.
/// Its field names are intentionally descriptive in the structured error chain.
#[derive(Debug, Error)]
#[allow(missing_docs)]
pub enum TypeScriptProjectHostError {
    #[error("admitted TypeScript executable could not be bound to its version identity")]
    ToolchainResolution {
        #[source]
        source: ToolchainResolutionError,
    },
    #[error("TypeScript package root is not absolute: {package_root:?}")]
    RelativePackageRoot { package_root: Box<Path> },
    #[error("could not resolve selected TypeScript package root {package_root:?}")]
    PackageRoot {
        package_root: Box<Path>,
        #[source]
        source: std::io::Error,
    },
    #[error(
        "Yarn Plug'n'Play manifest {marker:?} is present, but this checker currently requires a local node_modules layout"
    )]
    YarnPnpUnsupported { marker: Box<Path> },
    #[error(
        "project has a TypeScript installation but no explicitly admitted Node executable is available: {package_root:?}"
    )]
    NodeUnavailable { package_root: Box<Path> },
    #[error("could not probe the selected Node executable at {node:?}")]
    NodeProbe {
        node: Box<Path>,
        #[source]
        source: ToolchainProbeError,
    },
    #[error("could not read TypeScript package path {path:?}")]
    PackagePath {
        path: Box<Path>,
        #[source]
        source: std::io::Error,
    },
    #[error(
        "TypeScript package {package:?} escapes its admitted node_modules directory {node_modules:?}"
    )]
    PackageEscapesNodeModules {
        package: Box<Path>,
        node_modules: Box<Path>,
    },
    #[error("TypeScript package manifest {manifest:?} names {name:?}, not typescript")]
    InvalidPackageName { manifest: Box<Path>, name: Box<str> },
    #[error("TypeScript package manifest {manifest:?} has invalid semver version {version:?}")]
    InvalidPackageVersion {
        manifest: Box<Path>,
        version: Box<str>,
    },
    #[error("TypeScript package manifest {manifest:?} exceeds the {maximum}-byte bound")]
    ManifestTooLarge { manifest: Box<Path>, maximum: usize },
    #[error("could not decode TypeScript package manifest {manifest:?}: {message}")]
    ManifestInvalid {
        manifest: Box<Path>,
        message: Box<str>,
    },
    #[error("TypeScript compiler entry {compiler:?} is outside package {package:?}")]
    CompilerEscapesPackage {
        compiler: Box<Path>,
        package: Box<Path>,
    },
    #[error("automatically discovered TypeScript shim {shim:?} is not a package-manager symlink")]
    CompilerShimRejected { shim: Box<Path> },
    #[error(
        "TypeScript shim {shim:?} resolves to {target:?}, expected the admitted package compiler {expected:?}"
    )]
    CompilerLinkMismatch {
        shim: Box<Path>,
        target: Box<Path>,
        expected: Box<Path>,
    },
    #[error("could not probe TypeScript compiler {compiler:?} with admitted Node {node:?}")]
    CompilerProbe {
        compiler: Box<Path>,
        node: Box<Path>,
        #[source]
        source: ToolchainProbeError,
    },
    #[error("TypeScript compiler {compiler:?} returned an invalid --version result: {output:?}")]
    InvalidCompilerVersion {
        compiler: Box<Path>,
        output: Box<str>,
    },
    #[error(
        "TypeScript compiler {compiler:?} reports {observed}, but its selected module root provides {expected}"
    )]
    VersionMismatch {
        compiler: Box<Path>,
        expected: Box<str>,
        observed: Box<str>,
    },
    #[error(
        "could not configure TypeScript checker for Node {node:?} and module root {module_root:?}: {message}"
    )]
    CheckerConfiguration {
        node: Box<Path>,
        module_root: Box<Path>,
        message: Box<str>,
    },
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::{
        fs,
        os::unix::fs::symlink,
        sync::atomic::{AtomicU64, Ordering},
    };

    static NEXT: AtomicU64 = AtomicU64::new(0);

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "typescript-project-host-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed),
            ));
            fs::create_dir_all(&root).expect("create project fixture");
            Self(root)
        }

        fn install(&self, version: &str) -> PathBuf {
            let modules = self.0.join("node_modules");
            let package = modules.join("typescript");
            fs::create_dir_all(package.join("bin")).expect("create package");
            fs::write(
                package.join("package.json"),
                format!("{{\"name\":\"typescript\",\"version\":\"{version}\"}}"),
            )
            .expect("write package manifest");
            fs::write(package.join("bin/tsc"), "#!/usr/bin/env node\n").expect("write compiler");
            fs::create_dir_all(modules.join(".bin")).expect("create bin links");
            symlink(package.join("bin/tsc"), modules.join(".bin/tsc")).expect("link compiler");
            modules
        }

        fn limits() -> ToolchainProbeLimits {
            ToolchainProbeLimits::new(
                std::time::Duration::from_secs(2),
                std::num::NonZeroUsize::new(4096).expect("nonzero fixture bound"),
            )
            .expect("valid fixture probe limits")
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn project_discovery_accepts_exact_npm_package_link_and_version() {
        let fixture = Fixture::new();
        let modules = fixture.install("5.9.3");
        let result = find_project_typescript(&fixture.0).expect("inspect project installation");
        let ProjectTypeScriptSearch::Found(project) = result else {
            panic!("local TypeScript package should be selected");
        };
        assert_eq!(
            project.module_root,
            fs::canonicalize(modules).expect("canonical root")
        );
        assert_eq!(project.version, "5.9.3");
        assert!(project.compiler.ends_with("typescript/bin/tsc"));
    }

    #[test]
    fn project_discovery_rejects_wrong_shim_and_escape_symlinks() {
        let fixture = Fixture::new();
        let modules = fixture.install("5.9.3");
        let wrong = fixture.0.join("other-tsc");
        fs::write(&wrong, "not TypeScript").expect("write wrong shim target");
        fs::remove_file(modules.join(".bin/tsc")).expect("remove selected link");
        symlink(&wrong, modules.join(".bin/tsc")).expect("install wrong link");
        assert!(matches!(
            find_project_typescript(&fixture.0),
            Err(TypeScriptProjectHostError::CompilerLinkMismatch { .. })
        ));

        fs::remove_file(modules.join(".bin/tsc")).expect("remove wrong link");
        fs::remove_dir_all(modules.join("typescript")).expect("remove package");
        let outside = fixture.0.join("outside-typescript");
        fs::create_dir_all(outside.join("bin")).expect("create outside package");
        fs::write(
            outside.join("package.json"),
            "{\"name\":\"typescript\",\"version\":\"5.9.3\"}",
        )
        .expect("write outside manifest");
        fs::write(outside.join("bin/tsc"), "#!/usr/bin/env node\n")
            .expect("write outside compiler");
        symlink(&outside, modules.join("typescript")).expect("link package outside node_modules");
        assert!(matches!(
            find_project_typescript(&fixture.0),
            Err(TypeScriptProjectHostError::PackageEscapesNodeModules { .. })
        ));
    }

    #[test]
    fn project_discovery_accepts_pnpm_package_symlinks_but_rejects_regular_shims() {
        let fixture = Fixture::new();
        let modules = fixture.0.join("node_modules");
        let pnpm_package = modules.join(".pnpm/typescript@5.9.3/node_modules/typescript");
        fs::create_dir_all(pnpm_package.join("bin")).expect("create pnpm package");
        fs::write(
            pnpm_package.join("package.json"),
            "{\"name\":\"typescript\",\"version\":\"5.9.3\"}",
        )
        .expect("write pnpm package manifest");
        fs::write(pnpm_package.join("bin/tsc"), "#!/usr/bin/env node\n")
            .expect("write pnpm compiler");
        fs::create_dir_all(modules.join(".bin")).expect("create package manager bin");
        symlink(&pnpm_package, modules.join("typescript")).expect("link pnpm package");
        symlink(pnpm_package.join("bin/tsc"), modules.join(".bin/tsc")).expect("link pnpm shim");
        let ProjectTypeScriptSearch::Found(project) =
            find_project_typescript(&fixture.0).expect("inspect pnpm installation")
        else {
            panic!("pnpm TypeScript package should be selected");
        };
        assert!(
            project.compiler.starts_with(
                fs::canonicalize(modules.join(".pnpm")).expect("canonical pnpm store")
            )
        );

        fs::remove_file(modules.join(".bin/tsc")).expect("remove package manager shim");
        fs::write(modules.join(".bin/tsc"), "#!/bin/sh\nexit 0\n").expect("write unknown wrapper");
        assert!(matches!(
            find_project_typescript(&fixture.0),
            Err(TypeScriptProjectHostError::CompilerShimRejected { .. })
        ));
    }

    #[test]
    fn project_discovery_reports_yarn_pnp_without_executing_it() {
        let fixture = Fixture::new();
        fs::write(
            fixture.0.join(".pnp.cjs"),
            "throw new Error('must not execute')",
        )
        .expect("write marker");
        assert!(matches!(
            find_project_typescript(&fixture.0),
            Ok(ProjectTypeScriptSearch::Pnp(_))
        ));
    }

    #[test]
    fn project_discovery_reports_a_broken_typescript_symlink() {
        let fixture = Fixture::new();
        let modules = fixture.0.join("node_modules");
        fs::create_dir_all(&modules).expect("create node_modules");
        symlink(
            fixture.0.join("missing-typescript"),
            modules.join("typescript"),
        )
        .expect("create broken package symlink");
        assert!(matches!(
            find_project_typescript(&fixture.0),
            Err(TypeScriptProjectHostError::PackagePath { .. })
        ));
    }

    #[test]
    fn semver_parser_rejects_malformed_and_preserves_pre_release_identity() {
        assert!(validate_semver("5.9.3").is_some());
        assert!(validate_semver("5.9.3-rc.1+build.2").is_some());
        assert!(validate_semver("05.9.3").is_none());
        assert!(validate_semver("5.9").is_none());
        assert!(validate_semver("5.9.3; malicious").is_none());
        assert_eq!(parse_tsc_version(b"Version 5.9.3\n"), Some("5.9.3".into()));
    }

    #[test]
    fn local_tsc_probe_uses_only_the_admitted_node_directory() {
        use std::os::unix::fs::PermissionsExt;

        let fixture = Fixture::new();
        let modules = fixture.install("5.9.3");
        let node_directory = fixture.0.join("node dir");
        fs::create_dir_all(&node_directory).expect("create Node directory");
        let node = node_directory.join("node");
        fs::write(
            &node,
            r##"#!/bin/sh
test -z "${HOME+x}" || exit 31
test -z "${NODE_OPTIONS+x}" || exit 32
node_directory=${0%/*}
test "$PATH" = "$node_directory" || exit 33
test -f "$1" || exit 34
test "$2" = "--version" || exit 35
printf 'Version 5.9.3\n'
"##,
        )
        .expect("write Node environment fixture");
        fs::set_permissions(&node, fs::Permissions::from_mode(0o755))
            .expect("make Node executable");
        let compiler = modules.join("typescript/bin/tsc");
        let result = crate::application::toolchain_probe::probe_typescript_script_with_node(
            &compiler,
            &node,
            ToolchainProbeLimits::new(
                std::time::Duration::from_secs(2),
                std::num::NonZeroUsize::new(1024).expect("nonzero test bound"),
            )
            .expect("valid test probe limits"),
        );
        let output = result.expect("isolated test process should observe exact Node path");
        assert_eq!(output.as_ref(), b"Version 5.9.3\n");
        // This probes process isolation only; fixture output never creates a ready toolchain row.
    }

    #[test]
    fn project_admission_reports_missing_node_and_compiler_version_mismatch() {
        use std::os::unix::fs::PermissionsExt;

        let fixture = Fixture::new();
        fixture.install("5.9.3");
        let unavailable = TypeScriptProjectHost::new(None, None, None, None, Fixture::limits());
        assert!(matches!(
            unavailable.admit(&fixture.0),
            Err(TypeScriptProjectHostError::NodeUnavailable { .. })
        ));

        let node_directory = fixture.0.join("node path");
        fs::create_dir_all(&node_directory).expect("create Node directory");
        let node = node_directory.join("node");
        fs::write(
            &node,
            "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then printf 'v22.0.0\\n'; else printf 'Version 5.9.2\\n'; fi\n",
        )
        .expect("write bounded Node probe fixture");
        fs::set_permissions(&node, fs::Permissions::from_mode(0o755))
            .expect("make fixture executable");
        let mismatch = TypeScriptProjectHost::new(None, Some(node), None, None, Fixture::limits());
        assert!(matches!(
            mismatch.admit(&fixture.0),
            Err(TypeScriptProjectHostError::VersionMismatch {
                expected,
                observed,
                ..
            }) if expected.as_ref() == "5.9.3" && observed.as_ref() == "5.9.2"
        ));
        // Deliberately mismatched fixture output is only an admission refusal test; it never
        // constructs a Ready authority or validates a synthetic TypeScript installation.
    }

    #[test]
    fn each_project_root_selects_its_own_typescript_installation() {
        let fixture = Fixture::new();
        let first = fixture.0.join("apps/first");
        let second = fixture.0.join("apps/second");
        fs::create_dir_all(&first).expect("create first package root");
        fs::create_dir_all(&second).expect("create second package root");
        let first_modules = first.join("node_modules");
        let second_modules = second.join("node_modules");
        install_at(&first_modules, "5.9.3");
        install_at(&second_modules, "5.8.4");

        let ProjectTypeScriptSearch::Found(first_project) =
            find_project_typescript(&first).expect("inspect first project")
        else {
            panic!("first project TypeScript should be found");
        };
        let ProjectTypeScriptSearch::Found(second_project) =
            find_project_typescript(&second).expect("inspect second project")
        else {
            panic!("second project TypeScript should be found");
        };
        assert_eq!(first_project.version, "5.9.3");
        assert_eq!(second_project.version, "5.8.4");
        assert_ne!(first_project.module_root, second_project.module_root);
    }

    fn install_at(modules: &Path, version: &str) {
        let package = modules.join("typescript");
        fs::create_dir_all(package.join("bin")).expect("create package");
        fs::write(
            package.join("package.json"),
            format!("{{\"name\":\"typescript\",\"version\":\"{version}\"}}"),
        )
        .expect("write package manifest");
        fs::write(package.join("bin/tsc"), "#!/usr/bin/env node\n").expect("write compiler");
        fs::create_dir_all(modules.join(".bin")).expect("create package manager bin");
        symlink(package.join("bin/tsc"), modules.join(".bin/tsc")).expect("link package compiler");
    }
}
