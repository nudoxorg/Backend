//! Bounded page cursors and snapshots for immutable view roots.

use super::{MAX_SNAPSHOT_PAGE_ROWS, Row, RowId, ViewRoot};
use crate::canonical::{ViewRecipeId, ViewStateRoot, ViewVersion};

/// Authenticated continuation for one bounded snapshot page.
///
/// The continuation is keyed by the last stable row identity instead of a
/// row offset.  A persistent relation can seek directly to that key, so every
/// page costs O(log n + page) and never scans or clones the preceding view.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ViewPageCursor {
    pub(super) recipe: ViewRecipeId,
    pub(super) version: ViewVersion,
    pub(super) root: ViewStateRoot,
    pub(super) after: Option<RowId>,
}

impl ViewPageCursor {
    /// Creates a page cursor from already admitted typed identities.
    ///
    /// This is the protocol decoder seam: the IDs must have been admitted by
    /// a producer certificate or an expected descriptor before this value is
    /// constructed.
    #[must_use]
    pub const fn from_parts(
        recipe: ViewRecipeId,
        version: ViewVersion,
        root: ViewStateRoot,
        after: Option<RowId>,
    ) -> Self {
        Self {
            recipe,
            version,
            root,
            after,
        }
    }

    /// Returns the first page cursor for one retained view.
    #[must_use]
    pub const fn first(root: &ViewRoot) -> Self {
        Self {
            recipe: root.recipe,
            version: root.version,
            root: root.root,
            after: None,
        }
    }

    /// Returns the continuation immediately after one row in a retained view.
    #[must_use]
    pub const fn from_after(root: &ViewRoot, row: RowId) -> Self {
        Self {
            recipe: root.recipe,
            version: root.version,
            root: root.root,
            after: Some(row),
        }
    }

    /// Returns the recipe bound to this continuation.
    #[must_use]
    pub const fn recipe(self) -> ViewRecipeId {
        self.recipe
    }

    /// Returns the visible version bound to this continuation.
    #[must_use]
    pub const fn version(self) -> ViewVersion {
        self.version
    }

    /// Returns the visible relation root bound to this continuation.
    #[must_use]
    pub const fn root(self) -> ViewStateRoot {
        self.root
    }

    /// Returns the last row emitted before this page, if any.
    #[must_use]
    pub const fn after(self) -> Option<RowId> {
        self.after
    }
}

/// One bounded page of rows from an immutable view root.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ViewSnapshotPage {
    pub(super) cursor: ViewPageCursor,
    pub(super) rows: Box<[Row]>,
    pub(super) next: Option<ViewPageCursor>,
    pub(super) total_rows: u64,
}

impl ViewSnapshotPage {
    /// Constructs a page after validating its bounded continuation metadata.
    ///
    /// Producers use [`ViewRoot::page`] in normal operation. This constructor
    /// is also the narrow admission seam used by protocol decoders after row
    /// identities and certificates have been checked.
    /// # Errors
    ///
    /// Returns an error when the encoded identity or checked state is invalid.
    pub fn from_parts(
        cursor: ViewPageCursor,
        rows: impl Into<Box<[Row]>>,
        next: Option<ViewPageCursor>,
        total_rows: u64,
    ) -> Result<Self, ViewPageError> {
        let rows = rows.into();
        if rows.len() > MAX_SNAPSHOT_PAGE_ROWS || rows.len() as u64 > total_rows {
            return Err(ViewPageError::Overflow);
        }
        if next.is_some_and(|next| {
            next.recipe != cursor.recipe
                || next.version != cursor.version
                || next.root != cursor.root
                || rows.is_empty()
                || next.after != rows.last().map(|row| row.id)
        }) {
            return Err(ViewPageError::CursorMismatch);
        }
        if let Some(after) = cursor.after
            && rows.first().is_some_and(|row| row.id <= after)
        {
            return Err(ViewPageError::CursorMismatch);
        }
        if rows.windows(2).any(|pair| pair[0].id >= pair[1].id) {
            return Err(ViewPageError::CursorMismatch);
        }
        Ok(Self {
            cursor,
            rows,
            next,
            total_rows,
        })
    }

    /// Returns the page cursor supplied to the producer.
    #[must_use]
    pub const fn cursor(&self) -> ViewPageCursor {
        self.cursor
    }

    /// Returns rows in canonical relation order.
    #[must_use]
    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    /// Returns the continuation for the next bounded page.
    #[must_use]
    pub const fn next(&self) -> Option<ViewPageCursor> {
        self.next
    }

    /// Returns the complete descriptor row count advertised by the producer.
    #[must_use]
    pub const fn total_rows(&self) -> u64 {
        self.total_rows
    }

    /// Returns whether this page completes the descriptor's row set.
    #[must_use]
    pub const fn is_last(&self) -> bool {
        self.next.is_none()
    }
}

/// Failure while seeking or admitting a bounded snapshot page.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ViewPageError {
    /// The page credit is zero or exceeds the shared page limit.
    InvalidCredit,
    /// The page cursor names another recipe, version, or relation root.
    CursorMismatch,
    /// A continuation points after a row that is absent from this immutable
    /// root, which would otherwise permit silently skipping rows.
    MissingAnchor,
    /// The canonical row count could not be represented in the page metadata.
    Overflow,
}
