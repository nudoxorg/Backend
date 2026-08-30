# C1 builder rescue closure custody

Status: R22 DIRECT SOL VERIFICATION COMPLETE; INDEPENDENT ROLE CHAIN EVIDENCE_BLOCKED.

Frozen repository custody was clean at
`38662cf50f459e48f642461737acaa5ccccacf1e`; the canonical card SHA-256 was
`4993af03338d0721f0df6ff9b357d0b7d9b7c9e9bbcca8868af1427744377ed1`.

## Two materially distinct delegation attempts

Attempt one used a newly spawned explicit non-inheriting Terra manager. After exact repository
preflight it attempted:

```text
task_name="r22_reader_one"
model="gpt-5.6-luna"
fork_turns="none"
```

Literal receipt: `collab spawn failed: agent thread limit reached`.

Attempt two reactivated the original canonical Terra manager instead of allocating another manager
thread, then attempted a fresh reader from that established agent-tree branch:

```text
task_name="r22_attempt2_luna_cold_reader"
model="gpt-5.6-luna"
fork_turns="none"
```

Literal receipt: `agent thread limit reached`.

Neither reader ran; neither manager edited files or substituted self-review. R22's fresh cold-reader,
misreader, separate-Terra calibration, and closure review are therefore `EVIDENCE_BLOCKED`. Exact
external unblock: the agent-runtime owner must release fresh child-thread capacity, then rerun the
whole deck and separate hostile closure review against the current card digest.

Commit `5b1bcf8a` stored the same receipts in a new path. That path was then deleted because it was not in
the card's writable inventory; the commit remains a rejected custody-shape checkpoint in history.

## Direct Sol verification permitted by the parent

- All six named prepared-writer behavior/layout tests passed.
- The actual exported format and vocabulary rlibs were unique and hash-recorded. The negative fixtures
  produced the required E0308 cross-kind failure, one E0451 diagnostic per private stored field, and
  E0597 for both scoped entity and type arrays; the one-token legal mutant compiled.
- The paired prepared/manual whole-consumer test passed all valid 0/1/2 combinations and failure rows.
- The constant-body mutation failed with the exact full-width byte mismatch and status 101.
- The partial-write mutation failed at output length zero before preflight and status 101.
- The pristine source hash after both reversed mutations remained
  `7e31c1b17c52c0d077300d6820f85267fce1ba684ee22cbda2084403987ad5cc`.
- Four deterministic compressed code artifacts total 43,153 bytes; full callable observations are in
  `codegen/CODEGEN.md`.
- Rust 1.97.1 on the required AArch64 host observes `PreparedFragment` 48/8 and `FragmentView` 48/8.
- Two distinct fresh-target workspace gates each passed formatting, all 16 tests, and Clippy with
  warnings denied; both immediate `git diff --check` and `git status --short` results were empty.
  Actual-rlib hashes differed between target directories, so they are retained as per-run artifact
  identities rather than misreported as cross-build reproducibility proof.

Direct verification does not counterfeit independent approval. In addition, the advertised consumer
codegen command is not executable without the recorded `-p nudox-ir-format` correction, and owner/manual
optimized bodies retain safe bounds-panic calls. Therefore this receipt makes no isolated-prototype
promotion verdict. Later C1 slices and C2–C6 remain untouched by R22.
