# T1/T2 known failures — W2 get-it-green phase

Robert's October16 milestone is **built, wired and fast**, with honest verdicts;
green is the next phase. These cases remain enabled. No snapshot, guard, deadline,
baseline or tolerance is waived by this ledger. Owner below means the agent and
product area responsible for follow-up, not an invented human assignment.

## Current artifact smoke and observed flake

| Case | Observed symptom | Owner | Source |
| --- | --- | --- | --- |
| T1 published-artifact GUI smoke | Producer smoke passes, but artifact-consumer GUI fails after29s; retained owner stack waits in `jbd2_log_wait_commit`. | Codex / desktop owner durability | Focused build3/global511094, head `a2d3ecb1c5d6b597ff9f4039432cc8bccd557ed2`; focused T1 status failed; normal build511058/pr-t1/1039 later passed the required T1 context on this same exact head (status8269). This remains a recorded flake, not a claim that the latest T1 context is red. |

## Last complete Linux catalog packet — historical, not current-head acceptance

Build490580 (`pr-t1/827`), source
`2e5fb3320f927b647882698a23a1388f6e75d142`, artifact manifest SHA256
`3fc486d720a5576e607330498049cc93208313721b0d5cca00da37bacc6c6076`.
The retained packet has16real verdicts:8passed/8failed. Some implementation fixes
have since landed in the integration branch; this table must not be read as a
new-head failure claim. Refresh it from the next exact artifact run.

| Case | Observed symptom | Owner |
| --- | --- | --- |
| c-rxi-map | Authored C symbol returned without its source path/language and with stale-image summary. | Codex / native C source authority |
| cpp-pugixml | One rejected source among47leaves C++ publication partial. | Codex / native C++ authority |
| csharp-tinyioc | No complete native profile published. | Codex / C# profile publication |
| go-sync | `Weighted` search6.34s exceeds unchanged2.61s baseline ×1.5. | Codex / search performance |
| go-uuid | Pinned `NewString` query connection closes/broken pipe. | Codex / client-owner transport |
| java-jsr305 | `CheckForNull.java` compiler-authority runtime refusal. | Codex / Java compiler authority |
| python-tomli | GUI does not show the authored search result. | Codex / desktop search presentation |
| rust-ryu | Shared Cargo-target source scope refused; Robert chose explicit target selection, retaining the guard. | Codex / Rust target selection |

Passed in that same historical packet: cli-usage, polyglot, go-xxhash,
python-blinker, python-cachetools, python-itsdangerous, rust-itoa,
rust-percent-encoding. All remain in the catalog.

## Qualified newer Mac diagnostics

These observations use actual frozen a072 product binaries, but are **not Linux
CI acceptance or calibrated performance**. Raw Mac project coordinates and
content-derived identities must not be copied into Linux goldens.

| Case | Observed symptom | Owner |
| --- | --- | --- |
| go-sync | Actual advertised MCP outline continuation exceeds the unchanged12000-token complete-envelope budget; counts-only probe localizes growth to the opaque cursor. | Codex / MCP outline continuation |
| cpp-pugixml | Both native profiles publish, then `xml_document` search exceeds its bounded client deadline while Tantivy prepares the lexical index. | Codex / lexical index preparation |
| java-jsr305 | Mac diagnostic cannot select the required Java compiler; this is not evidence of a current Linux authority failure. | Codex / diagnostic toolchain setup |

The fresh public cachetools check confirms that `show TTLCache` includes its
`ttl` property and CLI/MCP qualified search returns it. The earlier missing-parent
diagnosis is withdrawn. Its exact-head Linux answer comparison remains a normal
case verdict, not an auto-approved golden.

Evidence: historical full packet
`/private/tmp/nudox-t2-827-2e5fb3320f927b647882698a23a1388f6e75d142.json`;
Mac manifest `/private/tmp/nudox-mac-functional-a072238-manifest.json`;
Go counts `/private/tmp/nudox-go-continuation-diagnostic/mcp-diagnostic.stderr`;
C++ `/tmp/nudox-t2-cpp-pugixml-a072-diagnostic2.log` and `diagnostic3.log`,
thread samples `/tmp/nudox-cpp-a072-preparation-sample-{1,2}.txt`;
cachetools `/tmp/nudox-cachetools-public-proof.{py,json,log}`.
