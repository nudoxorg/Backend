//! Defines compiler behavior for `compiler-application`, whose purpose is to bind application requests to native compilation and durable publication.
//! This module owns the compiler invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! One single-request local compiler specialization over explicit local ownership.

use compiler_driver::{
    CompileControl, CompileOutput, CompileRequest, CompileScratch, CompiledIr, DeclarationScope,
    ToolchainSelection, compile_ir, compile_semantic as compile_fused_semantic,
};
use compiler_publication::{
    OpenSemanticPublicationScratch, PublishControl, PublishedCompilation,
    SemanticImageArtifactFacts, SemanticPublicationScratch, open_published_semantic,
    publish_semantic,
};
use compiler_registry::{AdapterRoute, FullRegistry};
use heart_identity::{
    ArtifactId, ContentId, IrFragmentDomain, IrFragmentEncoding, SourceFactDomain,
};
use interface_core::{
    CompilerCapability, CompilerReadiness, CompilerRequest as ApplicationCompilerRequest,
    CompilerTerminal, GeneratedArtifact, PackageCompilePhase, PackageCompileRequest,
    PackageDeclarationScopeCause, PackageSourceCause, PublicationAuthority, SemanticImageAuthority,
    SourceAuthority,
};
use server_journal::{DurablePublisher, PublicationLimits, PublicationPaths, ShutdownError};

use crate::{
    LocalCompilerConfig, LocalCompilerOpenError, LocalCompilerPath, LocalCompilerScratch,
    LocalPackageRootSet, package_source,
    terminal::{compile_terminal, source_authority},
};

/// Concrete local compiler with bounded explicit native toolchains, publisher, paths, and scratch.
pub struct LocalCompiler<'path, 'scratch, 'cancel> {
    config: LocalCompilerConfig<'path, 'cancel>,
    package_roots: LocalPackageRootSet<'path>,
    publisher: DurablePublisher,
    scratch: &'scratch mut LocalCompilerScratch,
}

impl<'path, 'scratch, 'cancel> LocalCompiler<'path, 'scratch, 'cancel> {
    /// Creates the only durable publication owner used by this configured local compiler.
    ///
    /// The caller supplies a validated explicit toolchain table, artifact directory, journal
    /// directory, native work directory, cancellation authority, and all reusable scratch. No
    /// process-global discovery or heap-backed per-request arena is introduced here.
    ///
    /// # Errors
    ///
    /// Returns [`LocalCompilerOpenError`] when one explicit local path is relative or the durable
    /// publisher cannot establish its journal owner.
    pub fn create(
        config: LocalCompilerConfig<'path, 'cancel>,
        limits: PublicationLimits,
        scratch: &'scratch mut LocalCompilerScratch,
    ) -> Result<Self, LocalCompilerOpenError> {
        Self::create_with_package_roots(config, LocalPackageRootSet::EMPTY, limits, scratch)
    }

    /// Creates a durable compiler with explicit local package-store roots.
    ///
    /// The root table has already proven absolute, unique, ordered ecosystem ownership. No
    /// package command consults process environment or walks outside these roots.
    ///
    /// # Errors
    ///
    /// Returns the same exact configuration and durable-publisher terminal as [`Self::create`].
    pub fn create_with_package_roots(
        config: LocalCompilerConfig<'path, 'cancel>,
        package_roots: LocalPackageRootSet<'path>,
        limits: PublicationLimits,
        scratch: &'scratch mut LocalCompilerScratch,
    ) -> Result<Self, LocalCompilerOpenError> {
        for (path, role) in [
            (config.artifact_directory, LocalCompilerPath::Artifacts),
            (config.journal_directory, LocalCompilerPath::Journal),
            (config.native_work_directory, LocalCompilerPath::NativeWork),
        ] {
            if !path.is_absolute() {
                return Err(LocalCompilerOpenError::RelativePath { path: role });
            }
        }
        let journal = PublicationPaths::in_directory(config.journal_directory);
        let publisher = DurablePublisher::create(&journal, limits)
            .map_err(LocalCompilerOpenError::Publisher)?;
        Ok(Self {
            config,
            package_roots,
            publisher,
            scratch,
        })
    }

    /// Stops durable publication admission and joins its earned single owner.
    ///
    /// # Errors
    ///
    /// Returns [`ShutdownError`] when the durable publisher cannot finish its owned shutdown.
    pub fn shutdown(self) -> Result<(), ShutdownError> {
        self.publisher.shutdown()
    }

    /// Compiles into the queryable image derived from the same admitted fact
    /// lane as durable generation. The returned owner may be borrowed by
    /// renderers, graph queries, index projections, and IR-VCS for its
    /// complete lifetime.
    ///
    /// # Errors
    ///
    /// Returns the same closed application terminal as durable compilation
    /// when routing, deadlines, native parsing, or semantic IR construction fail.
    #[allow(
        clippy::result_large_err,
        reason = "the application terminal deliberately retains exact native failure evidence"
    )]
    pub fn compile_semantic(
        &mut self,
        request: ApplicationCompilerRequest<'_>,
    ) -> Result<CompiledIr, CompilerTerminal> {
        let source = request_source(request).map_err(source_terminal)?;
        let toolchain = self
            .toolchain(request)
            .map_err(|cause| toolchain_terminal(source, request, cause))?;
        let deadline = self.config.control.deadline().map_err(|timeout| {
            CompilerTerminal::DeadlineConstruction {
                source,
                language: request.profile.language(),
                stage: request.stage,
                timeout: *timeout,
            }
        })?;
        compile_ir(
            CompileRequest {
                profile: request.profile,
                stage: request.stage,
                source: request.source.as_bytes(),
                declaration_scope: DeclarationScope::standalone(request.profile),
                toolchain,
                authority: compiler_driver::SemanticAuthorityInput::None,
                control: CompileControl {
                    deadline,
                    cancelled: self.config.control.cancelled,
                },
            },
            CompileScratch {
                diagnostic_output: &mut self.scratch.diagnostic_output,
                native_work: self.config.native_work_directory,
            },
        )
        .map_err(compile_terminal)
    }

    #[allow(
        clippy::result_large_err,
        reason = "this single outer application boundary retains source, recipe, and publication authorities inline; any emitted native diagnostic is already the only cold boxed fact"
    )]
    fn compile_and_publish(
        &mut self,
        request: ApplicationCompilerRequest<'_>,
    ) -> Result<GeneratedArtifact, CompilerTerminal> {
        let mut ignored = |_| {};
        self.compile_scoped_and_publish(
            request,
            DeclarationScope::standalone(request.profile),
            compiler_driver::SemanticAuthorityInput::None,
            &mut ignored,
        )
    }

    #[allow(
        clippy::result_large_err,
        reason = "this single outer application boundary retains source, recipe, and publication authorities inline; any emitted native diagnostic is already the only cold boxed fact"
    )]
    fn compile_scoped_and_publish<Progress>(
        &mut self,
        request: ApplicationCompilerRequest<'_>,
        declaration_scope: DeclarationScope<'_>,
        authority: compiler_driver::SemanticAuthorityInput<'_>,
        progress: &mut Progress,
    ) -> Result<GeneratedArtifact, CompilerTerminal>
    where
        Progress: FnMut(PackageCompilePhase),
    {
        let source = request_source(request).map_err(source_terminal)?;
        let toolchain = self
            .toolchain(request)
            .map_err(|cause| toolchain_terminal(source, request, cause))?;
        let deadline = self.config.control.deadline().map_err(|timeout| {
            CompilerTerminal::DeadlineConstruction {
                source,
                language: request.profile.language(),
                stage: request.stage,
                timeout: *timeout,
            }
        })?;
        progress(PackageCompilePhase::Lower);
        let compiled = compile_fused_semantic(
            CompileRequest {
                profile: request.profile,
                stage: request.stage,
                source: request.source.as_bytes(),
                declaration_scope,
                toolchain,
                authority,
                control: CompileControl {
                    deadline,
                    cancelled: self.config.control.cancelled,
                },
            },
            CompileScratch {
                diagnostic_output: &mut self.scratch.diagnostic_output,
                native_work: self.config.native_work_directory,
            },
            CompileOutput {
                fragment_output: &mut self.scratch.fragment_output,
            },
        )
        .map_err(compile_terminal)?;
        let source = source_authority(compiled.artifact.source);
        let recipe = compiled.artifact.recipe;
        let fragment = ArtifactId::<IrFragmentEncoding, IrFragmentDomain>::from_encoded_bytes(
            compiled.artifact.fragment.as_ref(),
        );
        let semantic_length =
            compiler_ir::full_semantic_image_len(&compiled.ir).map_err(|cause| {
                crate::terminal::semantic_publication_terminal(
                    source,
                    recipe,
                    compiler_publication::PublishSemanticError::ImageMeasure {
                        ordinal: 0,
                        source: cause,
                    },
                )
            })?;
        self.scratch
            .semantic_image_output
            .resize(semantic_length, 0);
        progress(PackageCompilePhase::Publish);
        let publication = publish_semantic(
            &self.publisher,
            self.config.artifact_directory,
            core::slice::from_ref(&compiled),
            PublishControl::Observe(self.config.control.cancelled),
            SemanticPublicationScratch {
                manifest_output: &mut self.scratch.manifest_output,
                manifest_facts: &mut self.scratch.manifest_facts,
                ordinals: &mut self.scratch.ordinals,
                semantic_image_plan: &mut self.scratch.semantic_image_plan,
                semantic_image_output: &mut self.scratch.semantic_image_output,
                locality_output: &mut self.scratch.locality_output,
                binding_output: &mut self.scratch.binding_output,
            },
        )
        .map_err(|error| crate::terminal::semantic_publication_terminal(source, recipe, error))?;
        drop(compiled);

        progress(PackageCompilePhase::Reopen);
        let opened = open_published_semantic(
            &self.publisher,
            self.config.artifact_directory,
            OpenSemanticPublicationScratch {
                manifest_output: &mut self.scratch.manifest_output,
                manifest_facts: &mut self.scratch.manifest_facts,
                fragment_output: &mut self.scratch.fragment_output,
                semantic_image_output: &mut self.scratch.semantic_image_output,
                locality_output: &mut self.scratch.locality_output,
            },
        )
        .map_err(|error| crate::terminal::semantic_reopen_terminal(source, recipe, error))?
        .ok_or_else(|| crate::terminal::semantic_reopen_absent(source, recipe))?;
        let mut artifacts = opened.artifacts();
        let semantic = artifacts
            .next()
            .ok_or_else(|| crate::terminal::semantic_reopen_absent(source, recipe))?
            .map_err(|error| crate::terminal::semantic_artifact_terminal(source, recipe, error))?;
        let semantic_facts = semantic
            .fragment
            .facts
            .semantic_image
            .ok_or_else(|| crate::terminal::semantic_reopen_absent(source, recipe))?;
        if artifacts.next().is_some() {
            return Err(crate::terminal::semantic_reopen_cardinality(source, recipe));
        }
        Ok(generated(
            source,
            recipe,
            fragment,
            semantic_facts,
            &publication,
        ))
    }

    fn toolchain(
        &self,
        request: ApplicationCompilerRequest<'_>,
    ) -> Result<ToolchainSelection<'path>, ToolchainRouteError> {
        let route = FullRegistry
            .route(request.profile.language(), request.stage)
            .map_err(|_| ToolchainRouteError::UnsupportedStage)?;
        select_toolchain(self.config.toolchains, route)
    }
}

fn select_toolchain(
    toolchains: crate::LocalToolchainSet<'_>,
    route: AdapterRoute,
) -> Result<ToolchainSelection<'_>, ToolchainRouteError> {
    match route {
        AdapterRoute::Native { tool } => toolchains
            .select(tool)
            .ok_or(ToolchainRouteError::Missing { selected: tool }),
        AdapterRoute::ToolingUnavailable { tool } => {
            Err(ToolchainRouteError::ToolingUnavailable { tool })
        }
    }
}

impl CompilerCapability for LocalCompiler<'_, '_, '_> {
    fn readiness(&self) -> CompilerReadiness {
        CompilerReadiness::Ready
    }

    fn generate(
        &mut self,
        request: ApplicationCompilerRequest<'_>,
    ) -> Result<GeneratedArtifact, CompilerTerminal> {
        self.compile_and_publish(request)
    }

    fn compile_package<Progress>(
        &mut self,
        request: &PackageCompileRequest,
        progress: &mut Progress,
    ) -> Result<GeneratedArtifact, CompilerTerminal>
    where
        Progress: FnMut(PackageCompilePhase),
    {
        let target = request.as_ref().identity;
        if self
            .config
            .control
            .cancelled
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return Err(CompilerTerminal::PackageCancelled {
                target,
                phase: PackageCompilePhase::Locate,
            });
        }
        progress(PackageCompilePhase::Locate);
        let resolved =
            package_source::resolve(self.package_roots, request.as_ref()).map_err(|cause| {
                CompilerTerminal::PackageSource {
                    target,
                    phase: PackageCompilePhase::Locate,
                    cause,
                }
            })?;
        progress(PackageCompilePhase::EnterSource);
        let source = std::str::from_utf8(&resolved.bytes).map_err(|cause| {
            CompilerTerminal::PackageSource {
                target,
                phase: PackageCompilePhase::EnterSource,
                cause: PackageSourceCause::InvalidUtf8 {
                    valid_up_to: cause.valid_up_to(),
                    error_len: cause
                        .error_len()
                        .and_then(|length| u8::try_from(length).ok()),
                },
            }
        })?;
        let package = request.as_ref();
        let package_name_start = package
            .namespace
            .map_or(package.name.start, |namespace| namespace.start);
        let package_name = &package[interface_core::PackageTextRange {
            start: package_name_start,
            end: package.name.end,
        }];
        let lineage = compiler_ir::PackageLineage::new(
            package_ecosystem_name(package.ecosystem),
            package_name,
        )
        .map_err(|cause| CompilerTerminal::PackageSource {
            target,
            phase: PackageCompilePhase::EnterSource,
            cause: PackageSourceCause::DeclarationScope {
                cause: lineage_cause(cause),
            },
        })?;
        let scope = DeclarationScope::new(lineage, &resolved.relative_source).map_err(|cause| {
            CompilerTerminal::PackageSource {
                target,
                phase: PackageCompilePhase::EnterSource,
                cause: PackageSourceCause::DeclarationScope {
                    cause: declaration_scope_cause(cause),
                },
            }
        })?;
        progress(PackageCompilePhase::Authority);
        let application_request = ApplicationCompilerRequest {
            profile: request.target.profile,
            stage: request.target.stage,
            source,
        };
        match request.target.profile {
            compiler_vocabulary::LanguageProfile::C(_)
            | compiler_vocabulary::LanguageProfile::Cxx(_) => self.compile_scoped_and_publish(
                application_request,
                scope,
                compiler_driver::SemanticAuthorityInput::None,
                progress,
            ),
            compiler_vocabulary::LanguageProfile::Rust(_)
            | compiler_vocabulary::LanguageProfile::TypeScript(_)
            | compiler_vocabulary::LanguageProfile::Python(_)
            | compiler_vocabulary::LanguageProfile::Go(_)
            | compiler_vocabulary::LanguageProfile::Java(_)
            | compiler_vocabulary::LanguageProfile::CSharp(_) => {
                let _ = (
                    resolved.package_root,
                    resolved.source_path,
                    application_request,
                    scope,
                );
                Err(CompilerTerminal::Unavailable {
                    language: request.target.profile.language(),
                    stage: request.target.stage,
                })
            }
        }
    }
}

const fn package_ecosystem_name(ecosystem: interface_core::PackageEcosystem) -> &'static str {
    match ecosystem {
        interface_core::PackageEcosystem::Cargo => "cargo",
        interface_core::PackageEcosystem::Npm => "npm",
        interface_core::PackageEcosystem::Pypi => "pypi",
        interface_core::PackageEcosystem::Golang => "golang",
        interface_core::PackageEcosystem::Maven => "maven",
        interface_core::PackageEcosystem::Nuget => "nuget",
        interface_core::PackageEcosystem::Generic => "generic",
    }
}

const fn lineage_cause(cause: compiler_ir::PackageLineageFault) -> PackageDeclarationScopeCause {
    match cause {
        compiler_ir::PackageLineageFault::EmptyEcosystem => {
            PackageDeclarationScopeCause::EmptyEcosystem
        }
        compiler_ir::PackageLineageFault::EmptyName => PackageDeclarationScopeCause::EmptyPackage,
        compiler_ir::PackageLineageFault::SeparatorInEcosystem => {
            PackageDeclarationScopeCause::EcosystemSeparator
        }
        compiler_ir::PackageLineageFault::SeparatorInName => {
            PackageDeclarationScopeCause::PackageSeparator
        }
        compiler_ir::PackageLineageFault::Backslash { segment } => {
            PackageDeclarationScopeCause::LineageBackslash { segment }
        }
    }
}

const fn declaration_scope_cause(
    cause: compiler_ir::DeclarationKeyFault,
) -> PackageDeclarationScopeCause {
    match cause {
        compiler_ir::DeclarationKeyFault::Path(compiler_ir::DeclarationPathFault::Empty) => {
            PackageDeclarationScopeCause::EmptySourcePath
        }
        compiler_ir::DeclarationKeyFault::Path(compiler_ir::DeclarationPathFault::Backslash) => {
            PackageDeclarationScopeCause::SourceBackslash
        }
        compiler_ir::DeclarationKeyFault::EmptyName => PackageDeclarationScopeCause::EmptyPackage,
    }
}

const fn generated(
    source: SourceAuthority,
    recipe: compiler_vocabulary::CompileRecipeFact,
    fragment: ArtifactId<IrFragmentEncoding, IrFragmentDomain>,
    semantic_image: SemanticImageArtifactFacts,
    publication: &PublishedCompilation,
) -> GeneratedArtifact {
    GeneratedArtifact {
        source,
        recipe,
        fragment,
        semantic_image: SemanticImageAuthority {
            identity: semantic_image.identity,
            byte_len: semantic_image.byte_length,
        },
        publication: PublicationAuthority {
            generation: interface_core::GenerationAuthority {
                pinned_root: publication.publication.generation.pinned_root,
                dep_set: publication.publication.generation.dep_set,
            },
            manifest: publication.manifest.identity,
            binding: publication.binding.identity,
        },
    }
}

fn request_source(request: ApplicationCompilerRequest<'_>) -> Result<SourceAuthority, SourceError> {
    let byte_len = u32::try_from(request.source.len()).map_err(|_| SourceError::Length {
        actual: request.source.len(),
    })?;
    Ok(SourceAuthority {
        identity: ContentId::<SourceFactDomain>::from_canonical_bytes(request.source.as_bytes()),
        byte_len,
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SourceError {
    Length { actual: usize },
}

const fn source_terminal(cause: SourceError) -> CompilerTerminal {
    match cause {
        SourceError::Length { actual } => CompilerTerminal::SourceLength { actual },
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ToolchainRouteError {
    UnsupportedStage,
    Missing {
        selected: compiler_vocabulary::NativeTool,
    },
    ToolingUnavailable {
        tool: compiler_vocabulary::NativeTool,
    },
}

const fn toolchain_terminal(
    source: SourceAuthority,
    request: ApplicationCompilerRequest<'_>,
    cause: ToolchainRouteError,
) -> CompilerTerminal {
    match cause {
        ToolchainRouteError::UnsupportedStage => CompilerTerminal::UnsupportedStage {
            source,
            language: request.profile.language(),
            stage: request.stage,
        },
        ToolchainRouteError::Missing { selected } => CompilerTerminal::Toolchain {
            source,
            language: request.profile.language(),
            stage: request.stage,
            selected,
            configured: None,
        },
        ToolchainRouteError::ToolingUnavailable { tool } => CompilerTerminal::ToolingUnavailable {
            source,
            language: request.profile.language(),
            stage: request.stage,
            tool,
        },
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use compiler_driver::{ResolvedToolchain, ToolchainResolutionError, ToolchainSelection};
    use compiler_registry::AdapterRoute;
    use compiler_vocabulary::NativeTool;
    use thiserror::Error;

    use crate::{LocalToolchainSet, LocalToolchainSetError};

    use super::{ToolchainRouteError, select_toolchain};

    #[derive(Debug, Error)]
    enum RouteTestError {
        #[error("resolved toolchain fixture was rejected")]
        Resolved(#[from] ToolchainResolutionError),
        #[error("toolchain table fixture was rejected")]
        Table(#[from] LocalToolchainSetError),
        #[error("registry-unavailable route selected a resolved local toolchain")]
        Accepted,
        #[error("registry-unavailable route retained the wrong typed route cause")]
        Route { observed: ToolchainRouteError },
    }

    #[test]
    fn unavailable_registry_route_cannot_execute_a_resolved_local_toolchain()
    -> Result<(), RouteTestError> {
        let selections = [ToolchainSelection::ResolvedNative(
            ResolvedToolchain::from_version(
                NativeTool::GoCompiler,
                Path::new("/caller/probed/go"),
                b"go-version-provenance",
            )?,
        )];
        match select_toolchain(
            LocalToolchainSet::validate(&selections)?,
            AdapterRoute::ToolingUnavailable {
                tool: NativeTool::GoCompiler,
            },
        ) {
            Err(ToolchainRouteError::ToolingUnavailable {
                tool: NativeTool::GoCompiler,
            }) => Ok(()),
            Err(observed) => Err(RouteTestError::Route { observed }),
            Ok(_) => Err(RouteTestError::Accepted),
        }
    }
}
