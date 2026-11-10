"""Exercise installed SDK discovery plus Roslyn on private synthetic Web code.

Usage: python3 sdk_discovery.py /absolute/dotnet /absolute/oracle.dll
This does not evaluate a user project or execute fixture code.
"""
import json
import os
import pathlib
import subprocess
import sys
import tempfile


def main():
    dotnet, helper = map(pathlib.Path, sys.argv[1:])
    with tempfile.TemporaryDirectory(prefix="nudox-sdk-discovery-") as tmp:
        root = pathlib.Path(tmp)
        project = root / "App.csproj"
        project.write_text('<Project Sdk="Microsoft.NET.Sdk.Web"><PropertyGroup><TargetFramework>net10.0</TargetFramework><ImplicitUsings>enable</ImplicitUsings><ImportDirectoryBuildProps>false</ImportDirectoryBuildProps><ImportDirectoryBuildTargets>false</ImportDirectoryBuildTargets></PropertyGroup></Project>')
        (root / "global.json").write_text('{}')
        # Hostile ancestor props must not enter the generated SDK project.
        (root / "Directory.Build.props").write_text('<Project><ItemGroup><Using Include="Nudox.Forbidden" /></ItemGroup></Project>')
        (root / "Directory.Packages.props").write_text('<Project><ItemGroup><Using Include="Nudox.ForbiddenPackage" /></ItemGroup></Project>')
        (root / "Directory.Build.rsp").write_text('-p:ImplicitUsings=disable\n')
        env = {"HOME": tmp, "DOTNET_CLI_HOME": tmp, "NUGET_PACKAGES": tmp,
               "DOTNET_CLI_TELEMETRY_OPTOUT": "1", "DOTNET_SKIP_FIRST_TIME_EXPERIENCE": "1", "DOTNET_NOLOGO": "1"}
        command = [str(dotnet), "msbuild", str(project), "-nologo", "-noAutoResponse", "-nodeReuse:false", "-maxcpucount:2", "-target:ProcessFrameworkReferences,AddImplicitDefineConstants",
                   "-getProperty:MSBuildSDKsPath,NETCoreTargetingPackRoot,TargetFramework,DefineConstants,ImplicitUsings,Nullable,AllowUnsafeBlocks,LangVersion,OutputType", "-getItem:TargetingPack,Using",
                   "-p:RestoreSources=", "-p:MSBuildEnableWorkloadResolver=false",
                   "-p:ImportDirectoryBuildProps=false", "-p:ImportDirectoryBuildTargets=false",
                   "-p:ImportDirectoryPackagesProps=false",
                   "-p:ImportByWildcardBeforeMicrosoftCommonProps=false", "-p:ImportByWildcardAfterMicrosoftCommonProps=false", "-p:ImportUserLocationsByWildcardBeforeMicrosoftCommonProps=false", "-p:ImportUserLocationsByWildcardAfterMicrosoftCommonProps=false", "-p:ImportByWildcardBeforeMicrosoftCommonTargets=false", "-p:ImportByWildcardAfterMicrosoftCommonTargets=false", "-p:ImportUserLocationsByWildcardBeforeMicrosoftCommonTargets=false", "-p:ImportUserLocationsByWildcardAfterMicrosoftCommonTargets=false", "-p:ImportByWildcardBeforeMicrosoftCSharpTargets=false", "-p:ImportByWildcardAfterMicrosoftCSharpTargets=false", "-p:ImportUserLocationsByWildcardBeforeMicrosoftCSharpTargets=false", "-p:ImportUserLocationsByWildcardAfterMicrosoftCSharpTargets=false",
                   "-p:ImportByWildcardBeforeMicrosoftCommonCrossTargetingTargets=false", "-p:ImportByWildcardAfterMicrosoftCommonCrossTargetingTargets=false", "-p:ImportUserLocationsByWildcardBeforeMicrosoftCommonCrossTargetingTargets=false", "-p:ImportUserLocationsByWildcardAfterMicrosoftCommonCrossTargetingTargets=false", "-p:ImportByWildcardBeforeMicrosoftNetFrameworkProps=false", "-p:ImportByWildcardAfterMicrosoftNetFrameworkProps=false", "-p:ImportUserLocationsByWildcardBeforeMicrosoftNetFrameworkProps=false", "-p:ImportUserLocationsByWildcardAfterMicrosoftNetFrameworkProps=false", "-p:ImportByWildcardBeforeMicrosoftNetFrameworkTargets=false", "-p:ImportByWildcardAfterMicrosoftNetFrameworkTargets=false", "-p:ImportUserLocationsByWildcardBeforeMicrosoftNetFrameworkTargets=false", "-p:ImportUserLocationsByWildcardAfterMicrosoftNetFrameworkTargets=false", "-p:ImportByWildcardBeforeMicrosoftVisualBasicTargets=false", "-p:ImportByWildcardAfterMicrosoftVisualBasicTargets=false", "-p:ImportUserLocationsByWildcardBeforeMicrosoftVisualBasicTargets=false", "-p:ImportUserLocationsByWildcardAfterMicrosoftVisualBasicTargets=false"]
        result = subprocess.run(command, cwd=root, env=env, capture_output=True, text=True, check=True, timeout=30)
        observation = json.loads(result.stdout)
        assert observation["Properties"]["TargetFramework"] == "net10.0"
        assert observation["Properties"]["ImplicitUsings"] == "enable"
        injected = subprocess.run([arg for arg in command if arg != "-noAutoResponse"], cwd=root, env=env, capture_output=True, text=True, check=True, timeout=30)
        assert json.loads(injected.stdout)["Properties"]["ImplicitUsings"] == "disable"
        pack_root = pathlib.Path(observation["Properties"]["NETCoreTargetingPackRoot"]).resolve()
        packs = observation["Items"]["TargetingPack"]
        assert {item["NuGetPackageId"] for item in packs} == {"Microsoft.NETCore.App.Ref", "Microsoft.AspNetCore.App.Ref"}
        directories = []
        for item in packs:
            pack = pathlib.Path(item["Path"]).resolve()
            assert pack.is_relative_to(pack_root) and item["TargetFramework"] == "net10.0"
            refs = list((pack / "ref").iterdir())
            assert len(refs) == 1 and refs[0].is_dir()
            directories.append(refs[0])
        usings = [item["Identity"] for item in observation["Items"]["Using"]]
        assert "Microsoft.AspNetCore.Builder" in usings
        assert "Nudox.Forbidden" not in usings and "Nudox.ForbiddenPackage" not in usings
        defines = observation["Properties"]["DefineConstants"].split(";")
        assert "NET10_0" in defines
        assert "DEBUG" in defines and "RELEASE" not in defines
        assert observation["Properties"]["Nullable"] == ""
        assert observation["Properties"]["AllowUnsafeBlocks"] == "false"
        assert observation["Properties"]["LangVersion"] == "14.0"
        assert observation["Properties"]["OutputType"] == "Exe"
        source = "#if RELEASE\nMissingReleaseDefine();\n#endif\n#if NET10_0 && DEBUG\nvar builder = WebApplication.CreateBuilder(args);\nvar app = builder.Build();\napp.Run();\n#else\nMissingSdkDefine();\n#endif\npublic partial class Program;\n"
        out = root / "oracle.json"
        captured_config = root / "captured-config.json"
        captured_config.write_text(json.dumps([
            {"Path": "App.csproj", "Text": project.read_text()},
            {"Path": "Directory.Build.props", "Text": None},
            {"Path": "Directory.Build.targets", "Text": None},
        ]))
        cmd = [str(dotnet), "exec", str(helper), "--mode", "source", "--root", tmp, "--captured-sources",
               "--project-file", str(project), "--captured-config-file", str(captured_config), "--exact-defines", "--target-framework", "net10.0", "--exact-nullable", "disable", "--exact-unsafe", "false", "--exact-output-type", "Exe", "--out", str(out)]
        for directory in directories:
            cmd.extend(["--ref-dir", str(directory)])
        for namespace in usings:
            cmd.extend(["--using", namespace])
        for symbol in defines:
            cmd.extend(["--define", symbol])
        compiled = subprocess.run(cmd, input=json.dumps([{"Path":"Program.cs", "Text":source}]), capture_output=True, text=True, check=True, timeout=30)
        assert json.loads(out.read_text())["diagnostics"]["errorCount"] == 0, compiled.stderr
        assert json.loads(out.read_text())["assembly"]["tfm"] == "net10.0"
        target_probe = cmd.copy()
        target_probe[target_probe.index("--target-framework") + 1] = "net9.0"
        subprocess.run(target_probe, input=json.dumps([{"Path":"Program.cs", "Text":source}]), capture_output=True, text=True, check=True, timeout=30)
        assert json.loads(out.read_text())["assembly"]["tfm"] == "net9.0"
        no_entry = json.dumps([{"Path":"Program.cs", "Text":"public class NoEntry {}"}])
        subprocess.run(cmd, input=no_entry, capture_output=True, text=True, check=True, timeout=30)
        assert json.loads(out.read_text())["diagnostics"]["errorCount"] > 0
        no_output_override = cmd.copy()
        index = no_output_override.index("--exact-output-type")
        del no_output_override[index:index+2]
        subprocess.run(no_output_override, input=no_entry, capture_output=True, text=True, check=True, timeout=30)
        assert json.loads(out.read_text())["diagnostics"]["errorCount"] == 0
        unsafe_source = json.dumps([{"Path":"Program.cs", "Text":"public unsafe class UnsafeClass {}"}])
        library_cmd = cmd.copy()
        library_cmd[library_cmd.index("--exact-output-type") + 1] = "Library"
        subprocess.run(library_cmd, input=unsafe_source, capture_output=True, text=True, check=True, timeout=30)
        assert json.loads(out.read_text())["diagnostics"]["errorCount"] > 0
        legacy_unsafe = library_cmd.copy()
        for flag in ("--exact-nullable", "--exact-unsafe"):
            index = legacy_unsafe.index(flag)
            del legacy_unsafe[index:index+2]
        subprocess.run(legacy_unsafe, input=unsafe_source, capture_output=True, text=True, check=True, timeout=30)
        assert json.loads(out.read_text())["diagnostics"]["errorCount"] == 0
        nullable_source = json.dumps([{"Path":"Program.cs", "Text":"public class Sample { public string Name { get; set; } }"}])
        subprocess.run(library_cmd, input=nullable_source, capture_output=True, text=True, check=True, timeout=30)
        def nullability():
            return json.loads(out.read_text())["types"][0]["members"]["properties"][0]["type"]["nullable"]
        assert nullability() == "none"
        subprocess.run(legacy_unsafe, input=nullable_source, capture_output=True, text=True, check=True, timeout=30)
        assert nullability() == "notAnnotated"
        unbound = subprocess.run([str(dotnet), "exec", str(helper), "--mode", "source", "--root", tmp,
                                  "--captured-sources", "--exact-defines", "--out", str(out)],
                                 input=json.dumps([{"Path":"Program.cs", "Text":source}]), capture_output=True, text=True, timeout=30)
        assert unbound.returncode != 0
        fallback = subprocess.run([arg for arg in cmd if arg != "--exact-defines"], input=json.dumps([{"Path":"Program.cs", "Text":source}]), capture_output=True, text=True, check=True, timeout=30)
        assert json.loads(out.read_text())["diagnostics"]["errorCount"] > 0, fallback.stderr
        (root / "global.json").write_text('{"sdk":{"version":"9999.0.100","rollForward":"disable"}}')
        missing = subprocess.run(command, cwd=root, env=env, capture_output=True, text=True, timeout=30)
        assert missing.returncode != 0
        print("native SDK-selected Web packs/usings compile captured source; unavailable SDK refuses")


if __name__ == "__main__":
    main()
