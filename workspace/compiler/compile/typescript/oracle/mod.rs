//! Tier C — in-process checker oracle for the TypeScript producer.
//!
//! The syntactic OXC pipeline ([`super::oxc`]) is always the floor. This oracle
//! is opt-in enrichment that exceeds it using the `tsz` TypeScript compiler
//! (git-vendored, pure Rust) as an in-process checker.
//!
//! Rather than emitting `.d.ts` and re-parsing, the oracle now queries the tsz
//! **checker** directly (node/symbol → `TypeId` → structured `TypeData`) and
//! splices the recovered types into the syntactic IR. This unifies inferred-
//! type recovery (Mode 1) and cross-module type resolution (Mode 2) into a
//! single enrichment pass over the OXC-produced [`ir::entry::Index`]. It falls
//! back to the syntactic pass on any failure — see [`tsz`].

pub mod tsz;
pub mod tsz_types;
