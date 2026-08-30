# Proof matrix

| id | law | weakened implementation | falsifier | required evidence | state | owner |
| --- | --- | --- | --- | --- | --- |
| R1 | Source facts come from actual metadata and preserve `PRESENT`/`ABSENT`/`ERROR`. | Return a checked-in package list or treat metadata failure as no packages. | Fake metadata failure and malformed JSON must produce `BLOCKED` source facts. | Stable JSON fixture output and nonzero `--require-ready`. | RED | Luna |
| R2 | Every documented package/workspace is checked by exact manifest/package name. | Check only existing C0/I0 vocabulary or only the root workspace. | Remove each documented nested package/workspace from fixtures. | `MISSING_DOCUMENTED_PACKAGE`/`MISSING_DOCUMENTED_WORKSPACE` rows name the exact fact. | RED | Luna |
| R3 | The gate makes no product-behavior claim or product-plane edit. | Equate Cargo presence with compile/query/interface behavior. | Inspect record and source diff for any product assertion/API/dependency. | Negative-space scan and reviewer inventory. | RED | Terra |
| R4 | Later unnamed stages remain explicit blockers. | Guess a Trustfall/Qdrant/placement/unified crate or mark READY with them unnamed. | Current real metadata result must contain the five named `UNDOCUMENTED_PUBLIC_SEAM` rows. | Exact stable record. | RED | Luna then Terra |
| R5 | `--require-ready` exits nonzero whenever a fact blocks readiness. | Print `BLOCKED` but return zero. | Run current tree and every fixture mutant with `--require-ready`. | Exit code ledger. | RED | Luna then Terra |
| R6 | Fixture mutation kills constant/fake inventory behavior. | Ignore package additions/removals/duplicates or parse malformed input permissively. | Added, removed, duplicated, malformed, and failed-command mutations all fail. | `tools/wave-d8-seam-readiness-self-test.sh` raw transcript. | RED | Luna then Terra |
| R7 | Run is bounded and offline after Nix closure realization. | Resolve dependencies or access network/build state. | Audit command argv and run Nix entry point with `--offline --locked --no-deps`. | Nix gate transcript and resource ledger. | RED | Luna then Terra |
| R8 | JSON is deterministic and machine-readable. | Emit unordered human prose, timestamps, or machine paths. | Run twice, byte-compare, and parse with jq. | SHA-256 pair and jq parse. | RED | Luna then Terra |
