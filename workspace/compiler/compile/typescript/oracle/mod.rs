//! Tier C — in-process checker oracle for the TypeScript producer.
//!
//! The syntactic OXC pipeline ([`super::oxc`]) is always the floor. This oracle
//! is opt-in enrichment that exceeds it using the `tsz` TypeScript compiler
//! (git-vendored, pure Rust) as an in-process checker: it type-checks the
//! package, emits inference-accurate `.d.ts` (checker-inferred return types,
//! object shapes, `Promise<T>`), and re-extracts richer IR from those
//! declarations. It falls back to the syntactic pass on any failure — see
//! [`tsz`].

pub mod tsz;
