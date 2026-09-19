#!/usr/bin/python3
"""Deterministic native authority fixture for cold and persistent tests.

The fixture implements the compile-owned BCF session framing and BCN semantic
envelope. Values model each frontend's admitted helper contract while keeping
the transport behavior deterministic.
"""

import struct
import sys
import time


def envelope(language, session, manifest, authority, revision):
    records = semantic_records(language)
    language = language.encode("utf-8")
    out = bytearray(b"BCN\0" + struct.pack(">HBBI", 1, 0, 0, len(records)))
    out.extend(session)
    out.extend(manifest)
    out.extend(authority)
    out.extend(struct.pack(">QH", revision, len(language)))
    out.extend(language)
    for kind, key, value in records:
        out.extend(struct.pack(">BBII", kind, 0, len(key), len(value)))
        out.extend(key)
        out.extend(value)
    return bytes(out)


def semantic_records(language):
    if language == "clang":
        return [
            (1, b"main", b"kind=function;definition=definition;storage=none;virtuality=non-virtual"),
            (2, b"main", b"kind=builtin;const=false;volatile=false;restrict=false"),
            (3, b"main->i32", b"reference-local"),
            (4, b"warn", b"severity=warning;message=fixture"),
            (5, b"std", b"present"),
            (6, b"missing", b"absent"),
        ]
    if language in ("java", "csharp"):
        version = f"{language}-semantic-v1".encode()
        return [
            (1, b"main", b"\0".join((version, b"class", b"fixture", b"main()", b"fixture declaration"))),
            (2, b"main", b"\0".join((version, b"main", b"i32"))),
            (3, b"main->i32", b"\0".join((version, b"main", b"i32", b"0", b"1"))),
            (4, b"warn", b"\0".join((version, b"warning", b"fixture", b"0", b"1", b"fixture diagnostic"))),
            (5, b"std", b"\0".join((version, b"package", b"std", b"present"))),
            (6, b"missing", b"\0".join((version, b"package", b"missing", b"absent"))),
        ]
    if language == "python":
        return [
            (1, b"main", b'{"owner":"fixture","name":"main","kind":"function","span":{"start":0,"end":1},"signature":"main()","documentation":null,"class_form":null,"parameters":[],"annotations":[]}'),
            (2, b"main", b'{"owner":"main","site":"return","span":{"start":0,"end":1},"inferred":"int"}'),
            (3, b"main->int", b'{"owner":"main","target":"int","span":{"start":0,"end":1},"resolution":"local","module":null}'),
            (4, b"warn", b'{"severity":"warning","code":"fixture","span":{"start":0,"end":1},"message":"fixture diagnostic"}'),
            (5, b"std", b'{"binding":"std","module":"std","span":{"start":0,"end":1}}'),
            (6, b"missing", b'{"binding":"missing","module":"missing","span":{"start":0,"end":1}}'),
        ]
    if language == "go":
        version = b"go-semantic-v1"
        return [
            (1, b"main", b"\0".join((version, b"function", b"fixture", b"func()", b"fixture declaration", b"0", b"1"))),
            (2, b"main", b"\0".join((version, b"main", b"func()"))),
            (3, b"main->go/builtin/int", b"\0".join((version, b"main", b"go/builtin/int", b"0", b"1", b"foreign"))),
            (4, b"warn", b"\0".join((version, b"warning", b"fixture", b"0", b"1", b"fixture diagnostic"))),
            (5, b"go/std", b"\0".join((version, b"package", b"std", b"present"))),
            (6, b"go/missing", b"\0".join((version, b"package", b"missing", b"absent"))),
        ]
    return [
        (1, b"main", b"decl"),
        (2, b"main", b"i32"),
        (3, b"main->i32", b"ref"),
        (4, b"warn", b"ok"),
        (5, b"std", b"present"),
        (6, b"missing", b"absent"),
    ]


def frame_length(header):
    if header[:3] != b"BCF" or struct.unpack(">H", header[3:5])[0] != 1:
        raise RuntimeError("bad session frame")
    # BCF hello, request, cancel, reset, response lengths.
    return {1: 38, 2: 54, 3: 14, 4: 6, 5: 54}[header[5]]


def read_exact(length):
    value = sys.stdin.buffer.read(length)
    if len(value) != length:
        raise RuntimeError("truncated input")
    return value


def read_frame():
    header = sys.stdin.buffer.read(6)
    if not header:
        return None
    if len(header) != 6:
        raise RuntimeError("truncated frame")
    rest = read_exact(frame_length(header) - 6)
    return header + rest


def read_request_body():
    """Read one BCQ request body sent after a BCF request frame."""
    prefix = read_exact(10)
    if prefix[:4] != b"BCQ\0" or struct.unpack(">H", prefix[4:6])[0] != 1:
        raise RuntimeError("bad request body")
    language_length, input_count = struct.unpack(">HH", prefix[6:10])
    session = read_exact(32)
    manifest = read_exact(32)
    authority = read_exact(32)
    language = read_exact(language_length).decode("utf-8")
    for _ in range(input_count):
        name_length, value_length = struct.unpack(">HI", read_exact(6))
        read_exact(name_length)
        read_exact(value_length)
    return language, session, manifest, authority


def cold():
    request = sys.stdin.buffer.read()
    if len(request) < 106 or request[:4] != b"BCQ\0":
        raise RuntimeError("missing cold request")
    if struct.unpack(">H", request[4:6])[0] != 1:
        raise RuntimeError("unsupported request version")
    language_length, input_count = struct.unpack(">HH", request[6:10])
    session = request[10:42]
    manifest = request[42:74]
    authority = request[74:106]
    language_start = 106
    language_end = language_start + language_length
    language = request[language_start:language_end].decode("utf-8")
    # A cold helper consumes and validates the complete bounded request before
    # producing any semantic bytes. Input fields are not interpreted here.
    cursor = language_end
    for _ in range(input_count):
        if cursor + 6 > len(request):
            raise RuntimeError("truncated request field")
        name_length, value_length = struct.unpack(">HI", request[cursor : cursor + 6])
        cursor += 6 + name_length + value_length
        if cursor > len(request):
            raise RuntimeError("truncated request field")
    if cursor != len(request):
        raise RuntimeError("trailing request bytes")
    sys.stdout.buffer.write(envelope(language, session, manifest, authority, 0))
    sys.stdout.buffer.flush()


def persistent():
    hello = read_frame()
    if hello is None:
        return
    if hello[5] != 1:
        raise RuntimeError("expected hello")
    sys.stdout.buffer.write(hello)
    sys.stdout.buffer.flush()
    while True:
        request = read_frame()
        if request is None:
            return
        if request[5] != 2:
            raise RuntimeError("expected request")
        language, session, manifest, authority = read_request_body()
        revision = struct.unpack(">Q", request[46:54])[0]
        # The BCF response wakes the runner only after the complete semantic
        # envelope has been published on its bounded payload channel.
        sys.stderr.buffer.write(envelope(language, session, manifest, authority, revision))
        sys.stderr.buffer.flush()
        time.sleep(0.02)
        response = bytearray(request)
        response[5] = 5
        sys.stdout.buffer.write(response)
        sys.stdout.buffer.flush()


def main():
    first = sys.stdin.buffer.peek(4)[:4]
    if first == b"BCF\0":
        persistent()
    else:
        cold()


if __name__ == "__main__":
    main()
