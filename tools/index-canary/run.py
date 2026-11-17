#!/usr/bin/env python3
"""Run the reviewed Keel histories and publish only observed canary facts."""
import argparse
import fcntl
import hashlib
import json
import math
import os
from pathlib import Path
import re
import signal
import subprocess
import sys
import tempfile
import time


def atomic(path, data):
    path.parent.mkdir(parents=True, exist_ok=True)
    fd, name = tempfile.mkstemp(prefix=".canary-", dir=path.parent)
    try:
        with os.fdopen(fd, "w") as stream:
            stream.write(data)
            stream.flush()
            os.fsync(stream.fileno())
        os.chmod(name, 0o644)
        os.replace(name, path)
    finally:
        if os.path.exists(name):
            os.unlink(name)


def kill_delegated_children():
    """Kill and drain the dedicated service's delegated child cgroups."""
    root = Path(os.environ["KEEL_CGROUP_ROOT"])
    if not root.is_absolute() or not (root / "cgroup.controllers").exists():
        raise RuntimeError("invalid delegated cgroup root")
    children = [path for path in root.iterdir() if path.is_dir()]
    for child in children:
        (child / "cgroup.kill").write_text("1")
    deadline = time.monotonic() + 10
    while any("populated 1" in (child / "cgroup.events").read_text() for child in children if child.exists()):
        if time.monotonic() >= deadline:
            raise RuntimeError("delegated cgroups did not drain")
        time.sleep(0.05)
    # The killed runner cannot remove its owned scopes. Remove the now-empty
    # hierarchy bottom-up so repeated timeouts do not accumulate cgroups.
    for path in sorted(root.rglob("*"), key=lambda item: len(item.parts), reverse=True):
        if path.is_dir():
            try:
                path.rmdir()
            except FileNotFoundError:
                pass


def main():
    p = argparse.ArgumentParser()
    p.add_argument("--manifest", type=Path, required=True)
    p.add_argument("--bin-dir", type=Path, required=True)
    p.add_argument("--runner", type=Path, required=True)
    p.add_argument("--release-receipt", type=Path, required=True)
    p.add_argument("--state-dir", type=Path, required=True)
    p.add_argument("--work-dir", type=Path, help="Private disposable workspace parent")
    p.add_argument("--metrics", type=Path, required=True)
    p.add_argument("--timeout", type=int, default=1800)
    a = p.parse_args()
    if a.timeout < 1:
        p.error("timeout must be positive")
    a.state_dir.mkdir(parents=True, exist_ok=True, mode=0o700)
    if a.work_dir:
        a.work_dir.mkdir(parents=True, exist_ok=True, mode=0o700)
    with (a.state_dir / "lock").open("w") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        return run(a)


def run(a):
    # The caller supplies a systemd-delegated cgroup. Keel creates one child
    # per process and refuses to run without containment on Linux.
    os.environ["KEEL_REQUIRE_CGROUP"] = "1"
    start = time.monotonic()
    success = False
    durations = []
    error = None
    report = None
    with tempfile.TemporaryDirectory(prefix="run-", dir=a.work_dir or a.state_dir) as temp:
        evidence = Path(temp) / "evidence"
        try:
            manifest = json.loads(a.manifest.read_text())
            expected = {case["id"]: case for case in manifest["cases"]}
            if len(expected) != len(manifest["cases"]) or not expected:
                raise ValueError("empty or duplicate canary cases")
            proc = subprocess.Popen([str(a.runner), "--manifest", str(a.manifest),
                "--bin-dir", str(a.bin_dir), "--evidence", str(evidence)],
                start_new_session=True)
            try:
                code = proc.wait(timeout=a.timeout)
            except subprocess.TimeoutExpired:
                try:
                    os.killpg(proc.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                proc.wait()
                kill_delegated_children()
                raise RuntimeError("Keel run exceeded deadline")
            report = json.loads((evidence / "report.json").read_text())
            if report.get("manifest_sha256") != hashlib.sha256(a.manifest.read_bytes()).hexdigest():
                raise ValueError("manifest receipt mismatch")
            release = json.loads(a.release_receipt.read_text())
            if not re.fullmatch(r"[0-9a-f]{40}", release.get("source_revision", "")):
                raise ValueError("invalid release source revision")
            for key in ("manifest_sha256", "driver_sha256", "binaries"):
                if not release.get(key) or release[key] != report.get(key):
                    raise ValueError("release receipt mismatch: " + key)
            seen = {case["id"]: case for case in report["cases"]}
            if set(seen) != set(expected) or len(seen) != len(report["cases"]):
                raise ValueError("incomplete case receipt")
            invalid_cases = []
            for ident, case in expected.items():
                observed = seen[ident]
                steps = {step["id"]: step for step in observed["steps"]}
                if len(steps) != len(observed["steps"]):
                    raise ValueError("duplicate step receipt")
                for step in case["steps"]:
                    result = steps.get(step["id"], {})
                    if step["kind"] == "index" and result.get("status") in ("pass", "fail"):
                        duration = result.get("duration_ms")
                        if not isinstance(duration, (int, float)) or isinstance(duration, bool) or not math.isfinite(duration) or duration < 0:
                            raise ValueError("invalid duration")
                        durations.append(duration / 1000)
                if observed["status"] != "pass" or set(steps) != {s["id"] for s in case["steps"]} or any(s.get("status") != "pass" for s in steps.values()):
                    invalid_cases.append(ident)
            if invalid_cases:
                raise ValueError("case failed or incomplete: " + ", ".join(invalid_cases))
            success = code == 0 and report.get("status") == "pass" and bool(durations)
            if not success:
                raise ValueError("Keel did not report complete success")
            # Retain the small source/artifact receipt, never the disposable project.
            receipt = {key: report.get(key) for key in ("manifest_sha256", "driver_sha256", "binaries", "authority_environment_sha256", "containment_policy")}
            receipt["source_revision"] = release["source_revision"]
            receipt["success_unix_seconds"] = time.time()
            atomic(a.state_dir / "last-success.json", json.dumps(receipt, sort_keys=True) + "\n")
        except (OSError, ValueError, KeyError, TypeError, RuntimeError) as exc:
            success = False
            error = str(exc)
    # Keep only fixed receipt fields, not captured output or environment.
    # In particular credentials are never copied into retained evidence.
    safe_cases = []
    if isinstance(report, dict):
        for case in report.get("cases", []):
            safe_steps = []
            for step in case.get("steps", []):
                row = {key: step[key] for key in ("id", "status", "duration_ms") if key in step}
                if isinstance(step.get("error"), str):
                    # Keel redacts the private account key before errors leave
                    # its process boundary. Retain a bounded actionable reason.
                    row["error"] = step["error"][:2048]
                detail = step.get("detail")
                if isinstance(detail, dict):
                    row["publication"] = {key: detail[key] for key in ("view_root", "profiles") if key in detail}
                safe_steps.append(row)
            safe_case = {"id": case.get("id"), "status": case.get("status"), "steps": safe_steps}
            if isinstance(case.get("error"), str):
                safe_case["error"] = case["error"][:2048]
            safe_cases.append(safe_case)
    atomic(a.state_dir / "last-run.json", json.dumps({
        "completed_unix_seconds": time.time(), "success": success,
        "failure": error[:2048] if error else None, "cases": safe_cases,
    }, sort_keys=True) + "\n")
    last = None
    try:
        last = json.loads((a.state_dir / "last-success.json").read_text())["success_unix_seconds"]
        if not isinstance(last, (int, float)) or not math.isfinite(last) or last <= 0:
            last = None
    except (OSError, ValueError, KeyError, TypeError):
        pass
    lines = ["# HELP canary_run_success Whether the latest completed run passed all canary checks.",
             "# TYPE canary_run_success gauge", f"canary_run_success {int(success)}",
             "# HELP canary_run_duration_seconds Monotonic duration of the latest completed run.",
             "# TYPE canary_run_duration_seconds gauge", f"canary_run_duration_seconds {time.monotonic() - start:.3f}"]
    if durations:
        lines += ["# HELP canary_step_duration_seconds Maximum observed index step duration in the latest run.",
                  "# TYPE canary_step_duration_seconds gauge", f'canary_step_duration_seconds{{step="index"}} {max(durations):.3f}']
    if last is not None:
        lines += ["# HELP canary_last_success_timestamp_seconds Time of the last complete successful run.",
                  "# TYPE canary_last_success_timestamp_seconds gauge", f"canary_last_success_timestamp_seconds {last:.3f}"]
    atomic(a.metrics, "\n".join(lines) + "\n")
    if error:
        print("index canary failed:", error, file=sys.stderr)
    return 0 if success else 1


if __name__ == "__main__":
    sys.exit(main())
