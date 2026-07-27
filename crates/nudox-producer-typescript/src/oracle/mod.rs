//! Tier-2 oracle seam.
//!
//! The only sub-module today is [`tsz`], gated behind the `tsz` feature.
//! Without the feature the module tree is empty and the feature guard is the
//! sole compile-time artefact.

#[cfg(feature = "tsz")]
pub mod tsz;
