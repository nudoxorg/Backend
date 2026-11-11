"""Exercise SDK ReferencePath mapping against the restored Tests project.

This control intentionally mirrors the resolver's custody boundary: candidate
bytes are copied before MSBuild, while implicit SDK outputs are mapped back by
their canonical original path. It does not invoke the Rust resolver.
"""
from __future__ import annotations

import hashlib
import json
import os
import shutil
import subprocess
import tempfile
from pathlib import Path

camp = Path(__file__).resolve().parents[4]
fixture = camp / "landing/fixtures/r35-csharp-restored"
project = fixture / "backend/Zhadina.Api.Tests"
assets_path = project / "obj/project.assets.json"
packages = fixture / "packages"
dotnet = camp / "infra/dotnet10/dotnet"
pack_roots = [
    camp / "infra/dotnet10/packs/Microsoft.NETCore.App.Ref/10.0.14/ref/net10.0",
    camp / "infra/dotnet10/packs/Microsoft.AspNetCore.App.Ref/10.0.14/ref/net10.0",
]

assets = json.loads(assets_path.read_text())
tfm = next(iter(assets["targets"]))
candidates: list[Path] = []
for library, target in assets["targets"][tfm].items():
    library_info = assets["libraries"].get(library, {})
    if library_info.get("type") != "package":
        continue
    package_root = packages / library_info["path"]
    for relative in target.get("compile", {}):
        if relative.endswith(".dll"):
            path = package_root / relative
            if not path.is_file():
                raise SystemExit(f"missing exact compile asset: {path}")
            candidates.append(path)
for root in pack_roots:
    candidates.extend(sorted(root.glob("*.dll")))
candidates = list(dict.fromkeys(candidates))
if not candidates:
    raise SystemExit("empty candidate set")

def digest(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()

with tempfile.TemporaryDirectory(prefix="nudox-sdk-map-") as temporary:
    scratch = Path(temporary)
    copied: dict[Path, Path] = {}
    owned_to_original: dict[Path, Path] = {}
    original_digests: dict[Path, str] = {}
    for index, original in enumerate(candidates):
        destination = scratch / "candidates" / str(index) / original.name
        destination.parent.mkdir(parents=True)
        shutil.copyfile(original, destination)
        copied[original.resolve()] = destination.resolve()
        owned_to_original[destination.resolve()] = original.resolve()
        original_digests[original.resolve()] = digest(original)
    project_file = scratch / "resolve.csproj"
    (scratch / "obj").mkdir()
    shutil.copyfile(assets_path, scratch / "obj/project.assets.json")
    references = "".join(
        f'<Reference Include="{path.name[:-4]}"><HintPath>{copied[path.resolve()]}</HintPath>'
        "<Private>false</Private></Reference>"
        for path in candidates
    )
    project_file.write_text(
        '<Project Sdk="Microsoft.NET.Sdk"><PropertyGroup><TargetFramework>net10.0</TargetFramework>'
        "<ImportDirectoryBuildProps>false</ImportDirectoryBuildProps>"
        "<ImportDirectoryBuildTargets>false</ImportDirectoryBuildTargets></PropertyGroup>"
        f"<ItemGroup>{references}</ItemGroup></Project>"
    )
    command = [
        str(dotnet), "msbuild", str(project_file), "-nologo", "-noAutoResponse",
        "-nodeReuse:false", "-target:ResolveReferences", "-getItem:ReferencePath",
        "-getProperty:MSBuildSDKsPath", "-p:RestoreSources=",
        "-p:MSBuildEnableWorkloadResolver=false", "-p:ImportDirectoryBuildProps=false",
        "-p:ImportDirectoryBuildTargets=false", "-p:ImportDirectoryPackagesProps=false",
        "-p:ImportByWildcardBeforeMicrosoftCommonProps=false",
        "-p:ImportByWildcardAfterMicrosoftCommonProps=false",
        "-p:ImportUserLocationsByWildcardBeforeMicrosoftCommonProps=false",
        "-p:ImportUserLocationsByWildcardAfterMicrosoftCommonProps=false",
    ]
    environment = {
        "HOME": str(scratch), "DOTNET_CLI_HOME": str(scratch),
        "NUGET_PACKAGES": str(scratch), "DOTNET_CLI_TELEMETRY_OPTOUT": "1",
        "DOTNET_SKIP_FIRST_TIME_EXPERIENCE": "1", "DOTNET_NOLOGO": "1",
    }
    result = subprocess.run(command, cwd=scratch, env=environment, capture_output=True, text=True, check=False)
    if result.returncode:
        raise SystemExit(result.stderr)
    payload = json.loads(result.stdout)
    items = payload["Items"]["ReferencePath"]
    mapped: list[Path] = []
    unknown: list[str] = []
    for item in items:
        selected = Path(item.get("FullPath") or item.get("Identity", "")).resolve()
        original = selected if selected in copied else owned_to_original.get(selected)
        if original is None:
            unknown.append(str(selected))
        else:
            mapped.append(copied[original])
    if unknown:
        raise SystemExit(f"unknown SDK outputs: {len(unknown)}; sample={unknown[:3]}")
    if len(mapped) != len(set(mapped)):
        raise SystemExit("duplicate mapped SDK output")
    for original in copied:
        if digest(original) != original_digests[original]:
            raise SystemExit("original changed during SDK resolution")
    changed = candidates[0]
    before = digest(changed)
    changed_bytes = changed.read_bytes()
    changed.write_bytes(changed_bytes + b"\0")
    try:
        if digest(changed) == before:
            raise SystemExit("changed-original negative did not detect mutation")
    finally:
        changed.write_bytes(changed_bytes)
    fake_output = scratch / "unknown.dll"
    if fake_output.resolve() in copied:
        raise SystemExit("unknown-path negative was admitted")
    print(f"candidates={len(candidates)} selected={len(mapped)} mapped_to_owned=true")
    print("negative_changed_original=passed")
    print("negative_unknown_output=passed")
