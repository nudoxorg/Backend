//! Package-level content hashing.
//!
//! The content-addressing primitives ([`ContentHash`], [`ContentHasher`],
//! [`Freshness`]) now live in [`heart::content`]; this module re-exports them so
//! registry callers have one import path. The canonical generation stamp is
//! [`crate::BlobManifest::identity_bytes`], which supersedes the old
//! per-file-digest fold.

pub use heart::content::{ContentHash, ContentHasher, Freshness};
