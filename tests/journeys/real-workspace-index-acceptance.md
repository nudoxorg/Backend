# Real-workspace durable indexing acceptance

This lane checks durable indexing with the exact `backend-locald`, `backend-cli`, and `backend-mcp` artifacts built from one clean source revision. It starts an isolated owner in a fresh workspace, submits durable start/status calls with stable keys, waits for `Published` receipts, shuts down the owner, cold-starts the same workspace, and verifies the same receipts, selected Project membership frontier, semantic-profile history, and exact expected symbol queries through both CLI and MCP.

The runner is not a file-count benchmark. Its recognized source-candidate census is an independent input measurement, not an indexed-file count. Large-project acceptance additionally requires the typed selected-source frontier returned with selected semantic records: the frontier is bound to the requested local package, source-relation root, and source version, and reports the count of Project members after canonical membership pages and every owning file row have been validated. The exact frontier must match across CLI and MCP and across cold restart. Missing evidence blocks acceptance; a large project must exceed the absolute upper bound of the old inline representation, not merely the conservative estimate for a maximum-length label. `PASS` requires exact artifact provenance, a closed compiler-host snapshot, every configured language/profile case, complete terminal publication evidence, semantic freshness/history evidence, and stable post-restart queries. Native GUI acceptance remains `NOT_EVALUATED` here and must be recorded by the separate same-owner GUI lane.

## Inputs

For the TypeScript, Python and Go package corpus, pass `--language typescript`, `--language python`, or `--language go`. This is an explicit language shard, recorded in the result's `scope`; it does not establish all-language or large-project acceptance. Each project still needs real symbol expectations for its selected language, exact artifact provenance, complete semantic publication, CLI/MCP queries, and a cold restart. A TypeScript shard may also contain JavaScript expectations, but each project must have a TypeScript or TSX expectation; JavaScript-only packages cannot inflate TypeScript coverage. Large projects selected within a shard retain their original membership-capacity gates. Omitting `--language` retains every existing all-profile and large-project requirement.

Run independent packages in fresh output directories and bounded parallel processes. Keep pinned registry/forge coordinates and source checksums beside the result. Ten thousand versions of a smaller set of packages do not establish ten thousand distinct packages, and catalog acquisition, compiler prerequisites, structured failures, and empty query parity do not count as a passing package. Results from different build manifests must remain separate.

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
      "minimum_source_candidates": 2046,
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

This is a format illustration, not an acceptance corpus. Supply real project roots and source files. `symbols` must point to existing non-symlink source files with identifiers already present in those files. Across the corpus, include all supported compiler profiles: `rust`, `csharp`, `java`, `javascript`, `typescript`, `tsx`, `python`, `go`, `c`, and `cpp`. At least one project must be marked `large`; its recognized candidate count must meet the runner's source-derived minimum (currently 2,046) and the manifest's minimum. The runner raises a lower manifest value to the derived floor and preserves any stricter value. Choose symbol names that have an unambiguous, exact product result for their package. The runner does not synthesize project data or downgrade unavailable language toolchains to skips.

## Build receipt

A language shard may add `package` to a project entry:

```json
{"ecosystem":"npm","id":"typescript","version":"5.7.2","provenance":{"kind":"archive-sha256","sha256":"<64 lowercase hex digits>"}}
```

The closed ecosystem is `npm`, `pypi`, or `go` for the corresponding language. The provenance kind is `archive-sha256` or `source-tree-sha256`. Results retain this **declared** package metadata with the corpus-manifest hash under `package_provenance`. This field does not verify a downloaded artifact and cannot by itself establish a package pass. Artifact verification requires its separate hash-bound acquisition evidence. The runner's recognized-source census covers a different input domain and is not substituted for the package archive or complete source-tree digest. This optional metadata is unavailable in the default all-profile scope, whose admission requirements remain unchanged.

The distinct-package ledger additionally requires `verified-source-inventory-v1` evidence bound to the exact ecosystem, package, version, complete acquired source-tree hash, inventory hash, and runtime target root/subdirectory. Declaration-only metadata is refused even if the rest of a result claims `PASS`. Its source-tree digest is the canonical inventory file-list digest, not the recognized-source census digest. Each attempt must use that exact digest; older lane-specific tree hashes remain acquisition evidence and cannot be silently substituted.

Setup evidence must match the owner environment witness and the attempt's `stock` or `configured` label. A stock pass may contain neither a compiler-snapshot injection nor compiler environment overrides. The runner supports both modes; stock mode still records the supplied snapshot as a tooling witness and does not establish clean installation acceptance. Declaration-only package metadata cannot grant either mode's package credit. The ledger reads a stable, bounded, single-link regular result file and refuses symlinks, hard links, special files, and changed receipt bytes.

For independently verified source, add project `acquisition` with `inventory_path` (absolute), `inventory_sha256`, and `target_subdir` (`.` or a canonical package-relative directory). The bounded inventory is a closed object with schema `nudox.acquired-package-source-inventory.v1`, exact `package` fields `ecosystem`, `id`, `version`, canonical absolute `source_root`, and a sorted unique `files` list. Each file has exactly `path` (canonical root-relative POSIX), `bytes`, and `sha256`. Include every regular acquired file, including non-source files; only root `.git` administrative metadata is excluded. Symlinks and other nonregular entries are refused. Keep the inventory outside the acquired root.

The runner independently reads all file bytes, rejects missing/extra/changed files, and proves its runtime root equals `source_root/target_subdir` (including Go subpackages). It rechecks this inventory during provenance revalidation. `source_tree_sha256` is SHA-256 of the files list encoded as UTF-8 JSON with sorted object keys, no insignificant whitespace, and `ensure_ascii=False`. It is a new domain, not a previous lane inventory or recognized-source census digest. A declared `source-tree-sha256` must equal it. Verified results use `verification="verified-source-inventory-v1"` with `package`, `source_tree_sha256`, `acquired_inventory_sha256`, `target_subdir`, and `target_root_identity_sha256`. This verifies acquired source bytes and their declared package binding; it does not claim an archive was independently downloaded or verified.

For NPM/PyPI registry identity, add `acquisition.origin={"receipt_path":"<absolute sidecar>","receipt_sha256":"<digest>"}`. The closed `nudox.registry-artifact-origin.v1` sidecar has `package` (ecosystem/id/version), `registry_metadata` and `archive` (each exactly path/sha256/url), and `unpack={"strip_prefix":"<canonical archive root>/"}`. Retain the actual official registry metadata response and downloaded archive. The verifier checks official URL origin, exact package/release metadata, NPM SRI or PyPI declared digest/size, and archive-internal package.json or PKG-INFO identity. It safely streams every archive member, refuses links/special/traversal/duplicate members, and independently requires archive membership bytes to equal the acquired tree. It does not extract or modify sources. Success adds `origin_verification="verified-registry-artifact-v1"` and `origin_evidence` metadata/archive/archive-membership digests. Go origin remains refused until its separately authenticated module/target binding is supported. Private candidate setup and runtime surface acceptance remain independent gates.

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

`--setup-mode configured` is the default and injects the admitted closed compiler snapshot. `--setup-mode stock` omits that injection and excludes ambient compiler overrides; the supplied snapshot is still validated as a tooling witness and is not claimed as the product's selected authority. The result's closed `runtime_setup` records the mode, actual snapshot injection, compiler override names, and the same owner-environment digest as `environment_witnesses`. `runtime_tooling` separately reports whether PATH contains a Nix-store runtime, the environment key names, and the absence of exposed typed selected-tool authority. Stock launch mode alone does not prove a stock installation; these private candidate binaries have `installation_acceptance=false`. Preserve stock setup and product failures as measured outcomes.

An optional symbol `surface_contract` exercises paired warm/cold CLI and MCP search, resolve, show/document, source, references and graph, plus raw typed MCP references and declaration-row reads. It contains exactly `kind`, `source`, `references`, and `graph`. `kind` is checked on the resolved record; document/source pages do not expose that field. `source` binds `file_sha256`, half-open UTF-8 byte `start`/`end`, and `slice_sha256`. Each required reference has `path`, the same four span fields, a closed semantic `relation`, and a closed authority `confidence`. Named graph returns bounded neighbor records rather than a document page. A graph obligation with `label="neighbor"`, target `path`, and target `name` proves that source-backed neighbor is returned; other labels fail because this route does not expose edge direction or kind. The runner independently hashes every expected source slice, requires the complete source body rather than its header, compares named CLI/MCP projections, and checks typed reference obligations. Empty required sets are permitted but explicitly make no complete-universe claim; all endpoints are still exercised.

`surface_contracts_before_restart` and `surface_contracts_after_restart` retain contract digests, exact resolved coordinates, the record and page public key abbreviations separately, the full row digest, and reference semantic endpoint identities. Record and page abbreviations may differ at the same exact declaration coordinate; each projection is retained and compared within its own route after restart. Cold calls reuse the old coordinate and must retain the same row and semantic evidence. These are distinct identity planes; a display abbreviation is not claimed as a canonical symbol identifier. A symbol without this contract supplies no interface evidence. Unit tests of these observer gates are not product acceptance. Independently verified source inventories still have `origin_verification="unverified-declared-package"` until official metadata/archive origin and archive-to-tree membership are verified; no registry-identity credit follows from labels or source hashes alone.

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

The runner reads the active source capacity constants instead of copying them into policy. The current source allows 100,000 project file records, a 65,464-byte project-row value, and a conservative estimate of 1,915 files in one canonical project frontier for a maximum-length label. Separately, dividing that row capacity by the 32-byte membership key gives an absolute old-inline upper bound of 2,045 files before accounting for label and field overhead. Both the large-project candidate-census floor and the independent accepted Project-member gate currently use 2,046, but they are distinct evidence: candidates only establish a plausible input size, while the typed query frontier proves admitted membership. The candidate floor is read dynamically as one more than the source-derived absolute inline bound; the manifest may request a higher minimum. The independent corpus inventory is bounded to 32 projects, 2,000 symbol expectations, 150,000 recognized source candidates and 4 GiB total candidate bytes per run, with 32 MiB per source file. The compiler workspace separately permits a 64 MiB build charge and 64 GiB total file bytes. Client stdout/stderr are bounded to 2 MiB/1 MiB per call, retained client evidence to 64 MiB, and owner log evidence to 4 KiB each from the beginning and tail of each stream plus full byte count and digest. The runner never interprets the source census as a published file count.

The repository census available during harness preparation did not provide a truthful positive acceptance input: the smaller checkout had 759 recognized candidates, below the large-project minimum; the larger monorepo had 33,196, above both the 1,915-file conservative estimate and 2,045-file absolute inline upper bound. Those counts are source candidates only; they do not prove accepted Project members. No corpus manifest was fabricated and no backend acceptance was run. Select or prepare a real project set, and resolve any capacity refusal in the owning product path before interpreting a successful run as large-workspace coverage.

The stdlib-only protocol tests exercise parsers, capacity extraction, path-safety refusal, bounded logs/client evidence, duplex-pipe draining, timeout cleanup, and pinned-runner command admission. They use disposable Python child processes and do not launch backend binaries or establish runtime acceptance.
