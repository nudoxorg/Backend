//! Defines config behavior for `compiler-application`, whose purpose is to bind application requests to native compilation and durable publication.
//! This module owns the config invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Explicit local compiler configuration and caller-owned bounded scratch.

use std::{ops::Deref, path::Path, sync::atomic::AtomicBool, time::Duration};

use compiler_driver::ToolchainSelection;
use compiler_publication::manifest::StoredFragmentFacts;
use compiler_vocabulary::{MAX_NATIVE_DIAGNOSTIC_BYTES, NativeTool};
use thiserror::Error;

/// Maximum explicit native-tool rows one local compiler configuration may borrow.
pub const MAX_LOCAL_TOOLCHAINS: usize = NativeTool::ALL.len();
/// Largest per-invocation native work interval accepted by the portable local adapter.
pub const MAX_LOCAL_COMPILER_TIMEOUT: Duration = Duration::from_hours(1);

/// Maximum compact IR fragment bytes accepted by this single-request adapter.
///
/// Sized for the schema-2 envelope: header, directory, entity/type-node/atom
/// lanes, source and recipe facts, and the type-fact plane with its
/// declared+computed segment header at the documented small-module bounds,
/// with headroom for pooled children. Re-frozen together with the emission
/// geometry constants.
pub const MAX_FRAGMENT_OUTPUT_BYTES: usize = 2048;
/// Maximum canonical package-manifest bytes accepted by this one-fragment adapter.
pub const MAX_MANIFEST_OUTPUT_BYTES: usize = 512;
/// Maximum fragment entries published by one application `Generate` request.
pub const MAX_MANIFEST_ENTRIES: usize = 1;
/// Maximum all-resident locality bytes needed to verify the one-fragment package generation.
pub const MAX_LOCALITY_OUTPUT_BYTES: usize = 512;

/// Immutable explicit local paths and validated native-tool table for one service instance.
#[derive(Clone, Copy, Debug)]
pub struct LocalCompilerConfig<'path, 'cancel> {
    /// Caller-owned sorted native-tool selections; no ambient PATH lookup occurs.
    pub toolchains: LocalToolchainSet<'path>,
    /// Directory where immutable compact IR artifacts are stored by typed identity.
    pub artifact_directory: &'path Path,
    /// Directory owned by the durable journal publisher for facts, head, and journal frames.
    pub journal_directory: &'path Path,
    /// Empty caller-owned native work directory, distinct from the source and artifacts.
    pub native_work_directory: &'path Path,
    /// Caller-owned timeout policy and cancellation observation for each invocation.
    pub control: LocalCompilerControl<'cancel>,
}

/// Validated, bounded native-tool selections borrowed from the local service owner.
///
/// The slice is strictly ordered by [`NativeTool`] and has no duplicate tool. Its storage stays
/// caller-owned, so adapting another local host never allocates a routing table.
#[derive(Clone, Copy, Debug)]
pub struct LocalToolchainSet<'path> {
    entries: &'path [ToolchainSelection<'path>],
}

impl<'path> LocalToolchainSet<'path> {
    /// Validates a borrowed, strictly ordered table of explicit native-tool selections.
    ///
    /// # Errors
    ///
    /// Returns a closed table error when the table is oversized, unordered, or duplicates a
    /// native tool. An explicitly unavailable row is valid and remains observable at dispatch.
    pub fn validate(
        entries: &'path [ToolchainSelection<'path>],
    ) -> Result<Self, LocalToolchainSetError> {
        if entries.len() > MAX_LOCAL_TOOLCHAINS {
            return Err(LocalToolchainSetError::Capacity {
                actual: entries.len(),
                maximum: MAX_LOCAL_TOOLCHAINS,
            });
        }
        let mut previous = None;
        for selection in entries {
            let observed = selection_tool(*selection);
            if let Some(preceding) = previous {
                let ordering = observed.cmp(&preceding);
                if ordering.is_eq() {
                    return Err(LocalToolchainSetError::Duplicate { tool: observed });
                }
                if ordering.is_lt() {
                    return Err(LocalToolchainSetError::OutOfOrder {
                        preceding,
                        observed,
                    });
                }
            }
            previous = Some(observed);
        }
        Ok(Self { entries })
    }

    /// Returns the one validated selection belonging to the registry-selected native tool.
    pub(crate) fn select(self, tool: NativeTool) -> Option<ToolchainSelection<'path>> {
        match self
            .entries
            .binary_search_by_key(&tool, |selection| selection_tool(*selection))
        {
            Ok(index) => self.entries.get(index).copied(),
            Err(_) => None,
        }
    }
}

impl<'path> Deref for LocalToolchainSet<'path> {
    type Target = [ToolchainSelection<'path>];

    fn deref(&self) -> &Self::Target {
        self.entries
    }
}

/// Typed rejection while validating caller-owned native-tool routing rows.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum LocalToolchainSetError {
    /// Caller supplied more entries than this bounded local adapter can inspect.
    #[error("local toolchain table has {actual} entries; maximum is {maximum}")]
    Capacity {
        /// Observed borrowed entry count.
        actual: usize,
        /// Adapter's fixed maximum entry count.
        maximum: usize,
    },
    /// Two rows attempted to own the same native tool.
    #[error("local toolchain table duplicates {tool:?}")]
    Duplicate {
        /// Duplicate closed native tool.
        tool: NativeTool,
    },
    /// The borrowed rows were not in the only canonical native-tool order.
    #[error("local toolchain table orders {observed:?} after {preceding:?}")]
    OutOfOrder {
        /// Earlier row that should have been lower than the later row.
        preceding: NativeTool,
        /// Later row that violated canonical order.
        observed: NativeTool,
    },
}

/// Explicit cancellation and per-invocation timeout authority for local native compilation.
#[derive(Clone, Copy, Debug)]
pub struct LocalCompilerControl<'cancel> {
    /// Validated duration sampled afresh for every compile request.
    pub timeout: LocalCompilerTimeout,
    /// Caller-owned cancellation state observed before and during native execution.
    pub cancelled: &'cancel AtomicBool,
}

/// Bounded duration sampled from the monotonic clock when an invocation starts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(transparent)]
pub struct LocalCompilerTimeout(Duration);

impl LocalCompilerTimeout {
    /// Validates a portable local compiler invocation interval.
    ///
    /// # Errors
    ///
    /// Rejects a zero or oversized interval before the compiler capability becomes service-ready.
    pub fn new(duration: Duration) -> Result<Self, LocalCompilerTimeoutError> {
        if duration.is_zero() {
            return Err(LocalCompilerTimeoutError::Zero);
        }
        if duration > MAX_LOCAL_COMPILER_TIMEOUT {
            return Err(LocalCompilerTimeoutError::TooLong {
                requested: duration,
                maximum: MAX_LOCAL_COMPILER_TIMEOUT,
            });
        }
        Ok(Self(duration))
    }
}

impl Deref for LocalCompilerTimeout {
    type Target = Duration;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl LocalCompilerControl<'_> {
    /// Samples a fresh monotonic deadline for one invocation.
    ///
    /// Returns a fresh monotonic deadline for this invocation without retaining a stale instant.
    pub(crate) fn deadline(self) -> Result<std::time::Instant, LocalCompilerTimeout> {
        let now = std::time::Instant::now();
        now.checked_add(*self.timeout).ok_or(self.timeout)
    }
}

/// Typed rejection while constructing a reusable local compiler timeout policy.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum LocalCompilerTimeoutError {
    /// A zero interval would make every native invocation immediately stale.
    #[error("local compiler timeout must be nonzero")]
    Zero,
    /// The interval cannot be represented by this bounded local adapter policy.
    #[error("local compiler timeout {requested:?} exceeds maximum {maximum:?}")]
    TooLong {
        /// Caller requested interval.
        requested: Duration,
        /// Maximum accepted interval.
        maximum: Duration,
    },
}

fn selection_tool(selection: ToolchainSelection<'_>) -> NativeTool {
    match selection {
        ToolchainSelection::ResolvedNative(resolved) => resolved.tool,
        ToolchainSelection::ExplicitlyUnavailable { tool } => tool,
    }
}

/// Reusable caller-owned scratch for one synchronous local compiler service.
///
/// Fields remain private because their mutual non-aliasing and exact output capacities are the
/// adapter invariant; the owner passes this value into [`crate::LocalCompiler::create`].
pub struct LocalCompilerScratch {
    pub(crate) diagnostic_output: [u8; MAX_NATIVE_DIAGNOSTIC_BYTES],
    pub(crate) fragment_output: [u8; MAX_FRAGMENT_OUTPUT_BYTES],
    pub(crate) manifest_output: [u8; MAX_MANIFEST_OUTPUT_BYTES],
    pub(crate) manifest_facts: [Option<StoredFragmentFacts>; MAX_MANIFEST_ENTRIES],
    pub(crate) ordinals: [usize; MAX_MANIFEST_ENTRIES],
    pub(crate) locality_output: [u8; MAX_LOCALITY_OUTPUT_BYTES],
    pub(crate) binding_output: [u8; compiler_publication::binding::COMPILATION_BINDING_BYTES],
}

impl Default for LocalCompilerScratch {
    fn default() -> Self {
        Self {
            diagnostic_output: [0; MAX_NATIVE_DIAGNOSTIC_BYTES],
            fragment_output: [0; MAX_FRAGMENT_OUTPUT_BYTES],
            manifest_output: [0; MAX_MANIFEST_OUTPUT_BYTES],
            manifest_facts: [None; MAX_MANIFEST_ENTRIES],
            ordinals: [0; MAX_MANIFEST_ENTRIES],
            locality_output: [0; MAX_LOCALITY_OUTPUT_BYTES],
            binding_output: [0; compiler_publication::binding::COMPILATION_BINDING_BYTES],
        }
    }
}
