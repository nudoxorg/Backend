#!/bin/sh
# Verifies the seven-language native toolchain and drives the deterministic
# `backend-flow` compiler-corpus selection on a provisioned host.
#
# The runner is deliberately read-only with respect to the corpus: it only
# reads committed manifests and package caches named by `NUDOX_*_CORPUS_DIR`,
# and it writes its receipt outside the corpus (default stdout, or the
# `--receipt` path, conventionally `.local/fleet/`). It is offline-safe by
# default: cargo is invoked with `--offline` so a missing package cache fails
# loudly instead of silently reaching the network.
#
# Every lane is verified directly. A missing executable or authority is a
# typed terminal in the receipt (`status: "unavailable"`), never a silent
# skip; the runner then exits `3` so automation cannot mistake absence for
# success.
#
# Usage:
#   tests/fleet/run-fleet.sh [options]
#
# Options:
#   --list                 Print the seven lanes and their env/binaries.
#   --lane LANE[,LANE..]   Verify/run only these lanes (rust, typescript,
#                          python, go, java, csharp, clang). Repeatable.
#   --check-only           Verify toolchains/authorities; do not run cargo.
#   --online               Do not pass `--offline` to cargo.
#   --corpus-root DIR      Export `NUDOX_<LANE>_CORPUS_DIR=$DIR/<lane>` for each
#                          lane whose subdirectory exists.
#   --receipt FILE         Write the JSON receipt to FILE.
#   --stdout               Emit the JSON receipt on stdout.
#   --test NAME            Cargo test filter (default: all compiler_corpus
#                          tests). Example: `--test fleet_selection`.
#   -h, --help             Show this help.
#
# Exit codes:
#   0  every requested toolchain is present and the corpus run is GREEN.
#   3  a toolchain or authority is missing (typed terminal in receipt).
#   4  the corpus run failed while the toolchain was complete.
#   64 usage error.
#   69 the runner's JSON tool (`jq`) is unavailable.
#
# Provisioning notes (honest, not fabricated):
#   * rust/typescript/python/go/java/csharp/clang binaries all come from the
#     pinned nixpkgs devShell (`nix develop .#development`). No version is
#     asserted here beyond what the binary reports.
#   * `NUDOX_GO_ORACLE_BIN` is the nix-built oracle from
#     `frontends/go/src/legacy/oracle` (vendorHash pinned in `.config/nix/tools.nix`).
#   * `NUDOX_TYPESCRIPT_CHECKER_BIN` wraps the vendored `main.cjs` with the
#     pinned `typescript` npm package on `NODE_PATH`.
#   * `NUDOX_PYREFLY_BIN` is the pinned `pyrefly` package.
#   * `NUDOX_JDK` is the pinned Zulu 21 root; `NUDOX_CLANG_DRIVER` is the
#     pinned clang driver; `NUDOX_CSHARP_DOTNET` is the pinned dotnet host.
#   * The real 200-row selection additionally needs package caches named by
#     `NUDOX_RUST_CORPUS_DIR`, `NUDOX_TYPESCRIPT_CORPUS_DIR`,
#     `NUDOX_PYTHON_CORPUS_DIR`, `NUDOX_GO_CORPUS_DIR`,
#     `NUDOX_JAVA_CORPUS_DIR`, `NUDOX_CSHARP_CORPUS_DIR`, and
#     `NUDOX_CLANG_CORPUS_DIR`. Absent caches yield typed per-lane
#     `LocallyUnavailable` terminals; they are never substituted.
set -eu

workspace=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
cargo=${BACKEND_STABLE_CARGO:-cargo}

lanes='rust typescript python go java csharp clang'
selected=''

usage() {
  awk 'NR > 1 && /^#/ { sub(/^# ?/, ""); print; next } NR > 1 { exit }' "$0"
}

lane_var() {
  case "$1" in
    rust) printf '%s' COMPILER_STABLE_TOOLCHAIN ;;
    typescript) printf '%s' COMPILER_TYPESCRIPT_COMPILER ;;
    python) printf '%s' COMPILER_PYTHON_COMPILER ;;
    go) printf '%s' COMPILER_GO_COMPILER ;;
    java) printf '%s' COMPILER_JAVA_COMPILER ;;
    csharp) printf '%s' COMPILER_CSHARP_COMPILER ;;
    clang) printf '%s' COMPILER_CLANG_COMPILER ;;
    *) return 1 ;;
  esac
}

lane_tool() {
  case "$1" in
    rust) printf '%s' rustc ;;
    typescript) printf '%s' tsc ;;
    python) printf '%s' python3 ;;
    go) printf '%s' go ;;
    java) printf '%s' javac ;;
    csharp) printf '%s' dotnet ;;
    clang) printf '%s' clang ;;
    *) return 1 ;;
  esac
}

lane_version_arg() {
  case "$1" in
    go) printf '%s' version ;;
    *) printf '%s' --version ;;
  esac
}

lane_corpus_var() {
  case "$1" in
    rust) printf '%s' NUDOX_RUST_CORPUS_DIR ;;
    typescript) printf '%s' NUDOX_TYPESCRIPT_CORPUS_DIR ;;
    python) printf '%s' NUDOX_PYTHON_CORPUS_DIR ;;
    go) printf '%s' NUDOX_GO_CORPUS_DIR ;;
    java) printf '%s' NUDOX_JAVA_CORPUS_DIR ;;
    csharp) printf '%s' NUDOX_CSHARP_CORPUS_DIR ;;
    clang) printf '%s' NUDOX_CLANG_CORPUS_DIR ;;
    *) return 1 ;;
  esac
}

lane_debug_name() {
  case "$1" in
    rust) printf '%s' Rust ;;
    typescript) printf '%s' TypeScript ;;
    python) printf '%s' Python ;;
    go) printf '%s' Go ;;
    java) printf '%s' Java ;;
    csharp) printf '%s' CSharp ;;
    clang) printf '%s' Clang ;;
    *) return 1 ;;
  esac
}

select_lane() {
  case " $selected " in
    *" $1 "*) return 0 ;;
  esac
  if [ -z "$selected" ]; then
    selected=$1
  else
    selected="$selected $1"
  fi
}

check_only=0
offline=1
list_only=0
corpus_root=
receipt=
emit_stdout=0
test_filter=

while [ $# -gt 0 ]; do
  case "$1" in
    --list) list_only=1 ;;
    --lane)
      shift
      old_ifs=$IFS
      IFS=,
      for lane in $1; do select_lane "$lane"; done
      IFS=$old_ifs
      ;;
    --check-only) check_only=1 ;;
    --online) offline=0 ;;
    --corpus-root) shift; corpus_root=$1 ;;
    --receipt) shift; receipt=$1 ;;
    --stdout) emit_stdout=1 ;;
    --test) shift; test_filter=$1 ;;
    -h|--help) usage; exit 0 ;;
    *) printf '%s\n' "unknown argument: $1" >&2; usage >&2; exit 64 ;;
  esac
  shift
done

if [ -z "$selected" ]; then
  selected=$lanes
fi

if [ "$list_only" -eq 1 ]; then
  printf '%-12s %-28s %-10s %s\n' lane variable tool corpus-root-variable
  for lane in $lanes; do
    printf '%-12s %-28s %-10s %s\n' \
      "$lane" "$(lane_var "$lane")" "$(lane_tool "$lane")" "$(lane_corpus_var "$lane")"
  done
  exit 0
fi

for lane in $selected; do
  case " $lanes " in
    *" $lane "*) ;;
    *) printf '%s\n' "unknown lane: $lane" >&2; exit 64 ;;
  esac
done

if ! command -v jq >/dev/null 2>&1; then
  printf '%s\n' "fleet: jq is required to emit the receipt; enter the nix devShell" >&2
  exit 69
fi

if [ -n "$corpus_root" ]; then
  for lane in $lanes; do
    candidate=$corpus_root/$lane
    if [ -d "$candidate" ]; then
      var=$(lane_corpus_var "$lane")
      eval "export $var=\$candidate"
    fi
  done
fi

tmpdir=$(mktemp -d)
trap 'rm -rf "$tmpdir"' EXIT HUP INT TERM
: > "$tmpdir/toolchains.jsonl"
: > "$tmpdir/authorities.jsonl"
: > "$tmpdir/lanes.jsonl"
: > "$tmpdir/terminals.jsonl"

terminal() {
  jq -cn --arg kind "$1" --arg lane "$2" --arg variable "$3" --arg detail "$4" \
    '{kind:$kind, lane:$lane, variable:$variable, detail:$detail}' >> "$tmpdir/terminals.jsonl"
}

missing=0
for lane in $selected; do
  var=$(lane_var "$lane")
  tool=$(lane_tool "$lane")
  eval "configured=\${$var:-}"
  exe=
  probe=
  if [ "$lane" = rust ]; then
    if [ -n "$configured" ] && [ -x "$configured/bin/rustc" ]; then
      exe=$configured
      probe=$configured/bin/rustc
    elif [ -n "$configured" ] && [ -x "$configured" ]; then
      exe=$configured
      probe=$configured
    elif command -v rustc >/dev/null 2>&1; then
      probe=$(command -v rustc)
      exe=$probe
    fi
  else
    if [ -n "$configured" ] && [ -x "$configured" ]; then
      exe=$configured
      probe=$configured
    elif command -v "$tool" >/dev/null 2>&1; then
      probe=$(command -v "$tool")
      exe=$probe
    fi
  fi
  if [ -z "$probe" ]; then
    missing=1
    terminal missing-toolchain "$lane" "$var" "$tool is absent and $var is unset or not executable"
    jq -cn --arg lane "$lane" --arg tool "$tool" --arg variable "$var" \
      '{lane:$lane, tool:$tool, variable:$variable, executable:null, version:null, status:"unavailable", cause:"missing-toolchain"}' \
      >> "$tmpdir/toolchains.jsonl"
    continue
  fi
  if version=$("$probe" "$(lane_version_arg "$lane")" 2>&1 | head -n 1); then
    probe_status=0
  else
    probe_status=$?
    version=
  fi
  version_lower=$(printf '%s' "$version" | tr '[:upper:]' '[:lower:]')
  case "$version_lower" in
    *"command error"*|*"no such file"*|*"unable to locate"*|*"not found"*|*"couldn"*)
      probe_status=1
      ;;
  esac
  if [ "$probe_status" -ne 0 ] || [ -z "$version" ]; then
    missing=1
    terminal invalid-version "$lane" "$var" "$probe did not report a usable version"
    jq -cn --arg lane "$lane" --arg tool "$tool" --arg variable "$var" --arg exe "$probe" \
      '{lane:$lane, tool:$tool, variable:$variable, executable:$exe, version:null, status:"unavailable", cause:"invalid-version"}' \
      >> "$tmpdir/toolchains.jsonl"
    continue
  fi
  jq -cn --arg lane "$lane" --arg tool "$tool" --arg variable "$var" --arg exe "$probe" --arg version "$version" \
    '{lane:$lane, tool:$tool, variable:$variable, executable:$exe, version:$version, status:"available"}' \
    >> "$tmpdir/toolchains.jsonl"
done

# Semantic-authority seams the corpus producers consult. Each is verified for
# presence (and, for `NUDOX_JDK`, the `bin/javac` it must contain).
check_authority() {
  variable=$1
  expectation=$2
  eval "value=\${$variable:-}"
  if [ -z "$value" ]; then
    missing=1
    terminal missing-authority "" "$variable" "$variable is unset"
    jq -cn --arg variable "$variable" --arg expectation "$expectation" \
      '{variable:$variable, expectation:$expectation, path:null, status:"unavailable", cause:"unset"}' \
      >> "$tmpdir/authorities.jsonl"
    return
  fi
  ok=0
  case "$expectation" in
    directory) [ -d "$value" ] && ok=1 ;;
    jdk) [ -x "$value/bin/javac" ] && ok=1 ;;
    executable) [ -x "$value" ] && ok=1 ;;
  esac
  if [ "$ok" -eq 1 ]; then
    jq -cn --arg variable "$variable" --arg expectation "$expectation" --arg path "$value" \
      '{variable:$variable, expectation:$expectation, path:$path, status:"available"}' \
      >> "$tmpdir/authorities.jsonl"
  else
    missing=1
    terminal missing-authority "" "$variable" "$variable does not satisfy $expectation: $value"
    jq -cn --arg variable "$variable" --arg expectation "$expectation" --arg path "$value" \
      '{variable:$variable, expectation:$expectation, path:$path, status:"unavailable", cause:"invalid-path"}' \
      >> "$tmpdir/authorities.jsonl"
  fi
}

check_authority NUDOX_JDK jdk
check_authority NUDOX_TYPESCRIPT_CHECKER_BIN executable
check_authority NUDOX_CSHARP_DOTNET executable
check_authority NUDOX_PYREFLY_BIN executable
check_authority NUDOX_CLANG_DRIVER executable
check_authority NUDOX_GO_ORACLE_BIN executable
check_authority RUSTC executable
check_authority LIBCLANG_PATH directory

run_status="not-run"
cargo_exit=0
log=$tmpdir/cargo.log
: > "$log"

if [ "$check_only" -eq 0 ]; then
  if [ "$offline" -eq 1 ]; then
    set -- test -p backend-flow --test compiler_corpus --offline -- --nocapture --test-threads=1
  else
    set -- test -p backend-flow --test compiler_corpus -- --nocapture --test-threads=1
  fi
  if [ -n "$test_filter" ]; then set -- "$@" "$test_filter"; fi
  printf '%s\n' "fleet: running $cargo $*" >&2
  if "$cargo" "$@" >"$log" 2>&1; then
    cargo_exit=0
    run_status="GREEN"
  else
    cargo_exit=$?
    run_status="RED"
  fi
  printf '%s\n' "fleet: cargo exited $cargo_exit" >&2
  if [ "$cargo_exit" -ne 0 ]; then
    sed -n '1,40p' "$log" >&2 || true
  fi
fi

# Observations joined on the `language=<Name>` token emitted by the
# real-package inventory test. A lane absent from the log keeps a
# `not-observed` terminal so the receipt is still complete.
: > "$tmpdir/observed.tsv"
: > "$tmpdir/selection.tsv"
if [ -s "$log" ]; then
  awk '
    /^real-corpus language=/ {
      for (i = 1; i <= NF; i++) { split($i, kv, "="); obs[kv[1]] = kv[2] }
      printf "%s\t%s\t%s\t%s\t%s\t%s\t%s\n", obs["language"], obs["attempted"], obs["source_bound"], obs["output"], obs["verified"], obs["unavailable"], obs["terminals"]
    }
  ' "$log" > "$tmpdir/observed.tsv"
  awk '
    /^fleet-selection version=/ { for (i = 1; i <= NF; i++) { split($i, kv, "="); sel[kv[1]] = kv[2] } }
    /^real-corpus manifest=/ { for (i = 1; i <= NF; i++) { split($i, kv, "="); cap[kv[1]] = kv[2] } }
    END {
      printf "version=%s\tseed=%s\trows=%s\tbytes=%s\tfingerprint=%s\n", sel["version"], sel["seed"], sel["rows"], sel["bytes"], sel["fingerprint"]
      printf "manifest=%s\tunavailable=%s\tmismatches=%s\n", cap["manifest"], cap["unavailable_terminals"], cap["parity_mismatches"]
    }
  ' "$log" > "$tmpdir/selection.tsv"
fi

for lane in $selected; do
  debug=$(lane_debug_name "$lane")
  row=$(awk -F'\t' -v want="$debug" '$1 == want { print; exit }' "$tmpdir/observed.tsv")
  if [ -z "$row" ]; then
    terminal lane-not-observed "$lane" "$(lane_corpus_var "$lane")" \
      "no real-corpus observation for $debug (corpus root not provisioned or test not run)"
    jq -cn --arg lane "$lane" --arg corpus_variable "$(lane_corpus_var "$lane")" \
      '{lane:$lane, corpus_variable:$corpus_variable, attempted:null, source_bound:null, output:null, verified:null, unavailable:null, terminals:null, status:"not-observed"}' \
      >> "$tmpdir/lanes.jsonl"
  else
    printf '%s\n' "$row" | awk -F'\t' -v lane="$lane" -v cv="$(lane_corpus_var "$lane")" \
      '{ printf "{\"lane\":\"%s\",\"corpus_variable\":\"%s\",\"attempted\":%s,\"source_bound\":%s,\"output\":%s,\"verified\":%s,\"unavailable\":%s,\"terminals\":%s,\"status\":\"observed\"}\n", lane, cv, $2, $3, $4, $5, $6, $7 }' \
      >> "$tmpdir/lanes.jsonl"
  fi
done

sel_field() {
  awk -F'\t' -v want="$2" 'NR == 1 { for (i = 1; i <= NF; i++) { split($i, kv, "="); v[kv[1]] = kv[2] } print v[want] }' "$tmpdir/selection.tsv"
}
man_field() {
  awk -F'\t' -v want="$2" 'NR == 2 { for (i = 1; i <= NF; i++) { split($i, kv, "="); v[kv[1]] = kv[2] } print v[want] }' "$tmpdir/selection.tsv"
}

overall=GREEN
if [ "$missing" -eq 1 ]; then overall="TOOLCHAIN-INCOMPLETE"; fi
if [ "$run_status" = RED ]; then overall="RED"; fi

receipt_json=$tmpdir/receipt.json
jq -n \
  --arg schema "backend.fleet.receipt.v1" \
  --arg generated_at "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
  --arg host "$(uname -sm)" \
  --arg command "$cargo test -p backend-flow --test compiler_corpus -- --nocapture --test-threads=1" \
  --arg run_status "$run_status" \
  --arg overall "$overall" \
  --arg manifest_version "$(man_field manifest_version manifest)" \
  --arg selection_version "$(sel_field selection_version version)" \
  --arg selection_seed "$(sel_field selection_seed seed)" \
  --arg selection_rows "$(sel_field selection_rows rows)" \
  --arg selection_bytes "$(sel_field selection_bytes bytes)" \
  --arg selection_fingerprint "$(sel_field selection_fingerprint fingerprint)" \
  --arg manifest_unavailable "$(man_field manifest_unavailable unavailable)" \
  --arg manifest_mismatches "$(man_field manifest_mismatches mismatches)" \
  --slurpfile toolchains "$tmpdir/toolchains.jsonl" \
  --slurpfile authorities "$tmpdir/authorities.jsonl" \
  --slurpfile lanes "$tmpdir/lanes.jsonl" \
  --slurpfile terminals "$tmpdir/terminals.jsonl" \
  '{
     schema: $schema,
     generated_at: $generated_at,
     host: $host,
     command: $command,
     corpus_workspace: $command,
     status: $overall,
     corpus_run: $run_status,
     toolchains: $toolchains,
     authorities: $authorities,
     lanes: $lanes,
     selection: {
       manifest_version: (if $manifest_version == "" then null else ($manifest_version | tonumber? // $manifest_version) end),
       selection_version: (if $selection_version == "" then null else ($selection_version | tonumber? // $selection_version) end),
       seed: (if $selection_seed == "" then null else $selection_seed end),
       rows: (if $selection_rows == "" then null else ($selection_rows | tonumber? // $selection_rows) end),
       bytes: (if $selection_bytes == "" then null else ($selection_bytes | tonumber? // $selection_bytes) end),
       fingerprint: (if $selection_fingerprint == "" then null else $selection_fingerprint end),
       unavailable_terminals: (if $manifest_unavailable == "" then null else ($manifest_unavailable | tonumber? // $manifest_unavailable) end),
       parity_mismatches: (if $manifest_mismatches == "" then null else ($manifest_mismatches | tonumber? // $manifest_mismatches) end)
     },
     terminals: $terminals
   }' > "$receipt_json"

if [ -n "$receipt" ]; then
  mkdir -p "$(dirname -- "$receipt")"
  cp "$receipt_json" "$receipt"
  printf 'fleet: receipt written to %s\n' "$receipt" >&2
fi

if [ "$emit_stdout" -eq 1 ] || [ -z "$receipt" ]; then
  cat "$receipt_json"
fi

if [ "$missing" -eq 1 ]; then
  printf 'fleet: toolchain incomplete; see typed terminals in receipt\n' >&2
  exit 3
fi
if [ "$run_status" = RED ]; then
  printf 'fleet: corpus run failed (exit %s)\n' "$cargo_exit" >&2
  exit 4
fi
exit 0
