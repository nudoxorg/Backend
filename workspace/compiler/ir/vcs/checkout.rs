//! Incremental materialize + partial-checkout by `IntroId`.
//!
//! ## Design
//!
//! ### `materialize_index` (full)
//! Outputs the entire channel into a fresh [`MemWc`] via
//! `output_repository_no_pending(..., prefix="")`, reads every `{hex}.nir`
//! file, wraps its bytes in an `Arc<[u8]>`, and records the current tip Merkle
//! via `txn.read().current_state(&channel.read()).to_bytes()`.
//!
//! ### `materialize_index_incremental`
//! Fast path: if `current_state().to_bytes() == prev.tip`, returns `prev.clone()`
//! with zero I/O.
//!
//! Slow path (output-and-compare):
//! 1. `output_repository_no_pending` (full, prefix `""`) into a fresh `MemWc`.
//! 2. Start from `prev.symbols.clone()` so untouched `Arc`s are reused.
//! 3. For each `{hex}.nir` file in the new WC:
//!    - If bytes are byte-equal to `prev`, keep the **same `Arc`** pointer
//!      (callers can verify with `Arc::ptr_eq`).
//!    - Otherwise allocate a new `Arc` and insert/replace.
//! 4. Call `symbols.retain(|intro, _| new_intros.contains(intro))` to remove
//!    deleted symbols.
//!
//! Note: `touched_files` + `find_oldest_path` was not used for the incremental
//! step because `find_oldest_path` returns `None` for deleted files (their
//! graph blocks are purged after the deletion change is applied), making it
//! impossible to determine which intro was deleted. The output-and-compare
//! approach handles all three cases (add, modify, delete) correctly.
//!
//! The log walk via `txn.read().reverse_log(&channel.read(), None)` is used
//! only for the fast-path tip comparison (`SerializedMerkle.to_bytes() == prev.tip`).
//!
//! ### `checkout_symbol` / `checkout_symbols`
//! Each call outputs only `symbol_path(intro)` via `output_repository_no_pending`
//! with that path as the prefix string, then reads the file from the temporary
//! `MemWc`.  A bare filename prefix selects exactly the matching flat root
//! file — no `output_file` needed.

use std::collections::HashMap;
use std::sync::Arc;

use ir::change::IntroId;

use crate::serialize::{intro_hex_of, is_symbol_path};

// ---------------------------------------------------------------------------
// MaterializedIndex
// ---------------------------------------------------------------------------

/// A snapshot of the channel's symbol files, keyed by [`IntroId`].
///
/// The `tip` field is the 32-byte compressed Merkle of the channel tip at the
/// time this index was built.  Two indexes with the same `tip` represent
/// identical channel states.
///
/// The `symbols` map holds raw file bytes wrapped in `Arc<[u8]>`.  For
/// incremental rebuilds the **same `Arc` instance** is reused for untouched
/// symbols, which callers can verify with [`Arc::ptr_eq`].
#[derive(Clone, Debug)]
pub struct MaterializedIndex {
    /// The channel tip at the time this index was materialized.
    pub tip: [u8; 32],
    /// Raw bytes of every live symbol file, keyed by intro.
    pub symbols: HashMap<IntroId, Arc<[u8]>>,
}

impl Default for MaterializedIndex {
    fn default() -> Self {
        Self::empty()
    }
}

impl MaterializedIndex {
    /// Create an empty index (tip = `[0; 32]`, no symbols).
    pub fn empty() -> Self {
        Self {
            tip: [0u8; 32],
            symbols: HashMap::new(),
        }
    }

    /// Look up the raw bytes for `intro`, if present.
    pub fn get(&self, intro: IntroId) -> Option<&Arc<[u8]>> {
        self.symbols.get(&intro)
    }

    /// Number of symbols in the index.
    pub fn len(&self) -> usize {
        self.symbols.len()
    }

    /// True if the index contains no symbols.
    pub fn is_empty(&self) -> bool {
        self.symbols.is_empty()
    }

    /// Iterate over all intro IDs present in the index.
    pub fn intros(&self) -> impl Iterator<Item = IntroId> + '_ {
        self.symbols.keys().copied()
    }

    /// A **borrowed** [`F1View`](crate::f1::F1View) over a symbol's
    /// bytes — the materialized graph as zero-owned borrowed views, not an owned
    /// `PayloadTable`. Returns `None` if the intro is absent, or `Some(Err)`
    /// if the stored bytes are malformed.
    pub fn view(&self, intro: IntroId) -> Option<Result<crate::f1::F1View<'_>, crate::f1::Error>> {
        self.symbols
            .get(&intro)
            .map(|arc| crate::f1::F1View::from_bytes(&arc[..]))
    }

    /// Iterate `(IntroId, F1View)` over every symbol, borrowed.
    pub fn views(
        &self,
    ) -> impl Iterator<Item = (IntroId, Result<crate::f1::F1View<'_>, crate::f1::Error>)> + '_ {
        self.symbols
            .iter()
            .map(|(intro, arc)| (*intro, crate::f1::F1View::from_bytes(&arc[..])))
    }
}

// ---------------------------------------------------------------------------
// Helper: parse a `{hex}.nir` path to an `IntroId` (returns None on failure)
// ---------------------------------------------------------------------------

pub(crate) fn try_intro_from_path(path: &str) -> Option<IntroId> {
    if !is_symbol_path(path) {
        return None;
    }
    let hex = intro_hex_of(path)?;
    if hex.len() != 64 {
        return None;
    }
    let mut bytes = [0u8; 32];
    for (i, chunk) in hex.as_bytes().chunks(2).enumerate() {
        let hi = (chunk[0] as char).to_digit(16)? as u8;
        let lo = (chunk[1] as char).to_digit(16)? as u8;
        bytes[i] = (hi << 4) | lo;
    }
    Some(IntroId::from_raw(bytes))
}

// ---------------------------------------------------------------------------
// impl methods live in repo.rs (this file provides the struct + helpers only)
// ---------------------------------------------------------------------------

// Methods `materialize_index`, `materialize_index_incremental`,
// `checkout_symbol`, `checkout_symbols` are added to
// `impl<C> IrRepository<C>` in `repo.rs`.

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

// Tests for materialize_index / checkout_symbol / checkout_symbols.

#[cfg(test)]
#[path = "checkout_tests.rs"]
mod tests;
