#!/bin/sh
# Development-only convenience bundle. Do not use this as a release pipeline;
# verified distribution uses tools/package/macos-investor-bundle.py and its
# exact-source build and QA protocol in docs/operations/macos-investor-bundle-2026-10-04.md.
set -eu

profile="${1:-release}"
destination="${2:-target/Nudox.app}"
root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
if [ "$profile" = "debug" ]; then
  cargo_profile="dev"
  target_profile="debug"
else
  cargo_profile="$profile"
  target_profile="$profile"
fi
target_root="${CARGO_TARGET_DIR:-$root/target}"
case "$target_root" in
  /*) ;;
  *) target_root="$PWD/$target_root" ;;
esac
desktop_binary="$target_root/$target_profile/backend-desktop"
mcp_binary="$target_root/$target_profile/backend-mcp"
locald_binary="$target_root/$target_profile/backend-locald"

cargo build --locked --manifest-path "$root/Cargo.toml" --target-dir "$target_root" --profile "$cargo_profile" \
  -p backend-desktop -p backend-mcp -p backend-locald
install -d "$destination/Contents/MacOS" "$destination/Contents/Resources"
install -m 755 "$desktop_binary" "$destination/Contents/MacOS/Nudox"
install -m 755 "$mcp_binary" "$destination/Contents/MacOS/backend-mcp"
install -m 755 "$locald_binary" "$destination/Contents/MacOS/backend-locald"
install -m 644 "$root/apps/desktop/macos/Info.plist" "$destination/Contents/Info.plist"

printf '%s\n' "$destination"
