#!/usr/bin/env bash
# Live container: ingest serde, wait until it is Stored, then require a
# lib.rs-style package card (description / keywords / license) and a symbol hit.
set -uo pipefail

BASE_URL="${NUDOX_BASE_URL:-http://127.0.0.1:8080}"
TIMEOUT="${NUDOX_INDEX_TIMEOUT:-240}"
FAILURES=0

log() { printf '%s\n' "$*" >&2; }
fail() { log "FAIL: $*"; FAILURES=$((FAILURES + 1)); }
pass() { log "PASS: $*"; }

require_tools() {
  command -v curl >/dev/null 2>&1 || { log "curl is required"; exit 1; }
  command -v jq >/dev/null 2>&1 || { log "jq is required"; exit 1; }
}

ensure_up() {
  if ! curl -sf --connect-timeout 2 --max-time 5 "${BASE_URL}/healthz" >/dev/null 2>&1; then
    log "nothing is listening at ${BASE_URL}"
    exit 2
  fi
}

# Prints the response body. Sets HTTP_CODE.
request() {
  local method="$1" path="$2" body="${3:-}"
  local out
  out="$(mktemp)"
  if [[ -n "${body}" ]]; then
    HTTP_CODE="$(
      curl -sS --connect-timeout 2 --max-time 60 \
        -o "${out}" -w '%{http_code}' \
        -X "${method}" "${BASE_URL}${path}" \
        -H 'Content-Type: application/json' \
        -d "${body}" || echo 000
    )"
  else
    HTTP_CODE="$(
      curl -sS --connect-timeout 2 --max-time 30 \
        -o "${out}" -w '%{http_code}' \
        -X "${method}" "${BASE_URL}${path}" || echo 000
    )"
  fi
  cat "${out}"
  rm -f "${out}"
}

ingest_serde() {
  log "==> ingest serde 1.0.210"
  local raw
  raw="$(request POST /packages '{"ecosystem":"rust","name":"serde","version":"1.0.210"}')"
  if [[ "${HTTP_CODE}" != "200" ]]; then
    fail "ingest: HTTP ${HTTP_CODE} ${raw}"
    return 1
  fi
  PACKAGE_ID="$(jq -r '.package' <<<"${raw}")"
  if [[ -z "${PACKAGE_ID}" || "${PACKAGE_ID}" == "null" ]]; then
    fail "ingest: response has no package id: ${raw}"
    return 1
  fi
  local state_key
  state_key="$(jq -r '.state | keys[0]' <<<"${raw}")"
  if [[ "${state_key}" == "Failed" || "${state_key}" == "DeadLettered" ]]; then
    log "ingest: state ${state_key}; requesting sync"
    request POST "/packages/${PACKAGE_ID}/sync" '{}' >/dev/null
    if [[ "${HTTP_CODE}" != "200" ]]; then
      fail "sync: HTTP ${HTTP_CODE}"
      return 1
    fi
  fi
  pass "ingest: package ${PACKAGE_ID}"
  return 0
}

wait_stored() {
  log "==> wait for Stored (timeout ${TIMEOUT}s)"
  local start now raw state_key
  start="$(date +%s)"
  while true; do
    raw="$(request GET "/packages/${PACKAGE_ID}")"
    if [[ "${HTTP_CODE}" != "200" ]]; then
      fail "status: HTTP ${HTTP_CODE} ${raw}"
      return 1
    fi
    state_key="$(jq -r '.state | keys[0]' <<<"${raw}")"
    case "${state_key}" in
      Stored)
        pass "status: Stored"
        return 0
        ;;
      Failed|DeadLettered)
        fail "status: ${state_key} $(jq -c '.state' <<<"${raw}")"
        return 1
        ;;
    esac
    now="$(date +%s)"
    if (( now - start >= TIMEOUT )); then
      fail "status: still ${state_key} after ${TIMEOUT}s $(jq -c '.state' <<<"${raw}")"
      return 1
    fi
    sleep 2
  done
}

assert_package_card() {
  log "==> package card for serde"
  local raw
  raw="$(request POST /packages/search '{"target":"Packages","text":"serde","page":{"limit":5}}')"
  if [[ "${HTTP_CODE}" != "200" ]]; then
    fail "package search: HTTP ${HTTP_CODE} ${raw}"
    return 1
  fi
  printf '%s\n' "${raw}" >&2
  local name state_key signals
  name="$(jq -r '.items[0].value.coordinates.name.canonical // .items[0].value.coordinates.name // empty' <<<"${raw}")"
  state_key="$(jq -r '.items[0].value.state | keys[0] // empty' <<<"${raw}")"
  signals="$(
    jq -r '
      .items[0].value
      | [
          (if (.description // "") != "" then "description" else empty end),
          (if (.keywords // []) | length > 0 then "keywords" else empty end),
          (if (.license // "") != "" then "license" else empty end)
        ] | join(",")
    ' <<<"${raw}"
  )"
  if [[ "${name}" != "serde" ]]; then
    fail "package search: top hit name is '${name}', want serde"
    return 1
  fi
  if [[ "${state_key}" != "Stored" ]]; then
    fail "package search: top hit state is ${state_key}, want Stored"
    return 1
  fi
  if [[ -z "${signals}" ]]; then
    fail "package search: stored serde hit has no description, keywords, or license"
    return 1
  fi
  pass "package card: serde Stored with ${signals}"
  return 0
}

assert_symbol_hit() {
  log "==> symbol search Serialize"
  local raw
  raw="$(request POST /search '{"target":"Symbols","text":"Serialize","page":{"limit":10}}')"
  if [[ "${HTTP_CODE}" != "200" ]]; then
    fail "symbol search: HTTP ${HTTP_CODE} ${raw}"
    return 1
  fi
  if ! grep -q 'Serialize' <<<"${raw}"; then
    fail "symbol search: body has no Serialize hit"
    printf '%s\n' "${raw}" >&2
    return 1
  fi
  pass "symbol search: Serialize is present"
  return 0
}

main() {
  require_tools
  ensure_up
  ingest_serde || exit "${FAILURES}"
  wait_stored || exit "${FAILURES}"
  assert_package_card || true
  assert_symbol_hit || true
  log "==> summary: failures=${FAILURES}"
  exit "${FAILURES}"
}

main "$@"
