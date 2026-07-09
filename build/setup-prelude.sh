#!/usr/bin/env bash
# Materialize a writable Buck2 prelude with Nudox patches applied.
# Run from the repo root after entering the nix devshell (so `prelude` /
# `build/prelude` point at the store).
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SRC="$(readlink -f "$ROOT/build/prelude" 2>/dev/null || readlink -f "$ROOT/prelude")"
DEST="$ROOT/build/prelude-local"
if [[ ! -d "$SRC" ]]; then
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
echo "prelude ready at $DEST (buckconfig cell: build/prelude-local)"
