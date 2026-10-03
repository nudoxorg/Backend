# Frozen registry-discovery search benchmark

`registry_journal_benchmark.py` drives the production `index-search` CLI
against the retained 722,663-byte Maven discovery journal and its independent
query labels. It pins the retained journal SHA-256 to the value recorded in
the readiness evidence, freezes the exact label/build-manifest bytes before
parsing them, and rechecks those files, `Cargo.lock`, the runner, the source
revision, and both executable SHA-256/byte counts before and after the owner
processes. It copies the prepared workspace to a private run directory,
starts the supplied `backend-locald` with registry, discovery, advisory, and
forge access explicitly offline, follows every page cursor within one stable
index snapshot, and compares the exact returned coordinate set with the
labels. All inherited `BACKEND_*` variables are removed for the owner and
CLI processes so hidden source or online policy cannot alter the run.

The first pass measures the first open of the supplied workspace. If its
private copy already contains `registry-discovery/catalog-search-v1`, the
report calls this a preexisting-projection open and records its before/after
bytes. If it is absent, the report records that fact; any new projection is
then attributable to first open. The runner does not reset or delete a
prepared projection. It stops locald and reopens the same private workspace
in a new process to measure process-cold durable open and warm query pages; it
does not flush the operating system page cache. A page timing includes one CLI process
and one local socket round trip, so it describes the actual command path rather
than scorer-only time. The report also records executable hashes, source
revision, journal bytes, projection bytes, and sampled daemon RSS. It does not
build binaries or change the canonical journal, labels, or workspace template.
Because current locald requires the copied advisory authority to be private,
the runner changes only the private workspace copy to mode `0600` after
verifying that it is a current-user-owned regular single-link file. It refuses
links and leaves the prepared workspace template unchanged.

Run only with prebuilt binaries from the assigned Nix shell and after receiving
the measurement slot:

```console
nix shell '.#luna-tools' --command python3 tests/performance/registry_journal_benchmark.py \
  --locald /absolute/path/to/backend-locald \
  --cli /absolute/path/to/backend-cli \
  --build-manifest /absolute/path/to/runtime-build-manifest.json \
  --output /absolute/path/to/a/new/benchmark-run \
  --slot-granted
```

The default inputs are the retained Maven journal, prepared workspace, and
independent labels under `.local`; this path requires both the exact retained
722,663-byte size and SHA-256 `7b156608e0427b60a7fd394f1d4d0c4b68aa7d7df6d55f0b12a1da80f9f36899`.
It also pins the independent-label snapshot to SHA-256
`16475bf6c658d380873e01c5555e888093a052cb372699690f4e19b9c7d927bd`. For a
different frozen registry-discovery journal, pass
`--allow-non-maven-dataset` with `--journal`, `--workspace-template`, and
`--labels`. The labels must include `journal_sha256`, a nonempty
`dataset_kind`, a nonempty independent `scope`, and expected operands authored
without consulting search output. Both the template's
`registry-discovery/catalog.journal` and the copied private journal must match
the label-bound digest. The report records the declared dataset kind, label
scope, and exact frozen hashes for the journal, labels, build manifest,
runner, lockfile, and binaries.

This opt-in only relaxes the Maven size-and-digest assertion for a frozen snapshot.
It does not launch online discovery or validate how the snapshot was produced;
record the upstream request/receipt and source completeness separately. A
small canary journal measures the actual `index-search` path on that canary,
not production-scale throughput. It writes `report.json` and the locald log
under the new output directory.

The runner measures one binary variant per invocation. A v1/v2 comparison
must use separate frozen binaries, build manifests, and output directories,
with byte-identical source journal, independent labels, host/toolchain/profile,
page size, and repetition count. The build manifest and executable digests
identify each query/posting-format variant; the report does not infer an index
format from a filename. The sampled RSS is a 50 ms process sampler, not an
allocator peak. This runner does not report allocations or physical I/O, and a
reopen is not a cold-machine measurement. Those fields must remain unavailable
until measured at a supported boundary.
