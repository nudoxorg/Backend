#!/usr/bin/env bash
set -euo pipefail

backend_cli=${BACKEND_CLI:-backend-cli}
locald_bin=${BACKEND_LOCALD_BIN:-backend-locald}
journal_source=${REMOTE_MAVEN_CATALOG_JOURNAL:-}
labels_file=${REMOTE_MAVEN_LABELS:-}
expected_journal_sha256=7b156608e0427b60a7fd394f1d4d0c4b68aa7d7df6d55f0b12a1da80f9f36899

command -v "$backend_cli" >/dev/null 2>&1 || {
  printf '%s\n' "set BACKEND_CLI to the built backend CLI" >&2
  exit 69
}
command -v "$locald_bin" >/dev/null 2>&1 || {
  printf '%s\n' "set BACKEND_LOCALD_BIN to the built backend-locald binary" >&2
  exit 69
}
[[ -n "$journal_source" && -n "$labels_file" \
  && -f "$journal_source" && -f "$labels_file" ]] || {
  printf '%s\n' "set REMOTE_MAVEN_CATALOG_JOURNAL and REMOTE_MAVEN_LABELS to the frozen Maven fixtures" >&2
  exit 66
}
source_sha256=$(shasum -a 256 "$journal_source" | awk '{print $1}')
[[ "$source_sha256" == "$expected_journal_sha256" ]] || {
  printf 'frozen journal SHA-256 mismatch: %s\n' "$source_sha256" >&2
  exit 65
}

root=$(mktemp -d "${REMOTE_INDEX_TMPDIR:-/tmp}/backend-remote-catalog.XXXXXX")
root=$(cd "$root" && pwd -P)
owner_data=$root/owner
client_data=$root/client
project=$root/empty-project
endpoint=$owner_data/locald.sock
client_key=$client_data/remote-index-client.v1
capability_owner=$owner_data/maven-index-search.cap
capability_client=$client_data/maven-index-search.cap
cursor_file=$root/cursor.txt
coordinates_file=$root/coordinates.txt
page_count_file=$root/page-count.txt
peak_rss_file=$root/peak-rss.txt
locald_pid=

cleanup() {
  if [[ -n "$locald_pid" ]]; then
    kill "$locald_pid" 2>/dev/null || true
    wait "$locald_pid" 2>/dev/null || true
  fi
  if [[ "${REMOTE_INDEX_KEEP_ROOT:-}" == "1" ]]; then
    printf 'retained catalog journey data: %s\n' "$root" >&2
  else
    rm -rf "$root"
  fi
}
trap cleanup EXIT HUP INT TERM

mkdir -p "$owner_data/registry-discovery" "$client_data" "$project"
chmod 700 "$owner_data" "$client_data" "$project"
install -m 600 "$journal_source" "$owner_data/registry-discovery/catalog.journal"
[[ "$(shasum -a 256 "$owner_data/registry-discovery/catalog.journal" | awk '{print $1}')" == "$expected_journal_sha256" ]] || {
  printf '%s\n' "private journal copy changed during setup" >&2
  exit 65
}
: >"$cursor_file"
: >"$coordinates_file"
printf '0\n' >"$page_count_file"
printf '0\n' >"$peak_rss_file"

port=${REMOTE_INDEX_PORT:-$(python3 - <<'PY'
import socket
s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
s.bind(("127.0.0.1", 0))
print(s.getsockname()[1])
s.close()
PY
)}
address=127.0.0.1:$port

"$backend_cli" --workspace "$owner_data" cluster owner init \
  --bind "$address" --advertise "$address" >/dev/null

start_locald() {
  (
    cd "$project"
    exec "$locald_bin" --workspace "$owner_data" --endpoint "$endpoint" \
      --registry-discovery-source maven=https://search.maven.org \
      --registry-discovery-offline --registry-offline \
      --advisory-offline --forge-offline --idle-timeout-ms 0
  ) >>"$root/locald.log" 2>&1 &
  locald_pid=$!
  for _ in $(seq 1 200); do
    if [[ -S "$endpoint" ]] && "$backend_cli" --workspace "$owner_data" \
      --project "$project" --endpoint "$endpoint" health >/dev/null 2>&1; then
      return 0
    fi
    if ! kill -0 "$locald_pid" 2>/dev/null; then
      printf '%s\n' "backend-locald exited before becoming ready" >&2
      tail -n 80 "$root/locald.log" >&2
      exit 1
    fi
    sleep 0.1
  done
  printf '%s\n' "backend-locald did not become ready" >&2
  tail -n 80 "$root/locald.log" >&2
  exit 1
}

stop_locald() {
  kill "$locald_pid"
  wait "$locald_pid" 2>/dev/null || true
  locald_pid=
  for _ in $(seq 1 100); do
    [[ ! -S "$endpoint" ]] && return 0
    sleep 0.1
  done
  rm -f "$endpoint"
}

start_locald
owner_output=$("$backend_cli" --workspace "$owner_data" cluster owner show --format json)
owner_peer=$(printf '%s\n' "$owner_output" | python3 -c 'import json,sys; print(json.load(sys.stdin)["endpoint"])')
client_output=$("$backend_cli" --workspace "$client_data" cluster client init --key-file "$client_key")
client_peer=$(printf '%s\n' "$client_output" | sed -n 's/^Created private remote-index client identity \([[:xdigit:]]*\)\.$/\1/p')
[[ -n "$client_peer" ]] || {
  printf '%s\n' "could not read the public client peer ID" >&2
  exit 1
}

"$backend_cli" --workspace "$owner_data" --project "$project" \
  --endpoint "$endpoint" cluster owner grant product create \
  --client-peer "$client_peer" --capability-file "$capability_owner" \
  --operations index-search >/dev/null
cp "$capability_owner" "$capability_client"
chmod 600 "$capability_client"

connect_remote() {
  "$backend_cli" --workspace "$client_data" cluster client connect \
    --key-file "$client_key" --owner-peer "$owner_peer" \
    --owner-address "$address" --capability-file "$capability_client"
}
connect_report=$(connect_remote)
if [[ "$connect_report" != *"Authorized product root:"* \
  || "$connect_report" != *"Remote index-search snapshot:"* \
  || "$connect_report" != *"Operations: index-search"* ]]; then
  printf '%s\n' "remote client connect did not report its read-only catalog scope" >&2
  exit 1
fi
expected_snapshot=$(printf '%s\n' "$connect_report" | sed -n 's/^Remote index-search snapshot: //p')
[[ "$expected_snapshot" =~ ^[[:xdigit:]]{64}$ ]] || {
  printf '%s\n' "remote connect did not report the exact index-search snapshot" >&2
  exit 1
}

python3 - "$backend_cli" "$client_data" "$client_key" "$owner_peer" \
  "$address" "$capability_client" "$expected_snapshot" "$labels_file" \
  "$cursor_file" "$coordinates_file" "$page_count_file" "$peak_rss_file" \
  "$locald_pid" 5 <<'PY'
from pathlib import Path
import json, subprocess, sys, time
(cli, client_data, client_key, owner_peer, address, capability, expected_snapshot,
 labels_file, cursor_file, coordinates_file, page_count_file, peak_rss_file,
 owner_pid, stop_after) = sys.argv[1:]
stop_after = int(stop_after)
with open(labels_file, encoding="utf-8") as f:
    labels = json.load(f)
if labels.get("journal_sha256") != "7b156608e0427b60a7fd394f1d4d0c4b68aa7d7df6d55f0b12a1da80f9f36899":
    raise SystemExit("independent labels name a different frozen catalog journal")
identity = next(item["expected"] for item in labels["queries"] if item["query"] == "identity")
if len(identity) != 96:
    raise SystemExit("identity coordinate oracle no longer contains 96 labels")
page_count = int(Path(page_count_file).read_text(encoding="utf-8").strip() or "0")
peak_rss = int(Path(peak_rss_file).read_text(encoding="utf-8").strip() or "0")
cursor = Path(cursor_file).read_text(encoding="utf-8").strip()
while page_count < stop_after and (cursor or page_count == 0):
    args = [cli, "--workspace", client_data, "cluster", "client", "query",
            "--key-file", client_key, "--owner-peer", owner_peer,
            "--owner-address", address, "--capability-file", capability,
            "--operation", "index-search", "--value", "identity", "--limit", "10"]
    if cursor:
        args.extend(["--cursor", cursor])
    process = subprocess.Popen(args, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    samples = 0
    while True:
        try:
            owner_rss = int(subprocess.check_output(["ps", "-o", "rss=", "-p", owner_pid], text=True).strip())
            client_rss = int(subprocess.check_output(["ps", "-o", "rss=", "-p", str(process.pid)], text=True).strip())
            peak_rss = max(peak_rss, owner_rss + client_rss)
            samples += 1
        except (subprocess.CalledProcessError, ValueError):
            pass
        if process.poll() is not None:
            break
        time.sleep(0.01)
    stdout, stderr = process.communicate()
    if process.returncode:
        sys.stderr.write(stderr)
        raise SystemExit("remote discovered package search failed")
    if samples == 0:
        raise SystemExit("could not sample owner and client RSS during remote query")
    response = json.loads(stdout)
    if response.get("result") != "index-search-page":
        raise SystemExit("remote response was not the typed index-search page")
    page = response["data"]
    if bytes(page["snapshot"]).hex() != expected_snapshot:
        raise SystemExit("remote page snapshot does not match the signed grant")
    found = []
    for hit in page["hits"]:
        state, value = hit["state"], hit.get("value", {})
        if state == "package-group":
            if value["kind"] != "discovered":
                raise SystemExit("remote catalog page included a non-discovery package group")
            for release in value["releases"]:
                if release["state"] != "discovered":
                    raise SystemExit("discovery result included a non-discovery release")
                candidate = release["value"]
                if candidate["caught_up"]:
                    raise SystemExit("frozen journal result was falsely reported caught up")
                found.append(candidate["coordinate"])
        elif state == "discovered":
            if value["caught_up"]:
                raise SystemExit("frozen journal result was falsely reported caught up")
            found.append(value["coordinate"])
        else:
            raise SystemExit(f"unexpected non-discovery remote hit: {state}")
    with open(coordinates_file, "a", encoding="utf-8") as f:
        for coordinate in found:
            f.write(coordinate + "\n")
    cursor = page.get("next_cursor") or ""
    Path(cursor_file).write_text(cursor, encoding="utf-8")
    page_count += 1
    Path(page_count_file).write_text(str(page_count), encoding="utf-8")
    Path(peak_rss_file).write_text(str(peak_rss), encoding="utf-8")
    print(f"remote catalog page {page_count}: {len(found)} source coordinates")
    if not cursor:
        break
if page_count != stop_after and cursor:
    raise SystemExit(f"expected to stop after page {stop_after}, got {page_count}")
if stop_after == 5 and not cursor:
    raise SystemExit("cursor chain ended before the planned owner restart")
if not cursor:
    actual = Path(coordinates_file).read_text(encoding="utf-8").splitlines()
    if sorted(actual) != sorted(identity) or len(actual) != len(set(actual)):
        raise SystemExit("remote cursor chain did not return every independent coordinate exactly once")
    print(f"remote cursor chain: {len(actual)} independent source coordinates exactly once across {page_count} pages")
PY

stop_locald
start_locald
reconnect_report=$(connect_remote)
if [[ "$reconnect_report" != *"Remote index-search snapshot: $expected_snapshot"* ]]; then
  printf '%s\n' "cold owner restart changed the authorized search snapshot" >&2
  exit 1
fi
restart_journal_sha256=$(shasum -a 256 "$owner_data/registry-discovery/catalog.journal" | awk '{print $1}')
[[ "$restart_journal_sha256" == "$expected_journal_sha256" ]] || {
  printf '%s\n' "offline owner restart changed the frozen journal" >&2
  exit 1
}

python3 - "$backend_cli" "$client_data" "$client_key" "$owner_peer" \
  "$address" "$capability_client" "$expected_snapshot" "$labels_file" \
  "$cursor_file" "$coordinates_file" "$page_count_file" "$peak_rss_file" \
  "$locald_pid" 100 <<'PY'
from pathlib import Path
import json, subprocess, sys, time
(cli, client_data, client_key, owner_peer, address, capability, expected_snapshot,
 labels_file, cursor_file, coordinates_file, page_count_file, peak_rss_file,
 owner_pid, stop_after) = sys.argv[1:]
stop_after = int(stop_after)
labels = json.loads(Path(labels_file).read_text(encoding="utf-8"))
identity = next(item["expected"] for item in labels["queries"] if item["query"] == "identity")
page_count = int(Path(page_count_file).read_text(encoding="utf-8").strip())
peak_rss = int(Path(peak_rss_file).read_text(encoding="utf-8").strip())
cursor = Path(cursor_file).read_text(encoding="utf-8").strip()
while cursor and page_count < stop_after:
    args = [cli, "--workspace", client_data, "cluster", "client", "query",
            "--key-file", client_key, "--owner-peer", owner_peer,
            "--owner-address", address, "--capability-file", capability,
            "--operation", "index-search", "--value", "identity", "--limit", "10",
            "--cursor", cursor]
    process = subprocess.Popen(args, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    samples = 0
    while True:
        try:
            owner_rss = int(subprocess.check_output(["ps", "-o", "rss=", "-p", owner_pid], text=True).strip())
            client_rss = int(subprocess.check_output(["ps", "-o", "rss=", "-p", str(process.pid)], text=True).strip())
            peak_rss = max(peak_rss, owner_rss + client_rss)
            samples += 1
        except (subprocess.CalledProcessError, ValueError):
            pass
        if process.poll() is not None:
            break
        time.sleep(0.01)
    stdout, stderr = process.communicate()
    if process.returncode:
        sys.stderr.write(stderr)
        raise SystemExit("remote cursor continuation failed after owner restart")
    if samples == 0:
        raise SystemExit("could not sample owner and client RSS during cursor continuation")
    response = json.loads(stdout)
    if response.get("result") != "index-search-page":
        raise SystemExit("remote continuation was not the typed index-search page")
    page = response["data"]
    if bytes(page["snapshot"]).hex() != expected_snapshot:
        raise SystemExit("post-restart page snapshot does not match its signed grant")
    found = []
    for hit in page["hits"]:
        state, value = hit["state"], hit.get("value", {})
        if state == "package-group":
            if value["kind"] != "discovered":
                raise SystemExit("post-restart page included a non-discovery package group")
            for release in value["releases"]:
                if release["state"] != "discovered" or release["value"]["caught_up"]:
                    raise SystemExit("post-restart release lost its unacquired/windowed evidence")
                found.append(release["value"]["coordinate"])
        elif state == "discovered":
            if value["caught_up"]:
                raise SystemExit("post-restart result was falsely reported caught up")
            found.append(value["coordinate"])
        else:
            raise SystemExit(f"unexpected post-restart result state: {state}")
    with open(coordinates_file, "a", encoding="utf-8") as f:
        for coordinate in found:
            f.write(coordinate + "\n")
    cursor = page.get("next_cursor") or ""
    Path(cursor_file).write_text(cursor, encoding="utf-8")
    page_count += 1
    Path(page_count_file).write_text(str(page_count), encoding="utf-8")
    Path(peak_rss_file).write_text(str(peak_rss), encoding="utf-8")
    print(f"remote catalog page {page_count}: {len(found)} source coordinates (after cold restart)")
actual = Path(coordinates_file).read_text(encoding="utf-8").splitlines()
if cursor or sorted(actual) != sorted(identity) or len(actual) != len(set(actual)):
    raise SystemExit("remote cursor chain did not return every independent coordinate exactly once")
if page_count != 10:
    raise SystemExit(f"expected ten bounded remote pages, received {page_count}")
print(f"remote cursor chain: {len(actual)} independent source coordinates exactly once across {page_count} pages")
PY

"$backend_cli" --workspace "$owner_data" --format json \
  cluster owner grant list >"$root/grants.json"
python3 - "$root/grants.json" "$expected_snapshot" "$peak_rss_file" <<'PY'
import json, sys
with open(sys.argv[1], encoding="utf-8") as f:
    grants = json.load(f)
grant = next(item for item in grants if item["product"])
scope = grant["product"]
if scope["operations"] != ["index-search"]:
    raise SystemExit("owner persisted a broader operation scope than requested")
if scope["indexSearchSnapshot"] != sys.argv[2]:
    raise SystemExit("owner ledger lost the exact composite search-snapshot binding")
if grant["requests"] < 10 or grant["responseBytes"] == 0:
    raise SystemExit("durable grant accounting missed remote catalog requests")
if grant["responseBytes"] > grant["byteBudget"] or grant["requests"] > grant["requestBudget"]:
    raise SystemExit("durable remote catalog grant usage exceeded its signed budget")
peak = int(open(sys.argv[3], encoding="utf-8").read().strip())
if peak <= 0:
    raise SystemExit("owner/client RSS sample was empty")
print(f"durable remote grant: {grant['requests']}/{grant['requestBudget']} requests, {grant['responseBytes']}/{grant['byteBudget']} response bytes")
print(f"sampled owner plus client RSS peak: {peak} KiB")
PY

printf 'remote catalog journey passed (frozen Maven journal, exact composite snapshot, cold restart, typed cursor chain)\n'
