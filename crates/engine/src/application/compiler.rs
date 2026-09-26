//! Defines compiler behavior for the `backend-engine` application, whose purpose is to bind application requests to native compilation and durable publication.
//! This module owns the compiler invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! One single-request local compiler specialization over explicit local ownership.

use crate::driver::{PackageDeclarationScopeFault, ToolchainSelection};
use crate::publication::{PublishedCompilation, SemanticImageArtifactFacts};
use backend_library::interface::{
    CompilerRequest as ApplicationCompilerRequest, CompilerTerminal, GeneratedArtifact,
    PackageCompilePhase, PackageCompileRequest, PackageDeclarationScopeCause, PublicationAuthority,
    SemanticImageAccessError, SemanticImageAuthority, SemanticImageSnapshot, SourceAuthority,
};
use backend_semantic::registry::AdapterRoute;
use backend_store::journal::DurablePublisher;
use backend_version::{
    ArtifactId, ContentId, IrFragmentDomain, IrFragmentEncoding, SourceFactDomain,
};
use thiserror::Error;

mod local;

use crate::application::{
    LocalCompilerConfig, LocalCompilerScratch, LocalPackageRootSet, PackageAuthorityConfiguration,
    PackageAuthorityError,
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
        if sources.is_empty() || sources.len() > crate::application::MAX_MANIFEST_ENTRIES {
            return Err(PackageSourceSetError::Cardinality {
                observed: sources.len(),
                maximum: crate::application::MAX_MANIFEST_ENTRIES,
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
    pub manifest: crate::publication::manifest::CompilationManifestFacts,
    /// Generation binding proven against its deterministic immutable address.
    pub binding: crate::publication::binding::CompilationBindingFacts,
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
    Scratch(#[source] crate::application::LocalCompilerScratchError),
    /// A retained compact fragment failed its second grammar admission.
    #[error("retained compact fragment {ordinal} failed admission")]
    Fragment {
        /// Canonical package artifact ordinal.
        ordinal: usize,
        /// Exact fragment grammar failure.
        #[source]
        source: backend_semantic::ir::FragmentError,
    },
    /// Immutable package publication failed.
    #[error("package semantic publication failed")]
    Publish(#[source] crate::publication::PublishSemanticError),
    /// The publication owner could not reopen its selected closure.
    #[error("package semantic publication could not be reopened")]
    Reopen(#[source] crate::publication::OpenPublishedError),
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
        source: crate::publication::OpenedSemanticArtifactError,
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

fn select_toolchain(
    toolchains: crate::application::LocalToolchainSet<'_>,
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

struct StagedPackageArtifact {
    source: backend_semantic::ir::SourceIdentity,
    recipe: backend_semantic::vocabulary::CompileRecipeFact,
    ir: backend_semantic::ir::Ir,
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

const fn lineage_cause(
    cause: backend_semantic::ir::PackageLineageFault,
) -> PackageDeclarationScopeCause {
    match cause {
        backend_semantic::ir::PackageLineageFault::EmptyEcosystem => {
            PackageDeclarationScopeCause::EmptyEcosystem
        }
        backend_semantic::ir::PackageLineageFault::EmptyName => {
            PackageDeclarationScopeCause::EmptyPackage
        }
        backend_semantic::ir::PackageLineageFault::SeparatorInEcosystem => {
            PackageDeclarationScopeCause::EcosystemSeparator
        }
        backend_semantic::ir::PackageLineageFault::SeparatorInName => {
            PackageDeclarationScopeCause::PackageSeparator
        }
        backend_semantic::ir::PackageLineageFault::Backslash { segment } => {
            PackageDeclarationScopeCause::LineageBackslash { segment }
        }
    }
}

const fn declaration_scope_cause(
    cause: PackageDeclarationScopeFault,
) -> PackageDeclarationScopeCause {
    match cause {
        PackageDeclarationScopeFault::Lineage(cause) => lineage_cause(cause),
        PackageDeclarationScopeFault::Declaration(
            backend_semantic::ir::DeclarationKeyFault::Path(
                backend_semantic::ir::DeclarationPathFault::Empty,
            ),
        ) => PackageDeclarationScopeCause::EmptySourcePath,
        PackageDeclarationScopeFault::Declaration(
            backend_semantic::ir::DeclarationKeyFault::Path(
                backend_semantic::ir::DeclarationPathFault::Backslash,
            ),
        ) => PackageDeclarationScopeCause::SourceBackslash,
        PackageDeclarationScopeFault::Declaration(
            backend_semantic::ir::DeclarationKeyFault::EmptyName,
        ) => PackageDeclarationScopeCause::EmptyPackage,
    }
}

fn generated(
    source: SourceAuthority,
    recipe: backend_semantic::vocabulary::CompileRecipeFact,
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
            generation: backend_library::interface::GenerationAuthority {
                pinned_root: publication.publication.generation.pinned_root,
                dep_set: publication.publication.generation.dep_set,
            },
            manifest: publication.manifest.identity,
            binding: publication.binding.identity,
            receipt: backend_library::interface::DurableReceiptAuthority {
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
            backend_library::interface::CompilerCause::DeadlineExceeded { diagnostic: None },
        ),
        cause => {
            let (phase, class) = package_authority_projection(&cause);
            compiler_attempt_terminal(
                request,
                source,
                toolchain,
                backend_library::interface::CompilerCause::Authority {
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
    cause: backend_library::interface::CompilerCause,
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
    let recipe = backend_semantic::vocabulary::CompileRecipeFact::derive(
        request.profile,
        request.stage,
        resolved.tool,
        source.identity,
        resolved.identity,
    );
    CompilerTerminal::Compile {
        attempted: backend_library::interface::CompilerAttempt {
            source,
            recipe: recipe.identity,
        },
        cause,
    }
}

const fn package_authority_projection(
    cause: &PackageAuthorityError,
) -> (
    backend_semantic::vocabulary::AuthorityPhase,
    backend_semantic::vocabulary::AuthorityDiagnosticClass,
) {
    use backend_semantic::vocabulary::{
        AuthorityDiagnosticClass as Class, AuthorityPhase as Phase,
    };

    match cause {
        PackageAuthorityError::SourceOutsidePackage { .. }
        | PackageAuthorityError::TypeScriptEntryPath { .. }
        | PackageAuthorityError::RustToolchainExecutableMismatch { .. }
        | PackageAuthorityError::ClangProject(_) => (Phase::Open, Class::Binding),
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
    UnsupportedStage(backend_semantic::vocabulary::FrontendError),
    Missing {
        selected: backend_semantic::vocabulary::NativeTool,
    },
    ToolingUnavailable {
        tool: backend_semantic::vocabulary::NativeTool,
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

    use crate::driver::{ResolvedToolchain, ToolchainResolutionError, ToolchainSelection};
    use backend_semantic::registry::AdapterRoute;
    use backend_semantic::vocabulary::NativeTool;
    use thiserror::Error;

    use crate::application::{LocalToolchainSet, LocalToolchainSetError};

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
