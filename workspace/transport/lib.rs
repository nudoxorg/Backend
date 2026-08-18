//! Shared iroh/bao content-transfer plane: opaque blob fetch + verified bao ranges.

pub mod announce;
pub mod bao;
pub mod blob;
pub mod endpoint;
pub mod frame;

pub use announce::{
    ANNOUNCE_ALPN, Ack as AnnounceAck, Announcement, send_announcement, serve_announcements,
};
pub use bao::{BaoError, Outboard};
pub use blob::{BLOBS_ALPN, Fetcher, Provider, TransportHash};
pub use endpoint::{AddressLookup, EndpointId, SecretKey, bind_endpoint};
pub use frame::{
    DEFAULT_FRAME_CAP_BYTES, TERMINAL_DRAIN, finish_and_drain, recv_framed, send_framed,
};

// Re-export the seam vocabulary so implementors can name one crate.
pub use heart::sync::{ApplyHook, ContentIo, SyncError, VerifyError};
