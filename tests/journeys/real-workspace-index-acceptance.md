# Real-workspace durable indexing acceptance

This lane checks durable indexing with the exact `backend-locald`, `backend-cli`, and `backend-mcp` artifacts built from one clean source revision. It starts an isolated owner in a fresh workspace, submits durable start/status calls with stable keys, waits for `Published` receipts, shuts down the owner, cold-starts the same workspace, and verifies the same receipts, semantic-profile history, and exact expected symbol queries through both CLI and MCP.

The runner is not a file-count benchmark. Its recognized source-candidate census is an independent input measurement, not an indexed-file count. `PASS` requires exact artifact provenance, a closed compiler-host snapshot, every configured language/profile case, complete terminal publication evidence, semantic freshness/history evidence, and stable post-restart queries. Native GUI acceptance remains `NOT_EVALUATED` here and must be recorded by the separate same-owner GUI lane.

## Inputs

Use a clean checkout matching the build receipt, a Root-produced receipt and its verified runtime manifest, the exact three executable paths from that receipt, the canonical versioned closed compiler-host snapshot, and a real corpus manifest prepared from actual local projects. Keep the corpus manifest and result directory outside every listed project. The runner checks this before creating output files and rechecks source, artifacts, manifest, snapshot, and corpus content during the run.

The corpus manifest is duplicate-free JSON with schema `nudox.real-workspace-index-acceptance-manifest.v1` and this shape:

```json
{
  "schema": "nudox.real-workspace-index-acceptance-manifest.v1",
  "projects": [
    {
      "id": "operator-chosen-label",
      "path": "/absolute/path/to/an/actual/project",
      "large": true,
      "minimum_source_candidates": 2900,
      "symbols": [
        {
          "profile": "rust",
          "path": "src/lib.rs",
          "name": "AnExistingPublicSymbol"
        }
      ]
    }
  ]
}
```

This is a format illustration, not an acceptance corpus. Supply real project roots and source files. `symbols` must point to existing non-symlink source files with identifiers already present in those files. Across the corpus, include all supported compiler profiles: `rust`, `csharp`, `java`, `javascript`, `typescript`, `tsx`, `python`, `go`, `c`, and `cpp`. At least one project must be marked `large`; its actual recognized candidate count must meet both 2,900 and the manifest's minimum. Choose symbol names that have an unambiguous, exact product result for their package. The runner does not synthesize project data or downgrade unavailable language toolchains to skips.

## Build receipt

The receipt adapter accepts only `nudox.runtime-artifact-build-receipt.v1`, checks the source commit/tree/clean state and lockfile, verifies the recorded runner and Cargo/rustc identities, rejects check/test/Clippy or incomplete target selections, and re-hashes all three host-architecture executables. A runtime manifest is emitted only for a successful, cleanly terminated build with all three artifacts. An interrupted, failed, dirty, or partial build is not admissible.

The capture command must receive the exact process argv used by the pinned Cargo runner after `--`. When an explicit interpreter is used, the argv must be exactly `[absolute shell, absolute runner script, Cargo verb/options…]`; `-c`/evaluation forms are refused. Both the runner and explicit interpreter are hashed and rechecked around execution. Root supplies the realized runner, Cargo/rustc paths, argv, and artifact paths for a real build; this document intentionally does not provide a fake or host-independent build command.

For an already completed Root receipt, adapt it without running Cargo:

```sh
python3 tests/journeys/scripts/record-runtime-build-manifest.py adapt \
  --source-checkout "$SOURCE_CHECKOUT" \
  --receipt "$ROOT_BUILD_RECEIPT" \
  --output "$EVIDENCE_DIR/runtime-build-manifest.json"
```

The capture mode is for a future explicitly admitted build only. It is not part of source-only review and must not be run without Root's compiler-slot authorization.

## Run and evidence

After Root admits exact binaries and the local owner/MCP workflow, run:

```sh
python3 tests/journeys/scripts/run-real-workspace-index-acceptance.py \
  --source-checkout "$SOURCE_CHECKOUT" \
  --build-manifest "$EVIDENCE_DIR/runtime-build-manifest.json" \
  --locald "$BACKEND_LOCALD" \
  --cli "$BACKEND_CLI" \
  --mcp "$BACKEND_MCP" \
  --compiler-snapshot "$CLOSED_COMPILER_SNAPSHOT" \
  --corpus-manifest "$REAL_CORPUS_MANIFEST" \
  --output "$EVIDENCE_DIR/acceptance"
```

The single absolute deadline defaults to 90 minutes and may be set up to six hours. The result records exact source/tree/lock and executable hashes, project source census hashes/bytes, effective strict-offline policy, closed compiler roles, durable operation keys and receipts, CLI/MCP request and response hashes with bounded head/tail diagnostics, before/after semantic-profile history identities, cold-restart replay, and query identities. The owner and each client run in separate process groups; the runner drains pipes concurrently, enforces per-stream output caps, terminates/reaps owned groups on failure, and continuously retains bounded owner logs.

## Source limits and interpretation

The runner reads the active source capacity constants instead of copying them into policy. The current source allows 100,000 project file records, a 65,464-byte project-row value, and an estimated 1,915 files in one canonical project frontier for the maximum label. The independent corpus inventory is bounded to 32 projects, 2,000 symbol expectations, 150,000 recognized source candidates and 4 GiB total candidate bytes per run, with 32 MiB per source file. The compiler workspace separately permits a 64 MiB build charge and 64 GiB total file bytes. Client stdout/stderr are bounded to 2 MiB/1 MiB per call, retained client evidence to 64 MiB, and owner log evidence to 4 KiB each from the beginning and tail of each stream plus full byte count and digest. The runner never interprets the source census as a published file count.

The repository census available during harness preparation did not provide a truthful positive acceptance input: the smaller checkout had 759 recognized candidates, below the large-project minimum; the larger monorepo had 33,196, well above the current 1,915-file project-frontier estimate. No corpus manifest was fabricated and no backend acceptance was run. Select or prepare a real project set, and resolve any capacity refusal in the owning product path before interpreting a successful run as large-workspace coverage.

The stdlib-only protocol tests exercise parsers, capacity extraction, path-safety refusal, bounded logs/client evidence, duplex-pipe draining, timeout cleanup, and pinned-runner command admission. They use disposable Python child processes and do not launch backend binaries or establish runtime acceptance.
