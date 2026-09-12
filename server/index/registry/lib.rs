//! The `server-index-registry` crate exists to read live package registries into typed, cached package facts for every ecosystem.
//! Its public types are the complete boundary; implementation details remain private.
//! Callers compose capabilities through explicit authority, ownership, and failure values.
//!
//! This is the skeleton the registry builder fills: one adapter per ecosystem behind one trait,
//! a durable cache beneath the workspace root, and typed absence for every fact a registry cannot
//! supply. See `GUI2/DESIGN.md` §5 and §7.
