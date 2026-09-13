//! Cursor and subscription admission for one catalog snapshot.

use super::Library;
use crate::{Cursor, CursorSub, LibraryError};

impl Library {
    /// Returns a bounded subscription at the current immutable view version.
    #[must_use]
    pub fn subscribe(&self) -> CursorSub {
        CursorSub::for_view(&self.view, 256)
    }

    /// Returns a bounded subscription from a caller-owned observed cursor.
    ///
    /// # Errors
    ///
    /// Returns [`LibraryError::CursorMismatch`] when the cursor names another
    /// recipe, branch, log, schema, or a future sequence.
    pub fn subscribe_from(&self, cursor: Cursor) -> Result<CursorSub, LibraryError> {
        if cursor.recipe() != self.view.recipe
            || cursor.branch() != self.cursor.branch()
            || cursor.log() != self.cursor.log()
            || cursor.schema() != self.cursor.schema()
            || cursor.sequence() > self.cursor.sequence()
        {
            return Err(LibraryError::CursorMismatch);
        }
        Ok(CursorSub::from_cursor(cursor, 256))
    }
}
