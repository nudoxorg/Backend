"""The legacy lane binds package types from each project's restore result.

Usage: python3 restored_packages.py /absolute/path/to/dotnet /absolute/path/to/oracle.dll
The helper must already be published. Packages are simulated with an assembly that
sits beside the helper (Microsoft.CodeAnalysis.dll) placed in a private NuGet folder;
nothing is restored, built, downloaded or executed.
"""
import json
import pathlib
import shutil
import subprocess
import sys
import tempfile

SOURCE = "namespace App;\npublic class Holder { public Microsoft.CodeAnalysis.SyntaxTree Tree = null!; }\n"
PROJECT = (
    '<Project Sdk="Microsoft.NET.Sdk"><PropertyGroup><TargetFramework>net10.0</TargetFramework>'
    "<ImplicitUsings>enable</ImplicitUsings></PropertyGroup>"
    '<ItemGroup><PackageReference Include="Fake.Pkg" Version="1.0.0" /></ItemGroup></Project>'
)


def assets(folder, library_path="fake.pkg/1.0.0", compile_path="lib/net10.0/Microsoft.CodeAnalysis.dll"):
    return json.dumps({
        "version": 3,
        "targets": {"net10.0": {"Fake.Pkg/1.0.0": {"type": "package", "compile": {compile_path: {}}}}},
        "libraries": {"Fake.Pkg/1.0.0": {"type": "package", "path": library_path}},
        "packageFolders": {folder: {}},
        "project": {"frameworks": {"net10.0": {}}},
    })


def run(dotnet, helper, tmp, assets_text):
    root = pathlib.Path(tmp) / "src"
    shutil.rmtree(root, ignore_errors=True)
    (root / "App" / "obj").mkdir(parents=True)
    (root / "App" / "App.csproj").write_text(PROJECT, encoding="utf-8")
    (root / "App" / "Holder.cs").write_text(SOURCE, encoding="utf-8")
    if assets_text is not None:
        (root / "App" / "obj" / "project.assets.json").write_text(assets_text, encoding="utf-8")
    output = pathlib.Path(tmp) / "image.json"
    result = subprocess.run(
        [dotnet, "exec", helper, "--mode", "source", "--root", str(root), "--out", str(output)],
        capture_output=True, text=True, check=True, timeout=120,
    )
    document = json.loads(output.read_text(encoding="utf-8"))
    return document["diagnostics"]["errorCount"], result.stderr


def run_solution(dotnet, helper, tmp, library_assets, app_source):
    """Two sibling projects compiled as one: the package is restored only for `Lib`."""
    root = pathlib.Path(tmp) / "solution"
    shutil.rmtree(root, ignore_errors=True)
    for name in ("Lib", "App"):
        (root / name / "obj").mkdir(parents=True)
        (root / name / f"{name}.csproj").write_text(PROJECT, encoding="utf-8")
    (root / "Lib" / "Lib.cs").write_text("namespace Lib;\npublic class Marker { }\n", encoding="utf-8")
    (root / "App" / "Holder.cs").write_text(app_source, encoding="utf-8")
    (root / "Lib" / "obj" / "project.assets.json").write_text(library_assets, encoding="utf-8")
    (root / "App" / "obj" / "project.assets.json").write_text(library_assets, encoding="utf-8")
    output = pathlib.Path(tmp) / "solution-image.json"
    result = subprocess.run(
        [dotnet, "exec", helper, "--mode", "source", "--root", str(root), "--out", str(output)],
        capture_output=True, text=True, check=True, timeout=120,
    )
    document = json.loads(output.read_text(encoding="utf-8"))
    return document["diagnostics"]["errorCount"], result.stderr


def unresolved(stderr):
    lines = [line for line in stderr.splitlines() if line.startswith("oracle: unresolved-reference: ")]
    assert len(lines) == 1, stderr
    return json.loads(lines[0].split(": ", 2)[2])


def main(dotnet, helper):
    sibling = pathlib.Path(helper).parent / "Microsoft.CodeAnalysis.dll"
    assert sibling.is_file(), "the published helper carries Microsoft.CodeAnalysis.dll"
    with tempfile.TemporaryDirectory(prefix="nudox-roslyn-restore-") as tmp:
        packages = pathlib.Path(tmp) / "packages"
        asset_directory = packages / "fake.pkg" / "1.0.0" / "lib" / "net10.0"
        asset_directory.mkdir(parents=True)
        shutil.copy(sibling, asset_directory / "Microsoft.CodeAnalysis.dll")
        folder = str(packages) + "/"

        # Restored: the type the package supplies binds, with no unresolved reference.
        errors, stderr = run(dotnet, helper, tmp, assets(folder))
        assert errors == 0, stderr
        assert "restored packages: 1 compile assemblies from 1 restored project(s)" in stderr, stderr
        assert "unresolved-reference" not in stderr, stderr

        # Two restored projects compile as one assembly: the package binds in both, the project
        # reference needs nothing, and a type no package supplies is still named with its
        # project reported as restored.
        errors, stderr = run_solution(dotnet, helper, tmp, assets(folder), SOURCE)
        assert errors == 0, stderr
        assert "restored packages: 1 compile assemblies from 2 restored project(s)" in stderr, stderr
        errors, stderr = run_solution(
            dotnet, helper, tmp, assets(folder),
            "namespace App;\npublic class Holder { public Lib.Marker Ok = null!; public Absent.Thing Gone = null!; }\n",
        )
        assert errors == 1, stderr
        found = unresolved(stderr)
        assert found["code"] == "CS0246" and found["symbol"] == "Absent", found
        assert found["path"] == "App/Holder.cs" and found["line"] == 2, found
        assert found["project"] == "App/App.csproj" and found["restore"] == "present", found
        assert found["count"] == 1 and found["files"] == 1, found

        # Not restored: still refused, and the first unresolved reference names the
        # namespace, the file, the project, and that the project needs a restore.
        errors, stderr = run(dotnet, helper, tmp, None)
        assert errors > 0, stderr
        assert "App/App.csproj has no obj/project.assets.json" in stderr, stderr
        found = unresolved(stderr)
        assert found["code"] == "CS0234", found
        assert found["symbol"] == "Microsoft.CodeAnalysis", found
        assert found["path"] == "App/Holder.cs" and found["line"] == 2, found
        assert found["project"] == "App/App.csproj" and found["restore"] == "missing", found

        # Restored on another operating system: the folders are not readable here, so
        # nothing is guessed from them.
        errors, stderr = run(dotnet, helper, tmp, assets("C:\\Users\\dev\\.nuget\\packages\\"))
        assert errors > 0, stderr
        assert "restored on another operating system" in stderr, stderr
        assert unresolved(stderr)["restore"] == "missing"

        # An asset that climbs out of its package folder is never opened.
        errors, stderr = run(dotnet, helper, tmp, assets(folder, library_path="fake.pkg/1.0.0/../../../.."))
        assert errors > 0, stderr
        errors, stderr = run(dotnet, helper, tmp, assets(folder, compile_path="../../../../x/Microsoft.CodeAnalysis.dll"))
        assert errors > 0, stderr

        # A package asset missing from the folder is named, not skipped silently.
        shutil.rmtree(packages / "fake.pkg")
        errors, stderr = run(dotnet, helper, tmp, assets(folder))
        assert errors > 0, stderr
        assert "package Fake.Pkg/1.0.0 asset lib/net10.0/Microsoft.CodeAnalysis.dll is not in the NuGet package folder" in stderr, stderr


if __name__ == "__main__":
    main(sys.argv[1], sys.argv[2])
    print("restored package binding, unresolved-reference summary and restore refusals passed")
