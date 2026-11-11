"""Real helper control for private calls, UTF-8 source extents and diagnostics.

Usage: python3 private_source.py /absolute/dotnet /absolute/oracle.dll
This checks producer output, not a selected owner publication.
"""
import hashlib
import json
import pathlib
import struct
import subprocess
import sys
import tempfile


def main():
    dotnet, helper = map(pathlib.Path, sys.argv[1:])
    source = ('// λ before byte coordinates\npublic static class Accounts {\n'
              '    public static void MapAccounts() { MissingFields(); MissingFields(); }\n'
              '    private static bool MissingFields() { return false; }\n}\n')
    with tempfile.TemporaryDirectory(prefix="nudox-private-source-") as tmp:
        root = pathlib.Path(tmp)
        binding = root / "Accounts.cs"
        binding.write_text(source)
        command = [str(dotnet), "exec", str(helper), "--mode", "source", "--root", tmp,
                   "--authority-image", "--source-binding", str(binding), "--captured-sources"]
        def produce(text):
            return subprocess.run(command, input=json.dumps([{"Path": "Accounts.cs", "Text": text}]).encode(),
                                  capture_output=True, check=True, timeout=30)
        result = produce(source)
        image = result.stdout
        assert image[:4] == b"NCAI", image[:32]
        assert struct.unpack_from("<H", image, 46)[0] == 0, result.stderr
        assert image[12:44] == hashlib.sha256(source.encode()).digest()
        def section(number):
            _, width, count, offset, size = struct.unpack_from("<HHIII", image, 48 + 16 * (number - 1))
            assert size == width * count
            return width, count, offset
        atom_width, _, atoms = section(1)
        _, _, atom_bytes = section(2)
        def atom(index):
            offset, length = struct.unpack_from("<II", image, atoms + atom_width * index)
            return image[atom_bytes + offset:atom_bytes + offset + length]
        width, count, declarations = section(3)
        targets = []
        for index in range(count):
            row = declarations + width * index
            if atom(struct.unpack_from("<I", image, row + 4)[0]) == b"MissingFields":
                start = struct.unpack_from("<I", image, row + 20)[0]
                end = struct.unpack_from("<I", image, row + 48)[0]
                assert source.encode()[start:end] == b"private static bool MissingFields() { return false; }"
                targets.append(index)
        assert len(targets) == 1
        width, count, references = section(11)
        calls = [index for index in range(count)
                 if struct.unpack_from("<I", image, references + width * index + 4)[0] == targets[0]]
        assert len(calls) == 2, (len(calls), result.stderr)
        invalid = produce("public class Bad { void Run() { MissingName(); } }\n")
        assert struct.unpack_from("<H", invalid.stdout, 46)[0] > 0
        assert b"CS0103" in invalid.stderr
        print("real Roslyn retains both private calls, complete UTF-8 method source, and CS0103 diagnostics")


if __name__ == "__main__":
    main()
