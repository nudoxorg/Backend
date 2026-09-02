//! Defines types request behavior for `compiler-driver`, whose purpose is to run bounded native toolchains and lower their output into canonical IR.
//! This module owns the types request invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
use std::{path::Path, sync::atomic::AtomicBool, time::Instant};

use compiler_vocabulary::{LanguageProfile, Stage};

use super::{ResolvedToolchain, ToolchainSelection};

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
