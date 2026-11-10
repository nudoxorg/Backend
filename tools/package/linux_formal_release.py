#!/usr/bin/env python3
"""Package Linux binaries as a formal linux-x64 release candidate.

The binaries come from one of two places: a T1-published artifact
(``--t1-artifact-dir``, verified against its manifest), or the release job's
own ``cargo build --release`` of the clean source checkout
(``--release-dir``, stripped here). Either way they are portabilized: each ELF's
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
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shutil
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
    check_loader_policy,
    copy_admitted_file,
    elf_info,
    fail,
    ldd_dependencies,
    parse_json_bytes,
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
MINIMUM_OS = "Linux glibc"
GUI_SONAMES = Path(__file__).with_name("linux-gui-sonames.txt")
MAX_GUI_LIBRARIES = 192
NIX_STORE = "/nix/store/"
# Every glibc desktop already has these, and the nixpkgs builds need a newer
# glibc than our executables (GLIBC_ABI_GNU2_TLS), so the host's copy is used.
HOST_SONAMES = frozenset({"libasound.so.2", "libblkid.so.1", "libmount.so.1", "libselinux.so.1"})
# Installed command names, as the v0.2.0 archive and installer ship them.
LAUNCHERS = {"nudox": "backend-cli", "nudox-desktop": "backend-desktop", "nudox-mcp": "backend-mcp"}
LAUNCHER = """#!/bin/sh
here=$(dirname "$(readlink -f "$0")")
if [ -z "${FONTCONFIG_FILE:-}" ] && [ -f /etc/fonts/fonts.conf ]; then
  export FONTCONFIG_FILE=/etc/fonts/fonts.conf
fi
exec "$here/@BINARY@" "$@"
"""


def git(source: Path, *args: str) -> str:
    return subprocess.check_output(["git", "-C", str(source), *args], text=True, timeout=60).strip()


def load_t1_manifest(artifact_dir: Path) -> dict:
    raw = read_regular_bytes(artifact_dir / "manifest.json", 16 * 1024 * 1024, "T1 artifact manifest")
    manifest = parse_json_bytes(raw, "T1 artifact manifest")
    revision = manifest.get("source_sha")
    if (manifest.get("schema") != 1 or manifest.get("target") != TARGET
            or not isinstance(revision, str) or len(revision) != 40
            or any(character not in "0123456789abcdef" for character in revision)):
        fail("T1 artifact manifest has the wrong schema or target for linux-x64")
    ci_manifest = Path(__file__).resolve().parents[2] / ".config" / "ci" / "t1_artifacts.py"
    spec = importlib.util.spec_from_file_location("nudox_t1_artifacts", ci_manifest)
    if spec is None or spec.loader is None:
        fail("cannot load the T1 artifact verifier")
    verifier = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(verifier)
    try:
        return verifier.verify(artifact_dir, revision, TARGET)
    except (KeyError, OSError, TypeError, ValueError) as error:
        fail(f"T1 artifact verification failed: {error}")


def stage_release_build(release_dir: Path, stage: Path, strip: Path) -> dict[str, dict]:
    """Copy and strip the release build's executables; return their inventory.

    The inventory plays the T1 manifest's part for portabilize(): the bytes
    it relocates must be exactly the bytes staged here.
    """
    stage.mkdir(mode=0o700)
    expected = {}
    for name in EXECUTABLES:
        source = (release_dir / name).resolve(strict=True)
        source_bytes, source_digest = admit_file_digest(source, MAX_BINARY_BYTES)
        staged = stage / name
        copy_admitted_file(source, staged, source_digest, source_bytes, MAX_BINARY_BYTES)
        staged.chmod(0o755)
        run([str(strip), "--strip-all", str(staged)])
        size, digest = admit_file_digest(staged, MAX_BINARY_BYTES)
        expected[f"bin/{name}"] = {"bytes": size, "sha256": digest}
    return expected


def bundle_gui_libraries(lib_dir: Path, search: list[Path], patchelf: Path, readelf: Path, glibc_floor: str) -> None:
    """Bundle the desktop's GUI libraries and their closure.

    The dev shell hands these to the desktop through LD_LIBRARY_PATH, not its
    RPATH, and several are dlopen()ed, so ldd never sees them. The list is the
    set the tested early-access and v0.2.0 desktops carried. None may need a
    newer glibc than the executables (glibc_floor): that would silently raise
    the release's minimum OS.
    """
    search = list(search)
    def find(soname: str) -> Path | None:
        for directory in search:
            if (directory / soname).is_file():
                return (directory / soname).resolve(strict=True)
        return None
    queue = [line.strip() for line in GUI_SONAMES.read_text().splitlines() if line.strip() and not line.startswith("#")]
    missing, too_new, added = [], [], 0
    while True:
        if not queue:
            # Listed transitive libraries live in another library's RPATH,
            # which is only searched once that library is bundled.
            queue = [name for name in missing if find(name)]
            if not queue:
                break
            missing = [name for name in missing if name not in queue]
        soname = queue.pop()
        if soname in GLIBC_SONAMES or soname in HOST_SONAMES or soname.startswith("ld-linux-") or (lib_dir / soname).exists():
            continue
        source = find(soname)
        if source is None:
            missing.append(soname)
            continue
        added += 1
        if added > MAX_GUI_LIBRARIES:
            fail("GUI library closure exceeds its member bound")
        # A library's own RPATH is where its dependencies live.
        search.extend(Path(item) for item in run([str(patchelf), "--print-rpath", str(source)]).split(":") if item.startswith(NIX_STORE))
        library_bytes, digest = admit_file_digest(source, 96 * 1024 * 1024)
        packaged = lib_dir / soname
        copy_admitted_file(source, packaged, digest, library_bytes, 96 * 1024 * 1024)
        packaged.chmod(0o755)
        run([str(patchelf), "--set-rpath", "$ORIGIN", str(packaged)])
        info = elf_info(readelf, packaged)
        if any(version_tuple(version) > version_tuple(glibc_floor) for version in info["required_glibc_versions"]):
            too_new.append(soname)
        queue.extend(info["needed"])
    if missing:
        fail(f"GUI libraries not found on the GUI library path: {sorted(set(missing))}")
    if too_new:
        fail(f"GUI libraries need a newer glibc than the executables' {glibc_floor}; take them from the host (HOST_SONAMES): {sorted(too_new)}")


def write_launchers(root: Path) -> None:
    for command, binary in LAUNCHERS.items():
        path = root / "bin" / command
        path.write_text(LAUNCHER.replace("@BINARY@", binary))
        path.chmod(0o755)


def normalize_modes(root: Path) -> None:
    """World-readable files, executable where they already are: an install is per user."""
    for path in [root, *root.rglob("*")]:
        path.chmod(0o755 if path.is_dir() or os.access(path, os.X_OK) else 0o644)


def check_source(source: Path, revision: str) -> tuple[str, str, str]:
    source = source.resolve(strict=True)
    if git(source, "status", "--porcelain"):
        fail("source checkout is dirty")
    if git(source, "rev-parse", "HEAD") != revision:
        fail("source checkout is not at the T1 artifact revision")
    tree = git(source, "rev-parse", f"{revision}^{{tree}}")
    cargo_lock_bytes = subprocess.check_output(["git", "-C", str(source), "show", f"{revision}:Cargo.lock"], timeout=60)
    cargo_lock = hashlib.sha256(cargo_lock_bytes).hexdigest()
    cargo_toml = subprocess.check_output(["git", "-C", str(source), "show", f"{revision}:Cargo.toml"], text=True, timeout=60)
    pyproject = tomllib.loads(cargo_toml)
    version = pyproject["workspace"]["package"]["version"]
    check_loader_policy(source, revision)
    return tree, cargo_lock, version


def require_rpath(patchelf: Path, binary: Path, expected: str) -> None:
    if run([str(patchelf), "--print-rpath", str(binary)]) != expected:
        fail(f"{binary.name} retains an unexpected runtime search path")


def portabilize(bin_dir: Path, names: tuple[str, ...], output: Path, patchelf: Path, readelf: Path, ldd: Path,
                expected_files: dict[str, dict]) -> tuple[list[dict], str]:
    """Rewrite executable RPATHs and bundle the non-glibc shared-library closure."""
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
        recorded = expected_files.get(f"bin/{name}")
        if (not isinstance(recorded, dict) or recorded.get("bytes") != digest_bytes
                or recorded.get("sha256") != digest):
            fail(f"{name} differs from the T1 artifact manifest")
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
        require_rpath(patchelf, packaged, "$ORIGIN/../lib")
        packaged_bytes, packaged_digest = admit_file_digest(packaged, MAX_BINARY_BYTES)
        executables.append({
            "name": name,
            "packaged_path": f"bin/{name}",
            "sha256": packaged_digest,
            "bytes": packaged_bytes,
            "t1_artifact_sha256": digest,
            "t1_artifact_bytes": digest_bytes,
            "required_glibc_versions": patched["required_glibc_versions"],
        })

    for soname, record in libraries.items():
        packaged = root / "lib" / soname
        copy_admitted_file(record["source_path"], packaged, record["source_sha256"], record["source_bytes"], 96 * 1024 * 1024)
        packaged.chmod(0o755)
        run([str(patchelf), "--set-rpath", "$ORIGIN", str(packaged)])
        require_rpath(patchelf, packaged, "$ORIGIN")
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


def write_build_manifest(output: Path, revision: str, tree: str, cargo_lock: str, executables: list[dict],
                         profile: str = "t1") -> Path:
    manifest = {
        "schema": 1,
        "source": {"git_revision": revision, "git_tree": tree, "cargo_lock_sha256": cargo_lock},
        "profile": profile,
        "target": TARGET,
        "executables": {e["name"]: {
            "path": e["packaged_path"], "sha256": e["sha256"], "bytes": e["bytes"],
            "t1_artifact_sha256": e["t1_artifact_sha256"],
            "t1_artifact_bytes": e["t1_artifact_bytes"],
        } for e in executables},
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
    inputs = parser.add_mutually_exclusive_group(required=True)
    inputs.add_argument("--t1-artifact-dir", type=Path, help="T1's published artifact root (manifest.json + bin/)")
    inputs.add_argument("--release-dir", type=Path, help="cargo release output (target/release) built from --source HEAD")
    parser.add_argument("--source", required=True, type=Path, help="clean checkout at the binaries' exact source revision")
    parser.add_argument("--output", required=True, type=Path, help="new candidate output directory")
    parser.add_argument("--patchelf", required=True, type=Path)
    parser.add_argument("--readelf", required=True, type=Path)
    parser.add_argument("--ldd", required=True, type=Path)
    parser.add_argument("--strip", type=Path, help="strip tool; required with --release-dir")
    parser.add_argument("--gui-library-path", help="colon-separated directories holding the desktop's GUI libraries; "
                        "bundles them and adds the nudox, nudox-desktop and nudox-mcp launchers")
    args = parser.parse_args()

    output = args.output.resolve()
    if output.exists() or output == args.source.resolve() or args.source.resolve() in output.parents:
        fail("output directory must be new and outside the source checkout")

    if args.t1_artifact_dir is not None:
        t1_manifest = load_t1_manifest(args.t1_artifact_dir)
        revision = t1_manifest["source_sha"]
    else:
        if args.strip is None:
            fail("--release-dir requires --strip")
        revision = git(args.source, "rev-parse", "HEAD")
    tree, cargo_lock, version = check_source(args.source, revision)

    output.mkdir(mode=0o700, parents=True)
    patchelf, readelf, ldd = require_tool(args.patchelf, "patchelf"), require_tool(args.readelf, "readelf"), require_tool(args.ldd, "ldd")
    if args.t1_artifact_dir is not None:
        bin_dir, profile = args.t1_artifact_dir / "bin", "t1"
        expected_files = {item["path"]: item for item in t1_manifest["files"]}
    else:
        bin_dir, profile = output / "stage", "release"
        expected_files = stage_release_build(args.release_dir, bin_dir, require_tool(args.strip, "strip"))
    executables, minimum_glibc = portabilize(bin_dir, EXECUTABLES, output, patchelf, readelf, ldd, expected_files)
    if profile == "release":
        shutil.rmtree(bin_dir)
    if args.gui_library_path is not None:
        search = [Path(item) for item in args.gui_library_path.split(":") if item]
        bundle_gui_libraries(output / "Nudox" / "lib", search, patchelf, readelf, minimum_glibc)
        write_launchers(output / "Nudox")
    build_manifest_path = write_build_manifest(output, revision, tree, cargo_lock, executables, profile)
    normalize_modes(output / "Nudox")
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
        "minimum_os": f"{MINIMUM_OS} {minimum_glibc}",
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
