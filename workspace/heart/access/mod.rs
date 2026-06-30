//! Cross-cutting access + multi-source abstraction over every external provider
//! (terminus / qdrant / tantivy / postgres).
//!
//! Enterprises are expected to self-host these providers on their own
//! infrastructure, so every provider is abstracted over personal/private
//! ownership and supports *multiple sources*. That federation is exactly what
//! makes access control uniform — for enterprises and individuals alike.
//!
//! IMPLEMENT HERE: the federation + permission layer that wraps each concrete
//! backend before the runtime/registry ever touch it.

pub mod control;
pub mod source;
pub mod tenant;
pub mod visibility;
