# Card clang-W7-adapter — make compiler recognition, buck2 root discovery, corpus fixture fixes

- Registered role: `nudox_luna_implementer` (luna, effort max)
- Baseline: branch `luna/clang-lifecycle` @ `954213c91`.
- Owned paths: `compiler/driver/build_drive.rs`, `compiler/driver/tests/build_drive.rs`,
  `compiler/driver/tests/corpus_harness.rs`,
  `.codex/evidence/capabilities/clang-c-lifecycle/corpus.md` (your rows only).
- FORBIDDEN: `compiler/languages/clang/**`, `compiler/driver/lower/**`, `database.rs`,
  `clang_lifecycle.rs`, `clang_lane.rs` (a parallel worker owns those).

## Findings (Terra-adjudicated from the corpus run)

1. **Unsupported compiler `gcc`** (lua, q3vm rows): `parse_make` recognizes only
   clang/clang++/cc/c++. Real build systems invoke `gcc`, `g++`, `c99`, `c11`, and
   cross-prefixed `*-gcc`. Extend recognition to the standard GNU/POSIX compiler names and
   cross-prefixed triples ending in `-gcc`/`-g++`, keeping ccache/sccache wrapper transport.
   Recognition of the COMPILER CELL is transport classification (which command to transport),
   not flag guessing — document the closed list in the module header.
2. **Buck2 root discovery** (the corpus buck2 row reached a build-path failure instead of the
   typed terminal): the adapter must locate the cell root by walking from the build-system
   marker directory to the nearest ancestor containing `.buckconfig` (the marker `BUCK` may be
   deep inside a cell), run `buck2` with that CWD, and scope the
   `kind("compilation_database", //...)` query to the marker's relative package path. When the
   query returns no targets (verified: the bundled prelude exports no such rule), return the
   exact `ToolPresentUndrivable { tool, evidence }` with the captured query output. Prove with
   the real prepared checkout at `.local/corpus/buck2-examples/examples/with_prelude`
   (buck2 binary at ~/.local/bin/buck2): the terminal must be `ToolPresentUndrivable`, NOT a
   build-path failure, and the evidence must contain the uquery invocation result. Also record
   in corpus.md that `buck2 build //cpp/hello_world:main` succeeds from that root (build-level
   evidence).
3. **Header-only rows produced empty selections** (miniaudio, vurtun/lib, STC): the authored
   compdbs named only headers. Clang parses headers fine as TUs, but the lane's declaration
   walk is tuned to source cursors; the honest fixture for a single-header library is a small
   DRIVER translation unit in the corpus scratch (NOT in the upstream tree) that includes the
   header, plus the header on the include path. Fix the harness's authored-compdb rows:
   author `#include "miniaudio.h"` style driver TUs under the corpus scratch dir, add `-I` to
   the header dir, and assert non-zero entities. Record the driver-TU shape in corpus.md.

## Constraints

Typed terminals only, no silent skips, no new dependency, no unsafe, no network. Keep the
PATH-augmentation mutex discipline for cmake/meson/buck2 child processes. `#![forbid(unsafe_code)]`
+ deny set.

## Evidence (`CARGO_TARGET_DIR=$PWD/.local/target`)

1. `cargo test -p compiler-driver --offline --test build_drive` green twice with the new
   gcc-recognition and buck2-root falsifiers.
2. Corpus re-run with `NUDOX_CORPUS_DIR=$PWD/.local/corpus`: lua, q3vm rows now drive through
   make; miniaudio, vurtun/lib, STC rows now produce non-zero entities; the buck2 row yields the
   exact typed terminal + build evidence. (clang-authority capacity failures belong to a
   parallel worker — record them as-is.)
3. `cargo fmt --check` on owned files. One commit:
   `fix(clang): recognize real compiler names, find the buck cell root, and fix corpus fixtures`.
   Report: commit sha, row-by-row outcome changes, command tails, smallest remaining red.
