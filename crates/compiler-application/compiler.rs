//! Defines compiler behavior for `compiler-application`, whose purpose is to bind application requests to native compilation and durable publication.
//! This module owns the compiler invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! One single-request local compiler specialization over explicit local ownership.

use compiler_driver::{
    CompileControl, CompileOutput, CompileRequest, CompileScratch, CompiledFragment, CompiledIr,
    CompiledSemantic, DeclarationScope, PackageDeclarationScopeFault, ToolchainSelection,
    compile_ir, compile_semantic as compile_fused_semantic,
};
use compiler_publication::{
    OpenSemanticPublicationScratch, PublishControl, PublishedCompilation,
    SemanticImageArtifactFacts, SemanticPublicationScratch, open_published_semantic,
    open_semantic_generation, publish_semantic, semantic_generation_requirements,
};
use compiler_registry::{AdapterRoute, FullRegistry};
use backend_version::{
    ArtifactId, ContentId, IrFragmentDomain, IrFragmentEncoding, SourceFactDomain,
};
use interface_core::{
    CompilerCapability, CompilerReadiness, CompilerRequest as ApplicationCompilerRequest,
    CompilerTerminal, GeneratedArtifact, PackageCompilePhase, PackageCompileRequest,
    PackageDeclarationScopeCause, PackageSourceCause, PublicationAuthority,
    SemanticImageAccessError, SemanticImageAuthority, SemanticImageSnapshot, SourceAuthority,
};
use server_journal::{DurablePublisher, PublicationLimits, PublicationPaths, ShutdownError};
use thiserror::Error;

use crate::{
    LocalCompilerConfig, LocalCompilerOpenError, LocalCompilerPath, LocalCompilerScratch,
    LocalPackageRootSet, PackageAuthorityConfiguration, PackageAuthorityError,
    PackageAuthorityRequest, enter_package_authority, package_source,
    terminal::{compile_terminal, source_authority},
};

const MAX_PACKAGE_FRAGMENT_BYTES: usize = 64 * 1024 * 1024;
const MAX_PACKAGE_SEMANTIC_BYTES: usize = 512 * 1024 * 1024;

/// One already-admitted source member of a package compilation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PackageSource<'source> {
    relative_path: &'source str,
    source: &'source str,
}

impl<'source> PackageSource<'source> {
    /// Admits one normalized package-relative source path with its exact bytes.
    ///
    /// # Errors
    /// Returns an error when the path is empty, absolute, contains traversal,
    /// or uses a platform-dependent separator.
    pub fn new(
        relative_path: &'source str,
        source: &'source str,
    ) -> Result<Self, PackageSourceSetError> {
        let path = std::path::Path::new(relative_path);
        let normalized = !relative_path.is_empty()
            && !relative_path.contains('\\')
            && !path.is_absolute()
            && path
                .components()
                .all(|component| matches!(component, std::path::Component::Normal(_)));
        if !normalized {
            return Err(PackageSourceSetError::InvalidPath);
        }
        Ok(Self {
            relative_path,
            source,
        })
    }

    /// Returns the normalized package-relative source path.
    #[must_use]
    pub const fn relative_path(self) -> &'source str {
        self.relative_path
    }

    /// Returns the exact admitted UTF-8 source.
    #[must_use]
    pub const fn source(self) -> &'source str {
        self.source
    }
}

/// Checked package-wide semantic compilation input.
#[derive(Clone, Copy, Debug)]
pub struct PackageSourceSet<'source> {
    request: &'source PackageCompileRequest,
    package_root: &'source std::path::Path,
    sources: &'source [PackageSource<'source>],
}

impl<'source> PackageSourceSet<'source> {
    /// Admits a complete, strictly ordered source frontier for one package.
    ///
    /// # Errors
    /// Returns an error for a relative package root or an empty, oversized,
    /// duplicate, or unordered source frontier.
    pub fn new(
        request: &'source PackageCompileRequest,
        package_root: &'source std::path::Path,
        sources: &'source [PackageSource<'source>],
    ) -> Result<Self, PackageSourceSetError> {
        if !package_root.is_absolute() {
            return Err(PackageSourceSetError::RelativeRoot);
        }
        if sources.is_empty() || sources.len() > crate::MAX_MANIFEST_ENTRIES {
            return Err(PackageSourceSetError::Cardinality {
                observed: sources.len(),
                maximum: crate::MAX_MANIFEST_ENTRIES,
            });
        }
        if sources
            .windows(2)
            .any(|pair| pair[0].relative_path >= pair[1].relative_path)
        {
            return Err(PackageSourceSetError::Order);
        }
        Ok(Self {
            request,
            package_root,
            sources,
        })
    }
}

/// Package-source-frontier admission failure.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum PackageSourceSetError {
    /// Package roots must have host-independent absolute identity.
    #[error("package source root is relative")]
    RelativeRoot,
    /// A source path was not a normalized relative path.
    #[error("package source path is not normalized and relative")]
    InvalidPath,
    /// The source frontier was empty or exceeded the package manifest bound.
    #[error("package source frontier has {observed} members; maximum is {maximum}")]
    Cardinality {
        /// Observed source count.
        observed: usize,
        /// Maximum admitted source count.
        maximum: usize,
    },
    /// Source paths were duplicated or unordered.
    #[error("package source frontier is not strictly ordered")]
    Order,
}

/// One atomically published package generation and its reopened semantic images.
#[derive(Debug)]
pub struct PublishedSemanticPackage {
    /// Locality-independent generation, manifest, and binding identities.
    pub publication: PublishedCompilation,
    /// Complete semantic images copied only after the publication owner reopened the closure.
    pub images: Box<[SemanticImageSnapshot]>,
}

/// One immutable semantic generation reopened by exact claim rather than local journal head.
#[derive(Debug)]
pub struct ActivatedSemanticPackage {
    /// Manifest facts proven against the immutable manifest bytes.
    pub manifest: compiler_publication::manifest::CompilationManifestFacts,
    /// Generation binding proven against its deterministic immutable address.
    pub binding: compiler_publication::binding::CompilationBindingFacts,
    /// Complete semantic images copied only after closure verification.
    pub images: Box<[SemanticImageSnapshot]>,
}

/// Typed package-wide compilation or publication terminal.
#[derive(Debug, Error)]
pub enum PackageSemanticError {
    /// Canonical package lineage could not be constructed.
    #[error("package lineage is malformed")]
    Lineage,
    /// One package-relative declaration scope was rejected.
    #[error("package declaration scope is malformed for {path}")]
    Scope {
        /// Rejected normalized source path.
        path: Box<str>,
    },
    /// One authority, toolchain, lowering, or cancellation terminal occurred.
    #[error("package semantic compilation failed for {path}: {terminal:?}")]
    Compile {
        /// Source member that reached the terminal.
        path: Box<str>,
        /// Exact closed compiler terminal.
        terminal: Box<CompilerTerminal>,
    },
    /// A checked size computation overflowed or exceeded the package budget.
    #[error("package semantic publication exceeds its {lane} byte budget")]
    Capacity {
        /// Bounded output lane.
        lane: &'static str,
    },
    /// Reusable publication scratch could not be prepared.
    #[error("package semantic publication scratch failed")]
    Scratch(#[source] crate::LocalCompilerScratchError),
    /// A retained compact fragment failed its second grammar admission.
    #[error("retained compact fragment {ordinal} failed admission")]
    Fragment {
        /// Canonical package artifact ordinal.
        ordinal: usize,
        /// Exact fragment grammar failure.
        #[source]
        source: compiler_ir::FragmentError,
    },
    /// Immutable package publication failed.
    #[error("package semantic publication failed")]
    Publish(#[source] compiler_publication::PublishSemanticError),
    /// The publication owner could not reopen its selected closure.
    #[error("package semantic publication could not be reopened")]
    Reopen(#[source] compiler_publication::OpenPublishedError),
    /// The durable owner did not select the just-published generation.
    #[error("package semantic publication disappeared before reopen")]
    MissingPublication,
    /// One manifest-bound semantic artifact failed admission.
    #[error("semantic artifact {ordinal} failed reopen admission")]
    Artifact {
        /// Canonical package artifact ordinal.
        ordinal: usize,
        /// Exact paired fragment/image failure.
        #[source]
        source: compiler_publication::OpenedSemanticArtifactError,
    },
    /// A reopened semantic image could not become an immutable snapshot.
    #[error("semantic artifact {ordinal} failed snapshot admission")]
    Snapshot {
        /// Canonical package artifact ordinal.
        ordinal: usize,
        /// Exact extent or identity failure.
        cause: SemanticImageAccessError,
    },
    /// The reopened manifest did not yield the published source cardinality.
    #[error("semantic publication reopened {observed} artifacts; expected {expected}")]
    ReopenedCardinality {
        /// Expected admitted source count.
        expected: usize,
        /// Reopened artifact count.
        observed: usize,
    },
    /// A bounded owned lane could not reserve its exact capacity.
    #[error("package semantic publication allocation failed")]
    Allocation(#[source] std::collections::TryReserveError),
}

/// Concrete local compiler with bounded explicit native toolchains, publisher, paths, and scratch.
pub struct LocalCompiler<'path, 'scratch, 'cancel> {
    config: LocalCompilerConfig<'path, 'cancel>,
    package_roots: LocalPackageRootSet<'path>,
    package_authority: PackageAuthorityConfiguration<'path>,
    publisher: DurablePublisher,
    scratch: &'scratch mut LocalCompilerScratch,
    retained_semantic_image: Option<SemanticImageAuthority>,
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
            let image_bytes = compiler_ir::full_semantic_image_len(&compiled.ir).map_err(|_| {
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

        let manifest_bytes = compiler_publication::manifest::COMPILATION_MANIFEST_HEADER_BYTES
            .checked_add(
                package
                    .sources
                    .len()
                    .checked_mul(
                        compiler_publication::manifest::COMPILATION_SEMANTIC_MANIFEST_ENTRY_BYTES,
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
            let fragment = compiler_ir::FragmentView::validate(bytes)
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
        manifest: compiler_publication::manifest::CompilationManifestFacts,
        binding: compiler_publication::binding::CompilationBindingFacts,
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
        authority: compiler_driver::SemanticAuthorityInput<'_>,
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
        .map_err(|error| crate::terminal::semantic_publication_terminal(source, recipe, error))?;
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

struct StagedPackageArtifact {
    source: compiler_ir::SourceIdentity,
    recipe: compiler_vocabulary::CompileRecipeFact,
    ir: compiler_ir::Ir,
}

fn copy_bytes(bytes: &[u8]) -> Result<Box<[u8]>, PackageSemanticError> {
    let mut owned = Vec::new();
    owned
        .try_reserve_exact(bytes.len())
        .map_err(PackageSemanticError::Allocation)?;
    owned.extend_from_slice(bytes);
    Ok(owned.into_boxed_slice())
}

fn checked_package_bytes(
    accumulated: usize,
    additional: usize,
    maximum: usize,
    lane: &'static str,
) -> Result<usize, PackageSemanticError> {
    let total = accumulated
        .checked_add(additional)
        .ok_or(PackageSemanticError::Capacity { lane })?;
    if total > maximum {
        return Err(PackageSemanticError::Capacity { lane });
    }
    Ok(total)
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
    cause: PackageDeclarationScopeFault,
) -> PackageDeclarationScopeCause {
    match cause {
        PackageDeclarationScopeFault::Lineage(cause) => lineage_cause(cause),
        PackageDeclarationScopeFault::Declaration(compiler_ir::DeclarationKeyFault::Path(
            compiler_ir::DeclarationPathFault::Empty,
        )) => PackageDeclarationScopeCause::EmptySourcePath,
        PackageDeclarationScopeFault::Declaration(compiler_ir::DeclarationKeyFault::Path(
            compiler_ir::DeclarationPathFault::Backslash,
        )) => PackageDeclarationScopeCause::SourceBackslash,
        PackageDeclarationScopeFault::Declaration(compiler_ir::DeclarationKeyFault::EmptyName) => {
            PackageDeclarationScopeCause::EmptyPackage
        }
    }
}

fn generated(
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
            receipt: interface_core::DurableReceiptAuthority {
                sequence: *publication.publication.stable.sequence,
                durable_end: *publication.publication.stable.durable_end,
                immutable_checksum: publication.publication.immutable.checksum,
                head_checksum: publication.publication.head.checksum,
            },
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

#[allow(
    clippy::result_large_err,
    reason = "the public compiler terminal retains one bounded cold authority diagnostic"
)]
fn package_authority_terminal(
    target: ContentId<backend_version::CompilationTargetDomain>,
    request: ApplicationCompilerRequest<'_>,
    source: SourceAuthority,
    toolchain: ToolchainSelection<'_>,
    cause: PackageAuthorityError,
) -> CompilerTerminal {
    match cause {
        PackageAuthorityError::Cancelled { .. } => CompilerTerminal::PackageCancelled {
            target,
            phase: PackageCompilePhase::Authority,
        },
        PackageAuthorityError::AdapterUnavailable { .. } => CompilerTerminal::Unavailable {
            language: request.profile.language(),
            stage: request.stage,
        },
        PackageAuthorityError::ToolchainUnavailable { tool, .. } => CompilerTerminal::Toolchain {
            source,
            language: request.profile.language(),
            stage: request.stage,
            selected: tool,
            configured: None,
        },
        PackageAuthorityError::Deadline { .. } => compiler_attempt_terminal(
            request,
            source,
            toolchain,
            interface_core::CompilerCause::DeadlineExceeded { diagnostic: None },
        ),
        cause => {
            let (phase, class) = package_authority_projection(&cause);
            compiler_attempt_terminal(
                request,
                source,
                toolchain,
                interface_core::CompilerCause::Authority {
                    phase,
                    class,
                    diagnostic: None,
                },
            )
        }
    }
}

fn compiler_attempt_terminal(
    request: ApplicationCompilerRequest<'_>,
    source: SourceAuthority,
    toolchain: ToolchainSelection<'_>,
    cause: interface_core::CompilerCause,
) -> CompilerTerminal {
    let ToolchainSelection::ResolvedNative(resolved) = toolchain else {
        return CompilerTerminal::Toolchain {
            source,
            language: request.profile.language(),
            stage: request.stage,
            selected: match toolchain {
                ToolchainSelection::ExplicitlyUnavailable { tool } => tool,
                ToolchainSelection::ResolvedNative(_) => unreachable!(),
            },
            configured: None,
        };
    };
    let recipe = compiler_vocabulary::CompileRecipeFact::derive(
        request.profile,
        request.stage,
        resolved.tool,
        source.identity,
        resolved.identity,
    );
    CompilerTerminal::Compile {
        attempted: interface_core::CompilerAttempt {
            source,
            recipe: recipe.identity,
        },
        cause,
    }
}

const fn package_authority_projection(
    cause: &PackageAuthorityError,
) -> (
    compiler_vocabulary::AuthorityPhase,
    compiler_vocabulary::AuthorityDiagnosticClass,
) {
    use compiler_vocabulary::{AuthorityDiagnosticClass as Class, AuthorityPhase as Phase};

    match cause {
        PackageAuthorityError::SourceOutsidePackage { .. }
        | PackageAuthorityError::TypeScriptEntryPath { .. }
        | PackageAuthorityError::RustToolchainExecutableMismatch { .. } => {
            (Phase::Open, Class::Binding)
        }
        PackageAuthorityError::PythonSyntax(_) => (Phase::Parse, Class::Syntax),
        PackageAuthorityError::PythonPyrefly(_) => (Phase::TypeCheck, Class::Type),
        PackageAuthorityError::RustProject(_) | PackageAuthorityError::GoOracle(_) => {
            (Phase::Resolve, Class::Authority)
        }
        PackageAuthorityError::CSharp(_) => (Phase::TypeCheck, Class::Authority),
        PackageAuthorityError::TypeScript(_) => (Phase::TypeCheck, Class::Authority),
        PackageAuthorityError::JavaHarness(_)
        | PackageAuthorityError::ImageTooLarge { .. }
        | PackageAuthorityError::ToolchainUnavailable { .. }
        | PackageAuthorityError::Cancelled { .. }
        | PackageAuthorityError::Deadline { .. }
        | PackageAuthorityError::AdapterUnavailable { .. } => (Phase::Open, Class::Authority),
    }
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
    UnsupportedStage(compiler_vocabulary::FrontendError),
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
        ToolchainRouteError::UnsupportedStage(cause) => {
            CompilerTerminal::UnsupportedStage { source, cause }
        }
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
