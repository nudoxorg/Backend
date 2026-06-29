//! This provides our main interface for creating an index over all of the
//! parsed packages, and giving everything a global identification.
//!
//! Each item not only provides the mean to retrieve itself from the store, but
//! also it's status: Which stage it's in (if not complete), what dependents it
//! needs to be complete, and whether or not it's loaded into the runtime stores
//! and systems.
