#!/usr/bin/env bash
# Run the impact conversation for a Next.js-style project through the real
# owner, CLI and MCP server with a private HOME, and print every answer.
#
#   impact-alias-proof.sh BIN_DIR OUT_DIR
#
# BIN_DIR holds backend-locald, backend-cli and backend-mcp. Debug builds skip
# the account gate, so a transcript from them is development-mode evidence.
#
# The script exits non-zero when the proof fails. The fixture has ten
# TypeScript sources; `Button` is consumed by exactly seven of them (the home,
# about, dashboard and broken pages, NavBar, Card and Invoice, directly, through
# an alias, through the barrel or through another component), and one import,
# `@/widgets/Widget` in the broken page, resolves to no file, which the answer
# must state as a `code.unresolved-imports` gap rather than hide.
set -euo pipefail

bin=$(cd "${1:?usage: impact-alias-proof.sh BIN_DIR OUT_DIR}" && pwd)
out=${2:?usage: impact-alias-proof.sh BIN_DIR OUT_DIR}
here=$(cd "$(dirname "$0")" && pwd)
mkdir -p "$out"
out=$(cd "$out" && pwd)
root=$(mktemp -d "$out/run.XXXXXX")
chmod 700 "$root"
export HOME="$root/home" XDG_CONFIG_HOME="$root/home/.config" XDG_DATA_HOME="$root/home/.local/share" XDG_CACHE_HOME="$root/home/.cache" XDG_STATE_HOME="$root/home/.local/state"
mkdir -p "$HOME"
project="$root/nextjs-alias"
cp -R "$here/../fixtures/nextjs-alias" "$project"
workspace="$root/workspace"
mkdir -m 700 "$workspace"
authority="$root/authority.secret"
python3 -c "import sys; sys.stdout.buffer.write(bytes([0x5a]) * 32)" > "$authority"
chmod 600 "$authority"
endpoint=$(mktemp -u "${TMPDIR:-/tmp}/impact-proof.XXXXXX.sock")
export BACKEND_LOCALD_AUTHORITY_SECRET_FILE="$authority"

"$bin/backend-locald" --endpoint "$endpoint" --workspace "$workspace" --profile builtin \
  --authority-secret-file "$authority" --max-frame 1048576 --timeout-ms 180000 \
  --idle-timeout-ms 0 --registry-offline > "$root/locald.log" 2>&1 &
owner=$!
trap 'kill "$owner" 2>/dev/null || true; rm -f "$endpoint"' EXIT
for _ in $(seq 1 200); do [ -S "$endpoint" ] && break; sleep 0.1; done

cli() { "$bin/backend-cli" --endpoint "$endpoint" --workspace "$workspace" --project "$project" "$@"; }
mcp() { # mcp TOOL JSON_ARGUMENTS
  printf '%s\n%s\n%s\n' \
    '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"impact-alias-proof","version":"1"}}}' \
    '{"jsonrpc":"2.0","method":"notifications/initialized","params":{}}' \
    "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/call\",\"params\":{\"name\":\"$1\",\"arguments\":$2}}" |
    "$bin/backend-mcp" --endpoint "$endpoint" --workspace "$workspace" --project "$project" |
    python3 -c "import sys, json
for line in sys.stdin:
    try: reply = json.loads(line)
    except ValueError: continue
    if reply.get('id') == 2:
        print(json.dumps(reply, indent=1)[:6000])
        sys.exit(1 if reply.get('error') or reply.get('result', {}).get('isError') else 0)
sys.exit('no reply to the tool call')"
}
section() { printf '\n===== %s\n' "$*"; }
fail() { echo "PROOF FAILED: $*" >&2; exit 1; }

# Reads an impact answer as JSON on stdin and prints its `semantic_data` value.
semantic_value() {
  python3 -c "import sys, json
def find(v, k):
    if isinstance(v, dict):
        if k in v: return v[k]
        for x in v.values():
            r = find(x, k)
            if r is not None: return r
    if isinstance(v, list):
        for x in v:
            r = find(x, k)
            if r is not None: return r
json.dump(find(json.load(sys.stdin), 'semantic_data')['value'], sys.stdout)"
}

# A debug owner opens its stores slowly, and the first command is refused with
# a retry hint until it has. Wait for it before submitting anything.
for _ in $(seq 1 600); do
  cli health >/dev/null 2>&1 && break
  kill -0 "$owner" 2>/dev/null || { echo "backend-locald exited early" >&2; tail -n 40 "$root/locald.log" >&2; exit 1; }
  sleep 1
done

section "add"
cli add "$project" --wait --wait-timeout 600 2>&1 | head -20

section "wait for the TypeScript evidence (code domain files >= 10: the fixture has ten TypeScript sources)"
files=0
for _ in $(seq 1 240); do
  # Until the evidence is published the answer is a refusal or lacks the
  # section; either way the count is zero and the wait continues.
  files=$(cli --format json --detail full impact "$project" --component Button 2>/dev/null | semantic_value 2>/dev/null |
    python3 -c "import sys, json
print(next((d['files'] for d in json.load(sys.stdin)['domains'] if d['domain'] == 'code'), 0))" 2>/dev/null || echo 0)
  [ "$files" -ge 10 ] && break
  sleep 1
done
echo "code files: $files"
[ "$files" -ge 10 ] || fail "the code domain published $files files, expected at least 10"

for component in Button NavBar Footer; do
  section "impact $project --component $component (absolute path)"
  cli impact "$project" --component "$component" --limit 40 2>&1
done

section "impact . --component Button (relative path)"
cli impact . --component Button --limit 40 2>&1
section "impact . --sql-table members (relative path)"
# No SQL worker is installed here, so the answer is a refusal; the proof is that
# it prints one.
cli impact . --sql-table members 2>&1 || true
section "impact ./ --component Button --format json (relative, rows)"
semantic=$(cli --format json --detail full impact ./ --component Button --limit 40 2>&1 | semantic_value)
printf '%s' "$semantic" | python3 -c "import sys, json
sem = json.load(sys.stdin)
print('domains:', json.dumps(sem['domains']))
print('totals:', json.dumps(sem['totals']))
for row in sem['rows']:
    print(row['strength'], row['node']['kind'], row['node']['name'], row['node'].get('path'), '|', row['why'])
for gap in sem['gaps']:
    print('GAP', gap['facet'], gap.get('count'), gap['note'][:300])"

printf '%s' "$semantic" | python3 -c "import sys, json
expected = {
    'src/app/page.tsx', 'src/app/about/page.tsx', 'src/app/dashboard/page.tsx',
    'src/app/broken/page.tsx', 'src/components/NavBar.tsx',
    'src/components/ui/Card.tsx', 'src/features/billing/Invoice.tsx',
}
sem = json.load(sys.stdin)
consumers = {r['node'].get('path') for r in sem['rows']} - {'src/components/Button.tsx'}
assert consumers == expected, f'Button consumers: missing {sorted(expected - consumers)}, unexpected {sorted(consumers - expected)}'
gaps = [g for g in sem['gaps'] if g['facet'] == 'code.unresolved-imports']
assert len(gaps) == 1, f'expected one code.unresolved-imports gap, got {sem[\"gaps\"]}'
assert gaps[0].get('count') == 1 and '@/widgets/Widget' in gaps[0]['note'], gaps[0]
print('proof: seven Button consumers and the unresolved @/widgets/Widget import are reported')" ||
  fail "the Button impact answer does not match the fixture"

section "MCP backend.impact component Button"
mcp backend.impact "{\"package\":\"$project\",\"component\":\"Button\",\"limit\":40}"
section "MCP backend.impact (relative package)"
mcp backend.impact '{"package":".","component":"Button","limit":40}'

section "references: Button declaration"
cli search Button 2>&1 | head -30
coordinate=$(cli --format json --detail full search Button 2>/dev/null | python3 -c "import sys, json
def walk(v):
    if isinstance(v, dict):
        for k, x in v.items():
            if k in ('operand', 'coordinate') and isinstance(x, str) and 'Button' in x and 'components/Button' in x: yield x
            yield from walk(x)
    elif isinstance(v, list):
        for x in v: yield from walk(x)
print(next(walk(json.load(sys.stdin)), ''))")
echo "coordinate: $coordinate"
[ -n "$coordinate" ] || fail "search found no Button declaration coordinate"
cli references "$coordinate" --limit 60 2>&1 | head -80
echo
echo "transcript root: $root"
