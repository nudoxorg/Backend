//! Defines config behavior for the `backend-engine` application, whose purpose is to bind application requests to native compilation and durable publication.
//! This module owns the config invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Explicit local compiler configuration and caller-owned bounded scratch.

use std::{
    collections::TryReserveError, ops::Deref, path::Path, sync::atomic::AtomicBool, time::Duration,
};

use crate::driver::ToolchainSelection;
use crate::publication::manifest::StoredFragmentFacts;
use backend_library::interface::PackageEcosystem;
use backend_semantic::vocabulary::{MAX_NATIVE_DIAGNOSTIC_BYTES, NativeTool};
use thiserror::Error;

/// Maximum explicit native-tool rows one local compiler configuration may borrow.
pub const MAX_LOCAL_TOOLCHAINS: usize = NativeTool::ALL.len();
/// Maximum explicit package-root rows one local compiler configuration may borrow.
pub const MAX_LOCAL_PACKAGE_ROOTS: usize = 7;
/// Largest per-invocation native work interval accepted by the portable local adapter.
pub const MAX_LOCAL_COMPILER_TIMEOUT: Duration = Duration::from_hours(1);

/// Inline compact IR fragment bytes accepted without a heap allocation.
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
pub const MAX_MANIFEST_ENTRIES: usize = 100_000;
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

/// Immutable facts of one validated local package-store root.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LocalPackageRootFacts<'path> {
    /// Closed ecosystem whose package-manager layout is rooted here.
    pub ecosystem: PackageEcosystem,
    /// Absolute caller-selected cache or repository root.
    pub path: &'path Path,
}

/// One absolute package-store root accepted after entry validation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LocalPackageRoot<'path> {
    facts: LocalPackageRootFacts<'path>,
}

impl<'path> LocalPackageRoot<'path> {
    /// Validates one explicit package-store root without consulting environment state.
    ///
    /// # Errors
    ///
    /// Returns the rejected ecosystem when `path` is relative.
    pub fn new(
        ecosystem: PackageEcosystem,
        path: &'path Path,
    ) -> Result<Self, LocalPackageRootError> {
        if !path.is_absolute() {
            return Err(LocalPackageRootError::Relative { ecosystem });
        }
        Ok(Self {
            facts: LocalPackageRootFacts { ecosystem, path },
        })
    }
}

impl<'path> Deref for LocalPackageRoot<'path> {
    type Target = LocalPackageRootFacts<'path>;

    fn deref(&self) -> &Self::Target {
        &self.facts
    }
}

/// Canonically ordered, caller-owned package-store roots.
#[derive(Clone, Copy, Debug)]
pub struct LocalPackageRootSet<'path> {
    entries: &'path [LocalPackageRoot<'path>],
}

impl<'path> LocalPackageRootSet<'path> {
    /// The explicit absence of all package stores.
    pub const EMPTY: Self = Self { entries: &[] };

    /// Validates a bounded table in strictly ascending ecosystem order.
    ///
    /// # Errors
    ///
    /// Returns a typed capacity, duplicate, or ordering rejection.
    pub fn validate(
        entries: &'path [LocalPackageRoot<'path>],
    ) -> Result<Self, LocalPackageRootSetError> {
        if entries.len() > MAX_LOCAL_PACKAGE_ROOTS {
            return Err(LocalPackageRootSetError::Capacity {
                actual: entries.len(),
                maximum: MAX_LOCAL_PACKAGE_ROOTS,
            });
        }
        let mut previous = None;
        for entry in entries {
            if let Some(preceding) = previous {
                match entry.ecosystem.cmp(&preceding) {
                    core::cmp::Ordering::Equal => {
                        return Err(LocalPackageRootSetError::Duplicate {
                            ecosystem: entry.ecosystem,
                        });
                    }
                    core::cmp::Ordering::Less => {
                        return Err(LocalPackageRootSetError::OutOfOrder {
                            preceding,
                            observed: entry.ecosystem,
                        });
                    }
                    core::cmp::Ordering::Greater => {}
                }
            }
            previous = Some(entry.ecosystem);
        }
        Ok(Self { entries })
    }

    pub(crate) fn select(self, ecosystem: PackageEcosystem) -> Option<&'path Path> {
        self.entries
            .binary_search_by_key(&ecosystem, |entry| entry.ecosystem)
            .ok()
            .and_then(|index| self.entries.get(index))
            .map(|entry| entry.path)
    }
}

impl<'path> Deref for LocalPackageRootSet<'path> {
    type Target = [LocalPackageRoot<'path>];

    fn deref(&self) -> &Self::Target {
        self.entries
    }
}

/// Rejection while entering one package-store root.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum LocalPackageRootError {
    /// Relative roots would make package resolution depend on ambient process state.
    #[error("local {ecosystem:?} package root is relative")]
    Relative {
        /// Ecosystem whose root was rejected.
        ecosystem: PackageEcosystem,
    },
}

/// Rejection while validating the complete package-root table.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum LocalPackageRootSetError {
    /// More than one root per closed ecosystem was supplied.
    #[error("local package-root table has {actual} rows; maximum is {maximum}")]
    Capacity {
        /// Observed root count.
        actual: usize,
        /// Closed table maximum.
        maximum: usize,
    },
    /// Two rows attempted to own one ecosystem.
    #[error("local package-root table duplicates {ecosystem:?}")]
    Duplicate {
        /// Duplicated ecosystem.
        ecosystem: PackageEcosystem,
    },
    /// Rows were not ordered by the closed ecosystem vocabulary.
    #[error("local package-root table orders {observed:?} after {preceding:?}")]
    OutOfOrder {
        /// Earlier row.
        preceding: PackageEcosystem,
        /// Later lower-valued row.
        observed: PackageEcosystem,
    },
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
/// adapter invariant; the owner passes this value into [`crate::application::LocalCompiler::create`].
pub struct LocalCompilerScratch {
    pub(crate) diagnostic_output: [u8; MAX_NATIVE_DIAGNOSTIC_BYTES],
    pub(crate) fragment_output: FragmentOutput,
    pub(crate) manifest_output: Vec<u8>,
    pub(crate) manifest_facts: Vec<Option<StoredFragmentFacts>>,
    pub(crate) ordinals: Vec<usize>,
    pub(crate) locality_output: [u8; MAX_LOCALITY_OUTPUT_BYTES],
    pub(crate) binding_output: [u8; crate::publication::binding::COMPILATION_BINDING_BYTES],
    /// Reusable exact-demand storage for the complete semantic image.  The
    /// first fused compile grows this lane to the measured image length;
    /// later requests reuse its allocation and expose only the initialized
    /// prefix to publication and reopen.
    pub(crate) semantic_image_output: Vec<u8>,
    pub(crate) semantic_image_plan: Vec<crate::publication::manifest::SemanticImageRegion>,
    pub(crate) reopened_fragment_output: Vec<u8>,
}

pub(crate) enum FragmentOutput {
    Inline([u8; MAX_FRAGMENT_OUTPUT_BYTES]),
    Planned(Box<[u8]>),
}

impl FragmentOutput {
    pub(crate) fn as_mut(&mut self) -> &mut [u8] {
        match self {
            Self::Inline(bytes) => bytes,
            Self::Planned(bytes) => bytes,
        }
    }
}

impl LocalCompilerScratch {
    pub(crate) fn prepare_publication(
        &mut self,
        artifacts: usize,
        manifest_bytes: usize,
        fragment_bytes: usize,
        semantic_bytes: usize,
    ) -> Result<(), LocalCompilerScratchError> {
        if artifacts == 0 || artifacts > MAX_MANIFEST_ENTRIES {
            return Err(LocalCompilerScratchError::ArtifactCapacity {
                requested: artifacts,
                maximum: MAX_MANIFEST_ENTRIES,
            });
        }
        resize_zeroed(&mut self.manifest_output, manifest_bytes)?;
        resize_none(&mut self.manifest_facts, artifacts)?;
        resize_zeroed(&mut self.ordinals, artifacts)?;
        resize_regions(&mut self.semantic_image_plan, artifacts)?;
        resize_zeroed(&mut self.reopened_fragment_output, fragment_bytes)?;
        resize_zeroed(&mut self.semantic_image_output, semantic_bytes)?;
        Ok(())
    }

    /// Creates reusable scratch with an explicit compact-fragment output capacity.
    ///
    /// Capacities at or below [`MAX_FRAGMENT_OUTPUT_BYTES`] retain the inline lane. Larger
    /// capacities allocate exactly once and remain owned by this scratch across every request.
    ///
    /// # Errors
    ///
    /// Returns the exact width or allocation rejection before a compiler owner is started.
    pub fn with_fragment_capacity(
        capacity: core::num::NonZeroUsize,
    ) -> Result<Self, LocalCompilerScratchError> {
        if capacity.get() > u32::MAX as usize {
            return Err(LocalCompilerScratchError::CapacityWidth {
                requested: capacity.get(),
                maximum: u32::MAX as usize,
            });
        }
        if capacity.get() <= MAX_FRAGMENT_OUTPUT_BYTES {
            return Ok(Self::default());
        }
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(capacity.get())
            .map_err(LocalCompilerScratchError::Allocation)?;
        bytes.resize(capacity.get(), 0);
        let mut scratch = Self::default();
        scratch.fragment_output = FragmentOutput::Planned(bytes.into_boxed_slice());
        Ok(scratch)
    }
}

/// Rejection while constructing an explicitly sized reusable fragment lane.
#[derive(Debug, Error)]
pub enum LocalCompilerScratchError {
    /// A package attempted to publish more artifacts than the bounded manifest grammar admits.
    #[error("package publication requested {requested} artifacts; maximum is {maximum}")]
    ArtifactCapacity {
        /// Requested artifact count.
        requested: usize,
        /// Maximum admitted artifact count.
        maximum: usize,
    },
    /// The requested capacity does not fit the canonical fragment byte-coordinate width.
    #[error("fragment scratch capacity {requested} exceeds maximum {maximum}")]
    CapacityWidth {
        /// Requested reusable byte capacity.
        requested: usize,
        /// Largest canonical fragment byte capacity.
        maximum: usize,
    },
    /// The exact one-time scratch allocation could not be reserved.
    #[error("fragment scratch allocation failed")]
    Allocation(#[source] TryReserveError),
}

fn resize_zeroed<T: Default + Clone>(
    values: &mut Vec<T>,
    length: usize,
) -> Result<(), LocalCompilerScratchError> {
    if length > values.len() {
        values
            .try_reserve_exact(length - values.len())
            .map_err(LocalCompilerScratchError::Allocation)?;
    }
    values.resize(length, T::default());
    Ok(())
}

fn resize_none<T>(
    values: &mut Vec<Option<T>>,
    length: usize,
) -> Result<(), LocalCompilerScratchError> {
    if length > values.len() {
        values
            .try_reserve_exact(length - values.len())
            .map_err(LocalCompilerScratchError::Allocation)?;
    }
    values.resize_with(length, || None);
    values.fill_with(|| None);
    Ok(())
}

fn resize_regions(
    values: &mut Vec<crate::publication::manifest::SemanticImageRegion>,
    length: usize,
) -> Result<(), LocalCompilerScratchError> {
    if length > values.len() {
        values
            .try_reserve_exact(length - values.len())
            .map_err(LocalCompilerScratchError::Allocation)?;
    }
    values.resize(
        length,
        crate::publication::manifest::SemanticImageRegion::EMPTY,
    );
    values.fill(crate::publication::manifest::SemanticImageRegion::EMPTY);
    Ok(())
}

impl Default for LocalCompilerScratch {
    fn default() -> Self {
        Self {
            diagnostic_output: [0; MAX_NATIVE_DIAGNOSTIC_BYTES],
            fragment_output: FragmentOutput::Inline([0; MAX_FRAGMENT_OUTPUT_BYTES]),
            manifest_output: vec![0; MAX_MANIFEST_OUTPUT_BYTES],
            manifest_facts: vec![None],
            ordinals: vec![0],
            locality_output: [0; MAX_LOCALITY_OUTPUT_BYTES],
            binding_output: [0; crate::publication::binding::COMPILATION_BINDING_BYTES],
            semantic_image_output: Vec::new(),
            semantic_image_plan: vec![crate::publication::manifest::SemanticImageRegion::EMPTY],
            reopened_fragment_output: Vec::new(),
        }
    }
}
