#!/bin/sh
set -eu
here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
export CARGO_BUILD_JOBS=2 CARGO_NET_OFFLINE=true
cargo test --manifest-path "$here/Cargo.toml" --locked --offline
