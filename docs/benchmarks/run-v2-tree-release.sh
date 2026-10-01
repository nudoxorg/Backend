#!/usr/bin/env bash
set -euo pipefail

if [[ "${BENCH_BUILD_SLOT_GRANTED:-}" != "1" ]]; then
  printf '%s\n' 'Refusing to run Cargo: set BENCH_BUILD_SLOT_GRANTED=1 only after the root agent grants a slot.' >&2
  exit 2
fi
if [[ $# -ne 1 ]]; then
  printf 'usage: %s EXISTING_RESULT_DIRECTORY\n' "$0" >&2
  exit 2
fi

repo_root="$(git rev-parse --show-toplevel)"
result_dir="$1"
if [[ "$result_dir" != /* ]]; then
  result_dir="$repo_root/$result_dir"
fi
if [[ ! -d "$result_dir" ]]; then
  printf 'result directory must already exist: %s\n' "$result_dir" >&2
  exit 2
fi
result_dir="$(cd "$result_dir" && pwd -P)"
case "$result_dir/" in
  "$repo_root/"*)
    result_relative="${result_dir#"$repo_root/"}"
    if ! git -C "$repo_root" check-ignore --quiet -- "$result_relative"; then
      printf 'result directory inside the repository must be git-ignored: %s\n' "$result_dir" >&2
      exit 2
    fi
    ;;
esac

target_dir="${CARGO_TARGET_DIR:-}"
if [[ -z "$target_dir" ]]; then
  printf '%s\n' 'CARGO_TARGET_DIR must name an existing slot target; this script never chooses or creates a target.' >&2
  exit 2
fi
if [[ "$target_dir" != /* ]]; then
  target_dir="$repo_root/$target_dir"
fi
if [[ ! -d "$target_dir" ]]; then
  printf 'refusing to create a Cargo target directory; choose an existing slot target: %s\n' "$target_dir" >&2
  exit 2
fi
target_dir="$(cd "$target_dir" && pwd -P)"

files="${BENCH_TREE_FILES:-100000}"
samples="${BENCH_TREE_SAMPLES:-101}"
warmups="${BENCH_TREE_WARMUPS:-2}"
jobs="${BENCH_CARGO_BUILD_JOBS:-1}"
if [[ ! "$files" =~ ^[0-9]+$ ]] || (( files < 1 || files > 140000 )); then
  printf 'BENCH_TREE_FILES must be between 1 and 140000: %s\n' "$files" >&2
  exit 2
fi
if [[ ! "$samples" =~ ^[0-9]+$ ]] || (( samples < 100 || samples > 501 )); then
  printf 'BENCH_TREE_SAMPLES must be between 100 and 501 for p99 reporting: %s\n' "$samples" >&2
  exit 2
fi
if [[ ! "$warmups" =~ ^[0-9]+$ ]] || (( warmups > 20 )); then
  printf 'BENCH_TREE_WARMUPS must be between 0 and 20: %s\n' "$warmups" >&2
  exit 2
fi
if [[ ! "$jobs" =~ ^[1-4]$ ]]; then
  printf 'BENCH_CARGO_BUILD_JOBS must be between 1 and 4: %s\n' "$jobs" >&2
  exit 2
fi

hash_file() {
  if command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{print $1}'
  else
    sha256sum "$1" | awk '{print $1}'
  fi
}

snapshot_source() {
  local destination="$1"
  local path
  : > "$destination"
  while IFS= read -r path; do
    [[ -f "$repo_root/$path" ]] || continue
    printf '%s  %s\n' "$(hash_file "$repo_root/$path")" "$path" >> "$destination"
  done < <(git -C "$repo_root" ls-files --cached --others --exclude-standard | LC_ALL=C sort)
}

snapshot_source "$result_dir/source-files.before.sha256"
before_fingerprint="$(hash_file "$result_dir/source-files.before.sha256")"
{
  printf 'started_utc=%s\n' "$(date -u '+%Y-%m-%dT%H:%M:%SZ')"
  printf 'repo_head=%s\n' "$(git -C "$repo_root" rev-parse HEAD)"
  printf 'source_fingerprint=%s\n' "$before_fingerprint"
  printf 'cargo_target_dir=%s\n' "$target_dir"
  printf 'cargo_build_jobs=%s\n' "$jobs"
  printf 'file_count=%s\n' "$files"
  printf 'sample_count=%s\n' "$samples"
  printf 'warmups=%s\n' "$warmups"
  printf 'workspace_status_begin\n'
  git -C "$repo_root" status --short
  printf 'workspace_status_end\n'
  printf 'nix_version='; nix --version
  printf 'host='; uname -a
  if command -v sysctl >/dev/null 2>&1; then
    printf 'sysctl_hw_model='; sysctl -n hw.model 2>/dev/null || true
    printf 'sysctl_hw_ncpu='; sysctl -n hw.ncpu 2>/dev/null || true
    printf 'sysctl_hw_memsize='; sysctl -n hw.memsize 2>/dev/null || true
  fi
} > "$result_dir/run-manifest.txt"

nix shell '.#luna-tools' --command rustc --version --verbose > "$result_dir/rustc-version.txt"
nix shell '.#luna-tools' --command cargo --version > "$result_dir/cargo-version.txt"

command=(
  cargo bench --locked --offline --release -p backend-engine --bench compiler_input_tree_v2 --
  --files "$files" --samples "$samples" --warmups "$warmups" --readers 1,4,16
)
printf 'command=' >> "$result_dir/run-manifest.txt"
printf '%q ' "${command[@]}" >> "$result_dir/run-manifest.txt"
printf '\n' >> "$result_dir/run-manifest.txt"
nix shell '.#luna-tools' --command env \
  "CARGO_TARGET_DIR=$target_dir" \
  "CARGO_BUILD_JOBS=$jobs" \
  "${command[@]}" 2>&1 | tee "$result_dir/compiler-input-tree-v2.log"

snapshot_source "$result_dir/source-files.after.sha256"
after_fingerprint="$(hash_file "$result_dir/source-files.after.sha256")"
printf 'finished_utc=%s\n' "$(date -u '+%Y-%m-%dT%H:%M:%SZ')" | tee -a "$result_dir/run-manifest.txt"
printf 'source_fingerprint_after=%s\n' "$after_fingerprint" | tee -a "$result_dir/run-manifest.txt"
if [[ "$before_fingerprint" != "$after_fingerprint" ]]; then
  printf '%s\n' 'source tree changed during the run; mark all measurements invalid and repeat from a frozen source snapshot.' >&2
  exit 3
fi
