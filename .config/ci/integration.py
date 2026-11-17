#!/usr/bin/env python3
"""Run the complete artifact VM gate and retain the separate corpus evidence."""
from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
import re
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[2]
CI = ROOT / ".config" / "ci"


def invoke(command: list[str]) -> tuple[int, str | None]:
    try:
        return subprocess.run(command, cwd=ROOT, check=False).returncode, None
    except OSError as error:
        return 125, str(error)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bin-dir", type=Path, required=True)
    parser.add_argument("--source-sha")
    parser.add_argument("--report", type=Path)
    parser.add_argument("--ci-report", type=Path)
    parser.add_argument("--jobs", type=int, choices=range(1, 5))
    parser.add_argument("--print-missing", action="store_true")
    args = parser.parse_args()
    if args.ci_report and not args.source_sha:
        parser.error("--ci-report requires --source-sha")
    source_sha = args.source_sha
    if source_sha is None:
        source_sha = subprocess.check_output(
            ["git", "-C", str(ROOT), "rev-parse", "HEAD"], text=True
        ).strip()
    if not re.fullmatch(r"[0-9a-f]{40}", source_sha):
        parser.error("--source-sha must be a complete lowercase Git SHA")

    bin_dir = args.bin_dir.resolve()
    bundle = bin_dir.parent
    report = args.report or args.ci_report
    results = report.resolve().parent if report else ROOT / ".local" / "ci" / source_sha
    results.mkdir(parents=True, exist_ok=True)
    name = report.stem if report else source_sha
    guest_output = results / (name + ".keel")
    summary_path = results / (name + ".keel-summary.json")
    vm_command = [
        sys.executable, str(CI / "keel-vm.py"),
        "--source", str(ROOT), "--source-sha", source_sha,
        "--bundle", str(bundle), "--verifier", str(CI / "t1_artifacts.py"),
        "--output", str(guest_output),
    ]
    vm_code, vm_error = invoke(vm_command)

    # Retain the legacy packet exactly as the T2 consumer expects it. A red VM
    # still permits these independent diagnostics, and never becomes green.
    surface_command = [sys.executable, str(CI / "surface.py"), "--bin-dir", str(bin_dir)]
    if args.jobs is not None:
        surface_command += ["--jobs", str(args.jobs)]
    if args.report:
        surface_command += ["--report", str(args.report.resolve())]
    if args.ci_report:
        surface_command += [
            "--ci-report", str(args.ci_report.resolve()), "--source-sha", source_sha,
        ]
    if args.print_missing:
        surface_command.append("--print-missing")
    surface_code, surface_error = invoke(surface_command)

    artifact = bundle / "manifest.json"
    manifest_hash = hashlib.sha256(artifact.read_bytes()).hexdigest() if artifact.is_file() else None
    summary = {
        "schema": 1,
        "source_sha": source_sha,
        "artifact_manifest_sha256": manifest_hash,
        "status": "passed" if vm_code == surface_code == 0 else "failed",
        "keel": {"exit": vm_code, "error": vm_error, "output": str(guest_output)},
        "legacy": {"exit": surface_code, "error": surface_error},
    }
    summary_path.write_text(json.dumps(summary, sort_keys=True, indent=2) + "\n")
    # This separate envelope cannot overwrite the legacy CI-EVIDENCE cases map.
    print("KEEL-CI-EVIDENCE " + json.dumps(summary, sort_keys=True), flush=True)
    return vm_code if vm_code else surface_code


if __name__ == "__main__":
    raise SystemExit(main())
