#!/usr/bin/env python3
"""Package receipted Linux CLI/MCP/locald ELFs with their non-glibc closure."""
from __future__ import annotations

import argparse
import datetime as dt
import gzip
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tarfile
import tomllib

BINARIES = ("backend-cli", "backend-mcp", "backend-locald")
TARGET = "x86_64-unknown-linux-gnu"
INTERPRETER = "/lib64/ld-linux-x86-64.so.2"
TAG_RE = re.compile(r"checkpoint-[0-9]{8}-[a-f0-9]{10}-linux-x64\Z")
GLIBC_SONAMES = {
    "libc.so.6", "libm.so.6", "libdl.so.2", "libpthread.so.0", "librt.so.1",
    "libresolv.so.2", "libutil.so.1", "libanl.so.1", "ld-linux-x86-64.so.2",
}
NEEDED = re.compile(r"^\s*(\S+) => (\S+) \(", re.MULTILINE)
GLIBC_VERSION = re.compile(r"GLIBC_([0-9]+\.[0-9]+)")


def fail(message: str) -> None:
    raise ValueError(message)


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def run(command: list[str]) -> str:
    try:
        return subprocess.check_output(command, text=True, stderr=subprocess.STDOUT).strip()
    except (OSError, subprocess.CalledProcessError) as error:
        fail(f"tool failed: {' '.join(command)}: {error}")


def ldd_dependencies(linkage: str) -> list[tuple[str, str]]:
    """Accept sonames and exclude ldd's path-qualified loader alias row."""
    dependencies = []
    for soname, raw_path in NEEDED.findall(linkage):
        if "/" in soname:
            if (soname.startswith("/") and raw_path.startswith("/")
                    and Path(soname).name == Path(INTERPRETER).name
                    and Path(raw_path).name == Path(INTERPRETER).name):
                continue
            fail(f"ldd contains an unsafe path-qualified dependency: {soname}")
        if soname in (".", "..") or "\\" in soname:
            fail(f"ldd contains an unsafe dependency name: {soname}")
        dependencies.append((soname, raw_path))
    return dependencies


def load_json(path: Path, label: str) -> dict:
    if path.is_symlink() or not path.is_file() or path.stat().st_size > 16 * 1024 * 1024:
        fail(f"{label} must be a regular file under 16 MiB")
    try:
        value = json.loads(path.read_text())
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as error:
        fail(f"cannot read {label}: {error}")
    if not isinstance(value, dict):
        fail(f"{label} must contain a JSON object")
    return value


def require_tool(path: Path, label: str) -> Path:
    path = path.resolve(strict=True)
    if not path.is_file() or not os.access(path, os.X_OK):
        fail(f"{label} is not an executable file: {path}")
    return path


def version_tuple(value: str) -> tuple[int, ...]:
    return tuple(int(part) for part in value.split("."))


def elf_info(readelf: Path, path: Path) -> dict:
    header = run([str(readelf), "-h", str(path)])
    if "Class:" not in header or "ELF64" not in header or "Machine:" not in header or "Advanced Micro Devices X86-64" not in header:
        fail(f"not an x86_64 ELF image: {path}")
    dynamic = run([str(readelf), "-d", str(path)])
    needed = re.findall(r"\(NEEDED\).*\[(.*?)\]", dynamic)
    if any("/" in name for name in needed):
        fail(f"ELF image has a path-qualified DT_NEEDED entry: {path}")
    interpreter = run([str(readelf), "-l", str(path)])
    interpreter_match = re.search(r"Requesting program interpreter: ([^]]+)", interpreter)
    versions = sorted(set(GLIBC_VERSION.findall(run([str(readelf), "--version-info", str(path)]))), key=version_tuple)
    return {"needed": needed, "interpreter": interpreter_match.group(1) if interpreter_match else None, "required_glibc_versions": versions}


def check_source(source: Path, build: dict, receipt: dict) -> tuple[str, str, str, str]:
    source = source.resolve(strict=True)
    metadata = build.get("source")
    if not isinstance(metadata, dict):
        fail("build manifest has no source identity")
    revision = metadata.get("commit") or metadata.get("git_revision")
    tree = metadata.get("tree") or metadata.get("git_tree")
    lock_sha = receipt.get("source", {}).get("cargo_lock_sha256")
    for label, value in (("source commit", revision), ("source tree", tree)):
        if not isinstance(value, str) or not re.fullmatch(r"[a-f0-9]{40}", value):
            fail(f"build manifest has an invalid {label}")
    if not isinstance(lock_sha, str) or not re.fullmatch(r"[a-f0-9]{64}", lock_sha):
        fail("build manifest has an invalid Cargo.lock hash")
    actual = run(["git", "-C", str(source), "rev-parse", "HEAD"])
    dirty = run(["git", "-C", str(source), "status", "--porcelain"])
    if dirty:
        fail("source checkout is dirty")
    if subprocess.run(["git", "-C", str(source), "merge-base", "--is-ancestor", revision, actual], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL).returncode != 0:
        fail("build source is not an ancestor of the clean packaging checkout")
    selected_tree = run(["git", "-C", str(source), "rev-parse", revision + "^{tree}"])
    if selected_tree != tree:
        fail("build manifest tree does not match its selected source commit")
    selected_lock = subprocess.check_output(["git", "-C", str(source), "show", f"{revision}:Cargo.lock"])
    if hashlib.sha256(selected_lock).hexdigest() != lock_sha:
        fail("source Cargo.lock differs from build manifest")
    selected_cargo = subprocess.check_output(["git", "-C", str(source), "show", f"{revision}:Cargo.toml"], text=True)
    version = tomllib.loads(selected_cargo)["workspace"]["package"]["version"]
    if not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", version):
        fail("source workspace version is not stable semver")
    return revision, tree, lock_sha, version


def admit_receipt(build: dict) -> dict:
    record = build.get("root_receipt")
    source = build.get("source")
    if not isinstance(record, dict) or not isinstance(source, dict):
        fail("build manifest omits source or root receipt")
    receipt_path = Path(record.get("path", ""))
    if not receipt_path.is_absolute() or not receipt_path.is_file() or sha256(receipt_path) != record.get("sha256"):
        fail("root build receipt is missing or differs from its recorded hash")
    receipt = load_json(receipt_path, "root build receipt")
    build_source = build["source"]
    receipt_source = receipt.get("source", {})
    if receipt.get("exit") != 0 or receipt_source.get("clean_before") is not True or receipt_source.get("clean_after") is not True or receipt.get("toolchain", {}).get("unchanged") is not True or build_source.get("clean") is not True:
        fail("root build receipt does not attest a successful unchanged-source build")
    if any(receipt_source.get(key) != build_source.get(key) for key in ("commit", "tree")):
        fail("root build receipt commit/tree differs from build manifest")
    return receipt


def public_build_manifest(build: dict) -> dict:
    """Retain build identity and hashes while omitting machine-local paths."""
    if build.get("schema") != "nudox.runtime-build-manifest.v1":
        fail("build manifest has an unsupported runtime-artifact schema")
    source = build.get("source")
    if not isinstance(source, dict) or not all(key in source for key in ("commit", "tree", "clean")):
        fail("build manifest omits its source identity")
    executables = build.get("executables")
    if not isinstance(executables, dict) or set(executables) != set(BINARIES):
        fail("build manifest omits or adds executable identities")
    public_executables = {}
    for name in BINARIES:
        record = executables[name]
        if not isinstance(record, dict) or not re.fullmatch(r"[a-f0-9]{64}", record.get("sha256", "")):
            fail(f"build manifest has an invalid {name} hash")
        if not isinstance(record.get("bytes"), int) or isinstance(record["bytes"], bool) or record["bytes"] <= 0:
            fail(f"build manifest has an invalid {name} size")
        public_executables[name] = {"sha256": record["sha256"], "bytes": record["bytes"]}
    receipt = build.get("root_receipt")
    if not isinstance(receipt, dict) or receipt.get("schema") != "nudox.runtime-artifact-build-receipt.v1" or not re.fullmatch(r"[a-f0-9]{64}", receipt.get("sha256", "")):
        fail("build manifest has no hash-bound runtime artifact receipt")
    return {
        "schema": build["schema"],
        "source": {"commit": source["commit"], "tree": source["tree"], "clean": source["clean"]},
        "executables": public_executables,
        "root_receipt": {"schema": receipt["schema"], "sha256": receipt["sha256"]},
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--manifest", required=True, type=Path, help="successful source-bound Linux build manifest")
    parser.add_argument("--source", required=True, type=Path, help="exact clean build checkout")
    parser.add_argument("--output", required=True, type=Path, help="new candidate output directory")
    parser.add_argument("--patchelf", required=True, type=Path)
    parser.add_argument("--readelf", required=True, type=Path)
    parser.add_argument("--ldd", required=True, type=Path)
    parser.add_argument("--release-tag", help="immutable checkpoint tag; defaults to today's UTC date and source revision")
    args = parser.parse_args()

    manifest_path = args.manifest.resolve(strict=True)
    build = load_json(manifest_path, "build manifest")
    receipt = admit_receipt(build)
    revision, tree, lock_sha, version = check_source(args.source, build, receipt)
    release_tag = args.release_tag or f"checkpoint-{dt.datetime.now(dt.timezone.utc):%Y%m%d}-{revision[:10]}-linux-x64"
    if not TAG_RE.fullmatch(release_tag) or not release_tag.endswith("-" + revision[:10] + "-linux-x64"):
        fail("release tag must be checkpoint-YYYYMMDD-<source-10>-linux-x64 for the receipted source")
    patchelf = require_tool(args.patchelf, "patchelf")
    readelf = require_tool(args.readelf, "readelf")
    ldd = require_tool(args.ldd, "ldd")
    output = args.output.resolve()
    if output.exists() or output == args.source.resolve() or args.source.resolve() in output.parents:
        fail("output directory must be new and outside the source checkout")
    output.mkdir(mode=0o700, parents=True)
    root = output / "nudox-linux-x86_64"
    (root / "bin").mkdir(mode=0o700, parents=True)
    (root / "lib").mkdir(mode=0o700)

    sources: dict[str, Path] = {}
    artifact_records = build.get("executables")
    if not isinstance(artifact_records, dict):
        fail("build manifest omits executable records")
    for name in BINARIES:
        record = artifact_records.get(name)
        if not isinstance(record, dict):
            fail(f"build manifest omits {name}")
        source_binary = Path(record.get("path", ""))
        if not source_binary.is_absolute() or source_binary.is_symlink() or not source_binary.is_file() or sha256(source_binary) != record.get("sha256"):
            fail(f"{name} differs from the successful build receipt")
        sources[name] = source_binary.resolve(strict=True)

    glibc_family = set(GLIBC_SONAMES)
    libraries: dict[str, dict] = {}
    executables: list[dict] = []
    for name, source_binary in sources.items():
        original = elf_info(readelf, source_binary)
        if not original["interpreter"]:
            fail(f"{name} has no ELF interpreter")
        packaged = root / "bin" / name
        shutil.copyfile(source_binary, packaged)
        packaged.chmod(0o755)
        linkage = run([str(ldd), str(source_binary)])
        (output / f"{name}.ldd.txt").write_text(linkage + "\n")
        for soname, raw_path in ldd_dependencies(linkage):
            if soname in glibc_family:
                continue
            if raw_path == "not":
                fail(f"{name} has an unresolved shared dependency: {soname}")
            dependency = Path(raw_path)
            if not dependency.is_absolute() or not dependency.is_file():
                fail(f"could not resolve non-glibc dependency {soname}: {raw_path}")
            dependency = dependency.resolve(strict=True)
            digest = sha256(dependency)
            previous = libraries.get(soname)
            if previous and previous["source_sha256"] != digest:
                fail(f"different dependencies use the same ELF soname {soname}")
            libraries.setdefault(soname, {"source_path": dependency, "source_sha256": digest})
        run([str(patchelf), "--set-interpreter", INTERPRETER, "--set-rpath", "$ORIGIN/../lib", str(packaged)])
        patched = elf_info(readelf, packaged)
        if patched["interpreter"] != INTERPRETER or patched["needed"] != original["needed"]:
            fail(f"patchelf changed the wrong ELF contract for {name}")
        if run([str(patchelf), "--print-rpath", str(packaged)]) != "$ORIGIN/../lib":
            fail(f"{name} retains an unexpected runtime search path")
        executables.append({"name": name, "original_sha256": sha256(source_binary), "packaged_path": f"bin/{name}", "packaged_sha256": sha256(packaged), "packaged_bytes": packaged.stat().st_size, "original_elf": {"needed": original["needed"], "interpreter_present": bool(original["interpreter"]), "required_glibc_versions": original["required_glibc_versions"]}, "packaged_elf": patched})

    for soname, record in libraries.items():
        packaged = root / "lib" / soname
        shutil.copyfile(record["source_path"], packaged)
        packaged.chmod(0o755)
        run([str(patchelf), "--set-rpath", "$ORIGIN", str(packaged)])
        if run([str(patchelf), "--print-rpath", str(packaged)]) != "$ORIGIN":
            fail(f"{soname} retains an unexpected runtime search path")
        info = elf_info(readelf, packaged)
        record.update({"packaged_path": f"lib/{soname}", "packaged_sha256": sha256(packaged), "packaged_bytes": packaged.stat().st_size, "needed": info["needed"], "required_glibc_versions": info["required_glibc_versions"]})
        record.pop("source_path")

    for record in executables:
        if any(item not in glibc_family and item not in libraries for item in record["packaged_elf"]["needed"]):
            fail(f"{record['name']} has a dynamic dependency outside the bundled/system closure")
    for soname, record in libraries.items():
        missing = [item for item in record["needed"] if item not in glibc_family and item not in libraries and not item.startswith("ld-linux-")]
        if missing:
            fail(f"{soname} has dependencies outside the bundled/system closure: {missing}")
    glibc_versions = [value for record in executables for value in record["packaged_elf"]["required_glibc_versions"]]
    glibc_versions.extend(value for record in libraries.values() for value in record["required_glibc_versions"])
    if not glibc_versions:
        fail("ELF dependency scan found no glibc symbol floor")
    minimum_glibc = max(glibc_versions, key=version_tuple)

    public_build = public_build_manifest(build)
    (root / "build-manifest.json").write_text(json.dumps(public_build, indent=2, sort_keys=True) + "\n")
    package_tools = Path(__file__).resolve().parent
    installer_source = (package_tools / "install_linux.py").read_text()
    (output / "install-linux-x64-channel.py").write_text(installer_source)
    shutil.copyfile(package_tools / "native-qa-linux-x64.example.json", output / "native-qa-linux-x64.json")
    package = {
        "schema": "nudox.linux-portable-package.v1",
        "release_tag": release_tag,
        "source": build["source"],
        "original_build_manifest_sha256": sha256(manifest_path),
        "packager_path": "tools/package/linux_release_package.py",
        "packager_sha256": sha256(Path(__file__).resolve()),
        "patchelf_tool": "patchelf",
        "patchelf_version": run([str(patchelf), "--version"]),
        "patchelf_sha256": sha256(patchelf),
        "readelf_tool": "readelf",
        "readelf_version": run([str(readelf), "--version"]).splitlines()[0],
        "readelf_sha256": sha256(readelf),
        "ldd_tool": "ldd",
        "ldd_sha256": sha256(ldd),
        "created_at_utc": dt.datetime.now(dt.timezone.utc).isoformat(),
        "required_glibc": minimum_glibc,
        "executables": executables,
        "libraries": libraries,
        "glibc_libraries_bundled": False,
        "runtime_acceptance": "external_native_qa_record_required",
    }
    (root / "packaging-manifest.json").write_text(json.dumps(package, indent=2, sort_keys=True) + "\n")
    readme = f"""NuDox Linux x86_64 CLI/MCP/locald\n\nSource revision: {revision}\nRequires Linux x86_64 with glibc >= {minimum_glibc} and {INTERPRETER}.\nThe CLI, MCP and locald executables must remain siblings in bin/.\n\nVerify the supplied SHA-256 sidecar before extracting. For manual use:\n  tar -xzf nudox-linux-x86_64-{revision[:10]}.tar.gz\n  export PATH=\"$PWD/nudox-linux-x86_64/bin:$PATH\"\n  backend-cli --help\n  backend-mcp --help\n\nLanguage compiler integrations are discovered separately from this CLI package.\nTypeScript indexing uses host Node and the project's installed typescript package when configured.\nDefault project discovery does not require hidden NUDOX_* environment variables.\n"""
    (root / "README.txt").write_text(readme)

    archive_name = f"nudox-linux-x86_64-{revision[:10]}.tar.gz"
    archive = output / archive_name
    with archive.open("xb") as raw, gzip.GzipFile(filename="", mode="wb", fileobj=raw, mtime=0) as compressed, tarfile.open(fileobj=compressed, mode="w") as bundle:
        for path in (root, *sorted(root.rglob("*"))):
            if path.is_symlink():
                fail(f"package contains an unexpected symbolic link: {path}")
            info = bundle.gettarinfo(str(path), arcname=path.relative_to(output).as_posix())
            info.uid = info.gid = 0
            info.uname = info.gname = ""
            info.mtime = 0
            if path.is_file():
                with path.open("rb") as stream:
                    bundle.addfile(info, stream)
            else:
                bundle.addfile(info)
    archive_hash = sha256(archive)
    (output / f"{archive_name}.sha256").write_text(f"{archive_hash}  {archive_name}\n")
    release = {
        "schema": 1,
        "version": version,
        "source_sha": revision,
        "source_tree": tree,
        "cargo_lock_sha256": lock_sha,
        "platform": "linux-x64",
        "target": TARGET,
        "asset": archive_name,
        "sha256": archive_hash,
        "size_bytes": archive.stat().st_size,
        "minimum_glibc": minimum_glibc,
        "build_manifest_sha256": sha256(root / "build-manifest.json"),
        "packaging_manifest_sha256": sha256(root / "packaging-manifest.json"),
    }
    (output / "release-manifest.json").write_text(json.dumps(release, indent=2, sort_keys=True) + "\n")
    candidate_installer = installer_source.replace('PINNED_RELEASE_TAG = ""', f'PINNED_RELEASE_TAG = "{release_tag}"', 1)
    candidate_installer = candidate_installer.replace('PINNED_MANIFEST_SHA256 = ""', f'PINNED_MANIFEST_SHA256 = "{sha256(output / "release-manifest.json")}"', 1)
    if candidate_installer == installer_source or 'PINNED_MANIFEST_SHA256 = ""' in candidate_installer:
        fail("could not construct digest-pinned checkpoint installer")
    (output / "install-linux-x64.py").write_text(candidate_installer)
    print(json.dumps({"release_tag": release_tag, "archive": str(archive), "sha256": archive_hash, "size_bytes": archive.stat().st_size, "minimum_glibc": minimum_glibc, "bundled_libraries": sorted(libraries), "release_manifest": str(output / "release-manifest.json"), "installer": str(output / "install-linux-x64.py")}, indent=2))
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (ValueError, OSError, KeyError, TypeError) as error:
        print(f"Linux package stopped: {error}", file=sys.stderr)
        raise SystemExit(1)
