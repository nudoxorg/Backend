#!/usr/bin/env bash
# ── Setup a writable Buck2 prelude cell at nix/build/prelude-local/ ───────────
#
# Called by the devshell startup hook when the prelude version changes.
# Copies (does not symlink) the buck2-prelude source so the cell directory
# remains writable — the hook writes a .nix-source stamp file inside it.

set -eu

PRJ_ROOT="${PRJ_ROOT:-$(cd "$(dirname "$0")/.." && pwd)}"
TARGET="$PRJ_ROOT/nix/build/prelude-local"

# The prelude source (symlinked into the store by the shellHook).
SOURCE="$PRJ_ROOT/prelude"

if [ ! -d "$SOURCE" ]; then
  echo "setup-prelude: ERROR — prelude source not found at $SOURCE" >&2
  echo "  (the devshell hook creates this symlink before calling us)" >&2
  exit 1
fi

# Remove any stale prelude-local (broken symlink, or copy from a different rev).
rm -rf "$TARGET"

# Copy the prelude source files.  This is mostly .bzl files and templates,
# so it is small enough for a plain recursive copy.
mkdir -p "$TARGET"
cp -rL "$SOURCE/"* "$TARGET/"
chmod -R u+w "$TARGET"

echo "setup-prelude: copied $SOURCE → $TARGET"
