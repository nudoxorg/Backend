#!/usr/bin/env bash
# Post-vendor fix for arborium language crates.
#
# Upstream build.rs uses env!("CARGO_MANIFEST_DIR") (compile-time). Under Buck
# that expands to the build-script-build __srcs tree, which only materializes
# the .rs listed in `srcs` — not grammar/**. blake3/ring correctly use
# std::env::var (runtime), which resolves to the full package materialization
# that buildscript_run prepares. Apply the same pattern here.
set -euo pipefail
root="$(cd "$(dirname "$0")/../.." && pwd)"
old='let manifest_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));'
new='let manifest_dir = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));'
shopt -s nullglob
for f in "$root"/vendor/arborium-*-*/build.rs; do
  if grep -qF "$old" "$f"; then
    # portable in-place replace
    python3 -c "
from pathlib import Path
p = Path(r'''$f''')
t = p.read_text()
p.write_text(t.replace('''$old''', '''$new''', 1))
print('patched', p)
"
  fi
done
