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

BINARIES = ("backend-cli", "backend-mcp", "backend-desktop", "backend-locald", "backend-metrics",
            "backend-stylo-worker", "backend-sqlite-worker")
NATIVE_FILES = ("lib/libgcc_s.so.1", "development-observation.json",
                "observers/stylo_identity_observer", "observers/installed_basis_observer")
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
    if root.is_symlink() or (root / "bin").is_symlink() or (root / "lib").is_symlink() or (root / "observers").is_symlink() or (root / "manifest.json").is_symlink():
        raise ValueError("symlinked artifact root or inventory")
    manifest = json.loads((root / "manifest.json").read_text())
    if manifest["schema"] != 2 or manifest["source_sha"] != source_sha or manifest["target"] != target:
        raise ValueError("artifact schema/source/target mismatch")
    if not re.fullmatch(r"[a-f0-9]{40}", source_sha):
        raise ValueError("expected a full source SHA")
    if not isinstance(manifest.get("toolchain"), str) or not manifest["toolchain"].strip():
        raise ValueError("missing Rust toolchain identity")
    expected = {f"bin/{name}" for name in BINARIES} | {"smoke.py", "runtime.nar", *NATIVE_FILES}
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
    observation = json.loads((root / "development-observation.json").read_text())
    if (observation.get("schema") != "nudox.keel-development-native-observation.v1"
            or observation.get("source_revision") != source_sha
            or observation.get("host") != target
            or observation.get("source_status") != ""):
        raise ValueError("native observation source/profile mismatch")
    if manifest.get("profile") != "debug":
        raise ValueError("native artifact profile mismatch")
    for field, path in (("stylo_worker_sha256", "bin/backend-stylo-worker"),
                        ("sqlite_worker_sha256", "bin/backend-sqlite-worker"),
                        ("libgcc_sha256", "lib/libgcc_s.so.1"),
                        ("stylo_observer_sha256", "observers/stylo_identity_observer"),
                        ("sqlite_observer_sha256", "observers/installed_basis_observer")):
        if observation.get(field) != digest(root / path):
            raise ValueError(f"native observation mismatch: {path}")
    for field, state, identity in (("stylo", "observed-helper", "identity"),
                                    ("sqlite", "observed-build-helper", "artifact_identity")):
        record = observation.get(field, {})
        value = record.get(identity, "")
        if record.get("state") != state or not re.fullmatch(r"[0-9a-f]{64}", value) or not int(value, 16):
            raise ValueError(f"invalid native identity: {field}")
    products = observation.get("product_outputs", [])
    expected_products = set(BINARIES) - {"backend-stylo-worker", "backend-sqlite-worker"}
    if {item["name"] for item in products} != expected_products or len(products) != len(expected_products):
        raise ValueError("native owner inventory mismatch")
    for item in products:
        if item["sha256"] != digest(root / "bin" / item["name"]):
            raise ValueError("native owner checksum mismatch")
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


def publish(stage: Path, destination: Path) -> None:
    bin_dir = stage / "bin"
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
        (root / "lib").mkdir()
        (root / "observers").mkdir()
        for path in NATIVE_FILES:
            source = stage / path
            if source.is_symlink() or not source.is_file():
                raise ValueError(f"missing regular native payload: {path}")
            shutil.copy2(source, root / path)
        roots: set[str] = set()
        for name in BINARIES:
            binary = bin_dir / name
            if binary.is_symlink() or not binary.is_file():
                raise ValueError(f"missing regular product binary: {name}")
            shutil.copy2(binary, root / "bin" / name)
            dependencies = command("ldd", str(root / "bin" / name))
            if "not found" in dependencies:
                raise ValueError(f"unresolved runtime library: {name}")
            roots.update(store_roots(dependencies))
        for path in NATIVE_FILES[2:]:
            dependencies = command("ldd", str(root / path))
            if "not found" in dependencies:
                raise ValueError(f"unresolved observer runtime library: {path}")
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
        paths = [*(f"bin/{name}" for name in BINARIES), *NATIVE_FILES, "smoke.py", "runtime.nar"]
        manifest = {
            "schema": 2, "source_sha": sha, "target": TARGET, "toolchain": toolchain, "profile": "debug",
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
    producer.add_argument("--stage", required=True, type=Path)
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
        publish(args.stage, args.output)
        return
    # Smoke launches binaries from a private cwd; keep artifact paths absolute.
    args.artifact = args.artifact.absolute()
    manifest = verify(args.artifact, args.source_sha, args.target)
    if args.smoke or args.import_runtime:
        with (args.artifact / "runtime.nar").open("rb") as stream:
            subprocess.run(["nix-store", "--import"], stdin=stream, check=True, timeout=300)
    if args.smoke or args.import_runtime:
        observation = json.loads((args.artifact / "development-observation.json").read_text())
        for observer, worker, key in (("stylo_identity_observer", "backend-stylo-worker", "stylo"),
                                      ("installed_basis_observer", "backend-sqlite-worker", "sqlite")):
            actual = json.loads(command(str(args.artifact / "observers" / observer),
                                        str(args.artifact / "bin" / worker),
                                        str(args.artifact / "lib/libgcc_s.so.1")))
            if actual != observation[key]:
                raise ValueError(f"native observer mismatch: {worker}")
    if args.smoke:
        # Concourse gives the task cwd its own writable volume. The rootfs
        # /tmp is an overlay over the imported image/runtime; tiny durable
        # workspace writes there can wait behind the overlay's journal work.
        # Keep real file/directory fsyncs, but create smoke state on the task
        # volume, outside the immutable artifact input. This also keeps the
        # temporary path local to the consumer rather than exporting a build
        # machine path or passing its ambient environment to the product.
        with tempfile.TemporaryDirectory(prefix="nudox-artifact-smoke-", dir=Path.cwd()) as scratch:
            env = {**manifest["runtime_env"], "LANG": "C.UTF-8", "TMPDIR": scratch}
            for surface in ("cli", "mcp", "gui", "metrics"):
                subprocess.run([manifest["python"], str(args.artifact / "smoke.py"), surface,
                                "--bin-dir", str(args.artifact / "bin")], env=env, check=True, timeout=180)
    print(f"T1 artifacts verified: {args.source_sha}")


if __name__ == "__main__":
    main()
