//! `index::transport` — the ONE shared iroh/bao content-transfer plane
//! (CONSOLIDATION-NOTES §8b/§8c), formerly a standalone crate, now a module
//! of the `index` package.
//!
//! `heart::sync` defines the *abstract* seam: [`heart::sync::ContentIo`] /
//! [`heart::sync::ApplyHook`] and the [`heart::sync::SyncError`] /
//! [`heart::sync::VerifyError`] vocabulary. heart names no transport. This
//! module is the *concrete* transport those `ContentIo` implementors wire over,
//! so the iroh/iroh-blobs/bao plumbing lives once instead of being copy-pasted
//! per plane.
//!
//! Two transfer shapes, both content-addressed:
//!
//! * **Opaque blob** ([`blob`]) — the IR-sync shape. Hand raw bytes to a
//!   [`blob::Provider`], announce the returned [`blob::TransportHash`], and pull
//!   the blob back with a [`blob::Fetcher`] (size-capped) for the caller's own
//!   `ContentIo::verify`. Backed by an iroh-blobs `MemStore`.
//! * **Verified range** ([`bao`]) — the object-pack shape. Generate a bao
//!   outboard over a blob, serve a self-verifying Bao slice of any byte range,
//!   and verify+extract the exact requested bytes on the receiver.
//!
//! Both shapes share the [`endpoint`] builder (iroh `presets::Minimal` + secret
//! key + optional in-process discovery) and the [`frame`] length-prefixed
//! postcard framing over a QUIC bi-stream.
//!
//! The trust model is unchanged: content-addressing is the only trust anchor —
//! bytes are genuine iff they verify against their id, never because a
//! particular peer served them. This module moves plumbing; it does not decide
//! trust. Verification stays with the caller's `ContentIo::verify` (opaque
//! path) or is intrinsic to the bao slice (range path).

pub mod announce;
pub mod bao;
pub mod blob;
pub mod endpoint;
pub mod frame;

pub use announce::{ANNOUNCE_ALPN, Announcement, Ack as AnnounceAck, send_announcement, serve_announcements};
pub use bao::{BaoError, Outboard};
pub use blob::{BLOBS_ALPN, Fetcher, Provider, TransportHash};
pub use endpoint::{AddressLookup, EndpointId, SecretKey, bind_endpoint};
pub use frame::{DEFAULT_FRAME_CAP_BYTES, recv_framed, send_framed};

// Re-export the seam vocabulary so implementors can name one crate.
pub use heart::sync::{ApplyHook, ContentIo, SyncError, VerifyError};
