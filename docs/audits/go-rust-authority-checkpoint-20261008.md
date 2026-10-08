# Go dependency authority and Rust native controls

Root reviewed the production changes and test successors, replayed the twelve
atomic commits onto canonical `8c18a3d29b641a10a697c160d7e1fb1d35661ae6`,
and independently audited the raw native results. This is a source checkpoint,
not installed CLI/MCP acceptance or a release certification.

The Go owner now keeps normal `GOMODCACHE`/`GOPATH`/HOME cache precedence even
before that cache exists. Ordinary dependency installation populates the path
already selected by the running owner. Explicit-only and closed selections
retain their existing authority rules. Request-owned dependency witnesses
bind the offline selected Go package listing and admitted source bytes;
cancellation and same-length edits refuse stale authority. Seven closed failure
kinds survive the compiler/library boundary, and frontend guidance suggests
`go mod download` only for package-loading failures.

Root's final source tree is `ba33606a3db84f287ebc40ec78e34668fdcd285f`,
identical to the independently prepared canonical replay. Exactly eighteen
reviewed paths change. Canonical changes in the shared host and presentation
files are retained; Cargo.lock remains byte-identical to canonical. Rust has no
production rewrite in this stack: its additional example exercises the existing
HIR/type authority on a genuine pinned Serde checkout.

| Exact native source | Actual result |
| --- | --- |
| `2110a5daf2` | Two separate Rust integration controls pass: shared workspace root/sibling reuse and borrowed HIR/types/resolution/macros/exact spans. |
| `04a4c338bb` | Pinned Serde `6693a89cca77e0151437da1c7f890090b9ebf04c` passes online and with a fresh offline owner in 3.998s/2.507s. Both retain 90 selected declarations, 309 caller declarations, 33 exact trait bounds and one exact foreign typed method call at byte 718. Source is unchanged and no project Cargo.lock is created. Library's `go_` filter passes 22 tests; this includes existing `cargo_*` tests and is not 22 Go projects. |
| `a43002073e` | One genuine Go setup/cancellation/retry/drift integration test passes in 114.85s. Same-owner and fresh offline-owner phases both resolve the exact consumer-owned foreign `dependency::Value` call at bytes 86..91 and infer unannotated `Selected` as `int`. |
| `10abbe5bc2` | Eight witness/guidance unit tests pass; all seven closed failure names have cause-specific literal recovery controls. |

Across those non-overlapping recorded native commands, Root verified 33 actual
test result rows plus the two Serde example phases. Earlier witness suites and
the weaker predecessor Go test remain historical evidence, not additional unique
passes. Raw stdout/stderr hashes, source tree/lock identities, fresh admission,
image seals, unchanged graph stamps and child retirement are retained under
`docs/operations/evidence/go-rust-authority-20261008/`. The Root audit script and
its JSON output are included there. Root audited worker execution evidence;
Root did not independently rerun these binaries.

The failed Serde predecessor compile (missing `HasName` import) and the
1,204.718s presentation compilation timeout remain in the raw evidence. The
timeout ran no test body. Engine unit/test-feature prerequisites were not warm,
so the engine Go/cache controls were not run. Source replay onto canonical is
not a substitute for those combined checks.

Installed GNU Caddy/Serde CLI/MCP runs still have unresolved startup timeouts;
their negative results are not converted to native frontend passes. The
canonical `8c18` matched trio is a separate build without this new source stack.
Current installed public native facts, graphs, continuations and semantic search
remain required. Go closure authority is host-local, CGO remains disabled,
and actual recapture counts/reuse optimization remain unmeasured.
