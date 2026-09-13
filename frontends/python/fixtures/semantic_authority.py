#!/usr/bin/python3
"""Deterministic pyrefly-shaped semantic authority fixture."""

import json
import struct
import sys
import time


def compact(value):
    return json.dumps(value, ensure_ascii=False, separators=(",", ":")).encode()


def records():
    span = lambda start, end: {"start": start, "end": end}
    return [
        (1, "module.Café", compact({
            "owner": "__main__", "name": "Café", "kind": "class",
            "span": span(0, 72), "signature": "class Café(Protocol):",
            "documentation": "class docs", "class_form": "protocol",
            "parameters": [], "annotations": ["Protocol"],
        })),
        (1, "module.Café.title", compact({
            "owner": "Café", "name": "title", "kind": "function",
            "span": span(31, 72), "signature": "def title(self, value: str) -> str:",
            "documentation": "method docs", "class_form": None,
            "parameters": [
                {"name": "self", "kind": "positional_or_keyword", "span": span(41, 45),
                 "annotation": None, "has_default": False},
                {"name": "value", "kind": "positional_or_keyword", "span": span(47, 52),
                 "annotation": "str", "has_default": False},
            ], "annotations": ["str", "str"],
        })),
        (2, "module.Café.title.value", compact({
            "owner": "Café.title", "site": "parameter", "span": span(47, 52),
            "inferred": "str",
        })),
        (3, "module.Café.title->Imported", compact({
            "owner": "Café.title", "target": "Imported", "span": span(64, 72),
            "resolution": "foreign", "module": "package",
        })),
        (4, "diagnostic.information.reveal-type", compact({
            "severity": "information", "code": "reveal-type", "span": span(47, 52),
            "message": "revealed type: str",
        })),
        (5, "import.Imported", compact({
            "binding": "Imported", "module": "package", "span": span(5, 12),
        })),
        (6, "import.optional_missing", compact({
            "binding": "optional_missing", "module": "optional_missing",
            "span": span(13, 29),
        })),
    ]


def envelope(language, session, manifest, authority, revision):
    facts = records()
    encoded_language = language.encode()
    out = bytearray(b"BCN\0" + struct.pack(">HBBI", 1, 0, 0, len(facts)))
    out.extend(session)
    out.extend(manifest)
    out.extend(authority)
    out.extend(struct.pack(">QH", revision, len(encoded_language)))
    out.extend(encoded_language)
    for kind, key, value in facts:
        encoded_key = key.encode()
        out.extend(struct.pack(">BBII", kind, 0, len(encoded_key), len(value)))
        out.extend(encoded_key)
        out.extend(value)
    return bytes(out)


def read_exact(length):
    value = sys.stdin.buffer.read(length)
    if len(value) != length:
        raise RuntimeError("truncated input")
    return value


def request_body():
    prefix = read_exact(10)
    language_length, input_count = struct.unpack(">HH", prefix[6:10])
    session, manifest, authority = read_exact(32), read_exact(32), read_exact(32)
    language = read_exact(language_length).decode()
    for _ in range(input_count):
        name_length, value_length = struct.unpack(">HI", read_exact(6))
        read_exact(name_length + value_length)
    return language, session, manifest, authority


def persistent():
    hello = read_exact(38)
    sys.stdout.buffer.write(hello)
    sys.stdout.buffer.flush()
    while True:
        header = sys.stdin.buffer.read(6)
        if not header:
            return
        request = header + read_exact(48)
        language, session, manifest, authority = request_body()
        revision = struct.unpack(">Q", request[46:54])[0]
        sys.stderr.buffer.write(envelope(language, session, manifest, authority, revision))
        sys.stderr.buffer.flush()
        time.sleep(0.02)
        response = bytearray(request)
        response[5] = 5
        sys.stdout.buffer.write(response)
        sys.stdout.buffer.flush()


def cold(request):
    language_length, input_count = struct.unpack(">HH", request[6:10])
    session, manifest, authority = request[10:42], request[42:74], request[74:106]
    cursor = 106
    language = request[cursor:cursor + language_length].decode()
    cursor += language_length
    for _ in range(input_count):
        name_length, value_length = struct.unpack(">HI", request[cursor:cursor + 6])
        cursor += 6 + name_length + value_length
    if cursor != len(request):
        raise RuntimeError("trailing request bytes")
    sys.stdout.buffer.write(envelope(language, session, manifest, authority, 0))


first = sys.stdin.buffer.peek(4)[:4]
if first == b"BCF\0":
    persistent()
else:
    cold(sys.stdin.buffer.read())
