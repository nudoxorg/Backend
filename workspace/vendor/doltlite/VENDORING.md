# Vendored DoltLite engine

This directory contains the **DoltLite** engine — a SQLite fork whose B-tree pager
is replaced by a content-addressed prolly-tree store, giving a SQL database
Git-like version control (`dolt_commit`, `dolt_branch`, `dolt_checkout`,
`dolt_merge`, `dolt_gc`, the `dolt_log` / `dolt_conflicts` virtual tables, and
`dolt_at_<table>(ref)` point-in-time reads).

Upstream: <https://github.com/dolthub/doltlite>

## What is vendored (and why only this)

DoltLite's build system is the canonical SQLite autosetup/TCL toolchain: it weaves
the prolly engine, BLAKE3, ed25519, and mbedtls into a **single amalgamation**
just like stock SQLite's `sqlite3.c`. Rather than vendor the entire 127 MB source
tree (2000+ files: full test corpus, docs, packaging, WASM, JNI, …), we vendor the
generated amalgamation only:

| File | Purpose |
|---|---|
| `doltlite.c` | The full engine amalgamation (SQLite core + prolly engine + BLAKE3 + `dolt_*` functions). This is upstream's generated `sqlite3.c`, renamed. |
| `doltlite.h` | The public C API header (upstream's generated `sqlite3.h`, renamed). The API is the standard `sqlite3_*` surface. |
| `doltliteext.h` | The loadable-extension header (upstream's `sqlite3ext.h`); its `#include "sqlite3.h"` was rewritten to `#include "doltlite.h"`. |
| `LICENSE.md`, `APACHE_LICENSE` | Upstream licenses (retain verbatim). |
| `VERSION` | Upstream `VERSION` file. |
| `SQLITE_UPSTREAM_BASE` | The upstream SQLite commit DoltLite forked from. |
| `DOLTLITE_UPSTREAM_COMMIT` | The exact DoltLite git commit these files were generated from. |

## How the amalgamation was generated

From a clean `git clone --depth 1 https://github.com/dolthub/doltlite`:

```sh
mkdir build && cd build
../configure
make sqlite3.c sqlite3.h    # runs the TCL amalgamation generator (mksqlite3c.tcl)
```

The default configuration builds with `DOLTLITE_PROLLY=1`, so the amalgamation is
the *real* DoltLite engine (prolly tree + version control woven in), not stock
SQLite. The generated `sqlite3.c` / `sqlite3.h` / `sqlite3ext.h` were copied here
and renamed to the `doltlite.*` names DoltLite ships under.

## How it is compiled into Rust

`workspace/rusqdoltlite/build.rs` compiles `doltlite.c` with the `cc` crate,
statically, defining `DOLTLITE_PROLLY=1`, `SQLITE_THREADSAFE=1`, and the same
feature flags as the upstream release build (math functions, FTS5, R-Tree,
dbstat, column metadata). Only `-lpthread` is linked on Unix; `zlib` is *not*
required for the catalog subset (it is referenced only by the optional RBU
`compress=` path and resolves weakly).

## Regenerating / upgrading

To bump the vendored engine, repeat the generation steps above against a newer
DoltLite checkout, overwrite `doltlite.c` / `doltlite.h` / `doltliteext.h`
(re-applying the one-line `#include` rewrite in `doltliteext.h`), and update
`VERSION`, `SQLITE_UPSTREAM_BASE`, and `DOLTLITE_UPSTREAM_COMMIT`.
