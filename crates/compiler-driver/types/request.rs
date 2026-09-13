//! Defines types request behavior for `compiler-driver`, whose purpose is to run bounded native toolchains and lower their output into canonical IR.
//! This module owns the types request invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
use std::{
    num::TryFromIntError,
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
    time::Instant,
};

use compiler_ir::{
    DeclarationKey, DeclarationKeyFault, EntityKind, PackageLineage, PackageLineageFault,
};
use backend_semantic::vocabulary::{LanguageProfile, PackageUrl, Stage};
use backend_version::{ContentId, SourceFactDomain};

use super::{ResolvedToolchain, SourceIdentity, ToolchainSelection};

/// Typed package/file scope required to mint cross-generation declaration
/// identities.  Source content, row order, and byte spans are deliberately
/// absent: they are version payload/provenance, never declaration identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DeclarationScope<'source> {
    lineage: PackageLineage<'source>,
    path: &'source str,
    coordinate: Option<&'source PackageUrl>,
}

/// Exact package-scope admission failure.
///
/// Keeping lineage and declaration failures separate prevents callers from
/// losing which authority boundary rejected the package coordinate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PackageDeclarationScopeFault {
    /// The canonical coordinate could not enter declaration lineage.
    Lineage(PackageLineageFault),
    /// The package-relative path could not enter a declaration key.
    Declaration(DeclarationKeyFault),
}

impl<'source> DeclarationScope<'source> {
    /// Enters one producer-proved package lineage and package-relative path.
    pub const fn new(
        lineage: PackageLineage<'source>,
        path: &'source str,
    ) -> Result<Self, DeclarationKeyFault> {
        match DeclarationKey::new(lineage, path, EntityKind::Module, b"_") {
            Ok(_) => Ok(Self {
                lineage,
                path,
                coordinate: None,
            }),
            Err(cause) => Err(cause),
        }
    }

    /// Enters a package scope from the one already-parsed canonical coordinate.
    ///
    /// This retains version, qualifiers, and subpath for immutable image
    /// provenance in addition to the declaration lineage.
    pub fn for_package(
        coordinate: &'source PackageUrl,
        path: &'source str,
    ) -> Result<Self, PackageDeclarationScopeFault> {
        let lineage = PackageLineage::new(
            coordinate.package_type().as_str(),
            coordinate.lineage_name(),
        )
        .map_err(PackageDeclarationScopeFault::Lineage)?;
        DeclarationKey::new(lineage, path, EntityKind::Module, b"_")
            .map_err(PackageDeclarationScopeFault::Declaration)?;
        Ok(Self {
            lineage,
            path,
            coordinate: Some(coordinate),
        })
    }

    /// Enters the stable declaration scope used by the direct single-buffer
    /// application surface.
    ///
    /// Package compilation must supply its real package lineage and relative
    /// source path through [`Self::new`]. This scope is deliberately limited
    /// to the editor-style, package-less source command: its lineage and path
    /// remain stable across edits while source content remains version
    /// provenance rather than declaration identity.
    #[must_use]
    pub fn standalone(profile: LanguageProfile) -> DeclarationScope<'static> {
        let (ecosystem, path) = match profile {
            LanguageProfile::Rust(_) => ("standalone-rust", "input.rs"),
            LanguageProfile::TypeScript(backend_semantic::vocabulary::TypeScriptSource::TypeScript) => {
                ("standalone-typescript", "input.ts")
            }
            LanguageProfile::TypeScript(backend_semantic::vocabulary::TypeScriptSource::Tsx) => {
                ("standalone-typescript", "input.tsx")
            }
            LanguageProfile::Python(_) => ("standalone-python", "input.py"),
            LanguageProfile::Go(_) => ("standalone-go", "input.go"),
            LanguageProfile::Java(_) => ("standalone-java", "Input.java"),
            LanguageProfile::CSharp(_) => ("standalone-csharp", "Input.cs"),
            LanguageProfile::C(_) => ("standalone-clang", "input.c"),
            LanguageProfile::Cxx(_) => ("standalone-clang", "input.cc"),
        };
        let Ok(lineage) = PackageLineage::new(ecosystem, "editor-buffer") else {
            unreachable!("closed standalone package lineage is valid");
        };
        let Ok(scope) = DeclarationScope::<'static>::new(lineage, path) else {
            unreachable!("closed standalone declaration path is valid");
        };
        scope
    }

    pub(crate) const fn lineage(self) -> PackageLineage<'source> {
        self.lineage
    }

    pub(crate) const fn path(self) -> &'source str {
        self.path
    }

    pub(crate) const fn coordinate(self) -> Option<&'source PackageUrl> {
        self.coordinate
    }

    /// A deliberately obvious in-memory fixture scope.  This is exposed only
    /// so corpus fixtures can exercise the mandatory production field; real
    /// callers must enter their package lineage and source path with `new`.
    #[doc(hidden)]
    pub fn fixture() -> DeclarationScope<'static> {
        let Ok(lineage) = PackageLineage::new("fixture", "fixture") else {
            unreachable!("fixed fixture package lineage is valid");
        };
        let Ok(scope) = DeclarationScope::new(lineage, "fixture/source") else {
            unreachable!("fixed fixture declaration scope is valid");
        };
        scope
    }
}

/// An entered source buffer.  Its byte slice and durable identity enter the
/// pipeline together, so no later authority or lowerer can accidentally
/// analyze bytes under a different persisted identity.
#[derive(Clone, Copy, Debug)]
pub(crate) struct SourceLease<'source> {
    bytes: &'source [u8],
    identity: SourceIdentity,
}

impl<'source> SourceLease<'source> {
    pub(crate) fn enter(bytes: &'source [u8]) -> Result<Self, TryFromIntError> {
        let byte_len = u32::try_from(bytes.len())?;
        Ok(Self {
            bytes,
            identity: SourceIdentity {
                identity: ContentId::<SourceFactDomain>::from_canonical_bytes(bytes),
                byte_len,
            },
        })
    }

    pub(crate) const fn bytes(self) -> &'source [u8] {
        self.bytes
    }

    pub(crate) const fn identity(self) -> SourceIdentity {
        self.identity
    }
}

/// The only deadline/cancellation gate passed beyond entry.  It carries the
/// source identity whose work it governs, preventing detached traversal
/// loops from checking an unrelated request control.
#[derive(Clone, Copy, Debug)]
pub(crate) struct WorkPermit<'cancel> {
    source: SourceIdentity,
    deadline: Instant,
    cancelled: &'cancel AtomicBool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum WorkStopped {
    Cancelled,
    Deadline,
}

impl<'cancel> WorkPermit<'cancel> {
    pub(crate) const fn new(source: SourceIdentity, control: CompileControl<'cancel>) -> Self {
        Self {
            source,
            deadline: control.deadline,
            cancelled: control.cancelled,
        }
    }

    pub(crate) fn checkpoint(self) -> Result<(), WorkStopped> {
        if self.cancelled.load(Ordering::Acquire) {
            return Err(WorkStopped::Cancelled);
        }
        if Instant::now() >= self.deadline {
            return Err(WorkStopped::Deadline);
        }
        Ok(())
    }

    pub(crate) const fn source(self) -> SourceIdentity {
        self.source
    }

    pub(crate) const fn cancelled(self) -> &'cancel AtomicBool {
        self.cancelled
    }

    pub(crate) const fn control(self) -> CompileControl<'cancel> {
        CompileControl {
            deadline: self.deadline,
            cancelled: self.cancelled,
        }
    }
}

/// Project-bearing semantic authority required by a profile that cannot infer
/// its package graph from one source buffer.
#[derive(Clone, Copy, Debug)]
pub enum SemanticAuthorityInput<'source> {
    /// No profile-specific project authority accompanies this request.
    None,
    /// Caller-selected Cargo graph for in-process rust-analyzer admission.
    Rust {
        /// Exact Cargo root and toolchain context selected by the caller.
        project: &'source compiler_languages_rust::RustProject,
        /// Exact root-source byte budget checked before Cargo graph loading.
        maximum_source_bytes: compiler_languages_rust::SourceByteLimit,
        /// Complete Cargo feature controls forwarded to the rust-analyzer CargoConfig.
        features: compiler_languages_rust::RustFeatureControl<'source>,
    },
    /// Validated `go/packages` authority image bound to the exact request source.
    Go {
        /// Borrowed fixed-width authority bytes emitted by the configured Go producer.
        image: &'source [u8],
    },
    /// Validated Roslyn authority image bound to the exact request source.
    CSharp {
        /// Borrowed fixed-width authority bytes emitted by the configured Roslyn helper.
        image: &'source [u8],
    },
    /// Source-bound validated javac authority image for the exact request source.
    Java {
        /// Borrowed fixed-envelope bytes emitted by the configured javac producer.
        image: &'source [u8],
    },
    /// Borrowed TypeScript checker report bound to the exact request source.
    TypeScript {
        /// Validated checker facts produced for this source.
        report: &'source compiler_languages_typescript::Report,
    },
    /// Borrowed Python checker report bound to the exact request source.
    Python {
        /// Validated checker facts produced for this source.
        report: &'source compiler_languages_python::CheckerReport,
    },
}

/// Deadline and cancellation facts borrowed by one bounded native invocation.
#[derive(Clone, Copy, Debug)]
pub struct CompileControl<'cancel> {
    /// Monotonic deadline after which the native child is killed and reaped.
    pub deadline: Instant,
    /// Caller-owned cancellation flag observed before input and while waiting for the child.
    pub cancelled: &'cancel AtomicBool,
}

/// Immutable compile request borrowing recipe and cancellation authority from its caller.
#[derive(Clone, Copy, Debug)]
pub struct CompileRequest<'source, 'toolchain, 'cancel> {
    /// Closed language profile selected at the static registry boundary.
    pub profile: LanguageProfile,
    /// Requested semantic terminal; only `LowerIr` can produce a compact IR fragment.
    pub stage: Stage,
    /// Exact UTF-8-or-binary source bytes whose identity is persisted only on native lowering.
    pub source: &'source [u8],
    /// Producer-proved package/file scope used for stable declaration keys.
    /// Unlike source identity, this survives an edit of the file's contents.
    pub declaration_scope: DeclarationScope<'source>,
    /// Resolved native authority or explicit unavailable tool fact, never an ambient lookup.
    pub toolchain: ToolchainSelection<'toolchain>,
    /// Typed project authority required by profiles with semantic package context.
    pub authority: SemanticAuthorityInput<'source>,
    /// Bounded cancellation and deadline control for native work.
    pub control: CompileControl<'cancel>,
}

/// Internal recipe after the closed registry has admitted a resolved native toolchain.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct NativeRecipe<'source, 'toolchain> {
    pub(crate) profile: LanguageProfile,
    pub(crate) stage: Stage,
    pub(crate) source: &'source [u8],
    pub(crate) toolchain: ResolvedToolchain<'toolchain>,
}

/// Reusable caller-owned diagnostic lease; it never aliases semantic IR output.
pub struct CompileScratch<'diagnostic, 'work> {
    /// Bounded native stderr capture; a limit breach kills and reaps the native child.
    pub diagnostic_output: &'diagnostic mut [u8],
    /// Explicit caller-owned empty work directory; adapters never inherit the repository cwd.
    pub native_work: &'work Path,
}
