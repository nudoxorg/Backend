# Verified borrowed operation-binding checkpoint

State: implementation and focused manager reproduction only. This checkpoint is not a source-isolated review, a resource closure, or a capability-closure claim.

## Integrated source identity

- Operation implementation commit: `c058fb25d0e03e6c6bf3f394085bfae14e654803`, tree `9a258106ccca7e4bac5b500b599081958f6f26b9`.
- Sol prerequisite commit: `0484643b` (`nudox-object-pack` verifies a selected body with the pack borrow lifetime; its owning regression is included), tree `90fcd90ebabb58d40433a8fd97ac5db1e03163ff`.
- Sol chief-only correction: `52170327` (explicit `u64` type at the frozen journey's byte sum), tree `7dbbefa2de77f024f1f2e129eef2f65260a4def1`.
- Sol semantic correction: `83c9c2ba1b6570bbb1be1db1fea10e2fed3bae6b` changes a promised locality row admitted by a sealed complete-generation proof into resident-ready output and narrows the bound provider to generation/object/provenance facts. Final integrated candidate tree: `faeb19e4dca41106b580d46fd4813d69d129c007`.
- The chief, object-pack, and promised-presence corrections are Sol-owned. Terra neither edited nor staged them.

## Worker custody

The frozen card is `cards/luna-operation-1.md` (SHA-256 `e29cf515f2fe98455285c2f72fab8f939759d8dad8703d6fb733e68694195b10`), source baseline `4fdbe439b0d1f29ed5238d1a31cc505b47d5f327`, tree `a21ee93625d9be94243c4e47c6451a969d4103ec`.

1. `/root/canonical_byte_local_closure_terra/luna_operation_checkpoint_1`: registered `nudox_luna_implementer`, direct outer collaboration, `fork_turns="none"`, requested model `gpt-5.6-luna`, requested effort `max`, config `.codex/agents/nudox-luna-implementer.toml`, checkout `/Users/mileswirht/.config/codex/worktrees/ab6a/backend`. It returned no runtime/source-analysis/gate/commit receipt after bounded requests. After interruption, one authorized late manifest-only write (promotion of the existing hydration dev edge) was found and preserved. It is not an admissible worker result.
2. `/root/canonical_byte_local_closure_terra/luna_operation_checkpoint_2`: same registered configuration, card, baseline, and checkout. It returned no source-analysis receipt and made no source write during its single bounded retry window; it was interrupted. It is not an admissible worker result.

Neither direct worker supplied an effective-sandbox event; the index records that absence. Terra integrated the visible authorized manifest line, independently implemented the narrow operation surface, and committed `c058fb25`. This honestly preserves attempted Luna/max custody without attributing implementation or gates to either silent worker.

## Public terminal and retained mechanism

`LocalObjectProvider::bind_verified(&BorrowedGenerationView, EntryKey, &VerifiedGeneration)` is the sole generation equality boundary for the bound operation: stale `verified.pinned_root` returns `LocalObjectError::StaleGeneration { expected: view.id, observed: verified.pinned_root }` before any key lookup. A matching generation with no selected row returns `LocalObjectError::MissingKey { generation: view.id, key }`.

Success returns the distinct `BoundLocalObjectProvider`. It owns only copied, already-projected generation, object, and provenance facts; it does not retain the borrowed root/locality view, canonical backing, predicate, store, body, row, or verified witness. Its inherent `start()` takes no request, so no later generation/object comparison is available. Legacy `LocalObjectProvider` plus trait `Provider::start(PinnedObjectRequest)` remains unchanged. The Sol-authorized existing hydration dev edge is promoted to the operation production dependency solely to name sealed `VerifiedGeneration`; no other dependency or manifest change was made.

Resident and overlay selections retain one batch with their existing provenance, one complete terminal, then fused finish. A promised locality selection becomes resident-ready only after this sealed complete-generation proof has been admitted; the original locality artifact is not mutated and no store/body/fetch owner or second verification is added. This decision is covered by the Sol-owned semantic correction and owning operation test.

## Reproduced focused faults and gates

- Owning operation tests assert stale-before-missing-key error priority and exact operands, exact matching-generation missing-key operands, reuse of one verified witness over two legal keys, no-request bound starts, resident/overlay provenance, sealed promised-to-resident readiness, and one-terminal/fused completion.
- `RUSTC_WRAPPER= cargo test -p nudox-operation --locked --offline`: passed 4 unit tests; dormant chief target had 0 tests without its cfg.
- `RUSTC_WRAPPER= cargo clippy -p nudox-operation --all-targets --locked --offline -- -D warnings`: passed after retaining a documented `result_large_err` allowance for the pre-existing descriptor-rich public typed error; boxing is forbidden by this capability.
- `rustfmt --edition 2024 --check crates/nudox-operation/src/pinned_object.rs crates/nudox-operation/src/lib.rs`: passed.
- Before the Sol semantic correction, Nix quality-shell operation commands repeatedly stalled in flake evaluation after `fetching git input ...` without a child compiler or raw compiler result; the wrapper-unset direct commands above are retained as their honest manager receipts. Sol then reran the final semantic correction under the Nix quality shell: operation tests and clippy with `-D warnings` passed, and the frozen chief passed 1/1.
- After Sol's two prerequisite commits, the unchanged frozen chief command with wrapper unset passed one journey: `RUSTFLAGS='--cfg canonical_byte_local_closure_red --check-cfg=cfg(canonical_byte_local_closure_red)' cargo test -p nudox-operation --test canonical_byte_local_closure --locked --offline`.
- Sol separately recorded full `nudox-object-pack` tests/clippy and the same frozen chief as green for its prerequisite commits.

## Remaining counterexample and uncertainty

The strongest remaining counterexample is not an API compile failure: absent independent source-isolated hostile review, a later refactor could move the stale comparison into the bound run or retain a new hidden owner without the current focused tests proving every resource law. CBC-04, the 1/100,000 resource campaign, mutation campaign, and the mandated source-isolated review remain open. No allocation, layout, pointer-transfer, store-admission, or full-workspace claim is made by this operation checkpoint.
