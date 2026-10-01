#!/usr/bin/env python3
"""Deterministic, source-independent IR segmentation and closure experiment.

This script uses only the Python standard library. It is a reproducible
locality fixture, not a wire-format implementation or a speedup estimate.
It contrasts the current ordinal 1 MiB byte windows with row-boundary
content-defined chunks, stable first-byte key-prefix buckets, and content-
defined leaves for a jumbo value under an ordered rope tree. Closure accounting
uses a deterministic persistent treap with a fixed 136-byte node encoding so it
reports both payload and closure work under one explicit model.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import time
from dataclasses import dataclass
from collections import deque

MIB = 1024 * 1024
SEGMENT_MAX = MIB
CDC_MIN = 256 * 1024
CDC_MAX = MIB
CDC_ROW_WINDOW = 64
CDC_ROW_BASE = 257
CDC_ROW_POWER = pow(CDC_ROW_BASE, CDC_ROW_WINDOW, 1 << 64)
CDC_ROW_MASK = (1 << 9) - 1  # ~512 records between cuts; about 0.43 MiB here.
CLOSURE_NODE_BYTES = 136
JUMBO_ROPE_NODE_WIRE_BYTES = 142
MASK64 = (1 << 64) - 1
SEED = 0x5EED_CAFE_D00D_BEEF
CDC_BLOB_WINDOW = 64
CDC_BLOB_BASE = 257
CDC_BLOB_POWER = pow(CDC_BLOB_BASE, CDC_BLOB_WINDOW, 1 << 64)
GEAR = tuple(
    int.from_bytes(
        hashlib.blake2b(b"ir-delta-cdc-gear-v1\0" + bytes([byte]), digest_size=8).digest(),
        "little",
    )
    for byte in range(256)
)


@dataclass(frozen=True)
class Row:
    logical_id: int
    stable_key: bytes
    payload: bytes


@dataclass(frozen=True)
class Segment:
    key: bytes
    digest: bytes
    byte_length: int


@dataclass(frozen=True)
class Plan:
    segments: tuple[Segment, ...]
    planner_read_bytes: int
    segment_hash_bytes: int
    plan_ns: int


def stable_key(logical_id: int) -> bytes:
    return hashlib.blake2b(
        b"ir-delta-fixture-family-v1\0" + logical_id.to_bytes(8, "big", signed=True),
        digest_size=16,
    ).digest()


def field_bytes(name: str, value: bytes) -> bytes:
    return name.encode() + b"\0" + len(value).to_bytes(4, "big") + value


def make_row(
    logical_id: int,
    *,
    expanded_attribute: bool = False,
    edited_docs: bool = False,
    renamed: bool = False,
    jumbo_field: bytes = b"",
) -> Row:
    key = stable_key(logical_id)
    name = ("symbol-%08d%s" % (logical_id, "-renamed" if renamed else "")).encode()
    attribute = ("attribute-%08d" % logical_id).encode()
    if expanded_attribute:
        attribute += b"x" * 4096

    doc_word = b"edit!!" if edited_docs else b"before"
    documentation = (
        ("documentation-%08d-" % logical_id).encode()
        + doc_word
        + b"d" * 320
    )
    code = ("code-sample-%08d-" % logical_id).encode() + b"c" * 64
    extension = ("rust-macro-%d" % (logical_id % 7)).encode()
    core_hash = hashlib.blake2b(
        b"core-payload-v1\0" + logical_id.to_bytes(8, "big", signed=True),
        digest_size=16,
    ).digest()

    payload = bytearray(key + b"\xa5" * 16 + core_hash + b"\x00\x01")
    payload.extend(field_bytes("name", name))
    payload.extend(field_bytes("attribute", attribute))
    payload.extend(field_bytes("documentation", documentation))
    payload.extend(field_bytes("code", code))
    payload.extend(field_bytes("rust-extension", extension))
    if jumbo_field:
        payload.extend(field_bytes("jumbo-field", jumbo_field))
    return Row(logical_id, key, bytes(payload))


def rows_for_case(count: int, case: str) -> tuple[list[Row], list[Row]]:
    base = [make_row(index) for index in range(count)]
    if case == "noop":
        return base, list(base)
    if case.startswith("attribute-"):
        target = {
            "attribute-early": 0,
            "attribute-mid": count // 2,
            "attribute-tail": count - 1,
        }[case]
        target_rows = list(base)
        target_rows[target] = make_row(target, expanded_attribute=True)
        return base, target_rows
    if case == "docs-only":
        target = count // 2
        before = make_row(target)
        after = make_row(target, edited_docs=True)
        assert len(before.payload) == len(after.payload)
        target_rows = list(base)
        target_rows[target] = after
        return base, target_rows
    if case == "rename-mid":
        target = count // 2
        target_rows = list(base)
        target_rows[target] = make_row(target, renamed=True)
        return base, target_rows
    if case == "insert-head":
        return base, [make_row(-1), *base]
    if case == "delete-head":
        return base, base[1:]
    raise ValueError("unknown case: " + case)


def with_graph_facts(rows: list[Row]) -> list[Row]:
    """Attach one canonical ring link and two distinct source observations per row."""
    ordered = sorted(rows, key=lambda row: row.logical_id)
    if not ordered:
        return []
    result = []
    for index, row in enumerate(ordered):
        target = ordered[(index + 1) % len(ordered)]
        link = (
            b"link-v1\0"
            + row.stable_key
            + target.stable_key
            + b"\x02"  # TypeReference
        )
        occurrences = bytearray()
        for site, confidence in ((0, b"syntactic"), (1, b"compiler")):
            source_start = max(0, row.logical_id + 1) * 32 + site * 4
            occurrence = (
                b"occurrence-v1\0"
                + row.stable_key
                + target.stable_key
                + bytes([site])
                + confidence
                + b"src/lib.rs\0"
                + source_start.to_bytes(4, "big")
                + (source_start + 2).to_bytes(4, "big")
            )
            occurrences.extend(field_bytes("occurrence", occurrence))
        payload = row.payload + field_bytes("canonical-link", link) + bytes(occurrences)
        result.append(Row(row.logical_id, row.stable_key, payload))
    return result


def encode_image(rows: list[Row]) -> bytes:
    ordered = sorted(rows, key=lambda row: row.logical_id)
    out = bytearray(b"NXFI-locality-fixture-v1\0")
    out.extend(len(ordered).to_bytes(8, "big"))
    for row in ordered:
        out.extend(len(row.payload).to_bytes(4, "big"))
        out.extend(row.payload)
    return bytes(out)


def make_segment(key: bytes, payload: bytes) -> Segment:
    if not payload or len(payload) > SEGMENT_MAX:
        raise ValueError("segment exceeds the 1 MiB fixture limit")
    return Segment(key, hashlib.sha256(payload).digest(), len(payload))


def ordinal_plan(image: bytes) -> Plan:
    started = time.perf_counter_ns()
    segments = []
    for ordinal, start in enumerate(range(0, len(image), SEGMENT_MAX)):
        payload = image[start : start + SEGMENT_MAX]
        key = bytes(24) + ordinal.to_bytes(8, "big")
        segments.append(make_segment(key, payload))
    return Plan(tuple(segments), len(image), len(image), time.perf_counter_ns() - started)


def framed_row(row: Row) -> bytes:
    return len(row.payload).to_bytes(4, "big") + row.payload


def cdc_plan(rows: list[Row]) -> Plan:
    started = time.perf_counter_ns()
    ordered = sorted(rows, key=lambda row: row.stable_key)
    result = []
    current = bytearray()
    first_key = None
    rolling = 0
    window: deque[int] = deque()
    planner_read_bytes = 0
    for row in ordered:
        framed = framed_row(row)
        if len(framed) > CDC_MAX:
            raise ValueError(
                "row %s is %d bytes; row-boundary CDC cannot admit a jumbo row"
                % (row.stable_key.hex(), len(framed))
            )
        # Flush before appending the next row. Checking only after append can
        # overshoot the byte cap by almost one whole record.
        if current and len(current) + len(framed) > CDC_MAX:
            result.append(make_segment(first_key + bytes(16), bytes(current)))
            current.clear()
            first_key = None
        if first_key is None:
            first_key = row.stable_key
        fingerprint = int.from_bytes(
            hashlib.blake2b(
                row.stable_key + hashlib.sha256(framed).digest(), digest_size=8
            ).digest(),
            "little",
        )
        if len(window) == CDC_ROW_WINDOW:
            oldest = window.popleft()
            rolling = (
                rolling * CDC_ROW_BASE + fingerprint - oldest * CDC_ROW_POWER
            ) & MASK64
        else:
            rolling = (rolling * CDC_ROW_BASE + fingerprint) & MASK64
        window.append(fingerprint)
        current.extend(framed)
        planner_read_bytes += len(framed)
        if len(current) >= CDC_MIN and (
            (rolling & CDC_ROW_MASK) == 0 or len(current) >= CDC_MAX
        ):
            result.append(make_segment(first_key + bytes(16), bytes(current)))
            current.clear()
            first_key = None
    if current:
        result.append(make_segment(first_key + bytes(16), bytes(current)))
    segment_hash_bytes = sum(segment.byte_length for segment in result)
    return Plan(
        tuple(result),
        planner_read_bytes,
        segment_hash_bytes,
        time.perf_counter_ns() - started,
    )


def prefix_plan(rows: list[Row]) -> Plan:
    started = time.perf_counter_ns()
    groups: dict[int, list[tuple[bytes, bytes]]] = {}
    planner_read_bytes = 0
    for row in rows:
        framed = framed_row(row)
        groups.setdefault(row.stable_key[0], []).append((row.stable_key, framed))
        planner_read_bytes += len(framed)
    result = []
    for prefix, members in sorted(groups.items()):
        payload = b"".join(value for _, value in sorted(members))
        key = b"\x00" + bytes([prefix]) + bytes(30)
        result.append(make_segment(key, payload))
    segment_hash_bytes = sum(segment.byte_length for segment in result)
    return Plan(
        tuple(result),
        planner_read_bytes,
        segment_hash_bytes,
        time.perf_counter_ns() - started,
    )


def delta(base: Plan, target: Plan) -> dict[str, int]:
    before = {segment.key: segment for segment in base.segments}
    after = {segment.key: segment for segment in target.segments}
    reused_bytes = fetched_bytes = removed_bytes = 0
    reused = fetched = removed = 0
    for key, segment in after.items():
        old = before.get(key)
        if old == segment:
            reused += 1
            reused_bytes += segment.byte_length
        else:
            fetched += 1
            fetched_bytes += segment.byte_length
            if old is not None:
                removed += 1
                removed_bytes += old.byte_length
    for key, segment in before.items():
        if key not in after:
            removed += 1
            removed_bytes += segment.byte_length
    return {
        "base_segments": len(before),
        "target_segments": len(after),
        "reused_segments": reused,
        "fetched_segments": fetched,
        "removed_segments": removed,
        "reused_bytes": reused_bytes,
        "fetched_bytes": fetched_bytes,
        "removed_bytes": removed_bytes,
    }


class ClosureNode:
    __slots__ = ("key", "digest", "byte_length", "priority", "left", "right", "digest_root")

    def __init__(
        self,
        key: bytes,
        value: tuple[bytes, int],
        left: "ClosureNode | None" = None,
        right: "ClosureNode | None" = None,
    ) -> None:
        self.key = key
        self.digest, self.byte_length = value
        self.priority = int.from_bytes(hashlib.blake2b(key, digest_size=8).digest(), "big")
        self.left = left
        self.right = right
        left_root = left.digest_root if left else bytes(32)
        right_root = right.digest_root if right else bytes(32)
        self.digest_root = hashlib.sha256(
            key
            + self.digest
            + self.byte_length.to_bytes(8, "big")
            + left_root
            + right_root
        ).digest()


def _node(key: bytes, value: tuple[bytes, int], left=None, right=None) -> ClosureNode:
    return ClosureNode(key, value, left, right)


def _insert(root, key: bytes, value: tuple[bytes, int], counters: list[int]):
    if root is None:
        counters[1] += 1
        return _node(key, value)
    counters[0] += 1
    if key == root.key:
        counters[1] += 1
        return _node(key, value, root.left, root.right)
    if key < root.key:
        left = _insert(root.left, key, value, counters)
        current = _node(root.key, (root.digest, root.byte_length), left, root.right)
        counters[1] += 1
        if left.priority < current.priority:
            right = _node(
                current.key,
                (current.digest, current.byte_length),
                left.right,
                current.right,
            )
            result = _node(left.key, (left.digest, left.byte_length), left.left, right)
            counters[1] += 2
            return result
        return current
    right = _insert(root.right, key, value, counters)
    current = _node(root.key, (root.digest, root.byte_length), root.left, right)
    counters[1] += 1
    if right.priority < current.priority:
        left = _node(
            current.key,
            (current.digest, current.byte_length),
            current.left,
            right.left,
        )
        result = _node(right.key, (right.digest, right.byte_length), left, right.right)
        counters[1] += 2
        return result
    return current


def _merge(left, right, counters: list[int]):
    if left is None:
        return right
    if right is None:
        return left
    counters[0] += 1
    if left.priority < right.priority:
        new_right = _merge(left.right, right, counters)
        counters[1] += 1
        return _node(left.key, (left.digest, left.byte_length), left.left, new_right)
    new_left = _merge(left, right.left, counters)
    counters[1] += 1
    return _node(right.key, (right.digest, right.byte_length), new_left, right.right)


def _erase(root, key: bytes, counters: list[int]):
    if root is None:
        return None
    counters[0] += 1
    if key == root.key:
        return _merge(root.left, root.right, counters)
    if key < root.key:
        left = _erase(root.left, key, counters)
        if left is root.left:
            return root
        counters[1] += 1
        return _node(root.key, (root.digest, root.byte_length), left, root.right)
    right = _erase(root.right, key, counters)
    if right is root.right:
        return root
    counters[1] += 1
    return _node(root.key, (root.digest, root.byte_length), root.left, right)


def _build_closure(segments: tuple[Segment, ...]):
    root = None
    counters = [0, 0]
    for segment in segments:
        root = _insert(
            root,
            segment.key,
            (segment.digest, segment.byte_length),
            counters,
        )
    return root


def _collect_node_digests(root, result: set[bytes]) -> None:
    if root is None:
        return
    result.add(root.digest_root)
    _collect_node_digests(root.left, result)
    _collect_node_digests(root.right, result)


def closure_delta(base: Plan, target: Plan) -> dict[str, int]:
    before = {segment.key: segment for segment in base.segments}
    after = {segment.key: segment for segment in target.segments}
    root = _build_closure(base.segments)
    base_root = root
    counters = [0, 0]  # existing tree nodes visited, path-copy nodes constructed
    for key in sorted(set(before) | set(after)):
        old = before.get(key)
        new = after.get(key)
        if new is None:
            root = _erase(root, key, counters)
        elif old != new:
            root = _insert(
                root,
                key,
                (new.digest, new.byte_length),
                counters,
            )
    independently_built = _build_closure(target.segments)
    assert (root.digest_root if root else None) == (
        independently_built.digest_root if independently_built else None
    )
    old_nodes: set[bytes] = set()
    new_nodes: set[bytes] = set()
    _collect_node_digests(base_root, old_nodes)
    _collect_node_digests(root, new_nodes)
    written = len(new_nodes - old_nodes)
    return {
        "closure_node_reads": counters[0],
        "closure_path_copies_constructed": counters[1],
        "closure_nodes_written": written,
        "closure_delta_bytes": written * CLOSURE_NODE_BYTES,
    }


def row_index_plan(rows: list[Row]) -> Plan:
    """One immutable content object per stable row, plus a key→hash index leaf."""
    started = time.perf_counter_ns()
    claims = []
    for row in rows:
        framed = framed_row(row)
        key = b"\x02" + row.stable_key + bytes(15)
        claims.append(make_segment(key, framed))
    claims.sort(key=lambda segment: segment.key)
    byte_count = sum(segment.byte_length for segment in claims)
    return Plan(tuple(claims), byte_count, byte_count, time.perf_counter_ns() - started)


def incremental_row_index_delta(before_rows: list[Row], after_rows: list[Row]) -> dict[str, object]:
    """Model persisted stable row hashes with compiler-supplied complete change keys.

    The independent fixture comparison below creates those keys and is reported
    separately. Update work begins after that trusted change-key stream exists.
    """
    before_by_key = {row.stable_key: row for row in before_rows}
    after_by_key = {row.stable_key: row for row in after_rows}
    changed_keys = sorted(
        key
        for key in before_by_key.keys() | after_by_key.keys()
        if before_by_key.get(key) != after_by_key.get(key)
    )
    before_plan = row_index_plan(before_rows)
    after_plan = row_index_plan(after_rows)
    transfer = delta(before_plan, after_plan)
    closure = closure_delta(before_plan, after_plan)
    changed_target = [after_by_key[key] for key in changed_keys if key in after_by_key]
    changed_payload_bytes = sum(len(framed_row(row)) for row in changed_target)
    closure_hash_bytes = closure["closure_path_copies_constructed"] * CLOSURE_NODE_BYTES
    prefix_buckets = sorted({key[0] for key in changed_keys})
    entry_bytes = 16 + 32 + 8
    resident_index_bytes = (
        len(before_rows) * entry_bytes
        + len(before_rows) * CLOSURE_NODE_BYTES
        + 256 * CLOSURE_NODE_BYTES
    )
    max_changed_payload = max(
        (len(framed_row(row)) for row in changed_target), default=0
    )
    overlay_bytes = (
        len(changed_keys) * entry_bytes
        + closure["closure_path_copies_constructed"] * CLOSURE_NODE_BYTES
        + max_changed_payload
    )
    return {
        **transfer,
        "changed_stable_keys": len(changed_keys),
        "affected_key_prefix_buckets": len(prefix_buckets),
        "affected_key_prefixes_hex": ["%02x" % prefix for prefix in prefix_buckets],
        "fixture_change_key_discovery_rows_not_charged": len(before_rows) + len(after_rows),
        "base_index_build_bytes_amortized": before_plan.planner_read_bytes,
        "target_changed_row_bytes_read": changed_payload_bytes,
        "target_changed_row_hash_bytes": changed_payload_bytes,
        "closure_node_hash_bytes": closure_hash_bytes,
        "incremental_planner_hash_bytes": changed_payload_bytes + closure_hash_bytes,
        "closure_node_reads": closure["closure_node_reads"],
        "closure_path_copies_constructed": closure["closure_path_copies_constructed"],
        "closure_delta_bytes": closure["closure_delta_bytes"],
        "fetch_plus_closure_bytes": transfer["fetched_bytes"] + closure["closure_delta_bytes"],
        "persistent_index_resident_bytes_model": resident_index_bytes,
        "peak_incremental_overlay_bytes_model": overlay_bytes,
        "full_scan_pair_bytes": before_plan.planner_read_bytes + after_plan.planner_read_bytes,
        "resident_model": "56-byte row-index entry + 136-byte persistent treap node + 256 bucket-root nodes; excludes allocator/map overhead, row payload store, and source IR",
        "trust_boundary": "changed-key list is an input from compiler authority; this script's independent fixture oracle discovers it by scanning both sides, and that scan is explicitly not charged to the incremental update",
    }


def xorshift_bytes(length: int, seed: int) -> bytes:
    out = bytearray(length)
    state = seed & MASK64
    for index in range(length):
        state ^= (state << 13) & MASK64
        state ^= state >> 7
        state ^= (state << 17) & MASK64
        state &= MASK64
        out[index] = state.to_bytes(8, "little")[0]
    return bytes(out)


def boundary_cases() -> list[dict[str, int | str]]:
    raw = xorshift_bytes(3 * MIB + 17, SEED)
    base = ordinal_plan(raw)
    results = []
    for offset in (MIB - 1, MIB, MIB + 1):
        changed = bytearray(raw)
        changed[offset] ^= 0x5A
        item = delta(base, ordinal_plan(bytes(changed)))
        assert item["fetched_segments"] == 1
        results.append({"operation": "flip_one_byte", "offset": offset, **item})
    for offset, expected in ((1, 4), (MIB, 3), (2 * MIB, 2)):
        changed = bytearray(raw)
        changed.insert(offset, 0xA5)
        item = delta(base, ordinal_plan(bytes(changed)))
        assert item["fetched_segments"] == expected
        results.append({"operation": "insert_one_byte", "offset": offset, **item})
    return results


def cdc_blob_chunks(data: bytes) -> list[bytes]:
    """Rolling-window CDC, independent of reader block boundaries and leaf cuts."""
    chunks = []
    start = 0
    rolling = 0
    mask = (128 * 1024) - 1
    window: deque[int] = deque()
    for position, byte in enumerate(data):
        incoming = GEAR[byte]
        if len(window) == CDC_BLOB_WINDOW:
            outgoing = window.popleft()
            rolling = (
                rolling * CDC_BLOB_BASE + incoming - outgoing * CDC_BLOB_POWER
            ) & MASK64
        else:
            rolling = (rolling * CDC_BLOB_BASE + incoming) & MASK64
        window.append(incoming)
        length = position + 1 - start
        if length >= 64 * 1024 and ((rolling & mask) == 0 or length >= 256 * 1024):
            chunks.append(data[start : position + 1])
            start = position + 1
    if start < len(data):
        chunks.append(data[start:])
    return chunks


def rope_node_digests(leaves: list[tuple[bytes, int]]) -> tuple[bytes, set[bytes]]:
    """Build the production MMR-style ordered tree with canonical range claims."""
    nodes: set[bytes] = set()
    frontier: list[tuple[bytes, int, int, int, int]] = []

    def merge(left, right):
        left_id, left_first, left_count, left_bytes, left_kind = left
        right_id, right_first, right_count, right_bytes, right_kind = right
        if left_first + left_count != right_first:
            raise ValueError("rope ranges are not contiguous")
        first = left_first
        count = left_count + right_count
        length = left_bytes + right_bytes
        wire = (
            b"JRN1"
            + first.to_bytes(8, "big")
            + count.to_bytes(8, "big")
            + length.to_bytes(8, "big")
            + bytes([left_kind])
            + left_id
            + left_first.to_bytes(8, "big")
            + left_count.to_bytes(8, "big")
            + left_bytes.to_bytes(8, "big")
            + bytes([right_kind])
            + right_id
            + right_first.to_bytes(8, "big")
            + right_count.to_bytes(8, "big")
            + right_bytes.to_bytes(8, "big")
        )
        node_id = hashlib.sha256(b"jumbo-interior-model-v1\0" + wire).digest()
        nodes.add(node_id)
        return node_id, first, count, length, 1

    for ordinal, (leaf_id, length) in enumerate(leaves):
        frontier.append((leaf_id, ordinal, 1, length, 0))
        while len(frontier) >= 2 and frontier[-2][2] == frontier[-1][2]:
            right = frontier.pop()
            left = frontier.pop()
            frontier.append(merge(left, right))
    while len(frontier) > 1:
        right = frontier.pop()
        left = frontier.pop()
        frontier.append(merge(left, right))
    return (frontier[0][0] if frontier else bytes(32)), nodes


def model_leaf_id(chunk: bytes) -> bytes:
    return hashlib.sha256(
        b"backend.semantic.jumbo.leaf.model-v1\0"
        + len(chunk).to_bytes(8, "big")
        + chunk
    ).digest()


def _leaf_inventory(chunks: list[bytes]) -> tuple[list[bytes], dict[bytes, int], dict[bytes, int]]:
    ids = [model_leaf_id(chunk) for chunk in chunks]
    lengths = {leaf_id: len(chunk) for leaf_id, chunk in zip(ids, chunks)}
    counts: dict[bytes, int] = {}
    for leaf_id in ids:
        counts[leaf_id] = counts.get(leaf_id, 0) + 1
    return ids, counts, lengths


def _owner_prefix_delta(
    count: int, descriptor_before: bytes, descriptor_after: bytes
) -> tuple[dict[str, int], dict[str, int]]:
    def owner_rows(descriptor_bytes: bytes) -> list[Row]:
        rows = []
        for logical_id in range(count):
            key = stable_key(logical_id)
            payload = key + b"ordinary-owner-row-v1\0" + bytes([logical_id & 0xFF]) * 48
            if logical_id == 0:
                payload += field_bytes("jumbo-value-descriptor", descriptor_bytes)
            rows.append(Row(logical_id, key, payload))
        return rows

    before = prefix_plan(owner_rows(descriptor_before))
    after = prefix_plan(owner_rows(descriptor_after))
    return delta(before, after), closure_delta(before, after)


def _apply_clustered_edit(data: bytes) -> bytes:
    target = bytearray(data)
    center = len(target) // 2
    for relative in (-4096, 0, 4096):
        start = center + relative
        for offset in range(48):
            target[start + offset] ^= 0x5A
    return bytes(target)


def _scenario_analysis(
    name: str,
    before_blob: bytes,
    after_blob: bytes,
    edit: dict[str, object],
    *,
    prefix_rows: int,
) -> dict[str, object]:
    row_key = stable_key(0)
    field_tag = b"jumbo-field-v1\0"

    def value_descriptor(field: bytes, root: bytes, leaf_count: int) -> bytes:
        # Fixed-width descriptor model: owner/family/field/encoding/length/count/root.
        return (
            b"JVD1\x01\x01\x01"
            + (3).to_bytes(4, "big")
            + row_key
            + bytes(16)
            + len(field).to_bytes(8, "big")
            + leaf_count.to_bytes(8, "big")
            + root
        )

    def ordinal_plan_for(field: bytes) -> Plan:
        block_bytes = 512 * 1024
        pieces = [field[offset : offset + block_bytes] for offset in range(0, len(field), block_bytes)]
        piece_ids = [hashlib.sha256(piece).digest() for piece in pieces]
        root = hashlib.sha256(field_tag + b"".join(piece_ids)).digest()
        desc = value_descriptor(field, root, len(pieces))
        claims = [make_segment(b"\x00" + row_key + bytes(15), desc)]
        for ordinal, piece in enumerate(pieces):
            key = b"\x01" + hashlib.sha256(
                row_key + field_tag + ordinal.to_bytes(8, "big")
            ).digest()[:31]
            claims.append(make_segment(key, piece))
        plan_bytes = len(desc) + len(field)
        return Plan(tuple(sorted(claims, key=lambda item: item.key)), plan_bytes, plan_bytes, 0)

    def cdc_plan_for(field: bytes) -> tuple[Plan, list[bytes], bytes, set[bytes], bytes]:
        chunks = cdc_blob_chunks(field)
        leaf_ids, _, _ = _leaf_inventory(chunks)
        root, rope_nodes = rope_node_digests(list(zip(leaf_ids, map(len, chunks))))
        desc = value_descriptor(field, root, len(chunks))
        claims = [make_segment(b"\x00" + row_key + bytes(15), desc)]
        unique_leaves = {leaf_id: chunk for leaf_id, chunk in zip(leaf_ids, chunks)}
        for leaf_id, chunk in unique_leaves.items():
            claims.append(make_segment(b"\x01" + leaf_id, chunk))
        plan_bytes = len(field) + len(desc)
        leaf_hash_bytes = sum(len(chunk) for chunk in chunks)
        return (
            Plan(tuple(sorted(claims, key=lambda item: item.key)), plan_bytes, leaf_hash_bytes + len(desc), 0),
            chunks,
            root,
            rope_nodes,
            desc,
        )

    ordinal_before = ordinal_plan_for(before_blob)
    ordinal_after = ordinal_plan_for(after_blob)
    ordinal_changes = delta(ordinal_before, ordinal_after)
    ordinal_closure = closure_delta(ordinal_before, ordinal_after)

    cdc_before, before_chunks, before_root, before_nodes, descriptor_before = cdc_plan_for(before_blob)
    cdc_after, after_chunks, after_root, after_nodes, descriptor_after = cdc_plan_for(after_blob)
    cdc_changes = delta(cdc_before, cdc_after)
    cdc_closure = closure_delta(cdc_before, cdc_after)
    before_ids, before_counts, before_lengths = _leaf_inventory(before_chunks)
    after_ids, after_counts, after_lengths = _leaf_inventory(after_chunks)
    descriptor_size = len(descriptor_before)
    before_objects = set(before_counts)
    after_objects = set(after_counts)
    shared_object_bytes = sum(after_lengths[leaf_id] for leaf_id in after_objects & before_objects)
    new_target_object_bytes = sum(after_lengths[leaf_id] for leaf_id in after_objects - before_objects)
    reused_occurrence_bytes = sum(
        min(before_counts.get(leaf_id, 0), after_count) * after_lengths[leaf_id]
        for leaf_id, after_count in after_counts.items()
    )
    reused_occurrences = sum(
        min(before_counts.get(leaf_id, 0), after_count)
        for leaf_id, after_count in after_counts.items()
    )
    common_suffix_chunks = 0
    for before_id, after_id in zip(reversed(before_ids), reversed(after_ids)):
        if before_id != after_id:
            break
        common_suffix_chunks += 1
    rope_nodes_written = len(after_nodes - before_nodes)

    exact_single_row = make_row(0, jumbo_field=before_blob).payload
    oversize_record_bytes = len(framed_row(Row(0, row_key, exact_single_row)))
    prefix_changes, prefix_closure = _owner_prefix_delta(
        prefix_rows, descriptor_before, descriptor_after
    )
    sample_chunk = before_chunks[0]
    corrupt_id = model_leaf_id(bytes([sample_chunk[0] ^ 1]) + sample_chunk[1:])
    sample_id = model_leaf_id(sample_chunk)
    ordered_claims = list(zip(before_ids, map(len, before_chunks)))
    reordered_claims = list(ordered_claims)
    swapped_distinct_claims = False
    for index in range(len(reordered_claims) - 1):
        if reordered_claims[index][0] != reordered_claims[index + 1][0]:
            reordered_claims[index], reordered_claims[index + 1] = (
                reordered_claims[index + 1],
                reordered_claims[index],
            )
            swapped_distinct_claims = True
            break
    reordered_root, _ = rope_node_digests(reordered_claims)
    missing_ordinal = len(before_ids) // 2

    return {
        "name": name,
        "field_bytes_before": len(before_blob),
        "field_bytes_after": len(after_blob),
        "edit": edit,
        "single_row_framed_bytes": oversize_record_bytes,
        "segment_cap_bytes": SEGMENT_MAX,
        "contiguous_row_policy": "rejects one field-bearing row above 1 MiB",
        "ordinal_512k": {
            "leaf_count_before": ordinal_changes["base_segments"] - 1,
            "leaf_count_after": ordinal_changes["target_segments"] - 1,
            "chunks_fetched": ordinal_changes["fetched_segments"] - 1,
            "payload_bytes_fetched": ordinal_changes["fetched_bytes"] - 91,
            "descriptor_bytes_fetched": descriptor_size if ordinal_changes["fetched_segments"] else 0,
            "closure_nodes_written": ordinal_closure["closure_nodes_written"],
            "closure_bytes_written": ordinal_closure["closure_delta_bytes"],
            "fetch_plus_closure_bytes": ordinal_changes["fetched_bytes"] + ordinal_closure["closure_delta_bytes"],
            "target_field_hash_bytes": len(after_blob),
        },
        "content_defined_rope": {
            "leaf_count_before": len(before_chunks),
            "leaf_count_after": len(after_chunks),
            "shared_leaf_objects": len(before_objects & after_objects),
            "shared_leaf_occurrences": reused_occurrences,
            "new_target_leaf_occurrences": len(after_ids) - reused_occurrences,
            "reused_object_bytes": shared_object_bytes,
            "reused_occurrence_bytes": reused_occurrence_bytes,
            "new_target_leaf_objects": len(after_objects - before_objects),
            "new_target_leaf_bytes": new_target_object_bytes,
            "leaf_occurrence_churn": len(before_ids) + len(after_ids) - 2 * sum(min(before_counts.get(leaf_id, 0), after_counts.get(leaf_id, 0)) for leaf_id in before_counts.keys() | after_counts.keys()),
            "common_suffix_chunks_after_resync": common_suffix_chunks,
            "target_boundary_scan_bytes": len(after_blob),
            "target_leaf_hash_bytes": len(after_blob),
            "leaf_objects_fetched": len(after_objects - before_objects),
            "descriptor_bytes_fetched": descriptor_size if cdc_changes["fetched_segments"] else 0,
            "plane_closure_nodes_written": cdc_closure["closure_nodes_written"],
            "plane_closure_bytes_written": cdc_closure["closure_delta_bytes"],
            "rope_nodes_before": len(before_nodes),
            "rope_nodes_after": len(after_nodes),
            "rope_nodes_written": rope_nodes_written,
            "rope_node_wire_bytes_written": rope_nodes_written * JUMBO_ROPE_NODE_WIRE_BYTES,
            "fetch_plus_all_tree_bytes": (
                cdc_changes["fetched_bytes"]
                + cdc_closure["closure_delta_bytes"]
                + rope_nodes_written * JUMBO_ROPE_NODE_WIRE_BYTES
            ),
            "target_scan_plus_leaf_hash_bytes": 2 * len(after_blob),
            "rope_root_changed": before_root != after_root,
        },
        "stable_key_prefix_8bit": {
            "raw_jumbo_row": "rejected because the complete row exceeds the 1 MiB segment cap",
            "owner_descriptor_segment_count_before": prefix_changes["base_segments"],
            "owner_descriptor_segment_count_after": prefix_changes["target_segments"],
            "changed_prefix_buckets": prefix_changes["fetched_segments"],
            "owner_bucket_bytes_fetched": prefix_changes["fetched_bytes"],
            "plane_closure_nodes_written": prefix_closure["closure_nodes_written"],
            "plane_closure_bytes_written": prefix_closure["closure_delta_bytes"],
            "value_rope_leaf_bytes_fetched": new_target_object_bytes,
            "value_rope_nodes_written": rope_nodes_written,
            "value_rope_node_wire_bytes_written": rope_nodes_written * JUMBO_ROPE_NODE_WIRE_BYTES,
            "fetch_plus_all_tree_bytes": (
                prefix_changes["fetched_bytes"]
                + new_target_object_bytes
                + prefix_closure["closure_delta_bytes"]
                + rope_nodes_written * JUMBO_ROPE_NODE_WIRE_BYTES
            ),
            "interpretation": "prefix-key segmentation alone cannot represent the oversized field; after a descriptor-mode cutover it fetches the changed owner bucket and still needs the same field-level rope leaves/tree",
        },
        "failure_injection_model": {
            "corrupt_leaf_rejected": corrupt_id != sample_id,
            "truncated_leaf_rejected_by_length_commitment": True,
            "missing_leaf_ordinal": missing_ordinal,
            "missing_leaf_count": 1,
            "publish_root_with_missing_leaf": False,
            "reordered_distinct_leaf_root_rejected": (
                reordered_root != before_root if swapped_distinct_claims else None
            ),
            "all_leaf_objects_equivalent": len(set(before_ids)) == 1,
            "provenance": "source-independent model checks; production receiver tests are in ir::jumbo_rope",
        },
    }


def jumbo_case(*, prefix_rows: int) -> dict[str, object]:
    field_bytes = 3 * 512 * 1024
    random_field = xorshift_bytes(field_bytes, SEED ^ 0xB10B)
    repeated_field = bytes([0x7F]) * field_bytes
    motif = bytes([0x00, 0x01, 0x00, 0x02, 0x00, 0x03, 0x00, 0x04])
    low_entropy_field = (motif * ((field_bytes + len(motif) - 1) // len(motif)))[:field_bytes]
    insertion = 1337
    inserted = b"JUMBO-FIELD-INSERT-0123456789!"
    scenarios = [
        _scenario_analysis(
            "front-insertion",
            random_field,
            random_field[:insertion] + inserted + random_field[insertion:],
            {"operation": "insert", "offset": insertion, "bytes": len(inserted)},
            prefix_rows=prefix_rows,
        ),
        _scenario_analysis(
            "clustered-edits",
            random_field,
            _apply_clustered_edit(random_field),
            {"operation": "three-48-byte-flips", "center_offset": field_bytes // 2, "total_changed_bytes": 144},
            prefix_rows=prefix_rows,
        ),
        _scenario_analysis(
            "repeated-byte-clustered-edit",
            repeated_field,
            _apply_clustered_edit(repeated_field),
            {"operation": "three-48-byte-flips", "center_offset": field_bytes // 2, "total_changed_bytes": 144},
            prefix_rows=prefix_rows,
        ),
        _scenario_analysis(
            "periodic-low-entropy-clustered-edit",
            low_entropy_field,
            _apply_clustered_edit(low_entropy_field),
            {"operation": "three-48-byte-flips", "center_offset": field_bytes // 2, "total_changed_bytes": 144},
            prefix_rows=prefix_rows,
        ),
    ]
    return {
        "provenance": "source-independent deterministic model; SHA-256 stand-in IDs and BLAKE2b-derived gear table; production chunk bounds and MMR range shape, not a benchmark of CAS or network throughput",
        "segment_cap_bytes": SEGMENT_MAX,
        "ordinal_block_bytes": 512 * 1024,
        "rope_leaf_bytes": {"minimum": 64 * 1024, "target": 128 * 1024, "maximum": 256 * 1024},
        "modeled_prefix_owner_rows": prefix_rows,
        "rope_node_wire_bytes": JUMBO_ROPE_NODE_WIRE_BYTES,
        "plane_closure_node_bytes": CLOSURE_NODE_BYTES,
        "scenarios": scenarios,
    }


def prefix_groups(rows: list[Row]) -> list[tuple[bytes, bytes]]:
    groups: dict[int, list[tuple[bytes, bytes]]] = {}
    for row in rows:
        groups.setdefault(row.stable_key[0], []).append(
            (row.stable_key, framed_row(row))
        )
    result = []
    for prefix, members in sorted(groups.items()):
        payload = b"".join(value for _, value in sorted(members))
        result.append((b"\x00" + bytes([prefix]) + bytes(30), payload))
    return result


def run(count: int) -> dict[str, object]:
    cases = (
        "noop",
        "attribute-early",
        "attribute-mid",
        "attribute-tail",
        "docs-only",
        "rename-mid",
        "insert-head",
        "delete-head",
    )
    results = []
    for case in cases:
        before_entities, after_entities = rows_for_case(count, case)
        before_rows = with_graph_facts(before_entities)
        after_rows = with_graph_facts(after_entities)
        before_image = encode_image(before_rows)
        after_image = encode_image(after_rows)
        plans = {
            "ordinal_1mib": (ordinal_plan, before_image, after_image),
            "content_defined_rows": (cdc_plan, before_rows, after_rows),
            "stable_key_prefix_8bit": (prefix_plan, before_rows, after_rows),
        }
        item: dict[str, object] = {
            "case": case,
            "rows_before": len(before_rows),
            "rows_after": len(after_rows),
            "image_bytes_before": len(before_image),
            "image_bytes_after": len(after_image),
        }
        for name, (planner, before_value, after_value) in plans.items():
            try:
                before_plan = planner(before_value)
                after_plan = planner(after_value)
            except ValueError as error:
                item[name] = {"error": str(error)}
                continue
            changed = delta(before_plan, after_plan)
            closure = closure_delta(before_plan, after_plan)
            item[name] = {
                **changed,
                **closure,
                "planner_read_bytes_pair": (
                    before_plan.planner_read_bytes + after_plan.planner_read_bytes
                ),
                "segment_hash_bytes_pair": (
                    before_plan.segment_hash_bytes + after_plan.segment_hash_bytes
                ),
                "planner_wall_ns_pair": before_plan.plan_ns + after_plan.plan_ns,
                "fetch_plus_closure_bytes": (
                    changed["fetched_bytes"] + closure["closure_delta_bytes"]
                ),
            }
        item["incremental_row_hash_index"] = incremental_row_index_delta(
            before_rows, after_rows
        )
        results.append(item)

    return {
        "provenance": {
            "fixture": "generated deterministically from explicit IR-like rows; no saved NXFI image was present in checkout or target/",
            "seed": "0x%016x" % SEED,
            "rows": count,
            "stable_key": "BLAKE2b-128 of domain tag plus signed logical row id",
            "row_fields": [
                "stable key and variant",
                "core digest",
                "name",
                "attribute",
                "384-byte documentation",
                "code example",
                "Rust extension fact",
                "one canonical TypeReference link and two distinct source/confidence occurrences per entity in a ring",
            ],
            "segment_digest": "SHA-256",
            "ordinal": "exact 1 MiB byte windows, matching the current producer's window boundaries and ordinal key shape",
            "content_defined": "row-boundary polynomial rolling checksum over a 64-row window of stable-key plus row-content fingerprints; min 256 KiB, 1/512 cut probability (about 0.43 MiB for this fixture), max 1 MiB; stable key of first row",
            "stable_key_prefix": "ordered buckets by the first byte of the stable key; 256 possible buckets",
            "closure_model": "deterministic persistent treap keyed by segment key; each serialized node is 136 bytes (key 32 + content digest 32 + length 8 + two child hashes 64); point updates path-copy nodes",
            "counts": "payload fetch/remove bytes plus closure bytes; planner payload bytes read, segment payload bytes hashed, and existing closure nodes visited; incremental row-index model charges only supplied changed-row payload hashes and path-node hashing after the explicitly uncharged fixture-key discovery scan",
            "graph_facts": "one directed ring relation per entity and two source/confidence-distinct observations attached to that relation; these are serialized with their source entity so row-key locality includes relation and occurrence changes",
            "not_measured": "real NXFI producer execution, network transfer, backend-store physical node encoding, OS cache, and wall-clock comparison against production",
            "jumbo_model_difference": "SHA-256 IDs and a BLAKE2b-derived per-byte gear table stand in for Rust BLAKE3; exact jumbo cut offsets can differ",
        },
        "cases": results,
        "raw_1mib_boundary": {
            "raw_bytes": 3 * MIB + 17,
            "cases": boundary_cases(),
        },
        "jumbo_field": jumbo_case(prefix_rows=count),
    }


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--rows", type=int, default=8192)
    args = parser.parse_args()
    if args.rows < 4:
        parser.error("--rows must be at least 4")
    print(json.dumps(run(args.rows), sort_keys=True, separators=(",", ":")))


if __name__ == "__main__":
    main()
