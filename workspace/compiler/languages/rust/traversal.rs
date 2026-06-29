//! Version resolution over git.
//!
//! Walks newest to oldest across every ref tip and returns the first commit
//! whose manifest (`Cargo.toml`) declares the requested
//! version, then materializes that commit's tree into a workspace for parsing.
