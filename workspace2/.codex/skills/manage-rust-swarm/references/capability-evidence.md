# Capability evidence binding

Each managed capability uses a stable lowercase kebab-case ID and one directory:

```text
.codex/evidence/capabilities/<capability-id>/
|-- index.toml
|-- brief.md
|-- proof-matrix.md
|-- research.md
|-- closure.md
`-- packets/                 # optional retained reviews
    `-- <review-id>.md
```

Artifacts are replaced canonically and retained through Git history; do not append competing
amendments. `index.toml` is the binding authority. Store SHA-256 digests as lowercase hex and paths
relative to `workspace2/`.

## Index schema

```toml
schema = 1
capability_id = "leased-range-t0"
state = "phase0" # phase0 | building | review | closed | authority-fork | evidence-blocked
baseline_commit = "<full commit>"
baseline_tree = "<full tree>"
candidate_commit = "" # empty until frozen
candidate_tree = ""
testing_path = "TESTING.md"
testing_sha256 = "<digest>"

[artifacts]
brief = { path = ".codex/evidence/capabilities/leased-range-t0/brief.md", sha256 = "<digest>" }
matrix = { path = ".codex/evidence/capabilities/leased-range-t0/proof-matrix.md", sha256 = "<digest>" }
research = { path = ".codex/evidence/capabilities/leased-range-t0/research.md", sha256 = "<digest>" }
closure = { path = ".codex/evidence/capabilities/leased-range-t0/closure.md", sha256 = "" }

[[workers]]
task_id = "<runtime task id>"
role = "nudox_luna_implementer"
config_path = ".codex/agents/nudox-luna-implementer.toml"
model = "gpt-5.6-luna"
effort = "max"
sandbox = "workspace-write"
baseline_commit = "<full commit>"
checkout = "<absolute isolated checkout>"
card_sha256 = "<digest>"
result_commit = "<full commit or empty>"
retention = "active" # active | absorbed | superseded | rejected-with-counterexample

[[reviews]]
review_id = "pre-edit-1"
sidecar_task_id = "<distinct source-isolated dispatcher runtime task id>"
task_id = "<runtime task id>"
role = "nudox_terra_reviewer"
config_path = ".codex/agents/nudox-terra-reviewer.toml"
model = "gpt-5.6-terra"
effort = "xhigh"
sandbox = "workspace-write"
writable_roots = ["<absolute disposable build root only>"]
implicit_tmp_writes_excluded = true
effective_sandbox_event = "<runtime event/artifact proving effective sandbox and roots>"
snapshot_tree = "<candidate tree or exported-source digest>"
packet_path = ".codex/evidence/capabilities/leased-range-t0/packets/pre-edit-1.md"
packet_sha256 = "<digest>"
source_before_sha256 = "<snapshot aggregate digest>"
source_after_sha256 = "<same digest>"
result = "findings" # findings | clear
```

Use an empty string only for facts that do not exist yet in the named state. Never use placeholders in
a committed live index. Additional worker/review records repeat the tables; existing records are not
rewritten to point at a later card or packet.

## Binding laws

- A worker result is identified by its baseline, checkout, exact commit, tests, and changed paths.
  Role config, runtime identity, model, effort, and sandbox are useful provenance when available, but
  missing telemetry does not make concrete code inadmissible.
- A review is independent only when its packet excludes builder rationale and suspected fixes, its
  source snapshot has no Git history or manager journal, a distinct Codex parent is launched with
  `workspace-write` restricted to one disposable build root with `$TMPDIR` and `/tmp` implicit writes
  excluded, the source snapshot is outside and not added as writable, the registered reviewer is its
  verified child, an effective-sandbox/root event is retained, compiler output and temporary files
  stay under the build root, and before/after source digests match. The
  writable manager may request and ingest the review but cannot parent it directly.
- A revised brief or matrix gets a new digest. Earlier workers remain bound to the old digest and
  cannot be cited for changed rows.
- Rejected branches remain addressable until every salvage row is absorbed or rejected by an
  executable counterexample; then update `retention` without erasing the original commit.
- `evidence-blocked` records a reproducible product, authority, external-system, or toolchain failure,
  two distinct implementation attempts, affected matrix rows, and the external owner/action in
  `closure.md`. Missing worker/reviewer/model routing or receipt formatting is never
  `evidence-blocked`. The state proves no capability law and is not program completion.
