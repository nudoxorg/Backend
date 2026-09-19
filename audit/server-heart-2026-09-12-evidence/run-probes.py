#!/usr/bin/env python3
"""Run audit counterexamples in a disposable source copy, preserving the checkout."""

import argparse
from pathlib import Path
import re
import shutil
import subprocess
import tempfile


def main():
    evidence = Path(__file__).resolve().parent
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", type=Path, default=evidence.parents[1])
    parser.add_argument("--output", type=Path, help="New directory for the isolated copy")
    args = parser.parse_args()
    repo = args.repo.resolve()
    if args.output is None:
        destination = Path(tempfile.mkdtemp(prefix="server-heart-audit-"))
    else:
        destination = args.output.resolve()
        destination.mkdir(parents=True, exist_ok=False)
    print(f"Isolated workspace: {destination}", flush=True)
    ignore = shutil.ignore_patterns("turso", "target", "node_modules", "GUI2", "gui", ".git")
    for directory in ("heart", "server", "compiler", "interface"):
        shutil.copytree(repo / directory, destination / directory, ignore=ignore)
    manifest = (repo / "Cargo.toml").read_text()
    manifest, count = re.subn(
        r"(?ms)^members\s*=\s*\[.*?^\]\n",
        'members = ["audit-probes"]\n',
        manifest,
        count=1,
    )
    if count != 1:
        raise RuntimeError("Expected the audited multiline workspace members declaration")
    manifest = re.sub(r"(?m)^exclude\s*=.*\n", "", manifest, count=1)
    (destination / "Cargo.toml").write_text(manifest)
    shutil.copyfile(evidence / "probe-Cargo.lock", destination / "Cargo.lock")
    probes = destination / "audit-probes"
    (probes / "src").mkdir(parents=True)
    shutil.copyfile(evidence / "probe-Cargo.toml", probes / "Cargo.toml")
    shutil.copyfile(evidence / "probes.rs", probes / "src/main.rs")
    journal_tests = destination / "server/journal/publication/owner/tests.rs"
    journal_tests.write_text(
        journal_tests.read_text() + "\n" + (evidence / "journal-probes.rs").read_text()
    )
    commands = [
        ("behavior-and-routing.log", ["cargo", "test", "--offline", "-p", "audit-probes", "-p", "server-index-routing"]),
        ("journal-crash-states.log", ["cargo", "test", "--offline", "-p", "server-journal", "audit_", "--", "--nocapture"]),
    ]
    failed = False
    for name, command in commands:
        log = destination / name
        print("Running " + " ".join(command), flush=True)
        with log.open("w") as output:
            result = subprocess.run(command, cwd=destination, stdout=output, stderr=subprocess.STDOUT, check=False)
        print(f"Exit {result.returncode}; log: {log}", flush=True)
        print("\n".join(log.read_text().splitlines()[-25:]), flush=True)
        failed |= result.returncode != 0
    return int(failed)


if __name__ == "__main__":
    raise SystemExit(main())
