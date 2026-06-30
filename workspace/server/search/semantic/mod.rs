//! The semantic (qdrant) search surface — explicitly gated.
//!
//! This surface is only reachable when the caller has opted in (see
//! [`runtime::vector::gate`]); it is never the default and is never invoked
//! implicitly by the precise search path.
//!
//! IMPLEMENT HERE: the gated entry point that embeds a query and searches qdrant.
