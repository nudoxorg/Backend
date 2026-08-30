#!/usr/bin/env bash

# One inventory owns every Cargo workspace containing shipping code or a shipping
# laboratory. Paths are relative to workspace2. Consumers choose the semantic
# evidence they collect; they must not maintain a second manifest list.
shipping_workspace_manifests=(
  "Cargo.toml"
  "adapters/durable-journal/Cargo.toml"
  "adapters/observability/Cargo.toml"
  "domains/ir/Cargo.toml"
  "layout-lab/Cargo.toml"
  "planes/compiler/Cargo.toml"
  "planes/index/Cargo.toml"
)
