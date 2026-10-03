# Frozen registry-discovery search benchmark

`registry_journal_benchmark.py` drives the production `index-search` CLI
against the retained 722,663-byte Maven discovery journal and an independent
set of nine query labels. It verifies the journal and label hashes before
starting, copies the prepared workspace to a private run directory, starts
the supplied `backend-locald` offline, follows every page cursor, and compares
the exact returned coordinate set with the labels.

The first pass measures the initial durable projection build. The runner then
stops locald and reopens the same private workspace in a new process to measure
process-cold durable open and warm query pages; it does not flush the operating
system page cache. A page timing includes one CLI process
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
  --output /absolute/path/to/a/new/benchmark-run \
  --slot-granted
```

The default inputs are the retained Maven journal, prepared workspace, and
independent labels under `.local`; this path still requires the exact retained
722,663-byte input. For a different frozen registry-discovery journal, pass
`--allow-non-maven-dataset` with `--journal`, `--workspace-template`, and
`--labels`. The labels must include `journal_sha256`, a nonempty
`dataset_kind`, a nonempty independent `scope`, and expected operands authored
without consulting search output. Both the template's
`registry-discovery/catalog.journal` and the copied private journal must match
the label-bound digest. The report records the declared dataset kind, label
scope, and all three hashes.

This opt-in only relaxes the Maven byte-size assertion for a frozen snapshot.
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
