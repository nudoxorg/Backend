# Ryu native answer review (read-only)

Reviewed diagnostic artifact `/tmp/nudox-ci-green-fixed-evidence/replay.json` for source SHA `f11d4116d57b8d4c24f675059cd908f3dd61b17e` and artifact manifest SHA `72d2073a5b379755da9e8899e66f835abf8b3182ea1d8c84f86d39ed6f9e2d49`. This is a successful diagnostic replay, not an official acceptance or performance verdict.

## Independent source ground truth

Pinned source: `/tmp/nudox-w2-ryu-source-l7vfcgmn/ryu-1.0.18`; independent Syn AST listing: `/tmp/nudox-ci-green-ryu-syn-ast.tsv` (27 Rust files total).

The manifest's unique name-only library target is `ryu`, root `src/lib.rs`. The other named targets include bench `bench` at `benches/bench.rs`, example `upstream_benchmark`, and eight explicit tests. The default library module graph is exactly these 12 paths:

- `src/buffer/mod.rs`
- `src/common.rs`
- `src/d2s.rs`
- `src/d2s_full_table.rs` (default `not(feature = "small")` branch)
- `src/d2s_intrinsics.rs`
- `src/digit_table.rs`
- `src/f2s.rs`
- `src/f2s_intrinsics.rs`
- `src/lib.rs`
- `src/pretty/exponent.rs`
- `src/pretty/mantissa.rs`
- `src/pretty/mod.rs`

`d2s_small_table.rs`, parse/conversion modules, tests, example, and bench are outside this selected library closure. This establishes the expected closure independently from the returned semantic rows; it does not claim the public summary enumerates all 27 staged inputs.

## Packet verdict

**Approve the source-backed answers and retained selected scope; no substantive answer defect found.** Public observation records the exact selector `name: ryu`, `kinds: [lib]`, root `src/lib.rs`, selected source count 12, and closure witness `4f18628b75e653630fe9ab88a6b59854d7629b7d603f48e72bb328f08facfc30` (byte-array form). Profile observation is `published`, coverage `complete`, source count 12, with no refusal. The manifest and independently inspected graph agree. The 27-file total is supported by the independent source-frontier TSV, not emitted as a public profile count; therefore don't conflate the full input frontier with selected semantic coverage.

CLI search returns 23 Buffer rows, with the authored `struct Buffer` at `src/buffer/mod.rs:20` first and the `src/lib.rs:120` re-export immediately after. Other authored hits are all plausible target-closure results: trait `Float` at `src/buffer/mod.rs:105`; Buffer methods at lines 29, 47, 72, 87, 95; `format64` and `format32` in `src/pretty/mod.rs`; and `write_to_ryu_buffer` implementations at lines 112, 139, and 168. Two external `Buffer` rows have unknown language and no source path, correctly represented as external identities rather than attributed Rust declarations. The four unknown `type Buffer` rows have no fabricated source location. No authored answer points into bench, example, or test files.

CLI `format_finite` returns five plausible closure rows, led by the authored method at `src/buffer/mod.rs:72`; the other rows are Buffer, `format`, and pretty formatter helpers. Its summary says it prints into the buffer and returns a reference, which is accurate. Source docs at lines 55–68 make finite input a caller precondition; `Buffer::format` at line 47 handles nonfinite values before delegating. No answer incorrectly claims `format_finite` itself handles NaN or infinity.

CLI and MCP answers are `ready`; exact/names/graph/semantic coverage lanes are reported complete. `semantic_search` itself is separately `unavailable` because it is unconfigured, so these results should be described as lexical/source-backed retrieval rather than embeddings or semantic-vector evidence. MCP limit-three first pages and all continuations agree with the CLI anchors. Cursor hashes chain correctly for the CLI outline (6 pages), MCP outline (5 pages), Buffer MCP search (8 pages), and `format_finite` MCP search (2 pages). Both outline transports finish with complete extent. Their roots contain the Buffer struct, Float trait, and buffer module; neither includes bench/black_box/Bencher/test roots. No exact order/count assertion is warranted.

The source-capture summary explicitly describes selected closure coverage, not the 27-file staging frontier. The independent package inventory confirms that distinction. No existing Ryu answer golden was overwritten; this is suitable as a reviewed initial answer/baseline proposal, with the performance figures kept diagnostic-only.

Measured diagnostic figures in this artifact: CPU 71.75 s, peak RSS 1,329,532 KiB, CLI Add 90.6 s. These are evidence for this diagnostic run only; they do not demonstrate official acceptance or a performance improvement.
