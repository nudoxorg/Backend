use super::SnapshotPageClaim;
use crate::{Cursor, CursorRead, CursorResetReason, ViewRootDescriptorClaim};

/// Incremental reset hydrator shared by locald clients.
///
/// Each page is checked against one immutable descriptor and one exact row
/// anchor.  The accumulator rejects duplicate, skipped, reordered, or
/// over-counted pages before appending them.  Memory therefore grows with the
/// rows the caller explicitly chose to hydrate, while every transport frame
/// remains bounded by [`crate::MAX_SNAPSHOT_PAGE_ROWS`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SnapshotHydrator {
    previous: Cursor,
    cursor_sequence: u64,
    descriptor: ViewRootDescriptorClaim,
    next_after: Option<crate::RowId>,
    rows: Vec<crate::Row>,
    reason: CursorResetReason,
}

impl SnapshotHydrator {
    /// Starts hydration from the first producer-admitted page.
    /// # Errors
    ///
    /// Returns an error when the encoded identity or checked state is invalid.
    pub fn start(previous: Cursor, page: SnapshotPageClaim) -> Result<Self, String> {
        if page.after.is_some() {
            return Err("snapshot hydration must begin at the first page".to_owned());
        }
        let bootstrap = previous == Cursor::new();
        if !bootstrap
            && (page.cursor_sequence <= previous.sequence()
                || previous.recipe() != page.descriptor.recipe()
                || previous.branch() != page.descriptor.frontier().branch
                || previous.log() != page.descriptor.frontier().log
                || previous.schema() != page.descriptor.frontier().schema)
        {
            return Err("snapshot hydration cursor stream mismatch".to_owned());
        }
        let descriptor = page.descriptor.clone();
        let mut hydrator = Self {
            previous,
            cursor_sequence: page.cursor_sequence,
            descriptor,
            next_after: None,
            rows: Vec::new(),
            reason: page.reason,
        };
        hydrator.push_page(page)?;
        Ok(hydrator)
    }

    /// Appends the next page after validating its descriptor and anchor.
    /// # Errors
    ///
    /// Returns an error when the encoded identity or checked state is invalid.
    pub fn push_page(&mut self, page: SnapshotPageClaim) -> Result<(), String> {
        if page.cursor_sequence != self.cursor_sequence
            || page.descriptor != self.descriptor
            || page.after != self.next_after
            || page.reason != self.reason
        {
            return Err("snapshot hydration page does not continue its predecessor".to_owned());
        }
        let incoming = page.rows.len();
        let expected_total = usize::try_from(self.descriptor.row_count())
            .map_err(|_| "snapshot row count is not representable".to_owned())?;
        let next_len = self
            .rows
            .len()
            .checked_add(incoming)
            .ok_or_else(|| "snapshot hydration row count overflow".to_owned())?;
        if next_len > expected_total {
            return Err("snapshot hydration exceeds its descriptor row count".to_owned());
        }
        if let (Some(after), Some(first)) = (self.rows.last().map(|row| row.id), page.rows.first())
            && first.id <= after
        {
            return Err("snapshot hydration rows are not strictly ordered".to_owned());
        }
        if page.next_after.is_some() && page.rows.is_empty() {
            return Err("snapshot continuation cannot follow an empty page".to_owned());
        }
        self.rows.extend(page.rows.into_vec());
        self.next_after = page.next_after;
        Ok(())
    }

    /// Returns whether all descriptor rows have been received.
    #[must_use]
    pub const fn is_complete(&self) -> bool {
        self.next_after.is_none()
    }

    /// Recomputes the relation root and returns the checked replacement read.
    ///
    /// A missing page, forged row, wrong root commitment, or mismatched view
    /// version fails here before the reducer can publish the replacement.
    /// # Errors
    ///
    /// Returns an error when the encoded identity or checked state is invalid.
    pub fn finish(self) -> Result<CursorRead, String> {
        if self.next_after.is_some() || self.rows.len() as u64 != self.descriptor.row_count() {
            return Err("snapshot hydration is incomplete".to_owned());
        }
        let root = self
            .descriptor
            .clone()
            .admit_rows(self.rows)
            .map_err(|error| format!("snapshot root admission failed: {error:?}"))?;
        let cursor = Cursor::for_view_root_at(&root, self.cursor_sequence);
        Ok(CursorRead::Reset {
            cursor,
            root: Box::new(root),
            reason: self.reason,
        })
    }

    /// Returns the current opaque row continuation for the transport adapter.
    #[must_use]
    pub const fn next_after_anchor(&self) -> Option<crate::RowId> {
        self.next_after
    }

    /// Returns the cursor that the owner supplied before the reset.
    #[must_use]
    pub const fn previous(&self) -> Cursor {
        self.previous
    }
}
