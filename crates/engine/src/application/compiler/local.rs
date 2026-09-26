//! Configured local compiler owner.
//!
//! Package members lower against explicit roots and sidecar authority, then
//! publish as one generation. Returned images are copied only after that
//! closure reopens.

use crate::application::{
    LocalCompilerConfig, LocalCompilerOpenError, LocalCompilerPath, LocalCompilerScratch,
    LocalPackageRootSet, PackageAuthorityConfiguration, PackageAuthorityRequest,
    enter_package_authority, package_source,
    terminal::{compile_terminal, source_authority},
};
use crate::driver::{
    CompileControl, CompileOutput, CompileRequest, CompileScratch, CompiledFragment,
    CompiledSemantic, DeclarationScope, ToolchainSelection,
    compile_semantic as compile_fused_semantic,
};
use crate::publication::{
    OpenSemanticPublicationScratch, PublishControl, SemanticPublicationScratch,
    open_published_semantic, open_semantic_generation, publish_semantic,
    semantic_generation_requirements,
};
use backend_library::interface::{
    CompilerCapability, CompilerReadiness, CompilerRequest as ApplicationCompilerRequest,
    CompilerTerminal, GeneratedArtifact, PackageCompilePhase, PackageCompileRequest,
    PackageSourceCause, SemanticImageAccessError, SemanticImageAuthority, SemanticImageSnapshot,
    SourceAuthority,
};
use backend_semantic::registry::FullRegistry;
use backend_store::journal::{
    DurablePublisher, PublicationLimits, PublicationPaths, ShutdownError,
};
use backend_version::{ArtifactId, IrFragmentDomain, IrFragmentEncoding};

use super::{
    ActivatedSemanticPackage, LocalCompiler, MAX_PACKAGE_FRAGMENT_BYTES,
    MAX_PACKAGE_SEMANTIC_BYTES, PackageSemanticError, PackageSourceSet, PublishedSemanticPackage,
    StagedPackageArtifact, ToolchainRouteError, checked_package_bytes, copy_bytes,
    declaration_scope_cause, generated, package_authority_terminal, request_source,
    select_toolchain, source_terminal, toolchain_terminal,
};

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
        Self::create_with_package_authority(
            config,
            package_roots,
            PackageAuthorityConfiguration::UNAVAILABLE,
            limits,
            scratch,
        )
    }

    /// Creates a durable compiler with explicit package roots and sidecar authority producers.
    ///
    /// The configuration is borrowed for the compiler owner's complete lifetime. Package work
    /// therefore cannot outlive a checker, oracle, JDK, or rust-analyzer configuration it uses.
    /// C and C++ continue to use the driver's direct libclang authority.
    ///
    /// # Errors
    ///
    /// Returns the same exact path or publisher terminal as [`Self::create`].
    pub fn create_with_package_authority(
        config: LocalCompilerConfig<'path, 'cancel>,
        package_roots: LocalPackageRootSet<'path>,
        package_authority: PackageAuthorityConfiguration<'path>,
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
        let publisher = DurablePublisher::open_or_create(&journal, limits)
            .map_err(LocalCompilerOpenError::Publisher)?;
        Ok(Self {
            config,
            package_roots,
            package_authority,
            publisher,
            scratch,
            retained_semantic_image: None,
        })
    }

    /// Copies the one currently retained, already-reopened semantic image into an owned snapshot.
    ///
    /// # Errors
    ///
    /// Returns a typed supersession, authority mismatch, or exact allocation failure. A failed or
    /// newer compile never exposes stale bytes under the requested identity.
    pub fn semantic_image_snapshot(
        &self,
        requested: SemanticImageAuthority,
    ) -> Result<SemanticImageSnapshot, SemanticImageAccessError> {
        if self.retained_semantic_image != Some(requested) {
            return Err(SemanticImageAccessError::Superseded {
                requested,
                retained: self.retained_semantic_image,
            });
        }
        SemanticImageSnapshot::try_from_reopened(requested, &self.scratch.semantic_image_output)
    }

    /// Stops durable publication admission and joins its earned single owner.
    ///
    /// # Errors
    ///
    /// Returns [`ShutdownError`] when the durable publisher cannot finish its owned shutdown.
    pub fn shutdown(self) -> Result<(), ShutdownError> {
        self.publisher.shutdown()
    }

    /// Compiles every member of one admitted package frontier and publishes
    /// the resulting compact fragments and semantic images as one generation.
    ///
    /// Each language authority enters with the complete package root while
    /// retaining the member's exact path. Publication becomes visible only
    /// after all members lower successfully, and returned images are copied
    /// only from the owner-reopened immutable closure.
    ///
    /// # Errors
    /// Returns the first exact authority, compilation, capacity, publication,
    /// reopen, or image-admission failure. No partial package generation is
    /// returned.
    pub fn compile_package_sources<Progress>(
        &mut self,
        package: PackageSourceSet<'_>,
        progress: &mut Progress,
    ) -> Result<PublishedSemanticPackage, PackageSemanticError>
    where
        Progress: FnMut(PackageCompilePhase),
    {
        let request = package.request.as_ref();
        let target = package.request.target;
        let first_source = package.sources[0];
        let first_authority = request_source(ApplicationCompilerRequest {
            profile: target.profile,
            stage: target.stage,
            source: first_source.source,
        })
        .map_err(source_terminal)
        .map_err(|terminal| PackageSemanticError::Compile {
            path: first_source.relative_path.into(),
            terminal: Box::new(terminal),
        })?;
        let deadline =
            self.config
                .control
                .deadline()
                .map_err(|timeout| PackageSemanticError::Compile {
                    path: first_source.relative_path.into(),
                    terminal: Box::new(CompilerTerminal::DeadlineConstruction {
                        source: first_authority,
                        language: target.profile.language(),
                        stage: target.stage,
                        timeout: *timeout,
                    }),
                })?;
        let control = CompileControl {
            deadline,
            cancelled: self.config.control.cancelled,
        };
        let mut staged = Vec::new();
        staged
            .try_reserve_exact(package.sources.len())
            .map_err(PackageSemanticError::Allocation)?;
        let mut fragments = Vec::new();
        fragments
            .try_reserve_exact(package.sources.len())
            .map_err(PackageSemanticError::Allocation)?;
        let mut fragment_bytes = 0_usize;
        let mut semantic_bytes = 0_usize;

        for source in package.sources {
            progress(PackageCompilePhase::Authority);
            let application_request = ApplicationCompilerRequest {
                profile: target.profile,
                stage: target.stage,
                source: source.source,
            };
            let source_authority = request_source(application_request)
                .map_err(source_terminal)
                .map_err(|terminal| PackageSemanticError::Compile {
                    path: source.relative_path.into(),
                    terminal: Box::new(terminal),
                })?;
            let toolchain = self
                .toolchain(application_request)
                .map_err(|cause| toolchain_terminal(source_authority, application_request, cause))
                .map_err(|terminal| PackageSemanticError::Compile {
                    path: source.relative_path.into(),
                    terminal: Box::new(terminal),
                })?;
            let scope =
                DeclarationScope::for_package(request, source.relative_path).map_err(|_| {
                    PackageSemanticError::Scope {
                        path: source.relative_path.into(),
                    }
                })?;
            let source_path = package.package_root.join(source.relative_path);
            let authority = enter_package_authority(PackageAuthorityRequest {
                package_root: package.package_root,
                source_path: &source_path,
                source: source.source.as_bytes(),
                profile: target.profile,
                toolchain,
                control,
                configuration: self.package_authority,
            })
            .map_err(|cause| {
                package_authority_terminal(
                    request.identity,
                    application_request,
                    source_authority,
                    toolchain,
                    cause,
                )
            })
            .map_err(|terminal| PackageSemanticError::Compile {
                path: source.relative_path.into(),
                terminal: Box::new(terminal),
            })?;
            progress(PackageCompilePhase::Lower);
            let compiled = compile_fused_semantic(
                CompileRequest {
                    profile: target.profile,
                    stage: target.stage,
                    source: source.source.as_bytes(),
                    declaration_scope: scope,
                    toolchain,
                    authority: authority.input(),
                    control,
                },
                CompileScratch {
                    diagnostic_output: &mut self.scratch.diagnostic_output,
                    native_work: self.config.native_work_directory,
                },
                CompileOutput {
                    fragment_output: self.scratch.fragment_output.as_mut(),
                },
            )
            .map_err(compile_terminal)
            .map_err(|terminal| PackageSemanticError::Compile {
                path: source.relative_path.into(),
                terminal: Box::new(terminal),
            })?;
            let fragment = copy_bytes(compiled.artifact.fragment.as_ref())?;
            fragment_bytes = checked_package_bytes(
                fragment_bytes,
                fragment.len(),
                MAX_PACKAGE_FRAGMENT_BYTES,
                "compact fragment",
            )?;
            let image_bytes =
                backend_semantic::ir::full_semantic_image_len(&compiled.ir).map_err(|_| {
                    PackageSemanticError::Capacity {
                        lane: "semantic image",
                    }
                })?;
            semantic_bytes = checked_package_bytes(
                semantic_bytes,
                image_bytes,
                MAX_PACKAGE_SEMANTIC_BYTES,
                "semantic image",
            )?;
            fragments.push(fragment);
            staged.push(StagedPackageArtifact {
                source: compiled.artifact.source,
                recipe: compiled.artifact.recipe,
                ir: compiled.ir,
            });
        }

        let manifest_bytes = crate::publication::manifest::COMPILATION_MANIFEST_HEADER_BYTES
            .checked_add(
                package
                    .sources
                    .len()
                    .checked_mul(
                        crate::publication::manifest::COMPILATION_SEMANTIC_MANIFEST_ENTRY_BYTES,
                    )
                    .ok_or(PackageSemanticError::Capacity { lane: "manifest" })?,
            )
            .ok_or(PackageSemanticError::Capacity { lane: "manifest" })?;
        self.scratch
            .prepare_publication(staged.len(), manifest_bytes, fragment_bytes, semantic_bytes)
            .map_err(PackageSemanticError::Scratch)?;

        let mut compiled = Vec::new();
        compiled
            .try_reserve_exact(staged.len())
            .map_err(PackageSemanticError::Allocation)?;
        for (ordinal, (artifact, bytes)) in staged.into_iter().zip(&fragments).enumerate() {
            let fragment = backend_semantic::ir::FragmentView::validate(bytes)
                .map_err(|source| PackageSemanticError::Fragment { ordinal, source })?;
            compiled.push(CompiledSemantic {
                artifact: CompiledFragment {
                    source: artifact.source,
                    recipe: artifact.recipe,
                    fragment,
                },
                ir: artifact.ir,
            });
        }

        progress(PackageCompilePhase::Publish);
        let publication = publish_semantic(
            &self.publisher,
            self.config.artifact_directory,
            &compiled,
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
        .map_err(PackageSemanticError::Publish)?;
        drop(compiled);

        progress(PackageCompilePhase::Reopen);
        let opened = open_published_semantic(
            &self.publisher,
            self.config.artifact_directory,
            OpenSemanticPublicationScratch {
                manifest_output: &mut self.scratch.manifest_output,
                manifest_facts: &mut self.scratch.manifest_facts,
                fragment_output: &mut self.scratch.reopened_fragment_output,
                semantic_image_output: &mut self.scratch.semantic_image_output,
                locality_output: &mut self.scratch.locality_output,
            },
        )
        .map_err(PackageSemanticError::Reopen)?
        .ok_or(PackageSemanticError::MissingPublication)?;
        let mut images = Vec::new();
        images
            .try_reserve_exact(package.sources.len())
            .map_err(PackageSemanticError::Allocation)?;
        for (ordinal, artifact) in opened.artifacts().enumerate() {
            let artifact =
                artifact.map_err(|source| PackageSemanticError::Artifact { ordinal, source })?;
            let facts = artifact.fragment.facts.semantic_image.ok_or(
                PackageSemanticError::ReopenedCardinality {
                    expected: package.sources.len(),
                    observed: ordinal,
                },
            )?;
            let authority = SemanticImageAuthority {
                identity: facts.identity,
                byte_len: facts.byte_length,
            };
            images.push(
                SemanticImageSnapshot::try_from_reopened(
                    authority,
                    artifact.semantic_image.as_ref(),
                )
                .map_err(|cause| PackageSemanticError::Snapshot { ordinal, cause })?,
            );
        }
        if images.len() != package.sources.len() {
            return Err(PackageSemanticError::ReopenedCardinality {
                expected: package.sources.len(),
                observed: images.len(),
            });
        }
        Ok(PublishedSemanticPackage {
            publication,
            images: images.into_boxed_slice(),
        })
    }

    /// Reopens one exact immutable semantic claim independently of the local journal head.
    ///
    /// # Errors
    ///
    /// Returns exact manifest, binding, artifact, closure, allocation, or image-validation facts.
    pub fn activate_semantic_generation(
        &mut self,
        manifest: crate::publication::manifest::CompilationManifestFacts,
        binding: crate::publication::binding::CompilationBindingFacts,
    ) -> Result<ActivatedSemanticPackage, PackageSemanticError> {
        let artifact_count = usize::try_from(manifest.fragment_count)
            .map_err(|_| PackageSemanticError::Capacity { lane: "manifest" })?;
        let manifest_bytes = usize::try_from(manifest.byte_length)
            .map_err(|_| PackageSemanticError::Capacity { lane: "manifest" })?;
        self.scratch
            .prepare_publication(artifact_count, manifest_bytes, 0, 0)
            .map_err(PackageSemanticError::Scratch)?;
        let requirements = semantic_generation_requirements(
            manifest,
            binding,
            self.config.artifact_directory,
            &mut self.scratch.manifest_output,
            &mut self.scratch.manifest_facts,
        )
        .map_err(PackageSemanticError::Reopen)?;
        self.scratch
            .prepare_publication(
                artifact_count,
                manifest_bytes,
                requirements.fragment_bytes,
                requirements.semantic_image_bytes,
            )
            .map_err(PackageSemanticError::Scratch)?;
        let opened = open_semantic_generation(
            manifest,
            binding,
            self.config.artifact_directory,
            OpenSemanticPublicationScratch {
                manifest_output: &mut self.scratch.manifest_output,
                manifest_facts: &mut self.scratch.manifest_facts,
                fragment_output: &mut self.scratch.reopened_fragment_output,
                semantic_image_output: &mut self.scratch.semantic_image_output,
                locality_output: &mut self.scratch.locality_output,
            },
        )
        .map_err(PackageSemanticError::Reopen)?;
        let mut images = Vec::new();
        images
            .try_reserve_exact(artifact_count)
            .map_err(PackageSemanticError::Allocation)?;
        for (ordinal, artifact) in opened.artifacts().enumerate() {
            let artifact =
                artifact.map_err(|source| PackageSemanticError::Artifact { ordinal, source })?;
            let facts = artifact.fragment.facts.semantic_image.ok_or(
                PackageSemanticError::ReopenedCardinality {
                    expected: artifact_count,
                    observed: ordinal,
                },
            )?;
            images.push(
                SemanticImageSnapshot::try_from_reopened(
                    SemanticImageAuthority {
                        identity: facts.identity,
                        byte_len: facts.byte_length,
                    },
                    artifact.semantic_image.as_ref(),
                )
                .map_err(|cause| PackageSemanticError::Snapshot { ordinal, cause })?,
            );
        }
        if images.len() != artifact_count {
            return Err(PackageSemanticError::ReopenedCardinality {
                expected: artifact_count,
                observed: images.len(),
            });
        }
        Ok(ActivatedSemanticPackage {
            manifest,
            binding,
            images: images.into_boxed_slice(),
        })
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
            crate::driver::SemanticAuthorityInput::None,
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
        authority: crate::driver::SemanticAuthorityInput<'_>,
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
        self.compile_prepared_and_publish(
            request,
            source,
            declaration_scope,
            toolchain,
            authority,
            CompileControl {
                deadline,
                cancelled: self.config.control.cancelled,
            },
            progress,
        )
    }

    #[allow(
        clippy::too_many_arguments,
        clippy::result_large_err,
        reason = "the prepared boundary keeps one source, toolchain, permit, scope, and authority transaction coherent through publication"
    )]
    fn compile_prepared_and_publish<Progress>(
        &mut self,
        request: ApplicationCompilerRequest<'_>,
        _source: SourceAuthority,
        declaration_scope: DeclarationScope<'_>,
        toolchain: ToolchainSelection<'_>,
        authority: crate::driver::SemanticAuthorityInput<'_>,
        control: CompileControl<'_>,
        progress: &mut Progress,
    ) -> Result<GeneratedArtifact, CompilerTerminal>
    where
        Progress: FnMut(PackageCompilePhase),
    {
        self.retained_semantic_image = None;
        progress(PackageCompilePhase::Lower);
        let scratch = &mut *self.scratch;
        let compiled = compile_fused_semantic(
            CompileRequest {
                profile: request.profile,
                stage: request.stage,
                source: request.source.as_bytes(),
                declaration_scope,
                toolchain,
                authority,
                control,
            },
            CompileScratch {
                diagnostic_output: &mut scratch.diagnostic_output,
                native_work: self.config.native_work_directory,
            },
            CompileOutput {
                fragment_output: scratch.fragment_output.as_mut(),
            },
        )
        .map_err(compile_terminal)?;
        let source = source_authority(compiled.artifact.source);
        let recipe = compiled.artifact.recipe;
        let fragment = ArtifactId::<IrFragmentEncoding, IrFragmentDomain>::from_encoded_bytes(
            compiled.artifact.fragment.as_ref(),
        );
        let semantic_length =
            backend_semantic::ir::full_semantic_image_len(&compiled.ir).map_err(|cause| {
                crate::application::terminal::semantic_publication_terminal(
                    source,
                    recipe,
                    crate::publication::PublishSemanticError::ImageMeasure {
                        ordinal: 0,
                        source: cause,
                    },
                )
            })?;
        scratch.semantic_image_output.resize(semantic_length, 0);
        progress(PackageCompilePhase::Publish);
        let publication = publish_semantic(
            &self.publisher,
            self.config.artifact_directory,
            core::slice::from_ref(&compiled),
            PublishControl::Observe(self.config.control.cancelled),
            SemanticPublicationScratch {
                manifest_output: &mut scratch.manifest_output,
                manifest_facts: &mut scratch.manifest_facts,
                ordinals: &mut scratch.ordinals,
                semantic_image_plan: &mut scratch.semantic_image_plan,
                semantic_image_output: &mut scratch.semantic_image_output,
                locality_output: &mut scratch.locality_output,
                binding_output: &mut scratch.binding_output,
            },
        )
        .map_err(|error| {
            crate::application::terminal::semantic_publication_terminal(source, recipe, error)
        })?;
        drop(compiled);

        progress(PackageCompilePhase::Reopen);
        let opened = open_published_semantic(
            &self.publisher,
            self.config.artifact_directory,
            OpenSemanticPublicationScratch {
                manifest_output: &mut scratch.manifest_output,
                manifest_facts: &mut scratch.manifest_facts,
                fragment_output: scratch.fragment_output.as_mut(),
                semantic_image_output: &mut scratch.semantic_image_output,
                locality_output: &mut scratch.locality_output,
            },
        )
        .map_err(|error| {
            crate::application::terminal::semantic_reopen_terminal(source, recipe, error)
        })?
        .ok_or_else(|| crate::application::terminal::semantic_reopen_absent(source, recipe))?;
        let mut artifacts = opened.artifacts();
        let semantic = artifacts
            .next()
            .ok_or_else(|| crate::application::terminal::semantic_reopen_absent(source, recipe))?
            .map_err(|error| {
                crate::application::terminal::semantic_artifact_terminal(source, recipe, error)
            })?;
        let semantic_facts =
            semantic.fragment.facts.semantic_image.ok_or_else(|| {
                crate::application::terminal::semantic_reopen_absent(source, recipe)
            })?;
        if artifacts.next().is_some() {
            return Err(crate::application::terminal::semantic_reopen_cardinality(
                source, recipe,
            ));
        }
        let generated = generated(source, recipe, fragment, semantic_facts, &publication);
        self.retained_semantic_image = Some(generated.semantic_image);
        Ok(generated)
    }

    fn toolchain(
        &self,
        request: ApplicationCompilerRequest<'_>,
    ) -> Result<ToolchainSelection<'path>, ToolchainRouteError> {
        let route = FullRegistry
            .route(request.profile.language(), request.stage)
            .map_err(ToolchainRouteError::UnsupportedStage)?;
        select_toolchain(self.config.toolchains, route)
    }
}

impl CompilerCapability for LocalCompiler<'_, '_, '_> {
    fn readiness(&self) -> CompilerReadiness {
        CompilerReadiness::Ready
    }

    fn semantic_image_snapshot(
        &mut self,
        requested: SemanticImageAuthority,
    ) -> Result<SemanticImageSnapshot, SemanticImageAccessError> {
        Self::semantic_image_snapshot(self, requested)
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
        let scope =
            DeclarationScope::for_package(package, &resolved.relative_source).map_err(|cause| {
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
        let source_authority = request_source(application_request).map_err(source_terminal)?;
        let toolchain = self
            .toolchain(application_request)
            .map_err(|cause| toolchain_terminal(source_authority, application_request, cause))?;
        let deadline = self.config.control.deadline().map_err(|timeout| {
            CompilerTerminal::DeadlineConstruction {
                source: source_authority,
                language: application_request.profile.language(),
                stage: application_request.stage,
                timeout: *timeout,
            }
        })?;
        let control = CompileControl {
            deadline,
            cancelled: self.config.control.cancelled,
        };
        let authority = enter_package_authority(PackageAuthorityRequest {
            package_root: &resolved.package_root,
            source_path: &resolved.source_path,
            source: &resolved.bytes,
            profile: application_request.profile,
            toolchain,
            control,
            configuration: self.package_authority,
        })
        .map_err(|cause| {
            package_authority_terminal(
                target,
                application_request,
                source_authority,
                toolchain,
                cause,
            )
        })?;
        self.compile_prepared_and_publish(
            application_request,
            source_authority,
            scope,
            toolchain,
            authority.input(),
            control,
            progress,
        )
    }
}
