# P2 control A: rejected Luna builder checkpoint

## Custody and explicit task proof

The builder checkpoint started from manager commit `7cdfefa2` after the final clear card at
`e67b6ec1` / SHA-256 `a9d97dd625dba2ad2ff068472b5f5cf1ccd03e21df993c06cac8f819ea15bb2b`.
It was `/root/p2_build_manager/control_a_luna_reader_r3`, explicitly
`gpt-5.6-luna` with `fork_turns=none`, reactivated for the authorized build turn. It was limited
to the twelve adapter paths in `CONTROL_A_CARD.md` and was required to patch/commit or return a
concrete blocker. It patched its owned adapter path, did not stage or commit a red candidate, and
returned the concrete incompleteness blocker below.

## Written-then-rejected churn

The untracked attempt created only:

| Path | Physical lines | Result |
|---|---:|---|
| `adapters/durable-publication-heart/Cargo.toml` | 21 | untracked, incomplete checkpoint |
| `adapters/durable-publication-heart/Cargo.lock` | 504 | generated nested lock, untracked |
| `adapters/durable-publication-heart/src/lib.rs` | 3 | untracked, incomplete checkpoint |
| `adapters/durable-publication-heart/src/journal.rs` | 25 compressed / 412 normally formatted | untracked, incomplete checkpoint |
| `adapters/durable-publication-heart/target/**` | 1,043 files | generated build state, untracked |

The normally formatted journal alone was 412 lines; together with the 21-line manifest and 3-line
library, production was already 436 lines, above the card’s binding 432-line forecast before any
test/fault/UI path existed. The source was not normally formatted. No source or test path was
staged, committed, or accepted.

## Exact rejection findings

1. `src/journal.rs:20-25` had no feature-gated `FaultScript` or
   `FileJournal::open_or_create_with_fault`, so neither feature-on persistent-byte fault could be
   compiled.
2. Header validation omitted physical version, header bytes, workflow bytes, and frame bytes in
   the declared priority order.
3. Frame decode discarded its actual `WorkflowRecordError` and manufactured
   `UnknownVersion { observed: 0 }`.
4. The attempted source used compressed statements, `unwrap_or` fallbacks, and source scans found
   no authorized test, UI fixture, deterministic short-write schedule, allocation harness, feature
   projection, or retained-log audit.
5. The 0..=92 error schedule, directory/tail repair source-and-byte-image tests, independent
   oracle, compile-fail fixtures, clean transcript, and exact dependency/surface evidence were all
   absent.

The builder did run `cargo check --all-targets` (passing only with warnings) and a feature check
(passing only because the required feature API/tests did not exist). `cargo fmt --check` failed.
`cargo test --all-targets` was not accepted as a card gate. The manager reproduced the unformatted
source, physical/normal-format LOC, absent test paths, and untracked generated state.

## Decision

**Reject this checkpoint.** It is not a candidate and has no commit. The only permitted next Luna
turn is deletion-first cleanup of the exact untracked adapter attempt and its generated target
directory; no implementation repair is authorized from this incomplete red draft. The deletion
ledger and final Control A decision remain manager work.
