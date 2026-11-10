"""Bound captured foreign-occurrence evidence; fixture code is never executed.

Usage: python3 captured_occurrence_limit.py /absolute/dotnet /absolute/oracle.dll
"""
import json
import pathlib
import subprocess
import sys
import tempfile


def main():
    dotnet, helper = sys.argv[1:]
    with tempfile.TemporaryDirectory(prefix="nudox-csharp-occurrence-limit-") as tmp:
        root = pathlib.Path(tmp)
        config = root / "config.json"
        config.write_text(json.dumps([
            {"Path": "App.csproj", "Text": "<Project/>"},
            {"Path": "Directory.Build.props", "Text": None},
            {"Path": "Directory.Build.targets", "Text": None},
        ]))
        source = "public class Caller { public void Run() {\n" + "Target.Go();\n" * 8193 + "} }\n"
        members = [{"Path": "A.cs", "Text": source},
                   {"Path": "B.cs", "Text": "public static class Target { public static void Go() {} }"}]
        evidence = root / "evidence.json"
        command = [dotnet, "exec", helper, "--mode", "source", "--root", tmp,
                   "--captured-sources", "--project-file", str(root / "App.csproj"),
                   "--captured-config-file", str(config), "--authority-image",
                   "--source-binding", str(root / "A.cs"), "--out", str(root / "image"),
                   "--capture-evidence-file", str(evidence), "--capture-evidence-nonce",
                   "0123456789abcdef0123456789abcdef"]
        result = subprocess.run(command, input=json.dumps(members), capture_output=True, text=True, timeout=60)
        assert result.returncode != 0, "oversized occurrence list accepted"
        assert "foreign occurrence evidence exceeds limit" in result.stderr, result.stderr
        assert not evidence.exists(), "partial occurrence evidence was published"
        image_only = subprocess.run(command[:-4], input=json.dumps(members), capture_output=True, text=True, timeout=60)
        assert image_only.returncode == 0, image_only.stderr
        assert (root / "image").stat().st_size > 0
        print("oversized evidence refused; ordinary captured image remains available")


if __name__ == "__main__":
    main()
