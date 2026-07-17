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

use nudox_change::IntroId;

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

    /// A **borrowed** [`SymbolView`](crate::blob::SymbolView) over a symbol's
    /// bytes — the materialized graph as zero-owned borrowed views, not an owned
    /// `PristineIntroTable`. Returns `None` if the intro is absent, or `Some(Err)`
    /// if the stored bytes are malformed.
    pub fn view(&self, intro: IntroId) -> Option<Result<crate::blob::SymbolView<'_>, crate::blob::BlobError>> {
        self.symbols.get(&intro).map(|arc| crate::blob::SymbolView::from_bytes(&arc[..]))
    }

    /// Iterate `(IntroId, SymbolView)` over every symbol, borrowed.
    pub fn views(&self) -> impl Iterator<Item = (IntroId, Result<crate::blob::SymbolView<'_>, crate::blob::BlobError>)> + '_ {
        self.symbols
            .iter()
            .map(|(intro, arc)| (*intro, crate::blob::SymbolView::from_bytes(&arc[..])))
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
pub(crate) use try_intro_from_path as _try_intro_from_path;

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

// Tests for materialize_index / checkout_symbol / checkout_symbols.
#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use nudox_change::{EcosystemId, IntroId, PackageLineageId, PackageName};
    use nudox_ir::apply::PristineIntroTable;
    use nudox_ir::kind::KindDiscriminant;
    use nudox_ir::symbol::Visibility;
    use nudox_ir::wire::{
        EntryPayloadFlags, FunctionWire, KindWire, ModuleWire, OwnedEntryPayload, SymbolWire,
    };

    use crate::blob::SymbolView;
    use crate::repo::IrRepository;

    /// Decode a symbol blob's bytes back to its payload (for assertions).
    fn payload_of(bytes: &[u8]) -> nudox_ir::wire::OwnedEntryPayload {
        SymbolView::from_bytes(bytes).unwrap().to_owned_payload().unwrap()
    }

    fn pkg() -> PackageLineageId {
        PackageLineageId::new(EcosystemId::new("cargo"), PackageName::new("mylib"))
    }

    fn intro(n: u8) -> IntroId {
        IntroId::from_raw([n; 32])
    }

    fn sym(name: &str) -> SymbolWire {
        SymbolWire {
            name: name.to_owned(),
            visibility: Visibility::Public,
            documentation: None,
            source_path: "src/lib.rs".to_owned(),
            span_start: 0,
            span_end: name.len() as u32,
            aliases: Vec::new(),
            deprecation: None,
            doc_links: Vec::new(),
        }
    }

    fn function(name: &str) -> OwnedEntryPayload {
        OwnedEntryPayload::sealed(
            sym(name),
            KindDiscriminant::Function,
            KindWire::Function(FunctionWire {
                input_params: Box::new([]),
                output_params: Box::new([]),
            }),
            EntryPayloadFlags::default(),
        )
    }

    fn module(name: &str) -> OwnedEntryPayload {
        OwnedEntryPayload::sealed(
            sym(name),
            KindDiscriminant::Module,
            KindWire::Module(ModuleWire {}),
            EntryPayloadFlags::default(),
        )
    }

    fn build_ir(entries: &[(u8, OwnedEntryPayload)]) -> PristineIntroTable {
        let mut ir = PristineIntroTable::new();
        for (n, payload) in entries {
            ir.insert_live(intro(*n), payload.clone(), None);
        }
        ir
    }

    // -----------------------------------------------------------------------
    // Test 1: full materialize_index round-trips both symbols
    // -----------------------------------------------------------------------

    #[test]
    fn full_materialize_index_has_both_symbols() {
        let repo = IrRepository::in_memory(pkg(), "main").unwrap();

        let ir = build_ir(&[
            (1, module("root")),
            (2, function("do_thing")),
        ]);
        repo.record_generation(&ir).unwrap().expect("change recorded");

        let idx = repo.materialize_index().unwrap();

        assert_eq!(idx.len(), 2);
        assert!(idx.get(intro(1)).is_some(), "intro 1 must be present");
        assert!(idx.get(intro(2)).is_some(), "intro 2 must be present");

        // Decode the blobs and check payloads survive the round-trip.
        assert_eq!(payload_of(&idx.get(intro(1)).unwrap()[..]), module("root"));
        assert_eq!(payload_of(&idx.get(intro(2)).unwrap()[..]), function("do_thing"));
    }

    // -----------------------------------------------------------------------
    // Test 2: incremental reuse — untouched Arc is reused
    // -----------------------------------------------------------------------

    #[test]
    fn incremental_reuse_untouched_arc() {
        let repo = IrRepository::in_memory(pkg(), "main").unwrap();

        // Gen A: symbols 1 + 2.
        let ir_a = build_ir(&[
            (1, module("root")),
            (2, function("do_thing")),
        ]);
        repo.record_generation(&ir_a).unwrap().expect("change A");
        let idx_a = repo.materialize_index().unwrap();
        assert_eq!(idx_a.len(), 2);

        // Gen B: modify symbol 1, keep symbol 2, add symbol 3.
        let ir_b = build_ir(&[
            (1, module("root_v2")),   // changed
            (2, function("do_thing")), // unchanged
            (3, function("new_fn")),  // added
        ]);
        repo.record_generation(&ir_b).unwrap().expect("change B");

        // Incremental rebuild from idx_a.
        let idx_b = repo.materialize_index_incremental(&idx_a).unwrap();

        // Must have intros {1, 2, 3}.
        assert_eq!(idx_b.len(), 3, "expected 3 symbols in idx_b");
        assert!(idx_b.get(intro(1)).is_some());
        assert!(idx_b.get(intro(2)).is_some());
        assert!(idx_b.get(intro(3)).is_some());

        // Symbol 2 is untouched → same Arc pointer.
        assert!(
            Arc::ptr_eq(
                idx_a.get(intro(2)).unwrap(),
                idx_b.get(intro(2)).unwrap()
            ),
            "symbol 2 must reuse the same Arc (untouched)"
        );

        // Symbol 1 must have changed.
        assert!(
            !Arc::ptr_eq(
                idx_a.get(intro(1)).unwrap(),
                idx_b.get(intro(1)).unwrap()
            ),
            "symbol 1 must have a different Arc (modified)"
        );

        // idx_b must deep-equal a fresh full materialize.
        let idx_full = repo.materialize_index().unwrap();
        assert_eq!(idx_b.len(), idx_full.len());
        for intro_id in idx_full.intros() {
            let b_bytes = &idx_b.symbols[&intro_id][..];
            let f_bytes = &idx_full.symbols[&intro_id][..];
            assert_eq!(b_bytes, f_bytes, "bytes mismatch for intro {intro_id:?}");
        }
    }

    // -----------------------------------------------------------------------
    // Test 3: incremental removal — deleted symbol disappears
    // -----------------------------------------------------------------------

    #[test]
    fn incremental_removal_drops_deleted_symbol() {
        let repo = IrRepository::in_memory(pkg(), "main").unwrap();

        // Gen A: symbols 1 + 2.
        let ir_a = build_ir(&[
            (1, module("root")),
            (2, function("do_thing")),
        ]);
        repo.record_generation(&ir_a).unwrap().expect("change A");
        let idx_a = repo.materialize_index().unwrap();
        assert_eq!(idx_a.len(), 2);

        // Gen B: only symbol 1 (symbol 2 deleted).
        let ir_b = build_ir(&[(1, module("root"))]);
        repo.record_generation(&ir_b).unwrap().expect("change B");

        let idx_b = repo.materialize_index_incremental(&idx_a).unwrap();

        assert_eq!(idx_b.len(), 1, "only intro 1 survives");
        assert!(idx_b.get(intro(1)).is_some(), "intro 1 must still be present");
        assert!(idx_b.get(intro(2)).is_none(), "intro 2 must be absent (deleted)");
    }

    // -----------------------------------------------------------------------
    // Test 4: no-op — same tip returns same tip, is_empty / equal
    // -----------------------------------------------------------------------

    #[test]
    fn incremental_noop_when_tip_unchanged() {
        let repo = IrRepository::in_memory(pkg(), "main").unwrap();

        let ir = build_ir(&[(1, module("root"))]);
        repo.record_generation(&ir).unwrap().expect("change");
        let idx = repo.materialize_index().unwrap();

        // Call incremental with the SAME index — tip hasn't changed.
        let idx2 = repo.materialize_index_incremental(&idx).unwrap();

        // Tips must be equal.
        assert_eq!(idx.tip, idx2.tip, "tips must match on no-op");
        // Pointer equality: Arc instances must be the SAME (clone of prev.symbols).
        assert!(
            Arc::ptr_eq(
                idx.get(intro(1)).unwrap(),
                idx2.get(intro(1)).unwrap()
            ),
            "no-op incremental must return the exact same Arc"
        );
    }

    // -----------------------------------------------------------------------
    // Test 5: partial checkout — checkout_symbol + checkout_symbols
    // -----------------------------------------------------------------------

    #[test]
    fn partial_checkout_symbol_and_symbols() {
        let repo = IrRepository::in_memory(pkg(), "main").unwrap();

        let ir = build_ir(&[
            (1, module("root")),
            (2, function("do_thing")),
            (3, function("helper")),
        ]);
        repo.record_generation(&ir).unwrap().expect("change");

        // checkout_symbol for a present intro.
        let bytes1 = repo.checkout_symbol(intro(1)).unwrap();
        assert!(bytes1.is_some(), "intro 1 must be present");
        assert_eq!(payload_of(&bytes1.unwrap()[..]), module("root"));

        // checkout_symbol for an absent intro.
        let absent = repo.checkout_symbol(intro(99)).unwrap();
        assert!(absent.is_none(), "non-existent intro must return None");

        // checkout_symbols for a subset.
        let multi = repo.checkout_symbols(&[intro(1), intro(3)]).unwrap();
        assert_eq!(multi.len(), 2);
        assert!(multi.contains_key(&intro(1)));
        assert!(multi.contains_key(&intro(3)));
        assert!(!multi.contains_key(&intro(2)));

        assert_eq!(payload_of(&multi[&intro(3)][..]), function("helper"));
    }

    // -----------------------------------------------------------------------
    // Test 6: incremental from empty prev (MaterializedIndex::empty())
    // -----------------------------------------------------------------------

    #[test]
    fn incremental_from_empty_rebuilds_all() {
        let repo = IrRepository::in_memory(pkg(), "main").unwrap();

        let ir = build_ir(&[
            (1, module("root")),
            (2, function("do_thing")),
        ]);
        repo.record_generation(&ir).unwrap().expect("change");

        let prev = super::MaterializedIndex::empty();
        let idx = repo.materialize_index_incremental(&prev).unwrap();

        assert_eq!(idx.len(), 2);
        assert!(idx.get(intro(1)).is_some());
        assert!(idx.get(intro(2)).is_some());
    }
}
