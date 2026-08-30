#!/usr/bin/env bash

# One inventory owns every Cargo workspace containing shipping code or a shipping
# laboratory. Paths are relative to workspace2. Consumers choose the semantic
# evidence they collect; they must not maintain a second manifest list.
shipping_workspaces=(
  "Cargo.toml|crates"
  "adapters/durable-journal/Cargo.toml|adapters/durable-journal"
  "adapters/observability/Cargo.toml|adapters/observability"
  "domains/ir/Cargo.toml|domains/ir"
  "layout-lab/Cargo.toml|layout-lab"
  "planes/compiler/Cargo.toml|planes/compiler"
  "planes/index/Cargo.toml|planes/index"
  "planes/adaptive/Cargo.toml|planes/adaptive"
  "planes/application/Cargo.toml|planes/application"
)

shipping_workspace_manifests=()
shipping_source_roots=()
for shipping_workspace in "${shipping_workspaces[@]}"; do
  shipping_workspace_manifests+=("${shipping_workspace%%|*}")
  shipping_source_roots+=("${shipping_workspace#*|}")
done

# Unsafe is permitted only in these reviewed proof laboratories. The compiler
# separately requires a local safety explanation at every block.
reviewed_unsafe_modules=(
  "crates/nudox-runtime/src/initialized_prefix.rs"
  "crates/nudox-runtime/src/payload_slot.rs"
  "layout-lab/src/experiments.rs"
  "layout-lab/src/experiments/locality.rs"
)
