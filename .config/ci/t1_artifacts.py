#!/usr/bin/env python3
"""Publish and verify T1 Linux binaries plus their portable Nix runtime closure."""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile

BINARIES = ("backend-cli", "backend-mcp", "backend-desktop", "backend-locald")
RUNTIME_ENV = (
    "LD_LIBRARY_PATH", "FONTCONFIG_FILE", "__EGL_VENDOR_LIBRARY_DIRS",
    "LIBGL_DRIVERS_PATH", "VK_DRIVER_FILES", "VK_ICD_FILENAMES",
)
STORE = re.compile(r"/nix/store/[a-z0-9]{32}-[^/\s:'\"]+")
TARGET = "x86_64-unknown-linux-gnu"
MAX_BYTES = 8 * 1024 ** 3


def command(*args: str) -> str:
    return subprocess.check_output(args, text=True, timeout=120).strip()


def digest(path: Path) -> str:
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def store_roots(value: str) -> set[str]:
    return set(STORE.findall(value))


def verify(root: Path, source_sha: str, target: str) -> dict:
    if root.is_symlink() or (root / "bin").is_symlink() or (root / "manifest.json").is_symlink():
        raise ValueError("symlinked artifact root or inventory")
    manifest = json.loads((root / "manifest.json").read_text())
    if manifest["schema"] != 1 or manifest["source_sha"] != source_sha or manifest["target"] != target:
        raise ValueError("artifact schema/source/target mismatch")
    if not re.fullmatch(r"[a-f0-9]{40}", source_sha):
        raise ValueError("expected a full source SHA")
    if not isinstance(manifest.get("toolchain"), str) or not manifest["toolchain"].strip():
        raise ValueError("missing Rust toolchain identity")
    expected = {f"bin/{name}" for name in BINARIES} | {"smoke.py", "runtime.nar"}
    files = manifest["files"]
    if {item["path"] for item in files} != expected or len(files) != len(expected):
        raise ValueError("artifact payload inventory mismatch")
    total = 0
    for item in files:
        path = root / item["path"]
        if path.is_symlink() or not path.is_file():
            raise ValueError(f"missing or symlinked payload: {item['path']}")
        size = path.stat().st_size
        total += size
        if size != item["bytes"] or digest(path) != item["sha256"]:
            raise ValueError(f"artifact checksum mismatch: {item['path']}")
    if total > MAX_BYTES:
        raise ValueError("artifact exceeds 8 GiB budget")
    roots = manifest["runtime_roots"]
    if not roots or any(STORE.fullmatch(path) is None for path in roots):
        raise ValueError("invalid Nix runtime root")
    if set(manifest["runtime_env"]) - {*RUNTIME_ENV, "PATH"}:
        raise ValueError("unexpected runtime environment variable")
    referenced = store_roots("\n".join(manifest["runtime_env"].values()) + "\n" + manifest["python"])
    if not referenced <= set(roots) or not manifest["python"].startswith("/nix/store/"):
        raise ValueError("runtime environment is not covered by exported roots")
    for value in manifest["runtime_env"].values():
        if any(not part.startswith("/nix/store/") for part in value.split(":")):
            raise ValueError("runtime paths must be immutable Nix store paths")
    return manifest



def inventory(root: Path, source_sha: str) -> dict:
    """Read-only byte identities survive payload container expiry in CI events."""
    return inventory_record(root, verify(root, source_sha, TARGET))


def inventory_record(root: Path, manifest: dict) -> dict:
    return {"schema": 1, "source_sha": manifest["source_sha"],
            "target": manifest["target"], "artifact_manifest_sha256": digest(root / "manifest.json"),
            "toolchain": manifest["toolchain"].encode("utf-8")[:1024].decode("utf-8", errors="replace"),
            "toolchain_sha256": hashlib.sha256(manifest["toolchain"].encode("utf-8")).hexdigest(),
            "toolchain_truncated": len(manifest["toolchain"].encode("utf-8")) > 1024,
            "files": manifest["files"]}


def publish(bin_dir: Path, destination: Path) -> None:
    sha = command("git", "rev-parse", "HEAD")
    if command("git", "status", "--porcelain", "--untracked-files=no"):
        raise ValueError("artifact publication requires a clean tracked checkout")
    toolchain = command("rustc", "-vV")
    if f"host: {TARGET}" not in toolchain:
        raise ValueError(f"artifact publication currently supports {TARGET} only")
    if destination.exists():
        raise ValueError("artifact destination already exists; refusing overwrite")
    destination.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix=".t1-", dir=destination.parent) as temporary:
        root = Path(temporary)
        (root / "bin").mkdir()
        roots: set[str] = set()
        for name in BINARIES:
            binary = bin_dir / name
            if binary.is_symlink() or not binary.is_file():
                raise ValueError(f"missing regular product binary: {name}")
            shutil.copy2(binary, root / "bin" / name)
            dependencies = command("ldd", str(binary))
            if "not found" in dependencies:
                raise ValueError(f"unresolved runtime library: {name}")
            roots.update(store_roots(dependencies))
        runtime_env = {key: os.environ[key] for key in RUNTIME_ENV if os.environ.get(key)}
        tools = {}
        for name in ("python3", "Xvfb", "xdotool"):
            found = shutil.which(name)
            if found is None:
                raise ValueError(f"missing runtime tool: {name}")
            tools[name] = str(Path(found).resolve())
        runtime_env["PATH"] = ":".join(sorted({str(Path(path).parent) for path in tools.values()}))
        roots.update(store_roots("\n".join(runtime_env.values()) + "\n" + tools["python3"]))
        if not roots:
            raise ValueError("no immutable runtime roots discovered")
        closure = command("nix-store", "--query", "--requisites", *sorted(roots)).splitlines()
        sizes = command("nix-store", "--query", "--size", *sorted(set(closure))).splitlines()
        if sum(map(int, sizes)) + sum((root / "bin" / name).stat().st_size for name in BINARIES) > MAX_BYTES:
            raise ValueError("runtime closure exceeds 8 GiB artifact budget")
        shutil.copy2(Path(__file__).with_name("smoke.py"), root / "smoke.py")
        with (root / "runtime.nar").open("wb") as stream:
            subprocess.run(["nix-store", "--export", *sorted(set(closure))], stdout=stream, check=True, timeout=300)
        paths = [*(f"bin/{name}" for name in BINARIES), "smoke.py", "runtime.nar"]
        manifest = {
            "schema": 1, "source_sha": sha, "target": TARGET, "toolchain": toolchain,
            "runtime_roots": sorted(roots), "runtime_env": runtime_env, "python": tools["python3"],
            "files": [{"path": path, "bytes": (root / path).stat().st_size, "sha256": digest(root / path)} for path in paths],
        }
        (root / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
        manifest = verify(root, sha, TARGET)
        root.rename(destination)
    print(f"T1 artifacts: {sha} {TARGET}, {sum(item['bytes'] for item in manifest['files'])} bytes")
    # The manifest was verified immediately before atomic publication; avoid
    # hashing GiB payloads again just to retain their already-admitted hashes.
    print("CI-ARTIFACT " + json.dumps(inventory_record(destination, manifest), sort_keys=True), flush=True)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="action", required=True)
    producer = commands.add_parser("publish")
    producer.add_argument("--bin-dir", required=True, type=Path)
    producer.add_argument("--output", required=True, type=Path)
    consumer = commands.add_parser("verify")
    consumer.add_argument("artifact", type=Path)
    consumer.add_argument("--source-sha", required=True)
    consumer.add_argument("--target", default=TARGET)
    consumer.add_argument("--import-runtime", action="store_true", help="verify and import runtime without launching smoke (for T2 consumers)")
    consumer.add_argument("--smoke", action="store_true", help="import runtime closure and smoke the exact published binaries")
    inspector = commands.add_parser("inventory", help="verify and print retained byte identities without executing anything")
    inspector.add_argument("artifact", type=Path)
    inspector.add_argument("--source-sha", required=True)
    args = parser.parse_args()
    if args.action == "inventory":
        print(json.dumps(inventory(args.artifact.absolute(), args.source_sha), sort_keys=True))
        return
    if args.action == "publish":
        publish(args.bin_dir, args.output)
        return
    # Smoke launches binaries from a private cwd; keep artifact paths absolute.
    args.artifact = args.artifact.absolute()
    manifest = verify(args.artifact, args.source_sha, args.target)
    if args.smoke or args.import_runtime:
        with (args.artifact / "runtime.nar").open("rb") as stream:
            subprocess.run(["nix-store", "--import"], stdin=stream, check=True, timeout=300)
    if args.smoke:
        env = {**manifest["runtime_env"], "LANG": "C.UTF-8"}
        for surface in ("cli", "mcp", "gui"):
            subprocess.run([manifest["python"], str(args.artifact / "smoke.py"), surface,
                            "--bin-dir", str(args.artifact / "bin")], env=env, check=True, timeout=180)
    print(f"T1 artifacts verified: {args.source_sha}")


if __name__ == "__main__":
    main()
