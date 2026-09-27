# Continuous fuzzing

ilo builds these packages and mounts one durable corpus directory per target.
The binary is a bolero libFuzzer runner. argv[1] is that mount. AFL++ and
honggfuzz are not linked; ziggy should select its libFuzzer engine or exec
the script below. This repository does not configure MachineConfigurations.

```sh
nix build .#fuzz-store-raw-property
nix build .#fuzz-flow-evaluator
nix build .#fuzz-wire-workspace
nix build .#fuzz-wire-replication
./result/bin/fuzz-store-raw-property /durable/fuzz/store-raw-property/corpus
```

| Package | ilo order | Mount before exec | Committed seeds |
| --- | ---: | --- | --- |
| `.#fuzz-store-raw-property` | 1 | `/durable/fuzz/store-raw-property/corpus` | `tests/fuzz/targets/store-raw-property/corpus` |
| `.#fuzz-flow-evaluator` | 2 | `/durable/fuzz/flow-evaluator/corpus` | `tests/fuzz/targets/flow-evaluator/corpus` |
| `.#fuzz-wire-workspace` | 3 | `/durable/fuzz/wire-workspace/corpus` | `tests/fuzz/targets/wire-workspace/corpus` |
| `.#fuzz-wire-replication` | 4 | `/durable/fuzz/wire-replication/corpus` | `tests/fuzz/targets/wire-replication/corpus` |

`wire-workspace` is the version/delta wire harness. Compile/engine decode is not a package: `NativeEnvelope::decode` pulls `backend-compile` (semantic, tree-sitter, turso) and stays in the backlog. Create the mount directory before the unit starts. The wrapper writes new coverage only there and passes the committed seeds as a second, read-only corpus. Crashes go to the sibling `artifacts/` directory, never inside the corpus.

The same runners are also `.#continuous-fuzz.bins.<id>`. `.#continuous-fuzz` is the bundle Ember already copies. `.#fuzz-<id>` is the alias ilo systemd should hang.

```sh
nix build .#continuous-fuzz
nix build .#continuous-fuzz.corpora.store-raw-property
nix eval --json .#continuous-fuzz.metadata
cargo test -p backend-fuzz
```

`cargo test` must not be built with `--cfg fuzzing` or `--cfg fuzzing_libfuzzer`,
and it must not inherit `BOLERO_RANDOM_ITERATIONS`, `BOLERO_RANDOM_TEST_TIME_MS`,
or `BOLERO_RANDOM_MAX_LEN`. The harness exits 2 when those variables are set.

## Attr paths

The root flake's `packages` output cannot store a bare attrset: `nix flake check`
requires each package to be a derivation. `.#continuous-fuzz` is that derivation.
`bins`, `corpora`, `engines`, and `metadata` are `passthru`, so they are real
attributes of the same package. Short `.#` selects the evaluating system.
The systems are `aarch64-darwin`, `aarch64-linux`, and `x86_64-linux`.

| Path | What Ember gets |
| --- | --- |
| `packages.${system}.continuous-fuzz` | Bundle. `$out/bin/fuzz-<id>`, `$out/engines/fuzz-engine-<id>`, `$out/corpora/<id>/`, `$out/metadata.json` |
| `packages.${system}.continuous-fuzz.bins.<id>` | Supervised runner. Writable corpus first, committed seeds second |
| `packages.${system}.continuous-fuzz.corpora.<id>` | Committed seeds only. Does not build the Rust engine |
| `packages.${system}.continuous-fuzz.engines.<id>` | Instrumented bolero/libFuzzer binary with `FUZZ_TARGET=<id>` |
| `packages.${system}.continuous-fuzz.metadata` | DiffWake scores, scales, re-entry, triage. Pure data, `nix eval --json` |
| `packages.${system}.fuzz-<id>` | Alias of `bins.<id>`. This is the package ilo builds |

Current packages, in `ilo_priority` order: `fuzz-store-raw-property`, `fuzz-flow-evaluator`, `fuzz-wire-workspace`, `fuzz-wire-replication`.

| Ember piece | How it uses the attrset |
| --- | --- |
| WarmVault | On first start, copy `corpora.<id>` into `metadata.targets[].durable_corpus` (`/durable/fuzz/<id>/corpus`). That directory is the incremental corpus. It must outlive the nix store paths |
| DiffWake | Rank `metadata.targets` by `rank` = `complexity.score * gap.score * blast.score`. Recompute it. Skip `rank == null` and `schedule == false`. Do not multiply by `churn` |
| Fabric | Build `.#fuzz-<id>` or `.#continuous-fuzz`. Do not add them to `nix flake check`. One shared Rust derivation feeds every engine; seed changes do not rebuild it |
| TriagePlane | Alert when `bins.<id>` exits non-zero and `artifacts/` beside the durable corpus contains a file. Exit 2 and an empty `artifacts/` are supervision failures |
| `nudox fuzz` | `nix build .#fuzz-<id> && ./result/bin/fuzz-<id> /durable/fuzz/<id>/corpus` |
| ilo | Start `fuzz-<id>` by `ilo_priority` ascending. Mount `/durable/fuzz/<id>/corpus` and pass it as argv[1]. The engine hint is `bolero-libfuzzer`. Do not point AFL++ or honggfuzz at the binary |

`engine_hint` on a harnessed target is `bolero-libfuzzer`. `ci_engine_hint` is `bolero-test`. `ilo_priority` is the systemd start order. It is not an input to `rank`. A harness with gap 0 has rank 0 and `schedule` true; ilo still starts it.

## Rank

`complexity.score` is `loc + error_variants + discriminants`, measured with `wc -l` and enum reads on 2026-09-27. It is not premultiplied. `gap` is an ordinal classification, not an llvm-cov percentage (`llvm_cov_percent` is null; that percentage was not measured). `0` means raw bytes already have bolero or an exhaustive scan. `2` means only fixed hostile examples exist. `3` means the decoder is not callable outside its crate. `1` is unused. `blast` is the trust boundary: `5` untrusted remote or durable bytes, `4` a session frame, `3` a local process facade. `churn` is `git log --oneline` and `ranking_factor` is false. History reaches `2026-01-11`, and `crates/engine/src/lib.rs` has 18 commits, but `decode_command_dto` is `serde_json::from_slice`, so commit count is not a factor.

Nix recomputes `rank` and aborts unless `complexity.score` equals the sum of its parts.

| id | loc | variants | discriminants | complexity | gap | blast | rank | ilo | schedule |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- |
| `store-raw-property` | 908 | 22 | 4 | 934 | 0 | 5 | 0 | 1 | harness. Rank is 0 because gap is 0 |
| `flow-evaluator` | 772 | 23 | 1 | 796 | 2 | 3 | 4776 | 2 | harness |
| `wire-workspace` | 2521 | 31 | 5 | 2557 | 2 | 5 | 25570 | 3 | harness. Version/delta wire |
| `wire-replication` | 1380 | 36 | 15 | 1431 | 2 | 5 | 14310 | 4 | harness |
| `native-envelope` | 867 | 20 | 6 | 893 | 2 | 5 | 8930 | — | backlog. Compile/engine decode, pulls `backend-compile` |
| `session-frame` | 477 | 6 | 5 | 488 | 2 | 4 | 3904 | — | backlog, with native |
| `json-command` | 1121 | — | — | — | 2 | 3 | null | — | rejected, serde facade |
| `store-journal` | 394 | — | — | — | — | 5 | null | — | `decode_record` is `pub(super)` |
| `store-pack` | 310 | — | — | — | — | — | null | — | takes `WirePack`, not bytes |

`store-raw-property` calls `ValidatedFrame::validate`. Loc is `wc -l` of the production validator (908), not the 35-line `raw_property.rs` test module. `ValidateError` has 12 variants and `DescriptorError` has 10. Discriminants are the `NDX1` magic plus `SectionKind` (`Metadata`, `Rows`, `Data`). Gap stays 0 because `raw_property.rs` already runs `bolero::check!` and scans every `u8`. Blast is 5: the error type describes untrusted frame bytes, and the frame is the durable store envelope. The harness cap is 4096 bytes, below `MAX_FRAME_BYTES` (1_048_576). There is no public encoder. The canonical seed is the 41-byte one-section literal. `structural_mutation_laws` stays `pub(super)`.

`flow-evaluator` is a structured grammar (`FLW1` plus 16-byte records), not a production decoder. It calls `reduce_rows`, `filter`, and `distinct`. The independent oracle is an `i128` sum. Loc is `wc -l` of `operators/support.rs`, `operators/stateless.rs`, `types/errors.rs`, and `batch/schema.rs` (772). `FlowError` has 23 variants. The only input discriminant is `FLW1`. Gap is 2. Blast is 3 because this grammar is not a trust boundary on the wire. `types/row.rs` and `types/time.rs` are value holders and are not in the loc sum. Compile/engine decode was the next choice and was not exported.

Workspace wire is `crates/version/src/workspace` (`WorkspaceDecodeError` 11 variants, `WorkspaceError` 20, magics `WMF2` `WDL2` `WCM2` `WTR2` `WPR2`). `decode_untrusted` for a delta is inherent on `WorkspaceDelta`. Replication is `crates/replication/src/codec` plus `ReplicationError` (36) and tags 1 through 15. The public choke point is `decode_message`. Default `TransportLimits.max_frame` is 1 MiB and decoders reserve `Vec` capacity from the counted field, so the harness passes a 16 KiB frame and tiny counters.

`tests/laws` stays the structured model. Its regression file stores proptest RNG fingerprints, not wire bytes. If a shrink comment contains `bytes`, `cargo test -p backend-fuzz` fails until those minimized bytes are copied into `targets/<id>/corpus/<flat-name>`. Do not name the file after the fingerprint.

## Add a harness

Create `tests/fuzz/targets/<id>/`:

| File | Role |
| --- | --- |
| `oracle.rs` | `MAX_LEN`, `exercise`, `judge`, `canonical` |
| `max_len` | Decimal cap shared by Nix and Rust. `1..=1048576` |
| `dictionary.txt` | libFuzzer dictionary. Sibling of `corpus/`, never inside it |
| `score.nix` | Measured complexity, classified gap, classified blast, entrypoints |
| `corpus/canonical` | Fresh `encode` of the success seed |
| `corpus/empty` | Must be rejected |
| `corpus/bad_magic` | Must be rejected |
| `corpus/.gitattributes` | `* binary` |

`<id>` matches `[a-z][a-z0-9-]*`. `score.nix` must set `ilo_priority` to a unique integer from 1 through 9. Do not add a `[[bin]]` and do not edit `default.nix`. `tests/fuzz/build.rs` and `.config/nix/fuzz.nix` discover the directory. The package name is `fuzz-<id>`. Add a line to `tests/fuzz/instrumented` only when a new decoder crate joins the link closure (`tests/fuzz`, `crates/flow`, `crates/replication`, `crates/store`, `crates/version`, `crates/platform` today).

```sh
cargo test -p backend-fuzz
FUZZ_TARGET=<id> cargo run -p backend-fuzz --bin fuzz-target
```

The bounded engine is 16 iterations and 150 ms. That smoke is not a proof. Acceptance is the `canonical` seed plus replay of the committed directory.

## Corpora that survive

`corpora.<id>` is the committed seed derivation. The runner's first argument, when it does not start with `-`, is the writable corpus. Continuous Fuzzing should pass `/durable/fuzz/<id>/corpus`. LibFuzzer writes new coverage only to that first directory and treats the store seeds as a second, read-only corpus. Replacing the binary and passing the same directory resumes from the inputs already there. Crashes go to the sibling `artifacts/` directory. `-max_total_time` is refused on `bins.<id>`; the supervisor owns campaign lifetime. `-timeout=10` is the per-input hang cap.

A local always-on build, without Nix, needs both cfg flags. Bolero 0.13.4 selects libFuzzer from `fuzzing_libfuzzer` and only then stops naming `crate::test::TestEngine` when `fuzzing` is also set.

```sh
chmod +x tests/fuzz/libfuzzer-rustc
CARGO_TARGET_DIR=target/libfuzzer \
  RUSTC_WRAPPER=$PWD/tests/fuzz/libfuzzer-rustc \
  RUSTFLAGS='--cfg fuzzing --cfg fuzzing_libfuzzer -C panic=unwind -Cpasses=sancov-module -Cllvm-args=-sanitizer-coverage-inline-8bit-counters -Cllvm-args=-sanitizer-coverage-level=4 -Cllvm-args=-sanitizer-coverage-pc-table -Cllvm-args=-sanitizer-coverage-trace-compares -Cllvm-args=-sanitizer-coverage-stack-depth' \
  cargo build -p backend-fuzz --bin fuzz-target
FUZZ_TARGET=wire-workspace \
  BOLERO_LIBFUZZER_ARGS='-timeout=10 -max_len=65536 -dict=tests/fuzz/targets/wire-workspace/dictionary.txt tests/fuzz/targets/wire-workspace/corpus' \
  ./target/libfuzzer/debug/fuzz-target
```

`tests/fuzz/libfuzzer-rustc` keeps sancov for the names in `tests/fuzz/instrumented` and strips it everywhere else, including build scripts. The stack-depth flag is Linux-only; drop it on other hosts. The command above runs until the supervisor stops it. Do not pass `-max_total_time` if a green exit should not mean the campaign finished.

## Measurements

Confirmed on this host on 2026-09-27. `rustc 1.99.0-nightly (375b1431b 2026-07-10)`. The binary is the dev-profile instrumented `fuzz-target` at `/tmp/fuzz-engine/debug/fuzz-target`. Recompiling `backend-fuzz` into that directory finished in 1.19 s; the instrumented dependency crates were already there, so that figure is not a cold build. The earlier cold instrumented build of the wire harnesses finished in 12.69 s. It is not the Nix release package: `nix` is not installed here, so release exec/s and `nix flake check` wall time were not measured and are not claimed. Nix does not store these rates. Campaign flags: `-timeout=10 -rss_limit_mb=4096 -max_total_time=10 -print_final_stats=1`, writable corpus first, committed `corpus/` second. Peak pulse is the largest `exec/s` on a libFuzzer `pulse` line. `NEW` lines in the first second print an `exec/s` equal to the unit counter; those values are not the pulse rate. The wire rows below were re-read from the saved campaign logs on that basis.

| Target | exec/s (`stat::average_exec_per_sec`) | Peak pulse exec/s | Time of the first `NEW` after `INITED` | `INITED` cov | First `NEW` cov | Final cov / features | Executed units | Writable corpus | Crashes |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- | ---: |
| `store-raw-property` | 62288 | 65536 | 0.046 s (cov 433 → 436) | 433 | 436 | 550 / 635 | 685169 | 0 files → 32 files, 1016 bytes | 0 |
| `flow-evaluator` | 13542 | 16384 | 0.013 s (cov 754 → 755) | 754 | 755 | 1488 / 4932 | 148970 | 0 files → 299 files, 77834 bytes | 0 |
| `wire-workspace` | 39837 | 65536 | 0.011 s (cov 389 → 392) | 389 | 392 | 795 / 1514 | 438213 | 0 files → 136 files, 55374 bytes | 0 |
| `wire-replication` | 105078 | 131072 | 0.012 s (cov 230 → 231) | 230 | 231 | 955 / 1341 | 1155859 | 0 files → 124 files, 7190 bytes | 0 |

libFuzzer `DONE` lines: store `cov: 550 ft: 635 corp: 32/1055b exec/s: 62288`; flow `cov: 1488 ft: 4932 corp: 301/76Kb exec/s: 13542`; workspace `cov: 795 ft: 1514 corp: 139/54Kb exec/s: 39837`; replication `cov: 955 ft: 1341 corp: 123/7243b exec/s: 105078`. `new_units_added` was 63, 316, 371, and 148. Directory counts are a listing of the writable corpus after exit. The `corp:` field is libFuzzer's merged corpus, so the two counts differ.

Re-entry used the same writable directories and `-max_total_time=3`. Store loaded 32 durable files plus the 3 committed seeds and `INITED` at cov 550 (the first run's final coverage) at 0.066 s. Flow loaded 299 durable files plus the 3 seeds; process output started at 0.054 s and `INITED` at cov 1488 (the first run's final coverage) at 2.172 s, which is the time spent replaying that corpus. Workspace loaded 136 durable files plus the 3 seeds and `INITED` at cov 795 at 0.060 s. Replication loaded 124 durable files plus the 3 seeds and `INITED` at cov 955 at 0.040 s. Cold `INITED` coverage was 433, 754, 389, and 230, so each second process resumed the previous corpus. The 3-second runs' exec/s (store 10841, flow 368) are startup-dominated and are not the rates in the table.

`cargo test -p backend-fuzz` on the same host after these four harnesses: 8 passed, 0 failed, wall clock 0.626 s (libtest reported the suite at 0.00 s). That is the smoke, not a coverage proof.

## What other projects do

**s2n-quic** keeps one bolero `check!` per component, commits the corpus, and replays it with `cargo test`. CI restores `corpus.tar.gz` before tests ([workflow](https://github.com/aws/s2n-quic/blob/main/.github/workflows/ci.yml), [guide](https://github.com/aws/s2n-quic/blob/main/docs/dev-guide/ci.md)). Online QUIC and UDP fuzzers are different programs. Copy the split and the committed seeds. Do not adopt corpus tarballs, Kani, or an online protocol fuzzer.

**rustls** puts cargo-fuzz in a side workspace `fuzz/`. CI job `fuzz` in [`.github/workflows/build.yml`](https://github.com/rustls/rustls/blob/main/.github/workflows/build.yml) runs `cargo fuzz build` and then `cargo fuzz run $target -- -max_total_time=10`. OSS-Fuzz [`projects/rustls/build.sh`](https://github.com/google/oss-fuzz/blob/master/projects/rustls/build.sh) copies release binaries out and zips an external seed corpus. Copy the smoke-versus-always-on split and the seed corpus beside the binary. Do not make cargo-fuzz the discovery root.

**tokio** CI job `check-fuzzing` only runs `cargo fuzz check` ([`tokio/.github/workflows/ci.yml`](https://github.com/tokio-rs/tokio/blob/master/.github/workflows/ci.yml)). Always-on execution is OSS-Fuzz [`projects/tokio/build.sh`](https://github.com/google/oss-fuzz/blob/master/projects/tokio/build.sh). The integration is [tokio-rs/tokio#5391](https://github.com/tokio-rs/tokio/issues/5391) and [oss-fuzz#9480](https://github.com/google/oss-fuzz/pull/9480). Copy "CI proves the short oracle; a supervisor runs overnight."

Bolero's [corpus replay](https://camshaft.github.io/bolero/features/corpus-replay.html) looks beside a `#[test]`, under `__fuzz__/`, not at the committed tree. These harnesses read `targets/<id>/corpus` themselves. The OSS-Fuzz Rust guide ([getting started](https://google.github.io/oss-fuzz/getting-started/new-project-guide/rust-lang/)) ships a seed zip next to the binary and treats crashes as artifacts, not as CI success.

## Adversarial findings

1. **`Result::Ok` hid rejection.** The oracle returns `Ok` for a clean rejection. `Verdict::{Rejected, Accepted}` is the classifier. `empty` and `bad_magic` must be `Rejected`. `canonical` must be `Accepted` and byte-identical to a fresh encode.
2. **Bolero drops `String` errors.** `OracleFailure` implements `std::error::Error`.
3. **Inherited bolero budget.** `with_iterations` keeps an environment value that was already set. Unsetting it is `unsafe` here, so `drive` exits 2.
4. **LibFuzzer ignores argv.** `bolero-libfuzzer` reads only `BOLERO_LIBFUZZER_ARGS` and splits on spaces. The runner sets that variable, rejects spaces, and rejects `-max_total_time`.
5. **Read-only store corpus.** The writable directory is first. `corpora.<id>` is second and read-only.
6. **Crash files inside the corpus.** `-artifact_prefix` is the sibling `artifacts/` directory. Replay does not scan it.
7. **Symlinks and path traversal.** Replay rejects symlinks. Seed names must be one path segment.
8. **Corpus poisoning and proptest fingerprints.** Dictionaries live outside `corpus/`. The laws bridge refuses a `cc <64 hex>` line as decoder input, and fails if a shrink comment contains `bytes` until a human copies the minimized buffer.
9. **Delta false confidence.** `WorkspaceDelta` has no public encoder for the untrusted value. The law is determinism, the item cap, and identity agreement when both decoders accept.
10. **Shared encoder/decoder bugs.** A bug on both sides can still look like a fixpoint. `tests/laws` is the independent model. This package does not invent a second parser.
11. **Release `panic = "abort"`.** Only the fuzz derivation rewrites that profile to `unwind` and disables symbol stripping.
12. **`#[test]` corpus discovery.** Bolero's test engine looks beside the test. Replay reads the committed directory itself.
13. **Nextest group traps.** `.config/nix/control.nix` routes tests whose names match `corpus`, `native`, or `process` into scarce groups. These test names avoid those substrings.
14. **Hand-listed packages drift.** Discovery is `tests/fuzz/targets/<id>/` plus `readDir`. Nix aborts when `score.nix` disagrees with its own sum, when `llvm_cov_percent` is set, or when a seed file is missing.
15. **Bounded smoke is not a proof.** Sixteen iterations can stay green while bugs remain. CI acceptance is the canonical seed plus replay.
16. **LibFuzzer abort versus the shrunk input.** On an oracle failure bolero prints the shrunk input and then aborts. The crash artifact may be the pre-shrink buffer. Triage copies the shrunk bytes from stderr into `corpus/` after checking the cap.
17. **`fuzzing_libfuzzer` alone does not compile.** The derivation passes `--cfg fuzzing` as well. CI passes neither.
18. **Uninstrumented libFuzzer is a false failure.** Without sancov, libFuzzer loads the seeds and exits 1 with `no interesting inputs were found`. Applying sancov to every crate fails the link of build scripts. The wrapper allowlist is `tests/fuzz/instrumented`.
19. **A one-shot temp corpus throws away coverage.** The durable directory is outside the nix store and outside the git tree. The re-entry run above reloaded it and `INITED` at the previous final coverage.
20. **Adding a harness used to mean a new Cargo bin and a new Nix function.** One `fuzz-target` binary and `FUZZ_TARGET` select the oracle. `.#fuzz-<id>` appears from the directory listing.
21. **`fuzz-*` is not an AFL++ or honggfuzz target.** The package ilo builds is a bolero libFuzzer runner. It reads `BOLERO_LIBFUZZER_ARGS` and a corpus directory. AFL++ forkserver mode and honggfuzz persistent mode are not linked. Ziggy should select libFuzzer, or exec `result/bin/fuzz-<id> /durable/fuzz/<id>/corpus`.
22. **Gap 0 makes rank 0.** `store-raw-property` already has in-crate bolero, so the product is 0. Sorting units by rank would start it last. `ilo_priority` is a separate integer and `schedule` stays true.

## Crash triage

A red `cargo test -p backend-fuzz` prints the bolero failure, including `[BOLERO_RANDOM_SEED=...]` for a random input. Minimize by copying those bytes into the matching `corpus/` file only when they are at most the harness cap. Proptest failures stay in `tests/laws/proptest-regressions/`.

An always-on crash lands in `artifacts/`. Do not file a decoder bug for an empty artifact directory or a wrapper exit 2. Reproduce with the artifact or the stderr shrink, then commit the minimized bytes under a flat name.

## MachineConfigurations

This repository does not configure runners. ilo should `nix build .#fuzz-<id>` and exec `result/bin/fuzz-<id> /durable/fuzz/<id>/corpus`. Do not add those packages to `nix flake check`. Do not export `BOLERO_RANDOM_*` into `cargo test`. The binary reads a corpus directory, not AFL++ stdin, and it is not a honggfuzz persistent-mode target.
