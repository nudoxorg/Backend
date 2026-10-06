#!/usr/bin/env python3
"""Read-only structural forensics of oversized Core rows in retained NXFI.

This deliberately does not admit a selected store root or grant runtime credit.
It translates the frozen full-wire/Core-row grammar into explicit byte accounting.
Only public source/CAS objects are read; no owner or package code is executed.
"""
import argparse
import ast
import hashlib
import json
from pathlib import Path
import struct

parser = argparse.ArgumentParser()
parser.add_argument("--state", type=Path, required=True)
parser.add_argument("--project", type=Path, required=True)
parser.add_argument("--manifest", type=Path, required=True)
parser.add_argument("--history-receipt", type=Path, required=True)
parser.add_argument("--output", type=Path, required=True)
args = parser.parse_args()


def sha(data):
    return hashlib.sha256(data).hexdigest()


def u32(data, at):
    return struct.unpack_from("<I", data, at)[0]


def u16(data, at):
    return struct.unpack_from("<H", data, at)[0]


def be32(value):
    return struct.pack(">I", value)


head_before = sha((args.state / "objects/HEAD").read_bytes())
manifest = json.loads(args.manifest.read_bytes())
history = json.loads(args.history_receipt.read_bytes())
assert sha(args.manifest.read_bytes()) == history["manifest_sha256"]
rows = []
objects = []
image_count = 0
source_cache = {}
for path in sorted((args.state / "semantic-objects/objects").glob("*.object")):
    envelope = path.read_bytes()
    if envelope[123:127] != b"NXFI":
        continue
    assert envelope.startswith(b"LUNA_OBJECT_V1\0")
    assert envelope[15:47].hex() == path.stem
    assert struct.unpack_from("<Q", envelope, 115)[0] == len(envelope) - 123
    data = envelope[123:]
    assert u32(data, 8) == len(data) and u32(data, 12) == 176
    schema, directory_count = u16(data, 4), u16(data, 6)
    assert (schema, directory_count) in {(1, 26), (2, 27), (3, 29)}
    directories = {}
    for i in range(directory_count):
        offset = 176 + i * 16
        kind = u16(data, offset)
        assert kind == i + 1
        entry = tuple(u32(data, offset + d) for d in (4, 8, 12))
        assert entry[0] + entry[1] <= len(data)
        directories[kind] = entry

    def atom(index):
        assert index < directories[1][2]
        at = directories[1][0] + 8 * index
        start, length = u32(data, at), u32(data, at + 4)
        assert start + length <= directories[2][1]
        return data[directories[2][0] + start:directories[2][0] + start + length]

    def entity(index):
        assert index < directories[3][2]
        return directories[3][0] + 136 * index

    def identity(index):
        at = entity(index)
        return data[at + 36:at + 68]

    def members(index):
        assert index < directories[6][2]
        at = directories[6][0] + 8 * index
        start, length = u32(data, at), u32(data, at + 4)
        assert start + length <= directories[7][1]
        value = data[directories[7][0] + start:directories[7][0] + start + length]
        count = u32(value, 1)
        assert len(value) == 5 + count * 4
        return [u32(value, 5 + 4 * i) for i in range(count)]

    attributes = {}
    for i in range(directories[4][2]):
        at = directories[4][0] + 16 * i
        if data[at] != 5:
            continue
        coordinate, start, count = (u32(data, at + d) for d in (4, 8, 12))
        assert start + count <= directories[5][2]
        values = []
        for j in range(count):
            edge = directories[5][0] + 20 * (start + j)
            assert data[edge] == 2 and data[edge + 1] == 1
            values.append(atom(u32(data, edge + 8)))
        attributes[coordinate] = values
    image_count += 1
    object_row = {"path": str(path), "sha256": sha(envelope), "bytes": len(envelope), "nxfi_offset": 123, "nxfi_sha256": sha(data), "nxfi_schema": schema}
    for index in range(directories[3][2]):
        at = entity(index)
        name = atom(u32(data, at))
        parent = u32(data, at + 8)
        parentage = data[at + 7]
        member_values = members(u32(data, at + 124))
        attribute_values = attributes[u32(data, at + 132)]
        parts = {"identity": identity(index), "name": be32(len(name)) + name,
                 "kind": struct.pack(">H", u16(data, at + 4)), "visibility": data[at + 6:at + 7],
                 "parent": b"\0" if parent == 0xffffffff else b"\1" + identity(parent),
                 "parentage": {0: b"\1", 1: b"\2" + data[at + 84:at + 116], 2: b"\3" + data[at + 84:at + 100], 3: b"\0"}[parentage],
                 "authority_flags": data[at + 24:at + 32], "member_count": be32(len(member_values)),
                 "member_identities": b"".join(identity(i) for i in member_values),
                 "attribute_count": be32(len(attribute_values)),
                 "attributes": b"".join(be32(len(value)) + value for value in attribute_values)}
        payload = b"".join(parts.values())
        observed = 11 + 37 + len(payload)
        if observed <= 4096:
            continue
        source_atom = u32(data, at + 12)
        source_path = None if source_atom == 0xffffffff else atom(source_atom).decode()
        row = {"object": object_row, "family": "SemanticPlaneKind::Ir(Core)", "tag": 1,
               "entity_row": index, "key": identity(index).hex(), "name": name.decode(), "source_path": source_path,
               "source_byte_span": [u32(data, at + 16), u32(data, at + 20)], "members": len(member_values),
               "attributes": len(attribute_values), "field_bytes": {name: len(value) for name, value in parts.items()},
               "payload_bytes": len(payload), "payload_sha256": sha(payload), "segment_header_bytes": 11,
               "record_header_bytes": 37, "observed_segment_bytes": observed, "segment_ceiling": 4096,
               "attribute_values": [{"bytes": len(value), "sha256": sha(value), "prefix": value[:80].decode(errors="replace")} for value in attribute_values]}
        if source_path is not None:
            if source_path not in source_cache:
                source = (args.project / source_path).read_bytes()
                tree = ast.parse(source)
                text = source.decode()
                decorators = []
                for node in ast.walk(tree):
                    if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef)):
                        for decorator in node.decorator_list:
                            value = ast.get_source_segment(text, decorator).encode()
                            decorators.append((node.name, decorator.lineno, decorator.end_lineno, value))
                source_cache[source_path] = source, decorators
            source, decorators = source_cache[source_path]
            row["source_sha256"] = sha(source)
            row["source_decorator_matches"] = [
                {"declaration": name, "start_line": start, "end_line": end, "bytes": len(value), "sha256": sha(value)}
                for name, start, end, value in decorators if value in attribute_values]
        rows.append(row)
    if any(row["object"]["path"] == str(path) for row in rows):
        objects.append(object_row)
head_after = sha((args.state / "objects/HEAD").read_bytes())
assert head_before == head_after
receipt = {"schema": "nudox.retained-native-core-row-structural-forensics.v1", "whole_package_pass": False,
           "outer_store_crypto_readmission_claimed": False, "product_execution_performed": False,
           "state_mutated": False, "selected_head_before_sha256": head_before, "selected_head_after_sha256": head_after,
           "state": str(args.state), "project": str(args.project), "manifest_sha256": sha(args.manifest.read_bytes()),
           "source_commit": manifest["source"]["commit"], "history_receipt_sha256": sha(args.history_receipt.read_bytes()),
           "observer_sha256": sha(Path(__file__).read_bytes()), "nxfi_images_examined": image_count,
           "oversized_core_rows": rows, "implicated_objects": objects}
args.output.write_text(json.dumps(receipt, indent=2) + "\n")
print(json.dumps({"receipt": str(args.output), "sha256": sha(args.output.read_bytes()), "images": image_count,
                  "oversized_rows": len(rows), "matching4314": [{"key": row["key"], "name": row["name"], "field_bytes": row["field_bytes"]} for row in rows if row["observed_segment_bytes"] == 4314]}))
