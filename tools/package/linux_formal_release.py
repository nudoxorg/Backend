#!/usr/bin/env python3
"""Package a T1-published Linux artifact as a formal linux-x64 release candidate.

Takes the four binaries T1 already built and verified through the managed CI
pipeline (never a scratch build) and portabilizes them: rewrites each ELF's
RPATH and copies its non-glibc shared-library closure locally, so the
resulting archive runs without a Nix store. Reuses the proven ELF-relocation
logic from linux_release_package.py (the separate checkpoint-release tool)
rather than re-deriving it. Emits the schema the formal release pipeline
(release_contract.py / release_platforms.py["linux-x64"]) checks: a plain
Nudox/ prefixed tar.gz with Nudox/bin/*, Nudox/lib/*.so, and
Nudox/resources/build-manifest.json, plus a sibling release-manifest.json.
"""
from __future__ import annotations

import argparse
import json
from pathlib import Path
import subprocess
import sys
import tarfile
import tomllib

from linux_release_package import (
    GLIBC_SONAMES,
    INTERPRETER,
    MAX_BINARY_BYTES,
    MAX_NODE_LIBRARIES,
    MAX_NODE_LIBRARY_BYTES,
    admit_file_digest,
    copy_admitted_file,
    elf_info,
    fail,
    ldd_dependencies,
    read_regular_bytes,
    require_tool,
    run,
    sha256,
    version_tuple,
)

PLATFORM = "linux-x64"
TARGET = "x86_64-unknown-linux-gnu"
ASSET = "nudox-linux-x64.tar.gz"
EXECUTABLES = ("backend-desktop", "backend-cli", "backend-mcp", "backend-locald")
MINIMUM_OS = "Linux (glibc)"


def git(source: Path, *args: str) -> str:
    return subprocess.check_output(["git", "-C", str(source), *args], text=True, timeout=60).strip()


def load_t1_manifest(artifact_dir: Path) -> dict:
    manifest = json.loads(read_regular_bytes(artifact_dir / "manifest.json", 16 * 1024 * 1024, "T1 artifact manifest"))
    if manifest.get("schema") != 1 or manifest.get("target") != TARGET:
        fail("T1 artifact manifest has the wrong schema or target for linux-x64")
    for name in EXECUTABLES:
        if not (artifact_dir / "bin" / name).is_file():
            fail(f"T1 artifact is missing {name}")
    return manifest


def check_source(source: Path, revision: str) -> tuple[str, str, str]:
    subprocess.run(["git", "-C", str(source), "merge-base", "--is-ancestor", revision, "HEAD"],
                    check=True, timeout=30)
    tree = git(source, "rev-parse", f"{revision}^{{tree}}")
    cargo_lock = sha256(source / "Cargo.lock")
    pyproject = tomllib.loads((source / "Cargo.toml").read_text())
    version = pyproject["workspace"]["package"]["version"]
    return tree, cargo_lock, version


def portabilize(bin_dir: Path, names: tuple[str, ...], output: Path, patchelf: Path, readelf: Path, ldd: Path) -> tuple[list[dict], str]:
    """Rewrite each executable's RPATH and bundle its non-glibc closure. Mirrors
    linux_release_package.py's proven library-copy/patch loop; no new policy."""
    root = output / "Nudox"
    (root / "bin").mkdir(mode=0o700, parents=True)
    (root / "lib").mkdir(mode=0o700)
    libraries: dict[str, dict] = {}
    library_total_bytes = 0
    executables: list[dict] = []
    for name in names:
        source_binary = (bin_dir / name).resolve(strict=True)
        packaged = root / "bin" / name
        digest_bytes, digest = admit_file_digest(source_binary, MAX_BINARY_BYTES)
        copy_admitted_file(source_binary, packaged, digest, digest_bytes, MAX_BINARY_BYTES)
        original = elf_info(readelf, packaged)
        if not original["interpreter"]:
            fail(f"{name} has no ELF interpreter")
        packaged.chmod(0o755)
        linkage = run([str(ldd), str(packaged)])
        for soname, raw_path in ldd_dependencies(linkage):
            if soname in GLIBC_SONAMES:
                continue
            if raw_path == "not":
                fail(f"{name} has an unresolved shared dependency: {soname}")
            dependency = Path(raw_path)
            if not dependency.is_absolute() or not dependency.is_file():
                fail(f"could not resolve non-glibc dependency {soname}: {raw_path}")
            dependency = dependency.resolve(strict=True)
            if soname not in libraries and len(libraries) >= MAX_NODE_LIBRARIES:
                fail("shared-library closure exceeds its member bound")
            previous = libraries.get(soname)
            remaining = MAX_NODE_LIBRARY_BYTES - library_total_bytes + (previous["source_bytes"] if previous else 0)
            library_bytes, lib_digest = admit_file_digest(dependency, min(96 * 1024 * 1024, remaining))
            if previous and previous["source_sha256"] != lib_digest:
                fail(f"different dependencies use the same ELF soname {soname}")
            if previous is None:
                library_total_bytes += library_bytes
            libraries.setdefault(soname, {"source_path": dependency, "source_sha256": lib_digest, "source_bytes": library_bytes})
        run([str(patchelf), "--set-interpreter", INTERPRETER, "--set-rpath", "$ORIGIN/../lib", str(packaged)])
        patched = elf_info(readelf, packaged)
        if patched["interpreter"] != INTERPRETER or patched["needed"] != original["needed"]:
            fail(f"patchelf changed the wrong ELF contract for {name}")
        executables.append({
            "name": name,
            "packaged_path": f"bin/{name}",
            "sha256": digest,
            "bytes": digest_bytes,
            "required_glibc_versions": patched["required_glibc_versions"],
        })

    for soname, record in libraries.items():
        packaged = root / "lib" / soname
        copy_admitted_file(record["source_path"], packaged, record["source_sha256"], record["source_bytes"], 96 * 1024 * 1024)
        packaged.chmod(0o755)
        run([str(patchelf), "--set-rpath", "$ORIGIN", str(packaged)])
        info = elf_info(readelf, packaged)
        record.update({"needed": info["needed"], "required_glibc_versions": info["required_glibc_versions"]})

    for soname, record in libraries.items():
        missing = [item for item in record["needed"] if item not in GLIBC_SONAMES and item not in libraries and not item.startswith("ld-linux-")]
        if missing:
            fail(f"{soname} has dependencies outside the bundled/system closure: {missing}")

    glibc_versions = [v for e in executables for v in e["required_glibc_versions"]]
    glibc_versions.extend(v for r in libraries.values() for v in r["required_glibc_versions"])
    if not glibc_versions:
        fail("ELF dependency scan found no glibc symbol floor")
    minimum_glibc = max(glibc_versions, key=version_tuple)
    return executables, minimum_glibc


def write_build_manifest(output: Path, revision: str, tree: str, cargo_lock: str, executables: list[dict]) -> Path:
    manifest = {
        "schema": 1,
        "source": {"git_revision": revision, "git_tree": tree, "cargo_lock_sha256": cargo_lock},
        "target": TARGET,
        "executables": {e["name"]: {"path": e["packaged_path"], "sha256": e["sha256"], "bytes": e["bytes"]} for e in executables},
    }
    path = output / "Nudox" / "resources" / "build-manifest.json"
    path.parent.mkdir(mode=0o700, parents=True)
    path.write_text(json.dumps(manifest, sort_keys=True, separators=(",", ":")) + "\n")
    return path


def make_archive(output: Path) -> Path:
    archive = output / ASSET
    with tarfile.open(archive, "w:gz", format=tarfile.USTAR_FORMAT, dereference=True) as bundle:
        bundle.add(output / "Nudox", arcname="Nudox")
    return archive


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--t1-artifact-dir", required=True, type=Path, help="T1's published artifact root (manifest.json + bin/)")
    parser.add_argument("--source", required=True, type=Path, help="clean checkout at the artifact's exact source_sha")
    parser.add_argument("--output", required=True, type=Path, help="new candidate output directory")
    parser.add_argument("--patchelf", required=True, type=Path)
    parser.add_argument("--readelf", required=True, type=Path)
    parser.add_argument("--ldd", required=True, type=Path)
    args = parser.parse_args()

    output = args.output.resolve()
    if output.exists() or output == args.source.resolve() or args.source.resolve() in output.parents:
        fail("output directory must be new and outside the source checkout")

    t1_manifest = load_t1_manifest(args.t1_artifact_dir)
    revision = t1_manifest["source_sha"]
    tree, cargo_lock, version = check_source(args.source, revision)

    output.mkdir(mode=0o700, parents=True)
    patchelf, readelf, ldd = require_tool(args.patchelf, "patchelf"), require_tool(args.readelf, "readelf"), require_tool(args.ldd, "ldd")
    executables, minimum_glibc = portabilize(args.t1_artifact_dir / "bin", EXECUTABLES, output, patchelf, readelf, ldd)
    build_manifest_path = write_build_manifest(output, revision, tree, cargo_lock, executables)
    build_manifest_sha = sha256(build_manifest_path)
    archive = make_archive(output)
    archive_sha = sha256(archive)

    release_manifest = {
        "schema": 1,
        "version": version,
        "source_sha": revision,
        "source_tree": tree,
        "cargo_lock_sha256": cargo_lock,
        "platform": PLATFORM,
        "target": TARGET,
        "asset": ASSET,
        "sha256": archive_sha,
        "size_bytes": archive.stat().st_size,
        "minimum_os": MINIMUM_OS,
        "build_manifest_sha256": build_manifest_sha,
        "signed": False,
        "notarized": False,
    }
    (output / f"release-manifest-{PLATFORM}.json").write_text(json.dumps(release_manifest, sort_keys=True, indent=2) + "\n")
    (output / f"{ASSET}.sha256").write_text(f"{archive_sha}  {ASSET}\n")
    print(f"linux-x64 candidate: {revision} glibc>={minimum_glibc} -> {archive}")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (ValueError, OSError, KeyError, TypeError) as error:
        print(f"Linux formal release stopped: {error}", file=sys.stderr)
        raise SystemExit(1)
