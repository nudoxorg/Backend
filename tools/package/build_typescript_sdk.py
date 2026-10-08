"""Stage genuine Node/TypeScript assets for the source-bound portable packager.

This copies admitted installed assets only. It never installs packages or executes application
setup. The portable packager subsequently checks the relocated Node and Compiler API versions.
"""
from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import shutil
import sys

from linux_release_package import TARGET, admit_typescript_sdk, fail, load_json, sha256


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--manifest", required=True, type=Path)
    parser.add_argument("--node", required=True, type=Path)
    parser.add_argument("--node-version", required=True)
    parser.add_argument("--node-license", required=True, type=Path)
    parser.add_argument("--node-license-origin", required=True)
    parser.add_argument("--typescript-package", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()
    build = load_json(args.manifest, "runtime build manifest")
    if build.get("schema") != "nudox.runtime-build-manifest.v1" or not isinstance(build.get("source"), dict):
        fail("SDK staging requires an exact runtime build manifest")
    node = args.node.resolve(strict=True)
    license_path = args.node_license.resolve(strict=True)
    package_root = args.typescript_package.resolve(strict=True)
    if not node.is_file() or not os.access(node, os.X_OK) or not license_path.is_file():
        fail("SDK staging requires a genuine installed Node executable and its notice")
    package = load_json(package_root / "package.json", "installed TypeScript package")
    if package.get("name") != "typescript" or not isinstance(package.get("version"), str):
        fail("SDK staging requires the genuine TypeScript npm package")
    if node.stat().st_size > 512 * 1024 * 1024 or license_path.stat().st_size > 1024 * 1024:
        fail("Node or its notice exceeds the SDK staging bounds")
    output = args.output.resolve(strict=False)
    if output.exists() or output == package_root or package_root in output.parents:
        fail("SDK output must be new and outside the installed package")
    payload = output / "payload"
    (payload / "node/bin").mkdir(mode=0o700, parents=True)
    shutil.copyfile(node, payload / "node/bin/node")
    (payload / "node/bin/node").chmod(0o755)
    shutil.copyfile(license_path, payload / "node/LICENSE")
    module_root = payload / "node_modules/typescript"
    module_root.mkdir(mode=0o700, parents=True)
    pending = [package_root]
    count = 2
    total = node.stat().st_size + license_path.stat().st_size
    visited = 0
    package_bytes = 0
    while pending:
        for path in pending.pop().iterdir():
            visited += 1
            if visited > 2048:
                fail("installed TypeScript exceeds the finite SDK entry bound")
            if path.is_symlink() or not (path.is_dir() or path.is_file()):
                fail("installed TypeScript contains a link or special file")
            if path.is_dir():
                pending.append(path)
                continue
            count += 1
            total += path.stat().st_size
            package_bytes += path.stat().st_size
            if package_bytes > 96 * 1024 * 1024:
                fail("TypeScript package exceeds its 96 MiB source bound")
            if count > 512 or total > 512 * 1024 * 1024:
                fail("installed SDK exceeds its bounded package inventory")
            target = module_root / path.relative_to(package_root)
            target.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
            shutil.copyfile(path, target)
            target.chmod(0o644)
    files = {path.relative_to(payload).as_posix(): sha256(path)
             for path in sorted(payload.rglob("*")) if path.is_file()}
    receipt = {
        "schema": "nudox.typescript-sdk.v1", "source": build["source"], "target": TARGET,
        "files": files,
        "tools": {"node": {"version": args.node_version, "sha256": files["node/bin/node"]},
                  "typescript": {"version": package["version"]}},
        "origins": {"node": str(node), "typescript": str(package_root),
                    "node_license": {"source": args.node_license_origin, "sha256": sha256(license_path)}},
    }
    receipt_path = output / "sdk-source-receipt.json"
    receipt_path.write_text(json.dumps(receipt, indent=2, sort_keys=True) + "\n")
    admit_typescript_sdk(payload, receipt_path, build["source"])
    print(json.dumps({"payload": str(payload), "receipt": str(receipt_path),
                      "receipt_sha256": sha256(receipt_path), "file_count": len(files), "bytes": total}, indent=2))
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (ValueError, OSError, KeyError, TypeError) as error:
        print(f"TypeScript SDK staging stopped: {error}", file=sys.stderr)
        raise SystemExit(1)
