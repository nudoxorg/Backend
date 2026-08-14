# docs/INTEGRATION-PLAN.md — what integration coverage exists vs. what the program needs

This is a coverage map, not a test-writing task list. It answers one question:
**for each seam in the dependency graph (docs/AGENTS-DOCTRINE.md §1), is there a
real test crossing it, and against what — a synthetic fixture, or a real
package?** Grounded in an actual inventory, not a guess: every number below
comes from `cargo nextest list --workspace --exclude index --exclude driver
--exclude ir-vcs --message-format json` (2026-08-04/05) plus a manual read of
every `tests/*.rs` file the tool surfaced. Where the picture has shifted
since (this repo had multiple agents editing it concurrently while this
inventory was taken — see "A note on how this was produced" below), the
inventory's *shape* is still the reliable part; exact counts will drift.

## A note on how this was produced

`cargo nextest list` at the repo root, with no arguments, **does not
currently work** — it tries to compile `workspace/index`, which has never
compiled (docs/LIMITATIONS.md L6), and `driver`/`ir-vcs` both depend on it. Every
number in this document comes from the workaround: `--exclude index
--exclude driver --exclude ir-vcs`. That leaves 16 of the 19 root-workspace
crates reachable, plus (separately, via `--manifest-path
workspace/gui/Cargo.toml`) the GUI's 4 test binaries, which are **not**
included in the counts below unless said so explicitly — see "The GUI
surface" near the end.

While gathering this inventory, two unrelated compile breakages were
observed and self-resolved within minutes, caused by other agents actively
editing `crates/nudox-store/src/source/producer.rs` and
`crates/nudox-engine/src/{chunk/head.rs,wire/mod.rs}` /
`workspace/compiler/languages/rust/src/ra/item.rs` in this same wave (the
last three are explicitly off-limits to this track). Two throwaway test
files (`crates/nudox-engine/tests/zzz_probe_l19.rs`,
`crates/nudox-store/tests/l14_probe_tmp.rs`, the latter referenced by
`Cargo.toml` but transiently absent from disk) also appeared and vanished
during this session — debug scratch from a concurrent agent, not part of
this inventory's baseline, and not this track's to clean up.

## Inventory: what exists, by seam

The dependency law (docs/AGENTS-DOCTRINE.md §1) is
`lindsey → nudox-engine → {nudox-graph, nudox-store → nudox-ir → nudox-producer → producer-{lang}}`,
with `nudox-mcp → {nudox-engine, nudox-graph}` alongside it. Below, "real"
means the test drives a real language toolchain (rust-analyzer for Rust)
over a real package checkout under `result/`; "fixture" means it
exercises the same code path against a small hand-built or golden corpus.

| Seam | Fixture coverage | Real-package coverage |
| --- | --- | --- |
| `nudox-producer-rust` (oracle → IR) | `nudox-producer-rust`'s own unit tests (24) | `nudox-store::real_crate` (20 tests, axum — **checkout absent**, skips), `nudox-store::real_package` (1 test, log-0.4.33 — present, runs), `nudox-engine::symbol_head_cfg`'s 1 real test (memchr-2.8.3 — present, runs), `nudox-engine::real_lineage` (2 tests, memchr×3 generations — present, runs) |
| `nudox-producer-{go,java,csharp,python,typescript,clang}` (oracle → IR) | `snapshot`/`lowering`/`producer_tests` per crate (3–27 tests each, 128 total) + each crate's own unit tests (12–47 each) | **None.** No `result/` checkout for any of these six languages exists, and no test file for any of them references one. This is the sharpest form of docs/LIMITATIONS.md L2 ("Only Rust is reachable from the GUI"): it is *also* true of the test suite — every non-Rust producer has only ever been run against a crate it was purpose-built to parse. |
| `nudox-store` (producer → `PackageView`) | `nudox-store::corpus_contract` (8 tests, golden fixture) | via `real_crate`/`real_package` above |
| `nudox-graph` (IR → Trustfall queries) | `fixture_queries` (12), `queries_parse` (1, all Trustfall `.graphql` files parse), `schema_parses` (1), `typename_invariant` (2), `error_channel` (3), `pushdown` (1) | **None.** No graph test runs a Trustfall query against a real-crate corpus; every query is checked against the small fixture graph. |
| `nudox-engine` (store/graph → wire protocol) | `hyperlink_flows` (15), `impls_refs_flows` (10), `multi_package_flows` (5), `packages_subscription` (4, needs `--features fixtures`), `protocol_adversarial` (10, needs `--features fixtures`), `search_adversarial` (10, needs `--features fixtures`), `versions_adversarial` (13, 1 `#[ignore]`d for an unrelated reason — see "Known small gaps" below), `zero_jump` (5) | `real_producer_links` (1 test, axum — **checkout absent**, skips), `symbol_head_cfg` (1 real test, memchr — runs), `real_lineage` (2 tests, memchr×3 — runs) |
| `nudox-mcp` (engine/graph → MCP tools) | `endpoint` (6), `schema_source` (3), `schemas` (17), `tool_integration` (31, fixture corpus) | `real_crate_tokio` (17 tests, tokio — **checkout absent**, skips) |
| `lindsey` (engine → GUI) | `shell_flow`, `adversarial` (separate workspace; not in this inventory's counts) | **None.** No GUI test loads a real crate; every GUI test runs against the engine's fixture corpus or a `StaticSource`. |

**The load-bearing fact this table makes visible**: of the five real-package
binaries that exist, **three currently skip rather than run**, because
`result/` has no `axum` or `tokio` checkout (only `log-0.4.33` and the
three `memchr` generations). "Real-crate coverage exists" is true of the
*code* (the tests are written, and two of the five binaries do execute
against real data), but false in practice for axum/tokio-shaped coverage
until someone runs the manual fetch steps in docs/TESTING.md.

## What EXISTS today (the honest positive case)

- **Adversarial coverage is real and broad** on the wire protocol
  (`protocol_adversarial`: cancellation mid-stream, opening a nonexistent
  symbol, opening a symbol in an unloaded package, page-prefix invariants)
  and on search (`search_adversarial`: non-ASCII queries, very long queries,
  cancelled-generation races, case-insensitivity) — this is exactly the
  AGENTS-DOCTRINE §4 bar ("empty input, one element, duplicate keys,
  cancellation mid-stream, out-of-order events, unicode, pathological
  nesting"), met for the two protocols that carry it.
- **Version lineage has synthetic coverage plus a small real slice**:
  `versions_adversarial` covers synthetic multi-generation scenarios
  (renames, signature changes, out-of-order arrival); `real_lineage` proves
  the same machinery against three real `memchr` releases end to end
  (docs/LIMITATIONS.md L10's identical-11329-entries question is a real-lineage
  test concern, not an untested one).
- **Every Trustfall query in the schema is proven to parse**
  (`queries_parse::all_trustfall_queries_parse`) — a query that stops
  parsing fails the build, not a silent runtime 500.
- **The `cfg` chip has a real-crate assertion**, not just a fixture one:
  `symbol_head_cfg`'s ignored test asserts
  `memchr::arch::aarch64`'s real `#[cfg(target_arch = "aarch64")]` reaches
  `SymbolHead.cfg` unmodified through the whole producer→wire→chunk path.
- **MCP's schema is checked against its own SDL twice**
  (`schema_source::served_sdl_is_byte_identical_to_the_graph_crates_schema_file`,
  `...parses_to_the_same_schema_nudox_graph_exposes`) — the tool schema and
  the graph schema cannot drift without a test noticing.

## What is MISSING — concrete, not aspirational

Starting from docs/LIMITATIONS.md's "Things we have NOT tested at all" section
and extending it with what this inventory pass found:

1. **Six of seven producer languages have zero real-package coverage**
   (table above). This is worse than "we haven't tried" — every non-Rust
   producer's fixture tests were written *by the same people who wrote the
   producer*, so a fixture that happens to avoid a real-world edge case the
   author didn't think of will never be caught. docs/LIMITATIONS.md L2 already
   says the GUI can't reach these languages; this inventory adds that the
   *test suite* can't reach them either, independent of the GUI.
2. **`#[cfg(feature = "…")]`-gated items are untestable, not just untested**
   (docs/LIMITATIONS.md L14): the producer never enables any cargo feature when
   loading a workspace, so no test — fixture or real — can currently assert
   a feature-gated symbol appears in the IR, because none ever do.
3. **`result/` is missing `axum` and `tokio`**, so 3 of the 5
   real-crate-group test binaries (`nudox-store::real_crate`,
   `nudox-engine::real_producer_links`, `nudox-mcp::real_crate_tokio`, 38
   tests total) skip rather than run in this environment. The `scripts/
   fetch-real-crate.sh` these tests' own doc comments point to does not
   exist (see docs/TESTING.md) — this alone means the *documented* path to
   closing this gap has never worked.
4. **No graph query has ever run against a real-crate corpus.** Every
   `nudox-graph` test (fixture_queries, pushdown, typename_invariant) uses
   the small hand-built fixture. `real_crate_tokio`'s `graph_query_*` tests
   are the closest thing to this and are all `#[ignore]`d/checkout-absent.
   Whether Trustfall's query planner scales acceptably against a
   14,000-entry real corpus (`libc`-sized) is untested.
5. **Sandbox's real-VM tests have never run, and their cost is unmeasured**
   (new finding this session, not yet in docs/LIMITATIONS.md):
   `sandbox::escape`'s 4 `#[ignore]`d cases and
   `smolvm_backend::tests::smoke_real_launch_succeeds_or_typed_unavailable`
   need `NUDOX_GUEST_ROOTFS` and a `smolvm`/`libkrun` binary on `PATH`,
   neither present here. `.config/nextest.toml`'s `slow` group placement for
   them is a judgment call (a VM boot is presumptively >10s), not a
   measurement — nobody has ever seen these pass or fail on real infra
   during this program's life, as far as this inventory can tell.
6. **`ir-vcs::bench_replay_vs_snapshot` cannot be reached by any tool**,
   nextest or otherwise, because `ir-vcs` depends on `index` (docs/LIMITATIONS.md
   L6) and does not compile. docs/TESTING.md's own "Benchmark" tier command has
   been silently unrunnable for as long as L6 has existed.
7. **The GUI screenshot suite has no automated cadence and no concurrency
   guard.** `workspace/gui/tests/screenshots.rs` is real, substantial
   (docs/AGENTS-DOCTRINE.md's whole "GPUI testing" section exists because of the
   care it took to get right), and — per this track's own finding while
   building `.config/nextest.toml` — cannot currently be grouped/serialised
   by any nextest config this track is in scope to write (see docs/TESTING.md,
   "The GUI is a separate workspace"). Nothing stops two people from running
   it concurrently with another GPU-touching process today.
8. **Concurrent multi-package loading under memory pressure** (existing
   docs/LIMITATIONS.md bullet): still untested. `real-crate` group's whole reason
   to exist — serialising ~600 MB-per-load tests — is a mitigation for a
   *known* problem, not a test that the engine behaves correctly when two
   packages load in parallel under real memory pressure. Nothing asserts an
   upper bound or graceful degradation.
9. **The MCP server has real-crate-corpus tests, but they're the least
   exercised part of the suite in practice**: `real_crate_tokio`'s 17 tests
   (including 9 distinct `graph_query_*` traversal cases — occurrence-target,
   module coercion, variant coercion, trait implementors, package/symbol
   members) are exactly the "MCP server against a real corpus" gap
   docs/LIMITATIONS.md calls out, and they exist in source — but they've never
   run in this environment (`tokio` checkout absent), so "real-crate MCP
   coverage exists" is a claim about code that has never executed here.
10. **Linux**: still entirely untested, as docs/LIMITATIONS.md already states.
    Nothing in this session's work changes that; `.config/nextest.toml`'s
    `real-crate`/`gui` groups are themselves reasoned about in
    macOS-specific terms (RSS ceilings measured on this host, AppKit/Metal
    for `gui`) and have not been validated on Linux even conceptually.
11. **Mutation testing**: wired (`cargo-mutants` via
    `full-check.nu --nightly`) but not run this session, per
    docs/LIMITATIONS.md's existing note. Unchanged by this track.

## Known small gaps (not part of the five-group taxonomy, worth naming)

- `nudox-engine::versions_adversarial::symbol_removed_in_later_version_gets_removed_row`
  is `#[ignore]`d for an unrelated reason ("needs `open_symbol` fallback to
  an older generation") — a real, small, open correctness gap in version
  lineage, distinct from the real-crate/real-VM ignore reasons everything
  else in this document is about.
- `nudox-ir`'s `continuity::tests::c10_lone_pair_forcing_outranks_threshold`
  is marked "UNVERIFIED — this test has never passed and is ignored
  deliberately" in its own source comment — an acknowledged-but-unresolved
  gap in the continuity/pairing algorithm predating this session.
- `nudox-mcp::real_crate_tokio::graph_schema_lists_all_five_new_distinct_types`
  is `#[ignore]`d for a documentation reason ("run with --ignored to
  confirm"), not a real-crate-loading reason — it is grouped into
  `real-crate` by this track's config anyway (binary-level matching, see
  `.config/nextest.toml`), which is a reasonable simplification but worth
  knowing if someone goes looking for why a schema-only test needs a tokio
  checkout to run (it doesn't, strictly, but it lives in the same binary as
  17 tests that do).

## The GUI surface (separate workspace, not in the counts above)

`workspace/gui` has 4 test targets: `shell_flow`, `adversarial` (both normal
libtest binaries), and `shot_probe`/`screenshots` (both `harness = false`,
real AppKit + Metal). docs/AGENTS-DOCTRINE.md's "GPUI testing" section documents
several real, previously-hard-won correctness properties this suite checks
(the two-clocks problem, stale-frame capture, opaque/non-flat-fill
assertions) — this is not a thin suite. What it does *not* do, per this
inventory: load a real package (see table above), or run under any
selective/grouped invocation (see docs/TESTING.md's "GUI group assignment is NOT
wired here").

## Recommendations (ordered by what closes the most gap per unit of work)

1. Fetch `axum` and `tokio` into `result/` by hand (docs/TESTING.md has the
   exact steps) — this alone flips 38 currently-skipping tests to running,
   for zero code changes, and is the single highest-leverage action this
   document can point at.
2. Write (or restore) `scripts/fetch-real-crate.sh` so the ~20 doc comments
   that already reference it stop being false, and so future real-crate
   checkouts don't require re-deriving the `[workspace]`-table trick by hand
   each time.
3. Pick one non-Rust producer (go and typescript both look closest, judging
   by fixture-test breadth: 47 and 27 tests respectively) and give it a
   `real_*`-shaped test file the same way `nudox-store::real_crate` does for
   Rust — a real, small, real-world package for that language, checked into
   a `.real-*-crates/`-style directory. This is the fastest way to convert
   docs/LIMITATIONS.md L2 from "the GUI can't reach it" to "the GUI can't reach
   it, but we've verified the producer, so wiring it up is real work, not
   discovery work."
4. Fix L6 (`index`). It is blocking, independently, a working
   `cargo test --workspace`, a working `cargo nextest list` with no flags,
   and the only benchmark test in the repository
   (`ir-vcs::bench_replay_vs_snapshot`).
