# Retained Maven registry-search benchmark

`registry_journal_benchmark.py` drives the production `index-search` CLI
against the retained 722,663-byte Maven discovery journal and an independent
set of nine query labels. It verifies the journal and label hashes before
starting, copies the prepared workspace to a private run directory, starts
the supplied `backend-locald` offline, follows every page cursor, and compares
the exact returned coordinate set with the labels.

The first pass measures the initial durable projection build. The runner then
stops locald and reopens the same private workspace in a new process to measure
cold durable open and warm query pages. A page timing includes one CLI process
and one local socket round trip, so it describes the actual command path rather
than scorer-only time. The report also records executable hashes, source
revision, journal bytes, projection bytes, and sampled daemon RSS. It does not
build binaries or change the canonical journal, labels, or workspace template.

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
independent labels under `.local`. Pass `--journal`, `--workspace-template`,
`--project-template`, or `--labels` to use a different frozen copy; the
runner rejects any journal whose digest differs from the independent labels.
It writes `report.json` and the locald log under the new output directory.
