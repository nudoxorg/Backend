# Vendored Packages

This directory contains vendored dependencies that require special handling due to missing or large source files.

| Package | Sources Committed | Obtain With | What Breaks If Missing | Build Script Handling |
|---------|-------------------|-------------|----------------------|----------------------|
| `libpijul` | Yes | N/A (fork diff in `libpijul-fork.patch`) | Depth-1 structured record (DEPTH1 plan §2); all `libpijul` requirements resolve to this fork | Normal build |
| `gpui-component` | Yes | N/A (fork diff in `gpui-component-fork.patch`) | The GUI (`lindsey`) links gpui-ce, whose two API drifts from zed (`TextRun.letter_spacing`, `AnyView::into_any` → `into_any_element`) the upstream component library does not track | Normal build (path dep of `workspace/gui` only — not a root-workspace member) |
| `qdrant-edge` | **Yes** (restored 2026-08-07) | `curl -sSLO https://static.crates.io/crates/qdrant-edge/qdrant-edge-0.7.2.crate` — the published crate carries `cpp/`, `src/segment/spaces/metric_f16/cpp/` and `tokenizer/bccwj-suw_c1.0.model` verbatim | Was L7: the SIMD kernels are the *only* definitions of six symbols the Rust FFI declares, so their absence broke the final link of `driver` and 3 of its integration tests. Cargo patch resolves `qdrant-edge 0.7.2` to this vendored fork. | **Hard error** naming the missing file (see below) |
| `doltlite` | No (`.gitignore`d, 12 MB generated C) | `nu workspace/vendor/doltlite/fetch.nu` — pinned + hash-verified against `doltlite/manifest.toml` | `index`'s versioned catalog has no engine. Since 2026-08-07 that is a **typed refusal at open**, not a silent substitution — see "The doltlite trap" below. | (No build script; compiled by rusqdoltlite) |
| `rusqdoltlite` | Yes | N/A (Rust wrapper only; needs doltlite.c via sibling doltlite/) | DoltLite C bindings for catalog storage backend (behind `index::dolt-engine` feature) | Emits `cfg(doltlite_engine_linked)` only on a real compile; without it `Connection::open` returns `EngineError::EngineNotLinked` |
| `zstd-seekable` | Yes | N/A (fork is entirely build.rs; the seekable-format C sources and Rust FFI bindings are verbatim upstream 0.1.23) | `libpijul`'s optional change-file compression. Cargo patch resolves `zstd-seekable 0.1` (what `libpijul` depends on) to this vendored fork. | Normal build — see "The zstd-seekable duplicate-symbol clash" below |
| `pyroscope` | Yes | N/A (fork diff in `pyroscope-fork.patch`) | `nudox-serve` aborted (`fatal runtime error: current thread handle already set during thread spawn`) within minutes of running with telemetry enabled — see "The pyroscope thread-spawn abort" below. Cargo patch resolves `pyroscope 0.5` (what `heart`/`pyroscope_pprofrs` depend on) to this vendored fork. | Normal build |

## Workspace Exclusion

Both `qdrant-edge` and `rusqdoltlite` (and `doltlite`, which has no Cargo.toml) are **excluded** from the workspace `members` list in the root `Cargo.toml`.
- Exclusion prevents `cargo check --workspace --all-targets` from attempting to build them when their sources are missing.
- `qdrant-edge` remains active as a `[patch.crates-io]` entry: when `registry`'s `local` feature is enabled, the patch resolves and Cargo builds it; when disabled, it is skipped.
- `rusqdoltlite` is an optional `path` dependency of `index` (feature `dolt-engine`): only built if the feature is enabled.

## Self-Contained Manifests

Both excluded packages have explicit, self-contained `Cargo.toml` files with:
- Empty `[workspace]` table to stop Cargo from searching upward for workspace declarations
- Concrete `edition`, `version`, `license` instead of `.workspace = true` inheritance
- Explicit `[lints.rust]` and `[lints.clippy]` sections

This pattern decouples them from the root workspace so they can be built independently or excluded from workspace-wide operations.

## Build Script Behavior

When source files are missing:

- **qdrant-edge**: `build_common::require_vendored_source` **aborts the build**,
  naming the exact absent file, the symbols it is the only definition of, and the
  `curl` line that restores it. Both `build_quantization.rs` and
  `build_segment.rs` call it immediately before handing each file to `cc`, so a
  partially-vendored `cpp/` reports the one file it is short of.

  This used to be a `cargo:warning` and an early `return`, justified as "failing
  the build is too harsh, type-checking the crate is still useful". That
  reasoning does not survive contact with the facts:

  1. `qdrant-edge` is in the root manifest's `exclude` list, so
     `cargo check --workspace` never builds it. The only builds that reach this
     build script are ones that are about to *link* it (`registry/local`), and
     every one of those needs the kernels. Nothing benefits from the skip.
  2. `cargo check` never links, so nothing in the standard gate could observe
     the damage. The crate checked clean for days while `driver` could not
     produce a binary (doctrine §7 records the same trap).
  3. What the skip actually emits is an rlib with dangling references. The
     failure surfaces at the final link of every downstream binary, as six
     mangled symbol names that mention neither this crate nor `cpp/`.

  A build script that can prove its output will not link should say so, at the
  point it knows, in the vocabulary of the thing that is missing.

### The doltlite trap (fixed 2026-08-07 — kept because the shape recurs)

**What it was.** `doltlite.c` was absent, and unlike qdrant-edge its absence did
*not* produce a link error. `rusqdoltlite/sys.rs` declared the ordinary SQLite C
API (`sqlite3_open_v2`, `sqlite3_prepare_v2`, …) — because DoltLite is SQLite,
renamed — with no `#[link(name = …)]`. `index` separately hard-depends on
`rusqlite` with feature `bundled`, which statically links a complete stock SQLite
into the same binary.

So the externs resolved. The build was green. `index`'s `dolt-engine` feature ran
**plain SQLite**: no prolly-tree pager, no content addressing, no `dolt_*`
functions, no versioning. The only symptom was `dolt_commit` and friends failing
at runtime as "no such function", nowhere near the cause. Confirmed in situ by
`cargo test -p index --features dolt-engine`, which failed with
`Versioning { operation: "dolt_branch", detail: "no such function: dolt_branch" }`.

**Why it is now unconstructible, not merely detected.** Three layers, in
increasing order of how much they can be trusted:

1. `build.rs` emits `cfg(doltlite_engine_linked)` only after it has compiled the
   amalgamation *and* read the resulting archive back with `nm` to confirm the
   export set. Without the cfg, `sys.rs` declares no C externs at all and
   `Connection::open` returns `EngineError::EngineNotLinked`, naming the absent
   file. Nothing in the crate can produce a handle.
2. The API entry points are compiled under a `doltlite_` prefix
   (`-Dsqlite3_x=doltlite_x`) and every other global is localized by a partial
   link, so the archive exports exactly the 25 symbols `sys.rs` declares.
   `doltlite_open_v2` has one definition in the world; there is no stock SQLite
   for it to fall through to. This is also what keeps DoltLite and `rusqlite`'s
   bundled SQLite — which collide on **280 of 280** symbol names — in the same
   binary at all.
3. Every `Connection::open` runs `SELECT dolt_version()` and refuses with
   `EngineError::NotDoltLite` if it fails. A build flag is evidence about the
   build; this is evidence about the library that actually answered, so it
   survives link order, a swapped archive, or an interposed symbol.

**The general lesson.** A missing vendored source is only dangerous when
something *else* in the binary can satisfy the same names. The qdrant-edge case
was loud because its six NEON kernels are unique; this one was silent because
`sqlite3_open_v2` is not. When vendoring a fork of a widely-linked library, give
its symbols a namespace of their own — otherwise "is it linked?" and "is it the
right one?" are not the same question, and only the first one gets asked.

### The zstd-seekable duplicate-symbol clash

**What it was.** `index::pack` depends on `zstd` -> `zstd-safe` -> `zstd-sys`,
which compiles a complete libzstd from source. `libpijul`'s optional
`zstd-seekable` dependency (change-file compression) does too: its build.rs
tries `pkg-config` for a system libzstd and, on any failure — no pkg-config
installed is the default outside this repo's nix devshell — falls back to
compiling the entire bundled `zstd/lib/{common,compress,decompress}` tree
itself. Both compiled copies define the same `ZSTD_*` symbols. macOS `ld`
only *warns* on a duplicate symbol instead of failing the link, so
`cargo build -p index --features server` exited 0 while silently linking two
conflicting definitions of every compression entry point into one binary.
The only symptom was memory corruption at runtime: 34 SIGSEGVs under
`cargo nextest run -p index --features server`, every one of them in
`index::pack` — code that never touches `zstd-seekable` directly, which is
what made the actual cause easy to miss.

**Why vendoring, not an environment variable.** An earlier attempt set
`zstd = { version = "0.13", features = ["pkg-config"] }` on `index`'s own
`zstd` dependency plus a `PKG_CONFIG_PATH` in the flake devShell, so
`zstd-sys` would probe for and link the same system libzstd `zstd-seekable`
already preferred. That makes `zstd-sys`'s build hard-fail outside the
devshell (`pkg-config` isn't installed by default; `zstd-sys`'s build.rs
panics rather than falling back). A fix whose correctness depends on an
environment variable being set in one particular shell is not a fix a plain
`cargo build` can rely on.

**The fix.** `zstd-seekable`'s build.rs (see the vendored copy's own
comments) never compiles the bundled zstd library. It depends on `zstd-sys`
directly (a real Cargo dependency, not an env var), which makes cargo
resolve both `index::pack` and `zstd-seekable` to the *same* `zstd-sys` unit
— one compiled libzstd, linked once — and reads that unit's build script
output (`DEP_ZSTD_INCLUDE`, from `zstd-sys`'s `links = "zstd"`) for the
header path to compile only the small seekable-format wrapper
(`zstd/contrib/seekable_format/*`) against. No `pkg-config`, no
`PKG_CONFIG_PATH`, no devshell dependency, and no change to any on-disk
format: `index::pack`'s pack bytes and `libpijul`'s change-file bytes are
both produced by unmodified compression code, just linked against one
libzstd instead of two.

### The pyroscope thread-spawn abort

**What it was.** `nudox-serve`, run with telemetry enabled (the production
default — `heart::telemetry::init`, gated only by `OTEL_SDK_DISABLED`), died
within a couple of minutes with

```
fatal runtime error: current thread handle already set during thread spawn
```

— a `std`-level `rtabort!` (`library/std/src/thread/lifecycle.rs`'s
`ThreadInit::init`), not a panic: no backtrace, no unwind, no log line, just
`abort()`. Reproduced with telemetry on and never once across ~30 requests
with `OTEL_SDK_DISABLED=true` (which also gates the pyroscope agent —
`heart::telemetry::config`), which is why `.config/scripts/local-backends.nu`
used to set it unconditionally: not a preference, a load-bearing workaround
for a server that could not otherwise stay up.

**The mechanism.** `pyroscope_pprofrs` (via the vendored `pprof2` crate) arms
a process-wide `SIGPROF` interval timer at up to 100 Hz
(`heart::telemetry::pyroscope::PyroscopeHandle::start`, `sample_rate(100)`) —
one signal every 10 ms, for the life of the process. Separately, `pyroscope`
0.5.8's `Session::upload` (`src/session.rs`) built a **brand-new**
`reqwest::blocking::Client` on every periodic flush (`SessionManager`, every
~10s by default) — upstream's own code carried the comment `//todo do not
create a new client for every request`. `reqwest::blocking::Client::new()`
spawns a dedicated OS thread to drive its own private Tokio runtime, so every
flush is a fresh `pthread_create`, forever, regardless of request load: an
otherwise-idle `nudox-serve` reproduced the abort from wall-clock time alone.

Every one of those thread creations is a race against the SIGPROF timer: on
macOS (and other BSD-derived kernels), an interval-timer signal can be
delivered to a brand-new pthread while it is still inside libc's
`pthread_create` startup trampoline — *before* Rust's own per-thread init
(`ThreadInit::init`, which calls `set_current()` to register the "current
thread handle") has run. If that happens, whatever runs first wins the
`CURRENT` thread-local slot; when `ThreadInit::init` then tries to set it and
finds it already set, `std` aborts the whole process rather than risk two
different `Thread` identities on one OS thread. This is a genuine interaction
between a SIGPROF-based sampling profiler and `std::thread`'s current-thread
bookkeeping, not a bug in this repo's own code — confirmed in situ (not by
inference alone): both a request-burst run and a fully idle run of an
unpatched `nudox-serve` reproduced the identical abort message, one after
concurrent load and the other after roughly two and a half minutes of doing
nothing but sitting up with telemetry on, isolating the periodic session-flush
thread churn as sufficient on its own.

**Why vendoring, not an environment variable.** `pyroscope`'s own thread
creation, not this repo's, is what races the profiler's signal. There is no
public builder knob to reuse a client or to change the flush interval enough
to make the race practically unreachable, and disabling `OTEL_SDK_DISABLED`
(the previous workaround) throws out tracing/metrics/logs along with
profiling to dodge a defect in one dependency's HTTP send path.

**The fix.** `session.rs`'s `Session::upload` now builds its
`reqwest::blocking::Client` once, in a `static OnceLock`, and reuses it —
completing upstream's own unfinished `todo`. That removes the only
*recurring, guaranteed* source of new-thread creation in a long-running
server with telemetry on; the process no longer spawns any OS thread on a
fixed schedule once boot finishes. (Tokio's `spawn_blocking` pool can still
grow under load, in principle reopening a much lower-probability window — the
same residual risk any SIGPROF-sampled Rust server carries — but it stops
growing once warmed and produced no reproduction in either the request-burst
or extended-idle verification runs.) Verified against the real
`nudox-serve` binary: unpatched, it reliably aborted within 90s under load and
within ~150s fully idle; patched, it survived 60 real requests plus a 150s+
idle window with telemetry enabled and no OTLP collector reachable (the
fail-open path), across repeated runs, with zero aborts. See
`workspace/index/tests/telemetry_survives_pyroscope.rs` for the regression
test that drives the real binary as a subprocess (an `abort()` kills whatever
process calls the buggy code in-process, so this cannot be caught any other
way) and
`workspace/heart/telemetry/pyroscope.rs`/`workspace/index/server/main.rs` for
where telemetry is initialized.

### Divergence from upstream

Changes to vendored upstream files in this directory, none yet recorded as a
patch:

- `qdrant-edge/src/lib.rs` + `src/segment/common/anonymize.rs` — the dead generic
  `anonymize_collection_values{,_opt}` helpers are deleted; their
  `for<'a> &'a C: IntoIterator` HRTB overflows the trait solver whenever objc2's
  blanket `impl IntoIterator for &Retained<T>` is in the crate graph (it is, via
  iroh/netdev on macOS). Note that the root `Cargo.toml` still describes this
  divergence as `#![recursion_limit = "256"]` in `lib.rs`; that is not what the
  file contains.
- `qdrant-edge/build_quantization.rs`, `build_segment.rs`, `build_common.rs` —
  the missing-source guard described above.

`libpijul` establishes the convention these should follow
(`workspace/vendor/libpijul-fork.patch`): a vendored fork carries a regenerable
diff so the divergence is visible and can be replayed against a newer upstream.
`qdrant-edge` is now past "one change" and should get a `qdrant-edge-fork.patch`.

- `gpui-component` — vendored at upstream `c112e7b` (which pins zed gpui
  `1d217ee`) with `gpui-component-fork.patch` applied. The fork is the minimum
  needed to compile against gpui-ce `d435891`:
  - 15× `letter_spacing: None` added to full `TextRun { .. }` literals
    (`crates/ui/src/input/{element,indent}.rs`, `crates/ui/src/plot/label.rs`)
    — gpui-ce PR #111 added the field to `TextRun`.
  - 1× `FieldBuilder::View(view) => view.into_any()` →
    `into_any_element()` (`crates/ui/src/form/field.rs`) — the zed rename.
  The vendored workspace still names zed's git gpui; `workspace/gui`'s
  `[patch."https://github.com/zed-industries/zed.git"]` redirects it to gpui-ce.
  Regenerate with `git diff` in a checkout of `c112e7b` after applying the fork.

### Corrections to earlier notes in this file

This section previously claimed two things that were not true of the working
tree, both of which cost investigation time:

- "`build_segment.rs` was **not** modified." It was — it carried the same
  warn-and-skip guard as `build_quantization.rs`.
- "No stub tokenizer model was created. `qdrant-edge/tokenizer/` does not exist."
  It did exist, and it was a **4-byte file containing the ASCII text `STUB`**
  standing in for an 817,859-byte Vaporetto model that `japanese.rs` pulls in
  with `include_bytes!` and parses with `Model::read_slice(MODEL).unwrap()`.
  That compiles and panics on the first Japanese tokenization. It has been
  replaced with the genuine upstream artifact, verified against the crate's own
  `MODEL_CHECKSUM` (the sha512 in `japanese.rs`) — the file now matches it
  exactly. The model is `.gitignore`d, so it must be re-fetched per checkout.
