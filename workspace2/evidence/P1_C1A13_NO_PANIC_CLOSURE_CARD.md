# P1 C1a13 no-panic/omission final repair

Base: `f63ea1ea` on the isolated C1a repair branch. Ultimate closure review finds three genuine
C1a repair items: root-workspace format of the compacted test, production `expect` calls in
`root_view.rs`, and an over-forecast formatted forge fixture. This card owns only
`crates/nudox-root/src/root_view.rs`, `crates/nudox-root/tests/canonical_root_view.rs`, and the
existing `forged_validated_root` UI source/stderr pair.

1. Remove every new production `expect` from root validation. Thread checked conversion/geometry
errors through the existing source-bearing `RootReadError` contract; no `unwrap`, panic, or
fallback may replace them. Keep exact phase ordering and one-scratch allocation.
2. In trusted borrowed `get`, index only after its exact immutable slice binary search proves the
position; in scan, index only after the `next < len` guard. Use narrowly documented audited
indexing rather than `.get()` return-fallbacks so neither impossible invariant failure becomes
`Ok(None)` nor silently truncates the iterator. Preserve zero allocation and trusted
`ObjectRef::from` projection.
3. Run `cargo fmt --all` and its check from `workspace2`, preserving root integration source at
<=345 added formatted lines and full final behavior proof.
4. Rewrite the forge fixture without `unsafe`/zeroed values, using diverging placeholders if
needed to reach the literal fields. It must still explicitly fail private root/view literals and
direct reassignment through `&mut ValidatedRoot`, `&mut ValidatedLocality`, and
`&mut BorrowedGenerationView` (no `DerefMut`). Compact its formatted source to <=33 added lines
(28 forecast +20%), refresh checked stderr, and retain the separate root/view escape fixture.

Run root all-target strict clippy/tests/UI and workspace formatting. Production added LOC remains
<=620 and test added LOC <=420. The full layout-lab all-target clippy failure in unchanged
baseline `src/main.rs` is documented out-of-scope; the authorized C1a binary lint remains the
required lab gate. Fresh Terra recheck is mandatory before integration.
