# Frozen resource and fault ledger

This ledger is a rationale-free pre-edit test contract. Paths and test names are adapter-owned future
tests; the Sol-owned journey remains unchanged. `q` denotes `PublicationLimits` queue capacity, `g`
its group capacity, and `F` the accepted `JOURNAL_FRAME_BYTES` constant.

## Resource ledger

| Resource | Fixed bound | Required observation |
| --- | --- | --- |
| admitted commands | `q` | `tests/publication_admission.rs::full_item_limit_returns_verified_input_unchanged` observes zero, one, exactly `q`, and `q + 1`. |
| queued frame bytes | `q * F` | `tests/publication_admission.rs::full_byte_limit_preserves_credit_and_input` independently exhausts byte credit and observes no credit change on rejection. |
| unresolved waiters | `q` | `tests/publication_admission.rs::full_waiter_limit_preserves_credit_and_input` observes one terminal waiter per accepted submission and no detached waiter after every terminal path. |
| terminal receipt slots | `q` | `tests/publication_admission.rs::full_receipt_limit_preserves_credit_and_input` observes one exact terminal result per accepted submission and no duplicate fan-out. |
| grouped commands | `min(q, g)` | `tests/publication_group.rs::group_high_water_never_exceeds_group_capacity` observes exact ordered membership. |
| reusable group frame bytes | `g * F` | `tests/publication_group.rs::reused_group_storage_has_fixed_frame_extent` observes one reusable bounded extent, no retained event collection after sync. |
| immutable fact/head buffers | one fixed encoded fact plus one fixed encoded head | `tests/publication_resources.rs::publication_and_head_buffers_have_declared_fixed_extent` records declared byte counts, retained owners, and copies. |
| producer file/probe ownership | zero | `tests/publication_admission.rs::producer_never_observes_file_or_probe` uses an owner-blocking probe and asserts only the owner thread invokes D0 file/probe work. |

The candidate resource measurement is
`tests/publication_resources.rs::warmed_submission_group_and_reopen_resource_delta`. After warm-up it
records allocation count/current/max/bytes, live retained command/waiter/receipt/group counts, logical
frame writes, file syncs, fact/head writes, head syncs, rename-or-CAS operations, directory syncs, and
copies for one normal submission, one coherent duplicate, one conflict, one cancellation, and one
reopen. It compares every result with the accepted warmed `FileJournal::append` control and runs
`cargo tree --manifest-path adapters/durable-journal/Cargo.toml --locked --offline`; a new normal
dependency is failure.

## Exact law-to-falsifier schedule

| ID | Focused test / measurement | Forced weakened behavior that must fail |
| --- | --- | --- |
| DP-01 | `tests/publication_admission.rs::full_each_independent_limit_preserves_input_and_accounting` | consume returned verified input or alter any credit on the `q + 1` admission. |
| DP-02 | `tests/publication_admission.rs::single_owner_and_all_high_water_marks_are_bounded` | permit producer file/probe work or exceed one item/byte/waiter/receipt limit. |
| DP-03 | `tests/publication_duplicates.rs::coherent_duplicate_is_idempotent_and_one_fact_change_conflicts` | append a duplicate or advance a head after exactly one changed fact. |
| DP-04 | `tests/publication_cancellation.rs::each_admitted_queued_grouped_synced_pre_head_post_head_boundary_conserves_credits` | drop/release twice at one named boundary or make a cancelled prefix visible after reopen. |
| DP-05 | `tests/publication_terminals.rs::receiver_loss_and_poison_fan_out_exact_causal_terminal` | drop a receiver, inject group write/sync failure, erase its source, or strand a receipt. |
| DP-06 | `tests/publication_terminals.rs::shutdown_composes_primary_and_cleanup_sources_once` | hide either simultaneous owner or cleanup/join failure. |
| DP-07 | `tests/publication_group.rs::one_synced_group_reduces_to_exact_ordered_frames_and_receipts` | omit/duplicate/reorder one grouped frame or issue a receipt before the single group sync. |
| DP-08 | `tests/publication_journal_faults.rs::every_position_write_and_sync_fault_has_no_failed_effect` | accept a short/failed write or sync and issue a receipt/effect. |
| DP-09 | `tests/publication_facts.rs::fact_requires_named_stable_receipt_and_valid_checksum` | form a record before stable bytes or accept a stale/mutated receipt/record checksum. |
| DP-10 | `tests/publication_head_faults.rs::visible_head_always_names_existing_checksum_valid_fact` | publish head before fact durability or accept a failed temp/head write/sync/rename-or-CAS/directory sync. |
| DP-11 | `tests/publication_head_faults.rs::duplicate_head_is_byte_identical_and_conflict_preserves_old_head` | overwrite a visible head on conflict or perturb a coherent duplicate. |
| DP-12 | `tests/publication_recovery.rs::each_crash_prefix_reduces_journal_and_validates_fact_then_head` | trust shadow state, a torn fact, checksum-bad fact, or head whose journal receipt is absent. |
| DP-13 | `tests/publication_pending.rs::registration_rechecks_and_every_terminal_wakes_once` | strand a pending observer after ready/drop/cancel; omitted only if no pending public API is introduced. |
| DP-14 | `contracts/published-generation.md` doctests and `tests/publication_recovery.rs::reopen_rejects_mixed_head_and_immutable_facts` | construct public literal or mix A publication facts with B verified facts. |
| DP-15 | `tests/publication_resources.rs::warmed_submission_group_and_reopen_resource_delta` | add unmeasured allocation, retained collection, copy, syscall, or normal dependency. |

## Physical-fault and crash-prefix inventory

Each named injection reports an exact source-bearing typed error and reopens with an independent
`FileJournal` reduction plus immutable-publication/head validation:

```text
journal: seek/position, frame write length/error, frame sync
immutable publication: create, full/short write, file sync, parent directory sync
head: temp create, full/short write, temp sync, rename-or-CAS, parent directory sync
reopen prefixes: before journal append; after frame write; after frame sync; after fact write;
after fact sync; after temp write; after temp sync; after rename-or-CAS; after directory sync
```

All injections run both one submission and a group of `g` where `g > 1`; each asserts the returned
receipt set, accounting image, visible-head bytes, immutable-fact bytes, and independent reopen result.
