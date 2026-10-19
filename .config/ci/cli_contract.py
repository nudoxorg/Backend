#!/usr/bin/env python3
"""Artifact-driven CLI usage and absent-owner contracts (W1.3).

Run all cases, or select one with --case NAME. No Cargo, indexing, owner startup,
or golden update is involved. Expected diagnostics come from the reviewed user
contract, independently of the product's command registry/request enums.
"""
from __future__ import annotations

import argparse
import json
import shlex
import subprocess
import process_custody
import tempfile
import time
from pathlib import Path

from smoke import isolated_env

# name, arguments, format, exit, operand, required diagnostic fragments
FAULTS = [
    ("unknown-command", ["serch", "ferris"], "json", 64, "serch", ["did you mean `search`"]),
    ("unknown-option", ["search", "ferris", "--deep"], "json", 64, "--deep", ["--limit"]),
    ("missing-coordinate", ["show"], "json", 64, None, ["nudox show [COORDINATE]", "--selected-id SELECTED_ID", "supply a coordinate or --selected-id"]),
    ("missing-alias-coordinate", ["document"], "json", 64, "document", ["nudox document [COORDINATE]", "--selected-id SELECTED_ID", "--cursor CURSOR"]),
    ("out-of-range-limit", ["search", "ferris", "--limit", "900"], "json", 64, "limit", ["900"]),
    # Errors from global option parsing deliberately use the human fallback.
    ("unpaged-packages-zero", ["packages", "--limit", "0"], "human", 64, None, ["--limit", "does not take a page bound"]),
    ("unpaged-packages-limit", ["packages", "--limit", "20"], "human", 64, None, ["--limit", "does not take a page bound"]),
    ("unpaged-health-limit", ["health", "--limit", "5"], "human", 64, None, ["--limit", "does not take a page bound"]),
    ("passive-search", ["--passive", "search", "symbols"], "human", 64, None, ["--passive"]),
    ("invalid-execution-intent", ["add", ".", "--execution-intent", "remote"], "json", 64, "execution-intent", ["choose interactive or background"]),
    ("wait-timeout-without-wait", ["add", ".", "--wait-timeout", "30"], "json", 64, "--wait-timeout", ["--wait-timeout requires --wait"]),
    ("wait-timeout-above-bound", ["add", ".", "--wait", "--wait-timeout", "901"], "json", 64, "--wait-timeout", ["integer from 0 to 900 seconds"]),
    ("wait-timeout-negative", ["add", ".", "--wait", "--wait-timeout", "-1"], "json", 64, "--wait-timeout", ["integer from 0 to 900 seconds"]),
    ("wait-on-search", ["search", "ferris", "--wait"], "json", 64, "--wait", ["search takes only", "--limit"]),
]
CASES = ["help", "add-help", *(case[0] for case in FAULTS), "passive-absent-health", "passive-absent-status"]


def require(condition: bool, message: str) -> None:
    if not condition:
        raise RuntimeError(message)


def check_fault(result: subprocess.CompletedProcess, fmt: str, code: int,
                operand: str | None, fragments: list[str], slug: str = "usage") -> dict | None:
    require(result.returncode == code, f"exit {result.returncode}, expected {code}: {result.stdout!r} {result.stderr!r}")
    if fmt == "json":
        body = json.loads(result.stdout)
        require(isinstance(body, dict) and body.get("answer") == "fault", "missing typed fault")
        require(body.get("slug") == slug, f"wrong fault slug: {body}")
        require(not result.stderr, f"JSON fault leaked diagnostics to stderr: {result.stderr!r}")
        if operand is not None:
            require(body.get("operand") == operand, f"lost offending operand: {body}")
        text = body.get("detail", "")
    else:
        body = None
        require(not result.stdout, "human fault unexpectedly used stdout")
        text = result.stderr
    for fragment in fragments:
        require(fragment in text, f"missing diagnostic {fragment!r}: {text!r}")
    return body


def run_one(bin_dir: Path, name: str) -> float:
    started = time.monotonic()
    with tempfile.TemporaryDirectory(prefix="ncc-", dir="/tmp") as temporary:
        root = Path(temporary)
        project = root / "p é ' $(printf BAD)"
        project.mkdir(mode=0o700)
        workspace = root / "state `printf BAD`; literal"
        endpoint = root / "run" / "owner.sock"
        marker = root / "owner-started"
        launcher = root / "unexpected-locald"
        # A launch attempt is a failed test, never a substitute successful owner.
        launcher.write_text(f"#!/bin/sh\nprintf started > {shlex.quote(str(marker))}\nexit 73\n")
        launcher.chmod(0o700)
        env = isolated_env(root / "home")
        env.update(BACKEND_LOCALD_BIN=str(launcher), NO_COLOR="1")
        scope = ["--project", str(project), "--workspace", str(workspace), "--endpoint", str(endpoint)]

        def invoke(arguments: list[str], fmt: str = "json") -> subprocess.CompletedProcess:
            return process_custody.run([str(bin_dir / "backend-cli"), *scope, "--format", fmt, *arguments],
                                  cwd=project, env=env, stdin=subprocess.DEVNULL,
                                  capture_output=True, text=True, timeout=10)

        if name in ("help", "add-help"):
            result = invoke(["--help"] if name == "help" else ["add", "--help"])
            require(result.returncode == 0 and not result.stderr, "help did not exit cleanly")
            fragments = (["--format human|markdown|json", "--passive", "nudox add .",
                          'nudox search "error handling"', "nudox mcp-config", "Claude Code/Codex stdio setup",
                          "no config writes or service start", "surface <JSON>"]
                         if name == "help" else ["current directory", "--execution-intent", "bounded remote calibration",
                                                     "--wait", "--wait-timeout SECONDS", "terminal receipt"])
            for fragment in fragments:
                require(fragment in result.stdout, f"help omits {fragment!r}")
        elif name.startswith("passive-absent-"):
            command = name.removeprefix("passive-absent-")
            result = invoke(["--passive", command])
            # An absent endpoint is an I/O failure (exit 1), not a usage error.
            body = check_fault(result, "json", 1, str(endpoint), [], "endpoint")
            require(body.get("cause") == "unreachable", "absent owner lost unreachable cause")
            require("without starting it" in body.get("detail", ""), "passive refusal lost no-start explanation")
            require("call" not in body, "CLI refusal exposed MCP-only recovery arguments")
            recovery = body.get("shell", "")
            # The observed recovery retains its executable, passive mode, and
            # private authority boundary in a subshell. shlex must recover the
            # exact argv even when selected paths contain quotes and shell syntax.
            executable = str(bin_dir / "backend-cli")
            expected = [
                "(unset",
                "BACKEND_LOCALD_AUTHORITY_SECRET_FILE;",
                executable,
                "--project",
                str(project),
                "--workspace",
                str(workspace),
                "--passive",
                "--endpoint",
                str(endpoint),
                "health)" if command == "health" else "status)",
            ]
            tokens = shlex.split(recovery)
            require(tokens == expected, f"recovery changed its wrapper or selected argv: {tokens!r}")
            # Only after the exact token sequence is admitted, replace its
            # observed executable with a harmless argv printer. Never execute
            # the recovery command itself.
            quoted_executable = shlex.quote(executable)
            require(recovery.startswith("(unset BACKEND_LOCALD_AUTHORITY_SECRET_FILE; " + quoted_executable + " "),
                    f"recovery did not begin with its observed executable: {recovery!r}")
            stubbed = recovery.replace(quoted_executable + " ", "nudox ", 1)
            parsed = process_custody.run(["/bin/sh", "-c", 'nudox() { printf "%s\\000" "$@"; }; ' + stubbed],
                                    cwd=project, env=env, capture_output=True, timeout=10)
            require(parsed.returncode == 0 and not parsed.stderr, "recovery is not valid shell syntax")
            expected_argv = ["--project", str(project), "--workspace", str(workspace), "--passive",
                             "--endpoint", str(endpoint), command]
            require(parsed.stdout == b"".join(arg.encode() + b"\0" for arg in expected_argv),
                    "recovery changed literal operands in a real shell roundtrip")
            markdown = invoke(["--passive", command], "markdown")
            require(markdown.returncode == 1 and not markdown.stdout, "Markdown passive refusal used wrong exit/stream")
            require(markdown.stderr.endswith("→ Run in a shell:\n\n    " + recovery.replace("\n", "\n    ") + "\n"), "Markdown lost exact recovery command")
        else:
            _, arguments, fmt, code, operand, fragments = next(case for case in FAULTS if case[0] == name)
            check_fault(invoke(arguments, fmt), fmt, code, operand, fragments)
        require(not marker.exists(), "command unexpectedly launched locald")
        require(not workspace.exists(), "command unexpectedly created workspace state")
        require(not endpoint.parent.exists(), "command unexpectedly created endpoint directory")
    return round(time.monotonic() - started, 3)


def run_cases(bin_dir: Path, selected: list[str] | None = None) -> dict:
    names = CASES if selected is None else selected
    unknown = set(names) - set(CASES)
    require(not unknown and bool(names), f"invalid CLI cases: {sorted(unknown)}")
    seconds = {}
    failures = []
    for name in names:
        try:
            seconds[name] = run_one(bin_dir.resolve(), name)
            print(f"cli-contract: {name} passed ({seconds[name]}s)", flush=True)
        except Exception as error:
            failures.append(f"{name}: {error}")
            print(f"cli-contract: {name} failed: {error}", flush=True)
    require(not failures, "CLI contract failures:\n" + "\n".join(failures))
    return {"seconds": seconds}


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bin-dir", type=Path, required=True)
    parser.add_argument("--case", action="append", choices=CASES)
    args = parser.parse_args()
    try:
        run_cases(args.bin_dir, args.case)
    except Exception as error:
        print(f"cli-contract: failed: {error}", flush=True)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
