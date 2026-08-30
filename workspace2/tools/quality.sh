#!/usr/bin/env bash
set -euo pipefail

project_dir="$(cd "$(dirname "$0")/.." && pwd)"
# shellcheck source=dylint/shipping-workspaces.sh
source "$project_dir/tools/dylint/shipping-workspaces.sh"

if [[ "${NUDOX_QUALITY_NIX_ENV:-}" != 1 ]]; then
  exec nix develop --impure --expr '
    let
      nixpkgs = builtins.getFlake "nixpkgs";
      packages = import nixpkgs { system = builtins.currentSystem; };
    in packages.mkShell {
      packages = [ packages.clang packages.libiconv packages.rustup packages.zlib ];
      LIBRARY_PATH = packages.lib.makeLibraryPath [ packages.libiconv packages.zlib ];
    }
  ' -c env \
    NUDOX_QUALITY_NIX_ENV=1 \
    NUDOX_DYLINT_NIX_ENV=1 \
    "$0" "$@"
fi

cd "$project_dir"

cargo fmt --all -- --check
"$project_dir/tools/dylint/run.sh"
cargo test --workspace --all-targets
cargo test -p nudox-runtime --features loom-model --lib
cargo clippy --workspace --all-targets -- -D warnings
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo doc --workspace --no-deps --all-features

# Repository-level custody check: unsafe is denied by default, and the only local
# exceptions must remain inside the two named, independently reviewed proof modules.
unsafe_sites="$(rg -n --glob '*.rs' '\bunsafe\s+(fn|impl|trait)|unsafe\s*\{' crates || true)"
unexpected_unsafe="$(printf '%s\n' "$unsafe_sites" | rg -v '^crates/nudox-runtime/src/(initialized_prefix|payload_slot)\.rs:' || true)"
if [[ -n "$unexpected_unsafe" ]]; then
  printf '%s\n' "$unexpected_unsafe" >&2
  echo 'handwritten unsafe exists outside the reviewed runtime storage proof boundaries' >&2
  exit 1
fi
for reviewed_unsafe in crates/nudox-runtime/src/initialized_prefix.rs crates/nudox-runtime/src/payload_slot.rs; do
  if rg -q '\bunsafe\s+(fn|impl|trait)|unsafe\s*\{' "$reviewed_unsafe" && ! rg -q 'SAFETY:' "$reviewed_unsafe"; then
    echo "$reviewed_unsafe requires a local written SAFETY proof at every unsafe obligation" >&2
    exit 1
  fi
done

# Repository-level dependency check: source syntax cannot prove the resolved
# shipping graph. Resolve normal edges independently for every inventoried Cargo
# workspace so nested workspaces cannot fall outside the root graph silently.
for relative_manifest in "${shipping_workspace_manifests[@]}"; do
  normal_tree="$(
    cargo tree \
      --manifest-path "$project_dir/$relative_manifest" \
      --workspace \
      --edges normal \
      --prefix none \
      --locked
  )"
  if printf '%s\n' "$normal_tree" | rg '^(serde|serde_json|tokio|async-trait|futures) v'; then
    echo "forbidden normal dependency resolved in $relative_manifest" >&2
    exit 1
  fi
done
