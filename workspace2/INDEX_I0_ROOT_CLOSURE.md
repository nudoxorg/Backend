# Index I0 root closure receipt

## Identity and custody

```text
capability: closed typed snapshot and exact/lexical segment identity vocabulary
candidate commit: b69a4c88ee1845c146346fbb8c2d56c0fe295e38
candidate tree: 69933a2b8ddfa84dbd496bae784031f1ff3d7649
review worktree: root-review-index-i0 at nudox-review-index-i0
manager baseline: f9419673f451ec8796d3f9462f3bace667c13435
manager range: 139ca391..16c8993f; root repairs: 72731576..b69a4c88
unrelated dirty paths: none
```

The primary manager and first read-only reviewer were explicitly created with non-inheriting
`gpt-5.6-terra` overrides. Both writing workers were explicitly created with non-inheriting
`gpt-5.6-luna` overrides. Root created the final read-only reviewer with an explicit non-inheriting
`gpt-5.6-terra` override. Successful spawn calls are the runtime model evidence; the live listing API
does not expose a second selected-model field. The final reviewer proved no edits by returning the
same four-path status it received. Its one LOC finding was valid, but its custom tripwire table omitted
mandatory literal rows, so its approval mechanics are not closure evidence.

## Law ledger

| Law | Public terminal and falsifier | Pre-fix or mutant result | Candidate result | State |
| --- | --- | --- | --- | --- |
| Canonical bytes, not owner identity, determine IDs | `local_and_remote_canonical_bytes_produce_the_same_typed_ids`; erase input use | equal/different mutation collapsed to one digest, exit 101 | distinct owners with equal bytes agree; one-byte mutations differ | REPRODUCED |
| Exact and lexical identities cannot mix | three compile-fail doctests; project Lexical through Exact | two doctests compiled, exit 101 | all three fail compilation | REPRODUCED |
| Only approved family brands instantiate segment IDs | RootDomain compile-fail; add it to the sealed relation | doctest compiled, exit 101 | arbitrary registered domain is rejected | REPRODUCED |
| Raw family codes are closed and lossless | five-row bidirectional table plus 0/6/255; corrupt `From` | Exact returned 255, exit 101 | all codes round-trip; error retains named `code` | REPRODUCED |
| ID/brand representation adds no storage | `ids_and_markers_have_the_declared_layout` | duplicate Exact/Lexical wrappers existed at manager closure | IDs equal their `Deref` target layout; reused brands equal unit layout | REPRODUCED |
| Construction retains no heap owner or canonical-byte copy | serial warmed allocation test; allocate one boxed byte | one allocation/one byte observed, exit 101 | complete `AllocationInfo` is zero | REPRODUCED |
| Registry and dependency closure remain bounded | marker uniqueness, lock comparison, path diff | manager LOC ledger omitted a repair | labels unique; only local package absent from root lock; final counts exact | REPRODUCED |
| Negative space remains absent | source/path audit | duplicate markers and unused `FAMILY` projection existed | no query, manifest, backend, I/O, unsafe, SIMD, dynamic dispatch, or future identity | REPRODUCED |

## Mechanical tripwires

| Tripwire | Count and exact locations | Disposition | Evidence |
| --- | --- | --- | --- |
| panic/unwrap/expect/unreachable | 0 in changed production/tests | required absence | token scan and clippy |
| source-dropping conversion or `map_err` | 0 | required absence | all fallibility is `TryFrom<u8>` retaining `code` at `src/lib.rs:60-71` |
| dyn/Box/Vec/Arc/Rc | 0 in changed production/tests | required absence | source scan; allocation mutant is evidence only and was reverted |
| public tuple fields or positional semantic tuples | 0 shipping; five two-column fixture rows at `tests/vocabulary.rs:45-51` | test table, named on destructure | no `.0`/`.1` or public positional facts |
| unit/stateless namespace structs | three generated domain ZSTs at `nudox-id/src/marker.rs:103-115` | required identity brands, not namespaces | snapshot/exact/lexical IDs consume them; layout is zero-sized |
| public local traits or one-implementation delegation | public sealed `IndexSegmentFamily` at `src/lib.rs:75-95`, two implementations | required whitelist | deleting the projection admits arbitrary `Domain` because Rust does not enforce type-alias bounds |
| one-letter generic parameters | 0 | required absence | public alias uses `FamilyBrand` at `src/lib.rs:101-102` |
| numeric discriminants/sentinels/offsets/capacities/loop bounds | 10 shipping protocol cells at `src/lib.rs:36-44,65-69`; 11 named/golden test literals at `tests/vocabulary.rs:11-16,46-50` | required wire vocabulary and independent golden table | every code and boundary is bidirectionally exercised |
| test-only Option/discarded results/success-only assertions | 0 Option/Result discard; three compile-fail `_` bindings at `src/lib.rs:9,15,21`; six `black_box` return discards at `tests/vocabulary.rs:83-89` | compile probes and optimizer barriers | no error/owner is discarded; assertions inspect exact values |
| unsafe/SIMD/allocator/dependency additions | 0 unsafe/SIMD; one existing-version dev allocator harness | required measurement only | nested lock adds no registry package/version absent from root lock |
| public item without current consumer and falsifier | 0 | required absence | every enum variant, conversion, error, brand, trait projection, and alias is used by a public/fault test |

## Resource and scope ledger

```text
production: 120 net formatted Rust lines / cap 230 / reserve 110
tests: 92 formatted Rust lines / cap 120 / reserve 28
retained allocations: zero
measured construction: three warmed non-empty IDs, zero total/current/peak allocations and bytes
canonical-input copies: zero; only the required 32-byte digest value is returned
work: one inherited BLAKE3 stream per explicit ID construction; no retry, scan, or wrapper hash
dependencies: nudox-id path dependency; allocation-counter dev-only at an existing locked version
optimized text: no text-size claim for this vocabulary checkpoint
```

The lock comparison normalized package name/version/checksum triples. Its only nested package absent
from the root lock is local `nudox-index-vocab 0.1.0`. The candidate changes only the two approved ID
registry files, five nested index paths, manager evidence, and this root receipt.

## Hostile attempts

```text
owner/witness mixing: exact/lexical projection mutant made both cross-family doctests compile
input removal: shared ContentId input-erasure mutant failed the byte-mutation public journey
invalid family: RootDomain admission mutant made the whitelist doctest compile
error loss: all 253 invalid u8 values preserve the supplied code by the exhaustive decoder shape;
            0, first-unassigned, and 255 are exact executable boundaries
cancellation/progress/reuse: not applicable; the contract is synchronous, pure, and has no owner state
torn/corrupt/restart: not applicable; this checkpoint defines identity vocabulary, not a format
disabled diagnostics: not applicable; no probe or adapter is introduced
simpler design: duplicate family ZSTs and unused associated code were deleted; an untyped ContentId
                was rejected because it permits exact/lexical substitution
strongest surviving counterexample: selected-model identity has no independently inspectable live field;
                                   explicit successful model override calls are the available proof
```

## Clean closure

Pass one and pass two both ran against the exact candidate commit above with immediate empty
`git status --porcelain=v1`:

```text
root: cargo fmt --check
root: cargo test --offline --locked --workspace --all-targets --no-fail-fast
root: cargo clippy --offline --locked --workspace --all-targets -- -D warnings
root: cargo doc --offline --locked --workspace --no-deps
index: cargo fmt --check
index: cargo test --offline --locked --workspace --all-targets --no-fail-fast -- --test-threads=1
index: cargo test --offline --locked --workspace --doc --no-fail-fast
index: cargo clippy --offline --locked --workspace --all-targets -- -D warnings
index: cargo doc --offline --locked --workspace --no-deps
```

Unverified tooling: cross-target allocation behavior and a second runtime-selected-model field. No
claim depends on optimized text, network access, unsafe, SIMD, async, durability, or concurrency.
There are zero product blockers/majors, and both production and test reserves remain unused.

Verdict: **ACCEPTED** for the I0 typed-vocabulary child only. Snapshot manifest format, validation,
lookup, publication, query, and remote behavior remain open capabilities.
