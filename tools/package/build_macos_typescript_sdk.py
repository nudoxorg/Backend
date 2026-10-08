#!/usr/bin/env python3
"""Stage genuine installed Node and TypeScript into an exact-source Mac SDK receipt.

This creates no application build proof and does not download/install packages. Asset
origin verification remains separately receipted. Native version/API probes run only
in the Mac bundle producer after Mach-O closure/target/floor admission.
"""
from __future__ import annotations

import argparse
import importlib.util
import json
import os
from pathlib import Path
import sys

from linux_release_package import admit_file_digest, copy_admitted_file, parse_json_bytes, read_regular_bytes

spec = importlib.util.spec_from_file_location("macos_sdk_bundle", Path(__file__).with_name("macos-investor-bundle.py"))
assert spec and spec.loader
bundle = importlib.util.module_from_spec(spec)
spec.loader.exec_module(bundle)


def stage(source_root: Path, revision: str, tree: str, target: str, node: Path,
          node_version: str, node_digest: str, node_license: Path, license_digest: str,
          package: Path, typescript_version: str, output: Path) -> dict:
    if target not in bundle.MACOS_TARGETS:
        bundle.fail("Mac SDK staging requires an explicit Darwin target")
    source = bundle.validate_source(source_root, revision, tree)
    node = node.resolve(strict=True)
    node_license = node_license.resolve(strict=True)
    package = package.resolve(strict=True)
    output = output.absolute()
    if output.exists() or output.is_symlink() or output == package or package in output.parents:
        bundle.fail("Mac SDK output must be new and outside the installed TypeScript package")
    bundle.require_sha(node_digest, "selected Node executable")
    bundle.require_sha(license_digest, "selected Node license")
    if not node_version.strip() or not typescript_version.strip() or not os.access(node, os.X_OK):
        bundle.fail("Mac SDK staging requires explicit versions and a real executable Node")
    try:
        package_json = parse_json_bytes(read_regular_bytes(package / "package.json", 1024**2,
                                                         "selected TypeScript metadata"), "selected TypeScript metadata")
    except (OSError, ValueError) as error:
        bundle.fail(f"selected TypeScript metadata failed bounded identity admission: {error}")
    if package_json.get("name") != "typescript" or package_json.get("version") != typescript_version:
        bundle.fail("selected TypeScript package differs from the requested package version")
    inputs = {"typescript/node/bin/node": (node, 512 * 1024**2),
              "typescript/node/LICENSE": (node_license, 1024**2)}
    pending = [package]
    entries = package_bytes = 0
    while pending:
        for path in pending.pop().iterdir():
            entries += 1
            if entries > 2048 or path.is_symlink() or not (path.is_dir() or path.is_file()):
                bundle.fail("TypeScript package exceeds its finite entry bound or contains a link/special file")
            if path.is_dir():
                pending.append(path)
            else:
                package_bytes += path.stat().st_size
                if package_bytes > 96 * 1024**2 or len(inputs) >= 512:
                    bundle.fail("TypeScript package exceeds its finite file/byte bounds")
                inputs["typescript/node_modules/typescript/" + path.relative_to(package).as_posix()] = (path, 96 * 1024**2)
    admitted = {relative: admit_file_digest(path, maximum) for relative, (path, maximum) in inputs.items()}
    if sum(size for size, _ in admitted.values()) > 512 * 1024**2:
        bundle.fail("selected SDK exceeds its total byte bound")
    if admitted["typescript/node/bin/node"][1] != node_digest or admitted["typescript/node/LICENSE"][1] != license_digest:
        bundle.fail("selected Node or license differs from its explicit content pin")
    output.mkdir(mode=0o700, parents=True)
    payload = output / "payload"
    for relative, (path, maximum) in inputs.items():
        destination = payload / relative
        destination.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
        size, digest = admitted[relative]
        copy_admitted_file(path, destination, digest, size, maximum)
        destination.chmod(0o755 if relative == "typescript/node/bin/node" else 0o644)
    receipt = {"schema": 1, "source": source, "target": target,
               "files": {relative: digest for relative, (_, digest) in admitted.items()},
               "file_modes": bundle.sdk_file_modes(admitted),
               "tools": {"node": {"version": node_version, "sha256": node_digest}, "typescript": {"version": typescript_version}},
               "notices": {"node": "typescript/node/LICENSE", "typescript": "typescript/node_modules/typescript/LICENSE.txt",
                           "typescript_third_party": "typescript/node_modules/typescript/ThirdPartyNoticeText.txt"},
               "origins": {"kind": "operator-selected local assets; not an official signature-verification claim",
                           "node": str(node), "node_license": str(node_license), "typescript_package": str(package)},
               "runtime_probe_status": "pending relocated bundle producer Node/Compiler API probes"}
    bundle.validate_sdk_helper_payload(receipt, payload, source, target)
    if bundle.validate_source(source_root, revision, tree) != source:
        bundle.fail("application source changed during SDK staging")
    receipt_path = output / "compiler-helpers-source-receipt.json"
    receipt_path.write_text(json.dumps(receipt, sort_keys=True, indent=2) + "\n")
    return {"payload": str(payload), "receipt": str(receipt_path), "receipt_sha256": bundle.sha256(receipt_path),
            "files": len(admitted), "bytes": sum(size for size, _ in admitted.values())}


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__)
    for name in ["source-root", "node", "node-license", "typescript-package", "output"]:
        p.add_argument("--" + name, required=True, type=Path)
    for name in ["expected-revision", "expected-tree", "node-version", "node-sha256", "node-license-sha256", "typescript-version"]:
        p.add_argument("--" + name, required=True)
    p.add_argument("--target", required=True, choices=bundle.MACOS_TARGETS)
    a = p.parse_args()
    print(json.dumps(stage(a.source_root, a.expected_revision, a.expected_tree, a.target, a.node,
                           a.node_version, a.node_sha256, a.node_license, a.node_license_sha256,
                           a.typescript_package, a.typescript_version, a.output), indent=2))
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (bundle.PackageError, OSError, ValueError) as error:
        print(f"Mac SDK staging refused: {error}", file=sys.stderr)
        raise SystemExit(2)
