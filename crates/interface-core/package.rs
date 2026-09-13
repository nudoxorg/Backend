//! Typed package-url admission for local package compilation.
//!
//! The parser retains one exact owned spelling and validated component ranges.
//! No package resolver receives an unclassified ecosystem string or an
//! unpinned coordinate.

use core::ops::Deref;
use std::{io::ErrorKind, path::Path};

use backend_semantic::vocabulary::LanguageProfile;
pub use backend_semantic::vocabulary::{
    MAX_PACKAGE_URL_BYTES, PackageTextRange, PackageType as PackageEcosystem, PackageUrl,
    PackageUrlError, PackageUrlFacts, RejectedPackageUrl,
};

use crate::GenerateTarget;

/// Ordered public phases of one package-to-document compilation journey.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PackageCompilePhase {
    /// Resolve the pinned coordinate beneath its explicitly configured ecosystem root.
    Locate,
    /// Admit exact package source bytes and stable package-relative declaration scope.
    EnterSource,
    /// Produce the language's real semantic authority facts.
    Authority,
    /// Lower one authority transaction into compact and rich semantic IR.
    Lower,
    /// Durably publish the compact fragment and complete semantic image.
    Publish,
    /// Reopen and validate the complete published semantic image.
    Reopen,
    /// Build searchable discovery facts from the reopened image.
    Discover,
    /// Project documentation and hyperlinks from the same reopened truth.
    Render,
}

/// Filesystem phase that rejected package source entry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PackageSourceIoPhase {
    /// Canonicalizing the configured package-store root.
    CanonicalizeStore,
    /// Enumerating Cargo registry namespaces.
    EnumerateRegistry,
    /// Canonicalizing the resolved package directory.
    CanonicalizePackage,
    /// Canonicalizing the requested source file.
    CanonicalizeSource,
    /// Reading exact source metadata.
    SourceMetadata,
    /// Reading the exact admitted source bytes.
    ReadSource,
}

/// Portable and platform I/O facts retained without formatting away the original category.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PackageSourceIoFact {
    /// Standard-library error category.
    pub kind: ErrorKind,
    /// Platform error number when the operating system supplied one.
    pub raw_os_code: Option<i32>,
}

/// Closed unsafe path-component reason.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PackagePathComponentError {
    /// Empty, current-directory, or parent-directory component.
    Traversal,
    /// A percent escape decoded to a path separator or NUL.
    EncodedSeparator,
    /// Escape bytes were not valid UTF-8 after decoding.
    InvalidUtf8,
}

/// Exact declaration-scope rejection after package and source paths were resolved.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PackageDeclarationScopeCause {
    /// Ecosystem lineage segment was empty.
    EmptyEcosystem,
    /// Package lineage segment was empty.
    EmptyPackage,
    /// Ecosystem lineage segment contained the reserved separator.
    EcosystemSeparator,
    /// Package lineage segment contained the reserved separator.
    PackageSeparator,
    /// One lineage segment contained a platform path separator.
    LineageBackslash {
        /// Offending segment (`0` ecosystem, `1` package).
        segment: u8,
    },
    /// Package-relative source path was empty.
    EmptySourcePath,
    /// Package-relative source path contained a platform path separator.
    SourceBackslash,
}

/// Exact package-source resolution rejection.
#[derive(Debug, Eq, PartialEq)]
pub enum PackageSourceCause {
    /// No explicit root owns the request ecosystem.
    RootUnavailable {
        /// Ecosystem whose root was absent.
        ecosystem: PackageEcosystem,
    },
    /// Source selection was omitted rather than guessed.
    SubpathRequired {
        /// Ecosystem whose package needs an entry subpath.
        ecosystem: PackageEcosystem,
    },
    /// A PURL path component was unsafe or not representable as one local component.
    InvalidComponent {
        /// Exact canonical-PURL byte range containing the component.
        range: PackageTextRange,
        /// Closed component rejection.
        cause: PackagePathComponentError,
    },
    /// The canonical PURL could not mint a stable declaration scope.
    DeclarationScope {
        /// Exact closed scope rejection.
        cause: PackageDeclarationScopeCause,
    },
    /// Package source was not UTF-8 and cannot enter a semantic source compiler.
    InvalidUtf8 {
        /// Bytes before the first invalid sequence.
        valid_up_to: usize,
        /// Invalid sequence width when known.
        error_len: Option<u8>,
    },
    /// The requested package directory is absent.
    PackageUnavailable {
        /// Exact resolved path.
        path: Box<Path>,
    },
    /// Canonical package resolution escaped its configured ecosystem store.
    PackageEscapesStore {
        /// Canonical configured store root.
        store: Box<Path>,
        /// Canonical escaped package directory.
        package: Box<Path>,
    },
    /// The requested source file is absent or not a regular file.
    SourceUnavailable {
        /// Exact resolved path.
        path: Box<Path>,
    },
    /// Canonical source resolution escaped its canonical package directory.
    SourceEscapesPackage {
        /// Canonical package directory.
        package: Box<Path>,
        /// Canonical escaped source path.
        source: Box<Path>,
    },
    /// Exact source extent exceeded the package compiler budget.
    SourceTooLarge {
        /// Observed metadata length.
        observed: u64,
        /// Maximum accepted length.
        maximum: u64,
    },
    /// One exact filesystem operation failed.
    Io {
        /// Failed phase.
        phase: PackageSourceIoPhase,
        /// Exact path operand.
        path: Box<Path>,
        /// Preserved I/O facts.
        source: PackageSourceIoFact,
    },
    /// Cargo's registry namespace count exceeded the fixed local bound.
    RegistryNamespaceCapacity {
        /// Observed directory rows before stopping.
        observed: usize,
        /// Fixed maximum inspected rows.
        maximum: usize,
    },
    /// More than one Cargo registry namespace contained the exact pinned package.
    RegistryNamespaceAmbiguous {
        /// First canonical-order matching package directory.
        first: Box<Path>,
        /// Second canonical-order matching package directory proving ambiguity.
        second: Box<Path>,
    },
}

/// Immutable package compilation facts exposed by [`PackageCompileRequest`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PackageCompileFacts {
    /// Compiler profile, stage, and caller correlation.
    pub target: GenerateTarget,
    /// Ecosystem already proven compatible with `target.profile`.
    pub ecosystem: PackageEcosystem,
}

/// One profile-compatible, pinned package request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackageCompileRequest {
    facts: PackageCompileFacts,
    package: PackageUrl,
}

impl PackageCompileRequest {
    /// Binds a validated package URL to its one compatible language profile.
    ///
    /// # Errors
    ///
    /// Returns both closed families when a caller attempts to route a package
    /// through a foreign language adapter.
    pub fn new(
        target: GenerateTarget,
        package: PackageUrl,
    ) -> Result<Self, PackageProfileMismatch> {
        let ecosystem = package.ecosystem;
        let profile_language = target.profile.language();
        let package_language = ecosystem.language();
        if profile_language != package_language {
            return Err(PackageProfileMismatch {
                profile: target.profile,
                ecosystem,
            });
        }
        Ok(Self {
            facts: PackageCompileFacts { target, ecosystem },
            package,
        })
    }

    pub(crate) const fn correlation(&self) -> crate::CorrelationId {
        self.facts.target.correlation
    }
}

impl Deref for PackageCompileRequest {
    type Target = PackageCompileFacts;

    fn deref(&self) -> &Self::Target {
        &self.facts
    }
}

impl AsRef<PackageUrl> for PackageCompileRequest {
    fn as_ref(&self) -> &PackageUrl {
        &self.package
    }
}

/// Exact profile/ecosystem mismatch rejected before package resolution.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PackageProfileMismatch {
    /// Requested compiler profile.
    pub profile: LanguageProfile,
    /// Package ecosystem incompatible with the profile.
    pub ecosystem: PackageEcosystem,
}
