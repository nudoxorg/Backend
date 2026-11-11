#!/usr/bin/env python3
"""Automated native QA for a portable linux-x64 release candidate.

Runs inside a stock Ubuntu 24.04 container (the release pipeline's QA task):
no Nix, a fresh HOME, and only the packages a desktop user has (curl, Xvfb,
Mesa's software Vulkan driver, fonts). It installs the candidate with the
exact install-linux.sh this release will publish, drives every case the
release contract requires for linux-x64, and writes native-qa-linux-x64.json
beside the candidate only when all of them pass.

The archive is served from 127.0.0.1 rather than GitHub, because QA runs
before the candidate is uploaded; the installer's pinned SHA-256 still binds
the bytes, and the download, checksum, extract and launch steps are the
user's own.

A real API key (NUDOX_QA_API_KEY) is required: sign-in persistence and MCP
tool calls go through api.nudox.org, and each QA run records a few tool calls
against that key's account. ``--rehearse`` runs every case that needs no
account with a well-formed dummy key and never writes a QA record, so the
pipeline can be proven before a key exists.
"""
from __future__ import annotations

import argparse
import datetime
import functools
import http.server
import json
import os
from pathlib import Path
import re
import signal
import socketserver
import subprocess
import sys
import tempfile
import threading
import time

sys.path.insert(0, str(Path(__file__).resolve().parent))
from release_contract import linux_shell_installer, sha256  # noqa: E402
from release_platforms import PLATFORMS  # noqa: E402

PLATFORM = "linux-x64"
SYSTEM_PATH = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"
SYMBOL = "frobnicate_release_widget"
ACCOUNT_CASES = {"preferences", "cli_mcp"}
REHEARSAL_KEY = "ndx_" + "0" * 64
FIXTURE = f'''"""Release QA fixture."""


def {SYMBOL}(count: int) -> int:
    """Return twice the count."""
    return count * 2
'''


class QAFailure(Exception):
    pass


def check(condition: bool, message: str) -> None:
    if not condition:
        raise QAFailure(message)


class Session:
    """One clean user account: a new HOME, a minimal environment, the installed commands."""

    def __init__(self, work: Path, candidate: Path, manifest: dict, tag: str, key: str, rehearse: bool):
        self.work, self.candidate, self.manifest, self.tag, self.key = work, candidate, manifest, tag, key
        self.rehearse = rehearse
        self.home = work / "home"
        self.home.mkdir()
        self.bin = self.home / ".local" / "bin"
        self.prefix = self.home / ".local" / "share" / "nudox" / tag
        self.project = self.home / "qa-project"
        self.installer = work / "install-linux.sh"
        template = Path(__file__).with_name("install_linux.sh.in").read_bytes()
        self.installer.write_bytes(linux_shell_installer(template, tag, manifest))
        self.server = None
        self.desktop = None

    def env(self, **extra: str) -> dict:
        # Built from nothing: no inherited LD_LIBRARY_PATH, XDG_* or /nix paths.
        env = {"HOME": str(self.home), "PATH": f"{self.bin}:{SYSTEM_PATH}", "LANG": "C.UTF-8", "USER": "qa"}
        if self.rehearse:
            # Product commands only check a key is present; nothing here reaches the service.
            env["NUDOX_API_KEY"] = self.key
        env.update(extra)
        return env

    def run(self, command: list[str], *, timeout: int = 120, stdin: str | None = None, cwd: Path | None = None,
            env: dict | None = None, ok: bool = True) -> subprocess.CompletedProcess:
        result = subprocess.run(command, input=stdin, capture_output=True, text=True, timeout=timeout,
                                cwd=cwd or self.home, env=env or self.env())
        if ok:
            check(result.returncode == 0, f"{' '.join(command)} exited {result.returncode}: {(result.stderr or result.stdout)[-800:]}")
        return result

    def serve_candidate(self) -> str:
        handler = functools.partial(http.server.SimpleHTTPRequestHandler, directory=str(self.candidate))
        handler.log_message = lambda *args: None
        self.server = socketserver.TCPServer(("127.0.0.1", 0), handler)
        threading.Thread(target=self.server.serve_forever, daemon=True).start()
        return f"http://127.0.0.1:{self.server.server_address[1]}/{self.manifest['asset']}"

    def locald_pids(self) -> list[int]:
        """backend-locald processes started from this install."""
        pids = []
        for proc in Path("/proc").iterdir():
            if not proc.name.isdigit():
                continue
            try:
                exe = Path(os.readlink(proc / "exe"))
            except OSError:
                continue
            if exe.name == "backend-locald" and self.home in exe.parents:
                pids.append(int(proc.name))
        return pids

    def close(self) -> None:
        if self.desktop is not None and self.desktop.poll() is None:
            self.desktop.terminate()
        for pid in self.locald_pids():
            os.kill(pid, signal.SIGTERM)
        if self.server is not None:
            self.server.shutdown()


def glibc_floor(manifest: dict) -> tuple[int, int]:
    match = re.fullmatch(r"Linux glibc ([0-9]+)\.([0-9]+)", manifest["minimum_os"])
    check(match is not None, "candidate has no glibc floor")
    return int(match.group(1)), int(match.group(2))


def os_release() -> dict:
    fields = {}
    for line in Path("/etc/os-release").read_text().splitlines():
        if "=" in line:
            key, value = line.split("=", 1)
            fields[key] = value.strip('"')
    return fields


# ── cases, in the order they run ─────────────────────────────────────────


def case_minimum_os(s: Session) -> None:
    """Ubuntu 24.04's glibc meets the floor measured from every bundled ELF."""
    release = os_release()
    check(release.get("ID") == "ubuntu" and release.get("VERSION_ID") == "24.04", f"QA host is not Ubuntu 24.04: {release}")
    host = s.run(["getconf", "GNU_LIBC_VERSION"]).stdout.split()[-1]
    check(tuple(int(part) for part in host.split(".")[:2]) >= glibc_floor(s.manifest),
          f"host glibc {host} is below the candidate floor {s.manifest['minimum_os']}")


def case_install(s: Session) -> None:
    url = s.serve_candidate()
    s.run(["sh", str(s.installer)], env=s.env(NUDOX_INSTALL_ARCHIVE_URL=url), timeout=300)
    for command in ("nudox", "nudox-desktop", "nudox-mcp"):
        check((s.bin / command).is_symlink(), f"installer did not link {command}")
    version = s.run(["nudox", "--version"]).stdout
    check(s.manifest["version"] in version, f"installed nudox reports {version!r}, not {s.manifest['version']}")


def case_clean_environment(s: Session) -> None:
    """No Nix on the host, and every installed ELF resolves without one."""
    check(not Path("/nix").exists(), "QA host has a /nix directory")
    elves = [path for path in sorted((s.prefix / "bin").iterdir()) + sorted((s.prefix / "lib").iterdir())
             if path.is_file() and path.read_bytes()[:4] == b"\x7fELF"]
    check(len(elves) > 4, "installed tree has too few ELF images")
    for path in elves:
        linkage = s.run(["ldd", str(path)]).stdout
        check("not found" not in linkage, f"{path.name} has an unresolved library:\n{linkage}")
        check("/nix/" not in linkage, f"{path.name} resolves a library from /nix:\n{linkage}")


def case_preferences(s: Session) -> None:
    """Sign-in is saved owner-only and a new process reads it back."""
    s.run(["nudox", "auth", "login"], stdin=s.key + "\n")
    saved = [path for path in s.home.rglob("api-key") if path.is_file()]
    check(len(saved) == 1, f"expected one saved key file, found {saved}")
    check(saved[0].stat().st_mode & 0o077 == 0, f"saved key {saved[0]} is readable by others")
    status = s.run(["nudox", "--format", "json", "auth", "status"]).stdout
    check('"signed_in":true' in status.replace(" ", ""), f"a new process does not see the saved sign-in: {status[-400:]}")


def case_project_index_search(s: Session) -> None:
    s.project.mkdir()
    (s.project / "qa_fixture.py").write_text(FIXTURE)
    indexed = s.run(["nudox", "add", ".", "--wait"], cwd=s.project, timeout=600)
    check("failed" not in indexed.stdout, f"index operation failed:\n{indexed.stdout[-1200:]}")
    found = s.run(["nudox", "search", SYMBOL], cwd=s.project).stdout
    check(SYMBOL in found and "qa_fixture.py" in found, f"search did not find the fixture:\n{found[-800:]}")


def case_bundled_helpers(s: Session) -> None:
    """The CLI found and started the backend-locald installed beside it."""
    pids = s.locald_pids()
    check(len(pids) >= 1, "no backend-locald from the installed tree is running after indexing")


def case_cold_restart(s: Session) -> None:
    """Stop the local owner; the next command restarts it and the index is still there."""
    before = s.locald_pids()
    for pid in before:
        os.kill(pid, signal.SIGTERM)
    deadline = time.monotonic() + 30
    while any(Path(f"/proc/{pid}").exists() for pid in before) and time.monotonic() < deadline:
        time.sleep(0.5)
    check(not any(Path(f"/proc/{pid}").exists() for pid in before), "backend-locald did not stop on SIGTERM")
    found = s.run(["nudox", "search", SYMBOL], cwd=s.project, timeout=180).stdout
    check(SYMBOL in found, f"search after a cold restart lost the index:\n{found[-800:]}")
    check(set(s.locald_pids()) - set(before), "no new backend-locald after the restart")


def case_cli_mcp(s: Session) -> None:
    """The installed MCP server speaks the protocol and a signed-in tool call answers."""
    requests = [
        {"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
            "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "nudox-release-qa", "version": "1"}}},
        {"jsonrpc": "2.0", "method": "notifications/initialized"},
        {"jsonrpc": "2.0", "id": 2, "method": "tools/list"},
        {"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "backend.search", "arguments": {"query": SYMBOL}}},
    ]
    stdin = "".join(json.dumps(request) + "\n" for request in requests)
    result = s.run(["nudox-mcp", "--project", str(s.project)], stdin=stdin, cwd=s.project, timeout=180)
    replies = {reply.get("id"): reply for reply in map(json.loads, filter(None, result.stdout.splitlines()))}
    check(set(replies) >= {1, 2, 3}, f"MCP answered {sorted(replies)} of requests 1-3: {result.stderr[-400:]}")
    check(replies[1].get("result", {}).get("serverInfo", {}).get("version") == s.manifest["version"],
          f"MCP initialize reports the wrong version: {replies[1]}")
    tools = {tool["name"] for tool in replies[2].get("result", {}).get("tools", [])}
    check("backend.search" in tools, f"MCP tools/list lacks backend.search: {sorted(tools)[:10]}")
    call = replies[3].get("result", {})
    text = " ".join(item.get("text", "") for item in call.get("content", []))
    check(not call.get("isError") and SYMBOL in text, f"MCP backend.search failed: {text[:400]}")


def case_desktop_launch(s: Session) -> None:
    """The desktop maps its window on a fresh X server and stays up."""
    display = ":99"
    xvfb = subprocess.Popen(["Xvfb", display, "-screen", "0", "1440x900x24", "-nolisten", "tcp"],
                            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try:
        time.sleep(2)
        log = (s.work / "desktop.log").open("w")
        s.desktop = subprocess.Popen(["nudox-desktop"], env=s.env(DISPLAY=display), cwd=s.home,
                                     stdout=log, stderr=subprocess.STDOUT)
        deadline, mapped = time.monotonic() + 60, False
        while time.monotonic() < deadline and s.desktop.poll() is None:
            tree = subprocess.run(["xwininfo", "-root", "-tree"], capture_output=True, text=True,
                                  env={"DISPLAY": display, "PATH": SYSTEM_PATH}).stdout
            if re.search(r'"Nudox"', tree):
                mapped = True
                break
            time.sleep(1)
        log_tail = (s.work / "desktop.log").read_text()[-1200:]
        check(mapped, f"desktop window never mapped (exit {s.desktop.poll()}):\n{log_tail}")
        time.sleep(10)
        check(s.desktop.poll() is None, f"desktop exited {s.desktop.returncode} after mapping:\n{log_tail}")
        s.desktop.terminate()
        s.desktop.wait(30)
    finally:
        xvfb.terminate()
        xvfb.wait(30)


def case_browser_download_launch(s: Session) -> None:
    """The website path: download the archive, check it, unpack it by hand, run it."""
    downloads = s.home / "Downloads"
    downloads.mkdir()
    archive = downloads / s.manifest["asset"]
    s.run(["curl", "-fsSL", "-o", str(archive), f"http://127.0.0.1:{s.server.server_address[1]}/{s.manifest['asset']}"])
    check(sha256(archive) == s.manifest["sha256"], "downloaded archive differs from the release manifest")
    s.run(["tar", "-xzf", str(archive), "-C", str(downloads)])
    version = s.run([str(downloads / "Nudox" / "bin" / "nudox"), "--version"]).stdout
    check(s.manifest["version"] in version, f"unpacked nudox reports {version!r}")


def case_install_uninstall(s: Session) -> None:
    """Uninstall removes what install wrote and keeps the user's sign-in and projects."""
    s.close()
    s.run(["sh", str(s.installer), "--uninstall"])
    for command in ("nudox", "nudox-desktop", "nudox-mcp"):
        check(not (s.bin / command).exists() and not (s.bin / command).is_symlink(), f"uninstall left {command}")
    check(not s.prefix.exists(), "uninstall left the installed tree")
    if not s.rehearse:
        check(any(path.is_file() for path in s.home.rglob("api-key")), "uninstall deleted the saved sign-in")
    check((s.project / "qa_fixture.py").is_file(), "uninstall touched the user's project")


CASES = {
    "minimum_os": case_minimum_os,
    "install": case_install,
    "clean_environment": case_clean_environment,
    "preferences": case_preferences,
    "project_index_search": case_project_index_search,
    "bundled_helpers": case_bundled_helpers,
    "cold_restart": case_cold_restart,
    "cli_mcp": case_cli_mcp,
    "desktop_launch": case_desktop_launch,
    "browser_download_launch": case_browser_download_launch,
    "install_uninstall": case_install_uninstall,
}


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--candidate", required=True, type=Path, help="candidate directory from linux_formal_release.py")
    parser.add_argument("--tag", required=True, help="the candidate tag install-linux.sh is rendered for")
    parser.add_argument("--tested-by", required=True, help="who ran this QA, e.g. the Concourse build")
    parser.add_argument("--rehearse", action="store_true", help="skip the account cases and write no QA record")
    args = parser.parse_args()

    key = REHEARSAL_KEY if args.rehearse else os.environ.get("NUDOX_QA_API_KEY", "").strip()
    if not re.fullmatch(r"ndx_[0-9a-f]{64}", key):
        print("release QA stopped: NUDOX_QA_API_KEY must hold a Nudox API key (ndx_ + 64 hex)", file=sys.stderr)
        return 2
    manifest = json.loads((args.candidate / f"release-manifest-{PLATFORM}.json").read_text())
    check(manifest["platform"] == PLATFORM and sha256(args.candidate / manifest["asset"]) == manifest["sha256"],
          "candidate archive differs from its manifest")
    required = PLATFORMS[PLATFORM]["qa"]
    check(required <= set(CASES), f"QA runner lacks cases {sorted(required - set(CASES))}")

    results = {}
    with tempfile.TemporaryDirectory(prefix="nudox-release-qa-") as temporary:
        session = Session(Path(temporary), args.candidate.resolve(), manifest, args.tag, key, args.rehearse)
        try:
            for name, case in CASES.items():
                if args.rehearse and name in ACCOUNT_CASES:
                    print(f"QA {name}: skipped (rehearsal has no account)", flush=True)
                    continue
                started = time.monotonic()
                try:
                    case(session)
                except (QAFailure, OSError, subprocess.SubprocessError, ValueError, KeyError) as error:
                    print(f"QA {name}: FAILED after {time.monotonic() - started:.1f}s: {error}", flush=True)
                    results[name] = False
                    break
                print(f"QA {name}: passed in {time.monotonic() - started:.1f}s", flush=True)
                results[name] = True
        finally:
            session.close()
    expected = set(CASES) - (ACCOUNT_CASES if args.rehearse else set())
    if not all(results.get(name) for name in expected):
        print("release QA failed; no native-qa record written", file=sys.stderr)
        return 1
    if args.rehearse:
        print(f"release QA rehearsal passed {len(expected)} cases; no record written (account cases skipped)")
        return 0

    release = os_release()
    record = {
        "schema": 1,
        "archive_sha256": manifest["sha256"],
        "source_sha": manifest["source_sha"],
        "tested_by": args.tested_by,
        "tested_at": datetime.datetime.now(datetime.UTC).replace(microsecond=0).isoformat().replace("+00:00", "Z"),
        "os_version": release.get("PRETTY_NAME", "Ubuntu 24.04"),
        "host_os": PLATFORMS[PLATFORM]["host"],
        "target": manifest["target"],
        "cases": {name: True for name in sorted(required)},
    }
    (args.candidate / f"native-qa-{PLATFORM}.json").write_text(json.dumps(record, indent=2, sort_keys=True) + "\n")
    print(f"release QA passed: {len(required)} cases on {record['os_version']}")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except QAFailure as error:
        print(f"release QA stopped: {error}", file=sys.stderr)
        raise SystemExit(1)
