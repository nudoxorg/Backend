//! Reusable source/document viewport state and bounded virtualization.

use crate::core::ids::RowId;
use std::ops::Range;
use std::sync::Arc;

/// Scroll and anchor state shared by source and document collections.
#[derive(Clone, Debug, PartialEq)]
pub struct ViewportState {
    /// Stable viewport identity.
    pub id: ViewportId,
    /// Scroll offset in physical pixels.
    pub offset: f32,
    /// Height available to the collection.
    pub height: f32,
    /// First row kept as an anchor while rows above it remeasure.
    pub anchor: Option<RowId>,
    /// Whether a streaming document should follow its tail.
    pub follow_tail: bool,
}

/// The two reusable desktop collection kinds.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ViewportId {
    /// A source file viewport.
    Source,
    /// A document/reader viewport.
    Document,
}

impl ViewportState {
    /// Creates a viewport with a stable default anchor policy.
    #[must_use]
    pub const fn new(id: ViewportId) -> Self {
        Self {
            id,
            offset: 0.0,
            height: 0.0,
            anchor: None,
            follow_tail: false,
        }
    }

    /// Updates geometry without changing the user's anchor.
    pub fn set_geometry(&mut self, offset: f32, height: f32) {
        self.offset = offset.max(0.0);
        self.height = height.max(0.0);
    }

    /// Returns the visible row range for a bounded row-height table.
    #[must_use]
    pub fn visible_range(&self, heights: &[u32], overdraw: usize) -> Range<usize> {
        if heights.is_empty() || self.height <= 0.0 {
            return 0..0;
        }
        let mut start = 0usize;
        let mut y = 0.0_f32;
        for (index, height) in heights.iter().enumerate() {
            let next = y + *height as f32;
            if next > self.offset {
                start = index;
                break;
            }
            y = next;
            start = index.saturating_add(1);
        }
        let mut end = start;
        let bottom = self.offset + self.height;
        let mut cursor = y;
        for (index, height) in heights.iter().enumerate().skip(start) {
            cursor += *height as f32;
            end = index.saturating_add(1);
            if cursor >= bottom {
                break;
            }
        }
        start.saturating_sub(overdraw)..end.saturating_add(overdraw).min(heights.len())
    }
}

/// Stable-ID collection backing either source or document rows.
#[derive(Clone, Debug)]
pub struct VirtualCollection {
    rows: Arc<[RowId]>,
    heights: Arc<[u32]>,
}

impl VirtualCollection {
    /// Creates a collection whose rows can be reused across viewport kinds.
    #[must_use]
    pub fn new(rows: Arc<[RowId]>, heights: Arc<[u32]>) -> Self {
        assert_eq!(rows.len(), heights.len(), "row IDs and heights must align");
        Self { rows, heights }
    }

    /// Returns the stable row IDs.
    #[must_use]
    pub fn rows(&self) -> &[RowId] {
        &self.rows
    }

    /// Returns the row heights used for culling.
    #[must_use]
    pub fn heights(&self) -> &[u32] {
        &self.heights
    }

    /// Returns only rows that may be painted in the viewport.
    #[must_use]
    pub fn visible_rows(&self, viewport: &ViewportState, overdraw: usize) -> &[RowId] {
        let range = viewport.visible_range(&self.heights, overdraw);
        &self.rows[range]
    }

    /// Returns the full content height.
    #[must_use]
    pub fn content_height(&self) -> u32 {
        self.heights.iter().copied().sum()
    }
}

/// Viewport state for source files.
pub type SourceViewportState = ViewportState;
/// Viewport state for reader documents.
pub type DocumentViewportState = ViewportState;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn virtualization_returns_a_bounded_stable_id_slice() {
        let rows: Arc<[RowId]> = (0..100).map(RowId::test).collect::<Vec<_>>().into();
        let heights: Arc<[u32]> = vec![20; 100].into();
        let collection = VirtualCollection::new(rows, heights);
        let mut viewport = ViewportState::new(ViewportId::Document);
        viewport.set_geometry(400.0, 60.0);
        let visible = collection.visible_rows(&viewport, 1);
        assert_eq!(
            visible,
            &[
                RowId::test(19),
                RowId::test(20),
                RowId::test(21),
                RowId::test(22),
                RowId::test(23),
            ]
        );
    }
}
