# ISSUES.md — the issue register

Consolidated from six independent mining passes (115 raw items), the live per-language
suite runs, and this session's fix attempts. Duplicates merged; the oldest id and the
strongest evidence win. Statuses are never inflated: a doc that says RESOLVED with no
checkable evidence is recorded as `UNVERIFIED_CLAIM`, not RESOLVED.

---

## Answering the three questions

**(a) What's left.** 34 issues are unresolved and unowned or open (5 `OPEN_UNOWNED`,
29 `OPEN`), 12 are partially resolved, and 5 documented claims are not backed by
evidence. The single most blocking item is not a bug: **the corpus has no authoritative
entry count.** Three in-tree sources disagree three ways for the same memchr versions
(1,322 / 1,347 / 1,835), and a stale hardcoded `EXPECTED_MEMCHR_2_8_3_ENTRIES = 11_329`
blocks the real-crate proofs for two blocker-severity IR defects (L19, L23) before those
tests reach their substantive assertion. Nothing measured at "scale" can be trusted until
that is re-baselined.

**(b) Did the per-language suites pass.** No — not as a set, and two of the "greens" are
hollow. Under the literal gate commands: C# green (58/58, genuinely real-package),
TypeScript red at audit and now fixed (32/32 after a verified product fix), Go / Java /
C++ red **only because of a nextest timeout-config gap** (all pass with an adequate
timeout), Rust and Python green-but-hollow — Python is 43/43 green while extracting
exactly 1 entry from each of 22 real packages, and nudox-producer-rust's only
real-package test passes in 0.014 s having executed zero assertions. Java's "pass" is
also a floor-check: it asserts that 5 of 22 Maven packages keep working, and the other 17
are still broken (L45, unfixed).

**(c) The register.** Below: 74 deduplicated issues, per-language suite results, status
counts, the ordered open-work plan, and the unbacked-claim list.

---

## Per-language suite status

Verdicts are as observed today. `GREEN_BUT_HOLLOW` means every test passed **and none of
them measured real-package behaviour** — doctrine §4's failure mode, and materially worse
than RED because RED is honest. The crate is named where the input's language label was
wrong.

| Crate (suite) | Verdict | Ran / Pass / Fail / Skip | Real packages exercised | One-line reason |
|---|---|---|---|---|
| `nudox-producer-csharp` | **GREEN** | 58 / 58 / 0 / 0 | 24 (Polly, Newtonsoft.Json ×2, Serilog ×2, AutoMapper, Dapper, MediatR, Moq, xunit, NUnit, StackExchange.Redis, … + System.Text.Json as a documented negative case) | Real GitHub-tag source through the real dotnet 10 / Roslyn oracle, hundreds–18k entries each; only 332 fixture lines, and even those run through the real oracle. |
| `nudox-producer-typescript` | **RED → fixed this run** | 32 / 31 / 1 / 0 → 32 / 32 / 0 / 0 | 17 npm tarballs (lodash, axios, rxjs, zod, date-fns, uuid, ws, chalk, …) | The one real-package test failed on 4 fixtures ("referred but never declared"); a genuine product fix landed and was confirmed by adversarial revert (TS-1). |
| `nudox-producer-go` | **RED (harness)** | 51 / 50 / 1 / 1 | 22 Go modules (zap, gin, cobra, viper, testify ×2, go-redis, prometheus, …) | `corpus_sweep` times out at nextest's 30 s catch-all cap; passes under `-P ci` with all 22 modules lowering against oracle-derived floors. Not hollow. |
| `nudox-producer-java` | **RED (harness + real gap)** | 36 / 35 / 1 / 0 | 6 Maven checkouts (gson ×2, commons-lang3, slf4j-api, mapstruct, jakarta.validation-api) | Same 30 s timeout kills `corpus_sweep`; and even when it passes it reports "5/22 succeeded" — L45 (javadoc invoked with no classpath) is unfixed. |
| `nudox-producer-clang` (input mislabels this "Rust") | **RED (harness) / product GREEN** | 23 / 22 / 1 / 2 | 20 C/C++ packages (abseil-cpp, simdjson, fmt, zstd, catch2, spdlog, range-v3, nlohmann-json ×2 …) | 20/20 lower with named-symbol assertions under plain `cargo test` (636 s); no nextest profile budgets enough time, and 2 tests are silently excluded by `binary(=real_package)`. |
| `nudox-producer-rust` | **GREEN_BUT_HOLLOW** | 31 / 29 / 2 / 1 | **none** | Every passing test is a hand-authored fixture; the one real-package test (`module_census`) is `#[ignore]`d *and* env-gated, so it returns at its guard and reports PASS in 0.014 s. |
| `nudox-producer-python` (input mislabels this "rust") | **GREEN_BUT_HOLLOW** | 43 / 43 / 0 / 0 | 22 PyPI sdists **touched but never read** | `invoke()` returns `PythonOracle::default()` unconditionally (pyrefly cannot compile); the sweep's own assertion is `table_len == 1` for sqlalchemy (1205 files) and six (16 files) alike. |

---

## The register

74 issues. Sorted by status urgency (`OPEN_UNOWNED` → `OPEN` → `UNVERIFIED_CLAIM` →
`PARTIALLY_RESOLVED` → `RESOLVED`), then by severity (blocker → major → minor → cosmetic).
Ids suffixed `-cs` / `-ic` are proposals from CORPUS-SWEEP.md / INDEX-CAPABILITY.md that
**collide with already-assigned LIMITATIONS.md ids** — see `ID-COLLISION`.

| ID | Title | Area | Severity | Status | Evidence |
|---|---|---|---|---|---|
| L50-cs | Corpus entry counts disagree three ways; no measurement is authoritative | corpus-build | blocker | OPEN_UNOWNED | L1 banner 1,322/1,325 vs L38 "true counts" 1,344/1,347 vs CORPUS-SWEEP.md:573-577 1,825/1,835 for the same versions; corpus total 55,449 (L1) vs 79,273 for crates.io alone (sweep). No LIMITATIONS id assigned. |
| L45-cs | 26 of 154 corpus entries (16.9%) return Ok with a stub table and are counted as successes | corpus-build | blocker | OPEN_UNOWNED | All 22 Python packages → exactly 1 entry (python `producer.rs:46-59` discards `src`); 4 TypeScript packages → 2–3 entries (CommonJS invisible to ES-only `graph.rs`); `ws` emits `Const{ty:Any, value:"require('./lib/websocket')"}` — source text as a value. No LIMITATIONS id. |
| L48-cs | No const-expression representation in nudox-ir; value-level API info dropped by 6 of 7 producers | ir | major | OPEN_UNOWNED | csharp `lower.rs:1200` self-flags "KNOWN IR GAP"; java `annotation_attrs()` always sets `AttrTok::arg = None`; clang `OracleParam` has no default field; TS `Const.value` is unevaluated source. No LIMITATIONS id. |
| L49-cs | `ParamAttribute` collapses distinct calling conventions; `*args`/`**kwargs` indistinguishable | ir | major | OPEN_UNOWNED | `workspace/ir/model/src/kinds/param.rs:35-63` has no `Out` variant (C# `ref` and `out` both → `Inout`) and one `Variadic`; python `emit/mod.rs:389-390` maps `Varargs` and `Kwargs` to it. Found during synthesis, reported by no miner. |
| L48 | Nothing has been tested on Linux; all evidence is macOS/aarch64 | gui | minor | OPEN_UNOWNED | Cannot be proven either way from this host (Darwin). No Linux CI or test artifact found; doctrine's GPUI notes are all AppKit-specific. |
| L43-ic | No delta/changeset representation anywhere in ingest → store → search | index-search | blocker | OPEN | `index/ingest/enumerate.rs:108` has no cursor/since parameter; `index/store/apply.rs` emits an outbox upsert unconditionally in the `UpsertVersion` arm; engine `search.rs` re-loops every package per query (~150-165, ~345-355, ~448-456). A stem with 300 releases re-emits 300 upserts every poll, forever. |
| L44-ic | HashMap iteration order reaches the user as search-result order | index-search | blocker | OPEN | Reproduced independently: 4 runs of `real_search_benchmarks memchr_query_ranks_public_api_above_same_named_internal_module` gave 4 different rankings (public `memchr` at #7/#3/#2/#5). `view.rs:154` and `apply.rs:289` both iterate `HashMap` (`apply.rs:107`); `runtime.rs:710-714` comments that iteration is insertion order — false. |
| L48-ic | Every measurable subsystem's default configuration is a fake | index-search | blocker | OPEN | `index` defaults to the in-memory `test-engine` facade (its own header: "honest fakes"); the real `dolt-engine` cannot build (L7); `SECTION_SEMANTIC` hardcoded empty at `search.rs:227-228`; sole `impl FeedTransport` is `FixtureTransport` (`transport.rs:103`); sole `WatermarkStore` is `MemoryWatermarkStore` (`watermark.rs:102`). |
| L5 | serde_json times out (>300 s) during lowering; cause undiagnosed | language-frontend | major | OPEN | `CORPUS-REPORT.md:34,61` unchanged; grep across the repo finds no diagnosis, profile artifact or code comment referencing it. |
| L8 | No remote/upstream package store; corpus is local-checkout only and no fetched package is tested | corpus-build | major | OPEN | `crates/nudox-store/src/source/` contains only `producer.rs` and `fixtures.rs`; grep for `fn fetch` / `https://` / registry hosts → nothing. Only network code is dev-tooling `corpus/fetch.nu`. |
| L21 | Vendored `trustfall/` is untracked and carries a nested `.git` (gitlink risk) | corpus-build | major | OPEN | `git status --porcelain trustfall` → `?? trustfall/`; `trustfall/.git` exists; `git ls-files trustfall` → 0; root `Cargo.toml:132` still git-pins the dependency rather than using the local path. |
| L25 | `cargo nextest` cannot run the workspace without `--exclude` | corpus-build | major | OPEN | Bare `cargo nextest list` fails today: `ir-vcs` lib-test → 143 errors in `workspace/ir/vcs/f1/tests.rs` (KindWire, EnumWire, F1View, … not in scope), plus a duplicate-symbol link error in `driver` between libzstd_seekable and libzstd_sys. The `--exclude` form is clean. |
| L31 | No per-item source jumping — a producer span-emission gap, not GUI wiring | language-frontend | major | OPEN | `span: 0..0` is the only pattern in 5 producers (go 1, java 5, csharp 8, clang 3, python 6); `wire/mod.rs:862-890` `ImplRow` has no source field; `ra/source.rs:57` emits `<file-id-{}>`. Frame `10-source-tab.png` shows one whole-symbol byte range (8880–9435), never a per-item link. |
| L35 | The MCP server is never started by the application | mcp-daemon | major | OPEN | `grep -rn 'nudox_mcp|McpEndpoint|NudoxMcpServer' workspace/gui` → 0 hits; `set_mcp_endpoint` (`status_bar.rs:105`) has exactly one occurrence repo-wide — its own definition. |
| L37 | The local engine has no daemon, no file watching, and four no-op stubs | mcp-daemon | major | OPEN | `crates/nudox-engine/src/lib.rs:426-457` — `resolve_project`, `sync`, `jobs`, `command` each still documented "**Stub (M3/M4)**" and each returns an immediately-done/closed receiver. `grep 'use notify::'` → nothing. |
| L40 | C# namespaces are extracted and then thrown away | language-frontend | major | OPEN | `csharp/src/lower.rs:82-97` iterates only `extraction.types`, never `extraction.namespaces`; `aliases_from_doc_id` is called only from `type_symbol` (:109), never from `member_symbol` (:130-149). |
| L41 | Semantic search is a UI affordance with no engine behind it | index-search | major | OPEN | `search.rs:74-81` `SearchQuery{text,kinds,limit}` — no mode field; `SECTION_SEMANTIC` documented "Reserved" (:22,:60) and emitted empty (:227). GUI half is now honest: `04b-semantic-mode.png` shows a real empty state, not a dead tab. |
| L42 | Three GUI defects whose fix is in the backend (stale generation, opaque file-id path, ImplRow has no source) | gui | major | OPEN | `ra/source.rs:57` `<file-id-{}>`; `wire/mod.rs:862-890` ImplRow; `PackageLoadEvent::Loaded` emitted only at `runtime.rs:232,:616`, so the project panel keeps showing v2.7.6 while the reader serves 2.8.3. |
| L45 | Java lowers 5 of 22 Maven packages: javadoc is invoked with no classpath | language-frontend | major | OPEN | `java/src/producer.rs:99-119` builds its javadoc args from `-quiet -doclet -docletpath` + optional `--release` + sources — no `-classpath`/`--module-path` anywhere. Live `corpus_sweep` prints "maven corpus sweep summary: 5/22 succeeded". |
| L45-ic | Search ranking has no visibility or kind input; public API is outranked by private detail | index-search | major | OPEN | Same 4 live runs: `vis=Crate`/`vis=Private` modules tie at score 1.000 ahead of the public `fn memchr`; `Memchr` holds #0 only because 'M' byte-sorts before 'm'. `EntryInner::Reference` has `kind().discriminant() == None`, so the clean `pub use` alias is structurally unreachable by name search. |
| L47-ic | 16 adversarial vector-plane tests are in no build target under either build system | index-search | major | OPEN | `find workspace/registry/tests -maxdepth 1 -type f` → nothing (Cargo needs top-level `*.rs`); `tests/vector/` has 16 files; `cargo metadata --no-deps` lists exactly one target (`lib`) for `registry`. They have never compiled. |
| ID-COLLISION | Proposed LIMITATIONS ids L43–L50 collide with already-assigned entries; four findings are untracked | docs-claims | major | OPEN | L43/L44/L45 already taken (`LIMITATIONS.md:2132/2216/2253`); doctrine §8 already uses L46 for link repair; CORPUS-SWEEP's L45/L48/L49/L50 appear nowhere in LIMITATIONS.md under any id. Pasting either proposal verbatim overwrites real content. |
| NX-1 | `.config/nextest.toml` has no slow-timeout override for real-corpus binaries — 4 producer suites report false RED | corpus-build | major | OPEN | go/java/cpp `corpus_sweep` and rust `dependency_resolution` fall through to the `integration` catch-all (default 10 s × 3 = 30 s). Same tests pass under `-P ci` or plain `cargo test` (java 57.1 s, cpp 636 s, rust 39.3 s). |
| NX-2 | `default-filter binary(=real_package)` silently hides real-package tests in ≥2 crates, even under `--run-ignored all` | corpus-build | major | OPEN | go and clang each have a `tests/real_package.rs` with zero `#[ignore]`s that nextest never starts under either specified command; both pass under `cargo test --test real_package`. The filter was written for a different crate that collides by filename. |
| RS-1 | `nudox-producer-rust` has no real-package coverage; its one real-package test passes with zero assertions executed | language-frontend | major | OPEN | `module_census.rs` is `#[ignore]`d *and* gated on `NUDOX_PKG_ROOT`/`NUDOX_PKG_NAME`/`NUDOX_CENSUS_OUT`, which nothing in `.config/scripts` sets; under `--run-ignored all` its `else { eprintln!("SKIP"); return }` fires and nextest reports PASS in 0.014 s. |
| L7 | Vendored C sources (qdrant-edge, doltlite) are absent from the checkout | index-search | minor | OPEN | `ls workspace/vendor/qdrant-edge/cpp` and `ls workspace/vendor/doltlite/doltlite.c` both ENOENT. Blocks the real dolt-engine (L48-ic) and the only criterion dependency in the tree. |
| L26 | sandbox's real-VM tests cannot run here (no smolvm/libkrun, `NUDOX_GUEST_ROOTFS` unset) | mcp-daemon | minor | OPEN | `which smolvm libkrun` → nothing; env var unset; `workspace/compiler/sandbox/tests/escape.rs` present but unrunnable. |
| L27 | Bracket interpretation is decided per symbol, not per link attempt | ir | minor | OPEN | `prose.rs:108,:143` still gate on the single per-symbol bool `doc_link_table.has_declared_links()`; `entry/symbol.rs:97-102` `DocLink` carries only `target`/`label` — no byte span, so per-attempt granularity is architecturally impossible today. The stale-comment sub-issue is fixed (`doc_link_table.rs:231-244`). |
| L43 | pyrefly cannot be enabled — blake3 conflict, plus a missing module | language-frontend | minor | OPEN | `python/Cargo.toml:37-49` has the dependency commented out and `pyrefly = []` as an empty feature. Tighter blocker found live: `cargo check -p nudox-producer-python --features pyrefly` fails because `src/context.rs`, referenced by `#[cfg(feature="pyrefly")] pub mod context;`, does not exist on disk. |
| L46-prop | Concurrent multi-package loading under memory pressure — untested | index-search | minor | OPEN | No test under `nudox-engine/tests` or `nudox-store/tests` matches by name or content; nearest is `real_search_benchmarks.rs`, a latency benchmark. NOTE: the proposed id L46 collides with doctrine §8's L46. |
| L49 | Mutation testing (cargo-mutants) has never been run | ir | minor | OPEN | `.config/scripts/full-check.nu:100-105` wires `cargo mutants --package nudox-engine` into the nightly tier; `find . -iname 'mutants.out*'` → empty. |
| F1 | `qualified_display_name` repeats ancestor segments ("memchr.memchr.arch.generic.") | index-search | minor | OPEN | Visible in frame `04-search-hits.png` and in live search output (`memchr.memchr.memchr.memchr_raw`); traced to nudox-engine by `search_model.rs:470-480`'s own doc comment. Orchestration task #14 pending; the sibling `resolve_nominal` collapse (task #13) is done. |
| DOCSRS-5-streaming | Nothing distinguishes "streamed from a local engine" from a static render in any frame | gui | minor | OPEN | Only evidence is the MANIFEST.md row-10 caption, which SHOT-REVIEW.md's own rule says is not evidence. No mid-load/partial frame exists in the 28-frame set; no LIMITATIONS entry, no test. |
| TS-2 | `real_npm_packages`' doc comment claims a per-entry identifier assertion the test body does not implement | language-frontend | minor | OPEN | Comment promises "at least one entry's name is a real, non-empty identifier with no path separator"; the body contains only the `entry_count == 0` check. Doctrine §8: the comment is the bug. |
| DOCTRINE-7 | "As of 2026-08-05 the ENTIRE workspace compiles … the `--exclude` list is now a speed optimisation, not a necessity" | docs-claims | major | UNVERIFIED_CLAIM | `AGENTS-DOCTRINE.md` §7:187-190. Falsified by L25 — the exclude list is still load-bearing. Doctrine's own re-check loop (`cargo check -p $p --lib`) omits `--all-targets`, so it cannot see the failing test targets. |
| IC-3.5 | "No relevance measurement anywhere in this repository" (INDEX-CAPABILITY §3.5) | index-search | major | UNVERIFIED_CLAIM | False as written: `workspace/index/search/ranking/eval.rs` is a real 316-line nDCG@k harness with 8 passing unit tests. Substance survives narrowed: `mean_ndcg`/`evaluate` are called only from its own `#[cfg(test)]`, there are zero golden JSON fixtures, and it scores `cascade::rank_full` — which has zero callers in engine/driver/gui. |
| no-benchmarks | "We have benchmarks for scalability" | index-search | major | UNVERIFIED_CLAIM | Zero `[[bench]]` targets and zero criterion dev-deps in the real workspace (the one hit is vendored qdrant-edge, which cannot build — L7). The actual mechanism is `nudox_test_support::measured()`, a single-sample stopwatch inside an ordinary `#[test]`, on a host doctrine §8 records as 2×–4.6× noisy. No document states plainly that there is no benchmark suite. |
| L10-banner | LIMITATIONS L10's banner "SETTLED, lineage is now PROVEN end to end" | ir | major | UNVERIFIED_CLAIM | The entry contradicts itself: its own trailing Status line still reads "PARTIALLY RESOLVED … content-level lineage not yet demonstrated". Its 1,322/1,325 figures match neither L38's 1,347 nor this session's live 1,835. Register keeps L10 at PARTIALLY_RESOLVED. |
| L41-cite | LIMITATIONS L41's "registry's vector plane exists and is heavily tested (tests/vector/, 15 files)" | index-search | minor | UNVERIFIED_CLAIM | 16 files, not 15 — and none is in any build target, so none has ever compiled or run (L47-ic). The citation supports the opposite of what it is used for. |
| L19 | Rendered signatures drop generic wrappers (`Option<usize>` renders as `usize`) | ir | blocker | PARTIALLY_RESOLVED | Cause (a) rebuilt: `ir/model/src/foreign.rs` ForeignKey/ForeignResolver, `package/seal.rs:435-468` links `Ref::Foreign` at seal, `signature.rs:788-800` renders via `key.display`. `cargo test -p nudox-engine --test generic_signature_shapes -- --ignored` → 2 passed (its `#[ignore]` reason string is now stale). No real-crate proof: `real_memchr_generic_return.rs:80` asserts `EXPECTED_MEMCHR_2_8_3_ENTRIES = 11_329` vs actual 1,835 and dies first. |
| L23 | Symbol identities collide: 416 functions reduce to 188 identities | ir | blocker | PARTIALLY_RESOLVED | `seal.rs:302-403` adds a general escalation pass re-minting collided IntroIds (Disambiguator::Span → Ordinal). The named mechanism is still present verbatim: `ra/item.rs:626-636` declares Params under the enclosing parent, `item.rs:2265-2278` `plain_sym` hardcodes `span: 0..0`. Verified only on synthetic fixtures — the real-memchr proof is blocked by the same stale 11,329 assertion. |
| L39 | External type identity discarded — `Ref::Foreign` construction and collision handling | ir | blocker | PARTIALLY_RESOLVED | `Ref::Foreign` is genuinely constructed (`lower.rs:288`, `index.rs:341-415`); `apply.rs:144-161` `insert_live` now panics naming both incumbent and rejected declaration via typed `IntroCollision` (:177-197). Gap real: TypeScript still emits `Primitive::Builtin` (`emit.rs:950-960`) and clang `Type::TypeVar` (`lower.rs:505-518`) for unresolved names — 4 of 7 producers. Not adversarially verified. |
| L1 | IR generation cost is ~95% fixed rust-analyzer boot overhead, not proportional to output | corpus-build | major | PARTIALLY_RESOLVED | 2 of 4 vectors landed: `ra/loaded.rs:393` gates `run_build_scripts` on `workspace_has_build_script`, `:430` documents removing the eager `parallel_prime_caches`. Boot is still paid per package: measured `wall_ms=50921` and `wall_ms=55245` for one package each this session. |
| L2 | Six of seven languages are reachable through `ProducerRegistry`; Python deliberately gated | language-frontend | major | PARTIALLY_RESOLVED | `producer.rs:195-206` registers Rust/TS/Go/Java/C#/clang unconditionally, Python only under `#[cfg(feature="pyrefly")]`. `cargo test -p nudox-store --features fixtures --lib` → 48 passed, 0 failed, including the six `with_all_available_resolves_a_runner_for_*` tests. |
| L10 | Lineage/version picker proven only on memchr; content-level lineage undemonstrated | ir | major | PARTIALLY_RESOLVED | Picker works in the GUI: `13-version-picker.png` now distinct (MD5 0e6dce4d), three generations listed, rows wired at `header.rs:580-688`. But `real_lineage.rs:260,:436` are both still `#[ignore]`d; the sidebar shows a stale "v2.7.6 · 1825 symbols" (L42 item 1); entry counts unsettled (L50-cs). |
| L15 | Keybindings exist, are documented, and 7 still do nothing | gui | major | PARTIALLY_RESOLVED | Real handlers at `shell.rs:905,937`; NavigateBack/Forward/ToggleHud/DeepLinkLine deleted rather than left dangling (`keymaps.rs:130,204,633,739`). `keymaps.rs:853-883` still carries `PENDING_VIEWS = ["GraphView","PackageBrowser"]` — the 7 bindings await views that do not exist. |
| L28 | Nearly the entire real-crate corpus was measured under a silent `--no-deps` degradation | corpus-build | major | PARTIALLY_RESOLVED | Typed fix landed and is undocumented as such: `loaded.rs:101-137` `DependencyResolution{Full,NoDeps}` + `accept_degraded_dependencies` (:215), with `require_resolved_dependencies()` as the sole choke-point at `ra/mod.rs:83-85`, backed by `RustProducerError::DependenciesUnresolved` (`error.rs:19-47`). Offline resolvability confirmed (`.real-crates/memchr-2.8.3/.cargo/config.toml`). Re-baseline and the memchr-2.7.6 anomaly remain open. |
| L34 | Non-Rust producers are registered but never exercised through nudox-store/nudox-engine; oracles are untracked | language-frontend | major | PARTIALLY_RESOLVED | Oracle source now exists at `workspace/compiler/languages/{go,java,csharp}/oracle/` — but `git status` shows all three as `??`, so a clean checkout of HEAD lacks them. `grep -rln 'GoProducer|JavaProducer|CSharpProducer|ClangProducer|TypescriptProducer' crates/nudox-store/tests crates/nudox-engine/tests` → zero files. |
| L32 | Package provenance is thin (no owner, deps, repo, license, release date, coverage) | gui | minor | PARTIALLY_RESOLVED | `project_panel.rs:209-239` now shows ecosystem and generation count; lines 214-224 document that `PackageLoadEvent::Loaded` carries only `{name, ecosystem, version, symbol_count, root}` and that the rest was deliberately not fabricated. Blocked upstream in the wire, not a GUI oversight. |
| L3 | `Symbol::cfg` wired end to end; `attrs`/`aliases` still unpopulated; no target/feature page | language-frontend | minor | PARTIALLY_RESOLVED | `chunk/head.rs:272-294` wires `entry.sym().cfg` into `SymbolHead.cfg`; `symbol_head_cfg.rs` covers the real `memchr::arch::aarch64` case. Nine hardcoded `cfg: None` construction sites remain in `ra/item.rs` (260, 435, 594, 1114, 1503, 2165, 2276, 2288, 2317) — the doc's count of 5 is stale. |
| L9 | Corpus fixtures are Nix-pinned / hash-verified across seven ecosystems | corpus-build | minor | PARTIALLY_RESOLVED | `corpus/manifest.toml`: exactly 140 `[[packages]]`, 20 per ecosystem × 7, 154 `hash =` lines — matches the claimed table. Disclosed gaps (flake.nix `buildCorpus` handles crates.io only; NuGet fetches binaries not source) not re-verified this session. |
| L6 | The `index` crate and ir-vcs's 115 errors now compile and their tests run | index-search | blocker | RESOLVED | `cargo check -p index --lib` clean; `cargo test -p index` passed; `cargo test -p index --lib` → 731 passed, 0 failed; `-p ir-vcs --lib` and `-p driver --lib` also clean. Caveat: test targets are still broken — see L25. |
| L16 | View-scoped actions on SymbolPage did not reach handlers (no Focusable/track_focus) | gui | blocker | RESOLVED | `symbol_page/mod.rs:1961` implements `focus_handle`, `:417/:1416` apply `.track_focus`. `cargo test --manifest-path workspace/gui/Cargo.toml --test adversarial -- activating_a_tab_focuses_the_symbol_page_not_the_pane` → 1 passed; asserts real clipboard content, not a repaint flag. Version picker now opens (frame 13). |
| L22 | A second render root deleted the ancestor focus/context stack | gui | blocker | RESOLVED | `symbol_page/mod.rs:1383-1420`: `page_root` is the sole producer of the root element (id, key_context, track_focus, on_action applied once); `page_body` owns every `PageState` branch and is handed no root. Same adversarial test passes. |
| L24 | `nudox-producer-clang` no longer link-depends on libclang | language-frontend | blocker | RESOLVED | `otool -L target/debug/deps/nudox_store-942abe109caaa47e` on a freshly built binary → CoreFoundation, CoreServices, libiconv, libSystem only. Zero libclang references, with `LIBCLANG_PATH` unset. |
| L38 | 88% of memchr's IR was core, not memchr — every entry count ~8× inflated | ir | blocker | RESOLVED | `loaded.rs:523` `STD_SHIM_PACKAGE_PREFIX = "rustc-std-workspace-"` with exclusion logic at :278-310 and :519-665; live runs print `std_integration_features_excluded=[core,rustc-dep-of-std]`. Caveat: the `visible_from`/anchor half was not re-derived, and its "true count" 1,347 disagrees with this session's 1,835 (L50-cs). |
| L4 | bytes-1.11.0 could not be lowered: a bench target shared the crate name | language-frontend | major | RESOLVED | `cargo test -p nudox-store --test bytes_bench_name_collision_regression -- --ignored --nocapture` → "lowered 2058 entries from bytes-1.11.0 in 50.9s (1 `Bytes` struct, 0 bench leaks)", passed. Content assertion, not `is_ok()`. |
| L11 | `#[macro_export]` crates could not be lowered | language-frontend | major | RESOLVED | Fix routes `ModuleDef::{Function,Const,Static,Macro}` through the existing `id_of()` authority; log-0.4.33 → 419 entries, the same baseline doctrine §8 independently cites. **Not re-run this session** — the weakest RESOLVED row in this table. |
| L14 | Every `#[cfg(feature = "…")]`-gated item was missing from the IR | language-frontend | major | RESOLVED | `loaded.rs:255` loads with `CargoFeatures::All`, `:305-326` drops only rustc-dep-of-std shims. Canary flipped positive: `feature_gated_api.rs:211-212` now asserts `!into_owned_owners.is_empty()`; `cargo test -p nudox-store --features fixtures --test feature_gated_api -- --ignored` → both pass ("found 5 into_owned"). Stale disclaimer remains at `symbol_head_cfg.rs:26-41`. |
| L17 | Unresolved intra-doc links leaked raw markdown to the reader | ir | minor | RESOLVED | Root cause documented at `chunk/walk/doc_link_table.rs:33-72` (CommonMark tokenizes the upstream typo as a code span); fix in `prose.rs consume_bracket_run`. `cargo test -p nudox-engine --lib -- chunk::walk::tests::real_memchr_doc_comment_leaks_no_bracket_for_any_link chunk::walk::tests::adversarial_cross_crate_link_leaks_nothing` → both ok, reproducing the verbatim upstream comment. |
| L20 | Search results indistinguishable when leaf names collide (7 pixel-identical rows) | index-search | major | RESOLVED | `search.rs:297,704` `finalize_candidates` qualifies only colliding leaves. Three tests run, including `--ignored real_memchr_leaf_collisions_get_distinct_display_names` → 22 distinct names. GUI side: `search_model.rs:220-259` `RowQualifier` + private `ingest`, asserted by `screenshots.rs:586-636` on frames 04 and 04c. Residual noise tracked separately as F1. |
| L29 | A TODO was rendered as user-facing UI text in the Jobs panel | gui | major | RESOLVED | `shell.rs:125-292` `placeholder_panel!` renders `ui::EmptyState` with real copy. The requested guard exists: `workspace/gui/tests/adversarial.rs:1740-1760` `no_rendered_string_carries_developer_scaffolding` scans string literals for `TODO(`/`FIXME`, with a lexer self-test at :1765-1793. |
| L30 | The reader wasted ~60% of its viewport; "ON THIS PAGE" listed one entry | gui | major | RESOLVED | `symbol_page/mod.rs:189-249` adds `IMPLS_AUTO_EXPAND_MAX = 12` and a `Disclosure::{Auto,Chosen}` type; `outline.rs:43-59` adds Implementations/References/Source and wires each entry's `on_click` (:322) and `scroll_to_item` (:225). Frame `08-symbol-opened.png` (MD5 c62dbb68). GUI-WORKORDER-2.md is stale and still shows F2/F3 unclosed. |
| L33 | `Language::Cpp` resolved to no runner; Python registered a producer that could not produce | language-frontend | major | RESOLVED | `producer.rs:204/:250` `register_c_and_cpp` wires one runner under both C and Cpp; Python only under the feature. Three named regression tests found verbatim (:785, :856, :864); the fourth exists under a drifted name (:805). |
| L36 | No end-to-end MCP test combining real transport and a real lowered corpus | mcp-daemon | major | RESOLVED | `crates/nudox-mcp/tests/real_crate_memchr.rs` starts a real `McpEndpoint` over a real lowered memchr and asserts on decoded JSON-RPC bodies. `cargo test -p nudox-mcp --test real_crate_memchr -- --ignored --nocapture` → ok, `cost case=mcp_e2e/memchr-2.8.3 wall_ms=48976.9`. Note: the file is untracked in git. |
| L44 | Declaration identity was a lossy string projection in 5 of 7 producers | language-frontend | major | RESOLVED | 2 of 5 spot-checked and real: go `lower/mod.rs:533-537` falls back to positional index for blank params; java `lower.rs:214-219` erases a bounded typevar to its bound (JLS 4.6). Safety net `LoweringError::Duplicate` present at `ir/model/src/lower.rs:90`. TS/C#/Rust fixes not re-derived. |
| L46 | Silent doc-link repair is now typed, bounded, counted and visible | ir | major | RESOLVED | `wire/repair.rs` `LinkRepairKind` is a closed enum with no wildcard anywhere; `LinkOrigin` is mandatory on `InlineRun::Link` with no Default (`wire/mod.rs:590`); GUI paints a wavy warn underline + tooltip (`docs.rs:282-289, 386-390, 1197-1202`), proven by `repair_paint` (1560 px differ). Guard verified by mutation: breaking `DelimiterShape::repair_kind` turned exactly 5 tests red across 3 files; restored → 192 passed, 17 skipped. |
| TS-1 | 4 real npm fixtures (zod ×2, date-fns, uuid) failed to lower: "referred but never declared" | language-frontend | major | RESOLVED | Product fix: `LocalExport`/`locals` map distinguishing an export's external alias from its internal declared name, plus a single `resolve_export_target` choke-point and a `default_import_spans` correction. `cargo nextest run -p nudox-producer-typescript --run-ignored all` → 32/32. Causation proven by adversarial revert: only that function reverted reproduced exactly those 4 packages and no others. No assertion weakened. |
| L18 | Breadcrumb repeated the package name three times (memchr › memchr › memchr) | gui | cosmetic | RESOLVED | `symbol_page/header.rs:194-206` `collapse_repeated_crumbs`, called from `HeaderModel::from_head` (:281); parallel fix for signature rendering at `chunk/signature.rs:761-830`. Frame 08 shows a single "memchr". No dedicated unit test for the collapse — code-review-level confirmation. |
| L12 | Panel titles illegible in dark theme | gui | cosmetic | RESOLVED | `NudoxThemeExt::panel_title_style()` exists as a themed definition and is referenced from GUI code. Grep-level only, not re-run. |
| L13 | Screenshot evidence was self-contradicting (harness) | gui | minor | RESOLVED | SHOT-REVIEW.md R1-R3 define the acceptance criteria (opaque frame, minimum distinct colours, frame-differs-when-expected). Not re-run; corroborated only indirectly by the L16/L22 screenshot-backed tests passing. Weak evidence — see §6. |
| F5 | Result count contradicted what was drawn | gui | minor | RESOLVED | Frame `04-search-hits.png` header reads "21 · scroll for more · 0 ms"; `omni_search.rs:1815` emits the literal string. |
| F7 | Implementations count (6) is correct | gui | cosmetic | RESOLVED | Frame 08 lists exactly Clone, Debug, DoubleEndedIterator, FusedIterator, Iterator and the inherent impl — matches `.real-crates/memchr-2.8.3/src/memchr.rs:287-351` per DOCSRS-COMPARISON §3. No regression in the current capture. |
| storage-numbers | Catalog/pack/symbol-projection byte counts are independently reproducible | index-search | cosmetic | RESOLVED | Re-ran `storage_catalog_scaling`, `storage_pack_vs_raw`, `storage_symbol_projection` → byte-identical to INDEX-CAPABILITY §2.1-2.3 (migration 208,896 B; 154 pkgs 28,672 B / 305.0 B per pkg; memchr pack ratio 0.188; 227.6 B/row). Row and byte counts are trustworthy; wall-clock numbers are not. |

---

## Status counts

| Status | Count |
|---|---|
| OPEN_UNOWNED | 5 |
| OPEN | 29 |
| UNVERIFIED_CLAIM | 5 |
| PARTIALLY_RESOLVED | 12 |
| RESOLVED | 23 |
| SUPERSEDED | 0 |
| **Total** | **74** |

Severity across the 51 unresolved rows (OPEN_UNOWNED + OPEN + UNVERIFIED_CLAIM +
PARTIALLY_RESOLVED): 8 blocker, 27 major, 16 minor.

---

## What's actually left

Ordered by how much each unblocks, not by severity. Rationale stated for each.

**1. Make one entry count authoritative (L50-cs, L28, then L19 + L23).** This is first
because it is the only item that is *blocking other people's proofs*. Two blocker-severity
IR defects have working synthetic proofs and no real-crate proof solely because
`real_memchr_generic_return.rs:80` still asserts a hardcoded 11,329 against a live 1,835.
Delete the constant or derive it, re-baseline the corpus under `DependencyResolution::Full`,
and record one number per package-version in one place. Until this lands, every "scale",
"corpus total" and "cost per entry" figure in the repo is unciteable, and L1/L10/L38 cannot
be closed out.

**2. Stop counting hollow successes as successes (L45-cs, RS-1, L43, L34).** 26 of 154
corpus entries return `Ok` with a stub table; 22 Python packages each lower to exactly 1
entry; the Rust producer's only real-package test executes zero assertions. This is
precisely the defect class doctrine §4 was written about, and it is currently inflating
every aggregate in every report. The type-level fix pattern already exists in-tree —
`require_resolved_dependencies()` from L28 — so apply the same shape: make "produced
nothing" a typed error at the producer boundary rather than an `Ok` with an empty table.

**3. Make the gate honest and cheap (L25, NX-1, NX-2, DOCTRINE-7).** Four language suites
report RED for pure harness reasons and two crates' real-package tests never start at all.
This is a day of `.config/nextest.toml` work that immediately converts three RED verdicts
into trustworthy signal, and it removes the exact blind spot doctrine §8 warns about
("a crate outside the gate stops being measured"). Fix doctrine §7's false claim in the
same change, or the next agent will trust it again.

**4. Search correctness (L44-ic, L45-ic, F1, L41).** User-visible, and one of them is
non-deterministic *across process launches* — the same query returns a different order
every run because `HashMap` iteration reaches the UI. That is a correctness bug, not a
ranking preference, and it makes every search screenshot unreproducible. Ranking with no
visibility or kind term (public API losing to private modules) and the unreachable
re-export aliases are the same fix area. Semantic search should be either implemented or
removed from the UI — the honest empty state is a good stopgap, not an endpoint.

**5. Decide whether the index/registry plane is in scope (L48-ic, L43-ic, L47-ic, L7).**
Every default configuration in that subsystem is a fake: in-memory engine, fixture-only
transport, memory-only watermarks, 16 tests that have never compiled, and the real
storage engine cannot build because a vendored C file is absent. Either restore
`doltlite.c` and wire the real engine, or state plainly that this plane is
non-functional so nobody quotes its numbers as product behaviour.

**6. Producer span emission (L31, L42, L40, L48-cs, L49-cs).** Source jumping — asked for
explicitly in the brief — cannot be built in the GUI: 5 of 7 producers emit `span: 0..0`,
`ImplRow` has no source field, and `ra/source.rs:57` emits `<file-id-N>` instead of a path.
Fix the wire types first (real path + line range, source on `ImplRow`), then the producers;
the const-expression and `ParamAttribute` gaps are the same "the IR cannot say it" class
and are worth batching into one IR change.

**7. Java classpath (L45).** 17 of 22 Maven packages fail for one missing `-classpath`
argument in `java/src/producer.rs:99-119`. Highest ratio of packages-fixed to lines-changed
on this list; it is item 7 only because it does not unblock anyone else.

**8. Wire up what already exists (L35, L37).** The MCP server is written, tested
end-to-end (L36), and never started by the application; `set_mcp_endpoint` has zero call
sites. Four engine methods are documented stubs. Small, self-contained, no dependencies.

**9. Housekeeping (ID-COLLISION, L21, L26, L49, L46-prop, L48, TS-2, DOCSRS-5-streaming).**
Do `ID-COLLISION` before writing anything into LIMITATIONS.md: four findings currently have
no id at all, and pasting either proposal verbatim overwrites L43/L44/L45's real content.

---

## Claims that are not backed by evidence

These are the rows the repo's own discipline says must not be credited. Each was checked
and each failed.

| Claim | Where it lives | What the check found |
|---|---|---|
| "As of 2026-08-05 the ENTIRE workspace compiles — the `--exclude` list is now a speed optimisation, not a necessity." | `AGENTS-DOCTRINE.md` §7:187-190 | False today. Bare `cargo nextest list` fails: 143 errors in `ir-vcs`'s lib-test target plus a duplicate-symbol link error in `driver`. The doctrine's own re-check loop uses `-p X --lib`, which structurally cannot see this. |
| "No relevance measurement anywhere in this repository." | `INDEX-CAPABILITY.md` §3.5 | False as a blanket claim — `index/search/ranking/eval.rs` is a real 316-line nDCG harness with 8 passing tests. The *narrowed* claim survives: it has zero golden fixtures, is called only from its own test module, and scores a ranker with no callers in engine/driver/gui. |
| "We have benchmarks for scalability." | The original framing this audit was commissioned against | There is no benchmark suite. Zero `[[bench]]` targets, zero criterion dev-deps outside an unbuildable vendored crate. Every number is a single-sample stopwatch inside a `#[test]`, taken on a host doctrine §8 documents as 2×–4.6× noisy. Row/byte counts are reproducible (see `storage-numbers`); wall-clock numbers are not. |
| "L10 — SETTLED, and lineage is now PROVEN end to end." | `LIMITATIONS.md` L10 banner | The same entry's trailing Status line still reads "PARTIALLY RESOLVED … content-level lineage not yet demonstrated". Its entry counts match no other measurement in the repo. Both `real_lineage.rs` tests remain `#[ignore]`d. |
| "registry's vector plane exists and is heavily tested (tests/vector/, 15 files)." | `LIMITATIONS.md` L41 | 16 files, and none is in any build target — `cargo metadata` shows `registry` has exactly one target (`lib`). They have never compiled, let alone run. The citation supports the opposite of its use. |

Three rows carry the status RESOLVED on evidence weaker than the rest of the table, and
are flagged here rather than downgraded, because the fix mechanism was located in code
each time: **L11** (not re-run; behavioural claim rests on log-0.4.33 → 419 entries, a
figure doctrine §8 cites independently), **L12** (grep-level only), and **L13** (no direct
check; corroborated only by the fact that later screenshot-backed tests pass on the same
harness). Treat all three as "believed fixed, unproven this session".

Two further honesty notes that are not claims but affect what the table means:
`crates/nudox-mcp/tests/real_crate_memchr.rs` (the evidence for L36) and the three
`oracle/` directories (the evidence for L34) are **untracked in git** — they exist in this
working tree and would be absent from a clean checkout.

---

# Verification pass — 2026-08-07

Every unresolved row above was re-checked against the code by an independent
pass (five read-only verifiers partitioned by area, plus targeted execution).
This section records only what **changed**; unlisted rows were confirmed as
written.

## Rows that did not survive

| Row | Was | Actually |
|---|---|---|
| **L25** | OPEN, blocker on the gate | **STALE.** `cargo check --workspace --all-targets` exits 0 in ~2 min. `ir-vcs`'s lib-test has 4 warnings and 0 errors — its 23 `f1::tests::*` link and enumerate. The `driver` failure is real but is *undefined* qdrant-edge SIMD symbols, not a zstd duplicate-symbol conflict. Fixed and committed. |
| **DOCTRINE-7** | UNVERIFIED_CLAIM, "falsified" | **The claim was essentially TRUE and this row was the error.** Only `driver` needed excluding, not three packages. The 3-package exclusion was hiding **1040 tests** (1227 → 2267 under `-P default`): 731 in `index`, 216 in `ir-vcs`. |
| **L48** | OPEN, "nothing tested on Linux" | **FALSIFIED as written.** `workspace/gui/Cargo.toml:29` carries `x11` and `wayland` features and the keymaps document Linux/Windows. Untested ≠ unarchitected. No CI config, so the weaker claim stands. |

## Rows that were real but mis-sized

- **L45 (Java)** — ranked "highest ratio of packages-fixed to lines-changed; one
  missing `-classpath`". The flag is genuinely absent, and adding it fixes
  **zero** packages: `.real-crates/` Maven entries are source-only `-sources.jar`
  extractions, `~/.m2/repository` does not exist, and `mvn` is not on PATH. This
  is corpus provisioning coupled to L8/L9, not a one-liner. Also, the row calls
  java's sweep a hollow floor-check; `corpus_sweep.rs:260-275` explicitly
  documents itself as "diagnostic, not a pass/fail gate" and explains why.
- **L50-cs** — "three in-tree sources disagree three ways". In *code* there was
  exactly one hardcoded memchr count (`EXPECTED_MEMCHR_2_8_3_ENTRIES = 11_329`).
  The other figures live in prose. One stale constant + doc drift.
- **L31** — undercounted roughly 2×. Real `span: 0..0` counts: go 2, java 5,
  csharp 8, clang 7, python 11, **typescript 14** (omitted entirely by the row).
  ~47 sites across six producers, not ~23 across five.
- **L41-cite** — the row said 16 files, `LIMITATIONS.md` said 15. Actual is **16
  test files + 2 support modules**. The row was right; the doc was wrong.
- **L3** — the row said 9 `cfg: None` sites and the in-repo doc said 5. Counted:
  **9**, at `ra/item.rs` 260, 435, 594, 1114, 1503, 2165, 2276, 2288, 2317. The
  row was right.

## Rows whose RESOLVED status is weaker than recorded

- **F7** — `refs.rs:1420-1425` asserts `6 rows × 20.0 = 120.0`. That is arithmetic
  on a layout constant, not a count of real impls; it would pass against an empty
  list.
- **L14** — `symbol_head_cfg.rs:30-48` carries a live disclaimer that feature-gated
  items never reach the IR because no cargo features end up in `CfgOptions`
  *despite* `CargoFeatures::All`. Either the disclaimer is stale or L14 is not
  resolved. Unsettled without execution.
- **C# suite** — recorded as the one unambiguous GREEN (58/58, 24 real packages).
  All four real-package tests are `#[ignore]`d requiring "a published oracle"
  that nothing in `.config/scripts` performs. The 24 checkouts are genuinely on
  disk, but in a default run this suite is green *on fixtures only* — a softer
  form of the critique the register levels at Rust.

## Found underneath the rows, not in them

- **The doltlite substitution.** L48-ic says "the real dolt-engine cannot build
  (L7)". It links fine — against the wrong database. `rusqdoltlite/sys.rs`
  declares the SQLite C API with no `#[link(name=…)]` (DoltLite *is* SQLite,
  renamed), and `index` separately pulls `rusqlite` with feature `bundled`,
  statically linking a complete stock SQLite that resolves those externs. Proven
  from a built artifact: `sqlite3_open_v2` present, `strings` shows **3.46.0**,
  `dolt_commit` absent. The "sovereign versioned catalog" has no prolly-tree
  pager and no content addressing. `rusqdoltlite/build.rs` claimed a link failure
  would occur, which is why nobody noticed.
- **A stub that compiles and panics.** `qdrant-edge/tokenizer/bccwj-suw_c1.0.model`
  was a **4-byte file containing the ASCII text `STUB`** where an 817,859-byte
  Vaporetto model belongs. `japanese.rs` `include_bytes!`s it and
  `Model::read_slice(MODEL).unwrap()`s it — clean compile, panic on first use.
- **A second vacuous guard, green in the suite.**
  `typescript/tests/real_npm_packages.rs:167` fails only on `entry_count == 0`,
  which is unreachable: `produce()` always synthesizes the root.
- **Untracked evidence, systemic.** The register flags two files. The real figure
  is ~27 untracked test `.rs` files plus all four `oracle/` dirs, `.config/nextest*.toml`
  and every gate script — including the evidence for `storage-numbers` (all three
  `storage_*` tests), L46 (`repair_paint.rs`), L16/L22 (`screenshots.rs`), L14
  (`feature_gated_api.rs`), L10 (`real_lineage.rs`), L20/L45-ic
  (`real_search_benchmarks.rs`) and L36 (`real_crate_memchr.rs`).
  `AGENTS-DOCTRINE.md` and `LIMITATIONS.md` are themselves untracked. A clean
  checkout of HEAD could not reproduce most of this document's green rows.
  Additionally `nextest.toml` names test binaries that do not exist at HEAD, and
  nextest validates matchers eagerly — so on a fresh clone the config hard-errors
  before running anything. The config and the tests must be committed together.

## ID-COLLISION — settled

Highest L-number actually present in `LIMITATIONS.md` is **L46**. `L43`–`L46`
collide with real entries. **New ids must start at L47.**

## Corrections to this section's own method

`storage-numbers` remains NEEDS-EXECUTION: the three `storage_*` tests compute
`disk_delta_bytes` dynamically via `measured()` and the quoted byte figures are
not hardcoded in the source, so reproducing them requires a run, not a read.

---

## Implementation pass — 2026-08-07

Status deltas from working the register. Every row below names the commit, so a
reader can check the claim rather than trust it. The register's own table above
is **not** rewritten in place: these supersede it where they disagree.

### Resolved

| row | commit | what actually closed it |
|---|---|---|
| L50-cs | `f42e762`, `74ebbe7` | `corpus/entry-baseline.toml` is now the single authoritative home for entry counts, enforced by `nudox-store`'s `corpus_contract.rs`. memchr 2.7.6/2.8.0/2.8.3 = 1825/1835/1835. The Rust sweep independently re-measured all 23 crates.io entries and agreed with the baseline exactly — two harnesses converging, not one asserting. |
| L19 | `f42e762` | Passes. The stale `EXPECTED_MEMCHR_2_8_3_ENTRIES = 11_329` assert ran *before* the test's real claim, so the `Option<..>` check it existed for never executed. Deleting it turned the test green on the first run. |
| L44-ic | `622c13e` | Three determinism hazards that mask each other; only the combined revert reproduces. `corpus.rs` `HashMap`→`BTreeMap`, `index/name.rs` ordered insert (its doc *claimed* sorted-by-`IntroId` and the code never sorted), and `search.rs` gained a real total order ending in `StableRef`. |
| L7 | `9aa0ae6` | Vendored SIMD kernels and the 817,859-byte tokenizer model restored (what was on disk was a 4-byte file containing `STUB`). `require_vendored_source` makes a missing vendored source a hard build error instead of a warn-and-skip that surfaced 200 lines later as six undefined symbols. |
| L45-cs | `60c0c6f`, `74ebbe7` | `YieldContract` + `NoDeclarationsContributed`/`YieldContractOutgrown`, enforced from **both** pipelines (`produce()` and `produce_with_occurrences` — the brief claimed one choke point; there were two). 22 true positives, 0 false positives across 6 producers. |
| L35 | `5c0b168` | The application hosts it. See LIMITATIONS.md L35 for the `McpStatus` redesign and the §1 amendment this required. |

### Newly filed, because the work found them

| id | commit | one line |
|---|---|---|
| L47 | `74ebbe7` | TypeScript extracts ~nothing from CommonJS packages without bundled `.d.ts` — `lodash` yields 1 declaration for ~300 public functions. Now pinned as exact-equality stubs so the claim cannot outlive the obstruction. |
| L48 | `e7b9e9a` | Cargo autodiscovery cannot see `tests/<dir>/<name>.rs` and says nothing. 18 test files across `registry` and `index` had never compiled. 279 tests now run. |
| L49 | `e7b9e9a` | REPORTED, NOT REPRODUCED: a qdrant-edge `Drop`-path panic seen twice in ~11 contended runs; 8/8 clean on a quiet machine. Filed at the confidence the evidence supports. |
| L50 | `81571ab` | ROOT CAUSE: a failed `run_build_scripts` is swallowed by an unsubscribed `warn!`, so every `build.rs`-set `cfg` evaluates false and `#[cfg]`-gated public API is deleted from the lowering. `log 0.4.17` loses `set_logger`; `nom 5.1.3` loses 8 parsers. |

### Downgraded

`L48-ic` ("every measurable subsystem's default configuration is a fake") is no
longer true as written. Its `dolt-engine`-cannot-build clause is falsified by
`e952ee4`: the real version-controlled engine builds, links, and passes 19+2
tests. The row's other clauses (`SECTION_SEMANTIC` empty, `FixtureTransport` the
sole `impl`) stand. A row that is 80% true is more dangerous than one that is
false, because it survives spot-checks — re-scope it rather than closing it.

### The one trap worth reading twice

`corpus/entry-baseline.toml` records `log 0.4.17 = 1255` and `nom 5.1.3 = 3260`.
Both are **honest measurements of a broken lowering** (L50). When L50 is fixed
they must go UP and `corpus_entry_counts_match_the_recorded_baseline` will go
red. That is the test working. Both rows are annotated in the file itself; do
not "fix" the failure by editing the number down.
