#!/usr/bin/env python3
"""Package a hash-bound matched development build for macOS installation QA.

This is explicitly a preview, not the signed investor release pipeline. It uses
the release packager's Mach-O auditor and preserves source and build evidence.
"""
from __future__ import annotations

import argparse
import importlib.util
import json
import os
from pathlib import Path
import plistlib
import shutil
import subprocess

SPEC = importlib.util.spec_from_file_location(
    "investor_bundle", Path(__file__).with_name("macos-investor-bundle.py")
)
assert SPEC is not None and SPEC.loader is not None
bundle = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(bundle)
NAMES = ("backend-desktop", "backend-cli", "backend-mcp", "backend-locald")


def load_edges(image: Path) -> tuple[list[str], str | None]:
    """Read actual dylib loads; LC_RPATH entries are resolution metadata."""
    _, identity = bundle.inspect_load_metadata(image)
    lines = bundle.run(["otool", "-L", str(image)]).splitlines()[1:]
    return [line.strip().split(" (compatibility version", 1)[0]
            for line in lines if line.strip()], identity


def matched_build(source: Path, receipt: dict) -> dict[str, Path]:
    """Admit only one unchanged, clean, successful four-program invocation."""
    bundle.validate_source(source, receipt["source"], receipt["tree"])
    if receipt.get("exit") != 0 or receipt.get("source_unchanged") is not True:
        bundle.fail("application build did not succeed on unchanged source")
    command = receipt.get("command", [])
    packages = {command[i + 1] for i, word in enumerate(command[:-1]) if word == "-p"}
    if "build" not in command or "--locked" not in command or not set(NAMES) <= packages:
        bundle.fail("receipt is not a locked matched four-program build")
    runner = Path(command[1])
    if bundle.sha256(runner) != receipt["runner_sha256"]:
        bundle.fail("build runner changed since recorded invocation")
    records = dict(receipt.get("executables", {}))
    records["backend-desktop"] = receipt["gui"]
    result = {}
    for name in NAMES:
        record = records[name]
        path = Path(record["path"]).resolve(strict=True)
        if path != (source / ".local/target/debug" / name).resolve(strict=True):
            bundle.fail(f"artifact outside recorded source output: {name}")
        if bundle.sha256(path) != record["sha256"]:
            bundle.fail(f"artifact differs from build receipt: {name}")
        result[name] = path
    return result


def relocate(app: Path, inputs: dict[str, Path]) -> list[dict]:
    """Copy exact non-system load closure, then rewrite only staged images."""
    frameworks = app / "Contents/Frameworks"
    frameworks.mkdir()
    staged = {path: app / "Contents/MacOS" / name for name, path in inputs.items()}
    hashes: dict[str, str] = {}
    pending = list(staged)
    origins = []
    while pending:
        original = pending.pop()
        dependencies, identity = load_edges(original)
        for dependency in dependencies:
            if dependency == identity or bundle.is_system_path(dependency):
                continue
            if not dependency.startswith("/"):
                bundle.fail(f"unsupported unresolved input load: {original}: {dependency}")
            path = Path(dependency).resolve(strict=True)
            digest = bundle.sha256(path)
            if path.name in hashes and hashes[path.name] != digest:
                bundle.fail(f"different dependencies share a filename: {path.name}")
            if path not in staged:
                hashes[path.name] = digest
                destination = frameworks / path.name
                shutil.copy2(path, destination)
                destination.chmod(0o755)
                staged[path] = destination
                pending.append(path)
                origins.append({"path": str(path), "sha256": digest,
                                "staged": destination.relative_to(app).as_posix()})
    for original, image in staged.items():
        dependencies, identity = load_edges(original)
        if identity is not None:
            bundle.run(["install_name_tool", "-id", "@loader_path/" + image.name, str(image)])
        for dependency in dependencies:
            if dependency == identity or bundle.is_system_path(dependency):
                continue
            target = staged[Path(dependency).resolve(strict=True)]
            relative = os.path.relpath(target, image.parent)
            bundle.run(["install_name_tool", "-change", dependency,
                        "@loader_path/" + relative, str(image)])
        raw = bundle.run(["otool", "-l", str(image)]).splitlines()
        for i, line in enumerate(raw):
            if line.strip() == "cmd LC_RPATH":
                value = raw[i + 2].strip().removeprefix("path ").rsplit(" (offset", 1)[0]
                if value.startswith("/") and not bundle.is_system_path(value):
                    bundle.run(["install_name_tool", "-delete_rpath", value, str(image)])
        bundle.run(["codesign", "--force", "--sign", "-", str(image)])
    return origins


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source-root", required=True, type=Path)
    parser.add_argument("--build-receipt", required=True, type=Path)
    parser.add_argument("--output-dir", required=True, type=Path)
    args = parser.parse_args()
    source = args.source_root.resolve(strict=True)
    receipt = bundle.load_json(args.build_receipt, "matched build")
    inputs = matched_build(source, receipt)
    bundle.macos_tools()
    output = args.output_dir.resolve()
    output.mkdir(mode=0o700, parents=False, exist_ok=False)
    payload = output / "payload"
    macos = payload / "Nudox.app/Contents/MacOS"
    resources = payload / "Nudox.app/Contents/Resources"
    macos.mkdir(parents=True)
    resources.mkdir()
    app = payload / "Nudox.app"
    for name, path in inputs.items():
        shutil.copy2(path, macos / name)
    plist = plistlib.loads((source / "apps/desktop/macos/Info.plist").read_bytes())
    plist["CFBundleExecutable"] = "backend-desktop"
    minimum = max((bundle.minimum_macos(path) for path in inputs.values()), key=bundle.version_tuple)
    plist["LSMinimumSystemVersion"] = minimum
    plist["CFBundleVersion"] = receipt["source"][:12]
    (app / "Contents/Info.plist").write_bytes(plistlib.dumps(plist))
    shutil.copy2(args.build_receipt, resources / "application-build-receipt.json")
    for notice in source.glob("apps/**/resources/fonts/*OFL*"):
        shutil.copy2(notice, resources / notice.name)
    for notice in source.glob("LICENSE*"):
        if notice.is_file():
            shutil.copy2(notice, resources / notice.name)
    origins = relocate(app, inputs)
    images = bundle.inspect_macho_tree(app, "arm64", minimum,
                                     {"Contents/MacOS/" + name for name in NAMES})
    manifest = {"schema": 1, "kind": "macos-preview", "source": receipt["source"],
                "tree": receipt["tree"], "build": receipt, "dependency_origins": origins,
                "pre_bundle_sign_images": images, "signing": "ad-hoc", "notarized": False,
                "native_acceptance": "pending", "compiler_helpers": "host prerequisites"}
    (resources / "preview-manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
    bundle.run(["codesign", "--force", "--sign", "-", str(app)])
    bundle.run(["codesign", "--verify", "--deep", "--strict", str(app)])
    manifest["images"] = bundle.inspect_macho_tree(
        app, "arm64", minimum, {"Contents/MacOS/" + name for name in NAMES}
    )
    os.symlink("/Applications", payload / "Applications")
    installer = payload / "Install command-line tools.command"
    installer.write_text('''#!/bin/sh
set -eu
app="/Applications/Nudox.app"
if [ ! -x "$app/Contents/MacOS/backend-cli" ]; then
  app="$HOME/Applications/Nudox.app"
fi
if [ ! -x "$app/Contents/MacOS/backend-cli" ]; then
  printf '%s\\n' 'Copy Nudox.app to Applications before installing command-line tools.'
  exit 1
fi
destination="$HOME/.local/bin"
mkdir -p "$destination"
for pair in nudox:backend-cli nudox-mcp:backend-mcp nudox-locald:backend-locald; do
  name=${pair%%:*}
  binary=${pair#*:}
  target="$app/Contents/MacOS/$binary"
  if [ -e "$destination/$name" ] || [ -L "$destination/$name" ]; then
    if [ "$(readlink "$destination/$name" || :)" != "$target" ]; then
      printf '%s\\n' "Existing $destination/$name was preserved; install that link manually."
      exit 1
    fi
  else
    ln -s "$target" "$destination/$name"
  fi
done
printf '%s\\n' 'Installed nudox, nudox-mcp, and nudox-locald in ~/.local/bin.' 'Use nudox --help and configure your MCP client with the absolute nudox-mcp path.'
''')
    installer.chmod(0o755)
    (payload / "Read me.txt").write_text(
        "Nudox preview for Apple Silicon\n\n"
        "Drag Nudox.app to Applications. Optional: double-click Install command-line tools.command.\n"
        "The app includes matched GUI, CLI, MCP, and local daemon binaries. No Nix installation is needed to launch.\n"
        "Language compilation requires the appropriate installed compiler/toolchain.\n"
        "This preview is ad-hoc signed, not notarized; it is not an accepted investor release.\n"
        "Known GUI/index reliability gaps are still under repair.\n"
        f"Source: {receipt['source']}\n")
    dmg = output / "Nudox-preview-arm64.dmg"
    bundle.run(["hdiutil", "create", "-volname", "Nudox Preview", "-srcfolder", str(payload),
                "-format", "UDZO", "-ov", str(dmg)])
    bundle.run(["hdiutil", "verify", str(dmg)])
    manifest["dmg"] = {"path": dmg.name, "sha256": bundle.sha256(dmg), "bytes": dmg.stat().st_size}
    (output / "receipt.json").write_text(json.dumps(manifest, indent=2) + "\n")
    print(json.dumps(manifest["dmg"]))


if __name__ == "__main__":
    main()
