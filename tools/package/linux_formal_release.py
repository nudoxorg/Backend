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
import time

import linux_release_package as checkpoint

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
# Never bundled: their nixpkgs builds need glibc 2.42 (GLIBC_ABI_GNU2_TLS) and a
# host copy is older than what our other libraries link against (glib needs
# util-linux 2.40, libatopology ALSA 1.2.15; Ubuntu 24.04 has 2.39 and 1.2.11).
# Nothing in Backend uses them; anything that does takes the host's copy.
HOST_SONAMES = frozenset({"libasound.so.2", "libatopology.so.2", "libblkid.so.1", "libmount.so.1", "libselinux.so.1"})
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
                expected_files: dict[str, dict], companion: dict | None = None) -> tuple[list[dict], str]:
    """Rewrite executable RPATHs and bundle the non-glibc shared-library closure."""
    root = output / "Nudox"
    (root / "bin").mkdir(mode=0o700, parents=True)
    (root / "lib").mkdir(mode=0o700)
    libraries: dict[str, dict] = {}
    library_total_bytes = 0
    executables: list[dict] = []
    for name in names:
        pre_relocated = companion is not None and name in {"backend-stylo-worker", "backend-sqlite-worker"}
        if companion is not None and name in {"backend-locald", "backend-desktop"}:
            source_binary = Path(companion["owner"]["path"] if name == "backend-locald" else companion["desktop"]["artifact"]["path"])
            source_record = companion["owner"] if name == "backend-locald" else companion["desktop"]["artifact"]
        elif pre_relocated:
            source_record = companion["worker"] if name == "backend-stylo-worker" else companion["sqlite"]["worker"]
            source_binary = Path(source_record["path"])
        else:
            source_binary = (bin_dir / name).resolve(strict=True)
            source_record = expected_files.get(f"bin/{name}")
        packaged = root / "bin" / name
        digest_bytes, digest = admit_file_digest(source_binary, MAX_BINARY_BYTES)
        recorded = source_record
        if (not isinstance(recorded, dict) or recorded.get("bytes") != digest_bytes
                or recorded.get("sha256") != digest):
            fail(f"{name} differs from the T1 artifact manifest")
        copy_admitted_file(source_binary, packaged, digest, digest_bytes, MAX_BINARY_BYTES)
        original = elf_info(readelf, packaged)
        if not original["interpreter"]:
            fail(f"{name} has no ELF interpreter")
        packaged.chmod(0o755)
        if pre_relocated:
            from record_stylo_owner_build import bounded_process
            sqlite = companion["sqlite"]
            loader = companion["source_loader"]["path"] if name == "backend-stylo-worker" else sqlite["tools"]["loader"]["path"]
            raw = companion["formal_raw_stylo"] if name == "backend-stylo-worker" else sqlite["worker_raw"]["path"]
            linkage = bounded_process([loader, "--list", raw], time.monotonic() + 30, companion["observer_environment"]).decode("utf-8")
        else:
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
        if not pre_relocated:
            run([str(patchelf), "--set-interpreter", INTERPRETER, "--set-rpath", "$ORIGIN/../lib", str(packaged)])
        patched = elf_info(readelf, packaged)
        if patched["interpreter"] != INTERPRETER or patched["needed"] != original["needed"]:
            fail(f"patchelf changed the wrong ELF contract for {name}")
        require_rpath(patchelf, packaged, "$ORIGIN/../lib")
        if pre_relocated and sha256(packaged) != source_record["sha256"]:
            fail("pre-relocated companion changed during formal assembly")
        packaged_bytes, packaged_digest = admit_file_digest(packaged, MAX_BINARY_BYTES)
        executables.append({
            "name": name,
            "packaged_path": f"bin/{name}",
            "sha256": packaged_digest,
            "bytes": packaged_bytes,
            "t1_artifact_sha256": digest,
            "t1_artifact_bytes": digest_bytes,
            "required_glibc_versions": patched["required_glibc_versions"],
            "companion_input": companion is not None and name in {"backend-locald", "backend-desktop", "backend-stylo-worker", "backend-sqlite-worker"},
        })

    for soname, record in libraries.items():
        packaged = root / "lib" / soname
        bound = companion is not None and soname == "libgcc_s.so.1"
        if bound:
            source = companion["dependency_source"]
            if (record["source_bytes"], record["source_sha256"]) != (source["bytes"], source["sha256"]):
                fail("formal package source libgcc differs from companion closure")
            dependency = companion["dependency"]
            copy_admitted_file(Path(dependency["path"]), packaged, dependency["sha256"], dependency["bytes"], 16 * 1024 * 1024)
        else:
            copy_admitted_file(record["source_path"], packaged, record["source_sha256"], record["source_bytes"], 96 * 1024 * 1024)
        packaged.chmod(0o755)
        if not bound:
            run([str(patchelf), "--set-rpath", "$ORIGIN", str(packaged)])
        if bound and sha256(packaged) != companion["dependency"]["sha256"]:
            fail("formal package changed the companion libgcc")
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
                         profile: str = "t1", companion_evidence: dict | None = None) -> Path:
    manifest = {
        "schema": 1,
        "source": {"git_revision": revision, "git_tree": tree, "cargo_lock_sha256": cargo_lock},
        "profile": profile,
        "target": TARGET,
        "executables": {e["name"]: {
            "path": e["packaged_path"], "sha256": e["sha256"], "bytes": e["bytes"],
            **({"companion_artifact_sha256": e["t1_artifact_sha256"], "companion_artifact_bytes": e["t1_artifact_bytes"]}
               if e.get("companion_input") else
               {"t1_artifact_sha256": e["t1_artifact_sha256"], "t1_artifact_bytes": e["t1_artifact_bytes"]}),
        } for e in executables},
    }
    if companion_evidence is not None:
        manifest["companion"] = companion_evidence
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
    for name in ("companion-owner-receipt", "companion-cli-manifest", "stylo-worker", "stylo-worker-receipt"):
        parser.add_argument("--" + name, type=Path, help="source-bound companion input for SQLite and Stylo")
    args = parser.parse_args()
    companion_inputs = (args.companion_owner_receipt, args.companion_cli_manifest, args.stylo_worker, args.stylo_worker_receipt)
    if any(companion_inputs) and not all(companion_inputs):
        fail("formal companion inputs must be provided together")

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

    companion = None
    companion_receipt_digest = None
    if all(companion_inputs):
        cli_bytes = read_regular_bytes(args.companion_cli_manifest, 16 * 1024 * 1024, "companion CLI manifest")
        cli_build = parse_json_bytes(cli_bytes, "companion CLI manifest")
        cli_receipt = checkpoint.admit_receipt(cli_build)
        with checkpoint.clean_git_environment():
            selected = checkpoint.check_source(args.source, cli_build, cli_receipt)
        if selected[:3] != (revision, tree, cargo_lock):
            fail("formal and companion sources differ")
        worker, worker_digest = checkpoint.admit_stylo_worker(args.stylo_worker, args.stylo_worker_receipt, revision, tree, cargo_lock)
        worker_raw = read_regular_bytes(args.stylo_worker_receipt, 65536, "Stylo worker receipt")
        if hashlib.sha256(worker_raw).hexdigest() != worker_digest:
            fail("Stylo worker receipt changed during admission")
        worker_receipt = parse_json_bytes(worker_raw, "Stylo worker receipt")
        private = [worker_receipt["source_path"], *[worker_receipt["environment"][key] for key in ("CARGO_HOME", "CARGO_TARGET_DIR", "CARGO_BUILD_BUILD_DIR")]]
        companion, companion_receipt_digest = checkpoint.admit_stylo_bound_owner(args.companion_owner_receipt,
            source=(revision, tree, cargo_lock), cli_record=cli_build["executables"]["backend-cli"],
            cli_manifest_sha=hashlib.sha256(cli_bytes).hexdigest(), raw_worker=worker,
            worker_receipt_sha=worker_digest, worker_private_paths=private)
        if companion.get("sqlite") is None or companion.get("desktop") is None:
            fail("formal companion receipt must bind SQLite to both desktop and locald")
        if (companion["sqlite"]["dependency_source"]["bytes"], companion["sqlite"]["dependency_source"]["sha256"]) != (companion["dependency_source"]["bytes"], companion["dependency_source"]["sha256"]):
            fail("formal companion workers require different source libgcc")
        companion["formal_raw_stylo"] = str(args.stylo_worker.resolve(strict=True))
    output.mkdir(mode=0o700, parents=True)
    patchelf, readelf, ldd = require_tool(args.patchelf, "patchelf"), require_tool(args.readelf, "readelf"), require_tool(args.ldd, "ldd")
    if args.t1_artifact_dir is not None:
        bin_dir, profile = args.t1_artifact_dir / "bin", "t1"
        expected_files = {item["path"]: item for item in t1_manifest["files"]}
    else:
        bin_dir, profile = output / "stage", "release"
        expected_files = stage_release_build(args.release_dir, bin_dir, require_tool(args.strip, "strip"))
    names = (*EXECUTABLES, "backend-stylo-worker", "backend-sqlite-worker") if companion else EXECUTABLES
    executables, minimum_glibc = portabilize(bin_dir, names, output, patchelf, readelf, ldd, expected_files, companion)
    if profile == "release":
        shutil.rmtree(bin_dir)
    if args.gui_library_path is not None:
        search = [Path(item) for item in args.gui_library_path.split(":") if item]
        bundle_gui_libraries(output / "Nudox" / "lib", search, patchelf, readelf, minimum_glibc)
        write_launchers(output / "Nudox")
    if companion is not None:
        from record_stylo_owner_build import observe, bounded_process, require_runpath
        import sqlite_macos_development as sqlite_common
        root = output / "Nudox"
        env = companion["observer_environment"]
        library = root / "lib/libgcc_s.so.1"
        stylo = root / "bin/backend-stylo-worker"
        sqlite_worker = root / "bin/backend-sqlite-worker"
        sqlite = companion["sqlite"]
        require_runpath(readelf, library, "$ORIGIN", time.monotonic() + 30, env)
        for worker in (stylo, sqlite_worker):
            require_runpath(readelf, worker, "$ORIGIN/../lib", time.monotonic() + 30, env)
        if observe(Path(companion["observer"]["path"]), stylo, library, time.monotonic() + 30, env) != companion["worker_anchor"]:
            fail("formal Stylo closure differs from compiled owner anchor")
        observed = bounded_process([sqlite["observer"]["path"], str(sqlite_worker), str(library)], time.monotonic() + 30, env, sqlite_common.MAX_BASIS_BYTES)
        if sqlite_common.validate_observation(json.loads(observed), sqlite["sqlite_lock_sha256"]) != (sqlite["artifact_identity"], sqlite["basis_json"]):
            fail("formal SQLite closure differs from compiled owner basis")
        for path, record in ((stylo, companion["worker"]), (sqlite_worker, sqlite["worker"]), (library, companion["dependency"]), (Path(companion["observer"]["path"]), companion["observer"]), (Path(sqlite["observer"]["path"]), sqlite["observer"])):
            if admit_file_digest(path, MAX_BINARY_BYTES) != (record["bytes"], record["sha256"]):
                fail("formal companion or observer changed during observation")
        for name in ("backend-locald", "backend-desktop"):
            record = next(item for item in executables if item["name"] == name)
            if admit_file_digest(root / "bin" / name, MAX_BINARY_BYTES) != (record["bytes"], record["sha256"]):
                fail("formal owner changed during companion observation")
        if hashlib.sha256(read_regular_bytes(args.companion_owner_receipt, 65536, "companion receipt")).hexdigest() != companion_receipt_digest:
            fail("formal companion receipt changed during observation")
    evidence = None
    if companion is not None:
        if checkpoint.admit_stylo_worker(args.stylo_worker, args.stylo_worker_receipt, revision, tree, cargo_lock)[1] != worker_digest:
            fail("formal raw worker changed during assembly")
        _, final_receipt_digest = checkpoint.admit_stylo_bound_owner(args.companion_owner_receipt,
            source=(revision, tree, cargo_lock), cli_record=cli_build["executables"]["backend-cli"],
            cli_manifest_sha=hashlib.sha256(cli_bytes).hexdigest(), raw_worker=worker,
            worker_receipt_sha=worker_digest, worker_private_paths=private)
        if final_receipt_digest != companion_receipt_digest:
            fail("formal companion receipt changed during assembly")
        evidence = {"owner_receipt_sha256": companion_receipt_digest,
                    "stylo_anchor": companion["worker_anchor"],
                    "sqlite_artifact_identity": companion["sqlite"]["artifact_identity"],
                    "sqlite_lock_sha256": companion["sqlite"]["sqlite_lock_sha256"],
                    "dependency_sha256": companion["dependency"]["sha256"]}
    build_manifest_path = write_build_manifest(output, revision, tree, cargo_lock, executables, profile, evidence)
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
