"""Check a published helper against captured source bytes without running user code.

Usage: python3 captured_sources.py /absolute/dotnet /absolute/oracle.dll
"""
import hashlib
import json
import pathlib
import struct
import subprocess
import sys
import tempfile


def main():
    dotnet, helper = sys.argv[1:]
    with tempfile.TemporaryDirectory(prefix="nudox-csharp-captured-") as tmp:
        root = pathlib.Path(tmp)
        (root / "A.cs").write_text("public class DiskOnly {}", encoding="utf-8")
        (root / "App.csproj").write_text('<Project><ItemGroup><Analyzer Include="missing-generator.dll" /></ItemGroup></Project>', encoding="utf-8")
        sources = [
            {"Path": "A.cs", "Text": "\ufeffpublic class Captured { public void Call() { Target.Go(); } }"},
            {"Path": "B.cs", "Text": "public static class Target { public static void Go() {} }"},
        ]
        command = [dotnet, "exec", helper, "--mode", "source", "--root", tmp, "--captured-sources"]
        def run(entries, args=(), check=True):
            return subprocess.run(command + list(args), input=json.dumps(entries), text=True,
                                  capture_output=True, check=check, timeout=30)
        document = root / "out.json"
        run(sources, ["--out", str(document)])
        output = json.loads(document.read_text())
        assert output["diagnostics"]["generatorSupport"] == "unknown"
        names = [row["simpleName"] for row in output["types"]]
        assert names == ["Captured", "Target"], names
        manifest = root / "manifest.json"
        manifest.write_text(json.dumps(sources), encoding="utf-8")
        subprocess.run([dotnet, "exec", helper, "--mode", "source", "--root", tmp,
                        "--captured-sources-file", str(manifest), "--out", str(document)],
                       capture_output=True, text=True, check=True, timeout=30)
        assert [row["simpleName"] for row in json.loads(document.read_text())["types"]] == names
        assert json.loads(document.read_text())["diagnostics"]["generatorSupport"] == "unknown"
        legacy = subprocess.run([dotnet, "exec", helper, "--mode", "source", "--root", tmp,
                                 "--out", str(document)], capture_output=True, text=True,
                                check=True, timeout=30)
        assert json.loads(document.read_text())["diagnostics"]["generatorSupport"] == "unavailable"
        assert "source generator unavailable" in legacy.stderr
        image = root / "out.img"
        run(sources, ["--authority-image", "--source-binding", str(root / "A.cs"), "--out", str(image)])
        assert image.read_bytes()[12:44] == hashlib.sha256(sources[0]["Text"].encode()).digest()
        assert struct.unpack_from("<H", image.read_bytes(), 4)[0] == 5
        assert struct.unpack_from("<H", image.read_bytes(), 46)[0] == 0
        missing = [{"Path": "A.cs", "Text": "MissingName();\npublic partial class Program;\n"}]
        run(missing, ["--authority-image", "--source-binding", str(root / "A.cs"), "--out", str(image)])
        assert struct.unpack_from("<H", image.read_bytes(), 4)[0] == 5
        assert struct.unpack_from("<H", image.read_bytes(), 46)[0] > 0
        for path in ["../escape.cs", "/absolute.cs", "A.cs"]:
            bad = [sources[0], {"Path": path, "Text": "public class Bad {}"}]
            assert run(bad, check=False).returncode != 0, path
        print("captured sources, live-disk divergence, BOM binding, and invalid paths passed")


if __name__ == "__main__":
    main()
