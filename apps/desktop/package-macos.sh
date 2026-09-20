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
binary="$root/target/$target_profile/backend-desktop"

cargo build --manifest-path "$root/Cargo.toml" --profile "$cargo_profile" -p backend-desktop
install -d "$destination/Contents/MacOS" "$destination/Contents/Resources"
install -m 755 "$binary" "$destination/Contents/MacOS/Nudox"
install -m 644 "$root/apps/desktop/macos/Info.plist" "$destination/Contents/Info.plist"

printf '%s\n' "$destination"
