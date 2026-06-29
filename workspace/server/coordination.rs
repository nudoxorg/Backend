//! The connective mesh: coordinating an ingest across subsystems.
//!
//! A package is parsed once into a single projection that fans out to every
//! sink (text index, vector store, graph store, blob/registry persistence),
//! reporting progress and reconciling the resulting events back to the global
//! index.
