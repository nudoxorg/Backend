# P1 canonical-root retracted closure evidence

## Retraction

This packet's `REJECT PROTOTYPE` conclusion is retracted. A Sol cross-check located
the accepted safe typed-slice precedent in `nudox-object-pack`: after explicit schema
validation, `ObjectPackIndex` retains
`&[DirectoryRecord]` via `try_ref_from_bytes_with_elems`, and trusted projection uses
`ObjectRef::from(&row.descriptor)` without a second raw tag conversion. The existing
`DirectoryRecord` embeds the same `ObjectDescriptorWireRecord` required by C1.

The prior C1 UI fixture proves only the absence of one nonexistent unconditional API;
the independent closure reviewer already classified it as a limited falsifier. It cannot
support the broader impossibility claim made below. This packet remains historical
control/review/churn evidence only. P1 is reopened under a new canonical C1a card; it
has no current verdict until that replacement experiment closes.

## Objective and observable terminal

P1 tested whether the current canonical root grammar can make caller-retained bytes
the root authority while preserving a checked borrowed root/locality composition,
caller-reused closure/hydration scratch, and the existing non-forgeable
`VerifiedGeneration`. No `Published` type, stable receipt, or verified-to-published
transition was defined, constructed, or tested.

The C1 terminal was not reached. The safe control C0 remains the only complete vertical:
`GenerationRoot` boxed rows -> checked locality -> caller scratch -> existing
`VerifiedGeneration`. C1 is rejected before production implementation because its
frozen 63-byte record grammar lacks a demonstrated safe, O(1)-metadata, infallible trusted
object projection: `SchemaId` is a closed `TryFromBytes` enum. Retaining a decoded schema
for each row breaks the O(1) view target, while rechecking it during traversal breaks the
trusted-view law. Independently, parent keys rather than parent coordinates force binary
searches per ancestor, and the existing concrete selection types cannot consume a source
enum without an unbudgeted representation rewrite. The isolated compile-fail case is only
one API-absence sentinel in that conclusion, not a total proof against every possible route.

## Baseline, custody, and dirty proof

| Item | Exact evidence |
|---|---|
| Source candidate | `/private/tmp/nudox-orchestra`, `44c22154fd5238e4769562590420371979306050`, clean before the manager card |
| Manager worktree | `/private/tmp/nudox-prototype-canonical-root-hydration` |
| Manager branch/range | `codex/prototype-canonical-root-hydration`, `44c22154..7d02f1af` before closure evidence |
| Control card commits | `18f52755` initial card; `7d02f1af` calibrated replacement |
| Rejected C1 branch | `codex/prototype-canonical-root-hydration-c1-falsifier`, `7d02f1af..9e584d23`, worktree `/private/tmp/nudox-prototype-canonical-root-hydration-c1-falsifier` |
| Shared branch action | none: no edit, merge, rebase, or cherry-pick of `orchestra-shared` |
| Manager-source paths | evidence only; no Rust production, manifest, dependency, active identity, operation, or P2 path changed |

The source and manager worktrees were clean when recorded. The C1 branch was clean before
and after its isolated commit. Exact source digests and formatted baseline LOC are retained
in `P1_CANONICAL_ROOT_DIGEST_LOC_LEDGER.md`.

## Explicit model/task transport evidence

| Role | Explicit request | Returned task identity | Exact disposition |
|---|---|---|---|
| Cold mechanical reader | `model=gpt-5.6-luna`, `fork_turns=none` | `/root/p1_canonical_root_manager/p1_luna_cold_reader` | Reported API, decoder, work, and compile-fail ambiguity; reread calibrated card at `7d02f1af` |
| Independent reviewer | `model=gpt-5.6-terra`, `fork_turns=none` | `/root/p1_canonical_root_manager/p1_terra_preedit_review` | Read-only pre-edit reviews of both card digests and a separate candidate closure review |
| C1 mechanical falsifier | `model=gpt-5.6-luna`, `fork_turns=none` | `/root/p1_canonical_root_manager/p1_luna_c1_falsifier` | Isolated, committed trybuild falsifier `9e584d23`; never integrated |

The raw successful spawn transport returned those exact task identities. The manager issued the
named model overrides in each call; no inherited-model role was used. Reviewer and Luna work
were separated; the reviewer had no edit authority.

## Candidate/control table

| Candidate | Representation and measured/inspected authority | Outcome and rollback |
|---|---|---|
| C0 control | `GenerationRoot` retains one `Box<[RootRow]>`; `RootRow` source assertion is 64 B. Existing writer emits `8 + 63*N` canonical bytes. | Retained as the only complete safe vertical. Rollback is no action. |
| C1 rejected | Caller-owned exact bytes + proposed `ValidatedRoot` O(1) view and private paired root/locality source. | Rejected. No manager integration; the isolated falsifier commit remains on `codex/prototype-canonical-root-hydration-c1-falsifier`. |
| C2 deferred | Mapped/leased immutable byte owner adapter. | Not started: no C1 borrowed core exists to adapt. |
| C3 deferred | Self-referential escaping dependent view. | Not started: no escaping-view consumer, no measured copy/revalidation win, and no Miri premise. |

## Raw correctness and negative evidence

The baseline focused library gate `cargo test -p nudox-root -p nudox-hydration --lib` passed
32 tests (22 root, 10 hydration). It includes current permutation canonicality, hierarchy
rejections, locality-only identity stability, selection work, 100k root control, complete
closure verification, and verified-fact boundary tests.

The C1 worker commit adds only these 19 formatted test/UI lines:

| Path | Lines | Evidence |
|---|---:|---|
| `crates/nudox-hydration/tests/ui/object_descriptor_unchecked_reborrow.rs` | 9 | A validated `ObjectDescriptorWireRecord` cannot call an unconditional `ref_from_bytes` reborrow. |
| `crates/nudox-hydration/tests/ui/object_descriptor_unchecked_reborrow.stderr` | 10 | Exact compiler error `E0599`; it names `try_ref_from_bytes` as the only safe alternative. |

Manager reproduction of `cargo test -p nudox-hydration --test compile_fail` passed the old
non-forgeable-`VerifiedGeneration` UI case and the C1 reborrow UI case. The closure reviewer
found no blocker or major in this two-file candidate, but recorded MINOR F1: it proves only
that this exact unchecked `ref_from_bytes` API is absent; a manual parser, another checked
reparse, or a future helper remains outside its scope. This is therefore a limited falsifier,
not a claim that one missing method proves every possible representation.

The hostile reviewer found these blocking facts against the calibrated C1 card:

1. Existing `CanonicalRows`, `SelectedClosure`, and `GenerationView` are concrete over
   `&GenerationRoot`; a private source enum alone cannot reach closure, ordinal retention,
   `Need::bind`, plan, and verification.
2. The decoder needs its own exact count/extent/error/cycle contract; reusing `RootBuildError`
   would lie about allocation/build provenance.
3. C1 parent-key traversal incurs a binary search per ancestor edge rather than C0's stored
   compact coordinate dereference. Preserving old edge counters would hide the additional work.
4. A public C1 UI/integration/allocation evidence suite and its full LOC budget were missing.

The strongest counterexample combines the source facts and the compiled falsifier: a C1
validator can prove a schema cell once, but safe Rust cannot subsequently produce an unconditionally
valid unaligned `ObjectDescriptorWireRecord` borrow. Calling `try_ref_from_bytes` again is the
compiler-suggested route, but is precisely the forbidden trusted-path revalidation. Deleting schema
validation is the direct input-removal mutant: it would accept an unknown closed tag and turns the
error proof into a latent invalid enum/redecode problem, so it is not a permitted escape.

No constant or input-removal mutation of a C1 production implementation exists because no C1
production implementation passed pre-edit authority. That absence is recorded as `UNVERIFIED`,
not counted as green evidence.

## Resources and measured limits

| Claim | C0 control | C1 result |
|---|---|---|
| Retained root owner/backing | one boxed `64*N` native-row backing; pointer depth one | target `8 + 63*N` caller backing and O(1) metadata was not safely realizable |
| Construction peak | builder records input capacity plus packed row capacity, and drops input before hierarchy scratch | no C1 owner transfer/construction path implemented |
| Validation allocation/copy | C0 construction is fallible; locality view borrows caller artifact | C1 validator was rejected before allocation measurement; a safe view would either revalidate or retain per-row typed state |
| Lookup/range/ancestor work | current binary key lookup, O(N) scan, and `projected_rows`/`ancestor_edges` counters | parent-key C1 would add O(log N) comparisons per ancestor; no acceptable whole-consumer threshold exists |
| Release text/dependency graph | unchanged from C0 | C1 candidate changes only 19 test lines, no production text or dependencies |

The C1 compile fixture allocates nothing at runtime because it does not run; it is compile-fail
evidence only. It is not used as an allocation measurement. Platform allocator/copy/latency,
100k C1 density, pointer containment, owner drop, and a C1 end-to-end hydration run are all
`UNVERIFIED` because C1 failed before a valid core could exist.

## Reviewer tripwire evidence

The read-only reviewer scanned the calibrated card and ledger before source edits. Its literal
table counted: panic/unwrap/expect/unreachable 0; source-dropping conversion 0; ambiguous raw
conversion 1 (the under-specified proposed `TryFrom`); checked-arithmetic omission 3;
`dyn`/`Box`/`Vec`/`Arc` 3 contextual control mentions; public tuple 0; stateless namespace 0;
public local trait 0; one-letter generic 0; numeric grammar/budget terms 11; test-only
success-only assertions 0; unsafe/SIMD/allocator/dependency additions 0; unconsumed public
items 2. It raised BLOCKER B1/B2 and MAJOR M1/M2/M3, then cleared source-manifest/P2/unsafe/
dependency contamination. Its C1-branch closure table counted one deliberate non-executed
`unwrap` and one deliberate discarded result in the compile-fail fixture; every other
tripwire row was zero, and it cleared all production/P2/dependency/API/unsafe/SIMD/allocator
contamination.

## Changed/deleted surface and LOC

Manager branch through `7d02f1af`: 181 formatted evidence-only added lines and 6 evidence-line
deletions across the card and ledger. No production, test, lab, manifest, or dependency line changed.
Rejected C1 branch: 19 formatted test/UI lines added, 0 production/lab/dependency lines, 0 deleted.
No public API was added or deleted. C0 source/test LOC are preserved in the baseline ledger.

## Decisions, remaining gaps, and proposed learning

Accepted for evidence only: preserve C0 and the isolated C1 compile falsifier. Rejected: adopting a
canonical-byte-only root under the existing 63-byte grammar, safe Rust, O(1) view metadata, and the
no-redecode trusted-traversal law. Deferred: C2 and C3. Rollback is simply no cherry-pick from
`9e584d23`; delete no evidence branch.

UNVERIFIED: any grammar change; a typed borrowed projection that does not revalidate; C1 allocation,
copy, latency, owner-drop, 0/1/100k/density, pointer containment, lifetime/UI forgery of a real C1
view, complete hydration, release text, dependency graph beyond no-change diff, x86/Miri, mapped or
leased backing, and self-reference. These are explicit non-results, not implementation defects.

Proposed reusable skill findings (not applied):

1. Before a borrowed canonical-view card, enumerate every closed byte field and prove the safe
   trusted projection route; if one needs `TryFromBytes` on each read, stop before designing a
   source-neutral consumer layer.
2. A root-storage candidate that changes the source type must inventory every concrete iterator,
   selected view, and downstream borrowing type before assigning a cross-file LOC budget.

## Minimal future integration card

Prerequisites: the active typed identity integrity work must close; a separate root grammar/typed
projection design must prove safe infallible borrowed descriptors without per-row sidecars or trusted
revalidation; and it must name an acceptable parent-coordinate/range work tradeoff. Paths must be
disjoint from active identity paths and P2 durable publication. First public terminal: a canonical
root byte artifact validates into a coherent borrowed root/locality view and completes one
caller-scratch hydration to existing `VerifiedGeneration`, with a downstream forgery UI proof.
No branch or evidence here may be merged; P2 remains the exclusive prerequisite for later durable
publication authority.

## Historical verdict

The following historical verdict is retracted by the correction above: `REJECT PROTOTYPE`.
