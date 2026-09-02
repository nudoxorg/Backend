//! Groups the per-language semantic frontend crates of the canonical compiler.
//!
//! Each language frontend lives in its own crate under this one and owns the
//! typed fact extraction for that language; this grouping crate ships no
//! behavior of its own and exists so the workspace member globs always match
//! valid manifests.
