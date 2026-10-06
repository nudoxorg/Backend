"""Read-only byte trace. Native store admission supplies cryptographic proof.

This diagnostic reports original pack pointer omissions without rewriting any
object, publication, journal entry, or secret. It is not an acceptance oracle.
"""
import argparse
import hashlib
import json
from pathlib import Path

parser = argparse.ArgumentParser()
parser.add_argument("snapshot", type=Path)
args = parser.parse_args()
root = args.snapshot / "objects"
objects, packs, files = {}, {}, []
for path in sorted(root.rglob("*")):
    if not path.is_file() or path.name in ("authority.secret", "journal"):
        continue
    data = path.read_bytes()
    files.append((path, data))
    magic = b"LUNA_OBJECT_V1\0"
    if data.startswith(magic):
        at = len(magic)
        identity = data[at:at + 32].hex()
        at += 32
        schema = (data[at], int.from_bytes(data[at + 1:at + 3], "little"), data[at + 3])
        at += 4 + 64 + 8
        objects[identity] = (schema, data[at:])
    magic = b"backend.workspace.index.v3\0"
    if magic in data:
        at = data.index(magic) + len(magic)
        closure = data[at + 32:at + 64].hex()
        at += 96 + 160
        count = int.from_bytes(data[at:at + 2], "big")
        at += 2
        assert count <= 128
        packs[closure] = [data[at + i * 32:at + (i + 1) * 32].hex() for i in range(count)]
journal = (root / "journal").read_bytes()
width = journal.find(b"LUNA_J2\0", 8)
assert width == 396 and len(journal) % width == 0
publications = []
for offset in range(0, len(journal), width):
    frame = journal[offset:offset + width]
    assert frame.startswith(b"LUNA_J2\0")
    if frame[9] != 1:
        continue
    at = 8 + 2 + 16 + 32 + 32 + 33
    target = frame[at:at + 32]
    at += 32 + 32 + 32
    closure = frame[at:at + 32]
    at += 32 + 97
    base = int.from_bytes(frame[at:at + 8], "little")
    generation = int.from_bytes(frame[at + 8:at + 16], "little")
    ids = packs[closure.hex()]
    roots = [objects[i][1].hex() for i in ids if i in objects and objects[i][0] == (0x97, 5, 1)]
    needle = target + generation.to_bytes(8, "big") + closure
    occurrences = []
    for path, data in files:
        position = data.find(needle)
        if position >= 0:
            occurrences.append({"path": str(path.relative_to(root)),
                                "file_sha256": hashlib.sha256(data).hexdigest(),
                                "capture_option_bytes": data[position + 72:position + 105].hex()})
    publications.append({"generation": generation, "base_generation": base,
                         "target": target.hex(), "closure": closure.hex(),
                         "recovery_auxiliary_count": len(ids),
                         "recovery_capture_pointers": roots, "successor_basis_occurrences": occurrences})
print(json.dumps({"diagnostic_only": True, "snapshot": str(args.snapshot),
                  "journal_sha256": hashlib.sha256(journal).hexdigest(),
                  "publications": publications}, indent=2))
