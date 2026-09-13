//! Compact and full-width logical read selectors.

use crate::canonical::canonical_scoped_read;
use crate::support::facet_tag;
use crate::{FacetKind, SemanticError};
use backend_version::ScopeRoot;
use std::cmp::Ordering;
use std::hash::{Hash, Hasher};

/// A compact read descriptor retained for compatibility with replication.
/// `range == 0` denotes an exact key read; a nonzero range is a canonical
/// range/prefix token. `negative` is an absence assertion and requires a
/// complete witness before it can be used for reuse.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Read {
    facet: FacetKind,
    scope: ScopeRoot,
    negative: bool,
    range: u64,
}

impl Read {
    /// Creates an exact positive read.
    #[must_use]
    pub fn exact<S: crate::ScopeIdentity>(facet: FacetKind, scope: S) -> Self {
        Self {
            facet,
            scope: scope.scope_root(),
            negative: false,
            range: 0,
        }
    }

    /// Creates an exact negative read.
    #[must_use]
    pub fn negative<S: crate::ScopeIdentity>(facet: FacetKind, scope: S) -> Self {
        Self {
            facet,
            scope: scope.scope_root(),
            negative: true,
            range: 0,
        }
    }

    /// Creates a positive range/prefix read.
    #[must_use]
    pub fn range<S: crate::ScopeIdentity>(facet: FacetKind, scope: S, range: u64) -> Self {
        Self {
            facet,
            scope: scope.scope_root(),
            negative: false,
            range,
        }
    }

    /// Creates a negative range/prefix read.
    #[must_use]
    pub fn negative_range<S: crate::ScopeIdentity>(facet: FacetKind, scope: S, range: u64) -> Self {
        Self {
            facet,
            scope: scope.scope_root(),
            negative: true,
            range,
        }
    }

    /// Returns whether this read is a range/prefix read.
    #[must_use]
    pub const fn is_range(self) -> bool {
        self.range != 0
    }

    /// Returns the facet/relation being read.
    #[must_use]
    pub const fn facet(self) -> FacetKind {
        self.facet
    }

    /// Returns the exact authority scope root bound to this read.
    #[must_use]
    pub const fn scope_root(self) -> ScopeRoot {
        self.scope
    }

    /// Returns whether this read asserts absence.
    #[must_use]
    pub const fn is_negative(self) -> bool {
        self.negative
    }

    /// Returns the compact range token, or zero for an exact read.
    #[must_use]
    pub const fn range_token(self) -> u64 {
        self.range
    }

    /// Returns whether a changed read can alter this exact/range observation.
    #[must_use]
    pub fn intersects(self, changed: Self) -> bool {
        if facet_tag(self.facet) != facet_tag(changed.facet)
            || self.scope.as_bytes() != changed.scope.as_bytes()
        {
            return false;
        }
        self.range == 0 || changed.range == 0 || self.range == changed.range
    }

    /// Returns whether an exact/range pair has opposite polarity over an
    /// overlapping compact region.
    #[must_use]
    pub fn polarity_conflicts_with(self, other: Self) -> bool {
        self.facet == other.facet
            && self.scope == other.scope
            && self.negative != other.negative
            && self.intersects(other)
    }
}

impl Hash for Read {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.facet.hash(state);
        self.scope.as_bytes().hash(state);
        self.negative.hash(state);
        self.range.hash(state);
    }
}

impl Ord for Read {
    fn cmp(&self, other: &Self) -> Ordering {
        self.facet
            .cmp(&other.facet)
            .then_with(|| self.scope.as_bytes().cmp(other.scope.as_bytes()))
            .then_with(|| self.range.cmp(&other.range))
            .then_with(|| self.negative.cmp(&other.negative))
    }
}

impl PartialOrd for Read {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Exact logical selector used by the full-width dependency manifest.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum ReadSelector {
    /// One exact canonical relation key.
    Exact(Vec<u8>),
    /// A half-open canonical key interval.
    Range(ReadRange),
    /// A canonical key prefix, equivalent to its complete key range.
    Prefix(Vec<u8>),
}

/// Validated half-open canonical key interval.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ReadRange {
    start: Vec<u8>,
    end: Vec<u8>,
}

impl ReadRange {
    /// Creates a range whose end sorts strictly after its start.
    ///
    /// # Errors
    ///
    /// Returns [`SemanticError::InvalidSelector`] when the end does not sort
    /// after the start.
    pub fn new(start: Vec<u8>, end: Vec<u8>) -> Result<Self, SemanticError> {
        if start >= end {
            Err(SemanticError::InvalidSelector)
        } else {
            Ok(Self { start, end })
        }
    }

    /// Returns the inclusive lower bound.
    #[must_use]
    pub fn start(&self) -> &[u8] {
        &self.start
    }

    /// Returns the exclusive upper bound.
    #[must_use]
    pub fn end(&self) -> &[u8] {
        &self.end
    }
}

impl ReadSelector {
    /// Creates an exact selector.
    #[must_use]
    pub fn exact(key: Vec<u8>) -> Self {
        Self::Exact(key)
    }

    /// Creates a validated half-open selector.
    ///
    /// # Errors
    ///
    /// Returns [`SemanticError::InvalidSelector`] when the end does not sort
    /// after the start.
    pub fn range(start: Vec<u8>, end: Vec<u8>) -> Result<Self, SemanticError> {
        if start >= end {
            Err(SemanticError::InvalidSelector)
        } else {
            Ok(Self::Range(ReadRange { start, end }))
        }
    }

    /// Creates a prefix selector. An empty prefix intentionally means the
    /// complete key space of the bound scope.
    #[must_use]
    pub fn prefix(prefix: Vec<u8>) -> Self {
        Self::Prefix(prefix)
    }

    fn contains(&self, key: &[u8]) -> bool {
        match self {
            Self::Exact(expected) => expected.as_slice() == key,
            Self::Range(range) => range.start() <= key && key < range.end(),
            Self::Prefix(prefix) => key.starts_with(prefix),
        }
    }

    fn intersects(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Exact(left), right) => right.contains(left),
            (left, Self::Exact(right)) => left.contains(right),
            (Self::Range(left), Self::Range(right)) => {
                left.start() < right.end() && right.start() < left.end()
            }
            (Self::Prefix(left), Self::Prefix(right)) => {
                left.starts_with(right) || right.starts_with(left)
            }
            (Self::Prefix(prefix), Self::Range(range))
            | (Self::Range(range), Self::Prefix(prefix)) => {
                prefix.as_slice() < range.end()
                    && key_is_below_prefix_successor(range.start(), prefix)
            }
        }
    }

    /// Returns the interval representation used by manifest admission. The
    /// boolean distinguishes an exact point from a half-open range; exact
    /// points must not be widened to include keys with that point as a
    /// prefix.
    pub(crate) fn interval(&self) -> (Vec<u8>, Option<Vec<u8>>, bool) {
        match self {
            Self::Exact(key) => (key.clone(), None, true),
            Self::Range(range) => (range.start.clone(), Some(range.end.clone()), false),
            Self::Prefix(prefix) => (prefix.clone(), prefix_successor(prefix), false),
        }
    }
}

fn prefix_successor(prefix: &[u8]) -> Option<Vec<u8>> {
    let mut successor = prefix.to_vec();
    while let Some(last) = successor.last_mut() {
        if *last == u8::MAX {
            successor.pop();
        } else {
            *last += 1;
            return Some(successor);
        }
    }
    None
}

/// Compares a key with the lexicographic upper bound of a prefix without
/// materialising that bound.  Prefix/range invalidation is a read hot path;
/// allocating a temporary successor for every pair made a large manifest
/// surprisingly expensive.
fn key_is_below_prefix_successor(key: &[u8], prefix: &[u8]) -> bool {
    let Some(last) = prefix.iter().rposition(|byte| *byte != u8::MAX) else {
        // An all-0xff prefix has no finite successor and therefore extends to
        // the end of the canonical key space.
        return true;
    };
    for (left, right) in key.iter().zip(prefix.iter()).take(last) {
        if left != right {
            return left < right;
        }
    }
    match key.get(last) {
        None => true,
        Some(byte) => *byte <= prefix[last],
    }
}

/// A dependency read retaining the exact scope root and key selector.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScopedRead {
    facet: FacetKind,
    scope: ScopeRoot,
    selector: ReadSelector,
    negative: bool,
}

impl Ord for ScopedRead {
    fn cmp(&self, other: &Self) -> Ordering {
        self.facet
            .cmp(&other.facet)
            .then_with(|| self.scope.as_bytes().cmp(other.scope.as_bytes()))
            .then_with(|| self.selector.cmp(&other.selector))
            .then_with(|| self.negative.cmp(&other.negative))
    }
}

impl PartialOrd for ScopedRead {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Hash for ScopedRead {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.facet.hash(state);
        self.scope.as_bytes().hash(state);
        self.negative.hash(state);
        self.selector.hash(state);
    }
}

impl ScopedRead {
    /// Creates an exact positive read.
    #[must_use]
    pub fn exact(facet: FacetKind, scope: ScopeRoot, key: Vec<u8>) -> Self {
        Self {
            facet,
            scope,
            selector: ReadSelector::exact(key),
            negative: false,
        }
    }

    /// Creates an exact negative read.
    #[must_use]
    pub fn negative(facet: FacetKind, scope: ScopeRoot, key: Vec<u8>) -> Self {
        Self {
            negative: true,
            ..Self::exact(facet, scope, key)
        }
    }

    /// Creates a positive half-open range read.
    ///
    /// # Errors
    ///
    /// Returns [`SemanticError::InvalidSelector`] when `end` does not sort
    /// after `start`.
    pub fn range(
        facet: FacetKind,
        scope: ScopeRoot,
        start: Vec<u8>,
        end: Vec<u8>,
    ) -> Result<Self, SemanticError> {
        Ok(Self {
            facet,
            scope,
            selector: ReadSelector::range(start, end)?,
            negative: false,
        })
    }

    /// Creates a negative half-open range read.
    ///
    /// # Errors
    ///
    /// Returns [`SemanticError::InvalidSelector`] when `end` does not sort
    /// after `start`.
    pub fn negative_range(
        facet: FacetKind,
        scope: ScopeRoot,
        start: Vec<u8>,
        end: Vec<u8>,
    ) -> Result<Self, SemanticError> {
        Ok(Self {
            negative: true,
            ..Self::range(facet, scope, start, end)?
        })
    }

    /// Creates a positive prefix read.
    #[must_use]
    pub fn prefix(facet: FacetKind, scope: ScopeRoot, prefix: Vec<u8>) -> Self {
        Self {
            facet,
            scope,
            selector: ReadSelector::prefix(prefix),
            negative: false,
        }
    }

    /// Creates a negative prefix read.
    #[must_use]
    pub fn negative_prefix(facet: FacetKind, scope: ScopeRoot, prefix: Vec<u8>) -> Self {
        Self {
            negative: true,
            ..Self::prefix(facet, scope, prefix)
        }
    }

    /// Returns whether a changed selector intersects this read.
    #[must_use]
    pub fn intersects(&self, changed: &Self) -> bool {
        self.facet == changed.facet
            && self.scope == changed.scope
            && self.selector.intersects(&changed.selector)
    }

    /// Returns whether two reads name the same relation/scope region and have
    /// opposite polarity. Such assertions cannot coexist in one exact
    /// dependency manifest because one claims a value while the other claims
    /// its absence over an overlapping key region.
    #[must_use]
    pub fn polarity_conflicts_with(&self, other: &Self) -> bool {
        self.facet == other.facet
            && self.scope == other.scope
            && self.negative != other.negative
            && self.selector.intersects(&other.selector)
    }

    /// Returns the deterministic selector encoding.
    #[must_use]
    pub fn canonical_bytes(&self) -> Vec<u8> {
        canonical_scoped_read(self)
    }

    /// Returns the facet/relation being read.
    #[must_use]
    pub const fn facet(&self) -> FacetKind {
        self.facet
    }

    /// Returns the exact authority scope root.
    #[must_use]
    pub const fn scope_root(&self) -> ScopeRoot {
        self.scope
    }

    /// Returns the selected key, range, or prefix.
    #[must_use]
    pub const fn selector(&self) -> &ReadSelector {
        &self.selector
    }

    /// Returns whether this read asserts absence.
    #[must_use]
    pub const fn is_negative(&self) -> bool {
        self.negative
    }
}
