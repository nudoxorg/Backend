//! The connective mesh: coordinating an ingest across subsystems.
//!
//! A package is parsed once into a single projection that fans out to every
//! sink (text index, vector store, graph store, blob/registry persistence),
//! reporting progress and reconciling the resulting events back to the global
//! index.
//!
//! The three client → server flows live here:
//! - [`initialization`]: ensure a library is present (index it or request it),
//!   tracking usage / tiers / freshness.
//! - [`indexing`]: run the compiler, update postgres, generate blob info.
//! - [`search`]: answer search/read requests.
//! - [`health`]: track parse status + coordinate load balancing.

pub mod health;
pub mod indexing;
pub mod initialization;
pub mod search;
