# Direct source-isolated pre-edit Terra review receipt

This is the reviewer return captured verbatim in substance. It made no edits and did not inspect the writable manager checkout.

## Runtime and source custody

- Reviewer task: `/root/canonical_byte_local_closure_terra/pre_edit_reviewer`.
- Direct dispatch requested: `gpt-5.6-terra` / `xhigh`, `fork_turns="none"`; reviewer runtime itself reported that model/effort identity was not exposed and did not independently assert it.
- Registered config read from snapshot: `/private/tmp/canonical-byte-local-closure-review2.DTrwQB/snapshot/workspace2/.codex/agents/nudox-terra-reviewer.toml`; configured `gpt-5.6-terra` / `xhigh`; config SHA-256 `4ac19e5c151dd3bcd73ea9e175c3fb3f02fde419e245060d4ee65004919f120a`.
- Read-only snapshot: `/private/tmp/canonical-byte-local-closure-review2.DTrwQB/snapshot/workspace2`; supplied aggregate SHA-256 `704938ef090e647d7d1c1037f937e5f1a6b1fa70349d4cce0d7245e4fe6b0028`.
- Read-only rationale-free packet: `/private/tmp/canonical-byte-local-closure-review2.DTrwQB/packet/pre-edit-1.md`; SHA-256 `4430a0ffcdd8a7e1fa999c66f98c4a15001ead56667d1b176391d16ccb32ecac`.
- Reviewer reported a different source-after aggregate because it used an undocumented different aggregation recipe. The manager reran the documented sorted, relative-path per-file SHA-256 manifest aggregation after return; it remained `704938ef090e647d7d1c1037f937e5f1a6b1fa70349d4cce0d7245e4fe6b0028`. No source mutation was observed.

## Ranked findings

### F-01 — BLOCKER: frozen terminal asserts the wrong backing owner

Location: `crates/nudox-operation/tests/canonical_byte_local_closure.rs:185`.

The journey serializes fixture bodies into caller-owned `pack_bytes` at lines 134–135. `ObjectPackView` is reconstructed solely from that backing, and its lookup lends a sub-slice of it (`crates/nudox-object-pack/src/view.rs:54–88`). A conforming `MemoryStore<ObjectDomain, &[u8]>` transfer therefore retains the selected `pack_bytes` body pointer. The terminal instead asserts pointer equality to `bodies[1]`, a distinct static fixture allocation. Passing needs a body copy, bypass, or artificial pointer redirection, each forbidden by CBC-04. Falsifier: the current two-body journey; the valid stored leaf pointer is inside `pack_bytes`, never `bodies[1]`.

### F-02 — MAJOR: public journey does not kill the listed authority bypasses

Location: `crates/nudox-operation/tests/canonical_byte_local_closure.rs:103–163`.

The all-valid same-root flow does not exercise a mismatched root/locality pair, malformed root, malformed pack, partial projection, missing store body, or foreign verified root. Removing root/locality pairing, selected verification, store-presence verification, or verified-operation root binding can leave this happy path green. Falsifier: add each targeted fault and require its exact typed error. This finding remains actionable after the F-01 authority decision.

### F-03 — MAJOR: resource and public-surface proof absent

Location: `crates/nudox-operation/tests/canonical_byte_local_closure.rs:101–158`; matrix CBC-07/CBC-08.

Two-object fixture vectors and caller scratch do not establish 1/100,000 owner/backing peak, allocations, scan/work, release text, or current consumers for the planned generic borrowed-root types. A `ValidatedRoot` can retain an owned native arena while still satisfying the raw byte pointer assertion. Falsifier: non-empty isolated 1/100k validation/plan/bind measurement plus copy/reparse mutant.

### F-04 — QUESTION: one-time witness semantics are unspecified

Location: `crates/nudox-operation/tests/canonical_byte_local_closure.rs:163–164`.

The proposed API borrows `&VerifiedGeneration`, so the same witness can bind repeatedly. The chief phrase “binds ... exactly once” may mean sole comparison boundary rather than consuming one-shot authority. Clarify only if a caller-observable duplicate-bind law is intended; otherwise do not introduce an unearned one-shot state.

## Tripwire table

Scope: the entire frozen chief test; no production candidate exists.

| Tripwire | Count | Exact locations | Disposition |
| --- | ---: | --- | --- |
| panic/unwrap/expect/unreachable | 0 | chief test; literal patterns scanned | zero-hit |
| source-dropping conversion or `map_err` | 3 | lines 69, 112, 114 | named `JourneyError` source-preserving conversions; no erased `map_err` |
| lossy/ambiguous conversion or raw authority bypass | 13 syntax hits | lines 68–71, 75, 83–84, 101, 103, 108, 136, 140–141, 158 | fixture conversions; malformed-root proof remains missing (F-02) |
| checked-arithmetic sentinel/saturation or operand loss | 0 | chief test | zero-hit |
| `dyn`/`Box`/`Vec`/`Arc`/`Rc` | 6 | lines 7, 101, 108, 118–119, 134 | test fixtures only; no candidate shipping surface |
| public tuple fields / stateless structs / public local traits / one-letter generics | 0 | chief test | zero-hit |
| numeric semantic literals | 14 | lines 83–87, 101, 108, 125, 129, 141, 176, 185 | fixture/capacity values; no resource proof (F-03) |
| test-only discarded results or success-only assertions | 0 | chief test | zero-hit; exact terminal/pointer assertions exist |
| unsafe/SIMD/allocator/dependency additions | 0 | chief test | zero-hit |
| public item without current consumer/falsifier | 0 candidate items | chief test | planned APIs need evidence (F-02/F-03) |

## Cleared suspicions and self-check

Existing `GenerationView` keeps coherent root/locality fields private and checks generation plus row count (`nudox-root/src/locality/view.rs:291`); it is a sound precedent. Existing pack lookup/verification and borrowed memory-store owner are the simple control; no self-referential owner or refcount is justified. Existing staging already rejects partial/missing closure with typed errors. The strongest counterexample is F-01. Likely hidden cost is a borrowed-root parser retaining a native arena or reparsing. Cargo/Dylint/Clippy/allocation gates were not run: the direct reviewer had no independently verified writable compiler-output sandbox.
