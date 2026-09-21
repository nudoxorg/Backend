//! Strict, versioned transport contracts.
//!
//! The wire grammar is split by responsibility so certificates, commands,
//! replies, and events can evolve independently while retaining one public
//! DTO surface.

mod admission;
mod claims;
mod codec;
mod command;
mod event;
mod event_dto;
mod reply;
mod reply_admission;
mod reply_capability;
mod reply_content;
mod reply_coverage;
mod reply_failure;
mod reply_graph_query;
mod reply_page;
mod subscription;
mod subscription_snapshot;
#[cfg(test)]
mod tests;

pub use admission::{
    MAX_COMMAND_TEXT, ReplyAdmissionError, RequestAdmissionError, admit_reply,
    admit_reply_with_capability, admit_request, reply_memory_bound,
};
pub use claims::{WireCertificate, WireClaim, WireSchema};
pub use codec::{
    MAX_COMMAND_BODY, command_request_id, decode_command_body, decode_command_body_for_owner,
    decode_reply_body, decode_reply_body_with_verifier, encode_command_body,
};
pub use command::{CommandDto, ReplyDto, ViewDto};
pub use event_dto::EventDto;
pub use subscription::{SubscriptionDto, encode_compact_subscription};
pub use subscription_snapshot::{
    SnapshotHydrator, SnapshotPageClaim, SnapshotPageDto, encode_view_root_descriptor,
};

pub(crate) use command::{
    CursorWire, EmptyWire, FrontierWire, TextWire, cursor_from_wire,
    cursor_from_wire_with_capability, cursor_to_wire, ensure_version, frontier_from_wire,
    frontier_from_wire_with_capability, frontier_to_wire, required_certificate,
};
pub(crate) use event::{EventEnvelopeWire, ViewEnvelopeWire, decode_event_with_certificate};
pub use event::{decode_compact_view_event, encode_compact_view_event};
pub(crate) use reply::{
    BasisWire, DeltaWire, ReplyEnvelope, RowIdWire, RowWire, SnapshotWire, ViewRootWire,
    basis_from_wire, basis_from_wire_with_capability, basis_object, basis_to_wire, reply_from_wire,
    reply_from_wire_with_verifier, row_from_wire, row_from_wire_against,
    row_from_wire_against_with_capability, row_id_from_wire, row_id_to_wire, row_to_wire,
    snapshot_from_wire, snapshot_to_wire, view_root_from_wire, view_root_to_wire,
};
pub(crate) use reply_capability::{
    HealthWire, inventory_from_wire, inventory_to_wire, progress_from_wire, progress_to_wire,
};
pub(crate) use reply_coverage::{
    CoverageWire, FreshnessWire, coverage_from_wire, coverage_to_wire, freshness_from_wire,
    freshness_to_wire,
};

/// Current transport DTO version.
pub const DTO_VERSION: u16 = 7;
