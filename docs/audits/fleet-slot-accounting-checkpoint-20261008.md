# Fleet admission accounting

Two sibling Cargo builds can share an OS process group. The prior collector
summed their jobs and incorrectly refused two valid four-job builds as one
eight-job build; it also counted their occupancy as only one group.

Admission now charges each active Cargo PID a slot, including nested Cargo.
Captured start-token-qualified ancestry folds proven descendant compiler and
runtime work into those slots. Unattributed same-group work adds a residual
slot; non-Cargo compiler/runtime groups consume at least one slot. Missing or
duplicate Cargo PID/group joins refuse admission. Per-build parallelism is
checked on each Cargo process. Raw process-group counts and aggregate jobs
remain telemetry, with named compatibility aliases for existing report readers.

The fleet limit remains sixteen slots, host limits five local/eight ILO/eight
h16001mac, and each build may request at most four Cargo jobs. The authorized
remote admission flags retain destination-only memory guarding and permit
remote work despite unrelated local occupancy. Fresh complete host snapshots,
valid numeric memory, destination eight GiB available memory and sixteen GiB
free disk are still required. The sample remains advisory and reserves nothing;
managed permits and lifetime ownership are separate mandatory controls.

Root independently ran the 35 main tests and 11 script admission tests; direct
script execution also passed 11 tests. This is 57 executions, not 57 distinct
tests. The negative controls cover a five-job Cargo process, missing/duplicate
identity joins, host/fleet saturation and unattributed descendants. A fresh
whole-fleet sample from the exact reviewed collector was valid and allowed.
All five source/test files in replay `e420c5667a` exactly match reviewed atom
`5ea92f232a2b114b3838f4a1c905d8c50ce3aae7`; no Rust source or Cargo.lock changed.

The existing local V5 entrypoint was atomically pointed at that immutable
reviewed checkout. Its previous bytes were archived; the old 8809 checkout was
not modified. Each worker must bind the new collector and sampler identities
and obtain its own fresh sample. Earlier denied samples remain denied; an
activation receipt is not a reusable admission token. No foreign workloads
were signalled or removed.

Exact source hashes, Root command results, fresh sample and activation receipt
are retained under `docs/operations/evidence/fleet-slot-accounting-20261008`.
This tooling checkpoint makes no product performance or runtime coverage claim.
