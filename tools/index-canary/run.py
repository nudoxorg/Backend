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


def object_value(value):
    if not isinstance(value, dict):
        raise ValueError("invalid object")
    return value


def list_value(value):
    if not isinstance(value, list) or len(value) > 1024:
        raise ValueError("invalid list")
    return value


def identifier(value):
    if not isinstance(value, str) or not re.fullmatch(r"[A-Za-z0-9_.-]{1,128}", value):
        raise ValueError("invalid identifier")
    return value


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
    safe_cases = []
    with tempfile.TemporaryDirectory(prefix="run-", dir=a.work_dir or a.state_dir) as temp:
        evidence = Path(temp) / "evidence"
        try:
            manifest = object_value(json.loads(a.manifest.read_text()))
            manifest_cases = list_value(manifest["cases"])
            expected = {identifier(object_value(case)["id"]): case for case in manifest_cases}
            if len(expected) != len(manifest_cases) or not expected:
                raise ValueError("empty or duplicate canary cases")
            for case in manifest_cases:
                for step in list_value(case["steps"]):
                    identifier(object_value(step)["id"])
                    if not isinstance(step.get("kind"), str):
                        raise ValueError("invalid step kind")
            proc = subprocess.Popen([str(a.runner), "--manifest", str(a.manifest),
                "--bin-dir", str(a.bin_dir), "--evidence", str(evidence)],
                start_new_session=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
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
            report = object_value(json.loads((evidence / "report.json").read_text()))
            if report.get("manifest_sha256") != hashlib.sha256(a.manifest.read_bytes()).hexdigest():
                raise ValueError("manifest receipt mismatch")
            release = object_value(json.loads(a.release_receipt.read_text()))
            if not isinstance(release.get("source_revision"), str) or not re.fullmatch(r"[0-9a-f]{40}", release["source_revision"]):
                raise ValueError("invalid release source revision")
            if (a.bin_dir.parent / "share/nudox/source-revision.txt").read_text().strip() != release["source_revision"]:
                raise ValueError("runtime revision mismatch")
            for key in ("manifest_sha256", "driver_sha256", "binaries"):
                if not release.get(key) or release[key] != report.get(key):
                    raise ValueError("release receipt mismatch: " + key)
            containment = object_value(report["containment_policy"])
            if containment.get("required") is not True or containment.get("root") != {"Unix": list(os.fsencode(os.environ["KEEL_CGROUP_ROOT"]))}:
                raise ValueError("containment mismatch")
            authority = report.get("authority_environment_sha256")
            if not isinstance(authority, str) or not re.fullmatch(r"[0-9a-f]{64}", authority):
                raise ValueError("invalid authority receipt")
            report_cases = list_value(report["cases"])
            seen = {identifier(object_value(case)["id"]): case for case in report_cases}
            if set(seen) != set(expected) or len(seen) != len(report_cases):
                raise ValueError("incomplete case receipt")
            invalid_cases = []
            for ident, case in expected.items():
                observed = seen[ident]
                observed_steps = list_value(observed["steps"])
                steps = {identifier(object_value(step)["id"]): step for step in observed_steps}
                if len(steps) != len(observed_steps):
                    raise ValueError("duplicate step receipt")
                if not isinstance(observed.get("status"), str) or any(not isinstance(s.get("status"), str) for s in steps.values()):
                    raise ValueError("invalid status")
                for step in case["steps"]:
                    result = steps.get(step["id"], {})
                    if step["kind"] == "index" and result.get("status") in ("pass", "fail"):
                        duration = result.get("duration_ms")
                        if not isinstance(duration, (int, float)) or isinstance(duration, bool) or not math.isfinite(duration) or duration < 0:
                            raise ValueError("invalid duration")
                        durations.append(duration / 1000)
                if observed["status"] != "pass" or set(steps) != {s["id"] for s in case["steps"]} or any(s.get("status") != "pass" for s in steps.values()):
                    invalid_cases.append(ident)
                safe_cases.append({"id": ident, "status": observed["status"] if observed["status"] in ("pass", "fail") else "other", "steps": [{"id": key, "status": value["status"] if value["status"] in ("pass", "fail") else "other"} for key, value in steps.items()]})
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
        except (OSError, ValueError, KeyError, TypeError, RuntimeError, OverflowError) as exc:
            success = False
            error = "io-failure" if isinstance(exc, OSError) else "runtime-failure" if isinstance(exc, RuntimeError) else "invalid-receipt"
    # Keep only fixed receipt fields, not captured output or environment.
    # In particular credentials are never copied into retained evidence.
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
