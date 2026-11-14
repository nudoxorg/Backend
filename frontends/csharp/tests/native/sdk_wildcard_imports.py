"""Prove SDK wildcard import switches using private injection directories only.

Usage: python3 sdk_wildcard_imports.py /absolute/dotnet /absolute/sdk-version-dir
"""
import json
import pathlib
import subprocess
import sys
import tempfile


def main():
    dotnet, sdk = map(pathlib.Path, sys.argv[1:])
    cases = [("MicrosoftCommonProps", "Current/Microsoft.Common.props", "Imports/Microsoft.Common.props"),
             ("MicrosoftCommonTargets", "Microsoft.Common.CurrentVersion.targets", "Microsoft.Common.targets"),
             ("MicrosoftCSharpTargets", "Microsoft.CSharp.CurrentVersion.targets", "Microsoft.CSharp.targets")]
    with tempfile.TemporaryDirectory(prefix="nudox-sdk-wildcard-") as tmp:
        root = pathlib.Path(tmp)
        env = {"HOME": tmp, "DOTNET_CLI_HOME": tmp, "DOTNET_NOLOGO": "1", "DOTNET_CLI_TELEMETRY_OPTOUT": "1"}
        for suffix, imported, subdir in cases:
            case_root = root / suffix
            case_root.mkdir()
            project = case_root / "test.proj"
            project.write_text(f'<Project><Import Project="{sdk / imported}"/></Project>')
            for location, kind in [("ext", ""), ("user", "User")]:
                for direction in ("Before", "After"):
                    folder = case_root / location / "Current" / subdir / f"Import{direction}"
                    folder.mkdir(parents=True, exist_ok=True)
                    (folder / "probe.targets").write_text(f'<Project><PropertyGroup><{kind}{direction}Marker>injected</{kind}{direction}Marker></PropertyGroup></Project>')
            props = ["BeforeMarker", "AfterMarker", "UserBeforeMarker", "UserAfterMarker"]
            command = [str(dotnet), "msbuild", str(project), "-nologo", f"-p:MSBuildExtensionsPath={case_root / 'ext'}",
                       f"-p:MSBuildUserExtensionsPath={case_root / 'user'}", "-getProperty:" + ",".join(props)]
            enabled = subprocess.run(command, cwd=root, env=env, capture_output=True, text=True, check=True, timeout=30)
            assert all(json.loads(enabled.stdout)["Properties"][key] == "injected" for key in props), suffix
            flags = [f"-p:{kind}{direction}{suffix}=false" for kind in ("ImportByWildcard", "ImportUserLocationsByWildcard") for direction in ("Before", "After")]
            disabled = subprocess.run(command + flags, cwd=root, env=env, capture_output=True, text=True, check=True, timeout=30)
            assert all(json.loads(disabled.stdout)["Properties"][key] == "" for key in props), suffix
        print("all 12 SDK wildcard import hooks blocked in private positive/negative controls")


if __name__ == "__main__":
    main()
