#!/bin/sh
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

cargo build --manifest-path "$root/Cargo.toml" --target-dir "$target_root" --profile "$cargo_profile" \
  -p backend-desktop -p backend-mcp -p backend-locald
install -d "$destination/Contents/MacOS" "$destination/Contents/Resources"
install -m 755 "$desktop_binary" "$destination/Contents/MacOS/Nudox"
install -m 755 "$mcp_binary" "$destination/Contents/MacOS/backend-mcp"
install -m 755 "$locald_binary" "$destination/Contents/MacOS/backend-locald"
install -m 644 "$root/apps/desktop/macos/Info.plist" "$destination/Contents/Info.plist"

printf '%s\n' "$destination"
