# T2 surface prototype (W2.3–W2.5)

Build once with T1, then run the exact binaries with the pinned semantic tools:

```sh
nix develop .#surface --command python3 .config/ci/surface.py \
  --bin-dir /absolute/path/to/t1/bin --case polyglot --report /tmp/surface-report.json
```

`--update` is a separate, explicit review action. Inspect the changed JSON and
explain why the behavior is correct in the PR before committing it. CI prints
missing snapshots, but never accepts them. Indexing must succeed before any
snapshot can be admitted; every configured fixture search must return records.
MCP failures and missing replies fail the case even when the process exits zero.

The polyglot fixture is a synthetic bootstrap for the harness, containing seven
named declarations. It does not complete the representative real-project corpus
in W2.3. That corpus must use pinned Nix fetchers and record revisions, licenses
and selection reasons. GUI integration and parallel artifact consumers remain
separate work in W3.3.

CLI and MCP share one isolated owner, workspace and credential. Fixed scratch
paths keep source locations stable, and an exclusive lock prevents simultaneous
local runs from replacing that state. Normal exit waits for the owner's public
idle retirement. Logs remain under `/tmp/nudox-surface/<case>/locald.log`.
Indexing setup records its successful exit instead of its random caller-journal
receipts; subsequent query answers retain their product fields. Only explicitly
listed timestamp/timing/process fields are normalized.

Wall time is measured separately for each CLI operation and for the MCP session.
Peak RSS and CPU cover the owner over the whole case, including indexing; they
exclude compiler subprocesses and CLI/MCP resource use. Linux `/proc` is required.
The initial ratios and noise floors in `surface.py` are provisional and need
repeated measurements before this becomes a required performance gate. Compiler
shell acquisition is infrastructure setup, outside those operation measurements.
T2 remains advisory in the producer until its separate job is available.
