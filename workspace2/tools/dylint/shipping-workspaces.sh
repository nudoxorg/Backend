#!/usr/bin/env bash

# `Cargo.toml` is the only shipping inventory authority. Cargo membership under
# `crates/*` determines every package that the quality and semantic-lint gates
# inspect; this file carries only shared repository checks.
shipping_workspace_manifest="Cargo.toml"
shipping_source_root="crates"

# Unsafe is permitted only in these reviewed proof laboratories. The compiler
# separately requires a local safety explanation at every block.
reviewed_unsafe_modules=(
  "crates/nudox-index-graph-vector/src/lease/storage.rs"
  "crates/nudox-index-graph-vector/src/lease/wake.rs"
  "crates/nudox-runtime/src/initialized_prefix.rs"
  "crates/nudox-runtime/src/payload_slot.rs"
  "layout-lab/src/experiments.rs"
  "layout-lab/src/experiments/locality.rs"
)
