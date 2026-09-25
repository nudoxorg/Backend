#!/usr/bin/env bash
# Integration test against a live nudox-serve container (nix2container image).
# Contract: GET /healthz, POST /packages/search with heart::Query wire shape.
set -uo pipefail

BASE_URL="${NUDOX_BASE_URL:-http://127.0.0.1:8080}"
FAILURES=0

log() {
  printf '%s\n' "$*" >&2
}

fail() {
  log "FAIL: $*"
  FAILURES=$((FAILURES + 1))
}

pass() {
  log "PASS: $*"
}

require_tools() {
  command -v curl >/dev/null 2>&1 || {
    log "curl is required"
    exit 1
  }
  command -v jq >/dev/null 2>&1 || {
    log "jq is required"
    exit 1
  }
  command -v podman >/dev/null 2>&1 || {
    log "podman is required"
    exit 1
  }
}

ensure_container_up() {
  if ! curl -sf --connect-timeout 2 --max-time 5 "${BASE_URL}/healthz" >/dev/null 2>&1; then
    podman ps
    exit 2
  fi
}

post_packages_search() {
  local body="$1"
  local response_file
  response_file="$(mktemp)"
  local http_code
  http_code="$(
    curl -sS --connect-timeout 2 --max-time 30 \
      -o "${response_file}" \
      -w '%{http_code}' \
      -X POST "${BASE_URL}/packages/search" \
      -H 'Content-Type: application/json' \
      -d "${body}" \
      2>/dev/null || echo '000'
  )"
  printf '%s\n' "${http_code}"
  cat "${response_file}"
  rm -f "${response_file}"
}

assert_page_items_array() {
  local json="$1"
  local label="$2"
  if ! jq -e '.items | type == "array"' >/dev/null 2>&1 <<<"${json}"; then
    fail "${label}: response is not a page with an items array"
    return 1
  fi
  return 0
}

assert_hit_projection_keys() {
  local json="$1"
  local label="$2"
  local bad
  bad="$(
    jq -r '
      def check_hit(h):
        (h.description? | if . == null then true else type == "string" end)
        and (h.downloads? | if . == null then true else type == "number" end)
        and (h.dependents? | if . == null then true else type == "number" end)
        and (h.keywords? | if . == null then true else type == "array" end)
        and (h.license? | if . == null then true else type == "string" end);
      .items[]
      | if has("value") then .value else . end
      | if check_hit(.) then empty else "bad" end
    ' <<<"${json}" 2>/dev/null | head -n 1
  )"
  if [[ -n "${bad}" ]]; then
    fail "${label}: hit projection fields have unexpected types"
    return 1
  fi
  return 0
}

test_health() {
  log "==> health"
  if curl -sf --connect-timeout 2 --max-time 5 "${BASE_URL}/healthz" >/dev/null; then
    pass "health: GET /healthz returned 200"
    return 0
  fi
  fail "health: GET /healthz did not return 200"
  return 1
}

test_empty_text() {
  log "==> empty_text"
  local body='{"target":"Packages","text":"","page":{"limit":10}}'
  local raw http_code body_text
  raw="$(post_packages_search "${body}")"
  http_code="$(printf '%s' "${raw}" | head -n 1)"
  body_text="$(printf '%s' "${raw}" | tail -n +2)"

  if [[ "${http_code}" == "500" || "${http_code}" == "000" ]]; then
    fail "empty_text: got HTTP ${http_code} (expected 4xx or empty page, not 500)"
    return 1
  fi

  if [[ "${http_code}" =~ ^4 ]]; then
    pass "empty_text: empty query rejected with HTTP ${http_code}"
    log "empty_text: outcome=4xx"
    return 0
  fi

  if [[ "${http_code}" == "200" ]]; then
    if assert_page_items_array "${body_text}" "empty_text"; then
      local count
      count="$(jq -r '.items | length' <<<"${body_text}")"
      if [[ "${count}" == "0" ]]; then
        pass "empty_text: HTTP 200 with empty items array"
        log "empty_text: outcome=empty_page"
        return 0
      fi
      fail "empty_text: HTTP 200 returned ${count} items for empty query"
      return 1
    fi
    return 1
  fi

  fail "empty_text: unexpected HTTP ${http_code}"
  return 1
}

test_serde_search() {
  log "==> serde_search"
  local body='{"target":"Packages","text":"serde","page":{"limit":10}}'
  local raw http_code body_text
  raw="$(post_packages_search "${body}")"
  http_code="$(printf '%s' "${raw}" | head -n 1)"
  body_text="$(printf '%s' "${raw}" | tail -n +2)"

  if [[ "${http_code}" != "200" ]]; then
    fail "serde_search: expected HTTP 200, got ${http_code}"
    [[ -n "${body_text}" ]] && log "${body_text}"
    return 1
  fi

  if ! assert_page_items_array "${body_text}" "serde_search"; then
    return 1
  fi

  assert_hit_projection_keys "${body_text}" "serde_search" || return 1

  local count
  count="$(jq -r '.items | length' <<<"${body_text}")"
  pass "serde_search: HTTP 200 with items array (${count} hits)"
  printf '%s' "${body_text}"
  return 0
}

test_ranking_shape() {
  local json="$1"
  log "==> ranking_shape"

  local pair_count
  pair_count="$(
    jq -r '
      [.items[]
        | select(has("score") and has("value"))
        | select((.value.dependents? | type) == "number")
        | {score: .score, dependents: .value.dependents}
      ] | length
    ' <<<"${json}" 2>/dev/null || echo 0
  )"

  if [[ "${pair_count}" -lt 2 ]]; then
    pass "ranking_shape: skipped (fewer than two hits with numeric dependents)"
    log "ranking_shape: outcome=skipped"
    return 0
  fi

  local violation
  violation="$(
    jq -r '
      [.items[]
        | select(has("score") and has("value"))
        | select((.value.dependents? | type) == "number")
        | {score: .score, dependents: .value.dependents}
      ]
      | . as $hits
      | range(0; ($hits | length) - 1)
      | select($hits[.].score < $hits[.+1].score
        or ($hits[.].score == $hits[.+1].score
          and $hits[.].dependents < $hits[.+1].dependents))
      | "\(.): score/dependents order violated"
    ' <<<"${json}" 2>/dev/null | head -n 1
  )"

  if [[ -n "${violation}" ]]; then
    fail "ranking_shape: ${violation}"
    return 1
  fi

  pass "ranking_shape: dependents non-increasing as scores descend"
  log "ranking_shape: outcome=checked"
  return 0
}

main() {
  require_tools
  ensure_container_up

  test_health || true
  test_empty_text || true

  local serde_json=""
  if serde_json="$(test_serde_search)"; then
    test_ranking_shape "${serde_json}" || true
  else
    fail "ranking_shape: skipped because serde_search failed"
  fi

  log "==> summary: failures=${FAILURES}"
  exit "${FAILURES}"
}

main "$@"
