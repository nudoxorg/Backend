#!/usr/bin/env python3
"""Run one complete, artifact-bound Keel inventory inside the delegated guest."""

import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys


def sha256(path):
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def verify_bundle(bundle, expected_sha):
    manifest = json.loads((bundle / "manifest.json").read_bytes())
    if manifest.get("source_sha") != expected_sha:
        raise ValueError("T1 source SHA differs from the requested source")
    seen = set()
    for entry in manifest["files"]:
        relative = Path(entry["path"])
        if relative.is_absolute() or ".." in relative.parts or str(relative) in seen:
            raise ValueError("invalid or duplicate artifact path")
        seen.add(str(relative))
        path = bundle / relative
        if any((bundle.joinpath(*relative.parts[:i])).is_symlink() for i in range(1, len(relative.parts) + 1)) or not path.is_file():
            raise ValueError(f"missing or symlink artifact: {relative}")
        if path.stat().st_size != entry["bytes"] or sha256(path) != entry["sha256"]:
            raise ValueError(f"artifact hash or size mismatch: {relative}")
    if not {"bin/backend-cli", "bin/backend-mcp", "bin/backend-locald", "bin/backend-stylo-worker", "bin/backend-sqlite-worker", "lib/libgcc_s.so.1", "runtime.nar"} <= seen:
        raise ValueError("T1 bundle does not contain the required artifacts")
    for root in manifest["runtime_roots"]:
        if not root.startswith("/nix/store/") or not Path(root).exists():
            raise ValueError(f"runtime closure root unavailable: {root}")
    return manifest


def main():
    if len(sys.argv) != 7:
        raise ValueError("usage: keel-vm-run.py SOURCE_SHA BUNDLE MANIFEST HARNESS EVIDENCE VERIFIER")
    source_sha, bundle, manifest_path, harness, evidence, verifier = sys.argv[1:]
    if not re.fullmatch(r"[0-9a-f]{40}", source_sha):
        raise ValueError("invalid source SHA")
    bundle, manifest_path, harness, evidence = map(Path, (bundle, manifest_path, harness, evidence))
    if os.environ.get("KEEL_REQUIRE_CGROUP") != "1" or not os.environ.get("KEEL_CGROUP_ROOT"):
        raise ValueError("required delegated cgroup is absent")
    subprocess.run([os.environ["NUDOX_PYTHON"], verifier, "verify", str(bundle), "--source-sha", source_sha, "--observe-native"], check=True)
    t1_manifest_digest = sha256(bundle / "manifest.json")
    t1 = verify_bundle(bundle, source_sha)
    inventory_bytes = manifest_path.read_bytes()
    inventory = json.loads(inventory_bytes)
    ids = [case["id"] for case in inventory["cases"]]
    if inventory.get("schema") != 1 or not ids or len(set(ids)) != len(ids):
        raise ValueError("invalid or empty Keel inventory")
    driver_digest = sha256(harness)
    if driver_digest != "3ea59a7c4dcd7638f88f5ff93a622e9c65f487f3bf1eceba2071137eacd0ba1d":
        raise ValueError("Keel driver is not the reviewed public c1 executable")
    env = os.environ.copy()
    allowed = {"LD_LIBRARY_PATH", "FONTCONFIG_FILE", "__EGL_VENDOR_LIBRARY_DIRS", "LIBGL_DRIVERS_PATH", "VK_DRIVER_FILES", "VK_ICD_FILENAMES", "PATH"}
    for key, value in t1["runtime_env"].items():
        if key not in allowed or not isinstance(value, str):
            raise ValueError(f"unexpected T1 runtime environment key: {key}")
        env[key] = value + os.pathsep + env.get("PATH", "") if key == "PATH" else value
    env["LD_LIBRARY_PATH"] = str(bundle / "lib") + os.pathsep + env.get("LD_LIBRARY_PATH", "")
    canonical = json.loads(subprocess.check_output([str(harness), "--manifest", str(manifest_path), "--inventory"], env=env, timeout=120))
    if (canonical.get("schema") != 1 or canonical.get("kind") != "keel-contract-inventory"
            or canonical.get("manifest_sha256") != hashlib.sha256(inventory_bytes).hexdigest()
            or [case["id"] for case in canonical["cases"]] != ids):
        raise ValueError("canonical Keel inventory does not match the source manifest")
    evidence.mkdir(parents=True, exist_ok=False)
    completed = subprocess.run([str(harness), "--manifest", str(manifest_path), "--bin-dir", str(bundle / "bin"), "--evidence", str(evidence)], env=env, check=False)
    verify_bundle(bundle, source_sha)
    if (sha256(harness) != driver_digest or manifest_path.read_bytes() != inventory_bytes
            or sha256(bundle / "manifest.json") != t1_manifest_digest):
        raise ValueError("driver, inventory or T1 manifest changed during execution")
    report = json.loads((evidence / "report.json").read_bytes())
    reported = [case["id"] for case in report["cases"]]
    artifact_binaries = {Path(entry["path"]).name: entry["sha256"] for entry in t1["files"]
                         if Path(entry["path"]).parent == Path("bin")}
    if not report.get("binaries") or any(artifact_binaries.get(name) != digest
                                          for name, digest in report["binaries"].items()):
        raise ValueError("Keel report names a binary outside the verified T1 bundle")
    policy = report.get("containment_policy", {})
    root_bytes = policy.get("root", {}).get("Unix")
    if (report.get("schema") != 1 or report.get("driver_sha256") != driver_digest
            or report.get("manifest_sha256") != hashlib.sha256(inventory_bytes).hexdigest()
            or policy.get("required") is not True
            or root_bytes != list(os.fsencode(os.environ["KEEL_CGROUP_ROOT"]))
            or report.get("inventory") != canonical["cases"]
            or len(reported) != len(ids) or set(reported) != set(ids)):
        raise ValueError("Keel report identity or complete case coverage mismatch")
    if completed.returncode or report.get("status") != "pass" or any(case.get("status") != "pass" for case in report["cases"]):
        return 1
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (OSError, ValueError, KeyError, TypeError, json.JSONDecodeError, subprocess.SubprocessError) as error:
        print(f"keel-vm: {error}", file=sys.stderr)
        sys.exit(125)
