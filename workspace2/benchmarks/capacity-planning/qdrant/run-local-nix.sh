#!/usr/bin/env bash
set -euo pipefail

project_dir="$(cd "$(dirname "$0")/../../.." && pwd)"

if [[ "${NUDOX_QDRANT_NIX_ENV:-}" != 1 ]]; then
  exec nix develop --offline "path:$project_dir#quality" -c env NUDOX_QDRANT_NIX_ENV=1 "$0" "$@"
fi

if [[ $# -lt 1 || $# -gt 3 ]]; then
  echo "usage: $0 OUTPUT_DIRECTORY [baseline-10k|largest-100k|lever-10k|all] [restart-scenario]" >&2
  exit 64
fi

output_directory="$1"
scenario_set="${2:-baseline-10k}"
restart_scenario="${3:-}"
qdrant_binary="$(command -v qdrant)"

arguments=(
  python3 "$project_dir/benchmarks/capacity-planning/qdrant/launch.py"
  --project "$project_dir"
  --qdrant-binary "$qdrant_binary"
  --runner "$project_dir/benchmarks/capacity-planning/qdrant/runner.py"
  --output "$output_directory"
  --scenario-set "$scenario_set"
)

if [[ -n "$restart_scenario" ]]; then
  arguments+=(--restart-scenario "$restart_scenario")
fi

exec "${arguments[@]}"
