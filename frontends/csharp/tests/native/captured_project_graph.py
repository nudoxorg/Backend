"""Exercise separate captured project compilations with a real Roslyn helper.

Usage: python3 captured_project_graph.py /absolute/dotnet /absolute/oracle.dll
No project builds, MSBuild evaluation, generators, or user code run.
"""
import copy
import hashlib
import json
import pathlib
import subprocess
import sys
import tempfile


def main():
    dotnet, helper = sys.argv[1:]
    runtime = subprocess.run([dotnet, "--list-runtimes"], check=True, capture_output=True,
                             text=True, timeout=30).stdout.splitlines()
    line = next(line for line in runtime if line.startswith("Microsoft.NETCore.App "))
    _, version, directory = line.split(maxsplit=2)
    refs = str(pathlib.Path(directory.strip("[]")) / version)
    with tempfile.TemporaryDirectory(prefix="nudox-csharp-graph-") as directory:
        root = pathlib.Path(directory)
        target = '\ufeff// π\npublic static class Target { public static void Go() {} }\n'
        caller = 'public class Caller { public void Run() { Target.Go(); } }\n'

        def node(name, source, dependencies):
            return {"ProjectPath": f"{name}/{name}.csproj", "AssemblyName": name,
                    "TargetFramework": "net8.0", "LanguageVersion": "12.0", "DefineSymbols": [],
                    "Nullable": "enable", "AllowUnsafe": False, "OutputType": "Library", "Usings": [],
                    "Sources": [{"Path": f"{name}/Source.cs", "Text": source}],
                    "Config": [{"Path": f"{name}/{name}.csproj", "Text": "<Project/>"},
                               {"Path": f"{name}/Directory.Build.props", "Text": None},
                               {"Path": f"{name}/Directory.Build.targets", "Text": None},
                               {"Path": "Directory.Build.props", "Text": None},
                               {"Path": "Directory.Build.targets", "Text": None}],
                    "ReferenceDirectories": [refs], "ProjectReferences": dependencies}

        graph = {"Format": 1, "RootProject": "Caller/Caller.csproj", "Projects": [
            node("Caller", caller, ["Target/Target.csproj"]), node("Target", target, [])]}
        manifest, evidence = root / "graph.json", root / "evidence.json"
        command = [dotnet, "exec", helper, "--mode", "source", "--root", str(root),
                   "--project-file", str(root / graph["RootProject"]),
                   "--captured-project-graph-file", str(manifest), "--authority-image",
                   "--source-binding", str(root / "Caller/Source.cs"), "--out", str(root / "image"),
                   "--capture-evidence-file", str(evidence), "--capture-evidence-nonce", "a" * 32]

        def run(value, success=True):
            evidence.unlink(missing_ok=True)
            manifest.write_text(json.dumps(value), encoding="utf-8")
            result = subprocess.run(command, capture_output=True, text=True, timeout=30)
            assert (result.returncode == 0) == success, result.stderr
            return json.loads(evidence.read_text()) if success else result.stderr

        receipt = run(graph)
        assert receipt["errors"] == 0, receipt
        assert receipt["assembly"] == "Caller" and len(receipt["sources"]) == 2
        assert receipt["configSha256"] == hashlib.sha256(manifest.read_bytes()).hexdigest()
        assert len(receipt["occurrences"]) == 1, receipt
        coordinate = receipt["occurrences"][0]["target"].split("|")
        assert bytes.fromhex(coordinate[1]).decode() == "Target/Source.cs"
        assert coordinate[2] == hashlib.sha256(target.encode()).hexdigest()
        start, end = int(coordinate[5]), int(coordinate[6])
        assert target.encode()[start:end] == b"Go"

        # Internal members cannot become visible merely because both projects
        # have captured sources: separate assembly boundaries must survive.
        inaccessible = copy.deepcopy(graph)
        inaccessible["Projects"][1]["Sources"][0]["Text"] = target.replace("public static void", "internal static void")
        assert run(inaccessible)["errors"] > 0

        # Identically named internal types in distinct projects must not collide.
        separate = copy.deepcopy(graph)
        for project in separate["Projects"]:
            project["Sources"][0]["Text"] += "\ninternal class PrivateType {}\n"
        assert run(separate)["errors"] == 0

        # Per-project preprocessor symbols and nullable/options stay scoped.
        scoped = copy.deepcopy(graph)
        scoped["Projects"][1]["DefineSymbols"] = ["DEPENDENCY"]
        scoped["Projects"][1]["Sources"][0]["Text"] = "#if DEPENDENCY\n" + target.lstrip('\ufeff') + "#endif\n"
        scoped["Projects"][0]["Sources"][0]["Text"] += "\n#if DEPENDENCY\n#error leaked symbol\n#endif\n"
        assert run(scoped)["errors"] == 0

        for mutate in (
            lambda value: value["Projects"][1]["ProjectReferences"].append("Caller/Caller.csproj"),
            lambda value: value["Projects"][0]["ProjectReferences"].append("Missing/Missing.csproj"),
            lambda value: value["Projects"][1].update(ProjectPath="../Escape.csproj"),
            lambda value: value["Projects"][1]["Sources"][0].update(Path="Caller/Foreign.cs"),
            lambda value: value["Projects"].append(node("Unused", "public class Unused {}", [])),
            lambda value: value["Projects"].extend([value["Projects"][1]] * 64),
            lambda value: value.update(Unexpected=True),
        ):
            rejected = copy.deepcopy(graph)
            mutate(rejected)
            run(rejected, success=False)
        print("captured project graph: separate assemblies, source targets, options, and rejection bounds passed")


if __name__ == "__main__":
    main()
