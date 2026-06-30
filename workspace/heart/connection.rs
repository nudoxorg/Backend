//! Connection typestate markers shared by every backing store.
//!
//! A store handle is either [`Cold`] (configured, but not yet verified to be
//! reachable/migrated) or [`Live`] (connected and ready). Query methods live
//! only on the `Live` form, and the `Server` is assemblable only from `Live`
//! stores — so "used a store before it was connected" is a compile error.

/// A configured-but-unverified store handle.
pub struct Cold;

/// A connected, ready store handle.
pub struct Live;
