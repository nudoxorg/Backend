//! The initialization flow.
//!
//! Ensure a library is usable before serving against it:
//! - if it needs indexing, kick off indexing (otherwise let it be indexed);
//! - track the usage of a library, coordinating tiers and pulling it down again
//!   when it is unfresh;
//! - update the package's status in postgres and load it into pg;
//! - ensure init — and if not yet ready, send the request to make it so;
//! - return responses once the library is ready.
//!
//! IMPLEMENT HERE: the ensure-initialized state machine.
