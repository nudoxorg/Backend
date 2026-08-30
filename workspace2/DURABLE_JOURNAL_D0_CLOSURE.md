# Durable journal D0 integration closure

This closure accepts the smallest physical durability substrate and leaves the asynchronous
publication heart open. The integrated adapter is a blocking, single-owner file journal. It does
not claim MPSC admission, group commit, a CAS publication head, or remote durability.

## Integrated mechanism

- fixed typed header and frame records with complete-frame checksums;
- exclusive physical ownership before validation, replay, or receipt issuance;
- file sync before `StableReceipt`, plus parent-directory sync before a newly created journal escapes;
- exact poison and reopen/reconciliation semantics after uncertain write or sync outcomes;
- streaming replay with fixed frame storage and zero retained-record allocations;
- incomplete-tail truncation followed by repair sync; and
- exact source-bearing errors for physical step, sequence, offset, decode, and reduction failures.

## Salvage ledger

| Prototype property | Integrated disposition |
| --- | --- |
| typed fixed wire grammar and checksum | retained and centralized in the format authority |
| stable receipt after sync | retained with private construction and readable typed facts |
| failure injection at every write prefix | retained; no receipt escapes and the original source survives |
| crash/reopen prefix proof | retained against the independent workflow reducer |
| checksum mutation proof | restored at the public file boundary after compaction accidentally removed it |
| accumulated in-memory replay log | removed; a fixed-frame iterator feeds streaming replay |
| repeated test fixture/error boilerplate | consolidated without weakening negative observations |
| async queue and group commit claims | not integrated; the current code never implemented those mechanisms |

The compacting pass was rejected twice until it restored physical directory durability, exclusive
ownership, fragmented-write coverage, and the black-box checksum-enforcement proof. Code size was not
allowed to erase a property.

## Reproduced evidence

- seven unit tests, one isolated allocation test, and eight public integration tests pass;
- the stable-receipt construction compile-fail test passes;
- removing checksum rejection from `FileJournal::open` makes the public mutation test fail;
- every workflow-record byte is covered at the public file boundary;
- the adapter passes formatting, warnings-denied Clippy, documentation, and `wasm32-wasip2` checking;
- replay of 64 records reports zero allocations after fixture construction.

## Remaining child capabilities

1. Add bounded MPSC submission and reusable group-commit storage around this single-owner file
   authority without sharing the file or probes with producers.
2. Map one physical sync outcome to exact per-submission receipts, cancellation, poison, and shutdown.
3. Publish immutable objects before a compact CAS head and allow only a stable receipt to release the
   published typestate.

