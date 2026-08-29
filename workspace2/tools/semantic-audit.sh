#!/usr/bin/env bash
set -euo pipefail

project_dir="$(cd "$(dirname "$0")/.." && pwd)"
cd "$project_dir"

echo 'Public or cross-crate scalar fields requiring a semantic-unit decision:'
rg -n --glob '*.rs' \
  'pub(\(crate\))? [A-Za-z_][A-Za-z0-9_]*: (bool|u8|u16|u32|u64|u128|usize|i8|i16|i32|i64|i128|isize)([,>])' \
  crates || true

echo
echo 'Tuple newtypes requiring conversion, borrow, and layout evidence:'
rg -n --glob '*.rs' \
  'pub(\(crate\))? struct [A-Za-z_][A-Za-z0-9_]*(<[^;{]+>)?\((pub(\(crate\))? )?(bool|u8|u16|u32|u64|u128|usize|i8|i16|i32|i64|i128|isize)' \
  crates || true

echo
echo 'Public function signatures accepting or returning raw scalars:'
rg -n --glob '*.rs' \
  'pub(\(crate\))? (const )?(async )?fn [A-Za-z_][A-Za-z0-9_]*[^\n]*(bool|u8|u16|u32|u64|u128|usize|i8|i16|i32|i64|i128|isize)' \
  crates || true

echo
echo 'Primitive conversion or accessor ceremony to review:'
rg -n --glob '*.rs' \
  '( as (u8|u16|u32|u64|u128|usize|i8|i16|i32|i64|i128|isize)\b|fn (get|raw|value|offset|index|count|bytes|credits|quantum)\([^)]*\).*(bool|u8|u16|u32|u64|u128|usize|i8|i16|i32|i64|i128|isize))' \
  crates || true
