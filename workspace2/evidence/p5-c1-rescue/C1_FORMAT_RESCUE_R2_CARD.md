# C1 format rescue R2 — semantic-evidence recard

`f0bb2e9f` is superseded for closure. Its format API remains the candidate under test, but its
allocator test, discarded pointer arithmetic, underspecified private-field fixture, and ephemeral-rlib
custody claim are rejected. This R2 card changes only evidence/test semantics; it does not authorize
new wire bytes, public items, fields, crates, or C2 behavior.

## Frozen observable terminal and unchanged API

The sole terminal remains: validate one caller byte slice as `magic, schema, entity-count, type-count,
entity u32 lane, type u32 lane`; lend exactly `EntityId` and `TypeId` cursors. The byte layout, six
`FragmentError` variants, literal validator order, private `FragmentView` fields, and exact/fused
cursors are exactly `C1_FORMAT_RESCUE_MANAGER_CARD.md`. Allowed production paths remain its five IR
paths; allowed evidence is `workspace2/evidence/p5-c1-rescue/**`. All v1–v8 evidence and f0bb2e9f
closure are retained as rejected/superseded history, never patched into acceptance.

## R2 negative space and full test skeleton

No production or test source may contain `unsafe`, `GlobalAlloc`, `global_allocator`, `Vec`, `Box`,
`Arc`, `Rc`, `alloc`, a builder/prepared implementation, a third lane, a raw conversion, an ID
reexport, or an owning fixture. All mutations use stack arrays:

```rust
const ONE: [u8; 12] = [MAGIC, SCHEMA, 1, 1, 7, 0, 0, 0, 9, 0, 0, 0];
const ONE_PLUS_ONE: [u8; 13] = [MAGIC, SCHEMA, 1, 1, 7, 0, 0, 0, 9, 0, 0, 0, 0];
fn payload_mutations_remain_legal() {
    for index in HEADER_BYTES..ONE.len() {
        let mut bytes = ONE;
        bytes[index] ^= 1;
        let view = match FragmentView::validate(&bytes) { Ok(view) => view, Err(_) => return };
        assert_eq!(view.input_len(), bytes.len());
    }
}
```

The real test uses `match` rather than `expect`/`unwrap` in scenario/process fixtures. The skeleton is
literal: (a) 0/1/2 golden exact/fused cursors; (b) every strict prefix of all three goldens; (c) all
four header cells plus `ONE_PLUS_ONE`; (d) each payload-cell stack mutation; (e) internal private-field
pointer containment; and (f) actual-rlib positives/negatives. The payload test is named only for
payload validity; it must not claim pointer containment. The internal unit test is the sole containment
test and must causally assert each of `envelope`, `entity_lane`, and `type_lane` start/end lies inside
the original input start/end.

## Exact actual-rlib contract and custody

The test resolver returns a private `RlibCustody { path: PathBuf, cardinality: usize, sha256: [u8; 64] }`.
It reads the executable parent, enumerates `libnudox_ir_format-*.rlib`, rejects cardinality other than
one, then invokes external `shasum -a 256 <resolved path>` and parses exactly the 64 lower-hex bytes.
The test prints the exact resolved path, cardinality, and hash with `eprintln!`; final manager replay
uses `--nocapture` and copies that complete line into evidence even when the target is temporary.
`DefaultHasher` is forbidden.

Each compiler child has piped stdin, explicit `--extern` to the custody path, `stdout(Stdio::null())`,
and captured stderr. The private-view source supplies all known fields:

```rust
use nudox_ir_format::FragmentView;
const BAD: FragmentView<'static> = FragmentView {
    envelope: &[], entity_lane: &[], type_lane: &[],
};
```

It must have exactly one coded `error[E0451]` and all three symbols `envelope`, `entity_lane`, and
`type_lane`; no generic `error:` acceptance is permitted. The other *separate* fixtures retain exact
single-primary predicates for `From` E0277, `TryFrom` E0277, ID imports E0603, `FragmentBuilder` E0432,
`PreparedFragment` E0432, and `atom_ids` E0599. The legal fixture calls named `validate` through
`match`, then fully consumes both cursors.

## Resource claim correction

Delete the custom allocator and all zero-allocation/copy measurement claims from production tests and
evidence. `#![no_std]`, no `alloc` dependency, private borrowed subslices, and scalar decoding are
structural facts only. Allocation/copy/call-path cost for a whole consumer is **UNVERIFIED** pending a
separately calibrated named release-consumer/codegen artifact; it is not part of C1 R2.

## Budget, calibration, closure

`src/lib.rs` is 146 lines against its 210 ceiling. `tests/fragment.rs` forecast is 322, ceiling
360, reserve 38 after removing allocator code, adding custody, and hardening diagnostic cardinality;
any new test helper needs an explicit
consumer. No dependency, manifest, or lockfile change is expected. Before edit, Luna and independent
Terra must calibrate this card; after the bounded Luna repair, a different Terra reviews the exact repair
commit. Manager runs two fresh-target locked test/Clippy/fmt/diff/status gates. Final evidence includes
commands, status, test cardinalities, exact custody line, external SHA-256 source hashes, limits, and
one isolated-prototype verdict only.
