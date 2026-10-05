#!/usr/bin/env python3
# Gives unchanged sources back the mtimes they had when the cached build
# output was made, so Cargo reuses it. Usage: restore-mtimes.py MANIFEST
#
# Cargo rebuilds a workspace crate when any of its source files is newer
# than its last build. A fresh clone stamps every file "now", which makes a
# persistent build directory worthless. A file whose git blob matches the
# manifest gets its recorded mtime back; a changed or new file keeps "now"
# and is rebuilt. The manifest is then rewritten from what is on disk, so a
# build that failed part-way is still judged correctly on the next run.
import json
import os
import subprocess
import sys

manifest_path = sys.argv[1]
try:
    with open(manifest_path) as manifest_file:
        manifest = json.load(manifest_file)
except (FileNotFoundError, ValueError):
    manifest = {}

listing = subprocess.run(
    ["git", "ls-files", "--stage", "-z"], check=True, capture_output=True
).stdout
current = {}
restored = 0
for entry in listing.split(b"\0"):
    if not entry:
        continue
    meta, raw_path = entry.split(b"\t", 1)
    mode, blob, _stage = meta.split()
    path = os.fsdecode(raw_path)
    # Regular files only: symlinks and submodules carry no build mtime.
    if mode not in (b"100644", b"100755") or not os.path.isfile(path):
        continue
    blob = blob.decode()
    recorded = manifest.get(path)
    if recorded is not None and recorded["blob"] == blob:
        os.utime(path, ns=(recorded["mtime"], recorded["mtime"]))
        mtime = recorded["mtime"]
        restored += 1
    else:
        mtime = os.stat(path).st_mtime_ns
    current[path] = {"blob": blob, "mtime": mtime}

temporary = manifest_path + ".tmp"
with open(temporary, "w") as manifest_file:
    json.dump(current, manifest_file)
os.replace(temporary, manifest_path)
print(f"source mtimes: {restored} of {len(current)} files restored from the cache manifest")
