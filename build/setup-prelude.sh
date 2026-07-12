#!/usr/bin/env bash
# Materialize a writable Buck2 prelude with Nudox patches applied.
# Run from the repo root after entering the nix devshell (so `prelude` /
# `build/prelude` point at the store).
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
# Resolve the stock prelude. Prefer build/prelude, fall back to ./prelude.
# NB: `readlink -f` prints a resolved path (exit 0) even for non-existent
# targets, so test each candidate's existence explicitly rather than relying
# on `||` short-circuiting. Worktrees only have the root-level ./prelude link.
SRC=""
for _cand in "$ROOT/build/prelude" "$ROOT/prelude"; do
  if [[ -e "$_cand" ]]; then
    SRC="$(readlink -f "$_cand")"
    break
  fi
done
DEST="$ROOT/build/prelude-local"
if [[ -z "$SRC" || ! -d "$SRC" ]]; then
  echo "error: cannot resolve stock prelude at build/prelude or ./prelude" >&2
  exit 1
fi
rm -rf "$DEST"
cp -a "$SRC" "$DEST"
chmod -R u+w "$DEST"
python3 - "$DEST/rust/build.bzl" <<'PY'
import sys
from pathlib import Path
p = Path(sys.argv[1])
text = p.read_text()
old = """    if compile_ctx.dep_ctx.advanced_unstable_linking or crate_type == CrateType("rlib"):
        if dep_metadata_kind == MetadataKind("link"):
            dep_metadata_kind = MetadataKind("full")
"""
new = """    # Nudox: rlibs use full dep rlibs (not hollow -Zno-codegen metadata).
    # Hollow vs full SVHs can diverge on current nightlies → E0460/E0463 when
    # integration tests load intermediate first-party crates (registry→heart).
    if compile_ctx.dep_ctx.advanced_unstable_linking:
        if dep_metadata_kind == MetadataKind("link"):
            dep_metadata_kind = MetadataKind("full")
"""
if old not in text:
    # Already patched or upstream changed.
    if "Nudox: rlibs use full dep rlibs" in text:
        print("prelude already patched")
        raise SystemExit(0)
    raise SystemExit(f"patch site not found in {p}")
p.write_text(text.replace(old, new, 1))
print(f"patched {p}")
PY
# Patch Go toolchains to read binary path from [go] go_binary buckconfig key.
# This lets .buckconfig.local (written by the nix devshell hook) supply an
# absolute Nix store path so Buck2 actions find `go` without relying on PATH.
#
# NB: config must be read at .bzl *load* time (module scope), not inside the
# toolchain rule impl — read_config/read_root_config are unavailable during
# analysis ("This function is unavailable during analysis"). We hoist the read
# to a module-level constant and reference it from the impl.
python3 - \
  "$DEST/toolchains/go/system_go_bootstrap_toolchain.bzl" \
  "$DEST/toolchains/go/system_go_toolchain.bzl" <<'PY'
import re
import sys
from pathlib import Path

MARKER = "_NUDOX_GO_BINARY"
CONST = '\n# Nudox: hoisted to module scope — config reads are illegal during analysis.\n_NUDOX_GO_BINARY = read_root_config("go", "go_binary", "")\n'

# Any prior in-impl patch shape we may have written before.
IMPL_PATCHED = re.compile(
    r'    go = read_config\("go", "go_binary", (?:default_go|"go\.exe" if go_os == "windows" else "go")\)'
)
# Pristine upstream shape (single line).
PRISTINE = '    go = "go.exe" if go_os == "windows" else "go"'
# Replacement inside impl: prefer the buckconfig value, else the OS default.
REPL = '    go = _NUDOX_GO_BINARY or ("go.exe" if go_os == "windows" else "go")'

for path in sys.argv[1:]:
    p = Path(path)
    text = p.read_text()
    if MARKER in text:
        print(f"{p.name}: already patched")
        continue
    if IMPL_PATCHED.search(text):
        text = IMPL_PATCHED.sub(REPL, text, count=1)
    elif PRISTINE in text:
        text = text.replace(PRISTINE, REPL, 1)
    else:
        raise SystemExit(f"patch site not found in {p}")
    # Insert the module-level constant just before the first `def `.
    idx = text.index("\ndef ")
    text = text[:idx] + "\n" + CONST + text[idx:]
    p.write_text(text)
    print(f"patched {p.name}")
PY
echo "prelude ready at $DEST (buckconfig cell: build/prelude-local)"
