# P1 C1a canonical-root independent Sol review

## Verdict and exact terminal

**PROMOTE FOR FUTURE INTEGRATION REVIEW** for C1a only.

The candidate proves one narrow terminal: caller-owned canonical root bytes can be validated once
into a non-forgeable borrowed witness, paired with an independently validated locality artifact,
then projected by exact lookup and full scan without a retained root-row owner, revalidation,
copy, or post-setup allocation. It does not prove C1b selection/hydration authority or any P2
publication behavior.

## Custody and reviewed range

| Item | Exact value |
|---|---|
| Clean source candidate | `44c22154fd5238e4769562590420371979306050` |
| Manager branch/worktree | `codex/prototype-canonical-root-hydration`; `/private/tmp/nudox-prototype-canonical-root-hydration` |
| Code candidate | `316abe637151222475108c87468e1a28e7a1c1d7` |
| Manager closure | `a5e363a85275c50ae12b43d10f8a4f04d6a4ecc6` |
| Sol review branch/worktree | `codex/prototype-canonical-root-hydration-sol-review`; `/private/tmp/nudox-p1-sol-review` |
| Detached mutant worktree | `/private/tmp/nudox-p1-sol-mutants` at `316abe63`, restored clean after attacks |
| Shared branch | untouched; no merge, rebase, or integration action performed |

The source candidate and all three review worktrees were clean at their relevant boundaries. The
manager closure changes only evidence after `316abe63`; the independently reproduced code is
exactly the code candidate.

## Representation decision and simpler alternatives

The retained C0 control remains `Box<[RootRow<DomainTag>]>` at 64 bytes per row. C1a borrows the
existing `8 + 63*N` canonical bytes as one checked `&[RootWireRecord]` and retains only O(1)
metadata. Validation uses one temporary `Vec<u32>` of parent coordinates. `u32::MAX` is both the
root and destructive-DONE sentinel, so the same word supplies coordinate and memoization state.

| Alternative | 100k logical scratch | Whole-operation consequence | Sol result |
|---|---:|---|---|
| No-scratch Floyd from every row | 0 | deep chain is quadratic; roughly 2.5 billion slow advances even when the fast cursor terminates each walk early | rejected |
| `HashMap`/`HashSet` visitation | allocator- and load-factor-dependent, much larger | hashing, more ownership, dependencies on bucket behavior, and no better authority boundary | rejected |
| Parent coordinates plus separate byte state | 500,000 bytes | simple, but spends an avoidable 100,000 bytes | rejected |
| Parent coordinates plus packed 2-bit state | 425,000 bytes | saves most byte-state cost but adds bit access and a second logical region | rejected |
| Packed 2-bit state only | 25,000 bytes | redoes parent searches during traversal/cleanup and complicates the hot validator for no consumer-visible win | rejected |
| Destructive parent-coordinate memoization | **400,000 bytes** | one allocation, at most two binary-search passes, linear resolved-edge walk, no retained scratch | selected |

The selected scratch is 6.35% of the 6,300,008-byte 100k canonical input, is released before the
witness returns, and saves 25,000 bytes over the strongest split-vector draft. This is the best
observed balance, not a universal optimum: the packed-state-only alternative remains the memory
minimum if a future target values 375,000 transient bytes more than repeated searches and code
complexity.

## Independent reproduced gates

The Sol worktree reproduced, rather than inherited, these results:

- root workspace formatting: pass;
- strict workspace/all-target Clippy with warnings denied: pass;
- full workspace tests and doctests: pass, including all four hydration compile-fail cases;
- explicit rustfmt checks for the three new UI sources: pass;
- layout-lab formatting and strict release Clippy for `p1-canonical-root-control`: pass;
- fresh release replay: 25 lines, 24 measured rows, zero invariant violations, and an empty diff
  against the tracked TSV after removing only `elapsed_ns`.

The release replay on `aarch64-macos-64`, Rust 1.97.1, observed:

| Workload | validation alloc/dealloc/requested/peak | warmed get+scan |
|---|---|---|
| zero | 0 / 0 / 0 / 0 | 0 / 0 / 0 / 0 |
| one | 1 / 1 / 4 / 4 | 0 / 0 / 0 / 0 |
| 100k shallow | 1 / 1 / 400000 / 400000 | 0 / 0 / 0 / 0 |
| 100k deep | 1 / 1 / 400000 / 400000 | 0 / 0 / 0 / 0 |

Every successful allocator row says `allocator_failure_source=UNVERIFIED`; no OOM source was
invented. Source and public pointer-containment assertions, rather than allocation counts, prove
that the retained root and trusted projections borrow the caller's bytes.

## Hostile authority and totality review

The wire grammar has one owner in `encode.rs`; writer and reader share the same private header and
63-byte row types. Validation order is exact extent, raw descriptor provenance, typed-slice
retention, key order, parent marker, absent-parent key, missing-parent resolution, fallible scratch
reservation, and cycle detection. Trusted `get` and scan call `ObjectRef::from` only after that
proof and use coordinates established by binary search or a preceding length guard.

No new production `unsafe`, panic macro, `unwrap`, `expect`, `todo`, `unimplemented`, `DerefMut`,
root-row copy owner, manifest dependency, P2 publication edit, or hydration authority adapter is
present. `BorrowedGenerationView` is intentionally distinct from `GenerationView` and cannot bind
`Need`; `None` from `get` means only key absence, while locality failures remain explicit errors.

The compile-fail witnesses reject private root/view literals, mutation of root/locality/view facts,
and root or view lifetime escape. Runtime matrices mutate count/extent, descriptor schema, key
order, parent marker/key, missing parents, cycles, generation identity, root count, and locality
placement. Existing descriptor and locality suites cover their remaining tag vocabularies and
short-tail/ordering cases.

Three input-removal mutants were applied independently to a detached code checkout:

1. Removing key-order validation made the malformed-priority public test fail, observing
   `MissingParent` where `KeyOrder` was required.
2. Removing parent-marker validation made the same public test fail, observing `MissingParent`
   where `ParentPresent` was required.
3. Removing generation-identity pairing made the public locality-pairing test fail because a
   foreign root was admitted.

The checkout was restored to a zero-diff state after each sequence. These attacks demonstrate that
the public tests are sensitive to removal of the trusted-coordinate and authority checks rather
than only to output formatting.

## Text, dependencies, roles, and rollback

Production root source is baseline-relative +599 formatted LOC; `root_view.rs` is 576 lines. The
root public test is 335 lines, the three C1a UI sources total 59 lines, and the lab binary is 283
lines. No manifest changed.

The explicit Terra/xhigh manager was `/root/p1_canonical_root_manager` (`gpt-5.6-terra`). Named Luna implementation and
mechanical tasks and independent Terra reviewer tasks are recorded in
`P1_CANONICAL_ROOT_C1A_CLOSURE.md`; the terminal reviewer was
`p1_terra_c1a_terminal_review` (`gpt-5.6-terra`) and passed the exact final worker range before
manager integration. Rejected Luna checkpoints remain on isolated branches and are not hidden.

Rollback is to leave shared state untouched, as it is now. On any future integration branch, the
candidate range is `59459baf^..316abe63`; reverting that range restores C0 because C0 was never
deleted or adapted to depend on C1a.

## Remaining UNVERIFIED and future card

UNVERIFIED: injected allocator-OOM behavior and exact source propagation; Linux, Windows, other
macOS, 32-bit, and other toolchains/allocators; performance beyond the recorded host; C1b
caller-reused selection/hydration scratch and complete-closure authority; every P2 publication or
durable-receipt transition.

The smallest future shared-branch card starts from current shared state, replays C1a against that
state, and gives one ordinary external consumer reusable selection and hydration scratch. Its first
public terminal must reach the existing non-forgeable `VerifiedGeneration` only after complete
closure, while proving zero post-setup allocation. It must not modify P2 publication types or treat
C1a facts as publishable. This review grants no merge authority.

## Proposed skill findings

1. Require destructive-memoization candidates whenever a scratch word has an unreachable sentinel;
   it can remove a parallel state array without weakening safe indexing.
2. Require at least one guard-removal mutant for every trusted projection boundary; compile-fail
   authority proof and runtime mutation proof catch different regressions.
3. Report transient scratch as both exact bytes and percentage of the complete consumer input; the
   pair makes memory/complexity tradeoffs easier to judge.
