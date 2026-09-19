#!/usr/bin/python3
"""Cold authority fixture with an explicit partial coverage claim."""
import struct
import sys


def main():
    request = sys.stdin.buffer.read()
    if len(request) < 106 or request[:4] != b"BCQ\0":
        raise SystemExit(2)
    language_length, input_count = struct.unpack(">HH", request[6:10])
    session = request[10:42]
    manifest = request[42:74]
    authority = request[74:106]
    language = request[106 : 106 + language_length]
    cursor = 106 + language_length
    for _ in range(input_count):
        if cursor + 6 > len(request):
            raise SystemExit(2)
        name_length, value_length = struct.unpack(">HI", request[cursor : cursor + 6])
        cursor += 6 + name_length + value_length
    if cursor != len(request):
        raise SystemExit(2)

    records = [(1, b"main", b"decl"), (2, b"main", b"i32")]
    out = bytearray(b"BCN\0" + struct.pack(">HBBI", 1, 1, 0, len(records)))
    out.extend(session)
    out.extend(manifest)
    out.extend(authority)
    out.extend(struct.pack(">QH", 0, len(language)))
    out.extend(language)
    for kind, key, value in records:
        out.extend(struct.pack(">BBII", kind, 0, len(key), len(value)))
        out.extend(key)
        out.extend(value)
    sys.stdout.buffer.write(out)
    sys.stdout.buffer.flush()


if __name__ == "__main__":
    main()
