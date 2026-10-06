#!/usr/bin/env python3
"""Bind a private ordinary npm install to official immutable archives.

The acquired package stays unchanged. A separate consumer manifest admits its
runtime dependencies and explicitly named tooling/types. No lifecycle scripts
or product code run here; this receipt grants no runtime acceptance.
"""
import argparse
import concurrent.futures
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path, PurePosixPath
import shutil
import subprocess
import tarfile
import time
import urllib.parse
import urllib.request

p = argparse.ArgumentParser()
p.add_argument("--dossier", required=True, type=Path)
p.add_argument("--output", required=True, type=Path)
p.add_argument("--node-bin", required=True, type=Path)
p.add_argument("--origin-verifier", required=True, type=Path)
p.add_argument("--extra", action="append", default=[], help="Separate consumer name=range")
a = p.parse_args()
a.output.mkdir(mode=0o700, parents=True, exist_ok=False)
spec = importlib.util.spec_from_file_location("registry_origin", a.origin_verifier)
origin = importlib.util.module_from_spec(spec)
spec.loader.exec_module(origin)


def sha(raw):
    return hashlib.sha256(raw).hexdigest()


def save(path, value):
    assert not path.exists()
    path.write_text(json.dumps(value, sort_keys=True, indent=2) + "\n")


def resource():
    mem = int(next(x.split()[1] for x in Path("/proc/meminfo").read_text().splitlines() if x.startswith("MemAvailable:")))
    disk = shutil.disk_usage(a.output).free
    assert mem >= 24 * 1024**2 and disk >= 24 * 1024**3
    return {"memory_available_kib": mem, "disk_free_bytes": disk}


def fetch(url, path, cap):
    started = time.monotonic()
    with urllib.request.urlopen(urllib.request.Request(url, headers={"User-Agent": "nudox-authorized-source-closure/1"}), timeout=45) as r:
        raw = r.read(cap + 1)
        assert len(raw) <= cap and r.status == 200
        fact = {"url": url, "final_url": r.url, "bytes": len(raw), "sha256": sha(raw), "seconds": time.monotonic() - started}
    path.write_bytes(raw)
    return raw, fact


sample = resource()
src = a.dossier / "source"
inv_path, origin_path = a.dossier / "source-inventory.json", a.dossier / "registry-origin.json"
inv = json.loads(inv_path.read_bytes())
root_pkg = json.loads((src / "package.json").read_bytes())
assert inv["package"]["id"] == root_pkg["name"] and inv["package"]["version"] == root_pkg["version"]
package_json_before = sha((src / "package.json").read_bytes())
extras = dict(value.split("=", 1) for value in a.extra)
assert all(k and v and k not in root_pkg.get("dependencies", {}) for k, v in extras.items())
consumer = a.output / "consumer"
consumer.mkdir(mode=0o700)
deps = dict(root_pkg.get("dependencies", {}))
deps.update(extras)
save(consumer / "package.json", {"name": "nudox-private-source-closure", "version": "0.0.0", "private": True, "dependencies": deps})
home, cache = a.output / "home", a.output / "npm-cache"
home.mkdir(mode=0o700)
cache.mkdir(mode=0o700)
env = {"PATH": str(a.node_bin), "HOME": str(home), "npm_config_cache": str(cache), "npm_config_registry": "https://registry.npmjs.org/", "LANG": "C.UTF-8"}


def command(label, words):
    started = time.monotonic()
    with (a.output / (label + ".stdout")).open("xb") as out, (a.output / (label + ".stderr")).open("xb") as err:
        result = subprocess.run(words, cwd=consumer, env=env, stdout=out, stderr=err, stdin=subprocess.DEVNULL, timeout=300)
    fact = {"argv": words, "cwd": str(consumer), "env": env, "exit": result.returncode, "seconds": time.monotonic() - started}
    save(a.output / (label + ".json"), fact)
    assert result.returncode == 0, fact
    return fact


npm = str(a.node_bin / "npm")
node = str(a.node_bin / "node")
tools = {name: {"path": str(a.node_bin / name), "sha256": sha((a.node_bin / name).read_bytes()), "version": subprocess.check_output([str(a.node_bin / name), "--version"], env=env, text=True).strip()} for name in ["node", "npm"]}
lock_cmd = command("npm-lock", [npm, "install", "--package-lock-only", "--ignore-scripts", "--no-audit", "--no-fund", "--lockfile-version=3"])
lock_path = consumer / "package-lock.json"
lock_sha = sha(lock_path.read_bytes())
lock = json.loads(lock_path.read_bytes())
entries = []
for rel, info in sorted(lock["packages"].items()):
    if not rel:
        continue
    assert info.get("resolved", "").startswith("https://registry.npmjs.org/") and info.get("integrity") and not info.get("dev")
    name = info.get("name") or rel.rsplit("node_modules/", 1)[1]
    entries.append({"path": rel, "name": name, **info})
unique = {(r["name"], r["version"]): r for r in entries}
artifacts = a.output / "dependencies"
artifacts.mkdir(mode=0o700)


def acquire(item):
    (name, version), info = item
    directory = artifacts / sha((name + "@" + version).encode())[:24]
    directory.mkdir(mode=0o700)
    meta_url = "https://registry.npmjs.org/" + urllib.parse.quote(name, safe="") + "/" + urllib.parse.quote(version, safe="")
    meta_raw, meta_request = fetch(meta_url, directory / "registry-metadata.json", 8 * 1024**2)
    meta = json.loads(meta_raw)
    assert meta["name"] == name and meta["version"] == version
    assert meta["dist"]["tarball"] == info["resolved"] and meta["dist"]["integrity"] == info["integrity"]
    raw, archive_request = fetch(info["resolved"], directory / "archive.tgz", 64 * 1024**2)
    with tarfile.open(fileobj=io.BytesIO(raw)) as archive:
        roots = {PurePosixPath(member.name).parts[0] for member in archive if PurePosixPath(member.name).parts}
    assert len(roots) == 1
    prefix = next(iter(roots)) + "/"
    files, special = origin.archive_members(raw, prefix, 20000, 32 * 1024**2, 128 * 1024**2)
    identity = {"ecosystem": "npm", "id": name, "version": version}
    origin.verify_registry_metadata(identity, meta, meta_url, raw, info["resolved"], special)
    save(directory / "source-inventory.json", {"schema": "nudox.acquired-package-source-inventory.v1", "package": identity, "files": files})
    return {"package": identity, "directory": str(directory), "strip_prefix": prefix, "archive_sha256": sha(raw), "metadata_sha256": sha(meta_raw), "inventory_sha256": sha((directory / "source-inventory.json").read_bytes()), "files": files, "requests": [meta_request, archive_request]}


with concurrent.futures.ThreadPoolExecutor(max_workers=4) as pool:
    records = list(pool.map(acquire, sorted(unique.items())))
save(a.output / "acquisition.json", {"packages": records, "lock_sha256": lock_sha, "whole_package_pass": False})
resource()
install = command("npm-ci", [npm, "ci", "--ignore-scripts", "--no-audit", "--no-fund"])
assert sha(lock_path.read_bytes()) == lock_sha
by_key = {(r["package"]["id"], r["package"]["version"]): r for r in records}
installed = []
for entry in entries:
    directory = consumer / entry["path"]
    assert directory.is_dir(), entry
    record = by_key[(entry["name"], entry["version"])]
    for member in record["files"]:
        file = directory / member["path"]
        assert file.is_file() and file.stat().st_size == member["bytes"] and sha(file.read_bytes()) == member["sha256"], str(file)
    installed.append({"path": entry["path"], "package": record["package"], "verified_files": len(record["files"]), "archive_sha256": record["archive_sha256"]})
project = a.output / "project"
shutil.copytree(src, project)
shutil.copytree(consumer / "node_modules", project / "node_modules", symlinks=True)
assert sha((project / "package.json").read_bytes()) == package_json_before
for member in inv["files"]:
    file = project / member["path"]
    assert file.is_file() and sha(file.read_bytes()) == member["sha256"]
packet = {"schema": "nudox.official-npm-private-runtime-closure.v1", "root_package": inv["package"], "project": str(project), "root_inventory": str(inv_path), "root_inventory_sha256": sha(inv_path.read_bytes()), "root_origin": str(origin_path), "root_origin_sha256": sha(origin_path.read_bytes()), "source_package_json_sha256": package_json_before, "source_package_json_unchanged": True, "consumer_manifest": str(consumer / "package.json"), "consumer_manifest_sha256": sha((consumer / "package.json").read_bytes()), "extra_consumer_dependencies": extras, "lock_sha256": lock_sha, "tools": tools, "env": env, "lock_command": lock_cmd, "install_command": install, "installed": installed, "unique_releases": len(records), "resource_before": sample, "lifecycle_scripts_executed": False, "product_runtime_started": False, "whole_package_pass": False}
save(a.output / "runtime-source-packet.json", packet)
print(json.dumps({"packet": str(a.output / "runtime-source-packet.json"), "sha256": sha((a.output / "runtime-source-packet.json").read_bytes()), "project": str(project), "installed_paths": len(installed), "unique_releases": len(records)}))
