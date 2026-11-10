#!/usr/bin/env python3
"""The release pipeline's linux-x64 steps around build, QA and submit.

  plan    decide what this canonical commit releases: the Cargo.toml version,
          the next candidate number and its tag, or nothing (writes plan.json).
          --rehearse plans a build that is packaged and QA'd but never tagged
          or submitted, under the placeholder candidate number 0.
  waiver  turn the committed release-ci-waiver.json into the candidate's
          source-bound ci-waiver-linux-x64.json
  tag     create the canonical Forgejo tag the release contract requires: a
          candidate's v<version>-rc.<n>-<platform>, or the stable v<version>
          at the promoted candidate's own source revision

The waiver is a reviewed file in the source, not a pipeline setting: a release
with red CI lanes says so in the commit that is released. It names a version,
so it never carries over to the next one.
"""
from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import tomllib
import urllib.error
import urllib.parse
import urllib.request

sys.path.insert(0, str(Path(__file__).resolve().parent))
from release_contract import check_fast, read_ci_waiver  # noqa: E402

PLATFORM = "linux-x64"
RELEASES_REPO = "nudoxorg/releases"
FORGEJO_API = "https://dev.nudox.org/api/v1/repos/Nudox/Backend"
WAIVER_SOURCE = "tools/package/release-ci-waiver.json"
SOURCE_MARKER = re.compile(r"Canonical Forgejo Backend source: ([a-f0-9]{40})")


def git(source: Path, *args: str) -> str:
    return subprocess.check_output(["git", "-C", str(source), *args], text=True, timeout=60).strip()


def github_releases(token: str) -> list[dict]:
    releases = []
    for page in range(1, 21):
        request = urllib.request.Request(
            f"https://api.github.com/repos/{RELEASES_REPO}/releases?per_page=100&page={page}",
            headers={"Authorization": "Bearer " + token, "Accept": "application/vnd.github+json",
                     "X-GitHub-Api-Version": "2022-11-28"})
        with urllib.request.urlopen(request, timeout=60) as response:
            batch = json.load(response)
        releases.extend(batch)
        if len(batch) < 100:
            return releases
    raise ValueError("release inventory exceeds the lookup limit")


def committed_waiver(source: Path, version: str, revision: str) -> dict | None:
    """The source's waiver for this version, bound to the released revision."""
    path = source / WAIVER_SOURCE
    if not path.exists():
        return None
    committed = json.loads(path.read_text())
    if set(committed) != {"version", "contexts", "reason", "approved_by", "approved_at"}:
        raise ValueError(f"{WAIVER_SOURCE} needs exactly version, contexts, reason, approved_by, approved_at")
    if committed["version"] != version:
        print(f"release waiver: {WAIVER_SOURCE} is for {committed['version']}, not {version}; ignored")
        return None
    return {"schema": 1, "source_sha": revision, **{key: committed[key] for key in ("contexts", "reason", "approved_by", "approved_at")}}


def plan(source: Path, output: Path, rehearse: bool = False) -> dict:
    revision = git(source, "rev-parse", "HEAD")
    version = tomllib.loads((source / "Cargo.toml").read_text())["workspace"]["package"]["version"]
    result = {"version": version, "source_sha": revision, "rc": None, "tag": None, "skip": None, "rehearse": rehearse}
    if rehearse:
        result.update(rc=0, tag=f"v{version}-rc.0-{PLATFORM}")
    else:
        plan_candidate(source, result)
    output.write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps(result))
    return result


def plan_candidate(source: Path, result: dict) -> None:
    """Fill in the next candidate, or why there is none, from the published releases."""
    version, revision = result["version"], result["source_sha"]
    tags = {}
    for release in github_releases(os.environ.get("NUDOX_RELEASE_TOKEN", "")):
        marker = SOURCE_MARKER.search(release.get("body") or "")
        tags[release["tag_name"]] = marker.group(1) if marker else None
    candidates = {int(match.group(1)): sha for tag, sha in tags.items()
                  if (match := re.fullmatch(re.escape(f"v{version}") + r"-rc\.([0-9]+)-" + PLATFORM, tag))}
    if f"v{version}" in tags:
        result["skip"] = f"v{version} is already released; bump workspace.package.version to release again"
        return
    if revision in candidates.values():
        result["skip"] = f"{revision[:10]} already has a {PLATFORM} candidate for {version}"
        return
    result["rc"] = max(candidates, default=0) + 1
    result["tag"] = f"v{version}-rc.{result['rc']}-{PLATFORM}"
    # Fail before the build, not after it: fast must be green, and any
    # other red lane needs this version's committed waiver.
    waiver = committed_waiver(source, version, revision)
    if waiver is not None:
        validate_waiver(waiver)
    check_fast(revision, PLATFORM, waiver)


def validate_waiver(waiver: dict) -> None:
    """Check a waiver with the contract's own reader before anything is built."""
    with tempfile.TemporaryDirectory() as directory:
        Path(directory, f"ci-waiver-{PLATFORM}.json").write_text(json.dumps(waiver))
        read_ci_waiver(directory, {"platform": PLATFORM, "source_sha": waiver["source_sha"]})


def write_waiver(source: Path, candidate: Path) -> None:
    manifest = json.loads((candidate / f"release-manifest-{PLATFORM}.json").read_text())
    if manifest["source_sha"] != git(source, "rev-parse", "HEAD"):
        raise ValueError("candidate was not built from this source checkout")
    waiver = committed_waiver(source, manifest["version"], manifest["source_sha"])
    if waiver is None:
        print("release waiver: none for this version")
        return
    (candidate / f"ci-waiver-{PLATFORM}.json").write_text(json.dumps(waiver, indent=2) + "\n")
    print(f"release waiver: {', '.join(waiver['contexts'])} waived by {waiver['approved_by']}: {waiver['reason']}")


def create_tag(tag: str, revision: str) -> None:
    """Create the canonical Forgejo tag, or accept one that already pins this revision."""
    token = os.environ.get("FORGEJO_TOKEN", "")
    if not token or not re.fullmatch(r"v[0-9]+\.[0-9]+\.[0-9]+(?:-rc\.[1-9][0-9]*-[a-z0-9-]+)?", tag):
        raise ValueError("tag creation needs FORGEJO_TOKEN and a v<version> or candidate tag")
    if not re.fullmatch(r"[a-f0-9]{40}", revision):
        raise ValueError("tag revision must be a full commit SHA")
    headers = {"Authorization": "token " + token, "Content-Type": "application/json"}
    try:
        with urllib.request.urlopen(urllib.request.Request(f"{FORGEJO_API}/tags/{urllib.parse.quote(tag)}", headers=headers), timeout=30) as response:
            existing = json.load(response)
        if existing["commit"]["sha"] != revision:
            raise ValueError(f"Forgejo tag {tag} already exists at another revision")
        print(f"Forgejo tag {tag} already pins {revision[:10]}")
        return
    except urllib.error.HTTPError as error:
        if error.code != 404:
            raise
    body = json.dumps({"tag_name": tag, "target": revision, "message": f"Nudox {tag} release candidate"}).encode()
    with urllib.request.urlopen(urllib.request.Request(f"{FORGEJO_API}/tags", data=body, headers=headers, method="POST"), timeout=30):
        pass
    print(f"Forgejo tag {tag} created at {revision[:10]}")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("action", choices=("plan", "waiver", "tag"))
    parser.add_argument("--source", required=True, type=Path)
    parser.add_argument("--output", type=Path, help="plan: where plan.json goes")
    parser.add_argument("--candidate", type=Path, help="waiver: the candidate directory")
    parser.add_argument("--rehearse", action="store_true", help="plan: package and QA only")
    parser.add_argument("--tag", help="tag: the tag to create")
    parser.add_argument("--revision", help="tag: the commit to tag (default: --source HEAD)")
    args = parser.parse_args()
    if args.action == "plan":
        plan(args.source, args.output, args.rehearse)
    elif args.action == "waiver":
        write_waiver(args.source, args.candidate)
    else:
        create_tag(args.tag, args.revision or git(args.source, "rev-parse", "HEAD"))
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (ValueError, OSError, KeyError, subprocess.CalledProcessError) as error:
        message = f"HTTP {error.code}" if isinstance(error, urllib.error.HTTPError) else str(error)
        print(f"linux release stopped: {message}", file=sys.stderr)
        raise SystemExit(1)
