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

/// Product-neutral identities and read-model ports for the v3 desktop shell.
pub mod core;
/// One immutable snapshot, selectors, viewports, and persistence schema.
pub mod model;
/// Typed routes, intents, effects, actions, focus, and pure reduction.
pub mod navigation;
/// Background actor, stale-result coordinator, motion clock, and GPUI graph.
pub mod runtime;

mod graph;
#[cfg(unix)]
mod host;
mod motion;
mod presentation;
mod reducer;
mod store;
mod theme;
mod transport;
#[cfg(unix)]
mod ui;
#[cfg(unix)]
mod views;

#[cfg(all(unix, feature = "preview"))]
mod preview;
#[cfg(all(unix, feature = "visual-harness"))]
mod harness;
#[cfg(all(unix, feature = "visual-harness"))]
mod harness_readiness;

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
#[cfg(unix)]
pub use host::lease::{DesktopHost, HostError, HostMode};
#[cfg(unix)]
pub use host::launch::main_entry;
pub use reducer::model::{Model, poll};
pub use transport::error::ClientError;
pub use transport::limits::{ENDPOINT_ENV, MAX_ENDPOINT_PATH, MAX_EVENTS, MAX_FRAME, MAX_TEXT};
pub use transport::subscription::{
    CertifiedSubscriptionTransport, LocalEngine, SubscriptionRequest, SubscriptionTransport,
    snapshot_page_from_bytes, snapshot_page_from_value,
};
#[cfg(all(unix, feature = "visual-harness"))]
pub use harness::{
    LiveCapture, WorkspaceActionProbe, WorkspaceSemanticProbe, capture_live_workspace,
    capture_live_workspace_journey, capture_live_workspace_with_semantics,
    production_action_inventory,
};
#[cfg(unix)]
pub use transport::diff::diff_endpoint;
#[cfg(unix)]
pub use transport::search::search_endpoint;
#[cfg(unix)]
pub use transport::unix::UnixSubscriptionTransport;
#[cfg(all(unix, feature = "visual-harness"))]
pub use harness_readiness::{
    ReaderSurface, ReadinessError, ReadinessEvent, ReadinessOptions, ReadinessReport,
    ReadinessTimeoutCause, RevisionIdentity, SelectedIdentity, await_readiness,
};
