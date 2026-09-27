# Continuous fuzzing

`packages.${system}.continuous-fuzz` is the package contract. Its `passthru` is only `bins`, `corpora`, `engines`, and `metadata`. `.#fuzz-<id>` aliases `bins.<id>`. Lattice authors write `check.continuousFuzz { … }` for kind `continuous-fuzz`. That function belongs to Nix Test DSL. This repository does not define it. Harness repos do not author through the schema id. `metadata.schema` is `"nudox.continuous-fuzz.v1"`, the document id inside metadata. `metadata.latticeKind` is `"continuous-fuzz"`. Auth copies the metadata keys and sets `engine_catalog` entry `go-native` to `linked = true` and `adapter = "testing.F"`. This repo links `libfuzzer` with `adapter = "bolero"`. `aflpp`, `honggfuzz`, and `go-native` are the same records with `linked = false`. This repository does not configure MachineConfigurations.

```sh
nix build .#continuous-fuzz
nix eval --json .#continuous-fuzz.metadata
nix build .#continuous-fuzz.bins.journal-codec
./result/bin/fuzz-journal-codec /durable/fuzz/journal-codec/corpus
```

| `start_order` | Alias | Mount (argv[1]) | Committed seeds |
| ---: | --- | --- | --- |
| 1 | `.#fuzz-pack-decode` | `/durable/fuzz/pack-decode/corpus` | `tests/fuzz/targets/pack-decode/corpus` |
| 2 | `.#fuzz-journal-codec` | `/durable/fuzz/journal-codec/corpus` | `tests/fuzz/targets/journal-codec/corpus` |
| 3 | `.#fuzz-native-protocol` | `/durable/fuzz/native-protocol/corpus` | `tests/fuzz/targets/native-protocol/corpus` |
| 4 | `.#fuzz-store-raw-property` | `/durable/fuzz/store-raw-property/corpus` | `tests/fuzz/targets/store-raw-property/corpus` |
| 5 | `.#fuzz-wire-workspace` | `/durable/fuzz/wire-workspace/corpus` | `tests/fuzz/targets/wire-workspace/corpus` |
| 6 | `.#fuzz-wire-replication` | `/durable/fuzz/wire-replication/corpus` | `tests/fuzz/targets/wire-replication/corpus` |
| 7 | `.#fuzz-flow-evaluator` | `/durable/fuzz/flow-evaluator/corpus` | `tests/fuzz/targets/flow-evaluator/corpus` |

`kind = "fuzz"` rows are scheduled. `kind = "property"` rows are siblings in the same `targets` array: `laws` (`tests/laws`) and `store-frame` (`crates/store/src/view/validate/raw_property.rs`). They have `engine = "property"`, `schedule = false`, and null `bin`, `corpus`, `rank`, and `start_order`. They are not overnight units.

Create the mount before the unit starts. The wrapper writes new coverage only there and passes `corpora.<id>` as a second, read-only corpus. Crashes go to the sibling `artifacts/` directory.

## Attr paths

`nix flake check` requires each package to be a derivation, so `.#continuous-fuzz` is a derivation and the four collections are `passthru`. Short `.#` selects the evaluating system (`aarch64-darwin`, `aarch64-linux`, `x86_64-linux`).

| Path | Value |
| --- | --- |
| `packages.${system}.continuous-fuzz` | Bundle. `$out/bin/fuzz-<id>`, `$out/engines/fuzz-engine-<id>`, `$out/corpora/<id>/`, `$out/metadata.json` |
| `packages.${system}.continuous-fuzz.bins.<id>` | Supervised runner for a `kind=fuzz` target. argv[1] is the writable corpus |
| `packages.${system}.continuous-fuzz.corpora.<id>` | Committed seeds. Does not build the instrumented binary |
| `packages.${system}.continuous-fuzz.engines.<id>` | Adapter derivation. `passthru.adapter` is `{ id, family, linked, adapter }` |
| `packages.${system}.continuous-fuzz.engines.<id>.adapter` | Plug record. Here `id = "libfuzzer"` and `adapter = "bolero"` |
| `packages.${system}.continuous-fuzz.metadata` | Metadata document. `schema` is `nudox.continuous-fuzz.v1`. `latticeKind` is `continuous-fuzz` |
| `packages.${system}.fuzz-<id>` | Optional alias of `bins.<id>` |

`metadata.target_fields` is the row schema. Every fuzz, property, and backlog row has the same keys. `metadata.engine_catalog` is the adapter list. `metadata.kinds` is `[ "fuzz", "property" ]`. `rank` is `complexity.score * gap.score * blast.score`. Recompute it. Skip `rank == null` and `schedule == false`. `start_order` is not an input to rank. A fuzz row with gap 0 has rank 0 and `schedule` true, so it still runs.

Consumers read `metadata.consumers`. There is no second path: no cargo command, no test-runner filter, and no per-language field names in that document.

## Schema

```json
{
  "schema": "nudox.continuous-fuzz.v1",
  "latticeKind": "continuous-fuzz",
  "kinds": ["fuzz", "property"],
  "engine_catalog": [
    {"id": "libfuzzer", "family": "coverage-guided", "linked": true, "adapter": "bolero"},
    {"id": "aflpp", "family": "coverage-guided", "linked": false, "adapter": null},
    {"id": "honggfuzz", "family": "coverage-guided", "linked": false, "adapter": null},
    {"id": "go-native", "family": "coverage-guided", "linked": false, "adapter": null},
    {"id": "property", "family": "bounded", "linked": true, "adapter": "proptest"}
  ],
  "formula": "complexity.score * gap.score * blast.score"
}
```

A fuzz row sets `kind` to `"fuzz"`, `engine` to a catalog id, `bin` and `corpus` to the passthru paths, `max_input_bytes` to the cap, and `start_order` to the start sequence. A property row sets `kind` to `"property"`, `engine` to `"property"`, and the artifact fields to null. Auth keeps every key. It flips `go-native.linked` to true, sets that record's `adapter` to `"testing.F"`, and points fuzz rows at `engine = "go-native"` when that is the plug. Corpus path strings are per repo; the field name is `committed_corpus`.

## Rank

`complexity.score` is `loc + error_variants + discriminants`, measured with `wc -l` and enum reads on 2026-09-27. It is not premultiplied. `gap` is an ordinal classification, not an llvm-cov percentage (`llvm_cov_percent` is null; that percentage was not measured). `0` means raw bytes already have an in-process engine or an exhaustive scan. `2` means only fixed hostile examples exist. `3` means the decoder is not callable outside its crate. `1` is unused. `blast` is the trust boundary: `5` untrusted remote or durable bytes, `4` a session frame, `3` a local process facade. `churn` is `git log --oneline` and `ranking_factor` is false. History reaches `2026-01-11`, and `crates/engine/src/lib.rs` has 18 commits, but `decode_command_dto` is `serde_json::from_slice`, so commit count is not a factor.

Nix recomputes `rank` and aborts unless `complexity.score` equals the sum of its parts.

| id | loc | variants | discriminants | complexity | gap | blast | rank | start | schedule |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- |
| `pack-decode` | 2737 | 33 | 5 | 2775 | 2 | 5 | 27750 | 1 | harness. `IndexPack::open` |
| `journal-codec` | 763 | 18 | 18 | 799 | 2 | 5 | 7990 | 2 | harness. Effect and dispatch records |
| `native-protocol` | 867 | 20 | 6 | 893 | 2 | 5 | 8930 | 3 | harness. `NativeEnvelope::decode` |
| `store-raw-property` | 908 | 22 | 4 | 934 | 0 | 5 | 0 | 4 | harness. Rank is 0 because gap is 0 |
| `wire-workspace` | 2521 | 31 | 5 | 2557 | 2 | 5 | 25570 | 5 | harness. Version/delta wire |
| `wire-replication` | 1380 | 36 | 15 | 1431 | 2 | 5 | 14310 | 6 | harness |
| `flow-evaluator` | 772 | 23 | 1 | 796 | 2 | 3 | 4776 | 7 | harness |
| `session-frame` | 477 | 6 | 5 | 488 | 2 | 4 | 3904 | — | backlog. Separate from the native envelope |
| `json-command` | 1121 | — | — | — | 2 | 3 | null | — | rejected. serde, including json wire and mcp jsonrpc |
| `store-journal` | 394 | — | — | — | — | 5 | null | — | `decode_record` is `pub(super)` |
| `store-pack` | 310 | — | — | — | — | — | null | — | takes `WirePack`, not bytes |

`pack-decode` calls `IndexPack::open` with `IndexPackId::from_encoded_bytes`. Loc is `wc -l` of the pack codec (2737), excluding `pack/store.rs`. `IndexPackOpenError` has 28 variants and `IndexPackEncodeError` has 5. Discriminants are `NUDXIPK`, `IndexPackLane` (`Exact`, `Lexical`), and the two row states. Gap is 2: the crate had fixed examples, and llvm-cov was not measured. Blast is 5 because the pack is a durable untrusted artifact. The harness cap is 8192 bytes. `encode_index_pack` needs a compiler snapshot, so the canonical seed is the 88-byte zero-segment header whose snapshot id is `IndexSnapshot::canonical_identity_from_slots`. `IndexPack::open` accepts that header.

`journal-codec` calls `JournalCodec::{decode,validate,encode}` on `EffectLog` and `DispatchLog`. Loc is `wc -l` of `effects/journal_codec` and `dispatch/journal/codec.rs` (763). `JournalError` has 6 variants and `DispatchRecordError` has 12. Discriminants are `EffectJournalRecord` (5), `AmbiguousReason` (4), and `DispatchRecord` (9). Gap is 2. Blast is 5: these records are durable. The cap is 4096 bytes. `canonical` is a prepared effect record (45 bytes). `corpus/dispatch` is a `PublicationPending` record (42 bytes) and must be accepted. The hash-chain frame and `pub(super) decode_record` in the store recovery journal stay out of this harness.

`native-protocol` calls `NativeEnvelope::decode` and requires `encode` to reproduce the input. Loc is `wc -l` of the four native payload files (867). `NativeProtocolError` has 20 variants. Discriminants are the 6 `NativeRecordKind` variants. Gap is 2. Blast is 5: the bytes are an untrusted helper payload. The cap is 4096, below `MAX_NATIVE_PAYLOAD_BYTES` (262144). The canonical seed is the encoder output of one declaration record (135 bytes). Linking `backend-compile` pulls semantic and tree-sitter; the fuzz derivation no longer shrinks the workspace member list, because `backend-engine` path dependencies are members.

`store-raw-property` calls `ValidatedFrame::validate`. Loc is `wc -l` of the production validator (908), not the 35-line `raw_property.rs` test module. `ValidateError` has 12 variants and `DescriptorError` has 10. Discriminants are the `NDX1` magic plus `SectionKind` (`Metadata`, `Rows`, `Data`). Gap stays 0 because `raw_property.rs` already runs an in-process property engine and scans every `u8`. The same file is the property sibling `store-frame`. Blast is 5: the error type describes untrusted frame bytes, and the frame is the durable store envelope. The harness cap is 4096 bytes, below `MAX_FRAME_BYTES` (1_048_576). There is no public encoder. The canonical seed is the 41-byte one-section literal. `structural_mutation_laws` stays `pub(super)`.

`flow-evaluator` is a structured grammar (`FLW1` plus 16-byte records), not a production decoder. It calls `reduce_rows`, `filter`, and `distinct`. The independent oracle is an `i128` sum. Loc is `wc -l` of `operators/support.rs`, `operators/stateless.rs`, `types/errors.rs`, and `batch/schema.rs` (772). `FlowError` has 23 variants. The only input discriminant is `FLW1`. Gap is 2. Blast is 3 because this grammar is not a trust boundary on the wire. `types/row.rs` and `types/time.rs` are value holders and are not in the loc sum.

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

`<id>` matches `[a-z][a-z0-9-]*`. `score.nix` must set `start_order` to a unique integer from 1 through 9. Nix aborts when two harnesses share one. The exported row key is `start_order`. This repo's discoverer reads `oracle.rs`; that filename is the Rust plug, not a schema field. Do not add a `[[bin]]` and do not edit `default.nix`. `tests/fuzz/build.rs` and `.config/nix/fuzz.nix` discover the directory. The optional alias is `fuzz-<id>`. Add a line to `tests/fuzz/instrumented` only when a new decoder crate joins the link closure. The list today is `backend_fuzz`, `fuzz_target`, `backend_engine`, `backend_compile`, `backend_semantic`, `backend_flow`, `backend_replication`, `backend_version`, `backend_store`, and `backend_platform`.

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

## Nix evaluation

Confirmed on this host on 2026-09-27. `x86_64-linux`. `nix (Determinate Nix 3.22.5) 2.35.2`.

`nix eval --json .#continuous-fuzz.metadata` on the tree that sets `latticeKind` exited 0 in 0.415 s. Stdout was 18633 bytes, including one trailing newline, sha256 `2a51f7fd523065b78ffc945f97ebc4feba83973f564724225ba5144fa12ac5df`. Top-level keys, in the order Nix printed them: `attrs`, `backlog`, `consumers`, `engine_catalog`, `formula`, `kinds`, `latticeKind`, `scales`, `schema`, `supervision`, `target_fields`, `targets`, `throughput_note`. `latticeKind` is `continuous-fuzz`. `schema` is `nudox.continuous-fuzz.v1`. An earlier eval, before `latticeKind`, was 18601 bytes, sha256 `2d6e4e8d362203975fb8a0466c254770932eb7c4e4cecd563269a9dc881f7264`.

`nix eval --json .#continuous-fuzz.engines.journal-codec.adapter` exited 0 in 0.580 s and printed:

```json
{"adapter":"bolero","family":"coverage-guided","id":"libfuzzer","linked":true}
```

`nix build .#continuous-fuzz -o /tmp/continuous-fuzz-result` exited 0 in 386.998 s before `latticeKind` was added. Inside that build, cargo reported `Finished release profile [optimized] target(s) in 6m 18s`. The result is `/nix/store/ravxgrh8bhl99523a62v16ggfh34p0is-continuous-fuzz`. `nix path-info -S` reported a closure of 1354046904 bytes. The output contains seven `bin/fuzz-*` links, seven `engines/fuzz-engine-*` links, seven `corpora/<id>/` directories, and `metadata.json`. That file matches the pre-`latticeKind` eval, not the 18633-byte document above. The journal corpus directory contains `canonical`, `empty`, `bad_magic`, `dispatch`, and `cancelled-root`. The engine script execs `/nix/store/rw7v93r0xyaa0agymyrs48n23lhgklhr-backend-fuzz-engine-0.1.0/libexec/fuzz-target` (24 MB). This amend did not rebuild the package. `latticeKind` is a metadata field, confirmed by the eval above.

From the 18633-byte stdout, `kinds` is `["fuzz", "property"]` and `formula` is `complexity.score * gap.score * blast.score`. Scheduled fuzz rows and their `rank` / `start_order`: `pack-decode` 27750/1, `journal-codec` 7990/2, `native-protocol` 8930/3, `store-raw-property` 0/4, `wire-workspace` 25570/5, `wire-replication` 14310/6, `flow-evaluator` 4776/7. Property rows `laws` and `store-frame` have `schedule` false and null `bin`, `corpus`, `rank`, and `start_order`. `engine_catalog` links `libfuzzer` (`bolero`) and `property` (`proptest`). `aflpp`, `honggfuzz`, and `go-native` are `linked` false.

## Measurements

Confirmed on this host on 2026-09-27. `rustc 1.99.0-nightly (375b1431b 2026-07-10)`. The binary is the dev-profile instrumented `fuzz-target` at `/tmp/fuzz-engine/debug/fuzz-target` (90 MB after `backend-engine` joined the link). The cold instrumented build of that closure finished in 291 s (cargo reported 4 m 50 s). Recompiling `backend-fuzz` after the journal oracle change finished in 6.17 s. An earlier cold instrumented build of only the wire harnesses finished in 12.69 s, and a later incremental rebuild of that smaller closure finished in 1.19 s. Those campaign rates are the dev-profile binary above. They are not the Nix release package. `nix flake check` was not run. Release-profile exec/s was not measured. The Nix commands that were run are in the Nix evaluation section. Campaign flags: `-timeout=10 -rss_limit_mb=4096 -max_total_time=10 -print_final_stats=1`, writable corpus first, committed `corpus/` second. Peak pulse is the largest `exec/s` on a libFuzzer `pulse` line. `NEW` lines in the first second print an `exec/s` equal to the unit counter; those values are not the pulse rate. The wire rows below were re-read from the saved campaign logs on that basis. Time to the first `NEW` for the three new targets was measured on a fresh process with `time.monotonic` around the same binary, because this libFuzzer build does not prefix lines with its own clock when stdout is not a terminal.

| Target | exec/s (`stat::average_exec_per_sec`) | Peak pulse exec/s | Time of the first `NEW` | `INITED` cov | First `NEW` cov | Final cov / features | Executed units | Writable corpus | Crashes |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- | ---: |
| `pack-decode` | 23189 | 18724 | 0.014 s (cov 389 → 390) | 389 | 390 | 415 / 462 | 255086 | 0 files → 13 files, 971 bytes | 0 |
| `journal-codec` | 56797 | 65536 | 0.013 s (cov 297 → 300) | 297 | 300 | 627 / 913 | 624771 | 0 files → 127 files, 11278 bytes | 0 |
| `native-protocol` | 61288 | 65536 | 0.013 s (cov 293 → 299) | 293 | 294 | 345 / 506 | 674173 | 0 files → 40 files, 5074 bytes | 0 |
| `store-raw-property` | 62288 | 65536 | 0.046 s (cov 433 → 436) | 433 | 436 | 550 / 635 | 685169 | 0 files → 32 files, 1016 bytes | 0 |
| `flow-evaluator` | 13542 | 16384 | 0.013 s (cov 754 → 755) | 754 | 755 | 1488 / 4932 | 148970 | 0 files → 299 files, 77834 bytes | 0 |
| `wire-workspace` | 39837 | 65536 | 0.011 s (cov 389 → 392) | 389 | 392 | 795 / 1514 | 438213 | 0 files → 136 files, 55374 bytes | 0 |
| `wire-replication` | 105078 | 131072 | 0.012 s (cov 230 → 231) | 230 | 231 | 955 / 1341 | 1155859 | 0 files → 124 files, 7190 bytes | 0 |

libFuzzer `DONE` lines: pack `cov: 415 ft: 462 corp: 13/1057b exec/s: 23189`; journal `cov: 627 ft: 913 corp: 127/11400b exec/s: 56797`; native `cov: 345 ft: 506 corp: 40/5207b exec/s: 61288`; store `cov: 550 ft: 635 corp: 32/1055b exec/s: 62288`; flow `cov: 1488 ft: 4932 corp: 301/76Kb exec/s: 13542`; workspace `cov: 795 ft: 1514 corp: 139/54Kb exec/s: 39837`; replication `cov: 955 ft: 1341 corp: 123/7243b exec/s: 105078`. `new_units_added` was 13, 389, 107, 63, 316, 371, and 148 in that order. Directory counts are a listing of the writable corpus after exit. The `corp:` field is libFuzzer's merged corpus, so the two counts differ. The native clock measurement's first `NEW` moved coverage 293 → 299; the 10-second campaign's first logged `NEW` was 293 → 294. Both starts were cold.

Re-entry used the same writable directories and `-max_total_time=3`. Pack loaded 13 durable files plus the 3 committed seeds and `INITED` at cov 415 (the 10-second run's final coverage); `new_units_added` was 0. Journal loaded 127 durable files plus the 5 committed seeds and `INITED` at cov 627. Native loaded 40 durable files plus the 3 seeds and `INITED` at cov 345. Store loaded 32 durable files plus the 3 committed seeds and `INITED` at cov 550 at 0.066 s. Flow loaded 299 durable files plus the 3 seeds; process output started at 0.054 s and `INITED` at cov 1488 at 2.172 s, which is the time spent replaying that corpus. Workspace loaded 136 durable files plus the 3 seeds and `INITED` at cov 795 at 0.060 s. Replication loaded 124 durable files plus the 3 seeds and `INITED` at cov 955 at 0.040 s. The 3-second runs' exec/s are startup-dominated and are not the rates in the table. Pack's 3-second average was 14571. Journal's was 7235. Native's was 37569.

### One-line harness change, then warm re-entry

Measured on 2026-09-27 against `/tmp/fuzz-runs/journal-codec/corpus` with a pty so libFuzzer flushed `INITED`. The clock is `time.monotonic` from process start.

Unchanged binary, 3 seconds: `INITED` at 0.065 s, `cov: 627 ft: 913`, `exec/s: 7371`, `new_units_added: 2`, directory listing 128 files to 130, 0 files in `artifacts/`.

One line in `tests/fuzz/targets/journal-codec/oracle.rs` then an incremental instrumented rebuild (`cargo build --offline -p backend-fuzz --bin fuzz-target`, 4.00 s). The line was a period added to the stable-encode error string, then reverted so the tree does not keep a probe. Same warm directory, 3 seconds: `INITED` at 0.058 s, still `cov: 627 ft: 913` (the previous useful coverage), `stat::average_exec_per_sec: 6238`, `new_units_added: 1`, directory listing 130 files to 131, 0 crash artifacts. Coverage did not grow. The 6238 exec/s figure is the 3-second average, which includes corpus replay, and is not the 10-second rate of 56797.

`cargo test --offline -p backend-fuzz` was run twice after the revert, without `--cfg fuzzing` or `--cfg fuzzing_libfuzzer`. Each run reported 10 passed, 0 failed. That is 0 failures in 2 smoke runs. A flake rate is not estimated from two runs. Unique crashes in the two journal re-entry runs: 0.

`cargo test -p backend-fuzz` on the same host after these harnesses: 10 passed, 0 failed. Libtest reported the library suite at 0.00 s. The test-profile compile of that invocation finished in 3.16 s because the dependency crates were already built. That is the smoke, not a coverage proof. Clippy `cargo clippy -p backend-fuzz --all-targets --no-deps -- -D warnings` exited 0.

## What other organizations do

**OSS-Fuzz** takes a project `build.sh` that links `$LIB_FUZZING_ENGINE` and writes one binary per target. The default engine list is libFuzzer, AFL++, Honggfuzz, and Centipede. The project does not pick the engine inside the harness. A seed zip sits beside the binary ([new project guide](https://google.github.io/oss-fuzz/getting-started/new-project-guide/)). Copy: one target list, seeds separate from the binary, engines as plugs. Do not copy `build.sh`, Centipede, or a per-language schema.

**ClusterFuzz** runs those binaries. A fuzz target is the thing that accepts bytes. A fuzzing engine (libFuzzer or AFL) is the thing that mutates and keeps the corpus. LibFuzzer and AFL jobs share the corpus for the same targets; the job name selects the engine ([libFuzzer and AFL++](https://google.github.io/clusterfuzz/setting-up-fuzzing/libfuzzer-and-afl/), [glossary](https://google.github.io/clusterfuzz/reference/glossary/)). Copy: `engine` is a field on the target, not a second package layout. Do not run ClusterFuzz, and do not point AFL++ at a binary whose catalog entry says `linked = false`.

**OneFuzz** splits a libFuzzer task into `setup` (the executable), `inputs` (the corpus that grows), `readonly_inputs` (extra seeds that are not written back), and `crashes` ([containers](https://microsoft.github.io/onefuzz/containers.html)). A new build replaces `setup` and keeps `inputs`. Copy: `bins.<id>` can be replaced while `durable_corpus` stays, and `corpora.<id>` is the read-only seed set. Do not adopt Azure containers or OneFuzz templates.

Go's `testing.F` is the adapter auth sets on `go-native`. It is an engine plug, not a second schema ([Go fuzzing](https://go.dev/doc/security/fuzz/)).

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
13. **Test-runner filters are not the lattice.** `.#continuous-fuzz` has no test-runner command and no check layout. Backend's library gate still excludes `binary(backend_laws)` and the store property names from the native-compiler group so a substring in `control.nix` cannot drop them. That filter is a Backend test runner. Auth does not copy it.
14. **Hand-listed packages drift.** Discovery is `tests/fuzz/targets/<id>/` plus `readDir`. Nix aborts when `score.nix` disagrees with its own sum, when `llvm_cov_percent` is set, or when a seed file is missing.
15. **Bounded smoke is not a proof.** Sixteen iterations can stay green while bugs remain. CI acceptance is the canonical seed plus replay.
16. **LibFuzzer abort versus the shrunk input.** On an oracle failure bolero prints the shrunk input and then aborts. The crash artifact may be the pre-shrink buffer. Triage copies the shrunk bytes from stderr into `corpus/` after checking the cap.
17. **`fuzzing_libfuzzer` alone does not compile.** The derivation passes `--cfg fuzzing` as well. CI passes neither.
18. **Uninstrumented libFuzzer is a false failure.** Without sancov, libFuzzer loads the seeds and exits 1 with `no interesting inputs were found`. Applying sancov to every crate fails the link of build scripts. The wrapper allowlist is `tests/fuzz/instrumented`.
19. **A one-shot temp corpus throws away coverage.** The durable directory is outside the nix store and outside the git tree. The re-entry run above reloaded it and `INITED` at the previous final coverage.
20. **Adding a harness used to mean a new Cargo bin and a new Nix function.** One `fuzz-target` binary and `FUZZ_TARGET` select the oracle. `.#fuzz-<id>` appears from the directory listing.
21. **Unlinked engines are catalog rows.** `engine_catalog` lists `aflpp`, `honggfuzz`, and `go-native` with `linked = false`. `bins.<id>` is the libFuzzer adapter. Do not exec those bins under AFL++ or honggfuzz. Auth sets `go-native.linked` and does not exec this repo's binary.
22. **Gap 0 makes rank 0.** `store-raw-property` already has an in-process property engine, so the product is 0. Sorting units by rank would start it last. `start_order` is a separate integer and `schedule` stays true. The property sibling is `store-frame`, which is not scheduled.
23. **Byte-identical journal encode is the wrong law.** A minimized `DispatchRecord::Terminal` of kind `Cancelled` decodes, and `encode` writes the unused 32-byte output root back as zeros. The first journal campaign aborted on that (libFuzzer exit 77, artifact 85 bytes, now `corpus/cancelled-root`). The oracle now requires a stable canonical encode: `decode(encode(record)) == record` and a second encode equals the first. It does not require the input bytes to equal that canonical form. The native envelope law stays byte-identical; its 10-second campaign did not trip it.

## Crash triage

A red `cargo test -p backend-fuzz` prints the bolero failure, including `[BOLERO_RANDOM_SEED=...]` for a random input. Minimize by copying those bytes into the matching `corpus/` file only when they are at most the harness cap. Proptest failures stay in `tests/laws/proptest-regressions/`.

An always-on crash lands in `artifacts/`. Do not file a decoder bug for an empty artifact directory or a wrapper exit 2. Reproduce with the artifact or the stderr shrink, then commit the minimized bytes under a flat name.

## MachineConfigurations

This repository does not configure runners. Build `.#continuous-fuzz` or the `.#fuzz-<id>` alias and exec `result/bin/fuzz-<id> /durable/fuzz/<id>/corpus`. Do not add those packages to `nix flake check`. The scheduled binary is the libFuzzer adapter. Catalog rows with `linked = false` are not runners.
