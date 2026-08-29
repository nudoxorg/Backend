# I0 first child manager card — typed snapshot vocabulary

## Frozen specimen

| Item | Literal contract |
| --- | --- |
| Capability | Establish the portable, closed index vocabulary needed to name a snapshot and a segment without allowing an exact-family segment identity to stand in for lexical, relation, usage, or vector identity. |
| First observable terminal | An external `nudox-index-vocab` consumer hashes the same borrowed canonical byte slice through independent local and remote byte owners and obtains equal `IndexSnapshotId` and `IndexSegmentId<Exact>` values; a downstream attempt to assign `IndexSegmentId<Lexical>` to `IndexSegmentId<Exact>` does not compile. |
| Named baseline | Git `f9419673f451ec8796d3f9462f3bace667c13435`, branch `autonomous-index-contract`, clean before this documentation cycle. |
| Public journey | `workspace2/planes/index/crates/nudox-index-vocab/tests/vocabulary.rs::local_and_remote_canonical_bytes_produce_the_same_typed_ids`. It imports only the published vocabulary crate, makes separately owned local and remote `[u8; 24]` byte arrays with equal contents, borrows each as `&[u8]`, creates one snapshot ID plus exact and lexical segment IDs from each, and asserts equality. Two public `compile_fail` doctests in `src/lib.rs` use only `nudox_index_vocab` imports and prove that neither direct assignment nor `Into` can turn `IndexSegmentId<Lexical>` into `IndexSegmentId<Exact>`; a third imports the registered `RootDomain` and proves that the sealed family projection rejects arbitrary identity domains. |
| Checkpoint | Checkpoint 2 stops immediately after those two external terminals and their required negative/error evidence. |

The seven frozen governing inputs are, literally:

```text
workspace2/.codex/skills/steward-greenfield-rust-program/SKILL.md
workspace2/.codex/skills/deliver-reviewed-rust-slice/SKILL.md
workspace2/.codex/skills/manage-rust-swarm/SKILL.md
workspace2/.codex/skills/calibrate-rust-agent-contract/SKILL.md
workspace2/.codex/skills/review-rust-gem/SKILL.md
workspace2/.codex/skills/write-evidence-rubric/SKILL.md
workspace2/.codex/skills/build-greenfield-index-plane/SKILL.md
```

## Exact allowed writes

Only the following future production/test paths are writable by the builder. Every listed `planes/index`
path is absent at the frozen baseline; no wildcard authorizes another file.

```text
workspace2/crates/nudox-id/src/marker.rs
workspace2/crates/nudox-id/src/lib.rs
workspace2/planes/index/Cargo.toml
workspace2/planes/index/Cargo.lock
workspace2/planes/index/crates/nudox-index-vocab/Cargo.toml
workspace2/planes/index/crates/nudox-index-vocab/src/lib.rs
workspace2/planes/index/crates/nudox-index-vocab/tests/vocabulary.rs
```

The manager alone may update this card and `workspace2/INDEX_I0_CALIBRATION_RAW.md`; neither is a
production path. No root `workspace2/Cargo.toml`, lockfile, schema registry, existing test, future
index crate, query, manifest builder/view, adapter, fixture corpus, ROADMAP, or shared skill is
writable in this child proof.

## Preserved laws and explicit negative space

- Borrowed canonical bytes remain the source of each ID; local and remote differ only in byte owner,
  never in canonical bytes or hashing logic.
- `SegmentFamily` is the sole public closed, exhaustive `repr(u8)` vocabulary with `Exact`,
  `Lexical`, `Relation`, `Usage`, and `Vector`. A differently named sealed
  `IndexSegmentFamily` relation is implemented directly by the registered exact and lexical identity
  domains, reexported semantically as `Exact` and `Lexical`; relation/usage/vector receive no identity
  marker or constructor until their own proof. The first terminal constructs exact and lexical IDs to
  prove non-interchangeability.
- `IndexSnapshotId` and `IndexSegmentId<Family>` are typed values, not strings, raw hashes, a family
  field that callers can mismatch, or a public multi-field coherence witness.
- No allocation is allowed on either canonical-ID path after byte owners are set up; no byte copy,
  parsing, manifest, directory, scan, query, publication, range, I/O, async, backend, compatibility,
  unsafe, SIMD, macro, trait object, dynamic schema, or new dependency is in scope.
- The existing sealed declarative `nudox-id` table is the sole durable domain-label registry: no raw
  labels, runtime/linker registration, inventory, or collision scheme is permitted. This proof adds
  exactly `IndexSnapshotDomain`, `IndexExactSegmentDomain`, and `IndexLexicalSegmentDomain`, with
  the exact sixteen-byte labels `b"nudox.idx.snap.1"`, `b"nudox.idx.exact1"`, and
  `b"nudox.idx.lexic1"`. It may not add relation/usage/vector, delta, recipe, model,
  artifact-encoding, or other future-phase identities.

## Design and ownership decision

The safe/std baseline reuses two centrally registered zero-sized `nudox-id::Domain` values as the
sealed `IndexSegmentFamily` brands rather than wrapping them in second marker types.
`IndexSegmentId<Family>` is the existing `ContentId<<Family as
IndexSegmentFamily>::IdentityDomain>`; `IndexSnapshotId` is `ContentId<IndexSnapshotDomain>`.
`SegmentFamily` has `From<SegmentFamily> for u8` and `TryFrom<u8> for SegmentFamily`: `Exact = 1`,
`Lexical = 2`, `Relation = 3`, `Usage = 4`, `Vector = 5`; its only error is
`UnknownSegmentFamily { code: u8 }`, which preserves every raw code outside 1 through 5. The public
trait is sealed and exposes only `type IdentityDomain: nudox_id::Domain`; its projection is the
compile-time whitelist that prevents an arbitrary registered domain from becoming a segment family.
No helper such as `family()`, `from_family`, raw-label accessor, reexported raw domain name, or
conversion between family IDs is public. All family IDs use the existing public,
allocation-free inherent
`ContentId::from_canonical_bytes(&[u8])`; this proof adds no family-specific constructor. Each call is
one independent BLAKE3 pass over its own supplied bytes; no shared hashing is claimed.

Rejected alternatives:

1. `struct SegmentId { family: SegmentFamily, bytes: [u8; 32] }` permits coherent-looking but
   caller-assembled field combinations and makes cross-family misuse runtime-only.
2. One untyped `IndexSegmentDomain` plus a phantom wrapper leaves every family under the same protocol
   label and weakens the plan's family-specific identity law.

No generic has imagined users: the one generic removes two current copy/paste segment-ID aliases;
the two current consumers are the public exact and lexical construction paths. Its monomorphized
text cost is bounded by those two exercised calls and two marker implementations.

## Resource budget and stop conditions

| Resource | Hard cap and required evidence |
| --- | --- |
| Retained/live bytes | Zero retained allocations by the vocabulary; the caller retains its two 24-byte owners. `size_of`/`align_of` must show each ID is the existing 32-byte, 1-aligned content ID and each family marker is zero-sized. |
| Allocations/copies | Exactly zero allocations after byte-owner setup on both ID paths, measured with `allocation_counter::measure` around only a warmed non-empty constructor call. “No copies” means no copy or retention of canonical input bytes: source audit rejects `Vec`, `Box`, `Arc`, `to_vec`, `to_owned`, `copy_from_slice`, or an input-byte field. Returning the 32-byte digest value is not charged as an input-byte copy. The test binary runs alone with `--test vocabulary -- --test-threads=1`; unavailable harness evidence stops the claim. |
| Work | Source audit requires exactly one `ContentId::from_canonical_bytes` call per constructed ID and no `ContentHasher`, prehash, retry, scan, binary search, directory, range, or parsing code. The existing constructor supplies one BLAKE3 pass over the supplied 24 bytes per ID. |
| Text/dependency | No new registry dependency or version is allowed. Direct dependencies are only path `nudox-id` and test-only `allocation-counter`; every resolved registry package/version/checksum in the nested lockfile must already occur identically in `workspace2/Cargo.lock`. The allowed new package is only local `nudox-index-vocab`. No new registry package/version, unsafe/SIMD, macro, `dyn`, `Arc`, allocation policy, or root-workspace membership is allowed. Text-size claim is limited to the existing `nudox-id` implementation plus one vocabulary crate; no release-size win is claimed. |
| Production LOC | Forecast 155 normally formatted Rust LOC; ceiling 230; unused reserve 75 lines (greater than 25 and 10%). Stop before the next file if written plus remaining forecast reaches 138 lines, or a file exceeds forecast by 20%/25 lines. |
| Test LOC | Forecast 100 normally formatted Rust LOC; ceiling 120; unused reserve 20 lines (greater than 15 and 10%). The public compile-fail doctests belong to the production source forecast. Stop at 72 forecasted/written lines if the rest cannot fit. |
| Latency/queue | Synchronous pure hash only; no queue, future, task, timeout, or latency claim. |

Immediate stop triggers: a new public item absent from the skeleton; any need for a manifest, wire
record, `Vec`, `Box`, `Arc`, direct BLAKE3 use, dependency/manifest outside the listed nested
workspace, unsafe/SIMD, a second proof-bearing production module, a raw identity label outside
`nudox-id`, or an inability to make the family mismatch compile-fail externally.

## Normally formatted skeleton ledger

The forecast comes from the formatted, compile-checked scratch skeleton recorded below, not from the
ceiling. Rust LOC excludes TOML and lockfile lines but includes docs and test support.

| File | Public/private items and current responsibility | Forecast Rust LOC |
| --- | --- | ---: |
| `crates/nudox-id/src/marker.rs` | snapshot plus exact/lexical sealed index-family domain markers in the existing central registry | 12 |
| `crates/nudox-id/src/lib.rs` | five marker reexports | 2 |
| `planes/index/crates/nudox-index-vocab/src/lib.rs` | closed family enum; sealed exact/lexical marker relation; code/error; typed snapshot/segment aliases; canonical-byte constructors and downstream compile-fail doctests | 141 |
| **Production total** |  | **155** |
| `planes/index/crates/nudox-index-vocab/tests/vocabulary.rs` | public local/remote parity, raw-code rejection, layout, source-audited borrowed input, and serial allocation evidence | 100 |
| **Test total** |  | **100** |

The future nested manifests are intentionally tiny and not a reserve for implementation surface:
`planes/index/Cargo.toml` declares only `crates/nudox-index-vocab`, the current workspace package
facts, and existing pinned test-only `allocation-counter`; the crate manifest names only path
dependency `nudox-id = { path = "../../../../crates/nudox-id" }` and that workspace test dependency.
`planes/index/Cargo.lock` is generated from those exact inputs. The compile-fail public-consumer proof
is a `compile_fail` doctest in `src/lib.rs`, so no `trybuild` dependency or UI fixture exists. All
three manifests/lockfile are new, absent baseline files and are subject to the no-new-package/version
stop trigger.

## Frozen baseline/digest/LOC ledger

SHA-256 digests are file bytes at the named baseline. Rust LOC is normally formatted physical source.

| Path | Baseline SHA-256 | Baseline LOC |
| --- | --- | ---: |
| `workspace2/crates/nudox-id/src/marker.rs` | `7216af5891497eb23b854c176a9a6c7900c9d49d0a9ae1c6369400065354faa1` | 156 |
| `workspace2/crates/nudox-id/src/lib.rs` | `6f2bc5fdbc9bf560acd0af7e443a75148c524547f95c09210c1627389a01d75f` | 28 |
| `workspace2/planes/index/Cargo.toml` | absent | 0 |
| `workspace2/planes/index/Cargo.lock` | absent | — |
| `workspace2/planes/index/crates/nudox-index-vocab/Cargo.toml` | absent | 0 |
| `workspace2/planes/index/crates/nudox-index-vocab/src/lib.rs` | absent | 0 |
| `workspace2/planes/index/crates/nudox-index-vocab/tests/vocabulary.rs` | absent | 0 |

The frozen contract inputs are digested in `INDEX_I0_CALIBRATION_RAW.md`; `INDEX_GREENFIELD_PLAN.md`,
`PACKED_COLLECTIONS.md`, `TESTING.md`, and the seven governing skills are read-only inputs, not future
builder writes.

## Evidence rubric and exact commands

| Law | Artifact/command | Required result | Falsifier and stop |
| --- | --- | --- |
| Canonical-byte parity | `vocabulary.rs::local_and_remote_canonical_bytes_produce_the_same_typed_ids` | Equal independently constructed snapshot, exact, and lexical IDs from equal borrowed local/remote bytes. | Change one byte: every constructed ID must differ; if a different owner changes identity, stop. |
| Typed family boundary | three public `compile_fail` doctests in `src/lib.rs`; the cross-family pair imports only `nudox_index_vocab` and the whitelist case imports `nudox_id::RootDomain` | Neither direct assignment nor generic `Into` may turn `IndexSegmentId<Lexical>` into `IndexSegmentId<Exact>`; an arbitrary registered domain cannot instantiate `IndexSegmentId`. | Replace the associated domains with one untyped domain, remove the sealed whitelist, or add a conversion: a doctest must compile and expose the blocker. |
| Closed raw code | `vocabulary.rs::unknown_segment_family_retains_the_raw_value` | Codes 1 through 5 round-trip; raw `0`, `6`, and `255` return `UnknownSegmentFamily { code }` exactly. | Add a default/catch-all mapping or erase the operand. |
| Borrow/layout | `vocabulary.rs::ids_and_markers_have_the_declared_layout` | IDs are 32/1; markers are 0/1; constructors borrow caller bytes. | Retain bytes in a field, add a wrapper allocation, or report an unmeasured pointer claim. |
| Allocation/copy | serial `vocabulary` test plus source audit | Zero allocations after setup on a non-empty 24-byte input; no retained/copy of canonical input bytes. | A normal parallel counter, empty-only run, unrecorded harness provenance, or listed input-copy token stops the claim. |
| Hash work | source audit of vocabulary constructors | One existing `ContentId::from_canonical_bytes` call per snapshot/exact/lexical construction; no prehash/retry path. | A second hash construction, `ContentHasher`, or raw hash literal stops the claim. |
| Registry/no_std | expanded existing `nudox-id` marker test plus nested `cargo clippy` | All three new labels are pairwise distinct from every existing domain/encoding label; vocabulary stays `#![no_std]` and `#![forbid(unsafe_code)]`. | A duplicate/short/non-domain tag, raw registration route, or unsafe/no_std regression stops. |
| Negative space | `git diff --check`, path ledger, and nested lockfile package/version audit | Only literal card paths change; the allowed new crate is not a dependency; no manifest/query/view/publication/backends/root workspace change. | Any unlisted file, new dependency package/version, API, or future identity stops the phase. |

Commands, run from `workspace2` after the future child paths exist:

```text
RUSTC_WRAPPER= cargo fmt --manifest-path planes/index/Cargo.toml --all -- --check
RUSTC_WRAPPER= cargo test --manifest-path planes/index/Cargo.toml -p nudox-index-vocab --test vocabulary --no-fail-fast -- --test-threads=1
RUSTC_WRAPPER= cargo test --manifest-path planes/index/Cargo.toml -p nudox-index-vocab --doc --no-fail-fast
RUSTC_WRAPPER= cargo clippy --manifest-path planes/index/Cargo.toml -p nudox-index-vocab --all-targets -- -D warnings
RUSTC_WRAPPER= cargo doc --manifest-path planes/index/Cargo.toml -p nudox-index-vocab --no-deps
git diff --check f9419673f451ec8796d3f9462f3bace667c13435 -- workspace2/crates/nudox-id/src/marker.rs workspace2/crates/nudox-id/src/lib.rs workspace2/planes/index
```

The current documentation-cycle baseline commands passed: `RUSTC_WRAPPER= cargo fmt --manifest-path
workspace2/Cargo.toml --all -- --check` and `RUSTC_WRAPPER= cargo metadata --manifest-path
workspace2/Cargo.toml --no-deps --format-version 1`.

## Calibration model record

Each cold role is created by an explicit successful `spawn_agent` request with
`model: "gpt-5.6-luna"` and `fork_turns: "none"`; the successful runtime responses identify the
created child task paths in `INDEX_I0_CALIBRATION_RAW.md`. The live-agent inspection interface exposes
only task path and status, not the selected model, so this tool limitation is recorded rather than
replaced with a task-name claim. A failed model override would have stopped calibration; none failed.

“Clean baseline” means the Git tree at the named SHA was clean. The current documentation-only
calibration edits are not candidate production changes; the future builder compares only the literal
production/test paths in this ledger against that SHA. All listed paths are relative to the worktree
root `/private/tmp/nudox-autonomous-index-contract`.

## Required worker return and exact next decision

The builder returns only the committed first proof, candidate digest/LOC ledger, allocation/copy/work
measurement, exact command output, and the strongest failure attempt. A breaker receives this card,
not builder rationale, and must try the two-family mix, a borrowed-byte ownership leak, an unknown raw
family code, and a future-surface addition.

**Exact next decision:** approve this vocabulary proof as the sole I0 child completed, or commission
the separate one-entry manifest-format card; no current authority choice is unresolved.
