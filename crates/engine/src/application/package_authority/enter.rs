//! Package-authority admission for one explicit language profile.

use std::{path::Path, sync::atomic::Ordering, time::Instant};

use super::{
    PackageAuthorityError, PackageAuthorityOwner, PackageAuthorityRequest, PackageAuthorityStage,
};
use crate::driver::{CompileControl, ResolvedToolchain, ToolchainSelection};
use backend_frontend_csharp::legacy::{CSharpAuthorityControl, CSharpAuthorityRequest};
use backend_frontend_java::legacy::harness::{Harness, HarnessRequest, JavaSource};
use backend_frontend_python::legacy::extract;
use backend_frontend_rust::legacy::RustProject;
use backend_semantic::vocabulary::{LanguageProfile, TypeScriptSource};

/// Builds the one retained authority owner allowed for `request.profile`.
///
/// No authority value is returned after a cancellation or deadline checkpoint.
/// For subprocess adapters that cannot receive the driver's atomic flag, the
/// owner checks before entry and again before accepting their output; this
/// prevents a completed report/image from becoming visible after cancellation
/// or deadline won the enclosing compilation.
pub fn enter_package_authority<'request, 'config>(
    request: PackageAuthorityRequest<'request, 'config>,
) -> Result<PackageAuthorityOwner<'config>, PackageAuthorityError> {
    checkpoint(
        request.control,
        request.profile,
        PackageAuthorityStage::Admission,
    )?;
    let resolved = require_resolved_toolchain(request.toolchain, request.profile)?;
    let relative = request
        .source_path
        .strip_prefix(request.package_root)
        .map_err(|_| PackageAuthorityError::SourceOutsidePackage {
            profile: request.profile,
            package_root: request.package_root.to_path_buf().into_boxed_path(),
            source_path: request.source_path.to_path_buf().into_boxed_path(),
        })?;

    let owner =
        match request.profile {
            profile @ (LanguageProfile::C(_) | LanguageProfile::Cxx(_)) => {
                let project = backend_frontend_clang::ClangProject::open(
                    request.package_root,
                    request.source_path,
                )
                .map_err(PackageAuthorityError::ClangProject)?;
                PackageAuthorityOwner::Clang { profile, project }
            }
            LanguageProfile::TypeScript(profile) => {
                let checker = request.configuration.typescript.ok_or(
                    PackageAuthorityError::AdapterUnavailable {
                        profile: request.profile,
                        stage: PackageAuthorityStage::TypeScriptChecker,
                    },
                )?;
                validate_typescript_entry(request.package_root, relative, profile)?;
                let report = checker
                    .run_in_package(profile, request.source, request.package_root)
                    .map_err(PackageAuthorityError::TypeScript)?;
                checkpoint(
                    request.control,
                    request.profile,
                    PackageAuthorityStage::TypeScriptChecker,
                )?;
                PackageAuthorityOwner::TypeScript { profile, report }
            }
            LanguageProfile::Python(profile) => {
                let pyrefly = request.configuration.python.ok_or(
                    PackageAuthorityError::AdapterUnavailable {
                        profile: request.profile,
                        stage: PackageAuthorityStage::PythonPyrefly,
                    },
                )?;
                let syntax = extract(request.source, profile)
                    .map_err(PackageAuthorityError::PythonSyntax)?;
                checkpoint(
                    request.control,
                    request.profile,
                    PackageAuthorityStage::PythonSyntax,
                )?;
                let report = pyrefly
                    .analyze(request.source, profile, &syntax)
                    .map_err(PackageAuthorityError::PythonPyrefly)?;
                checkpoint(
                    request.control,
                    request.profile,
                    PackageAuthorityStage::PythonPyrefly,
                )?;
                PackageAuthorityOwner::Python {
                    profile: request.profile,
                    report,
                }
            }
            LanguageProfile::Rust(profile) => {
                let configuration = request.configuration.rust.ok_or(
                    PackageAuthorityError::AdapterUnavailable {
                        profile: request.profile,
                        stage: PackageAuthorityStage::RustProject,
                    },
                )?;
                if configuration.toolchain.tool.as_path() != resolved.as_ref() {
                    return Err(PackageAuthorityError::RustToolchainExecutableMismatch {
                        profile: request.profile,
                        configured: configuration.toolchain.tool.clone().into_boxed_path(),
                        resolved: resolved.as_ref().to_path_buf().into_boxed_path(),
                    });
                }
                let project = RustProject::open_with_source(
                    request.package_root,
                    request.source_path,
                    configuration.toolchain,
                    profile,
                )
                .map_err(PackageAuthorityError::RustProject)?;
                checkpoint(
                    request.control,
                    request.profile,
                    PackageAuthorityStage::RustProject,
                )?;
                PackageAuthorityOwner::Rust {
                    profile: request.profile,
                    project,
                    maximum_source_bytes: configuration.maximum_source_bytes,
                    features: configuration.features,
                }
            }
            LanguageProfile::Go(_) => {
                let oracle =
                    request
                        .configuration
                        .go
                        .ok_or(PackageAuthorityError::AdapterUnavailable {
                            profile: request.profile,
                            stage: PackageAuthorityStage::GoOracle,
                        })?;
                // Scoped to exactly the package that owns `source_path`:
                // sibling packages are import context only and are never
                // serialized, so a module whose subpackages share a
                // declaration spelling (e.g. `errgroup.Group` next to
                // `singleflight.Group` in `golang.org/x/sync`) cannot inject
                // a coordinate-free `DuplicateDeclarationIdentity` collision
                // into the selected package's image.
                let image = oracle
                    .authority_image_for_package(request.source_path, request.package_root)
                    .map_err(PackageAuthorityError::GoOracle)?;
                let image = retain_image(
                    image,
                    request.configuration.maximum_image_bytes,
                    request.profile,
                    PackageAuthorityStage::GoOracle,
                )?;
                checkpoint(
                    request.control,
                    request.profile,
                    PackageAuthorityStage::GoOracle,
                )?;
                PackageAuthorityOwner::Go {
                    profile: request.profile,
                    image,
                }
            }
            LanguageProfile::Java(profile) => {
                let configuration = request.configuration.java.ok_or(
                    PackageAuthorityError::AdapterUnavailable {
                        profile: request.profile,
                        stage: PackageAuthorityStage::JavaHarness,
                    },
                )?;
                let mut session = Harness::new().map_err(PackageAuthorityError::JavaHarness)?;
                session
                    .prepare(configuration.toolchain)
                    .map_err(PackageAuthorityError::JavaHarness)?;
                checkpoint(
                    request.control,
                    request.profile,
                    PackageAuthorityStage::JavaHarness,
                )?;
                let source = [JavaSource {
                    name: relative,
                    bytes: request.source,
                }];
                let mut image = Vec::new();
                session
                    .image(
                        configuration.toolchain,
                        HarnessRequest {
                            sources: &source,
                            classpath: configuration.classpath,
                            release: profile,
                        },
                        &mut image,
                    )
                    .map_err(PackageAuthorityError::JavaHarness)?;
                let image = retain_image(
                    image,
                    request.configuration.maximum_image_bytes,
                    request.profile,
                    PackageAuthorityStage::JavaHarness,
                )?;
                checkpoint(
                    request.control,
                    request.profile,
                    PackageAuthorityStage::JavaHarness,
                )?;
                PackageAuthorityOwner::Java {
                    profile: request.profile,
                    image,
                    _session: session,
                }
            }
            LanguageProfile::CSharp(profile) => {
                let configuration = request.configuration.csharp.ok_or(
                    PackageAuthorityError::AdapterUnavailable {
                        profile: request.profile,
                        stage: PackageAuthorityStage::CSharpRoslyn,
                    },
                )?;
                let image = configuration
                    .producer
                    .produce(CSharpAuthorityRequest {
                        package_root: request.package_root,
                        source_path: request.source_path,
                        source: request.source,
                        profile,
                        native_tool: resolved.tool,
                        toolchain: resolved.as_ref(),
                        control: CSharpAuthorityControl {
                            deadline: request.control.deadline,
                            cancelled: request.control.cancelled,
                        },
                        configuration: configuration.configuration,
                    })
                    .map_err(PackageAuthorityError::CSharp)?;
                let image = retain_image(
                    image.into_bytes().into_vec(),
                    request.configuration.maximum_image_bytes,
                    request.profile,
                    PackageAuthorityStage::CSharpRoslyn,
                )?;
                checkpoint(
                    request.control,
                    request.profile,
                    PackageAuthorityStage::CSharpRoslyn,
                )?;
                PackageAuthorityOwner::CSharp {
                    profile: request.profile,
                    image,
                }
            }
        };
    checkpoint(
        request.control,
        request.profile,
        PackageAuthorityStage::Admission,
    )?;
    Ok(owner)
}

fn checkpoint(
    control: CompileControl<'_>,
    profile: LanguageProfile,
    stage: PackageAuthorityStage,
) -> Result<(), PackageAuthorityError> {
    if control.cancelled.load(Ordering::Acquire) {
        return Err(PackageAuthorityError::Cancelled { profile, stage });
    }
    if Instant::now() >= control.deadline {
        return Err(PackageAuthorityError::Deadline { profile, stage });
    }
    Ok(())
}

fn require_resolved_toolchain<'toolchain>(
    selection: ToolchainSelection<'toolchain>,
    profile: LanguageProfile,
) -> Result<ResolvedToolchain<'toolchain>, PackageAuthorityError> {
    match selection {
        ToolchainSelection::ResolvedNative(resolved) => Ok(resolved),
        ToolchainSelection::ExplicitlyUnavailable { tool } => {
            Err(PackageAuthorityError::ToolchainUnavailable { profile, tool })
        }
    }
}

fn validate_typescript_entry(
    package_root: &Path,
    relative: &Path,
    profile: TypeScriptSource,
) -> Result<(), PackageAuthorityError> {
    let expected = match profile {
        TypeScriptSource::TypeScript => Path::new("index.ts"),
        TypeScriptSource::Tsx => Path::new("index.tsx"),
    };
    if relative == expected {
        return Ok(());
    }
    Err(PackageAuthorityError::TypeScriptEntryPath {
        package_root: package_root.to_path_buf().into_boxed_path(),
        source_relative: relative.to_path_buf().into_boxed_path(),
        expected: expected.to_path_buf().into_boxed_path(),
    })
}

fn retain_image(
    image: Vec<u8>,
    maximum: usize,
    profile: LanguageProfile,
    stage: PackageAuthorityStage,
) -> Result<Box<[u8]>, PackageAuthorityError> {
    if image.len() > maximum {
        return Err(PackageAuthorityError::ImageTooLarge {
            profile,
            stage,
            actual: image.len(),
            maximum,
        });
    }
    Ok(image.into_boxed_slice())
}
