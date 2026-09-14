//! Snapshot/query authority and cancellation.

use core::num::NonZeroU8;
use core::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};

use thiserror::Error;

/// Maximum selected lexical segments in one immutable snapshot.
pub const MAX_SEGMENTS: usize = backend_semantic::index_core::MAX_SELECTED_SEGMENTS;
/// Maximum worker candidates retained for one segment's bounded retry route.
pub const MAX_WORKERS: usize = 64;
/// Maximum result rows admitted by the routing boundary.
pub const MAX_TOP_K: usize = 256;
/// A dispatch loop samples cancellation at least once per this many attempts.
pub const CANCELLATION_SAMPLE_INTERVAL: usize = 8;

/// A fixed-width worker identity.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct WorkerId(u32);

impl WorkerId {
    /// Creates a worker identity from its stable wire number.
    #[must_use]
    pub const fn new(value: u32) -> Self {
        Self(value)
    }

    /// Returns the stable wire number.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

/// A fixed-width route identity for one planned query.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct RouteId([u8; 32]);

impl RouteId {
    pub(crate) const fn from_raw(value: [u8; 32]) -> Self {
        Self(value)
    }

    /// Returns the route's stable BLAKE3 digest bytes.
    #[must_use]
    pub const fn as_bytes(self) -> [u8; 32] {
        self.0
    }
}

/// A fixed-width segment ordinal in canonical snapshot selection order.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct SegmentOrdinal(pub(crate) u8);

impl SegmentOrdinal {
    /// Creates a segment ordinal when it fits the bounded selection.
    ///
    /// # Errors
    ///
    /// Returns [`BoundsError::Segments`] when `value` is outside the fixed selection width.
    pub fn new(value: usize) -> Result<Self, BoundsError> {
        if value >= MAX_SEGMENTS {
            Err(BoundsError::Segments { observed: value })
        } else {
            match u8::try_from(value) {
                Ok(value) => Ok(Self(value)),
                Err(_) => Err(BoundsError::Segments { observed: value }),
            }
        }
    }

    /// Returns the zero-based selection position.
    #[must_use]
    pub fn get(self) -> usize {
        usize::from(self.0)
    }
}

/// A fixed-width digest of the exact query bytes.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct QueryDigest([u8; 32]);

impl QueryDigest {
    /// Computes the canonical BLAKE3 digest of the exact query bytes.
    #[must_use]
    pub fn from_bytes(bytes: &[u8]) -> Self {
        Self(*blake3::hash(bytes).as_bytes())
    }

    /// Creates a digest from an already verified fixed-width value.
    #[must_use]
    pub const fn from_raw(value: [u8; 32]) -> Self {
        Self(value)
    }

    /// Returns the fixed-width digest bytes.
    #[must_use]
    pub const fn as_bytes(self) -> [u8; 32] {
        self.0
    }
}

/// A caller-selected deterministic ordering recipe identity.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct OrderingRecipe(u32);

impl OrderingRecipe {
    /// Creates an ordering recipe identity.  Interpretation belongs to the caller.
    #[must_use]
    pub const fn new(value: u32) -> Self {
        Self(value)
    }

    /// Returns the recipe identity.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

/// A fixed-width result bound.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct TopK(pub(crate) usize);

impl TopK {
    /// Checks a result bound before routing work begins.
    ///
    /// # Errors
    ///
    /// Returns [`BoundsError::TopK`] when `value` exceeds [`MAX_TOP_K`].
    pub const fn new(value: usize) -> Result<Self, BoundsError> {
        if value > MAX_TOP_K {
            Err(BoundsError::TopK { observed: value })
        } else {
            Ok(Self(value))
        }
    }

    /// Returns the result bound.
    #[must_use]
    pub const fn get(self) -> usize {
        self.0
    }
}

/// Exact, borrowed query inputs bound into a route plan.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Query<'query> {
    /// Query bytes borrowed from the caller.
    pub(crate) bytes: &'query [u8],
    /// Exact digest of [`Self::bytes`].
    pub(crate) digest: QueryDigest,
    /// Ordering recipe retained by cursors and replies.
    pub(crate) ordering: OrderingRecipe,
    /// Bounded output width.
    pub(crate) top_k: TopK,
}

impl<'query> Query<'query> {
    /// Creates a query with the default maximum output width.
    #[must_use]
    pub fn new(bytes: &'query [u8], ordering: OrderingRecipe) -> Self {
        Self {
            bytes,
            digest: QueryDigest::from_bytes(bytes),
            ordering,
            top_k: TopK(MAX_TOP_K),
        }
    }

    /// Rebinds the result width while retaining borrowed query bytes and digest.
    #[must_use]
    pub const fn with_top_k(self, top_k: TopK) -> Self {
        Self { top_k, ..self }
    }

    /// Returns the borrowed query bytes.
    #[must_use]
    pub const fn bytes(self) -> &'query [u8] {
        self.bytes
    }

    /// Returns the canonical query digest.
    #[must_use]
    pub const fn digest(self) -> QueryDigest {
        self.digest
    }

    /// Returns the ordering recipe.
    #[must_use]
    pub const fn ordering(self) -> OrderingRecipe {
        self.ordering
    }

    /// Returns the bounded output width.
    #[must_use]
    pub const fn top_k(self) -> TopK {
        self.top_k
    }
}
/// Exact retry policy for stale or unavailable worker attempts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RetryPolicy(pub(crate) NonZeroU8);

impl RetryPolicy {
    /// Creates a policy with exactly this many attempts per segment.
    ///
    /// # Errors
    ///
    /// Returns [`BoundsError::RetryAttempts`] when `attempts` is zero.
    pub fn new(attempts: u8) -> Result<Self, BoundsError> {
        NonZeroU8::new(attempts)
            .map(Self)
            .ok_or(BoundsError::RetryAttempts)
    }

    /// Returns the configured attempt count.
    #[must_use]
    pub const fn attempts(self) -> u8 {
        self.0.get()
    }
}

/// Caller-owned cancellation authority sampled by dispatch.
#[derive(Debug, Default)]
pub struct Cancellation(AtomicBool);

impl Cancellation {
    /// Creates a clear cancellation authority.
    #[must_use]
    pub const fn new() -> Self {
        Self(AtomicBool::new(false))
    }

    /// Requests cancellation.  The request is monotonic for this operation.
    pub fn cancel(&self) {
        self.0.store(true, AtomicOrdering::Release);
    }

    /// Samples the cancellation state.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.0.load(AtomicOrdering::Acquire)
    }
}

/// Bounded admission failures.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Error)]
pub enum BoundsError {
    /// Selected segment count exceeded the fixed route width.
    #[error("selected segment count {observed} exceeds {MAX_SEGMENTS}")]
    Segments {
        /// Complete observed segment count.
        observed: usize,
    },
    /// Worker directory exceeded the fixed candidate width.
    #[error("worker count {observed} exceeds {MAX_WORKERS}")]
    Workers {
        /// Complete observed worker count.
        observed: usize,
    },
    /// Result bound exceeded the fixed output width.
    #[error("top-k {observed} exceeds {MAX_TOP_K}")]
    TopK {
        /// Complete observed result bound.
        observed: usize,
    },
    /// Retry policy cannot contain zero attempts.
    #[error("retry attempts must be nonzero")]
    RetryAttempts,
}
