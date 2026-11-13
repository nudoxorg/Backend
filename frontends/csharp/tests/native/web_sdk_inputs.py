"""Check explicit SDK targeting packs and Web usings with Roslyn only.

Usage: python3 web_sdk_inputs.py /absolute/sdk-root /absolute/oracle.dll
This is an explicit-input component control, not SDK selection or product coverage.
"""
import json
import pathlib
import subprocess
import sys
import tempfile
import xml.etree.ElementTree as ET


def tag(element):
    return element.tag.rsplit("}", 1)[-1]


def main():
    sdk, helper = map(pathlib.Path, sys.argv[1:])
    dotnet = sdk / "dotnet"
    version = subprocess.run([dotnet, "--version"], capture_output=True, text=True,
                             check=True, timeout=30).stdout.strip()
    sdkdir = sdk / "sdk" / version
    props = ET.parse(sdkdir / "Microsoft.NETCoreSdk.BundledVersions.props").getroot()
    refs = {e.attrib["Include"]: (e.attrib["TargetingPackName"], e.attrib["TargetingPackVersion"])
            for e in props.iter() if tag(e) == "KnownFrameworkReference"
            and e.attrib.get("TargetFramework") == "net10.0"
            and e.attrib.get("Include") in ["Microsoft.NETCore.App", "Microsoft.AspNetCore.App"]}
    assert len(refs) == 2, refs
    web = ET.parse(sdkdir / "Sdks/Microsoft.NET.Sdk.Web/targets/Sdk.Server.props").getroot()
    usings = [e.attrib["Include"] for e in web.iter() if tag(e) == "Using" and "Include" in e.attrib]
    assert len(usings) == 9, usings
    with tempfile.TemporaryDirectory(prefix="nudox-csharp-web-") as tmp:
        root = pathlib.Path(tmp)
        (root / "App.csproj").write_text('<Project Sdk="Microsoft.NET.Sdk.Web"><PropertyGroup><TargetFramework>net10.0</TargetFramework><ImplicitUsings>enable</ImplicitUsings></PropertyGroup></Project>\n')
        (root / "Program.cs").write_text('var builder = WebApplication.CreateBuilder(args);\nbuilder.Services.AddRateLimiter(options => {});\nvar app = builder.Build();\napp.Run();\npublic partial class Program;\n')
        out = root / "result.json"
        cmd = [dotnet, "exec", helper, "--mode", "source", "--root", root,
               "--project-file", root / "App.csproj", "--out", out]
        for name, pack_version in refs.values():
            cmd += ["--ref-dir", sdk / "packs" / name / pack_version / "ref/net10.0"]
        for namespace in usings:
            cmd += ["--using", namespace]
        result = subprocess.run(cmd, capture_output=True, text=True, check=True, timeout=30)
        assert json.loads(out.read_text())["diagnostics"]["errorCount"] == 0, result.stderr
        captured_input = json.dumps([{"Path": "Program.cs", "Text": (root / "Program.cs").read_text()}])
        captured = subprocess.run(cmd + ["--captured-sources"], input=captured_input,
                                  capture_output=True, text=True, check=True, timeout=30)
        assert json.loads(out.read_text())["diagnostics"]["errorCount"] == 0, captured.stderr
        assert json.loads(out.read_text())["diagnostics"]["generatorSupport"] == "unknown"
        duplicate = subprocess.run(cmd + ["--ref-dir", sdk / "packs" / refs["Microsoft.NETCore.App"][0] / refs["Microsoft.NETCore.App"][1] / "ref/net10.0"], capture_output=True, text=True, timeout=30)
        assert duplicate.returncode != 0 and "duplicate reference assembly" in duplicate.stderr
        absent = subprocess.run(cmd + ["--ref-dir", root / "absent"], capture_output=True, text=True, timeout=30)
        assert absent.returncode != 0 and "reference assembly directory is unavailable" in absent.stderr
        invalid_dir = root / "invalid"
        invalid_dir.mkdir()
        (invalid_dir / "Invalid.dll").write_bytes(b"not a managed assembly")
        invalid = subprocess.run(cmd + ["--ref-dir", invalid_dir], capture_output=True, text=True, timeout=30)
        assert invalid.returncode != 0 and "configured reference assembly could not be loaded" in invalid.stderr
        invalid_captured = subprocess.run(cmd + ["--captured-sources", "--ref-dir", invalid_dir],
                                          input=captured_input, capture_output=True, text=True, timeout=30)
        assert invalid_captured.returncode != 0 and "configured reference assembly could not be loaded" in invalid_captured.stderr
        (invalid_dir / "Invalid.dll").unlink()
        with (invalid_dir / "Oversize.dll").open("wb") as oversized:
            oversized.truncate(33 * 1024 * 1024)
        limited = subprocess.run(cmd + ["--captured-sources", "--ref-dir", invalid_dir],
                                 input=captured_input, capture_output=True, text=True, timeout=30)
        assert limited.returncode != 0 and "captured reference set exceeds limit" in limited.stderr
        print("explicit Core/ASP.NET packs, Web usings, duplicate, absent and corrupt reference refusal passed")


if __name__ == "__main__":
    main()
