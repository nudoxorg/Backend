# Keel executable contract suite

Definitions live in Nix. The runner is Rust, uses admitted application artifacts,
and never builds or links the product as a test library. Each fixture records
independently authored source facts, refusal controls and revision histories.
Expected output is reviewed source; there is deliberately no bless/update flag.

The runner lives in the standalone [nudoxorg/keel](https://github.com/nudoxorg/keel)
repository, under `adapters/nudox`. Backend's flake pins its revision. Enter the
compiler shell (`nix develop .#compiler`) to obtain `BACKEND_KEEL_HARNESS`, the
absolute executable path used by CI. Product definitions remain in this directory.

```sh
nix-instantiate --eval --strict --json tests/keel/default.nix > /tmp/keel-cases.json
"$BACKEND_KEEL_HARNESS" --manifest /tmp/keel-cases.json --list
"$BACKEND_KEEL_HARNESS" --manifest /tmp/keel-cases.json --bin-dir /path/to/artifacts/bin --evidence /path/to/new-evidence
```

`default.nix` is the complete mandatory inventory. `cases.nix` contains live
Rust/Python/JavaScript owner histories. `native.nix` drives the actual CSS and
SQLite worker processes; SQL asserts syntactic facts and missing binding
context, without claiming database execution or bound semantic graph edges.
The artifacts must include `backend-locald`, `backend-cli`, `backend-mcp`,
`backend-stylo-worker`, and `backend-sqlite-worker` (the packaged name of the
isolated SQLite workspace's `sqlite-worker`). A missing artifact fails admission.

The runner starts a fresh offline owner per history, indexes through the real
CLI and requires the terminal publication receipt with complete native profile
coverage. Every query step runs the real CLI and MCP over the same owner. It
checks the independently authored expected fields, compares full JSON with MCP
structured content, and compares Markdown with MCP text (only terminal newlines
are ignored). Bindings copy opaque returned coordinates; no coordinate is
reconstructed from its spelling. Edits, deletion/re-addition and cold owner
restarts are explicit steps.

Native worker requests cross their real process framing. CSS uses STYLOR01 and
verifies STYLOS01 framing before reading typed results. SQL receives its versioned
JSON request. Native response source spans, decoded identifiers, coverage gaps,
limits and invalid syntax/version controls are independent fixture expectations.

## Evidence and failure behavior

The evidence destination must not exist. It retains the exact evaluated manifest,
artifact SHA-256 hashes, per-step raw stdout/stderr, decoded JSON, owner logs,
parity diffs and report.json. A failed prerequisite blocks subsequent steps in
that history and leaves independent cases runnable. Unknown fields, duplicate
identities, unsafe fixture paths, unsupported/paused languages, unknown step
kinds and executable steps without behavior assertions fail before execution.
An exit-zero-only check is rejected.

Each process has a deadline and bounded concurrent output capture. Its own
process group is killed and reaped on timeout or after the direct process exits,
so a child holding a pipe cannot hang the gate. A deliberately detached descendant
requires a containing cgroup on Linux; process groups alone cannot contain it.
Artifact hashes are checked again after execution; replacement invalidates the
whole run. A green report describes the observed artifacts and manifest; the
caller must bind artifact hashes to its source/build provenance before promotion.

`--case ID` is an explicit diagnostic selection, never an inferred coverage pass.
Keel's optional `--order FILE` accepts schema 1 `{cases:[...]}` only when it is a
complete permutation of the selected inventory. Missing, duplicate or unknown
IDs fail. Nudox graph/complexity estimates affect ordering, never test omission,
cache admission or the verdict. The order file and hash are retained in evidence.

## Driver verification and migration

In a standalone Keel checkout, `cargo test --locked -p backend-keel-harness
--test driver` exercises the built runner as an external executable. Its GitHub
CI also runs the Linux containment holdout in a delegated cgroup. These checks
attack malformed/vacuous definitions,
fixture escape, oracle failure and dependency blocking, independent pass
retention, dropped-case ordering, timeout cleanup and inherited-pipe cleanup.
These checks verify harness custody; they do not substitute for product runs.

[MIGRATION.md](MIGRATION.md) maps existing requirements and remaining gaps. The
new cases are an executable first tranche, not a claim that every old unit law has
been replaced. A module is removed only when an equivalent named integration row
has run against admitted product artifacts. This follows the existing Keel
TRANSITION plan's evidence gate and keeps uncovered protocol/pagination/crash
requirements visible throughout the clean-slate move.

The runner exports `--inventory` as canonical JSON containing the manifest and
case-definition digests, fixture-input digests, declared steps, authority inputs
and case-to-node mapping. A case's `requires` list declares earlier foundational
receipts; selection includes that closure, execution respects its topological
order, and a failed foundation blocks materializing the dependent world.
Independent cases continue. No blocked case is counted as a pass.

On Linux, set `KEEL_CGROUP_ROOT` to an explicitly delegated child of a cgroup-v2
mount. Every product process enters its own owned child cgroup before exec; the
scope kills and waits for all members, including children that create their own
sessions. `KEEL_REQUIRE_CGROUP=1` refuses to execute without that boundary. CI
crash and timeout worlds must require it. Reports distinguish `cgroup-v2` from
the process-group fallback. The explicit Linux containment holdout requires a
pinned `KEEL_TEST_SETSID` executable and runs with `--ignored`; its absence does
not establish full process-tree containment.
