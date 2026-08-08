# Vendored DoltLite engine

This directory contains the **DoltLite** engine — a SQLite fork whose B-tree pager
is replaced by a content-addressed prolly-tree store, giving a SQL database
Git-like version control (`dolt_commit`, `dolt_branch`, `dolt_checkout`,
`dolt_merge`, `dolt_gc`, the `dolt_log` / `dolt_conflicts` virtual tables, and
`dolt_at_<table>(ref)` point-in-time reads).

Upstream: <https://github.com/dolthub/doltlite>. Pinned version, commit, URLs and
hashes: [`manifest.toml`](manifest.toml).

## Restoring it (the one command)

```sh
nu workspace/vendor/doltlite/fetch.nu          # fetch, verify, install
nu workspace/vendor/doltlite/fetch.nu check    # verify what is on disk, offline
```

`doltlite.c` is ~12 MB of generated C and is `.gitignore`d, so **every fresh
checkout starts with no engine** and must run this once. `fetch.nu` verifies the
archive hash and each extracted file against `manifest.toml` before anything
lands in this directory; a mismatch aborts and installs nothing.

## What is vendored (and why only this)

DoltLite's build system is the canonical SQLite autosetup/TCL toolchain: it weaves
the prolly engine, BLAKE3, ed25519, and mbedtls into a **single amalgamation**
just like stock SQLite's `sqlite3.c`. Upstream publishes that generated
amalgamation as a release asset, so there is no reason to clone the 380 MB source
tree or run TCL locally.

| File | Purpose |
|---|---|
| `doltlite.c` | The full engine amalgamation (SQLite core + prolly engine + BLAKE3 + `dolt_*` functions). Upstream's generated `sqlite3.c`, renamed. |
| `doltlite.h` | The public C API header (upstream's generated `sqlite3.h`, renamed). The API is the standard `sqlite3_*` surface. |
| `doltliteext.h` | The loadable-extension header (upstream's `sqlite3ext.h`). Unused by this binding; kept so the vendored set matches the published archive byte for byte. |
| `LICENSE.md` | Upstream license (retain verbatim). |
| `manifest.toml` | The pin: tag, commit, archive URL + sha256, per-file sha256. |
| `fetch.nu` | Fetch/verify/install, and `--print-hashes` for bumping the pin. |

## How it is compiled into Rust

`workspace/vendor/rusqdoltlite/build.rs` compiles `doltlite.c` with the `cc`
crate, statically, defining `DOLTLITE_PROLLY=1`, `SQLITE_THREADSAFE=1`, and the
same feature flags as the upstream release build. It then does two things that
are not optional and are easy to mistake for over-engineering:

1. **Renames the API entry points.** The 25 functions `rusqdoltlite/sys.rs`
   declares are compiled as `doltlite_open_v2`, `doltlite_prepare_v2`, … via
   `-Dsqlite3_x=doltlite_x`, and `sys.rs` binds them with matching
   `#[link_name]`s.
2. **Localizes everything else.** A partial link (`ld -r` with an export
   allow-list) demotes the other ~1129 globals to file-local symbols, and the
   build script then reads the archive back with `nm` and fails if the export set
   is not exactly those 25.

### Why (this is the bug that made it necessary)

DoltLite *is* SQLite, so a naive build exports 291 `sqlite3_*` symbols. `index`
also links `rusqlite` with feature `bundled`, which statically embeds a complete
stock SQLite exporting 280 symbols — **every one of which DoltLite also defines.**
Measured on this tree, the intersection is 280/280.

With identical names, two things can happen and both are silent:

- The linker satisfies *both* crates from whichever archive it scans first. One
  engine serves the whole binary, and which one is a link-order accident.
- If both archive members get pulled in, the link dies with ~280 duplicate
  symbols, in a diagnostic that names neither this directory nor `rusqlite`.

And when `doltlite.c` was simply **absent**, the old build script warned and
returned while `sys.rs` declared bare `sqlite3_*` externs with no `#[link]`.
Those resolved happily against `rusqlite`'s bundled SQLite. The build was green,
plain SQL worked, and the versioned catalog silently had no versioning at all —
the only symptom being `dolt_commit` reporting "no such function" at runtime, far
from the cause. That is why the current build script emits
`cfg(doltlite_engine_linked)` only on a real compile, why `Connection::open`
refuses without it, and why it re-probes `dolt_version()` at runtime even then.

## Verifying you really have DoltLite

A successful build proves nothing on its own — that is precisely the mistake that
created the original defect. The checks that do mean something:

```sh
# 1. The engine archive exports exactly the declared FFI surface, doltlite_*.
nm -g -U "$(find target -name libdoltlite.a | head -1)"

# 2. A binary that links it defines dolt_* internals (localized, so `nm -a`).
nm -a target/debug/deps/engine-* | grep -c doltlite

# 3. The engine answers a dolt_* function at runtime. This is the real gate;
#    `Connection::open` runs it on every open.
RUSTC_BOOTSTRAP=1 cargo test --manifest-path workspace/vendor/rusqdoltlite/Cargo.toml
```

## Upgrading

1. Pick a newer tag from <https://github.com/dolthub/doltlite/releases>.
2. Download that release's `doltlite-amalgamation-<version>.zip`.
3. `nu fetch.nu print-hashes <zip>` and paste the results into `manifest.toml`,
   along with the new `tag`, `commit`, `version`, and `sqlite_source_id`.
4. `nu fetch.nu` and rebuild.

If the upgrade adds or removes a public API entry point this binding calls, the
build fails in `verify_exported_symbols` naming the missing symbol — `sys.rs` and
`build.rs`'s `API_FUNCTIONS` cannot drift apart quietly.

## Corrections to earlier versions of this file

- It listed `APACHE_LICENSE`, `VERSION`, `SQLITE_UPSTREAM_BASE` and
  `DOLTLITE_UPSTREAM_COMMIT` as vendored files. None of those exist in upstream's
  published amalgamation archive, and only `VERSION` exists in the upstream repo
  at all. That information now lives in `manifest.toml`, where it is pinned
  rather than merely transcribed.
- It described generating the amalgamation with `../configure && make sqlite3.c`
  from a clone. That works but is unnecessary: upstream attaches the generated
  amalgamation to every release, and a documented `curl` of a hashed asset is
  reproducible in a way that "run the TCL generator" is not.
- It said the build "links only `-lpthread` on Unix". Still true, but it omitted
  the rename/localize steps, which are the load-bearing part.
