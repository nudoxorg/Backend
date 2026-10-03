# Live registry-discovery and restore canary

`live_registry_discovery_canary.py` is a source-only prepared runner for one
bounded live owner experiment. It uses the existing `backend-locald`,
`backend-cli index-search`, `package-versions`, `dependencies`, `advisory`,
`forge-add`, and `forge-reference` paths. It does not build binaries, invoke
Cargo, run a package `add`, or start a compiler job. The runner refuses to
start unless `--slot-granted` is explicitly supplied; keep that flag absent
until the measurement owner approves the reviewed source and allocates a slot.
The synthetic safety-boundary tests can be run without network access, Cargo,
or a backend owner with `python3 tests/performance/test_live_registry_discovery_canary.py`;
they are harness tests, not ingestion or throughput evidence.

## What one run exercises

The owner uses a new private workspace and an empty private `HOME`. Its only
online discovery sources are explicit Cargo, npm, and PyPI feeds. The default
page budget is one: the existing service reads 16 recent crates.io names and
their sparse files, at most 32 npm changes and corresponding packuments, and
one normalized PyPI project. `--max-pages 2` doubles the npm window and selects
two PyPI projects; crates.io remains a 16-name recent-updates window. These are
source windows, not a registry-wide scan or catch-up proof. PyPI package
versions follow the owner's first-256 lexicographic release bound; a package's
reported latest version is a candidate only when it falls inside that bound.

Before starting the owner, the runner captures bounded independent HTTPS
responses for those same source shapes, the OSV point queries, and a GitHub tag
reference. Each response body, URL, response headers, request timestamp, byte
count, and SHA-256 is saved under `source-evidence/`. BLAKE3 proofs are computed
from the exact Cargo sparse line, npm version object, or PyPI version file list
used to create each target. Search candidates count as source-verified only
when both the exact PURL and BLAKE3 proof match. If the source changes between
the independent capture and owner fetch, the mismatch stays visible.

The case inventory records ordinary and yanked Cargo releases, an npm version
and a deprecated version when the bounded change window exposes one, a PyPI
published version and yanked release within the owner's version bound, plus its
latest release when that version is within the bound, one independent OSV
reference query per ecosystem (following page tokens to closure, with a
four-page cap), and a single forge repository pinned from a GitHub tag to a
commit SHA. The owner starts with advisory acquisition disabled: OSV query
responses are cross-check evidence, not owner inputs, and this canary does not
claim OSV ingestion or advisory-feed freshness. PyPI's project JSON can carry
a latest-release package-level vulnerabilities list, which is compared by
advisory IDs when present; it does not provide a complete per-version affected
range. The report records missing cases as missing; it does not turn an
unavailable feed or an unknown field into a zero count. PyPI's deprecated
`downloads` value is saved as a raw project-level value with its scope, never
treated as a per-release download count. Cargo's recent crate row may include
crate-level download totals; the sparse version record itself does not carry a
version download count.

The candidate `freshness` field describes the registry observation. The
separate `advisory` CLI reply is captured with refresh disabled and compared
across restore; it does not establish a fresh OSV snapshot. The report keeps
those two freshness scopes separate.

Each real `index-search` query follows every cursor page, rejects malformed
rows and repeated operands, and requires one stable generation snapshot across
its cursor chain. The runner records the current candidate's source proof,
standing, completeness, caught-up flag, freshness, and the `downloads`,
`yanked`, and `advisories` facet states. It separately shows two production
projection gaps: the discovery candidate DTO currently drops npm
`deprecation` and Cargo `cargo_sparse` dependency/feature facts. It also runs
the existing package-version, dependency, and advisory commands for a sampled
coordinate, then repeats those queries after restore.

The GitHub reference is resolved through the public Git References API; an
annotated tag is followed through its tag object until it resolves to a commit.
Only then does the runner issue one `forge-add` for the commit-pinned
coordinate. The fixture is a source-only repository; the report records its
actual indexed records and does not assert that it is a package or has a
supported dependency manifest.

## Owner commands and stable backup protocol

The live owner command is built by the runner with this exact policy (paths
are private paths generated under the fresh output directory):

```text
backend-locald --workspace <owner-workspace> --endpoint <endpoint/locald.sock> \
  --profile builtin \
  --registry-discovery-source cargo=https://crates.io \
  --registry-discovery-source npm=https://replicate.npmjs.com/registry \
  --registry-discovery-source pypi=https://pypi.org \
  --registry-discovery-max-pages <1-or-2> --advisory-offline \
  --forge-max-archive-bytes 67108864 --forge-max-metadata-bytes 2097152 \
  --forge-max-readme-bytes 262144 --forge-max-entries 20000 \
  --forge-max-tree-bytes 134217728 --forge-max-path-bytes 4096 \
  --forge-max-entry-bytes 4194304 --idle-timeout-ms 0
```

The environment is reduced to runtime, proxy, and TLS variables; every
inherited `BACKEND_*` variable is removed. A nonexistent `BACKEND_LOCALD_BIN`
prevents the CLI from silently starting another daemon. No advisory network
feed is configured. Sampled owner descendants are checked for Cargo, Rust,
native compiler, and common build-tool processes. These are point-in-time
snapshots, not continuous process monitoring or a historical ticket census.
Captured command JSON, stdout/stderr, exact argument vectors,
owner logs, source inputs, tool identities, source revision, `Cargo.lock`, and
build-manifest bytes are retained as local evidence. The manifest's source
commit must equal the clean worktree commit and its locald/CLI path, size, and
SHA-256 must match the executed files before and after each owner.

Owner, CLI, Turso, BLAKE3, Git provenance, and full process-census subprocesses
run in their own process groups with wall-time bounds. CLI output is capped at
64 MiB stdout plus 4 MiB stderr; owner output is capped at 32 MiB per stream;
Turso dumps, BLAKE3 hashing, Git checks, and process census output each have
separate byte limits. Partial stdout/stderr and a receipt with exit status,
elapsed time, observed/stored bytes, hashes, and failure reason are retained on
a limit or timeout. The runner admits at most
4,096 CLI operations total across live and restore phases and at most eight
search repetitions; owner lifetime is capped at one hour. Direct HTTPS reads
check both the per-read inactivity timeout and a total request-set deadline.

No CLI calls run while the backup is assembled. The owner applies completed
discovery batches on the command thread; background fetches that have not been
committed when the last query finishes remain outside the durable journal and
are eligible for refetch after restart. The runner copies every single-link
regular non-Turso file under the private workspace and records its relative
path, mode, size, and SHA-256. This includes registry discovery journals,
rebuildable search files, forge/CAS files, receipts, and any other workspace
files. Symlinks and hard-linked files fail the run.

For every `*.turso` file, `tursodb` runs `PRAGMA integrity_check`, captures the
schema object identities and canonicalized `.dump` row inserts, then creates a
separate `VACUUM INTO` snapshot. After SIGTERM stops the owner, the runner
rechecks every source file and compares each source database's logical schema
and row multiset with both its pre-backup signature and copied snapshot. It
authenticates every `DISCOV01` journal frame and compares the source journal
with the copied journal. Known Turso WAL/SHM/journal sidecars are recorded by
path, size, and hash; only those known sidecars are removed from the runner's
private restore copy before cold open. The offline restore owner starts with
registry discovery, package registry, advisory, and forge access disabled.

Restore parity is exact for every search generation and returned record,
except the time-sensitive `freshness` field. Package-version, dependency,
advisory, and forge-reference replies are compared with freshness removed. The
report labels the first restore request as process-cold, not machine-cold:
neither OS page cache nor filesystem cache is flushed. The bounded report also
includes backup wall time and bytes, per-query page and cursor-chain p50/p95/p99,
sampled owner RSS, and Linux `/proc` owner I/O counters when available.

## Run shape

Only use prebuilt tools matching the clean source commit. Supply a new private
artifact path and the exact `tursodb` and `b3sum` executable paths:

```console
nix shell .#luna-tools --command python3 tests/performance/live_registry_discovery_canary.py \
  --locald /absolute/path/to/backend-locald \
  --cli /absolute/path/to/backend-cli \
  --build-manifest /absolute/path/to/runtime-build-manifest.json \
  --tursodb /absolute/path/to/tursodb \
  --b3sum /absolute/path/to/b3sum \
  --output /private/tmp/nudox-live-registry-run-<unique-id> \
  --slot-granted
```

The final flag is a deliberate execution gate, not part of source review. The
run artifacts stay in the new output directory and never use the canonical
workspace, the developer's registry cache, or the remote ILO owner. A failed
run emits `failure-report.json` and retains captured command/source artifacts
for diagnosis. A result can be incomplete if a bounded live window does not
contain the requested yanked/deprecation cases or if an upstream source is
unavailable; the status and exact source window are part of the evidence.
`--max-operations` defaults to 2,048 and caps all CLI commands, including health
probes and every cursor page. `--owner-runtime-seconds` defaults to 1,200 and
can be set up to 3,600 seconds. Time-valued arguments must be finite, positive,
and within their configured limits; invalid values are refused before source
requests or owner startup.

## Current production boundary and follow-up seam

The current path stores discovery facts in
`crates/local-service/src/discovery.rs`'s append-only `catalog.journal` and
rebuildable Tantivy projection. Source adapters in
`crates/engine/src/registry/discovery.rs` already parse Cargo sparse
dependencies/features, npm deprecation, PyPI yanks and package-level
vulnerabilities, and conservative unknown/absent download/advisory facets.
`crates/local-service/src/builtin/product_state.rs` projects discovery
candidates into `crates/library/surface.rs`; that public metadata DTO has
only advisories, downloads, and yanked facets. Thus npm deprecation and Cargo
sparse facts are lost before the current `index-search` reply. The current
registry discovery journal and search projection are not Turso tables.

The existing Turso `AuthorityNamespace::package_metadata` and
`SourceObservationValue` in `extensions/turso/src/authority/types.rs` are not
the missing registry plane: they persist a count, unknown, or unavailable
observation, not version-keyed source facts and typed facet payloads. A small
production seam to review separately is a new
`extensions/turso/src/registry_discovery.rs` typed store with versioned source
progress and release-fact tables, transactionally binding source cursor,
completeness, caught-up state, observation time, PURL, standing, proof, and
tri-state metadata facets. Wire that store at the existing owner commit/read
boundary in `crates/local-service/src/discovery.rs`; keep the public candidate
shape/projection changes in `crates/library/surface.rs` and
`crates/local-service/src/builtin/product_state.rs` as a separately assigned
change. That seam would make a Turso metadata snapshot plus the existing
workspace registry/CAS file inventory restorable as one reviewed closure. No
schema migration or Turso change is included in this canary patch.

This is a bounded canary, not evidence of production-scale throughput. It does
not instrument allocations or HTTP response-body cache reuse, does not compare
old and new Tantivy query/posting formats, and cannot make a tiny first-page
sample representative of registry-wide completeness. Those require paired
frozen binaries and equal independently authored corpus labels, then a separate
allocated scale run.

## Upstream protocol references

- [Cargo registry index format](https://doc.rust-lang.org/cargo/reference/registry-index.html): one JSON line per release, with a `yanked` flag and per-crate sparse-index files.
- [npm replication API](https://github.com/npm/registry/blob/main/docs/REPLICATE-API.md) and [public registry API](https://github.com/npm/registry/blob/main/docs/REGISTRY-API.md): bounded change feed and current package metadata endpoints.
- [PyPI Index API](https://docs.pypi.org/api/index-api/) and [PyPI JSON API](https://docs.pypi.org/api/json/): release file/yanked metadata; the deprecated project `downloads` key is always `-1`, and the project vulnerabilities list describes the latest release.
- [OSV query API](https://github.com/google/osv.dev/blob/master/docs/api/post-v1-query.md): independent point query by package/version with explicit page-token closure; it does not establish local feed coverage or freshness.
- [OSV query response schema](https://osv.dev/docs/osv_service_v1.swagger.json): successful `v1VulnerabilityList` responses define both `vulns` and `next_page_token` as optional fields, so `{}` and a token-only page are valid empty-page shapes; malformed present fields are rejected.
- [GitHub REST Git References](https://docs.github.com/en/rest/git/refs) and [Git tags](https://docs.github.com/en/rest/git/tags): resolve a tag reference and peel annotated tags to their commit object.
- [SQLite `VACUUM INTO`](https://www.sqlite.org/lang_vacuum.html#vacuum_with_an_into_clause): produces a consistent logical database snapshot. The canary adds stopped-owner checks across all databases, journals, and workspace files because `VACUUM INTO` covers only one database at a time.
