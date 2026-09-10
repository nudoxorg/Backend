//! Defines request behavior for `interface-search`, whose purpose is to define one honest multi-lane search vocabulary and its ranking over every retrieval backend.
//! This module owns the request invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Bounded query text, scope, limits, and cursors accepted from every surface.

use compiler_ir_vocabulary::EntityKind;
use interface_identity::PackageCoordinate;

use crate::LaneSet;

/// Longest query any surface forwards.
pub const MAX_QUERY_BYTES: usize = 512;
/// Largest page any surface may request.
pub const MAX_RESULT_LIMIT: u16 = 200;
/// Page size when a surface does not choose one.
pub const DEFAULT_RESULT_LIMIT: u16 = 25;

/// Non-empty, trimmed, bounded query text.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct QueryText(Box<str>);

impl QueryText {
    /// Admits query text.
    ///
    /// # Errors
    ///
    /// Rejects empty (after trimming) or oversized text.
    pub fn new(text: &str) -> Result<Self, QueryTextError> {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return Err(QueryTextError::Empty);
        }
        if trimmed.len() > MAX_QUERY_BYTES {
            return Err(QueryTextError::TooLong {
                observed: trimmed.len(),
                maximum: MAX_QUERY_BYTES,
            });
        }
        Ok(Self(trimmed.into()))
    }

    /// Exact query text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Exact query admission failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum QueryTextError {
    /// Nothing but whitespace was supplied.
    Empty,
    /// The text exceeds the fixed budget.
    TooLong {
        /// Observed bytes.
        observed: usize,
        /// Accepted bytes.
        maximum: usize,
    },
}

/// Set of declaration kinds, as a bit set over the closed vocabulary.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct KindSet(u16);

/// Bit position of one kind inside a [`KindSet`].
///
/// Spelled as an explicit match rather than a discriminant cast so the set's wire shape is a
/// decision this module made, not a side effect of the vocabulary's declaration order.
const fn kind_bit(kind: EntityKind) -> u16 {
    1 << match kind {
        EntityKind::Function => 0_u16,
        EntityKind::Constant => 1,
        EntityKind::Record => 2,
        EntityKind::Module => 3,
        EntityKind::Field => 4,
        EntityKind::Alias => 5,
        EntityKind::Trait => 6,
        EntityKind::Implementation => 7,
        EntityKind::Enum => 8,
        EntityKind::Variant => 9,
        EntityKind::Static => 10,
        EntityKind::Reexport => 11,
        EntityKind::Parameter => 12,
        EntityKind::Macro => 13,
        EntityKind::Namespace => 14,
    }
}

impl KindSet {
    /// Every kind.
    pub const ALL: Self = Self((1 << 15) - 1);
    /// The kinds a reader browses for: everything except fields, variants, and parameters.
    pub const BROWSABLE: Self = Self(
        Self::ALL.0
            & !kind_bit(EntityKind::Field)
            & !kind_bit(EntityKind::Variant)
            & !kind_bit(EntityKind::Parameter),
    );
    /// No kinds.
    pub const EMPTY: Self = Self(0);

    /// Adds one kind.
    #[must_use]
    pub const fn with(self, kind: EntityKind) -> Self {
        Self(self.0 | kind_bit(kind))
    }

    /// Removes one kind.
    #[must_use]
    pub const fn without(self, kind: EntityKind) -> Self {
        Self(self.0 & !kind_bit(kind))
    }

    /// Whether the set contains a kind.
    #[must_use]
    pub const fn contains(self, kind: EntityKind) -> bool {
        self.0 & kind_bit(kind) != 0
    }

    /// Kinds excluded relative to [`KindSet::ALL`], in canonical order.
    pub fn excluded(self) -> impl Iterator<Item = EntityKind> {
        EntityKind::ALL
            .into_iter()
            .filter(move |kind| !self.contains(*kind))
    }

    /// Whether no kind is selected.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }
}

/// Which packages and kinds a request may match.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SearchScope {
    /// Restrict to these packages; `None` means every loaded package.
    pub packages: Option<Box<[PackageCoordinate]>>,
    /// Kinds admitted into the result.
    pub kinds: KindSet,
}

impl Default for SearchScope {
    fn default() -> Self {
        Self {
            packages: None,
            kinds: KindSet::BROWSABLE,
        }
    }
}

/// Bounded page size.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ResultLimit(u16);

impl ResultLimit {
    /// Clamps a requested size into the accepted range; zero becomes the default.
    #[must_use]
    pub const fn clamped(requested: u16) -> Self {
        if requested == 0 {
            Self(DEFAULT_RESULT_LIMIT)
        } else if requested > MAX_RESULT_LIMIT {
            Self(MAX_RESULT_LIMIT)
        } else {
            Self(requested)
        }
    }

    /// Accepted size.
    #[must_use]
    pub const fn get(self) -> u16 {
        self.0
    }
}

impl Default for ResultLimit {
    fn default() -> Self {
        Self(DEFAULT_RESULT_LIMIT)
    }
}

/// Opaque continuation: the merged-rank offset of the next page.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Cursor(pub u32);

/// One search request from any surface.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SearchRequest {
    /// Query text.
    pub text: QueryText,
    /// Package and kind scope.
    pub scope: SearchScope,
    /// Lanes the caller wants consulted.
    pub lanes: LaneSet,
    /// Page size.
    pub limit: ResultLimit,
    /// Continuation from a previous terminal.
    pub cursor: Option<Cursor>,
}

impl SearchRequest {
    /// The common case: every lane, browsable kinds, default page.
    #[must_use]
    pub fn simple(text: QueryText) -> Self {
        Self {
            text,
            scope: SearchScope::default(),
            lanes: LaneSet::ALL,
            limit: ResultLimit::default(),
            cursor: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_sets_and_limits_are_closed() {
        assert!(KindSet::BROWSABLE.contains(EntityKind::Function));
        assert!(!KindSet::BROWSABLE.contains(EntityKind::Field));
        let excluded: Vec<_> = KindSet::BROWSABLE.excluded().collect();
        assert_eq!(
            excluded,
            [EntityKind::Field, EntityKind::Variant, EntityKind::Parameter]
        );
        assert_eq!(ResultLimit::clamped(0).get(), DEFAULT_RESULT_LIMIT);
        assert_eq!(ResultLimit::clamped(9_999).get(), MAX_RESULT_LIMIT);
        assert_eq!(QueryText::new("   "), Err(QueryTextError::Empty));
    }
}
