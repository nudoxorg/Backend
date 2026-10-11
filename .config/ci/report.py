#!/usr/bin/env python3
"""Summarize CI-METRIC log records without treating polling jobs as CI runs."""

import argparse
from collections import defaultdict
import json
import math
from pathlib import Path
import re
import statistics


def summarize(logs):
    attempts = {}
    step_attempts = {}
    for log in logs:
        for line in re.sub(r"\x1b\[[0-?]*[ -/]*[@-~]", "", log).splitlines():
            if not line.startswith("CI-METRIC "):
                continue
            record = json.loads(line.removeprefix("CI-METRIC "))
            if record.get("kind") == "ci-step":
                key = (record["head_sha"], record["lane"], record["step"], record["started_at"])
                step_attempts[key] = record
                continue
            if record.get("kind") != "cargo-check":
                continue
            key = (record["head_sha"], record["target"], record["started_at"])
            attempts[key] = record
    groups = defaultdict(list)
    for record in attempts.values():
        outcome = "passed" if record["exit_code"] == 0 else "failed"
        groups[(record["target"], outcome)].append(record)
    summaries = []
    for (target, outcome), records in sorted(groups.items()):
        seconds = sorted(record["seconds"] for record in records)
        fresh = sum(record["fresh_artifacts"] for record in records)
        rebuilt = sum(record["rebuilt_artifacts"] for record in records)
        summaries.append({
            "target": target,
            "outcome": outcome,
            "attempts": len(records),
            "mean_seconds": round(statistics.mean(seconds), 3),
            "max_seconds": max(seconds),
            # Under 100 observations the nearest-rank p99 is just the maximum.
            "observed_p99_seconds": seconds[math.ceil(len(seconds) * .99) - 1] if len(seconds) >= 100 else None,
            "fresh_artifacts": fresh,
            "rebuilt_artifacts": rebuilt,
            "artifact_reuse_fraction": round(fresh / (fresh + rebuilt), 4) if fresh + rebuilt else None,
        })
    step_groups = defaultdict(list)
    for record in step_attempts.values():
        step_groups[(record["lane"], record["step"], record["outcome"])].append(record)
    step_summaries = []
    for (lane, step, outcome), records in sorted(step_groups.items()):
        seconds = sorted(record["seconds"] for record in records)
        step_summaries.append({
            "lane": lane,
            "step": step,
            "outcome": outcome,
            "attempts": len(records),
            "mean_seconds": round(statistics.mean(seconds), 3),
            "max_seconds": max(seconds),
            "observed_p99_seconds": seconds[math.ceil(len(seconds) * .99) - 1] if len(seconds) >= 100 else None,
        })
    return {
        "measurement": "Cargo check execution only; excludes queueing, checkout, shell preparation and runtime tests",
        "p99_note": "Observed nearest-rank percentile; omitted below 100 attempts. Sample size and workload mix still matter.",
        "summaries": summaries,
        "attempts": list(attempts.values()),
        "step_measurement": "Named CI step execution; excludes queueing, checkout and dev-shell preparation. Failed and timed-out steps are separate from passes.",
        "step_summaries": step_summaries,
        "step_attempts": list(step_attempts.values()),
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("logs", nargs="+", type=Path)
    args = parser.parse_args()
    print(json.dumps(summarize(path.read_text() for path in args.logs), indent=2))


if __name__ == "__main__":
    main()
