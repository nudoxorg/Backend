# End-to-end runtime audit

Harness: `src/main.rs`; raw output: `results.tsv`. It uses the public
`RemoteRuntime::new` / `Runtime::split`, producer `Admission::admit`, owner
`Owner::execute_next`, and `Owner::poll_terminal`. Five runs are made per cell;
the reported time is the median. Each cell admits and completes 20,000 items,
with 1/2/4/8 producer threads and 0/64/4096-byte payloads. Payload execution
does a checksum loop over the retained bytes. The 64-slot runtime is intentionally
at the public bitmap limit. `rejected_slots` counts retry attempts observed by
the harness; all final structural metrics are zeroed after terminal drain.

Headline medians (ns for 20k items; approximate Mops/s = 20,000,000 / ns):

| policy | producers | 0 B | 64 B | 4096 B |
|---|---:|---:|---:|---:|
| default | 1 | 1,181,583 (16.9) | 5,526,625 (3.62) | 6,293,041 (3.18) |
| default | 2 | 3,305,500 (6.05) | 5,520,083 (3.62) | 8,830,500 (2.26) |
| default | 4 | 2,894,208 (6.91) | 12,603,375 (1.59) | 8,192,083 (2.44) |
| default | 8 | 5,941,167 (3.37) | 11,356,083 (1.76) | 11,301,167 (1.77) |
| AtomicAccounting | 1 | 1,205,375 (16.6) | 5,920,500 (3.38) | 8,026,625 (2.49) |
| AtomicAccounting | 2 | 5,429,833 (3.68) | 8,947,917 (2.24) | 12,702,500 (1.57) |
| AtomicAccounting | 4 | 4,515,250 (4.43) | 5,770,875 (3.47) | 17,984,041 (1.11) |
| AtomicAccounting | 8 | 6,513,750 (3.07) | 6,093,291 (3.28) | 7,205,417 (2.78) |

Correctness was conserved in every cell: completed/checksum = 20,000 and
`checked_out`, `reserved_bytes`, and `terminal_occupied` were all zero after
drain. Retries ranged from 294 to 1,618, demonstrating real full-slot
contention rather than an isolated admission loop.

Interpretation: the single owner and ready/terminal bitmap coordination dominate
the public path under contention; payload work quickly becomes material. There is
no stable AtomicAccounting penalty in this noisy small-run benchmark, but at
4 producers and 4096 B it was 2.2x slower in this sample. Treat this as a
workload-sensitive signal, not a claim of a universal regression. Increasing
producer count past two did not scale: one owner is the architectural serial
stage. A lock-free multi-owner design would violate the documented single-owner
payload/terminal semantics and needs a separate ownership proof.

Limitations: this is one Apple-host run, no CPU pinning/perf counters, no
RemoteStorage allocation measurement, no async waiter path, and no root/hydration
artifact construction in the same transaction. The callback deliberately has a
simple byte checksum; it does not model a real workload. Existing SIMD evidence
in `.perf-scratch/simd-loops/AUDIT.md` covers `nudox-root/src/locality/artifact/rows.rs`
and reports large isolated parse-loop wins, but does not establish an end-to-end
runtime win. Rank random lookup (`rank.rs:25-42`) and overlay merge loops
(`overlay.rs:186-275`) remain the strongest candidates for separate integrated
benchmarks; SIMD/gather is unlikely to help the branchy binary search in
`artifact/view.rs:126-149`.
