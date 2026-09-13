//! Defines types toolchain behavior for `compiler-driver`, whose purpose is to run bounded native toolchains and lower their output into canonical IR.
//! This module owns the types toolchain invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
use core::ops::Deref;
use std::path::Path;

use compiler_vocabulary::NativeTool;
use backend_version::{ContentId, ToolchainDomain};
use thiserror::Error;

/// Immutable, caller-resolved native executable facts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResolvedToolchainView {
    /// Concrete tool family whose command line the caller resolved.
    pub tool: NativeTool,
    /// Central typed identity derived from caller-supplied exact version bytes.
    pub identity: ContentId<ToolchainDomain>,
}

/// Caller-resolved absolute executable capability with private execution authority.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResolvedToolchain<'path> {
    view: ResolvedToolchainView,
    executable: &'path Path,
}

impl<'path> Deref for ResolvedToolchain<'path> {
    type Target = ResolvedToolchainView;

    fn deref(&self) -> &Self::Target {
        &self.view
    }
}

/// Exposes the already-validated executable path without granting a second
/// mutable or ambient tool-resolution route.
impl AsRef<Path> for ResolvedToolchain<'_> {
    fn as_ref(&self) -> &Path {
        self.executable
    }
}

/// Rejection while binding an explicit executable to its version provenance.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum ToolchainResolutionError {
    /// Relative executable lookup would consult ambient process search state.
    #[error("resolved executable path is not absolute")]
    RelativeExecutable,
}

impl<'path> ResolvedToolchain<'path> {
    /// Binds one absolute executable path to exact caller-probed tool version bytes.
    pub fn from_version(
        tool: NativeTool,
        executable: &'path Path,
        version_bytes: &[u8],
    ) -> Result<Self, ToolchainResolutionError> {
        Self::from_identity(
            tool,
            executable,
            ContentId::<ToolchainDomain>::from_canonical_bytes(version_bytes),
        )
    }

    /// Rebinds a caller-owned executable path to its already-proven exact toolchain identity.
    ///
    /// This preserves version provenance when an outer asynchronous adapter takes ownership of
    /// configuration without running a second ambient tool probe.
    pub fn from_identity(
        tool: NativeTool,
        executable: &'path Path,
        identity: ContentId<ToolchainDomain>,
    ) -> Result<Self, ToolchainResolutionError> {
        if !executable.is_absolute() {
            return Err(ToolchainResolutionError::RelativeExecutable);
        }
        Ok(Self {
            view: ResolvedToolchainView { tool, identity },
            executable,
        })
    }

    pub(crate) fn executable(self) -> &'path Path {
        self.executable
    }
}

/// Closed toolchain state supplied to a compilation request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ToolchainSelection<'path> {
    /// A validated absolute executable may service one locally reproducible native adapter.
    ResolvedNative(ResolvedToolchain<'path>),
    /// This language's selected native tool is intentionally unavailable without a forged path.
    ExplicitlyUnavailable { tool: NativeTool },
}

/// Exact shape of a supplied toolchain selection retained by mismatch diagnostics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ToolchainSelectionFact {
    /// Request supplied a resolved executable for this concrete tool family.
    ResolvedNative { tool: NativeTool },
    /// Request supplied an explicit unavailable terminal for this concrete tool family.
    ExplicitlyUnavailable { tool: NativeTool },
}

impl<'path> ToolchainSelection<'path> {
    pub(super) fn fact(self) -> ToolchainSelectionFact {
        match self {
            Self::ResolvedNative(resolved) => ToolchainSelectionFact::ResolvedNative {
                tool: resolved.tool,
            },
            Self::ExplicitlyUnavailable { tool } => {
                ToolchainSelectionFact::ExplicitlyUnavailable { tool }
            }
        }
    }
}
