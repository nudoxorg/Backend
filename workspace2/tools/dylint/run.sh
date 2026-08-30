#!/usr/bin/env bash
set -euo pipefail

project_dir="$(cd "$(dirname "$0")/../.." && pwd)"

if [[ "${NUDOX_DYLINT_NIX_ENV:-}" != 1 ]]; then
  exec nix develop --impure --expr '
    let
      nixpkgs = builtins.getFlake "nixpkgs";
      packages = import nixpkgs { system = builtins.currentSystem; };
    in packages.mkShell {
      packages = [ packages.clang packages.libiconv packages.rustup packages.zlib ];
      LIBRARY_PATH = packages.lib.makeLibraryPath [ packages.libiconv packages.zlib ];
    }
  ' -c env NUDOX_DYLINT_NIX_ENV=1 "$0" "$@"
fi

toolchain="nightly-2026-05-28"
tool_root="$project_dir/target/dylint-tools"
tool_bin="$tool_root/bin"
lint_workspace="$project_dir/tools/dylint"
# shellcheck source=shipping-workspaces.sh
source "$lint_workspace/shipping-workspaces.sh"

if ! rustup toolchain list | grep -Fq "$toolchain-"; then
  rustup toolchain install "$toolchain" \
    --profile minimal \
    --component llvm-tools-preview,rustc-dev
fi
if ! rustup component list --toolchain "$toolchain" --installed | grep -Fq 'rustfmt-'; then
  rustup component add rustfmt --toolchain "$toolchain"
fi

installed_tools="$(cargo install --list --root "$tool_root" 2>/dev/null || true)"
if [[ ! -x "$tool_bin/cargo-dylint" ]] \
  || ! grep -Fq 'cargo-dylint v6.0.4:' <<<"$installed_tools"; then
  cargo install --locked --root "$tool_root" cargo-dylint@6.0.4
fi
if [[ ! -x "$tool_bin/dylint-link" ]] \
  || ! grep -Fq 'dylint-link v6.0.4:' <<<"$installed_tools"; then
  cargo install --locked --root "$tool_root" dylint-link@6.0.4
fi
export PATH="$tool_bin:$PATH"

(
  cd "$lint_workspace"
  cargo fmt --all -- --check
  cargo test --locked
)

lint_library="$(
  find "$lint_workspace/target/debug" \
    -maxdepth 1 \
    -type f \
    -name "*nudox_semantic_lints@$toolchain-*" \
    -print \
    -quit
)"
if [[ -z "$lint_library" ]]; then
  echo 'Dylint UI gate did not produce the pinned semantic lint library' >&2
  exit 1
fi

export DYLINT_RUSTFLAGS="-Dnudox_erased_map_err -Dnudox_stringly_state_field -Dnudox_redundant_public_accessor -Dnudox_dynamic_dispatch"

for relative_manifest in "${shipping_workspace_manifests[@]}"; do
  cargo dylint \
    --no-deps \
    --no-metadata \
    --lib-path "$lint_library" \
    --manifest-path "$project_dir/$relative_manifest" \
    --workspace \
    -- \
    --locked \
    --lib \
    --bins
done
