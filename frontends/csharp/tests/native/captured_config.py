"""Check private captured project configuration without running application code.

Usage: python3 captured_config.py /absolute/dotnet /absolute/oracle.dll
"""
import json
import hashlib
import pathlib
import subprocess
import sys
import tempfile


def main():
    dotnet, helper = sys.argv[1:]
    with tempfile.TemporaryDirectory(prefix="nudox-csharp-config-") as tmp:
        root = pathlib.Path(tmp)
        project = root / "App.csproj"
        project.write_text('<Project><ItemGroup><Using Include="System.Net" /></ItemGroup></Project>')
        (root / "Directory.Build.props").write_text('<Project><ItemGroup><Using Remove="System.Text" /></ItemGroup></Project>')
        config = [
            {"Path": "App.csproj", "Text": '<Project><ItemGroup><Using Include="System.Text" /></ItemGroup></Project>'},
            {"Path": "Directory.Build.props", "Text": None},
            {"Path": "Directory.Build.targets", "Text": None},
        ]
        manifest = root / "config.json"
        output = root / "out.json"
        source = [{"Path": "App.cs", "Text": "public class Sample { StringBuilder value = new(); }"}]
        command = [dotnet, "exec", helper, "--mode", "source", "--root", tmp,
                   "--project-file", str(project), "--captured-sources", "--captured-config-file", str(manifest),
                   "--out", str(output)]
        def run(entries, check=True):
            manifest.write_text(json.dumps(entries))
            return subprocess.run(command, input=json.dumps(source), capture_output=True, text=True,
                                  check=check, timeout=30)
        run(config)
        assert json.loads(output.read_text())["diagnostics"]["errorCount"] == 0
        assert json.loads(output.read_text())["diagnostics"]["generatorSupport"] == "unknown"
        project.unlink()
        run(config)
        assert json.loads(output.read_text())["diagnostics"]["errorCount"] == 0
        assert run(config[:-1], check=False).returncode != 0
        assert run([config[0], config[2], config[1]], check=False).returncode != 0
        invalid = list(config)
        invalid[0] = {"Path": "App.csproj", "Text": '<!DOCTYPE x [<!ENTITY e SYSTEM "file:///etc/passwd">]><Project>&e;</Project>'}
        result = run(invalid, check=False)
        assert result.returncode != 0 and "captured config XML is invalid" in result.stderr
        manifest.write_text(json.dumps(config))
        image = root / "out.img"
        evidence = root / "evidence.json"
        nonce = "0123456789abcdef0123456789abcdef"
        image_command = command[:-2] + ["--authority-image", "--source-binding", str(root / "App.cs"),
                                         "--capture-evidence-file", str(evidence), "--capture-evidence-nonce", nonce,
                                         "--out", str(image)]
        subprocess.run(image_command, input=json.dumps(source), capture_output=True, text=True, check=True, timeout=30)
        receipt = json.loads(evidence.read_text())
        assert receipt["nonce"] == nonce and receipt["sourceBinding"] == "App.cs"
        assert receipt["imageSha256"] == hashlib.sha256(image.read_bytes()).hexdigest()
        assert receipt["configSha256"] == hashlib.sha256(manifest.read_bytes()).hexdigest()
        assert receipt["sources"][0]["sha256"] == hashlib.sha256(source[0]["Text"].encode()).hexdigest()
        assert receipt["references"] and receipt["generators"] == receipt["inputClosure"] == "unknown"
        candidates = (pathlib.Path(dotnet).parent / "shared/Microsoft.NETCore.App").glob("*/System.Runtime.dll")
        system_runtime = next(ref for ref in receipt["references"] if ref["Name"] == "System.Runtime")
        assert any(system_runtime["Length"] == path.stat().st_size and
                   system_runtime["Sha256"] == hashlib.sha256(path.read_bytes()).hexdigest()
                   for path in candidates)
        reused = subprocess.run(image_command, input=json.dumps(source), capture_output=True, text=True, timeout=30)
        assert reused.returncode != 0
        deep = "/".join(["nested"] * 8 + ["Deep.csproj"])
        deep_command = [dotnet, "exec", helper, "--mode", "source", "--root", tmp,
                        "--project-file", str(root / deep), "--captured-sources", "--captured-config-file", str(manifest),
                        "--out", str(output)]
        manifest.write_text("[]")
        too_deep = subprocess.run(deep_command, input=json.dumps(source), capture_output=True, text=True, timeout=30)
        assert too_deep.returncode != 0 and "captured config project exceeds scope depth" in too_deep.stderr
        manifest.write_bytes(b"\xff")
        bad_utf8 = subprocess.run(command, input=json.dumps(source), capture_output=True, text=True, timeout=30)
        assert bad_utf8.returncode != 0 and "captured config manifest could not be read" in bad_utf8.stderr
        print("captured config bytes and absence probes isolated; invalid probes/XML refused")


if __name__ == "__main__":
    main()
