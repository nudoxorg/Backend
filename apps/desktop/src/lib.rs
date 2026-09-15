//! The Nudox desktop surface: a local-first reader for compiled code.
//! It owns no durable state; the local service is the authority and this
//! process holds one client session and one certified bounded subscription.
//!
//! The crate is layered so that everything except the views compiles and is
//! tested without a window. `transport` and `reducer` admit the service's
//! immutable view roots, `presentation` turns those values into the shapes a
//! reader needs, `theme` and `motion` hold every colour and duration in the
//! application, `ui` builds stateless elements from them, and `views` is the
//! only layer that owns a window.

#![deny(unsafe_code)]

#[cfg(any(unix, windows))]
mod host;
mod motion;
mod presentation;
mod reducer;
mod store;
mod theme;
mod transport;
#[cfg(any(unix, windows))]
mod ui;
#[cfg(any(unix, windows))]
mod views;

#[cfg(all(any(unix, windows), feature = "preview"))]
mod preview;

#[cfg(test)]
mod tests;

pub use backend_library::{
    CompleteViewProjection, CoverageCapability, Cursor, CursorEvent, CursorRead, CursorResetReason,
    CursorSub, EventDto, SnapshotHydrator, SnapshotPageClaim, SnapshotPageDto, SubscriptionDto,
    ViewDto, ViewPageCursor, ViewPageError, ViewProjection, ViewProjectionError,
    ViewRootDescriptor, ViewRootDescriptorClaim, ViewSnapshotPage, WireCertificate, WireClaim,
    WireSchema,
};
pub use backend_replication::{
    LocalSubscriptionId, LocalSubscriptionOperation, LocalSubscriptionRequest,
    LocalSubscriptionResponse,
};
#[cfg(any(unix, windows))]
pub use host::lease::{DesktopHost, HostError, HostMode};
#[cfg(any(unix, windows))]
pub use host::launch::main_entry;
pub use reducer::model::{Model, poll};
pub use transport::error::ClientError;
pub use transport::limits::{ENDPOINT_ENV, MAX_ENDPOINT_PATH, MAX_EVENTS, MAX_FRAME, MAX_TEXT};
pub use transport::subscription::{
    CertifiedSubscriptionTransport, LocalEngine, SubscriptionRequest, SubscriptionTransport,
    snapshot_page_from_bytes, snapshot_page_from_value,
};
#[cfg(any(unix, windows))]
pub use transport::diff::diff_endpoint;
#[cfg(any(unix, windows))]
pub use transport::search::search_endpoint;
#[cfg(any(unix, windows))]
pub use transport::unix::UnixSubscriptionTransport;
