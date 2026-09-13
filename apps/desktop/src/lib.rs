//! Bounded desktop subscription adapter over immutable library view roots.
//!
//! The desktop keeps only the currently rendered root and a cursor. It never
//! becomes an authoritative cache: every event is admitted against the exact
//! prior root, and a gap or malformed transition requires a complete reset
//! supplied by the daemon. The host GUI owns process startup and composes
//! [`Model::try_new`] with [`Model::poll_transport`]; this crate stays a
//! transport/reducer library and does not create a second UI process.

mod error;
#[cfg(unix)]
mod host;
mod model;
#[cfg(unix)]
mod native;
#[cfg(unix)]
mod service_diff;
#[cfg(unix)]
mod service_search;
mod subscription;
mod transport;

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
pub use error::ClientError;
#[cfg(unix)]
pub use host::{DesktopHost, HostMode};
pub use model::{Model, poll};
#[cfg(unix)]
pub use service_diff::diff_endpoint;
#[cfg(unix)]
pub use service_search::search_endpoint;
pub use subscription::{
    CertifiedSubscriptionTransport, LocalEngine, SubscriptionRequest, SubscriptionTransport,
    snapshot_page_from_bytes, snapshot_page_from_value,
};
#[cfg(unix)]
pub use transport::UnixSubscriptionTransport;

/// Maximum events accepted in one interactive subscription batch.
pub const MAX_EVENTS: usize = backend_library::MAX_SUBSCRIPTION_EVENTS;
/// Maximum bytes in one subscription frame.
pub const MAX_FRAME: usize = backend_replication::LOCAL_CONTROL_MAX_FRAME;
/// Maximum bytes in a configured endpoint path.
pub const MAX_TEXT: usize = 8 * 1024;
/// Maximum path length accepted for a local Unix endpoint.
///
/// This matches the local daemon's configured path budget and keeps an
/// invalid path from reaching the platform socket API.
pub const MAX_ENDPOINT_PATH: usize = backend_replication::MAX_UNIX_ENDPOINT_PATH_BYTES;
/// Environment variable naming the local daemon Unix endpoint.
pub const ENDPOINT_ENV: &str = "BACKEND_LOCALD_ENDPOINT";

/// Starts the native desktop application and its local owner.
#[must_use]
pub fn main_entry() -> std::process::ExitCode {
    #[cfg(unix)]
    {
        native::run()
    }
    #[cfg(not(unix))]
    {
        eprintln!("backend-desktop: this build requires a supported local transport");
        std::process::ExitCode::from(69)
    }
}
