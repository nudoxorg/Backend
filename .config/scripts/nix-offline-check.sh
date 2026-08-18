#!/usr/bin/env bash
set -euo pipefail

root="$(git rev-parse --show-toplevel)"
cd "$root"

if rg -n --fixed-strings "dev.nudox.org" flake.nix flake.lock; then
  echo "FAIL: private dev.nudox.org input remains in flake metadata" >&2
  exit 1
fi

echo "PASS: flake metadata contains no dev.nudox.org reference"
echo "CHECK: evaluating devshell offline with the lock file unchanged"
nix develop --offline --no-update-lock-file --command true
echo "PASS: nix develop evaluated offline without contacting private infrastructure"
