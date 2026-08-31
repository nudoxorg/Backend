#!/bin/sh
# Capture process CPU and high-water RSS around each selected real-API stage.
set -eu

if [ "$#" -lt 1 ]; then
    printf '%s\n' 'Usage: run-with-process-metrics.sh OUTPUT_DIR [runner arguments]' >&2
    exit 64
fi

capacity_root=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
workspace_root=$(CDPATH= cd -- "$capacity_root/../.." && pwd)
output_root=$1
shift
mkdir -p "$output_root"

if [ "${NUDOX_CAPACITY_BENCH_EXECUTABLE+x}" = x ]; then
    runner=$NUDOX_CAPACITY_BENCH_EXECUTABLE
else
    runner=$(
        cargo bench --manifest-path "$workspace_root/Cargo.toml" -p nudox-root --locked \
            --bench capacity-planning --no-run --message-format=json |
            jq -r 'select(.target.name == "capacity-planning" and .executable != null) | .executable'
    )
fi
if [ ! -x "$runner" ]; then
    printf '%s\n' 'could not resolve the capacity-planning benchmark executable' >&2
    exit 70
fi

for stage in \
    native-compile-lower-ir \
    durable-compiler-publication \
    deterministic-index-build \
    exact-core-query \
    tantivy-lexical-build \
    tantivy-lexical-query \
    vector-ingress \
    vector-exact-query
do
    stage_root="$output_root/$stage"
    mkdir -p "$stage_root"
    if [ "$(uname -s)" = Darwin ]; then
        /usr/bin/time -l "$runner" "$@" --only-stage "$stage" --output "$stage_root" \
            >"$stage_root/summary.txt" 2>"$stage_root/process-time.txt"
        awk '
            {
                for (field = 1; field < NF; field += 1) {
                    if ($(field + 1) == "user") user_seconds = $field
                    if ($(field + 1) == "sys") system_seconds = $field
                    if ($(field + 1) == "maximum" && $(field + 2) == "resident") rss_bytes = $field
                }
            }
            END {
                quote = sprintf("%c", 34)
                printf "{" quote "cpu_user_seconds" quote ":%.9f," quote "cpu_system_seconds" quote ":%.9f," quote "peak_rss_bytes" quote ":%.0f}\n", user_seconds, system_seconds, rss_bytes
            }
        ' "$stage_root/process-time.txt" >"$stage_root/process-metrics.json"
    else
        /usr/bin/time -v "$runner" "$@" --only-stage "$stage" --output "$stage_root" \
            >"$stage_root/summary.txt" 2>"$stage_root/process-time.txt"
        awk '
            /^[[:space:]]*User time \(seconds\):/ { user_seconds = $NF }
            /^[[:space:]]*System time \(seconds\):/ { system_seconds = $NF }
            /^[[:space:]]*Maximum resident set size \(kbytes\):/ { rss_bytes = $NF * 1024 }
            END {
                quote = sprintf("%c", 34)
                printf "{" quote "cpu_user_seconds" quote ":%.9f," quote "cpu_system_seconds" quote ":%.9f," quote "peak_rss_bytes" quote ":%.0f}\n", user_seconds, system_seconds, rss_bytes
            }
        ' "$stage_root/process-time.txt" >"$stage_root/process-metrics.json"
    fi
done
