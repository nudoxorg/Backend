//! One single-request local compiler specialization over explicit local ownership.

use nudox_compile_driver::{
    CompileControl, CompileOutput, CompileRequest, CompileScratch, ToolchainSelection, compile,
};
use nudox_compile_publication::{
    PublicationScratch, PublishControl, PublishedCompilation, publish_compiled,
};
use nudox_compile_registry::{AdapterRoute, FullRegistry};
use nudox_durable_journal::{DurablePublisher, PublicationLimits, PublicationPaths, ShutdownError};
use nudox_id::{ArtifactId, ContentId, IrFragmentDomain, IrFragmentEncoding, SourceFactDomain};
use wave_application_core::{
    CompilerCapability, CompilerReadiness, CompilerRequest as ApplicationCompilerRequest,
    CompilerTerminal, GeneratedArtifact, PublicationAuthority, SourceAuthority,
};

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

    #[allow(
        clippy::result_large_err,
        reason = "the core boundary deliberately retains exact bounded terminal facts; boxing would allocate on the compilation failure path"
    )]
    fn compile_and_publish(
        &mut self,
        request: ApplicationCompilerRequest<'_>,
    ) -> Result<GeneratedArtifact, CompilerTerminal> {
        let source = request_source(request)?;
        let toolchain = self.toolchain(request, source)?;
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

    #[allow(
        clippy::result_large_err,
        reason = "the closed terminal retains source, language, stage, and selected-tool authorities; boxing this early validation path would allocate before native work"
    )]
    fn toolchain(
        &self,
        request: ApplicationCompilerRequest<'_>,
        source: SourceAuthority,
    ) -> Result<ToolchainSelection<'path>, CompilerTerminal> {
        let route = FullRegistry
            .route(request.language, request.stage)
            .map_err(|_| CompilerTerminal::UnsupportedStage {
                source,
                language: request.language,
                stage: request.stage,
            })?;
        let selected = route_tool(route);
        self.config
            .toolchains
            .select(selected)
            .ok_or(CompilerTerminal::Toolchain {
                source,
                language: request.language,
                stage: request.stage,
                selected,
                configured: None,
            })
    }
}

impl CompilerCapability for LocalCompiler<'_, '_, '_> {
    fn readiness(&self) -> CompilerReadiness {
        CompilerReadiness::Ready
    }

    #[allow(
        clippy::result_large_err,
        reason = "the core boundary intentionally returns a fixed-capacity terminal to retain exact facts without failure-path allocation"
    )]
    fn generate(
        &mut self,
        request: ApplicationCompilerRequest<'_>,
    ) -> Result<GeneratedArtifact, CompilerTerminal> {
        self.compile_and_publish(request)
    }
}

const fn generated(
    source: SourceAuthority,
    recipe: nudox_compile_vocab::CompileRecipeFact,
    fragment: ArtifactId<IrFragmentEncoding, IrFragmentDomain>,
    publication: &PublishedCompilation,
) -> GeneratedArtifact {
    GeneratedArtifact {
        source,
        recipe,
        fragment,
        publication: PublicationAuthority {
            generation: wave_application_core::GenerationAuthority {
                pinned_root: publication.publication.generation.pinned_root,
                dep_set: publication.publication.generation.dep_set,
            },
            manifest: publication.manifest.identity,
            binding: publication.binding.identity,
        },
    }
}

#[allow(
    clippy::result_large_err,
    reason = "the closed terminal preserves the exact source-width rejection; boxing would allocate before a compiler capability is selected"
)]
fn request_source(
    request: ApplicationCompilerRequest<'_>,
) -> Result<SourceAuthority, CompilerTerminal> {
    let byte_len =
        u32::try_from(request.source.len()).map_err(|_| CompilerTerminal::SourceLength {
            actual: request.source.len(),
        })?;
    Ok(SourceAuthority {
        identity: ContentId::<SourceFactDomain>::from_canonical_bytes(request.source.as_bytes()),
        byte_len,
    })
}

const fn route_tool(route: AdapterRoute) -> nudox_compile_vocab::NativeTool {
    match route {
        AdapterRoute::Native { tool } | AdapterRoute::ToolingUnavailable { tool } => tool,
    }
}
