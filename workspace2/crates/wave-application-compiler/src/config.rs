//! Explicit local compiler configuration and caller-owned bounded scratch.

use std::{path::Path, sync::atomic::AtomicBool, time::Instant};

use nudox_compile_driver::ResolvedToolchain;
use nudox_compile_publication::manifest::StoredFragmentFacts;

/// Maximum stderr bytes retained by one local native compiler invocation.
pub const MAX_NATIVE_DIAGNOSTIC_OUTPUT_BYTES: usize = 256;
/// Maximum compact IR fragment bytes accepted by this single-request adapter.
pub const MAX_FRAGMENT_OUTPUT_BYTES: usize = 512;
/// Maximum canonical package-manifest bytes accepted by this one-fragment adapter.
pub const MAX_MANIFEST_OUTPUT_BYTES: usize = 512;
/// Maximum fragment entries published by one application `Generate` request.
pub const MAX_MANIFEST_ENTRIES: usize = 1;
/// Maximum all-resident locality bytes needed to verify the one-fragment package generation.
pub const MAX_LOCALITY_OUTPUT_BYTES: usize = 512;

/// Immutable explicit local paths and resolved native executable for one service instance.
#[derive(Clone, Copy, Debug)]
pub struct LocalCompilerConfig<'path, 'cancel> {
    /// Caller-resolved absolute executable and version identity; no ambient PATH lookup occurs.
    pub toolchain: ResolvedToolchain<'path>,
    /// Directory where immutable compact IR artifacts are stored by typed identity.
    pub artifact_directory: &'path Path,
    /// Directory owned by the durable journal publisher for facts, head, and journal frames.
    pub journal_directory: &'path Path,
    /// Empty caller-owned native work directory, distinct from the source and artifacts.
    pub native_work_directory: &'path Path,
    /// Caller-owned deadline and cancellation observation for each synchronous invocation.
    pub control: LocalCompilerControl<'cancel>,
}

/// Explicit cancellation and deadline authority for local native compilation.
#[derive(Clone, Copy, Debug)]
pub struct LocalCompilerControl<'cancel> {
    /// Monotonic deadline copied into the compiler request without an ambient timeout policy.
    pub deadline: Instant,
    /// Caller-owned cancellation state observed before and during native execution.
    pub cancelled: &'cancel AtomicBool,
}

/// Reusable caller-owned scratch for one synchronous local compiler service.
///
/// Fields remain private because their mutual non-aliasing and exact output capacities are the
/// adapter invariant; the owner passes this value into [`crate::LocalCompiler::create`].
pub struct LocalCompilerScratch {
    pub(crate) diagnostic_output: [u8; MAX_NATIVE_DIAGNOSTIC_OUTPUT_BYTES],
    pub(crate) fragment_output: [u8; MAX_FRAGMENT_OUTPUT_BYTES],
    pub(crate) manifest_output: [u8; MAX_MANIFEST_OUTPUT_BYTES],
    pub(crate) manifest_facts: [Option<StoredFragmentFacts>; MAX_MANIFEST_ENTRIES],
    pub(crate) ordinals: [usize; MAX_MANIFEST_ENTRIES],
    pub(crate) locality_output: [u8; MAX_LOCALITY_OUTPUT_BYTES],
    pub(crate) binding_output: [u8; nudox_compile_publication::binding::COMPILATION_BINDING_BYTES],
}

impl Default for LocalCompilerScratch {
    fn default() -> Self {
        Self {
            diagnostic_output: [0; MAX_NATIVE_DIAGNOSTIC_OUTPUT_BYTES],
            fragment_output: [0; MAX_FRAGMENT_OUTPUT_BYTES],
            manifest_output: [0; MAX_MANIFEST_OUTPUT_BYTES],
            manifest_facts: [None; MAX_MANIFEST_ENTRIES],
            ordinals: [0; MAX_MANIFEST_ENTRIES],
            locality_output: [0; MAX_LOCALITY_OUTPUT_BYTES],
            binding_output: [0; nudox_compile_publication::binding::COMPILATION_BINDING_BYTES],
        }
    }
}
