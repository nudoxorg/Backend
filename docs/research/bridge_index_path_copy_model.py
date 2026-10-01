#!/usr/bin/env python3
"""Source-only cost model for a typed history bridge index.

This mirrors the byte widths and path-copy update *shape* of
backend_version::LazyTree without linking or building the Rust workspace.
The node framing, row widths, cut parameters, and one-key-at-a-time update
behavior are copied from the inspected source. SHA-256 stands in for BLAKE3
for both content IDs and content-defined cuts, so node partition counts are
illustrative, not canonical Rust results.

Run with Python 3 and only the standard library. This is not a benchmark of
compiler time, cold verification, allocator use, or FileStore envelope I/O.
"""

from __future__ import annotations

import bisect
import hashlib
import json
import random
from functools import lru_cache


N = 65_536
EDIT_COUNTS = (1, 10, 100)

# From crates/version/src/tree/cut.rs and tree/mod.rs.
NODE_MAGIC = b"V2NODE\0"
CUT_DOMAIN = b"backend.version.cut\0"
TREE_ABI = 1
CUT_POLICY_VERSION = 2
MIN_ENTRIES = 64
TARGET_ENTRIES = 256
MAX_ENTRIES = 1_024
MAX_NODE_BYTES = 64 * 1_024
MIN_BODY_BYTES = MAX_NODE_BYTES // 4
TARGET_BODY_BYTES = MAX_NODE_BYTES // 2
NODE_OVERHEAD_BYTES = 24  # 16-byte node prefix + 8-byte body length.
U64_MAX = (1 << 64) - 1

# Placeholder schema for the proposed bridge relation. Schema bytes affect
# anchors/root IDs but not the encoded byte counts. A production schema must
# allocate its own closed domain/type/version and be included in root claims.
RELATION_DOMAIN = 0x73
RELATION_TYPE = 0x7F01
RELATION_VERSION = 1

KEY_BYTES = 32
PHYSICAL_OBJECT_ID_BYTES = 32
BYTE_LENGTH_BYTES = 8
VALUE_BYTES = PHYSICAL_OBJECT_ID_BYTES + BYTE_LENGTH_BYTES  # 40
LEAF_ENTRY_BYTES = 8 + KEY_BYTES + 8 + VALUE_BYTES  # 88
BRANCH_ENTRY_BYTES = 8 + KEY_BYTES + 40 + 8  # key + framed 32-byte root + row count

# Minimal, valid-size STPV v2 c005 manifest framing, from
# crates/semantic/src/ir/typed_plane_manifest_v2.rs. All 65,536 segment
# descriptors are assigned to Core; six other families are empty, including
# the three-byte LanguageExtensions family tag. Image facts are Shared plus
# Unavailable provenance (two bytes).
C005_FIXED_BYTES = (
    6  # magic + u16 wire version
    + (32 * 6 + 2 + 1)  # SemanticBuildIdentity
    + 2  # shared authority + unavailable provenance
    + (32 + 32 + 1)  # input/read claim
    + (32 + 32 + 1)  # content root + generation root + family count
    + (6 * (1 + 8 + 4) + (3 + 8 + 4))  # seven family headers
)
C005_SEGMENT_DESCRIPTOR_BYTES = 32 + 32 + 4 + 8 + 32  # 108 bytes
C005_BYTES = C005_FIXED_BYTES + N * C005_SEGMENT_DESCRIPTOR_BYTES

# Current typed V2 history locator body: 6-byte header, u64-sized c005 bytes,
# u32 segment count, 72 bytes/segment, u32 jumbo count. The persisted sidecar
# adds a 32-byte identity and 32-byte checksum around the body.
LOCATOR_FIXED_BODY_BYTES = 6 + 8 + 4 + 4
LOCATOR_ENVELOPE_BYTES = 32 + 32
FLAT_SEGMENT_BRIDGE_BYTES = 32 + 32 + 8  # semantic ID, FileStore ID, length

# Candidate descriptor retains the c005 bytes inline for the conservative
# comparison and substitutes a (root ID, exact count) pair for the flat list.
TREE_ROOT_DESCRIPTOR_BYTES = 32 + 8


def u64(value: int) -> bytes:
    return value.to_bytes(8, "big")


@lru_cache(maxsize=None)
def anchor_word(key: bytes, level: int) -> int:
    """BLAKE3 source framing with SHA-256 as a deterministic stand-in."""
    framing = (
        CUT_DOMAIN
        + bytes((TREE_ABI, CUT_POLICY_VERSION, RELATION_DOMAIN))
        + RELATION_TYPE.to_bytes(2, "big")
        + bytes((RELATION_VERSION,))
        + level.to_bytes(2, "big")
        + MIN_ENTRIES.to_bytes(2, "big")
        + TARGET_ENTRIES.to_bytes(2, "big")
        + MAX_ENTRIES.to_bytes(2, "big")
        + u64(len(key))
        + key
    )
    return int.from_bytes(hashlib.sha256(framing).digest()[:8], "big")


def partition(items: list, level: int, entry_bytes: int) -> list[list]:
    """Mirror anchored_cut_points_sized for fixed-width modeled entries."""
    output: list[list] = []
    start = 0
    encoded_bytes = NODE_OVERHEAD_BYTES
    for index, item in enumerate(items):
        key = item[0] if isinstance(item, tuple) else item.first_key
        if encoded_bytes + entry_bytes > MAX_NODE_BYTES:
            if index == start:
                raise ValueError("modeled entry cannot fit in a canonical node")
            output.append(items[start:index])
            start = index
            encoded_bytes = NODE_OVERHEAD_BYTES
        encoded_bytes += entry_bytes
        count = index - start + 1
        body_bytes = encoded_bytes - NODE_OVERHEAD_BYTES
        if count < MIN_ENTRIES and body_bytes < MIN_BODY_BYTES:
            continue
        word = anchor_word(key, level)
        count_anchor = count >= MIN_ENTRIES and word <= U64_MAX // TARGET_ENTRIES
        byte_anchor = False
        if body_bytes >= MIN_BODY_BYTES:
            weight = min(entry_bytes, TARGET_BODY_BYTES)
            byte_anchor = word <= (U64_MAX // TARGET_BODY_BYTES) * weight
        if count >= MAX_ENTRIES or count_anchor or byte_anchor:
            output.append(items[start : index + 1])
            start = index + 1
            encoded_bytes = NODE_OVERHEAD_BYTES
    if start < len(items):
        output.append(items[start:])
    return output


def node_prefix(kind: int, level: int) -> bytes:
    return (
        NODE_MAGIC
        + bytes((TREE_ABI, CUT_POLICY_VERSION, kind, RELATION_VERSION, RELATION_DOMAIN))
        + RELATION_TYPE.to_bytes(2, "big")
        + level.to_bytes(2, "big")
    )


class Node:
    __slots__ = ("first_key", "row_count", "commitment", "encoded_bytes")

    def __init__(self, first_key: bytes, row_count: int, commitment: bytes, encoded_bytes: int):
        self.first_key = first_key
        self.row_count = row_count
        self.commitment = commitment
        self.encoded_bytes = encoded_bytes


def leaf_node(entries: list[tuple[bytes, bytes]]) -> Node:
    body_bytes = len(entries) * LEAF_ENTRY_BYTES
    digest = hashlib.sha256()
    digest.update(node_prefix(1, 0))
    digest.update(u64(body_bytes))
    for key, value in entries:
        digest.update(u64(len(key)))
        digest.update(key)
        digest.update(u64(len(value)))
        digest.update(value)
    return Node(entries[0][0], len(entries), digest.digest(), NODE_OVERHEAD_BYTES + body_bytes)


def branch_node(level: int, children: list[Node]) -> Node:
    body_bytes = len(children) * BRANCH_ENTRY_BYTES
    digest = hashlib.sha256()
    digest.update(node_prefix(2, level))
    digest.update(u64(body_bytes))
    row_count = 0
    for child in children:
        digest.update(u64(len(child.first_key)))
        digest.update(child.first_key)
        digest.update(u64(len(child.commitment)))
        digest.update(child.commitment)
        digest.update(u64(child.row_count))
        row_count += child.row_count
    return Node(children[0].first_key, row_count, digest.digest(), NODE_OVERHEAD_BYTES + body_bytes)


def canonical_tree(rows: list[tuple[bytes, bytes]]) -> dict[bytes, int]:
    """Build node commitments and encoded lengths for one full canonical root."""
    if not rows:
        raise ValueError("model expects a non-empty map")
    nodes: dict[bytes, int] = {}
    level_nodes = [leaf_node(group) for group in partition(rows, 0, LEAF_ENTRY_BYTES)]
    for node in level_nodes:
        nodes[node.commitment] = node.encoded_bytes
    level = 1
    while len(level_nodes) > 1:
        groups = partition(level_nodes, level, BRANCH_ENTRY_BYTES)
        parents = [branch_node(level, group) for group in groups]
        if len(parents) >= len(level_nodes):
            raise ValueError("modeled branch cuts did not reduce the tree")
        for node in parents:
            nodes[node.commitment] = node.encoded_bytes
        level_nodes = parents
        level += 1
    return nodes


def segment_key(label: str, index: int) -> bytes:
    return hashlib.sha256(f"{label}:{index}".encode()).digest()


def bridge_value(label: str, index: int) -> bytes:
    object_id = hashlib.sha256(f"physical:{label}:{index}".encode()).digest()
    # Segment length is fixed for this model; the wire field is an exact u64.
    return object_id + u64(32_768)


def simulate(edits: int) -> dict[str, int]:
    base = sorted((segment_key("base", i), bridge_value("base", i)) for i in range(N))
    rng = random.Random(0xB71D_0000 + edits)
    removed = rng.sample(range(N), edits)
    operations: list[tuple[bytes, bytes | None]] = []
    for i, index in enumerate(removed):
        operations.append((base[index][0], None))
        operations.append((segment_key("new", edits * 1000 + i), bridge_value("new", edits * 1000 + i)))
    operations.sort(key=lambda operation: operation[0])

    previous_nodes = canonical_tree(base)
    current = base.copy()
    touched_bytes = 0
    touched_nodes = 0
    per_edit_bytes = []
    for key, after in operations:
        position = bisect.bisect_left(current, (key, b""))
        present = position < len(current) and current[position][0] == key
        if after is None:
            if not present:
                raise AssertionError("remove key was not present")
            del current[position]
        else:
            if present:
                raise AssertionError("insert key already existed")
            current.insert(position, (key, after))
        next_nodes = canonical_tree(current)
        new_node_ids = next_nodes.keys() - previous_nodes.keys()
        step_bytes = sum(next_nodes[node_id] for node_id in new_node_ids)
        step_nodes = len(new_node_ids)
        touched_bytes += step_bytes
        touched_nodes += step_nodes
        per_edit_bytes.append((step_bytes, step_nodes))
        previous_nodes = next_nodes

    return {
        "logical_replacements": edits,
        "single_key_changes": 2 * edits,
        "path_copy_nodes_touched": touched_nodes,
        "path_copy_canonical_bytes_touched": touched_bytes,
        "mean_bytes_per_logical_replacement": round(touched_bytes / edits),
        "mean_nodes_per_logical_replacement": round(touched_nodes / edits, 2),
        "flat_bridge_bytes_per_commit": N * FLAT_SEGMENT_BRIDGE_BYTES,
        "c005_canonical_bytes_per_commit": C005_BYTES,
        "current_c005_plus_locator_bytes": (
            C005_BYTES
            + LOCATOR_FIXED_BODY_BYTES
            + C005_BYTES
            + N * FLAT_SEGMENT_BRIDGE_BYTES
            + LOCATOR_ENVELOPE_BYTES
        ),
        "path_copy_locator_bytes_if_manifest_remains_inline": (
            C005_BYTES
            + LOCATOR_FIXED_BODY_BYTES
            + C005_BYTES
            + TREE_ROOT_DESCRIPTOR_BYTES
            + touched_bytes
            + LOCATOR_ENVELOPE_BYTES
        ),
        "path_copy_locator_bytes_if_manifest_is_referenced": (
            C005_BYTES
            + 6  # locator header
            + 32  # exact c005 FileStore object ID claim
            + TREE_ROOT_DESCRIPTOR_BYTES
            + touched_bytes
            + LOCATOR_ENVELOPE_BYTES
        ),
    }


def main() -> None:
    results = {
        "model": {
            "rows": N,
            "c005_fixed_bytes": C005_FIXED_BYTES,
            "c005_bytes": C005_BYTES,
            "cut_policy": [MIN_ENTRIES, TARGET_ENTRIES, MAX_ENTRIES],
            "key_bytes": KEY_BYTES,
            "value_bytes": VALUE_BYTES,
            "segment_bridge_record_bytes": FLAT_SEGMENT_BRIDGE_BYTES,
            "hash_stand_in": "SHA-256 for BLAKE3; encoded widths exact for modeled types",
            "update_shape": "one remove and one insert per logical segment replacement, sequential sorted TreeChange operations",
            "jumbo_objects": 0,
        },
        "scenarios": [simulate(count) for count in EDIT_COUNTS],
    }
    print(json.dumps(results, indent=2, sort_keys=True))


if __name__ == "__main__":
    main()
