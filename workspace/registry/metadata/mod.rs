//! Package metadata — the postgres-backed source of truth for everything
//! associated with a package: its canonical GUID and the hash of its
//! code/treesitter representation, plus the cross-store links.
//!
//! IMPLEMENT HERE: the metadata records + the postgres access that owns them.

pub mod guid;
pub mod hash;
