# Executable evidence

The seven tests in `probes.rs` assert the buggy behavior observed on 12 September 2026. The two tests in `journal-probes.rs` assert publication-recovery rejection at interrupted-write states. They intentionally pass while these defects exist. Convert them into assertions of the desired behavior when making the corresponding repairs.

The routing test in the production checkout instead asserts the desired repaired behavior. Luna demonstrated that it fails with the five-line guard removed, returning an `Ok(MergeOutcome)` where `ResponseAttempt` is required. After restoring the guard, all 12 routing tests passed. The main agent reviewed the patch.

Run from any directory:

```sh
python3 /Users/mileswirht/Downloads/backend/audit/server-heart-2026-09-12-evidence/run-probes.py
```

The script copies the current repository's relevant source directories into a fresh temporary directory, substitutes a small Cargo workspace to avoid the existing missing `GUI2` member, injects the journal counterexamples into that copy, and runs the tests offline. It does not change production sources. Paths to the copy and result logs are printed. The recorded probe lockfile is included; cached dependencies are required for offline execution. This setup is intentionally separate from the original workspace lockfile and does not certify the full workspace build.

The nine counterexamples cover:

- Changed bytes behind a safe `AsRef` owner after verified insertion.
- Zero scores for all proper prefixes at the production lexical weight.
- A removed term membership hiding another live matching membership.
- Empty Qdrant projection accepted for a nonempty validated segment selection, through a real local HTTP exchange.
- Residency capacity exhausted after 257 single-segment manifest advances.
- An impossible admission retained as a pending future in an empty runtime.
- Distributed top-one returning stale A while canonical newest-wins search returns B.
- Complete publication fact before head rejected on reopen.
- Interrupted temporary head preventing reopening the valid previous publication.

No live Qdrant instance, model inference, full workspace build, fuzzing campaign, or performance benchmark was run.
