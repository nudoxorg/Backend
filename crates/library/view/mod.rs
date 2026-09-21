//! Immutable product views, query records, and checked view transitions.
//!
//! A [`ViewRoot`] is a published snapshot, never a mutable bag of rows. The
//! recipe key is stable across versions; each complete root derives its own
//! [`ViewVersion`] from its exact basis, frontier, coverage, and row relation.
//! A transition is prepared against one exact root and consumes that
//! preparation before a [`CommittedViewDelta`] can be observed.

mod descriptor;
mod model;
mod page;
mod proof;
mod root;
mod transition;

pub(crate) use descriptor::DescriptorParts;
pub use descriptor::{ViewRootDescriptor, ViewRootDescriptorClaim};
pub use model::{
    Basis, Coverage, CoverageCapability, Document, Fragment, Freshness, GraphRelation, Lane,
    MAX_COVERAGE_EVIDENCE, MAX_ROW_IDENTITY_PREIMAGE_BYTES, NameRecord, Outline, OutlineExtent,
    OutlineNode, Reason, Row, RowId, RowIdentityPreimage, RowIdentityPreimageError, RowState,
    SourceAvailability,
};
pub use page::{ViewPageCursor, ViewPageError, ViewSnapshotPage};
pub use proof::{CompleteViewProjection, ViewProjection, ViewProjectionError};
pub use root::{MAX_SNAPSHOT_PAGE_ROWS, ViewRoot};
pub use transition::{
    CommittedViewDelta, MAX_VIEW_PATCH_ROWS, PreparedViewDelta, RowChange, ViewDelta, ViewError,
    ViewSnapshot,
};
