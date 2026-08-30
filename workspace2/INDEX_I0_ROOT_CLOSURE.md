# Index I0 root closure receipt

Verdict: **INTEGRATED — minimal identity vocabulary only; index artifact/query work remains open**

## Integrated correction

The historical receipt below records the prototype and the process evidence that produced it. It is
not the current source authority. After the typed-identity repair closed raw cross-domain rebranding,
commit `b1a59b16` integrated only the mechanisms that survived a fresh source comparison:

| Prototype mechanism | Disposition | Integrated owner and proof |
| --- | --- | --- |
| Snapshot/exact/lexical brands | absorb | direct checked `ContentId` aliases in `nudox-index-vocab`; canonical-byte equality and mutation test |
| Direct and `Into` family separation | retain | two compile-fail examples reject lexical-to-exact assignment/conversion |
| Raw family authority | supersede with proof | central identity authority byte plus exact wrong-domain `TryFrom` assertion retaining the complete raw value |
| Allocation/layout behavior | retain | allocation-counter and `Deref::Target` size/alignment tests |
| Relation/usage/vector codes | reject as future-only surface | no current segment, caller, parser, or falsifier; the names remain in the historical commit if a real capability later earns one |
| Dynamic unknown-family decoder | reject as false boundary | no dynamic family-code ingress exists; central identity decoding is the actual raw boundary |
| Sealed family projection trait and generic alias | absorb then delete | its only invariant is owned more strongly by distinct central domain types; public compile-fail and raw rebranding tests kill regression |

The integrated correction therefore did not preserve accidental API merely to keep the prototype
recognizable. It preserved every measured or falsifiable mechanism in its strongest current owner.
The losing implementation, tests, and measurements remain named below in Git evidence.

At the integrated source, `nudox-index-vocab` is 29 production lines and 92 test lines. Its nested
workspace passes all-target locked/offline tests, strict all-target Clippy, formatting, and doctests.
This verdict closes only I0 vocabulary; it does not claim an index snapshot format, immutable segment
view, query engine, placement policy, or remote index.

## Historical prototype receipt

## Identity and custody

```text
capability: closed typed snapshot and exact/lexical segment identity vocabulary
canonical contract: workspace2/INDEX_I0_MANAGER_CARD.md
canonical contract SHA-256: 28d0870cc36c8e3d6d93ed10ac80ecddb612a1ec2456960638eb01fdbf223bfe
calibration artifact: workspace2/INDEX_I0_CALIBRATION_RAW.md
calibration SHA-256: dd826f507d554214c211b0de51d42145c00cf1a90361641ffb8de06e9f1a132d
post-calibration semantic edits: root repairs changed the final source candidate; full current cold deck not rerun
source-candidate commit: 526ad7a7e5bae982ff528132b6c6775d4f5ddbc9
source-candidate tree: f50541fef7b9f82adda86f2412de943967316730
receipt-containing commit: SELF (record externally after this evidence-only commit)
review worktree/branch: nudox-index-i0-closure, detached shared source candidate
receipt worktree/branch: nudox-index-i0-receipt, root-index-i0-receipt
manager baseline/range: f9419673; manager 139ca391..16c8993f; root source repairs 72731576..526ad7a7
unrelated dirty paths: none
```

From source candidate through this receipt, only shared skills, this receipt, ROADMAP, root steward
calibration, and `workspace2/evidence/index-i0/*` differ. `git diff --name-only 526ad7a7..b58c9d78`
contains no shipping source, test, manifest, lock, fixture, or generated consumer. The integrated
shared source candidate is itself `526ad7a7`; no cross-history source mapping is required.

Model custody:

- Primary manager: the historical record says an explicit non-inheriting `gpt-5.6-terra` call
  succeeded, but its returned task/session identity is not retained. **UNVERIFIED**.
- Luna builder and repair: `INDEX_I0_CALIBRATION_RAW.md:170-177` retains explicit
  `model: "gpt-5.6-luna"`, `fork_turns: "none"`, and writing commits, but not returned task/session
  identities. **UNVERIFIED**.
- Current independent reviewer: root invoked `index_i0_closure_reviewer` with explicit
  non-inheriting `gpt-5.6-terra`; the service returned `/root/index_i0_closure_reviewer`. Its exact
  corrected output is retained in `workspace2/evidence/index-i0/independent-review.md`. **REPRODUCED**.
- Reviewer no-edit proof: candidate commit/tree stayed exact and immediate
  `git status --porcelain=v1` was empty. **REPRODUCED**.

Missing historical task identities and stale calibration prevent acceptance; prose is not upgraded to
model evidence merely because the source implementation is green.

## Law ledger

| law | public terminal | falsifier | pre-fix result | candidate result | state | artifact |
|---|---|---|---|---|---|---|
| Canonical bytes, not owner identity, determine IDs | local/remote public journey | erase shared input use | byte mutation collapsed, exit 101 | distinct owners/equal bytes agree; mutation differs | REPRODUCED | `526ad7a7`/`f50541fe`; `workspace2/planes/index/crates/nudox-index-vocab/tests/vocabulary.rs:18-41`; `workspace2/evidence/index-i0/clean-pass-1.md:33`, `workspace2/evidence/index-i0/clean-pass-2.md:33` |
| Exact and lexical identities cannot mix | direct and `Into` doctests | project Lexical through Exact | two doctests compiled, exit 101 | both reject compilation | REPRODUCED | `526ad7a7`; `workspace2/planes/index/crates/nudox-index-vocab/src/lib.rs:6-16`; `workspace2/evidence/index-i0/clean-pass-1.md:34`, `workspace2/evidence/index-i0/clean-pass-2.md:34` |
| Only approved family brands instantiate IDs | RootDomain doctest | add RootDomain to sealed projection | doctest compiled, exit 101 | arbitrary registered domain rejected | REPRODUCED | `526ad7a7`; `workspace2/planes/index/crates/nudox-index-vocab/src/lib.rs:18-22,75-102`; `workspace2/evidence/index-i0/clean-pass-1.md:34`, `workspace2/evidence/index-i0/clean-pass-2.md:34` |
| Raw family codes are closed and lossless | bidirectional five-row table and three unknowns | map Exact to 255 | expected 1 observed 255, exit 101 | all five round-trip; unknown retains `code` | REPRODUCED | `526ad7a7`; `workspace2/planes/index/crates/nudox-index-vocab/tests/vocabulary.rs:43-61`; `workspace2/evidence/index-i0/clean-pass-1.md:33`, `workspace2/evidence/index-i0/clean-pass-2.md:33` |
| IDs and brands add no storage | layout public test | duplicate wrapper brands | duplicate ZST representation existed | IDs equal digest target layout; central brands equal unit layout | REPRODUCED | `526ad7a7`; `workspace2/planes/index/crates/nudox-index-vocab/tests/vocabulary.rs:63-78` |
| Construction retains no heap/input owner | isolated allocation test | allocate one `Box<u8>` in closure | one allocation/one byte, exit 101 | complete allocation fact is zero | REPRODUCED | `526ad7a7`; `workspace2/planes/index/crates/nudox-index-vocab/tests/vocabulary.rs:80-92`; `workspace2/evidence/index-i0/clean-pass-1.md:33`, `workspace2/evidence/index-i0/clean-pass-2.md:33` |
| Hash work is explicit and bounded | each alias uses existing `ContentId` constructor | add prehash/retry/second constructor | none in source candidate | one existing BLAKE3 stream per explicit call | REPRODUCED | `526ad7a7`; aliases at `workspace2/planes/index/crates/nudox-index-vocab/src/lib.rs:97-102`; constructor at `workspace2/crates/nudox-id/src/content.rs:102-106` |
| Registry, `no_std`, and dependencies remain bounded | full root/index gates and lock audit | duplicate label/new package/unsafe | manager ledger was stale | labels unique; `no_std`/unsafe forbid; no new registry version | REPRODUCED | `526ad7a7`; `workspace2/planes/index/crates/nudox-index-vocab/src/lib.rs:1-3`; `workspace2/crates/nudox-id/src/marker.rs:140-172`; both clean-pass artifacts |
| Future index surface stays absent | exact path/source scan | add query/manifest/backend/I/O type | duplicate marker/unused associated constant existed | no query, manifest, backend, I/O, async, unsafe, SIMD, or future ID | REPRODUCED | `526ad7a7`; independent-review exact four-path scan |

## Mechanical tripwires

The fresh reviewer returned the literal table below. Its raw corrected response and self-check are in
`workspace2/evidence/index-i0/independent-review.md`.

| tripwire | count | exact locations | disposition | law/measurement |
|---|---:|---|---|---|
| panic/unwrap/expect/unreachable | 0 | exact scan of `workspace2/crates/nudox-id/src/marker.rs`, `workspace2/crates/nudox-id/src/lib.rs`, `workspace2/planes/index/crates/nudox-index-vocab/src/lib.rs`, `workspace2/planes/index/crates/nudox-index-vocab/tests/vocabulary.rs` | false-positive | exact scan |
| source-dropping conversion or map_err | 0 | exact scan of `workspace2/crates/nudox-id/src/marker.rs`, `workspace2/crates/nudox-id/src/lib.rs`, `workspace2/planes/index/crates/nudox-index-vocab/src/lib.rs`, `workspace2/planes/index/crates/nudox-index-vocab/tests/vocabulary.rs` | false-positive | exact scan |
| checked-arithmetic sentinel/saturation or operand loss | 3 | `workspace2/planes/index/crates/nudox-index-vocab/tests/vocabulary.rs:12`, `workspace2/planes/index/crates/nudox-index-vocab/tests/vocabulary.rs:15`, `workspace2/planes/index/crates/nudox-index-vocab/tests/vocabulary.rs:55` | required | named derived and raw-boundary test facts; no checked arithmetic |
| dyn/Box/Vec/Arc/Rc | 0 | exact scan of `workspace2/crates/nudox-id/src/marker.rs`, `workspace2/crates/nudox-id/src/lib.rs`, `workspace2/planes/index/crates/nudox-index-vocab/src/lib.rs`, `workspace2/planes/index/crates/nudox-index-vocab/tests/vocabulary.rs` | false-positive | exact scan |
| public tuple fields or positional semantic tuples | 6 | `workspace2/planes/index/crates/nudox-index-vocab/tests/vocabulary.rs:45-50` | required | private two-column test table, named at destructure |
| unit/stateless namespace structs | 0 | exact scan of `workspace2/crates/nudox-id/src/marker.rs`, `workspace2/crates/nudox-id/src/lib.rs`, `workspace2/planes/index/crates/nudox-index-vocab/src/lib.rs`, `workspace2/planes/index/crates/nudox-index-vocab/tests/vocabulary.rs` | false-positive | registry brands are uninhabited enums |
| public local traits and one-implementation delegation | 3 | `workspace2/crates/nudox-id/src/marker.rs:68`, `workspace2/crates/nudox-id/src/marker.rs:74`, `workspace2/planes/index/crates/nudox-index-vocab/src/lib.rs:80` | required | sealed registries have multiple implementations; vocabulary implementations at lines 85-95 |
| one-letter generic parameters | 0 | exact scan of `workspace2/crates/nudox-id/src/marker.rs`, `workspace2/crates/nudox-id/src/lib.rs`, `workspace2/planes/index/crates/nudox-index-vocab/src/lib.rs`, `workspace2/planes/index/crates/nudox-index-vocab/tests/vocabulary.rs` | false-positive | exact scan |
| numeric discriminants/sentinels/offsets/capacities/loop bounds | 24 | `workspace2/crates/nudox-id/src/marker.rs:155,165`; `workspace2/planes/index/crates/nudox-index-vocab/src/lib.rs:36,38,40,42,44,65-69`; `workspace2/planes/index/crates/nudox-index-vocab/tests/vocabulary.rs:11-16,46-50,55` | required; marker offsets cold-only | closed codes and exact boundary/mutation facts |
| test-only Option/discarded results/success-only assertions | 6 | `workspace2/planes/index/crates/nudox-index-vocab/tests/vocabulary.rs:83-89` | required | optimizer barriers execute measured constructors; no `Option` or result discard |
| unsafe/SIMD/allocator/dependency additions | 1 | `workspace2/planes/index/Cargo.toml:12`; `workspace2/planes/index/crates/nudox-index-vocab/Cargo.toml:13` | required | one test-only existing-version allocator harness |
| public item without current consumer and falsifier | 0 | exact scan of `workspace2/crates/nudox-id/src/marker.rs`, `workspace2/crates/nudox-id/src/lib.rs`, `workspace2/planes/index/crates/nudox-index-vocab/src/lib.rs`, `workspace2/planes/index/crates/nudox-index-vocab/tests/vocabulary.rs` | false-positive | every public item has an integration/doctest consumer and falsifier |

## Resource and scope ledger

```text
formatted production Rust LOC: baseline 184 / candidate 304 / delta +120 / cap +230 / reserve 110
  marker.rs: 156 -> 173 (+17)
  nudox-id/lib.rs: 28 -> 29 (+1)
  vocabulary/src/lib.rs: 0 -> 102 (+102)
formatted test Rust LOC: baseline 0 / candidate 92 / delta +92 / cap 120 / reserve 28
allocation site: vocabulary.rs:86-90 / count 0 / bytes 0 / measured closure lifetime /
  rejected allocation owner: none / alternative: caller-owned stack bytes borrowed directly
copies/materializations: no canonical-input copy or retention; each call returns its required 32-byte digest value
logical work/branches: one existing hash stream per explicit construction; one five-arm closed-code decode;
  no retry, scan, prehash, queue, or hidden collection
optimized consumer/control: no release-text or codegen claim in this vocabulary child; explicitly unscored
dependencies/features/exports: one path dependency nudox-id; test-only allocation-counter already locked;
  local nudox-index-vocab is the only package absent from root lock; three domain exports, one closed enum,
  one named error, one sealed two-consumer projection, and two aliases
written-then-rejected churn: manager 118 production/83 test checkpoint; root deleted duplicate family brands
  and unused associated constant, repaired conversion/allocation evidence, and closed at +120/+92
```

## Hostile attempts

```text
owner/witness mixing: Lexical->Exact direct/Into and RootDomain family doctest mutants all failed gate 101
input removal: empty-input ContentId mutant made byte-mutation journey fail, exit 101
invalid tag/offset/length after validation: N/A established by exact four-path scan; this slice defines no format/view
error source and rejected-owner loss: all invalid u8 paths return named original code; no owned rejection path exists
cancellation/progress/reuse: N/A established by exact four-path scan; no mutable source, task, waker, or lease exists
torn/corrupt/restart: N/A established by exact four-path scan; no persistence or I/O exists
disabled diagnostics/bounded export: N/A established by exact four-path scan; no probe/event/adapter exists
simpler std/representation deletion: duplicate wrapper brands and unused associated constant were deleted;
  enum-plus-digest was rejected because callers could forge family/bytes coherence
strongest surviving counterexample: historical manager/Luna task identities and current full-deck calibration
  cannot be reconstructed from code; closure stays OPEN
```

## Clean closure

Focused index tests, three doctests, clippy, docs, and registry-label tests pass at exact source
`526ad7a7`/tree `f50541fe`. Two complete passes each ran 25 commands across root, observability, IR,
compiler, and index workspaces with offline locked resolution and immediate clean status:

- pass one: `workspace2/evidence/index-i0/clean-pass-1.md`; raw-summary SHA-256
  `107d7035dfd510ceaa653997caebeb9b176c3f604c3c6efdb7a92548317c9c7d`;
- pass two: `workspace2/evidence/index-i0/clean-pass-2.md`; raw-summary SHA-256
  `cbf0830385e0dc165bafecd268d6e0654758e8659f17ee6a6409821f96b03e78`.

Raw command logs remained under the two named `/private/tmp/nudox-i0-gate-pass*-71890a1a`
directories through receipt review; every individual log hash and byte length is in the compact
artifacts. The fresh independent review confirms zero blockers/majors. Online registry resolution,
cross-target allocation behavior, historical role task identities, and a current full calibration
deck remain unverified.

Post-gate source edits: none. Post-gate evidence-only edits: this rebuilt receipt, exact review, and
compact gate ledgers. Root concurrently hardened canonical-card custody, arithmetic errors, review
locations, source/receipt separation, integrated-history equivalence, rubric-writer calibration, and
added the candidate-bound gate runner plus cold steward calibration.

Verdict: **OPEN**. Every implementation law is `REPRODUCED`, but stale calibration and incomplete
historical manager/Luna task custody are explicit process blockers. No numeric score or next index
manifest child is authorized.
