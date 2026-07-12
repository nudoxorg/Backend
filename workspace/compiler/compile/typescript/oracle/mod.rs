//! Tier C — external checker oracles for the TypeScript producer.
//!
//! The syntactic OXC pipeline ([`super::oxc`]) is always the floor. These
//! oracles are opt-in enrichment that exceed it using the one true checker
//! (Microsoft's, shipped as the `tsgo` static binary since TS 7.0 GA). Every
//! oracle falls back to the syntactic pass on failure — see [`tsgo`].

pub mod tsgo;
