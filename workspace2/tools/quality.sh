#!/usr/bin/env bash
set -euo pipefail

project_dir="$(cd "$(dirname "$0")/.." && pwd)"
cd "$project_dir"

cargo fmt --all -- --check
nix shell nixpkgs#clang -c cargo test --workspace --all-targets
nix shell nixpkgs#clang -c cargo test -p nudox-runtime --features loom-model --lib
nix shell nixpkgs#clang -c cargo clippy --workspace --all-targets -- -D warnings
nix shell nixpkgs#clang -c cargo clippy --workspace --all-targets --all-features -- -D warnings
nix shell nixpkgs#clang -c cargo doc --workspace --no-deps --all-features

if rg -n --glob '*.rs' '\b(Box|Arc)<dyn\b|\bdyn\s+[A-Z_a-z]' crates; then
  echo 'public/hot-path dynamic dispatch is forbidden in Wave 1' >&2
  exit 1
fi

if rg -n --glob '*.rs' '\bserde(_json)?\b|#\[derive\([^]]*(Serialize|Deserialize)' crates; then
  echo 'Serde is forbidden in the Wave 1 data plane' >&2
  exit 1
fi

if rg -n --glob '*.rs' '\b(unbounded|SegQueue)\s*\(' crates; then
  echo 'unbounded queues are forbidden' >&2
  exit 1
fi

if rg -n --glob '*.rs' '\b(todo!|unimplemented!)\s*\(' crates; then
  echo 'incomplete implementation macro found' >&2
  exit 1
fi

unsafe_sites="$(rg -n --glob '*.rs' '\bunsafe\s+(fn|impl|trait)|unsafe\s*\{' crates || true)"
unexpected_unsafe="$(printf '%s\n' "$unsafe_sites" | rg -v '^crates/nudox-runtime/src/(initialized_prefix|payload_slot)\.rs:' || true)"
if [[ -n "$unexpected_unsafe" ]]; then
  printf '%s\n' "$unexpected_unsafe" >&2
  echo 'handwritten unsafe exists outside the reviewed runtime storage proof boundaries' >&2
  exit 1
fi
for reviewed_unsafe in crates/nudox-runtime/src/initialized_prefix.rs crates/nudox-runtime/src/payload_slot.rs; do
  if rg -q '\bunsafe\s+(fn|impl|trait)|unsafe\s*\{' "$reviewed_unsafe" && ! rg -q 'SAFETY:' "$reviewed_unsafe"; then
    echo "$reviewed_unsafe requires a local written SAFETY proof at every unsafe obligation" >&2
    exit 1
  fi
done

normal_tree="$(cargo tree --workspace --edges normal --prefix none)"
if printf '%s\n' "$normal_tree" | rg '^(serde|serde_json|tokio|async-trait|futures) v'; then
  echo 'forbidden runtime/data-plane dependency resolved' >&2
  exit 1
fi

if rg -n --glob '*.rs' '\.map_err\(\|_\|' crates; then
  echo 'erased error source in map_err' >&2
  exit 1
fi

if rg -n --glob '*.rs' '(\.ok\(\)\?|filter_map\s*\(\s*Result::ok)' crates; then
  echo 'validated packed data may not silently omit an internal decoding/index failure' >&2
  exit 1
fi

if rg -n --glob '*.rs' 'pub const fn (get|as_bytes|content|length|schema|kind|pinned_root|projection|dep_set)\s*\(' crates; then
  echo 'accessor ceremony found; use Deref or direct validated fields' >&2
  exit 1
fi

if rg -n --glob '*.rs' 'pub (const )?fn (from_bytes|from_slice|from_wire)\s*\(' crates; then
  echo 'bespoke representation constructor found; use standard From/TryFrom' >&2
  exit 1
fi

if rg -n --glob '*.rs' 'pub const fn wire\s*\(|\.wire\(\)' crates; then
  echo 'bespoke wire conversion found; use closed types with From/TryFrom' >&2
  exit 1
fi

if rg -n --glob '*.rs' 'pub struct [A-Za-z0-9_]+;' crates; then
  echo 'public unit struct found; use a free function, empty semantic enum, or real state' >&2
  exit 1
fi

if rg -n --glob '*.rs' 'impl fmt::Display for [A-Za-z0-9_]*Error' crates; then
  echo 'ordinary error boilerplate found; derive thiserror and retain structured sources' >&2
  exit 1
fi

if rg -n --glob '*.rs' 'impl([^\n]*)?(core::)?fmt::Debug for [A-Za-z0-9_]*Error' crates; then
  echo 'manual error Debug tree found; use descriptive sealed bounds plus derive' >&2
  exit 1
fi

if rg -n --glob '*.rs' "\b(step|expected|observed): &'static str" crates; then
  echo 'stringly scenario state found; use closed typed step/expected/observed vocabularies' >&2
  exit 1
fi

if rg -n --pcre2 --glob '*.rs' '^(?!\s*//).*\bcounts\.[01]\b|^(?!\s*//).*\.checked_mul\([0-9]+\)|^(?!\s*//).*\.max\([0-9]+\)|^(?!\s*//).*\b0\.\.[0-9]+(_[A-Za-z0-9]+)?\b' crates; then
  echo 'positional accounting or anonymous arithmetic/workload constant found' >&2
  exit 1
fi

if rg -n --glob '*.rs' 'const [A-Z0-9_]+: usize = [0-9]+[[:space:]]*[+*]' crates; then
  echo 'unexplained layout arithmetic found; derive it from named field types or format widths' >&2
  exit 1
fi

if rg -n --glob '*.rs' 'clippy::(too_many_lines|cognitive_complexity)' crates; then
  echo 'control-flow lint suppression found; split invariant transitions or make policy declarative' >&2
  exit 1
fi

if rg -n --glob '*.rs' 'unreachable!\s*\(' crates; then
  echo 'unreachable panic found; make the validated representation carry the proof' >&2
  exit 1
fi

if rg -n --glob '*.rs' '\.expect\s*\(|\.unwrap\s*\(' crates; then
  echo 'panic-based error handling found; propagate a typed error with complete reporting context' >&2
  exit 1
fi

while IFS= read -r crate_root; do
  root_lines="$(wc -l < "$crate_root")"
  if ((root_lines > 120)); then
    echo "$crate_root has $root_lines lines; crate roots must remain module maps" >&2
    exit 1
  fi
done < <(find crates -path '*/src/lib.rs' -type f -print)
