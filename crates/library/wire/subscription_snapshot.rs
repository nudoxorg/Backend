//! One typed wire projection for bounded local subscriptions.
//!
//! The local control envelope is transport specific, but the successful
//! payload inside it is deliberately owned here.  CLI, MCP, desktop, and the
//! local daemon therefore share one decoder and one cursor/event admission
//! state machine instead of maintaining subtly different JSON parsers.

use super::subscription::{cursor_from_wire_against, reset_reason, reset_reason_name, same_stream};
use super::{
    BasisWire, CoverageWire, CursorWire, DTO_VERSION, RowIdWire, RowWire, WireCertificate,
    WireClaim, basis_from_wire, basis_from_wire_with_capability, basis_object, basis_to_wire,
    coverage_from_wire, coverage_to_wire, cursor_to_wire, ensure_version, row_from_wire,
    row_from_wire_against_with_capability, row_id_from_wire, row_id_to_wire,
};
use crate::{
    Cursor, CursorResetReason, ViewRootDescriptor, ViewRootDescriptorClaim, ViewSnapshotPage,
};

use super::reply_admission::{CapabilityAdmission, CoverageAdmission, VerifierAdmission};

mod codec;
mod hydration;

pub use codec::encode_view_root_descriptor;
pub use hydration::SnapshotHydrator;

/// A bounded reset page carrying a constant-size root descriptor.
///
/// This DTO is intentionally separate from [`crate::SubscriptionDto::Reset`]. The
/// legacy reset remains useful for tiny compatibility replies, while this
/// form lets a daemon send O(1) reset metadata followed by O(page) payloads.
/// The producer certificate is attached to each page and covers its rows;
/// the final hydrated row set is admitted against the descriptor's root and
/// coverage before becoming a [`ViewRoot`](crate::ViewRoot).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SnapshotPageDto {
    cursor: Cursor,
    descriptor: ViewRootDescriptor,
    page: ViewSnapshotPage,
    reason: CursorResetReason,
    certificate: Option<WireCertificate>,
}

/// A bounded page admitted from a producer certificate while its visible root
/// remains deferred.
///
/// A reset often targets a root that the client has never seen.  Requiring an
/// already typed [`ViewRootDescriptor`] in that case would force a full query
/// before hydration and reintroduce the O(view) path this DTO is designed to
/// remove.  This claim therefore carries typed recipe/version/source values,
/// but keeps the visible root as a fixed-width commitment until
/// [`SnapshotHydrator::finish`] recomputes it from all pages.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SnapshotPageClaim {
    cursor_sequence: u64,
    descriptor: ViewRootDescriptorClaim,
    after: Option<crate::RowId>,
    rows: Box<[crate::Row]>,
    next_after: Option<crate::RowId>,
    reason: CursorResetReason,
    certificate: WireCertificate,
}

impl SnapshotPageClaim {
    /// Returns the deferred descriptor admitted from the producer certificate.
    #[must_use]
    pub const fn descriptor(&self) -> &ViewRootDescriptorClaim {
        &self.descriptor
    }

    /// Returns the exact owner sequence paired with the reset descriptor.
    #[must_use]
    pub const fn cursor_sequence(&self) -> u64 {
        self.cursor_sequence
    }

    /// Returns the row anchor used for this page, if any.
    #[must_use]
    pub const fn after(&self) -> Option<crate::RowId> {
        self.after
    }

    /// Returns the bounded authenticated rows in this page.
    #[must_use]
    pub fn rows(&self) -> &[crate::Row] {
        &self.rows
    }

    /// Returns the next row anchor, if another page is required.
    #[must_use]
    pub const fn next_after(&self) -> Option<crate::RowId> {
        self.next_after
    }

    /// Returns why the producer requested a reset.
    #[must_use]
    pub const fn reason(&self) -> CursorResetReason {
        self.reason
    }

    /// Returns the producer certificate retained for this bounded page.
    #[must_use]
    pub const fn certificate(&self) -> &WireCertificate {
        &self.certificate
    }
}
