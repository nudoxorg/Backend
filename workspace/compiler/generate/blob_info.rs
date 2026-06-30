//! Assembling per-symbol blob information from the three generated resolutions,
//! including the canonical hash of the code/treesitter representation that
//! postgres records as a package's identity.
//!
//! IMPLEMENT HERE: the projection that turns generated resolutions into the
//! records the registry/runtime sinks consume.
