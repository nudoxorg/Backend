"""Exercise the published Roslyn helper with top-level and library sources.

Usage: python3 top_level.py /absolute/path/to/dotnet /absolute/path/to/oracle.dll
The helper must already be published; these tests never build or run the fixture.
"""
import json
import pathlib
import struct
import subprocess
import sys
import tempfile


def check(dotnet, helper, source, expected_errors):
    with tempfile.TemporaryDirectory(prefix="nudox-roslyn-top-level-") as tmp:
        root = pathlib.Path(tmp)
        (root / "Program.cs").write_text(source, encoding="utf-8")
        output = root / "image.json"
        result = subprocess.run(
            [dotnet, "exec", helper, "--mode", "source", "--root", str(root),
             "--out", str(output)], capture_output=True, text=True, check=True, timeout=30,
        )
        document = json.loads(output.read_text(encoding="utf-8"))
        assert document["diagnostics"]["errorCount"] == expected_errors, result.stderr
        assert "CS8805" not in result.stderr, result.stderr
        return result.stderr


def check_partial_roles(dotnet, helper, source, roles):
    with tempfile.TemporaryDirectory(prefix="nudox-roslyn-partial-") as tmp:
        root = pathlib.Path(tmp)
        binding = root / "Program.cs"
        binding.write_text(source, encoding="utf-8")
        output = root / "image.bin"
        subprocess.run(
            [dotnet, "exec", helper, "--mode", "source", "--root", str(root),
             "--authority-image", "--source-binding", str(binding), "--out", str(output)],
            capture_output=True, text=True, check=True, timeout=30,
        )
        image = output.read_bytes()
        # The v4 directory's third entry is the fixed 52-byte declaration plane.
        _, count, offset, length = struct.unpack_from("<IIII", image, 48 + 2 * 16)
        assert length == count * 52
        actual = [image[offset + i * 52 + 2] for i in range(count)]
        assert actual == roles, actual


def check_context(dotnet, helper, source, expected, same_file=False, errors=0):
    with tempfile.TemporaryDirectory(prefix="nudox-roslyn-context-") as tmp:
        root = pathlib.Path(tmp)
        binding = root / "Program.cs"
        binding.write_text(source, encoding="utf-8")
        if not same_file:
            (root / "Endpoints.cs").write_text('public static class Endpoints { public static void MapAccounts() {} }\n', encoding="utf-8")
        output = root / "image.bin"
        subprocess.run([dotnet, "exec", helper, "--mode", "source", "--root", str(root),
                        "--authority-image", "--source-binding", str(binding), "--out", str(output)],
                       capture_output=True, text=True, check=True, timeout=30)
        image = output.read_bytes()
        version = struct.unpack_from("<H", image, 4)[0]
        _, count, offset, length = struct.unpack_from("<IIII", image, 48 + 10 * 16)
        assert length == count * 28
        rows = [struct.unpack_from("<IIIIII", image, offset + i * 28) for i in range(count)]
        contexts = [row for row in rows if row[0] == 0xffffffff]
        assert version == (5 if expected or errors else 4), version
        assert struct.unpack_from("<H", image, 46)[0] == errors
        assert len(contexts) == (1 if expected else 0), contexts
        if expected:
            start, end = contexts[0][4:6]
            expected_span = b"Endpoints.MapAccounts" if same_file else b"MapAccounts"
            assert source.encode()[start:end] == expected_span, (start, end)
            assert (contexts[0][1] != 0xffffffff) == same_file, contexts[0]


def main():
    dotnet, helper = sys.argv[1:]
    check(dotnet, helper, 'System.Console.WriteLine("ready");\npublic partial class Program;\n', 0)
    check(dotnet, helper, 'public class Library { public int Value => 1; }\n', 0)
    diagnostic = check(dotnet, helper, 'MissingName();\npublic partial class Program;\n', 1)
    assert "CS0103" in diagnostic, diagnostic
    check_partial_roles(dotnet, helper, 'System.Console.WriteLine("ready");\npublic partial class Program;\n', [1])
    check_partial_roles(dotnet, helper, 'public partial class Parts {}\npublic partial class Parts {}\n', [1, 2])
    check_context(dotnet, helper, 'Endpoints.MapAccounts();\npublic partial class Program;\n', True)
    check_context(dotnet, helper, 'Endpoints.MapAccounts();\npublic static class Endpoints { public static void MapAccounts() {} }\npublic partial class Program;\n', True, same_file=True)
    check_context(dotnet, helper, 'MissingName();\npublic partial class Program;\n', False)
    check_context(dotnet, helper, 'public class C { public void M() { Endpoints.MapAccounts(); } }\n', False)
    print("Roslyn top-level, library, invalid-source, partial-role, and context controls passed")


if __name__ == "__main__":
    main()
