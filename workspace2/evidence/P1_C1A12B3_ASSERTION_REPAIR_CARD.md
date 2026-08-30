# P1 C1a12B3 five-assertion repair

Base: `2b62eec` on the isolated C1a repair branch. The B2 review passed canonical byte equality,
branch/incoming-cycle witnesses, mismatch cases, scale acceptance, and literal private-field UI
proof, then blocked five assertions only. This card permits editing only
`crates/nudox-root/tests/canonical_root_view.rs`; no UI, production, lab, dependency, C1b, or P2
change is authorized.

1. Compare `view.bytes.as_ptr()` and length with the original supplied `encoded.as_ptr()` and
   length, proving the complete retained root range is inside that caller buffer.
2. Build the two exact cross-row collision directions: an early missing parent with a later
   invalid parent-presence marker must return `ParentPresent`; a later absent-parent-key with a
   still later missing parent must return `AbsentParentKey`.
3. Compare every public scanned and looked-up `GenerationEntry` field: key, parent, `ObjectRef`,
   and `Locality`, in canonical order—not keys/count only.
4. Construct genuinely different valid localities for the same root (one ordinary empty and one
   public valid nonresident exception) and assert public locality differs while root identity and
   canonical bytes remain stable.
5. Assert the concrete grammar geometry theorem for every represented success case:
   `encoded.len() == HEADER + ROW * entry_count`, alongside the existing checked failure cases.

Preserve clean all-target clippy and all existing accepted B2 evidence. Run formatting, strict
root all-target clippy, root integration test, then commit. Fresh Terra review is required before
the C1a12C lab card.
