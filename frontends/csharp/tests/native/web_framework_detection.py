"""The legacy lane chooses the ASP.NET Core shared framework from the project files.

Usage: python3 web_framework_detection.py /absolute/path/to/dotnet /absolute/path/to/oracle.dll
The helper must already be published and the dotnet host must have the ASP.NET Core
shared framework installed beside its base runtime; the fixtures are never built or run.
"""
import json
import pathlib
import subprocess
import sys
import tempfile

PROGRAM = (
    "var builder = WebApplication.CreateBuilder(args);\n"
    "var app = builder.Build();\n"
    "app.MapGet(\"/\", () => \"ok\");\n"
    "app.Run();\n"
)


def error_count(dotnet, helper, project_xml, sources):
    with tempfile.TemporaryDirectory(prefix="nudox-roslyn-web-") as tmp:
        root = pathlib.Path(tmp)
        (root / "App.csproj").write_text(project_xml, encoding="utf-8")
        for name, text in sources.items():
            (root / name).write_text(text, encoding="utf-8")
        output = root / "image.json"
        result = subprocess.run(
            [dotnet, "exec", helper, "--mode", "source", "--root", str(root), "--out", str(output)],
            capture_output=True, text=True, check=True, timeout=60,
        )
        document = json.loads(output.read_text(encoding="utf-8"))
        return document["diagnostics"]["errorCount"], result.stderr


def main(dotnet, helper):
    web = (
        '<Project Sdk="Microsoft.NET.Sdk.Web"><PropertyGroup>'
        "<TargetFramework>net10.0</TargetFramework><ImplicitUsings>enable</ImplicitUsings>"
        "</PropertyGroup></Project>"
    )
    library = (
        '<Project Sdk="Microsoft.NET.Sdk"><PropertyGroup>'
        "<TargetFramework>net10.0</TargetFramework><ImplicitUsings>enable</ImplicitUsings>"
        "</PropertyGroup></Project>"
    )
    errors, stderr = error_count(dotnet, helper, web, {"Program.cs": PROGRAM})
    assert errors == 0, stderr
    assert "Microsoft.AspNetCore.App" in stderr, stderr

    # The same source in a project that does not ask for the framework stays unresolved:
    # the framework is chosen from the project files, never assumed.
    errors, stderr = error_count(dotnet, helper, library, {"Program.cs": PROGRAM})
    assert errors > 0, stderr
    assert "Microsoft.AspNetCore.App" not in stderr, stderr

    # An explicit framework reference selects it as well.
    explicit = library.replace(
        "</Project>", '<ItemGroup><FrameworkReference Include="Microsoft.AspNetCore.App" /></ItemGroup></Project>'
    )
    errors, stderr = error_count(dotnet, helper, explicit, {"Program.cs": PROGRAM.replace("WebApplication", "Microsoft.AspNetCore.Builder.WebApplication")})
    assert errors == 0, stderr


if __name__ == "__main__":
    main(sys.argv[1], sys.argv[2])
