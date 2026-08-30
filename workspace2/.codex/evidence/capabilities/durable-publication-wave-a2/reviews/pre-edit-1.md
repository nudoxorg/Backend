# Raw direct source-isolated pre-edit reviewer receipt

The direct reviewer final response was retained from
`/tmp/nudox-a2-direct-review-build.d6Sh63/reviewer-final.md`. The source archive was
`/tmp/nudox-a2-direct-review-source.krBkw8`, exported at manager commit
`4ec8c959431ab5938177de5b709d7d9d8dc175bc`; source aggregate SHA-256 before and after was
`e9604384d81da0be4a1387f6bfc09a911558bcb996b3b49728d02827a3b4b02f`.

## Findings

1. **BLOCKER — the Sol-owned terminal cannot become green within the allowed paths.** The red journey
   creates and uses `FileJournal` directly and always returns `StableButUnpublished`; it never calls
   a future publication service. An adapter-only change cannot alter that local test error. Falsifier:
   `cargo test --test wave_a2_red -- --ignored` fails exactly with `StableButUnpublished`. Smallest
   correction: Sol replaces the direct journal sequence and unconditional error with the new public
   adapter terminal. Otherwise DP-14 and the public outcome are untestable in scope.
2. **MAJOR — provenance and role-custody proof is incomplete.** The packet omitted current snapshot
   and packet digests, resolved role config, model/effort receipt, direct-consumer ledger, and exact
   gates. Falsifier: an index/receipt binds `4ec8c959` to baseline `b2eb3b24`, snapshot and packet
   digests, and the reviewer runtime/config identity.
3. **MAJOR — DP-15’s standard-library control had not been measured.** The existing allocation test
   measured replay rather than one warmed nonempty `&mut FileJournal::append`. Falsifier: an isolated
   warmed append control records setup/post-setup allocations, retained bytes, copies, file syncs,
   and exact receipt facts; candidate measures the same values plus group buffer.

## Tripwire inventory

Scanned set: `adapters/durable-journal/src/{lib,error,format,journal}.rs`,
`adapters/durable-journal/src/journal/tests.rs`,
`adapters/durable-journal/tests/{allocation,file_journal,wave_a2_red}.rs`,
`adapters/durable-journal/tests/harness/mod.rs`, and `crates/nudox-hydration/src/publication.rs`.

| Tripwire | Count | Exact locations | Reviewer disposition |
| --- | ---: | --- | --- |
| panic/unwrap/expect/unreachable | 1 | `src/journal.rs:132` | `unwrap_or_else` selects `.` for an absent parent path; no panic-like call. |
| source-dropping conversion or map_err | 35 | `src/journal.rs:42,43,47,70,87,88,107,138,143,155,160,165,167,172,177,181,198,199,210,217,222,224,259`; `src/journal/tests.rs:172,194,243,245,290,292,295,349,354,358,359`; `tests/wave_a2_red.rs:70` | Every listed site retains a typed causal source or is transparent conversion. |
| lossy/ambiguous From/TryFrom/raw authority bypass | 2 | `src/lib.rs:37,58` | Lossless transparent numeric newtypes; no authority conversion. |
| checked arithmetic sentinel/saturation or operand loss | 6 | `src/lib.rs:22`; `src/format.rs:127,128`; `tests/harness/mod.rs:108,111,114` | Production overflow retains frame sequence; fixture math is bounded. |
| dyn/Box/Vec/Arc/Rc | 7 | `src/journal/tests.rs:99`; `tests/harness/mod.rs:93`; `tests/file_journal.rs:32,187,204`; `tests/wave_a2_red.rs:52,59` | Test fixtures and Sol setup only; no production dynamic owner. |
| public tuple fields or positional semantic tuples | 5 | `src/journal/tests.rs:80,92,96,100,104` | Private test `Fixture(PathBuf)` only. |
| unit/stateless namespace structs | 0 | full scanned set | None. |
| public local traits or one-implementation delegation | 0 | full scanned set | None. |
| one-letter generic parameters | 0 | full scanned set | None. |
| numeric discriminants/sentinels/offsets/capacities/loop bounds | 154 literals | `src/format.rs:12,13,14,15,16,154,155,156`; `src/journal.rs:150`, plus tests | Wire values are named; any future group/head capacity needs a named control. |
| test-only Option/discarded results/success-only assertions | 15 | `src/lib.rs:21,78`; `src/format.rs:125`; `src/journal.rs:238`; `src/journal/tests.rs:124,376`; `tests/allocation.rs:36,38,42`; `tests/file_journal.rs:25,35,162`; `tests/wave_a2_red.rs:106,107,108` | Existing fixtures are exact; the red journey’s discarded facts culminate in finding 1. |
| unsafe/SIMD/allocator/dependency additions | 0 additions | `Cargo.toml:15`; `Cargo.lock:6,90,126,181` | Pre-existing transitive baseline entries; no source unsafe. |
| public item without current consumer/falsifier | 0 new items | full scanned set | Future `PublishedGeneration` needs compile-fail and public-terminal falsifiers. |

The strongest counterexample was the unchanged ignored journey: no adapter implementation can make its
unconditional local error succeed. The reviewer accepted the existing exclusive `&mut
FileJournal::append` as the simplest standard-library control. It cleared all 35 `map_err` sites as
source-preserving. The likely hidden future cost is fixed group storage plus per-submission
response/cancellation state.

## Runtime and gate receipt

- Reviewer thread: `01a05154-0727-7351-bd70-93a4ce61d525`.
- Command: direct Codex process with `--model gpt-5.6-terra` and
  `model_reasoning_effort=xhigh`; the exact registered reviewer developer instructions were loaded
  into `/tmp/nudox-a2-direct-review-build.d6Sh63/.codex/config.toml` from
  `.codex/agents/nudox-terra-reviewer.toml`.
- Effective sandbox: workspace-write restricted to the disposable build root; snapshot read-only;
  network restricted; approvals disabled; explicit `$TMPDIR` and `CARGO_TARGET_DIR` under build root;
  implicit `/tmp` and TMPDIR grants excluded.
- Gates: strict Clippy passed; normal offline test suite passed 16 tests with the Sol red test ignored;
  explicit red journey failed as expected. An empty isolated Cargo home could not resolve `blake3`, so
  cold-cache offline closure remains unverified.
- Verdict: **BLOCKED — not approved for production edits.** The receipt is retained as a finding,
  not as a substitute for a fresh review after the corrections below.

