#!/usr/bin/env python3
"""Package receipted Linux CLI/MCP/locald ELFs with their non-glibc closure.

TypeScript uses linked TSZ and embedded declarations by default; no Node/SDK payload
is required. An optional legacy Compiler API payload requires both SDK flags.
"""
from __future__ import annotations

import argparse
import datetime as dt
import gzip
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import shutil
import stat
import subprocess
import sys
import tarfile
import tomllib

BINARIES = ("backend-cli", "backend-mcp", "backend-locald")
DESKTOP_ROLE = {"executable": "backend-desktop", "launcher": "nudox-desktop"}
MAX_BINARY_BYTES = 512 * 1024 * 1024
MAX_TYPESCRIPT_PACKAGE_BYTES = 96 * 1024 * 1024
MAX_NODE_LIBRARY_BYTES = 512 * 1024 * 1024
MAX_NODE_LIBRARIES = 64
TARGET = "x86_64-unknown-linux-gnu"
INTERPRETER = "/lib64/ld-linux-x86-64.so.2"
TAG_RE = re.compile(r"checkpoint-[0-9]{8}-[a-f0-9]{10}-linux-x64\Z")
# Shared with native standalone SDK admission; one finite loader classification.
LINUX_SYSTEM_SONAMES_BYTES = Path(__file__).with_name("linux_system_sonames.txt").read_bytes()
GLIBC_SONAMES = frozenset(LINUX_SYSTEM_SONAMES_BYTES.decode("ascii").splitlines())

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


def checkpoint_shell(release_tag: str, installer: bytes, minimum_glibc: str) -> bytes:
    """Pin the saved-file launcher to this invocation's completed installer bytes."""
    if not isinstance(release_tag, str) or not TAG_RE.fullmatch(release_tag):
        fail("checkpoint shell requires a canonical Linux release tag")
    if not isinstance(installer, bytes) or not 0 < len(installer) <= 16 * 1024 * 1024:
        fail("checkpoint shell requires a bounded nonempty Python installer")
    if not isinstance(minimum_glibc, str) or not re.fullmatch(r"(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)", minimum_glibc):
        fail("checkpoint shell requires the measured glibc version")
    template = Path(__file__).with_name("install_linux_checkpoint.sh.in").read_text()
    substitutions = {
        "@INSTALLER_URL@": f"https://github.com/nudoxorg/releases/releases/download/{release_tag}/install-linux-x64.py",
        "@INSTALLER_SHA256@": hashlib.sha256(installer).hexdigest(),
        "@INSTALLER_BYTES@": str(len(installer)),
        "@MINIMUM_GLIBC@": minimum_glibc,
        "@MINIMUM_GLIBC_TUPLE@": minimum_glibc.replace(".", ","),
    }
    for token, value in substitutions.items():
        if token not in template:
            fail("checkpoint shell template omits " + token)
        template = template.replace(token, value)
    if re.search(r"@[A-Z][A-Z0-9_]*@", template):
        fail("checkpoint shell template contains an unfilled placeholder")
    return template.encode("utf-8")


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


def read_regular_bytes(path: Path, maximum: int, label: str) -> bytes:
    """Read one bounded regular object without following its final link or waiting on a FIFO."""
    try:
        descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
        with os.fdopen(descriptor, "rb") as stream:
            before = os.fstat(stream.fileno())
            if not stat.S_ISREG(before.st_mode) or before.st_size > maximum:
                fail(f"{label} must be a regular file under its byte bound")
            value = stream.read(maximum + 1)
            after = os.fstat(stream.fileno())
            current = path.lstat()
            if len(value) > maximum or (before.st_dev, before.st_ino, before.st_size, before.st_mtime_ns, before.st_ctime_ns) != (after.st_dev, after.st_ino, after.st_size, after.st_mtime_ns, after.st_ctime_ns) or (current.st_dev, current.st_ino) != (after.st_dev, after.st_ino):
                fail(f"{label} changed during admission")
            return value
    except OSError as error:
        fail(f"cannot read {label}: {error}")


def parse_json_bytes(raw: bytes, label: str) -> dict:
    try:
        value = json.loads(raw)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        fail(f"cannot read {label}: {error}")
    if not isinstance(value, dict):
        fail(f"{label} must contain a JSON object")
    return value


def load_json(path: Path, label: str) -> dict:
    return parse_json_bytes(read_regular_bytes(path, 16 * 1024 * 1024, label), label)


def admit_file_digest(source: Path, maximum_bytes: int) -> tuple[int, str]:
    descriptor = os.open(source, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(descriptor, "rb") as incoming:
        before = os.fstat(incoming.fileno())
        if not stat.S_ISREG(before.st_mode) or before.st_size > maximum_bytes:
            fail("admitted file exceeds its regular-file byte bound")
        digest = hashlib.sha256()
        total = 0
        while total <= before.st_size:
            chunk = incoming.read(min(1024 * 1024, before.st_size - total + 1))
            if not chunk:
                break
            total += len(chunk)
            digest.update(chunk)
        after = os.fstat(incoming.fileno())
        current = source.lstat()
        identity = lambda value: (value.st_dev, value.st_ino, value.st_size, value.st_mtime_ns, value.st_ctime_ns)
        if total != before.st_size or identity(before) != identity(after) or identity(current) != identity(after):
            fail("admitted file changed during bounded hashing")
        return total, digest.hexdigest()


def copy_admitted_file(source: Path, target: Path, expected_digest: str, expected_bytes: int, maximum_bytes: int) -> None:
    """Copy from one regular FD and bind pre-relocation private-stage bytes to admission."""
    created = False
    try:
        descriptor = os.open(source, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
        with os.fdopen(descriptor, "rb") as incoming:
            before = os.fstat(incoming.fileno())
            if not isinstance(expected_bytes, int) or isinstance(expected_bytes, bool) or expected_bytes < 0 or expected_bytes > maximum_bytes:
                fail("admitted copy has an invalid original length or byte bound")
            if not stat.S_ISREG(before.st_mode) or before.st_size != expected_bytes or before.st_size > maximum_bytes:
                fail("admitted copy source differs from the original admitted length or byte bound")
            digest = hashlib.sha256()
            with target.open("xb") as outgoing:
                created = True
                copied = 0
                while copied <= before.st_size:
                    chunk = incoming.read(min(1024 * 1024, before.st_size - copied + 1))
                    if not chunk:
                        break
                    copied += len(chunk)
                    digest.update(chunk)
                    outgoing.write(chunk)
                if copied != before.st_size:
                    fail("admitted copy source size changed while copying")
            after = os.fstat(incoming.fileno())
            current = source.lstat()
            identity = lambda metadata: (metadata.st_dev, metadata.st_ino, metadata.st_size, metadata.st_mtime_ns, metadata.st_ctime_ns)
            if identity(before) != identity(after) or identity(current) != identity(after) or digest.hexdigest() != expected_digest or sha256(target) != expected_digest:
                fail("staged bytes differ from the original admitted digest or source object changed")
    except (OSError, ValueError):
        if created:
            target.unlink(missing_ok=True)
        raise


def check_loader_policy(source: Path, revision: str) -> None:
    try:
        compiled = subprocess.check_output(["git", "-C", str(source), "show", f"{revision}:tools/package/linux_system_sonames.txt"], stderr=subprocess.PIPE)
    except subprocess.CalledProcessError:
        fail("build source has no compiled Linux loader policy")
    if compiled != LINUX_SYSTEM_SONAMES_BYTES:
        fail("packager Linux loader policy differs from the exact compiled build source")


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
    check_loader_policy(source, revision)
    selected_cargo = subprocess.check_output(["git", "-C", str(source), "show", f"{revision}:Cargo.toml"], text=True)
    version = tomllib.loads(selected_cargo)["workspace"]["package"]["version"]
    if not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", version):
        fail("source workspace version is not stable semver")
    return revision, tree, lock_sha, version


def admit_receipt(build: dict, *, include_desktop: bool = False) -> dict:
    binaries = (*BINARIES, DESKTOP_ROLE["executable"]) if include_desktop else BINARIES
    record = build.get("root_receipt")
    source = build.get("source")
    if not isinstance(record, dict) or not isinstance(source, dict):
        fail("build manifest omits source or root receipt")
    receipt_path = Path(record.get("path", ""))
    if not receipt_path.is_absolute():
        fail("root build receipt path is not absolute")
    receipt_bytes = read_regular_bytes(receipt_path, 16 * 1024 * 1024, "root build receipt")
    if hashlib.sha256(receipt_bytes).hexdigest() != record.get("sha256"):
        fail("root build receipt is missing or differs from its recorded hash")
    receipt = parse_json_bytes(receipt_bytes, "root build receipt")
    if record.get("schema") != "nudox.runtime-artifact-build-receipt.v1" or receipt.get("schema") != record["schema"]:
        fail("root build receipt has an unsupported or mismatched schema")
    public = public_build_manifest(build, include_desktop=include_desktop)
    artifacts = receipt.get("artifacts")
    if not isinstance(artifacts, dict) or set(artifacts) != set(binaries):
        fail("root build receipt must attest exactly the selected four product artifacts" if include_desktop else "root build receipt must attest exactly the three product artifacts")
    for name in binaries:
        artifact = artifacts[name]
        if not isinstance(artifact, dict) or not isinstance(artifact.get("bytes"), int) or isinstance(artifact["bytes"], bool):
            fail(f"root build receipt has an invalid artifact byte count: {name}")
        if any(artifact.get(key) != public["executables"][name][key] for key in ("sha256", "bytes")):
            fail(f"root build receipt artifact differs from the build manifest: {name}")
    build_source = build["source"]
    receipt_source = receipt.get("source", {})
    if not isinstance(receipt.get("exit"), int) or isinstance(receipt["exit"], bool) or receipt["exit"] != 0:
        fail("root build receipt has no exact successful integer exit status")
    if receipt_source.get("clean_before") is not True or receipt_source.get("clean_after") is not True or receipt.get("toolchain", {}).get("unchanged") is not True or build_source.get("clean") is not True:
        fail("root build receipt does not attest a successful unchanged-source build")
    if any(receipt_source.get(key) != build_source.get(key) for key in ("commit", "tree")):
        fail("root build receipt commit/tree differs from build manifest")
    return receipt


def public_build_manifest(build: dict, *, include_desktop: bool = False) -> dict:
    """Retain build identity and hashes while omitting machine-local paths."""
    binaries = (*BINARIES, DESKTOP_ROLE["executable"]) if include_desktop else BINARIES
    if build.get("schema") != "nudox.runtime-build-manifest.v1":
        fail("build manifest has an unsupported runtime-artifact schema")
    source = build.get("source")
    if not isinstance(source, dict) or not all(key in source for key in ("commit", "tree", "clean")):
        fail("build manifest omits its source identity")
    executables = build.get("executables")
    if not isinstance(executables, dict) or set(executables) != set(binaries):
        fail("build manifest omits or adds executable identities")
    public_executables = {}
    for name in binaries:
        record = executables[name]
        if not isinstance(record, dict) or not re.fullmatch(r"[a-f0-9]{64}", record.get("sha256", "")):
            fail(f"build manifest has an invalid {name} hash")
        if not isinstance(record.get("bytes"), int) or isinstance(record["bytes"], bool) or record["bytes"] <= 0 or record["bytes"] > MAX_BINARY_BYTES:
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



def admit_typescript_sdk(directory: Path, receipt_path: Path, source: dict) -> tuple[dict, dict[str, Path], str, dict[str, int]]:
    """Admit a complete source-bound SDK payload before ELF relocation; no project setup runs."""
    directory = directory.resolve(strict=True)
    receipt_bytes = read_regular_bytes(receipt_path, 16 * 1024 * 1024, "TypeScript SDK receipt")
    receipt = parse_json_bytes(receipt_bytes, "TypeScript SDK receipt")
    if receipt.get("schema") != "nudox.typescript-sdk.v1" or receipt.get("target") != TARGET:
        fail("TypeScript SDK receipt has an unsupported schema or target")
    if receipt.get("source") != source:
        fail("TypeScript SDK receipt does not bind the exact application source")
    records = receipt.get("files")
    if not isinstance(records, dict) or not records or len(records) > 512:
        fail("TypeScript SDK receipt has no bounded complete file inventory")
    expected_directories: set[str] = set()
    for relative, digest in records.items():
        if not isinstance(relative, str) or "\\" in relative or not isinstance(digest, str) or not re.fullmatch(r"[a-f0-9]{64}", digest):
            fail("TypeScript SDK receipt has an invalid path or digest")
        path = PurePosixPath(relative)
        if path.is_absolute() or path.as_posix() != relative or any(part in {".", ".."} for part in path.parts):
            fail("TypeScript SDK receipt has an unsafe file path")
        expected_directories.update(parent.as_posix() for parent in path.parents if parent.parts)
    observed: dict[str, Path] = {}
    admitted_sizes: dict[str, int] = {}
    total = 0
    package_bytes = 0
    pending = [directory]
    while pending:
        for path in pending.pop().iterdir():
            relative = path.relative_to(directory).as_posix()
            if path.is_symlink() or not (path.is_dir() or path.is_file()):
                fail("TypeScript SDK payload contains a link or special file")
            if path.is_dir():
                if relative not in expected_directories:
                    fail("TypeScript SDK contains an unreceipted directory")
                pending.append(path)
                continue
            estimated_length = path.stat().st_size
            total += estimated_length
            if relative not in records or total > 512 * 1024 * 1024 or len(observed) >= 512:
                fail("TypeScript SDK payload exceeds or differs from its bounded receipt")
            maximum = 512 * 1024 * 1024 - (total - estimated_length)
            is_typescript = relative.startswith("node_modules/typescript/")
            if is_typescript:
                if estimated_length > MAX_TYPESCRIPT_PACKAGE_BYTES - package_bytes:
                    fail("TypeScript package exceeds its 96 MiB byte bound")
                maximum = min(maximum, MAX_TYPESCRIPT_PACKAGE_BYTES - package_bytes)
            admitted_length, digest = admit_file_digest(path, maximum)
            if is_typescript:
                package_bytes += admitted_length
            total += admitted_length - estimated_length
            if records[relative] != digest:
                fail(f"TypeScript SDK payload differs from its receipt: {relative}")
            observed[relative] = path
            admitted_sizes[relative] = admitted_length
    if set(observed) != set(records):
        fail("TypeScript SDK payload has missing or unreceipted files")
    required = {"node/bin/node", "node/LICENSE", "node_modules/typescript/package.json",
                "node_modules/typescript/bin/tsc", "node_modules/typescript/lib/typescript.js",
                "node_modules/typescript/LICENSE.txt", "node_modules/typescript/ThirdPartyNoticeText.txt"}
    if not required.issubset(observed):
        fail("TypeScript SDK payload omits its runtime, Compiler API, or license notices")
    package = load_json(observed["node_modules/typescript/package.json"], "TypeScript package")
    tools = receipt.get("tools")
    if not isinstance(tools, dict) or not isinstance(tools.get("node"), dict) or not isinstance(tools.get("typescript"), dict):
        fail("TypeScript SDK receipt omits Node and TypeScript identities")
    if package.get("name") != "typescript" or package.get("version") != tools["typescript"].get("version"):
        fail("TypeScript Compiler API package differs from its SDK identity")
    if tools["node"].get("sha256") != records["node/bin/node"] or not os.access(observed["node/bin/node"], os.X_OK):
        fail("TypeScript SDK Node is not the receipted executable")
    return receipt, observed, hashlib.sha256(receipt_bytes).hexdigest(), admitted_sizes

def admit_embedding_helper(path: Path, receipt_path: Path, revision: str, tree: str, lock_sha: str) -> tuple[dict, str]:
    """Optional source-bound CPU helper; absence never manufactures model availability."""
    raw = read_regular_bytes(receipt_path, 65536, "embedding build receipt")
    receipt = parse_json_bytes(raw, "embedding build receipt")
    if receipt.get("schema") != "nudox.embedding-helper-build-receipt.v1" or receipt.get("preset") != "candle-minilm-cpu-v1":
        fail("unsupported embedding helper build receipt")
    source = receipt.get("source", {})
    if (source.get("commit"), source.get("tree"), source.get("cargo_lock_sha256")) != (revision, tree, lock_sha):
        fail("embedding helper and product were not built from the same exact source")
    if type(receipt.get("exit")) is not int or receipt["exit"] != 0 or source.get("clean_before") is not True or source.get("clean_after") is not True or receipt.get("toolchain", {}).get("unchanged") is not True:
        fail("embedding helper receipt does not attest an unchanged successful build")
    if receipt.get("argv") != ["cargo", "build", "--release", "--locked", "--no-default-features", "--features", "cpu", "-p", "backend-embedding-candle"]:
        fail("embedding helper receipt does not attest the fixed CPU build recipe")
    artifact = receipt.get("artifact", {})
    size, digest = admit_file_digest(path, MAX_BINARY_BYTES)
    if type(artifact.get("bytes")) is not int or size != artifact["bytes"] or digest != artifact.get("sha256"):
        fail("embedding helper differs from its exact successful build receipt")
    return {"bytes": size, "sha256": digest}, hashlib.sha256(raw).hexdigest()


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--manifest", required=True, type=Path, help="successful source-bound Linux build manifest")
    parser.add_argument("--source", required=True, type=Path, help="exact clean build checkout")
    parser.add_argument("--output", required=True, type=Path, help="new candidate output directory")
    parser.add_argument("--patchelf", required=True, type=Path)
    parser.add_argument("--readelf", required=True, type=Path)
    parser.add_argument("--ldd", required=True, type=Path)
    parser.add_argument("--typescript-sdk-directory", type=Path, help="optional legacy Compiler API payload: complete Node and TypeScript SDK with notices; requires --typescript-sdk-receipt")
    parser.add_argument("--typescript-sdk-receipt", type=Path, help="exact source-bound optional SDK receipt; requires --typescript-sdk-directory")
    parser.add_argument("--embedding-helper", type=Path, help="optional receipted CPU Candle helper")
    parser.add_argument("--embedding-helper-receipt", type=Path, help="exact successful helper build receipt")
    parser.add_argument("--include-desktop", action="store_true", help="opt in to a hash-receipted fourth backend-desktop executable and schema-2 desktop bundle")
    parser.add_argument("--release-tag", help="immutable checkpoint tag; defaults to today's UTC date and source revision")
    args = parser.parse_args()
    if (args.typescript_sdk_directory is None) != (args.typescript_sdk_receipt is None):
        fail("TypeScript SDK directory and its receipt must be provided together")

    manifest_path = args.manifest.resolve(strict=True)
    build_bytes = read_regular_bytes(manifest_path, 16 * 1024 * 1024, "build manifest")
    build = parse_json_bytes(build_bytes, "build manifest")
    binaries = (*BINARIES, DESKTOP_ROLE["executable"]) if args.include_desktop else BINARIES
    receipt = admit_receipt(build, include_desktop=args.include_desktop)
    revision, tree, lock_sha, version = check_source(args.source, build, receipt)
    release_tag = args.release_tag or f"checkpoint-{dt.datetime.now(dt.timezone.utc):%Y%m%d}-{revision[:10]}-linux-x64"
    if not TAG_RE.fullmatch(release_tag) or not release_tag.endswith("-" + revision[:10] + "-linux-x64"):
        fail("release tag must be checkpoint-YYYYMMDD-<source-10>-linux-x64 for the receipted source")
    sdk_admission = None
    if args.typescript_sdk_directory is not None:
        sdk_admission = admit_typescript_sdk(args.typescript_sdk_directory, args.typescript_sdk_receipt, build["source"])
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
    admitted_digests: dict[str, str] = {}
    admitted_lengths: dict[str, int] = {}
    artifact_records = build.get("executables")
    if not isinstance(artifact_records, dict):
        fail("build manifest omits executable records")
    for name in binaries:
        record = artifact_records.get(name)
        if not isinstance(record, dict):
            fail(f"build manifest omits {name}")
        source_binary = Path(record.get("path", ""))
        expected_bytes = record.get("bytes")
        if not isinstance(expected_bytes, int) or isinstance(expected_bytes, bool) or expected_bytes <= 0 or expected_bytes > MAX_BINARY_BYTES or not source_binary.is_absolute() or source_binary.is_symlink():
            fail(f"{name} has an invalid admitted build length or source path")
        actual_bytes, actual_digest = admit_file_digest(source_binary, MAX_BINARY_BYTES)
        if actual_bytes != expected_bytes or actual_digest != record.get("sha256"):
            fail(f"{name} differs from the successful build receipt")
        sources[name] = source_binary.resolve(strict=True)
        admitted_digests[name] = record["sha256"]
        admitted_lengths[name] = record["bytes"]

    sdk_root = root / "share/nudox/typescript"
    if sdk_admission is not None:
        sdk_receipt, sdk_sources, sdk_receipt_digest, sdk_sizes = sdk_admission
        for relative, sdk_source in sdk_sources.items():
            target = sdk_root / relative
            target.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
            if relative == "node/bin/node":
                continue  # Copied once by the shared ELF relocation pass below.
            copy_admitted_file(sdk_source, target, sdk_receipt["files"][relative], sdk_sizes[relative], 512 * 1024 * 1024)
            target.chmod(0o755 if relative == "node/bin/node" else 0o644)
        sources["typescript-node"] = sdk_sources["node/bin/node"]
        admitted_digests["typescript-node"] = sdk_receipt["files"]["node/bin/node"]
        admitted_lengths["typescript-node"] = sdk_sizes["node/bin/node"]

    glibc_family = set(GLIBC_SONAMES)
    libraries: dict[str, dict] = {}
    embedding_receipt_digest = None
    if (args.embedding_helper is None) != (args.embedding_helper_receipt is None):
        fail("embedding helper and its build receipt must be provided together")
    if args.embedding_helper is not None:
        helper_record, embedding_receipt_digest = admit_embedding_helper(args.embedding_helper, args.embedding_helper_receipt, revision, tree, lock_sha)
        sources["backend-embedding-candle"] = args.embedding_helper.resolve(strict=True)
        admitted_digests["backend-embedding-candle"] = helper_record["sha256"]
        admitted_lengths["backend-embedding-candle"] = helper_record["bytes"]
        (root / "share/nudox/embedding/bin").mkdir(mode=0o700, parents=True)

    library_total_bytes = 0
    executables: list[dict] = []
    for name, source_binary in sources.items():
        packaged_relative = "share/nudox/typescript/node/bin/node" if name == "typescript-node" else "share/nudox/embedding/bin/backend-embedding-candle" if name == "backend-embedding-candle" else f"bin/{name}"
        packaged = root / packaged_relative
        runtime_search = "$ORIGIN/../../../../../lib" if name == "typescript-node" else "$ORIGIN/../../../../lib" if name == "backend-embedding-candle" else "$ORIGIN/../lib"
        copy_admitted_file(source_binary, packaged, admitted_digests[name], admitted_lengths[name], MAX_BINARY_BYTES)
        original = elf_info(readelf, packaged)
        if not original["interpreter"]:
            fail(f"{name} has no ELF interpreter")
        packaged.chmod(0o755)
        linkage = run([str(ldd), str(packaged)])
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
            if soname not in libraries and len(libraries) >= MAX_NODE_LIBRARIES:
                fail("package shared-library closure exceeds its 64 member bound")
            previous = libraries.get(soname)
            remaining = MAX_NODE_LIBRARY_BYTES - library_total_bytes + (previous["source_bytes"] if previous else 0)
            library_bytes, digest = admit_file_digest(dependency, min(96 * 1024 * 1024, remaining))
            previous = libraries.get(soname)
            if previous and previous["source_sha256"] != digest:
                fail(f"different dependencies use the same ELF soname {soname}")
            if previous is None:
                library_total_bytes += library_bytes
            libraries.setdefault(soname, {"source_path": dependency, "source_sha256": digest, "source_bytes": library_bytes})
        run([str(patchelf), "--set-interpreter", INTERPRETER, "--set-rpath", runtime_search, str(packaged)])
        patched = elf_info(readelf, packaged)
        if patched["interpreter"] != INTERPRETER or patched["needed"] != original["needed"]:
            fail(f"patchelf changed the wrong ELF contract for {name}")
        if run([str(patchelf), "--print-rpath", str(packaged)]) != runtime_search:
            fail(f"{name} retains an unexpected runtime search path")
        executables.append({"name": name, "original_sha256": admitted_digests[name], "packaged_path": packaged_relative, "packaged_sha256": sha256(packaged), "packaged_bytes": packaged.stat().st_size, "original_elf": {"needed": original["needed"], "interpreter_present": bool(original["interpreter"]), "required_glibc_versions": original["required_glibc_versions"]}, "packaged_elf": patched})

    for soname, record in libraries.items():
        packaged = root / "lib" / soname
        copy_admitted_file(record["source_path"], packaged, record["source_sha256"], record["source_bytes"], 96 * 1024 * 1024)
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

    sdk = {"bundled": False}
    if sdk_admission is not None:
        clean_environment = {key: value for key, value in os.environ.items()
                             if key not in {"LD_LIBRARY_PATH", "LD_PRELOAD", "LD_AUDIT", "NIX_LD", "NIX_LD_LIBRARY_PATH", "NODE_PATH", "NODE_OPTIONS"}}
        packaged_node = sdk_root / "node/bin/node"
        for arguments, expected, label in [
            (["--version"], sdk_receipt["tools"]["node"]["version"], "Node"),
            (["-e", "process.stdout.write(require(process.argv[1]).version)", str(sdk_root / "node_modules/typescript/lib/typescript.js")],
             sdk_receipt["tools"]["typescript"]["version"], "TypeScript Compiler API"),
        ]:
            result = subprocess.run([str(packaged_node), *arguments], env=clean_environment,
                                    stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, timeout=15)
            if result.returncode != 0 or result.stdout.strip() != expected:
                fail(f"relocated SDK {label} did not reproduce its admitted version identity")

        node_record = next(record for record in executables if record["name"] == "typescript-node")
        sdk_files = {
            path.relative_to(root).as_posix(): {"kind": "file", "sha256": sha256(path), "size_bytes": path.stat().st_size}
            for path in sorted(sdk_root.rglob("*")) if path.is_file()
        }
        sdk = {
            "schema": "nudox.typescript-sdk.v1", "bundled": True,
            "root": "share/nudox/typescript", "node": node_record,
            "typescript_version": sdk_receipt["tools"]["typescript"]["version"],
            "node_version": sdk_receipt["tools"]["node"]["version"],
            "source_receipt_sha256": sdk_receipt_digest,
            "files": sdk_files,
        }
    executables = [record for record in executables if record["name"] not in {"typescript-node", "backend-embedding-candle"}]

    embedding_setup = None
    if embedding_receipt_digest is not None:
        # These are the post-relocation bytes actually shipped, not original ELF claims.
        def setup_record(relative: str) -> dict:
            path = root / relative
            return {"path": relative, "sha256": sha256(path), "bytes": path.stat().st_size}
        helper_build_bytes = read_regular_bytes(args.embedding_helper_receipt, 65536, "embedding build receipt")
        if hashlib.sha256(helper_build_bytes).hexdigest() != embedding_receipt_digest:
            fail("embedding build receipt changed before package publication")
        lock_bytes = subprocess.check_output(["git", "-C", str(args.source), "show", f"{revision}:Cargo.lock"])
        if hashlib.sha256(lock_bytes).hexdigest() != lock_sha:
            fail("selected Cargo.lock changed before embedding receipt publication")
        (root / "share/nudox/embedding/build-receipt.json").write_bytes(helper_build_bytes)
        (root / "share/nudox/embedding/Cargo.lock").write_bytes(lock_bytes)
        setup = {"schema": "nudox.local-embedding-setup.v1", "preset": "candle-minilm-cpu-v1",
                 "source": {"commit": revision, "tree": tree, "cargo_lock_sha256": lock_sha},
                 "cli": setup_record("bin/backend-cli"),
                 "helper": setup_record("share/nudox/embedding/bin/backend-embedding-candle"),
                 "dependencies": [setup_record(record["packaged_path"]) for _, record in sorted(libraries.items())],
                 "build_receipt_sha256": embedding_receipt_digest,
                 "build_receipt": setup_record("share/nudox/embedding/build-receipt.json"),
                 "source_lock": setup_record("share/nudox/embedding/Cargo.lock")}
        # The already source-receipted CLI observes the typed closure identity; this is not
        # semantic attestation. Successful source/helper build custody above is its authority.
        command = [str(root / "bin/backend-cli"), "--json", "embedding-setup", "inspect", "--program", str(root / setup["helper"]["path"])]
        for dependency in setup["dependencies"]:
            command.extend(["--dependency", str(root / dependency["path"])])
        observed = subprocess.run(command, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=30,
                                  env={key: value for key, value in os.environ.items() if key not in {"LD_LIBRARY_PATH", "LD_PRELOAD", "LD_AUDIT", "NIX_LD", "NIX_LD_LIBRARY_PATH"}})
        if observed.returncode != 0 or len(observed.stdout) > 65536 or len(observed.stderr) > 65536:
            fail("packaged CLI could not observe its helper closure")
        observation = parse_json_bytes(observed.stdout, "typed packaged helper identity")
        identity = observation.get("identity")
        if observation.get("state") != "observed-helper" or not isinstance(identity, str) or not re.fullmatch(r"[a-f0-9]{64}", identity):
            fail("packaged helper identity observation is malformed")
        for record in [setup["cli"], setup["helper"], *setup["dependencies"]]:
            if setup_record(record["path"]) != record:
                fail("packaged helper closure changed while its identity was observed")
        setup["helper_identity"] = identity
        setup_path = root / "share/nudox/embedding/setup.json"
        setup_path.write_text(json.dumps(setup, indent=2, sort_keys=True) + "\n")
        embedding_setup = {"path": "share/nudox/embedding/setup.json", "sha256": sha256(setup_path), "size_bytes": setup_path.stat().st_size}

    public_build = public_build_manifest(build, include_desktop=args.include_desktop)
    (root / "build-manifest.json").write_text(json.dumps(public_build, indent=2, sort_keys=True) + "\n")
    package_tools = Path(__file__).resolve().parent
    installer_source = (package_tools / "install_linux.py").read_text()
    (output / "install-linux-x64-channel.py").write_text(installer_source)
    shutil.copyfile(package_tools / "native-qa-linux-x64.example.json", output / "native-qa-linux-x64.json")
    package = {
        "schema": "nudox.linux-portable-package.v1",
        "release_tag": release_tag,
        "source": build["source"],
        "original_build_manifest_sha256": hashlib.sha256(build_bytes).hexdigest(),
        "build_manifest_sha256": sha256(root / "build-manifest.json"),
        "system_sonames_sha256": hashlib.sha256(LINUX_SYSTEM_SONAMES_BYTES).hexdigest(),
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
        "typescript_sdk": sdk,
        "embedding_setup": embedding_setup,
        "glibc_libraries_bundled": False,
        "runtime_acceptance": "external_native_qa_record_required",
        "checkpoint_shell_asset": "install-linux-x64.sh",
    }
    if args.include_desktop:
        package.update(schema="nudox.linux-portable-package.v2", desktop=dict(DESKTOP_ROLE))
    (root / "packaging-manifest.json").write_text(json.dumps(package, indent=2, sort_keys=True) + "\n")
    sdk_note = ("This archive also includes a receipt-bound Node/TypeScript SDK for explicitly\n"
                "configured legacy Compiler API integration. Keep share/nudox/typescript\n"
                "and lib/ with the sibling executables.\n") if sdk_admission is not None else ""
    readme = f"""NuDox Linux x86_64 CLI/MCP/locald\n\nSource revision: {revision}\nRequires Linux x86_64 with glibc >= {minimum_glibc} and {INTERPRETER}.\nThe CLI, MCP and locald executables must remain siblings in bin/.\n\nVerify the supplied SHA-256 sidecar before extracting. For manual use:\n  tar -xzf nudox-linux-x86_64-{revision[:10]}.tar.gz\n  export PATH=\"$PWD/nudox-linux-x86_64/bin:$PATH\"\n  backend-cli --help\n  backend-mcp --help\n\nTypeScript analysis uses linked TSZ with embedded declarations by default; Node and\nan npm TypeScript installation are not required for this compiler path.\nPython analysis uses linked Pyrefly by default. Selected project interpreter,\nstandard-library and virtual-environment inputs are captured separately.\n{sdk_note}Default project discovery does not require hidden NUDOX_* environment variables.\n"""
    if args.include_desktop:
        readme += "\nOpen the app from a graphical Linux session (Wayland or X11):\n  backend-desktop\nKeep the matching CLI, MCP and locald executables beside backend-desktop in bin/.\n"
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
    if args.include_desktop:
        release.update(schema=2, desktop=dict(DESKTOP_ROLE))
    (output / "release-manifest.json").write_text(json.dumps(release, indent=2, sort_keys=True) + "\n")
    candidate_installer = installer_source.replace('PINNED_RELEASE_TAG = ""', f'PINNED_RELEASE_TAG = "{release_tag}"', 1)
    candidate_installer = candidate_installer.replace('PINNED_MANIFEST_SHA256 = ""', f'PINNED_MANIFEST_SHA256 = "{sha256(output / "release-manifest.json")}"', 1)
    if candidate_installer == installer_source or 'PINNED_MANIFEST_SHA256 = ""' in candidate_installer:
        fail("could not construct digest-pinned checkpoint installer")
    installer_bytes = candidate_installer.encode("utf-8")
    (output / "install-linux-x64.py").write_bytes(installer_bytes)
    shell_path = output / "install-linux-x64.sh"
    shell_path.write_bytes(checkpoint_shell(release_tag, installer_bytes, minimum_glibc))
    shell_digest = sha256(shell_path)
    (output / "install-linux-x64.sh.sha256").write_text(f"{shell_digest}  {shell_path.name}\n")
    print(json.dumps({"release_tag": release_tag, "archive": str(archive), "sha256": archive_hash, "size_bytes": archive.stat().st_size, "minimum_glibc": minimum_glibc, "bundled_libraries": sorted(libraries), "release_manifest": str(output / "release-manifest.json"), "installer": str(output / "install-linux-x64.py"), "shell_installer": {"path": str(shell_path), "sha256": shell_digest, "size_bytes": shell_path.stat().st_size}}, indent=2))
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (ValueError, OSError, KeyError, TypeError) as error:
        print(f"Linux package stopped: {error}", file=sys.stderr)
        raise SystemExit(1)
