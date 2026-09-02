//! Defines compiler behavior for `compiler-application`, whose purpose is to bind application requests to native compilation and durable publication.
//! This module owns the compiler invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! One single-request local compiler specialization over explicit local ownership.

use compiler_driver::{
    CompileControl, CompileOutput, CompileRequest, CompileScratch, CompiledIr, ToolchainSelection,
    compile, compile_ir,
};
use compiler_publication::{
    PublicationScratch, PublishControl, PublishedCompilation, publish_compiled,
};
use compiler_registry::{AdapterRoute, FullRegistry};
use heart_identity::{
    ArtifactId, ContentId, IrFragmentDomain, IrFragmentEncoding, SourceFactDomain,
};
use interface_core::{
    CompilerCapability, CompilerReadiness, CompilerRequest as ApplicationCompilerRequest,
    CompilerTerminal, GeneratedArtifact, PublicationAuthority, SourceAuthority,
};
use server_journal::{DurablePublisher, PublicationLimits, PublicationPaths, ShutdownError};

use crate::{
    LocalCompilerConfig, LocalCompilerOpenError, LocalCompilerPath, LocalCompilerScratch,
    terminal::{compile_terminal, publication_terminal, source_authority},
};

/// Concrete local compiler with bounded explicit native toolchains, publisher, paths, and scratch.
pub struct LocalCompiler<'path, 'scratch, 'cancel> {
    config: LocalCompilerConfig<'path, 'cancel>,
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

    /// Compiles directly into the canonical in-memory IR used by renderers,
    /// graph queries, index projections, and IR-VCS.
    ///
    /// Unlike [`CompilerCapability::generate`], this path does not create a
    /// fragment byte stream or publish an artifact. The returned owner may be
    /// borrowed by every downstream stage for its complete lifetime.
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
                language: request.language,
                stage: request.stage,
                timeout: *timeout,
            }
        })?;
        compile_ir(
            CompileRequest {
                language: request.language,
                stage: request.stage,
                source: request.source.as_bytes(),
                toolchain,
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
        let source = request_source(request).map_err(source_terminal)?;
        let toolchain = self
            .toolchain(request)
            .map_err(|cause| toolchain_terminal(source, request, cause))?;
        let deadline = self.config.control.deadline().map_err(|timeout| {
            CompilerTerminal::DeadlineConstruction {
                source,
                language: request.language,
                stage: request.stage,
                timeout: *timeout,
            }
        })?;
        let compiled = compile(
            CompileRequest {
                language: request.language,
                stage: request.stage,
                source: request.source.as_bytes(),
                toolchain,
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
        let source = source_authority(compiled.source);
        let recipe = compiled.recipe;
        let fragment = ArtifactId::<IrFragmentEncoding, IrFragmentDomain>::from_encoded_bytes(
            compiled.fragment.as_ref(),
        );
        let publication = publish_compiled(
            &self.publisher,
            self.config.artifact_directory,
            core::slice::from_ref(&compiled),
            PublishControl::Observe(self.config.control.cancelled),
            PublicationScratch {
                manifest_output: &mut self.scratch.manifest_output,
                manifest_facts: &mut self.scratch.manifest_facts,
                ordinals: &mut self.scratch.ordinals,
                locality_output: &mut self.scratch.locality_output,
                binding_output: &mut self.scratch.binding_output,
            },
        )
        .map_err(|error| publication_terminal(source, recipe, error))?;
        Ok(generated(source, recipe, fragment, &publication))
    }

    fn toolchain(
        &self,
        request: ApplicationCompilerRequest<'_>,
    ) -> Result<ToolchainSelection<'path>, ToolchainRouteError> {
        let route = FullRegistry
            .route(request.language, request.stage)
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
}

const fn generated(
    source: SourceAuthority,
    recipe: compiler_vocabulary::CompileRecipeFact,
    fragment: ArtifactId<IrFragmentEncoding, IrFragmentDomain>,
    publication: &PublishedCompilation,
) -> GeneratedArtifact {
    GeneratedArtifact {
        source,
        recipe,
        fragment,
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
            language: request.language,
            stage: request.stage,
        },
        ToolchainRouteError::Missing { selected } => CompilerTerminal::Toolchain {
            source,
            language: request.language,
            stage: request.stage,
            selected,
            configured: None,
        },
        ToolchainRouteError::ToolingUnavailable { tool } => CompilerTerminal::ToolingUnavailable {
            source,
            language: request.language,
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
