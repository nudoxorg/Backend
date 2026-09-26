#!/usr/bin/env python3
"""Sample one native gallery process; graph-memory routes mark virtual phases.

This runner never restarts the process between phases. macOS libproc supplies
RSS, physical footprint, and its lifetime peak directly in bytes. It writes
bounded scalar samples rather than retaining screenshots or per-frame ledgers.
Use the gallery `soak` command (streaming) when available; motion-report itself
retains every frame's ledger, so growth there includes the report accumulator.
"""

import argparse
import ctypes
import hashlib
import json
import os
from pathlib import Path
import selectors
import signal
import subprocess
import sys
import time


RUSAGE_FIELDS = """user_time system_time pkg_idle_wkups interrupt_wkups pageins
wired_size resident_size phys_footprint proc_start_abstime proc_exit_abstime
child_user_time child_system_time child_pkg_idle_wkups child_interrupt_wkups
child_pageins child_elapsed_abstime diskio_bytesread diskio_byteswritten
cpu_time_qos_default cpu_time_qos_maintenance cpu_time_qos_background
cpu_time_qos_utility cpu_time_qos_legacy cpu_time_qos_user_initiated
cpu_time_qos_user_interactive billed_system_time serviced_system_time
logical_writes lifetime_max_phys_footprint instructions cycles billed_energy
serviced_energy interval_max_phys_footprint runnable_time""".split()


class RusageInfoV4(ctypes.Structure):
    _fields_ = [("uuid", ctypes.c_uint8 * 16)] + [
        (name, ctypes.c_uint64) for name in RUSAGE_FIELDS
    ]


class MacMemory:
    def __init__(self):
        if sys.platform != "darwin":
            raise RuntimeError("macOS libproc is required")
        self.lib = ctypes.CDLL("/usr/lib/libproc.dylib", use_errno=True)
        self.lib.proc_pid_rusage.argtypes = [ctypes.c_int, ctypes.c_int, ctypes.c_void_p]
        self.lib.proc_pid_rusage.restype = ctypes.c_int

    def sample(self, pid):
        usage = RusageInfoV4()
        if self.lib.proc_pid_rusage(pid, 4, ctypes.byref(usage)) != 0:
            return {"error": os.strerror(ctypes.get_errno())}
        return {
            "rss_bytes": usage.resident_size,
            "footprint_bytes": usage.phys_footprint,
            "peak_footprint_bytes": usage.lifetime_max_phys_footprint,
        }


def build_script(path, counts, cycles, actions_per_frame, frame_ms):
    """Write deterministic repeated real-input cycles, with warm/settled phases."""
    # Search, focus, return, pointer jitter, reach/tour, and zoom are real GPUI
    # events. Same workload repeats so font and query caches can become warm.
    pattern = [
        "key /", 'type "glyph::RelationLabel"', "key enter", "key r",
        "key r", "key t", "key right", "key escape", "key escape",
        "route graph-hover 0", "leave", "wheel-zoom 720,412 1.02",
        "wheel-zoom 720,412 0.98039216", "leave",
    ]
    queries = ["glyph::RelationLabel", "RelationGroup", "render", "from_str",
               "Visitor", "Value", "parse", "Invocation", "Grammar",
               "Invocation -> list of text", "new", "RelationLabel"]
    # At least one draw between search and Enter is needed for asynchronous
    # search delivery. A memory-soak's keyboard pattern therefore runs in
    # action groups separated by virtual frames rather than one giant burst.
    warm_at = ((4704 + frame_ms - 1) // frame_ms) * frame_ms
    lines = ["route graph-memory cold @0", "key / @64",
             'type "glyph::RelationLabel" @96', "key enter @128",
             "route graph-hover 0 @1920", "leave @1984",
             "key escape @2048", "key escape @2080", "key escape @2112",
             "key 0 @2144", f"route graph-memory warm @{warm_at}"]
    # The first workload event follows the marker's actual draw, even when
    # using a coarser frame period for a long memory-only experiment.
    at, total, phases = warm_at + frame_ms, 0, [{"label": "cold", "actions": 0},
                               {"label": "warm", "actions": 0}]
    for cycle in range(cycles):
        for count in counts:
            for action in range(count):
                act = pattern[action % len(pattern)]
                if action % len(pattern) == 1:
                    query = queries[(total // len(pattern)) % len(queries)]
                    act = f"type {json.dumps(query)}"
                lines.append(f"{act} @{at}")
                total += 1
                if (action + 1) % actions_per_frame == 0:
                    at += frame_ms
                if action % len(pattern) in (2, 4, 5, 8):
                    at += 1800
                elif action % len(pattern) in (3, 6, 9):
                    at += 600
            at += 800
            # Restore the same free map state before comparing memory.
            for act in ["key escape", "key escape", "key escape", "key 0", "leave"]:
                lines.append(f"{act} @{at}")
                at += frame_ms
            at += 2600
            label = f"cycle{cycle + 1}-after{count}-total{total}"
            lines.append(f"route graph-memory {label} @{at}")
            phases.append({"label": label, "actions": total})
            at += frame_ms
    at += 2600
    lines.append(f"route graph-memory final @{at}")
    phases.append({"label": "final", "actions": total})
    path.write_text("\n".join(lines) + "\n")
    return {"until_ms": at + frame_ms, "actions": total, "phases": phases,
            "actions_per_frame": actions_per_frame, "frame_ms": frame_ms,
            "cleanup_actions_per_phase": 5, "warmup_actions": 9}


def compare_states(phases, provenance, reference):
    """Compare replay state, excluding cumulative scheduling and process metrics."""
    previous = json.loads(reference.read_text())
    failures = []
    if previous.get("exit_code") or previous.get("timed_out") or previous.get("violations") or previous.get("missing_phases"):
        failures.append("state comparison reference did not pass")
    for key in ["binary_sha256", "input_sha256", "pinned_fixture_source_sha256",
                "live_world_json_sha256"]:
        if provenance.get(key) != previous.get("provenance", {}).get(key):
            failures.append(f"state comparison provenance differs: {key}")
    def states(items):
        return {phase["phase"]: {key: value for key, value in phase.get("source", {}).items()
                                 if key != "frame_requests_total"}
                for phase in items if phase["phase"] != "cold"}
    actual, wanted = states(phases), states(previous.get("phases", []))
    if actual.keys() != wanted.keys():
        failures.append("state comparison phase labels differ")
    for label in actual.keys() & wanted.keys():
        for key in actual[label].keys() | wanted[label].keys():
            if actual[label].get(key) != wanted[label].get(key):
                failures.append(f"state comparison {label}: {key} differs")
    return {"reference": str(reference.resolve()), "passed": not failures,
            "excluded": ["cold bootstrap state", "frame_requests_total",
                         "PID", "wall time", "RSS", "physical footprint", "CPU"],
            "violations": failures}


def run(command, directory, timeout, interval, expected=None, cwd=None, provenance=None,
        compare_reference=None, sample_phase=None, sample_seconds=10):
    memory = MacMemory()
    directory.mkdir(parents=True, exist_ok=True)
    started = time.monotonic()
    sample_count, phases, buffer = 0, [], b""
    timed_out = False
    sampler = None
    cpu_profile = None
    peak = {"rss_bytes": 0, "footprint_bytes": 0, "peak_footprint_bytes": 0}
    with (directory / "process.log").open("wb") as log, \
            (directory / "samples.jsonl").open("w") as trace:
        process = subprocess.Popen(command, stdout=subprocess.PIPE,
                                   stderr=subprocess.STDOUT, start_new_session=True, cwd=cwd)
        selector = selectors.DefaultSelector()
        selector.register(process.stdout, selectors.EVENT_READ)
        deadline = started + timeout
        next_sample = started

        def observe(label=None, source=None):
            nonlocal sample_count, sampler, cpu_profile
            sample = {"wall_ms": round((time.monotonic() - started) * 1000, 3),
                      "pid": process.pid, **memory.sample(process.pid)}
            if label:
                sample["phase"] = label
                if source:
                    sample["source"] = source
                phases.append(sample)
            for field in peak:
                peak[field] = max(peak[field], sample.get(field, 0))
            trace.write(json.dumps(sample) + "\n")
            trace.flush()
            # Only scalar values retained, never target images or ledgers.
            sample_count += 1
            if sample_phase is not None and label == sample_phase and sampler is None:
                profile_path = directory / "cpu.sample.txt"
                sample_command = ["/usr/bin/sample", str(process.pid), str(sample_seconds),
                                  "10", "-file", str(profile_path.resolve())]
                with (directory / "cpu-sample.log").open("wb") as sample_log:
                    sampler = subprocess.Popen(sample_command, stdout=sample_log,
                                               stderr=subprocess.STDOUT)
                cpu_profile = {"phase": label, "command": sample_command,
                               "started_wall_ms": sample["wall_ms"],
                               "path": str(profile_path.resolve()),
                               "limitations": "Sampling perturbs this diagnostic run; its timing is not acceptance evidence."}

        while selector.get_map():
            now = time.monotonic()
            if now >= deadline and process.poll() is None:
                timed_out = True
                os.killpg(process.pid, signal.SIGTERM)
                try:
                    process.wait(timeout=2)
                except subprocess.TimeoutExpired:
                    os.killpg(process.pid, signal.SIGKILL)
            if now >= next_sample and process.poll() is None:
                observe()
                next_sample = now + interval
            for key, _ in selector.select(timeout=min(interval, 0.05)):
                data = os.read(key.fileobj.fileno(), 65536)
                if not data:
                    selector.unregister(key.fileobj)
                    continue
                log.write(data)
                log.flush()
                buffer += data
                while b"\n" in buffer:
                    line, buffer = buffer.split(b"\n", 1)
                    prefix = b"FACET_MEMORY_PHASE "
                    if line.startswith(prefix):
                        payload = line[len(prefix):].decode("utf-8", "replace")
                        source = None
                        if payload.startswith("{"):
                            source = json.loads(payload)
                            payload = source.pop("label")
                        observe(payload, source)
                    state_prefix = b"FACET_MEMORY_STATE "
                    if line.startswith(state_prefix):
                        source = json.loads(line[len(state_prefix):])
                        label = source.pop("label")
                        # The actual next-draw snapshot supersedes the raw
                        # adapter mark (which precedes motion sampling).
                        phases[:] = [phase for phase in phases if phase["phase"] != label]
                        observe(label, source)
        exit_code = process.wait()
        if sampler:
            try:
                cpu_profile["exit_code"] = sampler.wait(timeout=30)
            except subprocess.TimeoutExpired:
                sampler.terminate()
                try:
                    sampler.wait(timeout=2)
                except subprocess.TimeoutExpired:
                    sampler.kill()
                    sampler.wait()
                cpu_profile["timed_out_processing"] = True
    labels = {phase["phase"] for phase in phases}
    missing = [phase["label"] for phase in (expected or [])
               if phase["label"] not in labels]
    warm = next((phase for phase in phases if phase["phase"] == "warm"), None)
    if warm:
        for phase in phases:
            phase["delta_from_warm"] = {
                field: phase[field] - warm[field]
                for field in ["rss_bytes", "footprint_bytes"]
                if field in phase and field in warm
            }
    violations = []
    if any(value <= 0 for value in peak.values()):
        violations.append("no complete process memory measurement was available")
    for phase in phases:
        if "error" in phase or any(phase.get(key, 0) <= 0 for key in peak):
            violations.append(f"{phase['phase']}: missing process memory measurement: {phase.get('error', 'zero/unavailable counters')}")
    if sample_phase is not None:
        if cpu_profile is None:
            violations.append(f"CPU profile phase {sample_phase!r} was not observed")
        elif cpu_profile.get("exit_code") != 0 or not Path(cpu_profile["path"]).exists():
            violations.append("CPU profile did not complete successfully")
    if expected:
        for phase in phases:
            if phase["phase"] == "cold":
                continue
            source = phase.get("source", {})
            if not source:
                violations.append(f"{phase['phase']}: missing source snapshot")
                continue
            for key, wanted in [("moving", False), ("pending_motion", 0),
                                ("floating_entries", 0),
                                ("find_open", False), ("searching", False),
                                ("exploration", "free"), ("focused", None),
                                ("hovered", None), ("prism", None)]:
                if source.get(key) != wanted:
                    violations.append(f"{phase['phase']}: {key}={source.get(key)!r}, expected {wanted!r}")
            if source.get("frames_requested") != 0:
                violations.append(f"{phase['phase']}: still requests frames")
            retained = source.get("retained", {})
            if not retained:
                violations.append(f"{phase['phase']}: missing retained counters")
            for key, limit in [("motion_tracks", 2), ("trail", 24),
                               ("search_cache", 16), ("prism_rows", 0),
                               ("tours", source.get("world_packages", 0))]:
                if retained.get(key, limit + 1) > limit:
                    violations.append(f"{phase['phase']}: retained {key}={retained.get(key)!r} exceeds {limit}")
            if warm and phase["phase"] != "warm":
                base = warm.get("source", {})
                for key in ["viewport", "camera", "world_nodes", "source"]:
                    if source.get(key) != base.get(key):
                        violations.append(f"{phase['phase']}: {key} differs from warm")
        report_path = directory / "run.json"
        if report_path.exists():
            report = json.loads(report_path.read_text())
            if "soak" in command:
                for key, wanted in [("retained_frames", 0), ("retained_frame_timings", 0),
                                    ("invalidations_available", True),
                                    ("cpu_is_performance_evidence", False)]:
                    if report.get(key) != wanted:
                        violations.append(f"soak report {key}={report.get(key)!r}, expected {wanted!r}")
            coverage = report.get("coverage", {})
            if not coverage:
                violations.append("missing actual-state coverage")
            for key, value in coverage.items():
                if value <= 0:
                    violations.append(f"coverage {key} is zero")
        else:
            violations.append("missing gallery run report")
    result = {"command": command, "cwd": str(cwd), "provenance": provenance,
              "pid": process.pid, "exit_code": exit_code,
              "timed_out": timed_out, "wall_seconds": time.monotonic() - started,
              "sample_count": sample_count, "sample_interval_ms": interval * 1000,
              "cpu_profile": cpu_profile,
              "max_sampled": peak, "phases": phases, "missing_phases": missing,
              "violations": violations,
              "measurement": "macOS proc_pid_rusage RUSAGE_INFO_V4 bytes",
              "limitations": [
                  "Marker samples occur when the parent reads stderr, near the phase boundary.",
                  "RSS/footprint include GPUI, fonts, renderer, and harness allocations.",
                  "Gallery uses GPUI HeadlessAppContext; actual on-screen Metal/window resource retention is outside this replay.",
                  "Peak physical footprint is a kernel lifetime high-water mark.",
                  "Growth alone is not proof of a leak; inspect repeated warm phases and source counters.",
              ]}
    if compare_reference:
        comparison = compare_states(phases, provenance, compare_reference)
        result["state_comparison"] = comparison
        violations.extend(comparison["violations"])
    if "motion-report" in command:
        result["limitations"].append(
            "motion-report retains every frame's ledger/state; its accumulator grows with frame count.")
    (directory / "memory.json").write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps({key: result[key] for key in ["pid", "exit_code", "timed_out",
                     "wall_seconds", "max_sampled", "phases", "missing_phases", "violations"]}, indent=2))
    return 124 if timed_out else (exit_code or (1 if missing or violations else 0))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--timeout", type=float, default=300)
    parser.add_argument("--interval-ms", type=float, default=20)
    parser.add_argument("--gallery", type=Path)
    parser.add_argument("--cwd", type=Path, default=Path(__file__).resolve().parents[2])
    parser.add_argument("--scene", default="graph-pinned-world")
    parser.add_argument("--counts", default="100,1000")
    parser.add_argument("--cycles", type=int, default=3)
    parser.add_argument("--actions-per-frame", type=int, default=1)
    parser.add_argument("--frame-ms", type=int, default=32)
    parser.add_argument("--command", default="soak", choices=["soak", "motion-report"])
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument("--compare-state", type=Path,
                        help="require settled source state to match a prior passing memory.json replay")
    parser.add_argument("--sample-phase", help="start read-only macOS CPU sampling at a phase marker")
    parser.add_argument("--sample-seconds", type=int, default=10)
    parser.add_argument("target", nargs=argparse.REMAINDER)
    args = parser.parse_args()
    if (args.timeout <= 0 or args.interval_ms <= 0 or args.cycles <= 0
            or args.actions_per_frame <= 0 or args.frame_ms <= 0 or args.sample_seconds <= 0):
        parser.error("timeout, interval, cycles, actions-per-frame, frame-ms, and sample-seconds must be positive")
    args.out.mkdir(parents=True, exist_ok=True)
    if args.self_test:
        memory = MacMemory()
        before = memory.sample(os.getpid())
        allocation = bytearray(32 * 1024 * 1024)
        after = memory.sample(os.getpid())
        assert allocation[0] == 0
        assert after["footprint_bytes"] - before["footprint_bytes"] > 24 * 1024 * 1024, (before, after)
        assert after["rss_bytes"] - before["rss_bytes"] > 24 * 1024 * 1024, (before, after)
        assert after["peak_footprint_bytes"] >= after["footprint_bytes"]
        print(json.dumps({"self_test": "passed", "before": before, "after": after}, indent=2))
        return 0
    expected = None
    provenance = {}
    def digest(path):
        with path.open("rb") as file:
            return hashlib.file_digest(file, "sha256").hexdigest()
    if args.gallery:
        counts = [int(count) for count in args.counts.split(",")]
        if not counts or any(count <= 0 for count in counts):
            parser.error("counts must be positive")
        script = args.out / "input.txt"
        plan = build_script(script, counts, args.cycles, args.actions_per_frame, args.frame_ms)
        (args.out / "plan.json").write_text(json.dumps(plan, indent=2) + "\n")
        command = [str(args.gallery.resolve()), args.command, "--scene", args.scene,
                   "--size", "1440x900", "--scale", "1", "--frame-ms", str(args.frame_ms),
                   "--input-file", str(script.resolve()), "--until", str(plan["until_ms"]),
                   "--out", str((args.out / "run.json").resolve())]
        expected = plan["phases"]
        provenance["binary_sha256"] = digest(args.gallery.resolve())
        provenance["input_sha256"] = digest(script)
        fixture = args.cwd / "apps/facet/src/graph/gallery_fixture.rs"
        if fixture.exists():
            provenance["pinned_fixture_source_sha256"] = digest(fixture)
        world = args.cwd / "Nudox-Design-System/v4/graph/world.json"
        if world.exists():
            provenance["live_world_json_sha256"] = digest(world)
    else:
        command = args.target[1:] if args.target[:1] == ["--"] else args.target
        if not command:
            parser.error("provide --gallery or a target command after --")
    provenance["runner_sha256"] = digest(Path(__file__))
    return run(command, args.out, args.timeout, args.interval_ms / 1000,
               expected, args.cwd.resolve(), provenance, args.compare_state,
               args.sample_phase, args.sample_seconds)


if __name__ == "__main__":
    raise SystemExit(main())
