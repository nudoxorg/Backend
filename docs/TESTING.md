# Testing Baseline

Run commands from the repository root.

This file documents two, related systems:

1. **`cargo nextest`** (`.config/nextest.toml`) — the primary way to run the
   Rust test suite selectively, by cost tier. This is new; read
   "Nextest: groups and profiles" below first.
2. **The nushell-script tiers** (`nu .config/scripts/*.nu`) — the older,
   broader gate that also covers Buck, Nix, and live-service checks nextest
   cannot express. Still current where noted; several of its claims below
   were stale and are corrected in place rather than deleted.

After an implementation-agent wave, run `nu .config/scripts/recheck.nu` for the
cheap whitespace, metadata, GUI metadata, and Nushell syntax gate before
starting the longer suites.

## Nextest: groups and profiles

`.config/nextest.toml` defines six test groups and three profiles for the
**root Cargo workspace** (the 19 crates under `[workspace]` in the root
`Cargo.toml`; NOT `workspace/gui` — see "The GUI is a separate workspace"
below). Read the comments in that file for the full reasoning; this section
is the operational summary.

| Group | What | Cost / precondition |
| --- | --- | --- |
| `unit` | `#[cfg(test)]` tests inside a crate's own `src/` (`kind(lib)`) | Fast, hermetic, no I/O |
| `integration` | `tests/*.rs` fixture/cross-crate suites (everything not claimed by a more specific group below) | Fast (measured 2026-08-04: 1108 tests, zero over 10s — see "How `slow` was populated", **and the expiry note there**), hermetic |
| `real-crate` | Drives in-process rust-analyzer over a checkout in `result/` (docs/LIMITATIONS.md L1) | 20–60s, ~600 MB peak RSS, **serialised** (`max-threads = 1`); `#[ignore]`d at the source |
| `slow` | Anything over ~10s not covered above: `ir-vcs`'s release-mode `bench_replay_vs_snapshot`, and `sandbox`'s real-VM tests (`escape.rs`, `smolvm_backend`'s smoke test) | Capped at 2 concurrent; `#[ignore]`d at the source |
| `corpus-sweep` | The four `tests/corpus_sweep.rs` binaries (clang, go, java, python), one `#[test]` each, each looping every provisioned package of one ecosystem under `result/` through the full `produce()` pipeline | Minutes, **serialised** (`max-threads = 1`). **Not** `#[ignore]`d — kept out of `-P default` by an explicit filter term, because each panics rather than skips without `result/` (and go needs `go`, java needs `javac`) |
| `gui` | *Defined but not assignable from this file* — see "The GUI is a separate workspace" | N/A here |

| Profile | Selects | Use |
| --- | --- | --- |
| `default` | `unit` + `integration` only | The pre-commit gate |
| `ci` | everything (`all()`) | CI, with 2 retries and a JUnit report at `target/nextest/ci/junit.xml` |
| `perf` | `real-crate` + `corpus-sweep` + `slow` only, `test-threads = 1`, `retries = 0` | Measurement runs — a retried benchmark reports the wrong number under the right label |

### Exact commands

```bash
# One-time per shell: `driver` does not link (L7, see below), and
# nudox-producer-clang needs libclang at *runtime*, not just build time.
export LIBCLANG_PATH=$(dirname "$(find /nix/store -maxdepth 2 -name 'libclang.dylib' -path '*clang-*-lib*' 2>/dev/null | head -1)")
export DYLD_LIBRARY_PATH="$LIBCLANG_PATH"
EXCLUDE='--exclude driver'

# Pre-commit gate (unit + integration, ~2200 tests, no ignored tests run)
RUSTC_BOOTSTRAP=1 cargo nextest run -P default $EXCLUDE

# Everything this workspace can see, including real-crate/slow (needs
# result/ checkouts and, for the sandbox VM tests, NUDOX_GUEST_ROOTFS +
# a smolvm/libkrun binary on PATH — both absent on a bare checkout, so those
# specific tests will fail rather than skip; see docs/LIMITATIONS.md and the "new
# limitation" note below)
RUSTC_BOOTSTRAP=1 cargo nextest run -P ci $EXCLUDE --run-ignored all

# Measurement run only (real-crate + slow), serialised, no retries
RUSTC_BOOTSTRAP=1 cargo nextest run -P perf $EXCLUDE --run-ignored all

# Run one named group directly, e.g. to iterate on unit tests only:
RUSTC_BOOTSTRAP=1 cargo nextest run -E 'group(unit)' $EXCLUDE

# List (does not execute anything) — the config-validation check:
RUSTC_BOOTSTRAP=1 cargo nextest list -P default $EXCLUDE
cargo nextest show-config test-groups $EXCLUDE --show-default
```

**Why `$EXCLUDE` is required, and why it is now one package, not three**
(docs/LIMITATIONS.md **L7**, not L6).

This file used to say `$EXCLUDE='--exclude index --exclude driver --exclude
ir-vcs'` and blame L6 ("`workspace/index` has never compiled"). L6 has been
recorded as RESOLVED in docs/LIMITATIONS.md since 2026-08-05 — `index` compiles
and its 731 tests run, `ir-vcs` compiles too — while this file,
`.config/nextest.toml`, and `.config/scripts/nextest-suite.nu` all went on
asserting the opposite. The cost of that drift, measured 2026-08-07:

| exclusion | `cargo nextest list -P default --workspace …` |
| --- | ---: |
| `--exclude index --exclude driver --exclude ir-vcs` (old) | 1227 tests |
| `--exclude driver` (current) | 2267 tests |

So the stale two extra exclusions were hiding **~1040 tests that build
today** — 731 in `index`, 216 in `ir-vcs` (the remainder is churn from
concurrent work landing during the measurement). Doctrine §7 already warned
about exactly this: an excluded crate stops being measured, and nothing
re-checks whether the exclusion is still true.

`driver` is the one package left, and the honest reason is that its build is
**non-deterministic on this checkout**, not that it fails one fixed way. The
cause class is docs/LIMITATIONS.md **L7** — vendored C sources under
`workspace/vendor/` that `driver` transitively needs (qdrant-edge's SIMD
kernels, doltlite's generated amalgamation).

Three runs of `cargo build -p driver --all-targets` within 25 minutes on
2026-08-07 gave three different answers, because a concurrent track was
restoring those sources while the check ran. All three recorded rather than
averaged into a claim:

| time | result |
| --- | --- |
| 16:14 | FAILS — `error[E0433]: cannot find type Path` at `workspace/vendor/qdrant-edge/build_segment.rs:22` (mid-edit tree) |
| 16:34 | **SUCCEEDS** — exit 0, real `target/debug/driver`. A bare `cargo nextest list -P default --workspace` with *no* `--exclude` also exits 0 and lists **2470** tests, 199 of them in `driver` |
| 16:37 | FAILS — `rusqdoltlite` build script; `workspace/vendor/doltlite/doltlite.c` had been restored, and cc-rs under this nix cc-wrapper rejects the triple: `error: unable to create target: 'Unable to find target for this triple (no targets are registered)'` |

The widely-quoted link diagnostic — undefined `_dotProduct_half_4x4`,
`_euclideanDist_half_4x4`, `_impl_score_dot_neon`, `_impl_score_l1_neon`,
`_impl_xor_popcnt_neon_uint128`, `_manhattanDist_half_4x4` from
`libqdrant_edge`, on bin `driver` and tests `live_download`, `live_socket`,
`pipeline_end_to_end` — is consistent with the same L7 cause and names exactly
the f16 kernels `build_segment.rs` documents. It is recorded here as
**reported, not reproduced**: by the time this was checked the failure had
moved. What is definitely *not* true, in any observed run: this is not L6, and
it has nothing to do with a "duplicate symbol between libzstd_seekable and
libzstd_sys" — no diagnostic mentions zstd at all.

A build-script, compile, or link error all kill the `cargo build` step before
any nextest.toml filterset runs, so the exclusion has to stay a CLI flag
rather than a config filter. Recheck with `cargo build -p driver
--all-targets`, **twice, on a settled tree** — and **not** `cargo check -p
driver --lib`, which cannot see this class of failure at all (`check` never
links; `--lib` never looks at the failing bin/test targets). That combination
is how the old "everything compiles" claim stayed green while being wrong.
Note that dropping the flag starts running `driver`'s 199 tests, which have
never executed here — a decision, not a no-op.

**Why `LIBCLANG_PATH`/`DYLD_LIBRARY_PATH` are required** (new finding, not
yet in docs/LIMITATIONS.md — flagged for the owner of that file): `nudox-store`'s
`ProducerRegistry::with_all_available` deliberately does **not** register a
clang producer, with a doc comment explaining that linking
`nudox-producer-clang` at all aborts at `dyld` load time with `Library not
loaded: @rpath/libclang.dylib` — "a link-time defect in the `clang`/
`clang-sys` binding on this host, not something this registry can route
around." That comment is correct and, on this host, incomplete in one way:
`nudox-producer-clang` is still a *workspace member* with its own unit
tests, so its own test binary hits the identical `dyld` abort the moment
`cargo nextest list`/`run` tries to enumerate it — independent of whether
anything ever calls into `ClangProducer`. A build-time `LIBCLANG_PATH` is
not sufficient on its own; `DYLD_LIBRARY_PATH` must also point at a
directory containing `libclang.dylib` at *run* time. Confirmed on this host
by pointing both at
`/nix/store/<hash>-clang-21.1.8-lib/lib` (path varies by Nix generation —
locate it with
`find /nix/store -maxdepth 2 -name libclang.dylib -path '*clang-*-lib*'`);
without it, `cargo nextest list` aborts with signal 6 while listing
`nudox-producer-clang` (and, transitively, anything that links it, such as
`nudox-engine`'s `impls_refs_flows` test binary).

**New limitation, not yet in docs/LIMITATIONS.md** (flagged for that file's
owner): `sandbox`'s real-VM tests (`tests/escape.rs`'s 4 cases, plus
`smolvm_backend::tests::smoke_real_launch_succeeds_or_typed_unavailable`)
need `NUDOX_GUEST_ROOTFS` and a `smolvm`/`libkrun` binary on `PATH`, neither
present in this environment. They are `#[ignore]`d, so a plain `-P ci`
without `--run-ignored` never touches them; `--run-ignored all` will attempt
and fail them rather than skip (unlike the `real-crate` group's `result/`
checks, which skip gracefully — see below). Their real wall-clock cost is
therefore unmeasured on this host; `.config/nextest.toml`'s `slow` group
assignment for them (`max-threads = 2`) is a conservative placement, not a
confirmed measurement.

### The GUI is a separate workspace

`workspace/gui` (package `lindsey`) is a deliberately separate Cargo
workspace (docs/AGENTS-DOCTRINE.md §1) — it has its own lockfile and is not a
member of the root workspace `.config/nextest.toml` governs.

An earlier draft of `.config/nextest.toml` tried to make one config file
cover both workspaces via
`--manifest-path workspace/gui/Cargo.toml --config-file .config/nextest.toml`.
That does not work: nextest validates every name-matcher in a config file's
`default-filter`/`overrides[].filter` against whichever workspace's metadata
is actually loaded, and hard-errors if a matcher matches nothing — in
*either* direction (a matcher naming a root-workspace crate fails when
loaded against the GUI's one-package workspace, and vice versa). A single
nextest config file cannot bind test-group assignments for two different
Cargo workspaces. `.config/nextest.toml`'s `gui` test-group definition
(`max-threads = 1`, and the reasoning behind that cap) is real and correct,
but nothing in that file can *assign* a test to it — see the "GUI group
assignment is NOT wired here" comment at the bottom of the file for the
full account. Until a config scoped to `workspace/gui/Cargo.toml` exists
(out of scope for this file, `.config/nextest.toml` at the repo root), run
the GUI suite directly:

```bash
cargo test --manifest-path workspace/gui/Cargo.toml --test shell_flow
cargo test --manifest-path workspace/gui/Cargo.toml --test adversarial
cargo test --manifest-path workspace/gui/Cargo.toml --test screenshots
```

`screenshots` and `shot_probe` are `harness = false` (docs/AGENTS-DOCTRINE.md,
"GPUI testing": the file *is* `fn main`, so it runs on the real main thread
where AppKit is legal, and opens a real Metal device). Nothing currently
enforces running them one at a time or apart from anything else that
touches the GPU on this host — do that by hand (don't background a second
GUI test run while `screenshots` is executing) until the GUI-scoped config
above exists.

### How `slow`'s "zero tests over 10s" number was produced

`RUSTC_BOOTSTRAP=1 cargo nextest run --workspace --exclude index --exclude
driver --exclude ir-vcs --no-fail-fast --message-format libtest-json-plus`
(with `NEXTEST_EXPERIMENTAL_LIBTEST_JSON=1`), 2026-08-04: 1108 passing, 8
`#[ignore]`d-and-skipped-by-the-runner, 8 failing (pre-existing/unrelated —
see "Known-red tests" below), and **zero** individual test executions over
10 seconds by `exec_time`. This is why `integration` and `unit` carry no
`max-threads` cap: there is nothing in the measured corpus for a cap to
protect against today.

**That number has an expiry date, and it expired.** It was taken over a
corpus that did not yet contain the four `tests/corpus_sweep.rs` files, all
of which are untracked additions made after 2026-08-04, none of which is
`#[ignore]`d, and one of which (clang's) runs for minutes. Measured
2026-08-07 on this host, one `#[test]` each, `cargo test -p <pkg> --test
corpus_sweep -- --test-threads=1`:

| sweep | wall clock |
| --- | ---: |
| `nudox-producer-python::corpus_sweep` | 0.11 s |
| `nudox-producer-java::corpus_sweep` | 21.6 s |
| `nudox-producer-go::corpus_sweep` | 22.1 s |
| `nudox-producer-clang::corpus_sweep` | **595 s** (20/20 green; worst entries abseil-cpp 137.9 s, range-v3 96.8 s, spdlog 74.1 s; peak RSS 646 MB) |

Two of those are already over the `integration` tier's 10 s premise and one
is over it by roughly sixty-fold, so they are no longer in `integration`;
they have their own `corpus-sweep` group with a budget derived from these
numbers (`slow-timeout = { period = "150s", terminate-after = 8 }` → SIGKILL
at 1200 s = 2.02× the measured 595 s; the 2× headroom is docs/AGENTS-DOCTRINE.md
§8's documented 21.9 s-idle/44.6 s-under-load contention swing). Before that
change they inherited `[profile.default]`'s 10 s/terminate-after-3, i.e. a
**SIGKILL at 30 s** — clang's sweep was being killed at 5% of its real
runtime, and go's and java's were sitting 8 s under the axe.

The 2026-08-04 measurement remains valid for everything that *is* still in
`unit`/`integration` — but re-take it after adding any real-corpus test, and
do not read it as a standing property of the tier.

A **fifth** `tests/corpus_sweep.rs` (`nudox-producer-rust`) landed on
2026-08-07 after these numbers were taken. It is matched by the same
`binary(=corpus_sweep)` filter with no config edit — deliberate; see
`.config/nextest.toml` — but it is `#[ignore]`d, drives in-process
rust-analyzer rather than an external toolchain, and self-declares "~20 min"
in its ignore reason, which is close to the 1200 s kill threshold above. Its
cost has **not** been measured here. Re-take it and adjust `terminate-after`
before trusting a `-P ci --run-ignored all` run that includes it.

### Newly-visible red tests, surfaced by narrowing `$EXCLUDE` (2026-08-07)

These are **not** new breakage. They are tests that existed and never ran,
because the package or the binary they live in was excluded from every gate.
They are listed rather than re-hidden.

Dropping `--exclude index --exclude ir-vcs`:
`RUSTC_BOOTSTRAP=1 cargo nextest run -P default -p index -p ir-vcs
--no-fail-fast` → **1023 tests run, 1020 passed, 3 timed out, 2 skipped**.
The three are all iroh/network paths that hang rather than fail, and are
SIGKILLed by `[profile.default]`'s 10 s/terminate-after-3:

```
TIMEOUT [30.010s] index::object_pack_transport fetch_from_non_enrolled_endpoint_is_rejected
TIMEOUT [30.008s] ir-vcs sync::tests::test_tampered_blob_rejected
TIMEOUT [30.009s] ir-vcs sync::tests_repo_glue::full_iroh_sync_loop
```

Two more passed but crossed 20 s and will be one contention swing away from
the same fate: `index::storage_pack_vs_raw
ndpk_pack_bytes_scale_with_a_larger_real_crate_and_stay_deterministic` and
`ir-vcs sync::tests::test_push_from_non_enrolled_endpoint_rejected`. The
right fix is a decision about that tier (are these `slow`? do they need a
network precondition and a typed skip?), not a bigger number on
`[profile.default]` — raising the gate's timeout to make a hang go green is
the weakening doctrine §6 names explicitly.

Qualifying `binary(=real_package)` to `nudox-store` (it was silently deleting
go's and clang's identically-named binaries from the gate) puts three
previously-unrun tests back in `-P default`. Two pass; one fails:

```
$ RUSTC_BOOTSTRAP=1 cargo test -p nudox-producer-clang --test real_package
test real_multi_file_c_package_lowers_end_to_end_with_compile_commands_json ... ok
test same_fixture_without_compile_commands_json_loses_both_functions ... FAILED

---- same_fixture_without_compile_commands_json_loses_both_functions stdout ----
panicked at workspace/compiler/languages/clang/tests/real_package.rs:149:9:
clang producer failed: `clang-libclang/1` contributed no declarations for
`fixture-mathutils-no-db`: the lowering holds only the root module `produce`
synthesized, which is exactly what a producer that never read a byte of the
package also yields. Fix the oracle so it declares what it finds — or, if this
producer genuinely cannot analyse sources in this build, override
`Producer::yield_contract` to return `YieldContract::RootOnly` naming the blocker
```

That is the producer-contract guard doing its job: the test's own premise is
that dropping `compile_commands.json` "loses both functions", and the guard
now refuses to let a root-only lowering pass as a result. It needs a decision
from the clang producer's owner (declare `YieldContract::RootOnly`, or assert
the degraded shape explicitly), not a filter change.

### Known-red tests (not caused by this track, not fixed by this track)

The run above surfaced 8 failing tests unrelated to `.config/nextest.toml`:
`nudox-mcp::schema_source$served_sdl_documents_the_key_format_agents_must_use`,
`nudox-mcp::tool_integration$graph_query_symbol_members_traversal_is_reachable`,
`nudox-mcp::tool_integration$graph_schema_returns_the_full_sdl`,
`nudox-producer::nudox_producer$tests::producer_error_display_includes_package`,
`nudox-producer::nudox_producer$tests::oracle_exit_display_includes_stderr`,
`nudox-producer-clang::nudox_producer_clang$tests::template_function_type_var`,
`registry::registry$vector::core::store::tests::payload_value_ordering_stability_btreemap_determinism`,
`registry::registry$vector::remote::voyage::tests::token_cap_exact_boundary_math_1188_fits`.
Several are in files under active concurrent edit this wave
(`crates/nudox-engine/src/chunk/head.rs`, `crates/nudox-engine/src/wire/mod.rs`,
`workspace/compiler/languages/rust/src/ra/item.rs` were all `git status`-dirty
during this check) and may already be fixed by the time this is read —
re-run the command above before assuming these are still failing.

## Nushell-script tiers

Still current, and cover ground nextest does not attempt (Buck, Nix flake
checks, live-backend TCP tests, mutation/fuzz tooling). They pre-date
nextest and overlap with it for the plain Cargo-test portion; `nextest run
-P ci` is now the more precise way to run that portion (it distinguishes
real-crate/GUI/slow costs the nu tiers do not), but the nu tiers are not
being removed — the Buck/Nix/live/nightly steps below have no nextest
equivalent.

| Tier | Command | Coverage |
| --- | --- | --- |
| Fast | `cargo test --workspace --lib` | Root workspace library unit tests. Equivalent to nextest's `-E 'group(unit)'`, without the group's selectability. Needs the same `--exclude driver` nextest does (L7, above) — but note `--lib` alone would not actually trip L7, since `driver`'s failures are all bin/test targets; pass it anyway so the command stays correct if `--lib` is ever dropped. |
| Default | `nu .config/scripts/full-check.nu` | Runs the default nextest profile for the root workspace, then the fixture-enabled `nudox-engine` profile so MCP fixture coverage (including token-budget measurements) is included. Corpus sweeps and ignored real-package tests remain opt-in because they require `result/` checkouts. The rest of the tier is unchanged: measurement-support tests, standalone GUI tests (when manifest present), and compiler Buck tests when `buck2` is available. `RUSTC_BOOTSTRAP=1` is set throughout because `nudox-ir` still uses unstable macro declarations; this is a diagnostic bridge until the crate is made stable-compatible. |
| Live | `SERVER_TEST_BACKENDS=1 nu .config/scripts/full-check.nu --live` | Default checks plus required TCP tests against configured catalog/vector/object-store services. Missing opt-in is a failure. Live pipeline uses bin/driver. |
| Infrastructure | `nu .config/scripts/full-check.nu --nix` | Default checks plus `nix flake check --no-update-lock-file`. A requested Nix failure is a failure, never a skip. |
| Nightly | `nu .config/scripts/full-check.nu --nightly` | Mutation (`cargo-mutants`) and fuzz inventory (`cargo fuzz list`) gates. Each missing requested tool fails instead of becoming a green no-op. |

## Exact Commands

- Proptest: `PROPTEST_CASES=256 cargo test --workspace`. This runs registered property tests. The existing `proptest!` block at `workspace/index/tests/object_pack/adversarial.rs` is not currently a Cargo target according to `cargo metadata`, so the baseline does not claim that file is covered. (Also subject to the L6 exclusion above.)
- Fuzz: `cargo fuzz list` should report `decode-entry` and `decode-body`. Run `cargo fuzz run decode-entry -- -max_total_time=60` or `cargo fuzz run decode-body -- -max_total_time=60`; generated corpora/artifacts remain ignored.
- Mutants: `nu .config/scripts/full-check.nu --nightly` runs `cargo mutants --package nudox-engine --no-shuffle` when `cargo-mutants` is installed. Mutation testing is intentionally not part of the default gate.
- Benchmark: `cargo test -p ir-vcs --release -- --ignored --nocapture bench_replay_vs_snapshot`. This was documented as "currently unreachable" because `ir-vcs` depended on the non-compiling `index` (L6); L6 is resolved and `ir-vcs`'s 216 tests now list and build, so the `slow` group's advance assignment for this test is live rather than aspirational (`cargo nextest run -P perf -E 'test(=bench_replay_vs_snapshot)' --run-ignored all`). There is no Cargo `benches/` harness in this repository.
- Real-crate corpus: populating `result/<name>-<version>/` is a manual copy, **not** a script — `scripts/fetch-real-crate.sh`, referenced pervasively in `crates/nudox-store/tests/real_crate.rs`'s and siblings' doc comments and `#[ignore]` messages, does not exist anywhere in this repository (`find . -iname 'fetch-real-crate*'` finds nothing). docs/LIMITATIONS.md L9 records how the existing checkouts were actually made: copied out of the local cargo registry cache. The manual equivalent, per docs/AGENTS-DOCTRINE.md §8 ("a fixture checkout inside the repo is captured by the repo's workspace"):
  ```bash
  cp -R ~/.local/share/cargo/registry/src/index.crates.io-*/axum-0.8.9 result/axum
  printf '\n[workspace]\n' >> result/axum/Cargo.toml
  ```
  Without the appended `[workspace]` table, `cargo metadata` fails with "current package believes it's in a workspace when it's not," which the Rust producer reports as a *load* failure (reads like a producer bug, is actually a missing checkout step). `result/` today has `log-0.4.33`, `memchr-2.7.6`/`2.8.0`/`2.8.3`, and 17 other crates, but **not** `axum` or `tokio` — so `nudox-store::real_crate`, `nudox-engine::real_producer_links`, and `nudox-mcp::real_crate_tokio` will SKIP (they check for the checkout and print `SKIP: ...` rather than fail) until those two are fetched by hand. `nudox-store::real_package`, `nudox-engine::symbol_head_cfg`, and `nudox-engine::real_lineage` already have what they need.
- GUI semantic tests: see "The GUI is a separate workspace" above.
- The GUI can be started with `cargo run --manifest-path workspace/gui/Cargo.toml`. There is no pixel-comparison CI; screenshot coverage is a manual `cargo test --test screenshots` run, not claimed by any automated gate.

The Buck command used by the default tier when available is
`buck2 test //workspace/compiler/...`. If `buck2` is absent, the script prints
an explicit skip instead of silently claiming compiler coverage.

## Measurement

`crates/nudox-test-support` provides zero-dependency wall, disk, and RSS
measurement helpers (`RunCost`, `measured()`, `disk_bytes()`). RSS is read via
`getrusage` (macOS/BSD) or `/proc/self/status` VmHWM (Linux). The crate has no
production dependencies and never fails a test on unsupported platforms.

The backend-image check measures and reports source-file download with ETag
validation via `tests/lib/http.nu:download-package-file`. The ETag header is
required and the response bytes must be non-empty.

## Nix Infrastructure

The live pipeline (`tests/backend-image/check.nu`) uses `bin/driver` from the
server derivation. The OCI image config (entrypoint, ports, env, labels) is
asserted by `tests/lib/oci.nu` against the nix2container JSON.
