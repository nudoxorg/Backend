#!/usr/bin/env python3
"""Join closed MCP raw calls and measure a digest-only page-root BQPC rewrite.

This script never changes a MAC or claims a newly forged full token. It measures
the exact uncompressed BQPC frame delta and reports the outer-token size only as
an offline zlib estimate with the fixed existing MAC/expiry length retained.
"""

from __future__ import annotations

import base64
import collections
import hashlib
import json
import re
import tarfile
import zlib
from pathlib import Path


ROOT = Path("/Users/mileswirht/.cache/nudox/source-reviews/prod-native-combined-20261016")
COHORTS = {
    "angular5950": ROOT / "ROOT-ANGULAR5950-CLOSED-COLLECT01/courier.tar.gz",
    "httpie_graph639": ROOT / "ROOT-HTTPIE-GRAPH639-APPLICATION-CLOSED-COLLECT01/full-raw.tar",
    "cpp638_d824": ROOT / "ROOT-CPP638-D824-CLOSED-COLLECT04/full-raw.tar",
}
TOKEN = re.compile(r"mcp2-([^-]+)-(pc4-.*)-([0-9a-f]{32})\Z")
CALL_FILE = re.compile(r"evidence/mcp-\d+-(request|response)\.json\Z")
COUNTER = re.compile(r"evidence/mcp-(\d+)-(request|response)\.json\Z")


def digest(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def walk(value):
    if isinstance(value, dict):
        for key, item in value.items():
            if key.lower() in {"cursor", "nextcursor", "next_cursor"} and isinstance(item, str):
                yield item
            yield from walk(item)
    elif isinstance(value, list):
        for item in value:
            yield from walk(item)


def decode_owner(owner: str) -> tuple[bytes, bytes, bytes, list[dict]]:
    encoded = owner[4:]
    compressed = base64.urlsafe_b64decode(encoded + "=" * ((-len(encoded)) % 4))
    body = zlib.decompress(compressed)
    if not body.startswith(b"BQPC\x00\x01"):
        raise ValueError("pc4 body is not BQPC/1")
    offset = 6
    header_size = int.from_bytes(body[offset : offset + 4], "big")
    offset += 4
    header = body[offset : offset + header_size]
    offset += header_size
    has_certificate = body[offset]
    offset += 1
    if has_certificate != 1:
        raise ValueError("expected a certificate")
    count = int.from_bytes(body[offset : offset + 4], "big")
    offset += 4
    claims = []
    for claim_index in range(count):
        start = offset
        tag = body[offset]
        offset += 1
        schema = None
        claim_id = None
        canonical_length = 0
        if tag in (1, 2, 3, 4, 5, 6, 7):
            schema = body[offset]
            offset += 1
            claim_id = body[offset : offset + 32]
            offset += 32
            if tag in (1, 2, 4, 5, 6):
                size = int.from_bytes(body[offset : offset + 4], "big")
                offset += 4
                offset += size
                if tag == 6:
                    canonical_length = size
        elif tag == 8:
            offset += 32
            for _ in range(2):
                size = int.from_bytes(body[offset : offset + 4], "big")
                offset += 4 + size
        elif tag == 9:
            offset += 1 + 32 * 3
            size = int.from_bytes(body[offset : offset + 4], "big")
            offset += 4 + size
        elif tag == 10:
            offset += 4 * 32 + 2 + 32 + 8
        elif tag == 11:
            offset += 4 * 32
            size = int.from_bytes(body[offset : offset + 4], "big")
            offset += 4 + size
        else:
            raise ValueError(f"unknown claim tag {tag}")
        claims.append(
            {
                "index": claim_index,
                "tag": tag,
                "schema": schema,
                "id": claim_id,
                "start": start,
                "end": offset,
                "canonical_bytes": canonical_length,
            }
        )
    if offset != len(body):
        raise ValueError("trailing BQPC bytes")
    return body, header, compressed, claims


def root_ids(header: bytes) -> tuple[str, str | None]:
    command = json.loads(header)["command"]
    data = command.get("data", {})
    if command.get("kind") in {"search", "search_published_names", "name"}:
        cursor = data.get("cursor") or {}
        basis = data.get("basis")
    elif command.get("kind") in {"graph_page", "outline_page"}:
        page = data.get("page", {})
        continuation = page.get("continuation", {})
        cursor = continuation.get("cursor", continuation)
        basis = page.get("basis")
    else:
        raise ValueError(f"unsupported query command kind {command.get('kind')!r}")
    return cursor.get("root"), basis


def rewrite_root_claim(body: bytes, header: bytes, claims: list[dict]) -> tuple[bytes, int]:
    page_root, owner_root = root_ids(header)
    if not page_root or not owner_root:
        raise ValueError(f"query header omitted page/owner root: {header.decode()}")
    page_digest = bytes.fromhex(page_root)
    owner_digest = bytes.fromhex(owner_root)
    if page_digest == owner_digest:
        raise ValueError("owner and predecessor page roots are identical; compact rewrite is forbidden")
    root_claims = [claim for claim in claims if claim["tag"] == 6 and claim["id"] == page_digest]
    if len(root_claims) != 1:
        raise ValueError(f"expected one exact page-root witness, observed {len(root_claims)}")
    # BQPC root-commitment tag = 7, schema + exact digest retained; canonical
    # relation payload is the only removed field.
    root_claim = root_claims[0]
    replacement = bytes([7, root_claim["schema"]]) + root_claim["id"]
    rewritten_claims = []
    for claim in claims:
        if claim["index"] == root_claim["index"]:
            rewritten_claims.append(replacement)
        else:
            rewritten_claims.append(body[claim["start"] : claim["end"]])
    prefix = body[:6]
    header_length = len(header).to_bytes(4, "big")
    has_certificate_offset = 6 + 4 + len(header)
    prefix = body[:has_certificate_offset]
    count = len(claims).to_bytes(4, "big")
    new_body = prefix + b"\x01" + count + b"".join(rewritten_claims)
    if len(owner_digest) != 32 or len(page_digest) != 32:
        raise ValueError("bad root digest size")
    return new_body, root_claim["canonical_bytes"]


def collect(cohort: str, path: Path) -> tuple[list[dict], dict]:
    archive_bytes = path.read_bytes()
    archive_hash = digest(archive_bytes)
    with tarfile.open(path) as archive:
        names = archive.getnames()
        entries = []
        for name in names:
            if not CALL_FILE.fullmatch(name):
                continue
            raw = archive.extractfile(name).read()
            value = json.loads(raw)
            counter = int(COUNTER.fullmatch(name).group(1))
            entries.append((counter, name, raw, value))
        entries.sort(key=lambda item: item[0])
        outstanding = {}
        session = 0
        joined_calls = []
        correlation_errors = []
        for counter, name, raw, value in entries:
            if name.endswith("-request.json"):
                rpc_method = value.get("method")
                if rpc_method == "initialize":
                    if outstanding:
                        correlation_errors.append(
                            f"initialize at {name} began with outstanding IDs {sorted(map(str, outstanding))}"
                        )
                    session += 1
                    outstanding = {}
                if "id" not in value:
                    continue  # JSON-RPC notification has no response.
                request_id = value["id"]
                if request_id in outstanding:
                    correlation_errors.append(
                        f"duplicate outstanding request id {request_id!r} at {name} in session {session}"
                    )
                    continue
                logical_method = (
                    value.get("params", {}).get("name")
                    if rpc_method == "tools/call"
                    else rpc_method
                )
                outstanding[request_id] = (
                    session,
                    counter,
                    name,
                    raw,
                    logical_method,
                )
            else:
                if "id" not in value:
                    correlation_errors.append(f"response omitted id at {name}")
                    continue
                request_id = value["id"]
                joined = outstanding.pop(request_id, None)
                if joined is None:
                    correlation_errors.append(
                        f"orphan/ambiguous response id {request_id!r} at {name} in session {session}"
                    )
                    continue
                joined_calls.append((joined, (counter, name, raw, value)))
        if outstanding:
            correlation_errors.append(
                f"archive ended with outstanding IDs {sorted(map(str, outstanding))} in session {session}"
            )
        if correlation_errors:
            raise ValueError(f"{cohort}: session-aware JSON-RPC correlation failed: {correlation_errors[:8]}")
        occurrences = []
        unique_owners = {}
        for (session_id, request_counter, request_name, request_raw, method), (
            response_counter,
            response_name,
            response_raw,
            response,
        ) in joined_calls:
            if method not in {"backend.search", "backend.graph"}:
                continue
            for full_token in walk(response):
                match = TOKEN.fullmatch(full_token)
                if not match:
                    continue
                expiry, owner, mac = match.groups()
                body, header, compressed, claims = decode_owner(owner)
                new_body, root_canonical = rewrite_root_claim(body, header, claims)
                compressed_new = zlib.compress(new_body, level=6)
                owner_bytes = owner.encode()
                new_owner_bytes = b"pc4-" + base64.urlsafe_b64encode(compressed_new).rstrip(b"=")
                # Expiry and fixed-length MAC slots are carried only as length
                # accounting. No modified token/MAC is constructed or emitted.
                estimated_full_chars = len(f"mcp2-{expiry}-".encode()) + len(new_owner_bytes) + 1 + len(mac)
                owner_hash = digest(owner_bytes)
                occurrence = {
                    "cohort": cohort,
                    "request_file": request_name,
                    "request_sha256": digest(request_raw),
                    "request_id": response["id"],
                    "jsonrpc_session": session_id,
                    "request_counter": request_counter,
                    "method": method,
                    "response_file": response_name,
                    "response_counter": response_counter,
                    "response_sha256": digest(response_raw),
                    "full_token_sha256": digest(full_token.encode()),
                    "full_token_chars": len(full_token),
                    "owner_token_sha256": owner_hash,
                    "owner_token_chars": len(owner),
                    "expiry_text": expiry,
                    "mac_hex_chars": len(mac),
                    "command_kind": json.loads(header)["command"]["kind"],
                    "owner_root_id": root_ids(header)[1],
                    "page_cursor_root_id": root_ids(header)[0],
                    "page_root_claim_canonical_bytes": root_canonical,
                    "certificate_claims": len(claims),
                    "certificate_claim_tags": [claim["tag"] for claim in claims],
                    "bqpc_body_bytes_before": len(body),
                    "bqpc_body_bytes_after": len(new_body),
                    "bqpc_body_bytes_saved": len(body) - len(new_body),
                    "zlib_bytes_before": len(compressed),
                    "zlib_bytes_after_python_zlib_level6": len(compressed_new),
                    "owner_token_chars_after_python_zlib_level6": len(new_owner_bytes),
                    "full_token_chars_after_fixed_mac_length_python_zlib_estimate": estimated_full_chars,
                    "compression_runtime_note": f"Python zlib {zlib.ZLIB_VERSION}, level=6; not Rust flate2 output",
                }
                occurrences.append(occurrence)
                unique_owners.setdefault(owner_hash, occurrence)
        return occurrences, {
            "cohort": cohort,
            "archive": str(path),
            "archive_sha256": archive_hash,
            "response_files": sum(name.endswith("-response.json") for name in names),
            "jsonrpc_sessions": session,
            "session_aware_pairs": len(joined_calls),
            "joined_owner_occurrences": len(occurrences),
            "unique_owner_tokens": len(unique_owners),
            "duplicate_occurrences": len(occurrences) - len(unique_owners),
            "unique_owner_claim_tag_counts": {
                str(tag): count
                for tag, count in sorted(
                    collections.Counter(
                        tag
                        for row in unique_owners.values()
                        for tag in row["certificate_claim_tags"]
                    ).items()
                )
            },
            "unique_totals": {
                key: sum(row[key] for row in unique_owners.values())
                for key in (
                    "full_token_chars",
                    "owner_token_chars",
                    "bqpc_body_bytes_before",
                    "bqpc_body_bytes_after",
                    "bqpc_body_bytes_saved",
                    "zlib_bytes_before",
                    "zlib_bytes_after_python_zlib_level6",
                    "owner_token_chars_after_python_zlib_level6",
                    "full_token_chars_after_fixed_mac_length_python_zlib_estimate",
                    "page_root_claim_canonical_bytes",
                )
            },
        }


def main() -> None:
    output = Path(__file__).resolve().parents[1] / "evidence/pc4-page-root-cursor-20261020"
    output.mkdir(parents=True, exist_ok=True)
    manifest = []
    summaries = []
    for cohort, path in COHORTS.items():
        occurrences, summary = collect(cohort, path)
        manifest.extend(occurrences)
        summaries.append(summary)
    (output / "joined-manifest.json").write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n")
    (output / "summary.json").write_text(
        json.dumps(
            {
                "scope": "retained backend.search/backend.graph owners, joined chronologically within initialize-delimited JSON-RPC sessions",
                "method_source": "tools/call request.params.name; JSON-RPC method retained for other calls",
                "correlation": "sort evidence/mcp-NNNNNN-{request,response}.json by numeric counter; initialize resets session; match exactly one outstanding request id; duplicate IDs and orphan responses fail closed",
                "prior_global_id_join": "retained as evidence/pc4-page-root-cursor-20261020/legacy-global-id-join-invalidated.json; invalid because IDs recur across initialize-delimited sessions",
                "unique_definition": "exact owner pc4 token bytes, SHA-256 keyed; repeated response appearances counted as duplicates",
                "modified_mac": False,
                "full_token_after_size": "estimate only: Python zlib level 6 plus unchanged expiry/MAC field lengths; no token/MAC forged",
                "cohorts": summaries,
            },
            indent=2,
            sort_keys=True,
        )
        + "\n"
    )
    print(json.dumps(summaries, indent=2))


if __name__ == "__main__":
    main()
