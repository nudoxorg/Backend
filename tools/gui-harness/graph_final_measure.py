#!/usr/bin/env python3
"""Post-release measurements. Never builds; requires the designated binary SHA.

Run only after the lead's release signal and official quiet perf audit. Uses the
existing gallery commands, streaming memory sampler, and compiler guard.
"""
import argparse
import hashlib
import json
from pathlib import Path
import re
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[2]
TOOLS = Path(__file__).resolve().parent


def digest(path):
    with path.open("rb") as file:
        return hashlib.file_digest(file, "sha256").hexdigest()


def fixture_search_target(path):
    """Derive the pinned fixture's existing qualified node query, if present."""
    if not path.is_file():
        return None
    text = path.read_text()
    modules = re.findall(r'Module\s*\{\s*pkg:\s*\d+,\s*path:\s*"([^"]+)"', text)
    for match in re.finditer(
            r'Node::new\(Kind::\w+,\s*"([^"]+)",\s*\d+,\s*(\d+)\)', text):
        name, module_index = match.group(1), int(match.group(2))
        if name == "RelationLabel" and module_index < len(modules):
            module = modules[module_index]
            return {"query": f"{module}::{name}", "name": name,
                    "source": str(path.resolve())}
    return None


def live_dense_target(world_path, gallery_path):
    """Derive current max-fanout live query and cross-check DenseFocus's target."""
    world = json.loads(world_path.read_text())
    degree = [0] * len(world["nodes"])
    for edge in world["edges"]:
        degree[edge[0]] += 1
        degree[edge[1]] += 1
    # gallery::fanout visits top-level items and breaks ties by lower node ID.
    item_ids = [i for i, node in enumerate(world["nodes"])
                if node.get("u", -1) == -1]
    if not item_ids:
        raise ValueError("live world has no top-level items")
    node_id = max(item_ids, key=lambda i: (degree[i], -i))
    node = world["nodes"][node_id]
    module = world["modules"][node["m"]]
    package = world["packages"][node["p"]]
    target = {"query": f"{package['name']}::{module['path']}::{node['n']}",
              "name": node["n"], "id": node_id, "fanout": degree[node_id],
              "source": str(world_path.resolve())}
    if target["fanout"] <= 0:
        raise ValueError("live world has no positive-fanout subject")
    source = gallery_path.read_text()
    scene_target = re.search(
        r"At::DenseFocus\s*=>\s*\{.*?let\s+target\s*=\s*find\(&world,\s*\"([^\"]+)\",\s*\"([^\"]+)\"\)",
        source, re.S)
    if scene_target:
        declared = f"{scene_target.group(1)}::{scene_target.group(2)}"
        if declared != target["query"]:
            raise ValueError(
                f"live max-fanout subject {target['query']} differs from DenseFocus target {declared}")
        target["declared_dense_focus_query"] = declared
    else:
        raise ValueError("could not derive the declared graph-live-dense-focus subject")
    return target


def input_script(pinned_target, dense_target):
    lines, checks = [], []

    def search(query, first_at, enter_at):
        lines.extend([f"key / @{first_at}",
                      f"type {json.dumps(query)} @{first_at + 64}",
                      f"key enter @{enter_at}"])

    if pinned_target:
        search(pinned_target["query"], 64, 512)
        lines.append("key t @4096")
        checks.append({"label": "first_t", "at_ms": 4096, "before_mode": "free",
                       "after_mode": "tour", "target": pinned_target})
        lines.append("key escape @8192")
        search(pinned_target["query"], 8320, 9216)
        lines.append("key t @12288")
        checks.append({"label": "warm_t", "at_ms": 12288, "before_mode": "free",
                       "after_mode": "tour", "target": pinned_target})
        lines.append("key escape @14336")
        search(pinned_target["query"], 14848, 15872)
        lines.append("key r @20480")
        checks.append({"label": "first_r", "at_ms": 20480, "before_mode": "free",
                       "after_mode": "reach", "target": pinned_target, "retain_focus": True})
        lines.append("key r @28672")
        checks.append({"label": "repeat_r", "at_ms": 28672, "before_mode": "reach",
                       "after_mode": "free", "target": pinned_target, "retain_focus": True})
        lines.append("key escape @32768")
        dense_search_at, dense_enter_at = 33536, 34560
    else:
        dense_search_at, dense_enter_at = 64, 1024

    search(dense_target["query"], dense_search_at, dense_enter_at)
    dense_at = 36864 if pinned_target else 4096
    lines.append(f"key r @{dense_at}")
    checks.append({"label": "dense_r", "at_ms": dense_at, "before_mode": "free",
                   "after_mode": "reach", "target": dense_target, "dense": True,
                   "retain_focus": True})
    lines.append(f"leave @{dense_at + 8192}")
    return "\n".join(lines) + "\n", checks


def analyze_input(path, output, checks):
    report = json.loads(path.read_text())
    frames, failures, costs = report["frames"], [], []
    if not any(check.get("dense") for check in checks):
        failures.append("maximum-fanout Reach dispatch check is absent")
    for check in checks:
        label, at, mode = check["label"], check["at_ms"], check["after_mode"]
        target = check["target"]
        frame = next((f for f in frames if f["at_ms"] == at), None)
        prior = [f for f in frames if f["at_ms"] < at]
        if frame is None or not prior:
            failures.append(f"{label}: exact input frame or preceding state absent")
            continue
        before, after = prior[-1]["state"], frame["state"]
        if (not before.get("focused") or before.get("find_open")
                or before.get("searching") or before.get("moving")
                or before.get("pending_motion") or before.get("exploration") != check["before_mode"]):
            failures.append(f"{label}: source was not focused, ready and settled before dispatch")
        if (before.get("focused") or {}).get("name") != target["name"]:
            failures.append(f"{label}: qualified search did not focus {target['query']}")
        if check.get("retain_focus") and (after.get("focused") or {}).get("name") != target["name"]:
            failures.append(f"{label}: successful R action did not retain focus on {target['query']}")
        if before.get("discovery_prepare_ms") is None or before.get("discovery_ready") is not True:
            failures.append(f"{label}: true worker preparation metric/readiness absent")
        if before.get("source") != "rust-live" or before.get("world_nodes", 0) < 55000:
            failures.append(f"{label}: full live world was not measured")
        if frame.get("input_events") != 1 or after.get("exploration") != mode:
            failures.append(f"{label}: exactly one successful mode input was not observed")
        for field in ("input_cpu_ms", "input_max_ms", "cpu_ms"):
            if not isinstance(frame.get(field), (int, float)) or frame[field] < 0:
                failures.append(f"{label}: {field} absent or invalid")
        costs.append({"label": label, "at_ms": at, "input_cpu_ms": frame.get("input_cpu_ms"),
                      "input_max_ms": frame.get("input_max_ms"), "draw_cpu_ms": frame.get("cpu_ms"),
                      "expected_mode_before": check["before_mode"], "expected_mode_after": mode,
                      "target": target,
                      "focus_before": before.get("focused"), "focus_after": after.get("focused"),
                      "tours_before": before.get("retained", {}).get("tours"),
                      "tours_after": after.get("retained", {}).get("tours")})
    tour_costs = [cost for cost in costs if cost["label"] in {"first_t", "warm_t"}]
    if len(tour_costs) == 2:
        first, warm = tour_costs
        if first["tours_before"] is None or first["tours_after"] != first["tours_before"] + 1:
            failures.append("first T did not create exactly one previously absent tour")
        if warm["tours_before"] != first["tours_after"] or warm["tours_after"] != warm["tours_before"]:
            failures.append("warm T did not reuse the prepared package tour")
    preparation = [f["state"].get("discovery_prepare_ms") for f in frames]
    values = {v for v in preparation if isinstance(v, (int, float)) and v > 0}
    if len(values) != 1:
        failures.append("one stable, positive actual cold worker preparation duration was not reported")
    alignment = report.get("alignment", {})
    result = {"preparation_worker_ms": next(iter(values)) if len(values) == 1 else None,
              "input_costs": costs, "alignment": alignment, "violations": failures,
              "skipped": [] if any(c["label"] == "first_t" for c in checks) else [
                  "pinned RelationLabel T/R cases skipped because the pinned fixture has no matching node"],
              "limitations": ["Each input cost is an individual cold/warm act, not a latency distribution.",
                              "Input time includes dispatch, adapter and immediate foreground task settling.",
                              "Discovery preparation duration measures the worker body; startup and readiness waiting are separate.",
                              "R still performs world-wide reach construction on the UI thread.",
                              "Headless GPUI draw time excludes native GPU presentation."]}
    output.write_text(json.dumps(result, indent=2) + "\n")
    return not failures


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--sha256", required=True)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--mode", choices=["input", "pinned-memory", "live-memory", "dense-memory"], required=True)
    parser.add_argument("--hover-report", type=Path, help="same-binary final quiet perf motion report, required for dense-memory")
    parser.add_argument("--perf-manifest", type=Path, help="same-binary official perf REPORT.json, required for dense-memory")
    args = parser.parse_args()
    binary, out = args.binary.resolve(), args.out.resolve()
    if digest(binary) != args.sha256:
        parser.error("designated frozen binary digest differs; no measurement started")
    out.mkdir(parents=True, exist_ok=True)
    provenance = {"binary": str(binary), "binary_sha256": args.sha256, "cwd": str(ROOT),
                  "runner_sha256": digest(Path(__file__)),
                  "pinned_fixture_source_sha256": digest(ROOT / "apps/facet/src/graph/gallery_fixture.rs"),
                  "live_world_json_sha256": digest(ROOT / "Nudox-Design-System/v4/graph/world.json")}
    (out / "provenance.json").write_text(json.dumps(provenance, indent=2) + "\n")

    def guarded(command, directory, timeout):
        argv = [sys.executable, str(TOOLS / "graph_run_guard.py"), "--out", str(directory / "guard"),
                "--cwd", str(ROOT), "--timeout", str(timeout), "--binary", str(binary),
                "--sha256", args.sha256, "--", *command]
        (directory / "command.json").write_text(json.dumps({"argv": argv, "cwd": str(ROOT)}, indent=2) + "\n")
        return subprocess.run(argv, cwd=ROOT).returncode

    if args.mode == "input":
        script = out / "input.txt"
        pinned_target = fixture_search_target(ROOT / "apps/facet/src/graph/gallery_fixture.rs")
        dense_target = live_dense_target(ROOT / "Nudox-Design-System/v4/graph/world.json",
                                         ROOT / "apps/facet/src/graph/gallery.rs")
        script_text, checks = input_script(pinned_target, dense_target)
        script.write_text(script_text)
        (out / "input-plan.json").write_text(json.dumps({
            "pinned_target": pinned_target, "dense_target": dense_target,
            "checks": checks,
            "skipped": [] if pinned_target else [
                "pinned RelationLabel T/R cases skipped because the pinned fixture has no matching node"],
        }, indent=2) + "\n")
        until_ms = max(check["at_ms"] for check in checks) + 8192 + 16
        input_states = None
        for run in (1, 2):
            directory = out / f"run{run}"
            directory.mkdir()
            report = directory / "motion.json"
            command = [str(binary), "motion-report", "--scene", "graph-world", "--size", "1440x900",
                       "--scale", "2", "--frame-ms", "16", "--input-file", str(script),
                       "--until", str(until_ms), "--out", str(report)]
            status = guarded(command, directory, 240)
            if not report.exists() or not analyze_input(report, directory / "input-costs.json", checks) or status:
                return status or 1
            parsed = json.loads(report.read_text())
            stable = [{**{key: value for key, value in frame.items()
                         if key not in {"cpu_ms", "input_cpu_ms", "input_max_ms", "state"}},
                       "state": {key: value for key, value in frame["state"].items()
                                 if key != "discovery_prepare_ms"}} for frame in parsed["frames"]]
            if input_states is not None and stable != input_states:
                (directory / "state-comparison.json").write_text(json.dumps({"passed": False,
                    "reason": "Consecutive actual replay states differ; CPU and worker duration excluded."}, indent=2) + "\n")
                return 1
            input_states = stable
            if run == 2:
                (directory / "state-comparison.json").write_text(json.dumps({"passed": True,
                    "excluded": ["draw CPU", "input CPU", "worker preparation duration"]}, indent=2) + "\n")
        return 0
    if args.mode in {"pinned-memory", "live-memory"}:
        for run in (1, 2):
            directory = out / f"run{run}"
            directory.mkdir()
            scene, counts, timeout = (("graph-pinned-world", "100,1000", 600)
                                      if args.mode == "pinned-memory" else ("graph-world", "100", 300))
            command = [sys.executable, str(TOOLS / "graph_memory_soak.py"), "--gallery", str(binary),
                       "--cwd", str(ROOT), "--out", str(directory), "--scene", scene,
                       "--counts", counts, "--cycles", "3", "--frame-ms", "32", "--timeout", str(timeout)]
            if run == 2:
                command += ["--compare-state", str(out / "run1/memory.json")]
            status = guarded(command, directory, timeout + 20)
            if status:
                return status
        return 0

    if not args.hover_report or not args.perf_manifest:
        parser.error("dense-memory requires same-binary --hover-report and --perf-manifest")
    manifest = json.loads(args.perf_manifest.read_text())
    if manifest.get("binary_sha256") != args.sha256 or manifest.get("fixture_sha256") != provenance["live_world_json_sha256"]:
        parser.error("dense-memory perf provenance differs")
    report = json.loads(args.hover_report.read_text())
    targets = [int(match.group(1)) for event in report.get("input_events", [])
               if (match := re.fullmatch(r"route graph-hover (\d+)", event["act"]))]
    if not targets or report.get("scene") != "graph-check-live-hover":
        parser.error("actual highest-fanout scene route target absent")
    target = targets[0]
    if not any((frame["state"].get("hovered") or {}).get("id") == target
               and frame["state"]["drawn"].get("hover_relations", 0) > 0 for frame in report["frames"]):
        parser.error("target was not actually picked with real hover relations in reference")
    script, phases, lines = out / "input.txt", ["warm"], ["leave @0", "route graph-memory warm @4096"]
    at = 4160
    for cycle in range(1, 4):
        for iteration in range(100):
            lines += [f"route graph-hover {target} @{at}", f"leave @{at + 768}"]
            at += 1536
        at += 3072
        label = f"cycle{cycle}-settled"
        phases.append(label)
        lines.append(f"route graph-memory {label} @{at}")
        at += 64
    script.write_text("\n".join(lines) + "\n")
    (out / "dense-plan.json").write_text(json.dumps({"target": target, "phases": phases,
        "input_sha256": digest(script), "reference_report_sha256": digest(args.hover_report),
        "reference_manifest_sha256": digest(args.perf_manifest), "cycles": 3, "hover_leave_pairs": 300,
        "until_ms": at, "settle_ms": 3072}, indent=2) + "\n")
    for run in (1, 2):
        directory = out / f"run{run}"
        directory.mkdir()
        command = [sys.executable, str(TOOLS / "graph_memory_soak.py"), "--cwd", str(ROOT),
                   "--out", str(directory), "--timeout", "300", "--", str(binary), "soak",
                   "--scene", "graph-check-live-hover", "--size", "1440x900", "--scale", "2",
                   "--frame-ms", "64", "--input-file", str(script), "--until", str(at),
                   "--out", str(directory / "run.json")]
        status = guarded(command, directory, 320)
        if status:
            return status
        memory, soak = json.loads((directory / "memory.json").read_text()), json.loads((directory / "run.json").read_text())
        failures = []
        actual = {phase["phase"]: phase for phase in memory["phases"]}
        if set(actual) != set(phases):
            failures.append("dense-memory phase coverage differs")
        for phase in actual.values():
            state = phase.get("source", {})
            for key, wanted in [("moving", False), ("pending_motion", 0), ("frames_requested", 0),
                                ("floating_entries", 0), ("hovered", None), ("fading_hover", None)]:
                if state.get(key) != wanted:
                    failures.append(f"{phase['phase']}: {key} did not settle")
            if state.get("camera") != actual.get("warm", {}).get("source", {}).get("camera"):
                failures.append(f"{phase['phase']}: camera differs from warm")
            retained = state.get("retained", {})
            if retained.get("motion_tracks", 99) > 2 or any(retained.get(key) != 0 for key in ("trail", "tours", "prism_rows", "search_cache")):
                failures.append(f"{phase['phase']}: unexpected graph-owned retention")
        if soak.get("retained_frames") != 0 or soak.get("retained_frame_timings") != 0:
            failures.append("dense-memory retained report or trace history")
        if soak.get("coverage", {}).get("hovered_frames", 0) == 0 or soak.get("coverage", {}).get("quiet_frames", 0) == 0:
            failures.append("dense-memory real hover/quiet coverage absent")
        stable = {label: {key: value for key, value in phase.get("source", {}).items()
                          if key not in {"frame_requests_total", "discovery_prepare_ms"}} for label, phase in actual.items()}
        if run == 1:
            reference = stable
        elif stable != reference:
            failures.append("dense-memory consecutive settled source states differ")
        (directory / "dense-check.json").write_text(json.dumps({"violations": failures, "states": stable,
            "excluded_from_identity": ["RSS", "footprint", "PID", "CPU", "wall time", "frame_requests_total", "discovery_prepare_ms"]}, indent=2) + "\n")
        if failures:
            return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
