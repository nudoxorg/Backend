"""Prove captured mode does not execute a synthetic configured generator.

Usage: python3 generator_isolation.py /absolute/dotnet /absolute/oracle.dll
The generated analyzer is a private fixture; no application is run.
"""
import json
import pathlib
import subprocess
import sys
import tempfile
import xml.sax.saxutils


def main():
    dotnet, helper = map(pathlib.Path, sys.argv[1:])
    roslyn = helper.parent
    with tempfile.TemporaryDirectory(prefix="nudox-generator-isolation-") as tmp:
        root = pathlib.Path(tmp)
        generator = root / "generator"
        generator.mkdir()
        marker = root / "executed.marker"
        (generator / "Generator.csproj").write_text(
            '<Project Sdk="Microsoft.NET.Sdk"><PropertyGroup><TargetFramework>net10.0</TargetFramework>'
            '<RestoreIgnoreFailedSources>true</RestoreIgnoreFailedSources><UseSharedCompilation>false</UseSharedCompilation>'
            '</PropertyGroup><ItemGroup><Reference Include="Microsoft.CodeAnalysis"><HintPath>'
            + xml.sax.saxutils.escape(str(roslyn / "Microsoft.CodeAnalysis.dll"))
            + '</HintPath></Reference></ItemGroup></Project>', encoding="utf-8")
        (generator / "Generator.cs").write_text(
            'using Microsoft.CodeAnalysis; [Generator] public sealed class Fixture : ISourceGenerator {'
            ' public void Initialize(GeneratorInitializationContext context) {}'
            ' public void Execute(GeneratorExecutionContext context) { System.IO.File.WriteAllText('
            + json.dumps(str(marker)) + ', "executed"); } }', encoding="utf-8")
        feed = root / "empty-feed"
        feed.mkdir()
        subprocess.run([dotnet, "build", generator / "Generator.csproj", "-m:2", "--disable-build-servers", "-v:q", f"-p:RestoreSources={feed}"],
                       check=True, capture_output=True, text=True, timeout=90)
        dll = generator / "bin/Debug/net10.0/Generator.dll"
        project = root / "project"
        project.mkdir()
        (project / "App.csproj").write_text('<Project><ItemGroup><Analyzer Include="'
            + xml.sax.saxutils.escape(str(dll)) + '" /></ItemGroup></Project>', encoding="utf-8")
        (project / "App.cs").write_text('public class FixtureApp {}', encoding="utf-8")
        output = root / "out.json"
        base = [dotnet, "exec", helper, "--mode", "source", "--root", project, "--out", output]
        captured = subprocess.run(base + ["--captured-sources"], input=json.dumps([
            {"Path": "App.cs", "Text": "public class FixtureApp {}"}]),
            check=True, capture_output=True, text=True, timeout=30)
        assert json.loads(output.read_text())["diagnostics"]["generatorSupport"] == "unknown"
        assert not marker.exists(), captured.stderr
        legacy = subprocess.run(base, check=True, capture_output=True, text=True, timeout=30)
        assert json.loads(output.read_text())["diagnostics"]["generatorSupport"] == "applied", legacy.stderr
        assert marker.read_text() == "executed"
        print("captured generator not executed; legacy synthetic generator executed")


if __name__ == "__main__":
    main()
