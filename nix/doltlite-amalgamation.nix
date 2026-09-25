# Pinned DoltLite amalgamation from workspace/vendor/doltlite/manifest.toml.
# The ~12 MB C sources are gitignored; Nix fetches them before cargo builds
# rusqdoltlite so the serving binary links the real versioned catalog engine.
{ pkgs }:

pkgs.runCommand "doltlite-amalgamation-0.11.41"
  {
    nativeBuildInputs = [ pkgs.unzip ];
  }
  ''
    set -eu
    unzip -q "${
      pkgs.fetchurl {
        url = "https://github.com/dolthub/doltlite/releases/download/v0.11.41/doltlite-amalgamation-0.11.41.zip";
        sha256 = "7e796b9557945c428ca884a56e849f5c11c43d4c56b0ff8b176f7df9c7d1e858";
      }
    }" -d "$TMPDIR"
    mkdir -p "$out"
    cp "$TMPDIR/doltlite-amalgamation-0.11.41/doltlite.c" \
      "$TMPDIR/doltlite-amalgamation-0.11.41/doltlite.h" \
      "$TMPDIR/doltlite-amalgamation-0.11.41/doltliteext.h" \
      "$out/"
  ''
