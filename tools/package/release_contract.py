#!/usr/bin/env python3
"""Validate immutable native candidates, publish them, and promote one platform."""
from __future__ import annotations

import argparse
import base64
import datetime
import stat
import tarfile
import tempfile
import hashlib
import http.client
import json
import os
import plistlib
import re
import subprocess
import sys
import tomllib
import urllib.error
import urllib.parse
import urllib.request
import zipfile
from pathlib import Path

from release_platforms import PLATFORMS

ASSET = PLATFORMS["macos"]["asset"]
QA_CASES = PLATFORMS["macos"]["qa"]


def sha256(path):
    digest = hashlib.sha256()
    with Path(path).open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def read_json(path):
    path = Path(path)
    if path.is_symlink() or not path.is_file() or path.stat().st_size > 65536:
        raise ValueError("release JSON must be a regular file under 64 KiB")
    return json.loads(path.read_text())


def metadata_path(directory, name, platform=None):
    directory = Path(directory)
    paths = [directory / (name + ".json")]
    if platform:
        paths.append(directory / f"{name}-{platform}.json")
    else:
        paths.extend(directory.glob(name + "-*.json"))
    existing = [path for path in paths if path.exists() or path.is_symlink()]
    if len(existing) != 1:
        raise ValueError(f"candidate requires exactly one {name} regular file")
    return existing[0]


def archive_evidence(archive, profile):
    def inspect(names, read, regular):
        if len(names) != len(set(names)) or any(
            name.startswith("/") or ".." in name.split("/") or "\\" in name
            or ":" in name or name.split("/")[0] != profile["root"] for name in names
        ):
            raise ValueError("archive contains ambiguous or unsafe paths")
        if profile["host"] == "Windows" and len({name.casefold() for name in names}) != len(names):
            raise ValueError("archive contains case-ambiguous Windows paths")
        for executable in profile["executables"]:
            if executable not in names or not regular(executable):
                raise ValueError("archive omits a required product executable")
        build_bytes = read(profile["build"], 16 * 1024 * 1024)
        info = None
        if profile["host"] == "Darwin":
            info = plistlib.loads(read("Nudox.app/Contents/Info.plist", 65536))
        return build_bytes, info
    if archive.name.endswith(".zip"):
        with zipfile.ZipFile(archive) as bundle:
            def regular(name):
                mode = bundle.getinfo(name).external_attr >> 16
                return not bundle.getinfo(name).is_dir() and not stat.S_ISLNK(mode)
            def read(name, limit):
                if not regular(name) or bundle.getinfo(name).file_size > limit:
                    raise ValueError("invalid or oversized archive evidence")
                return bundle.read(name)
            return inspect(bundle.namelist(), read, regular)
    with tarfile.open(archive, "r:gz") as bundle:
        members = bundle.getmembers()
        # Portable Linux payloads must be staged with actual files, not links
        # escaping to the build machine's Nix store or compiler installation.
        if any(not (member.isfile() or member.isdir()) for member in members):
            raise ValueError("Linux archive contains links or special files")
        def read(name, limit):
            member = bundle.getmember(name)
            if not member.isfile() or member.size > limit:
                raise ValueError("invalid or oversized archive evidence")
            with bundle.extractfile(member) as stream:
                return stream.read(limit + 1)
        return inspect([member.name for member in members], read,
                       lambda name: bundle.getmember(name).isfile())


def validate(directory, source=None, platform=None, require_qa=True):
    directory = Path(directory)
    manifest = read_json(metadata_path(directory, "release-manifest", platform))
    required = {"schema", "version", "source_sha", "source_tree", "cargo_lock_sha256", "platform", "target", "asset", "sha256", "size_bytes", "minimum_os", "build_manifest_sha256", "signed", "notarized"}
    if set(manifest) != required or manifest["schema"] != 1:
        raise ValueError("invalid release manifest schema")
    if not isinstance(manifest["version"], str) or not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", manifest["version"]):
        raise ValueError("invalid release version")
    for name in ("source_sha", "source_tree"):
        if not isinstance(manifest[name], str) or not re.fullmatch(r"[a-f0-9]{40}", manifest[name]):
            raise ValueError(f"invalid {name}")
    for name in ("cargo_lock_sha256", "sha256", "build_manifest_sha256"):
        if not isinstance(manifest[name], str) or not re.fullmatch(r"[a-f0-9]{64}", manifest[name]):
            raise ValueError(f"invalid {name}")
    selected = manifest["platform"]
    if not isinstance(selected, str) or selected not in PLATFORMS or (platform and selected != platform):
        raise ValueError("candidate belongs to the wrong platform lane")
    profile = PLATFORMS[selected]
    if manifest["target"] not in profile["targets"] or manifest["asset"] != profile["asset"]:
        raise ValueError("candidate target/asset differs from platform contract")
    if manifest["signed"] is not profile["signed"] or manifest["notarized"] is not profile["notarized"]:
        raise ValueError("candidate is missing platform signing/notarization evidence")
    if not isinstance(manifest["minimum_os"], str) or not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9 ._+-]{0,63}", manifest["minimum_os"]):
        raise ValueError("candidate requires a supported minimum OS")
    if selected == "macos" and not re.fullmatch(r"[0-9]+\.[0-9]+(?:\.[0-9]+)?", manifest["minimum_os"]):
        raise ValueError("invalid macOS minimum version")
    archive = directory / manifest["asset"]
    if (archive.is_symlink() or not archive.is_file() or type(manifest["size_bytes"]) is not int
            or archive.stat().st_size != manifest["size_bytes"] or sha256(archive) != manifest["sha256"]):
        raise ValueError("candidate archive differs from its immutable manifest")
    build_bytes, info = archive_evidence(archive, profile)
    build = json.loads(build_bytes)
    if hashlib.sha256(build_bytes).hexdigest() != manifest["build_manifest_sha256"] or any(build["source"].get(key) != manifest[field] for key, field in (("git_revision", "source_sha"), ("git_tree", "source_tree"), ("cargo_lock_sha256", "cargo_lock_sha256"))):
        raise ValueError("archive build evidence differs from release source")
    if info and (info.get("CFBundleShortVersionString") != manifest["version"] or info.get("LSMinimumSystemVersion") != manifest["minimum_os"]):
        raise ValueError("archive version/minimum OS differs from release metadata")
    checksum = directory / (manifest["asset"] + ".sha256")
    if checksum.is_symlink() or checksum.read_text() != f'{manifest["sha256"]}  {manifest["asset"]}\n':
        raise ValueError("checksum sidecar differs from manifest")
    if require_qa:
        qa = read_json(metadata_path(directory, "native-qa", selected))
        fields = {"schema", "archive_sha256", "source_sha", "tested_by", "tested_at", profile["qa_host"], "cases"}
        if selected != "macos":
            fields |= {"host_os", "target"}
        if set(qa) != fields or qa["schema"] != 1:
            raise ValueError("invalid native acceptance record")
        if qa["archive_sha256"] != manifest["sha256"] or qa["source_sha"] != manifest["source_sha"]:
            raise ValueError("native QA does not attest this exact archive and source")
        if not all(isinstance(qa[name], str) and qa[name].strip() for name in ("tested_by", "tested_at", profile["qa_host"])):
            raise ValueError("native QA requires operator, timestamp and host OS")
        try:
            tested_at = datetime.datetime.fromisoformat(qa["tested_at"].replace("Z", "+00:00"))
            if tested_at.utcoffset() != datetime.timedelta(0):
                raise ValueError("native acceptance timestamp must be UTC")
        except (TypeError, ValueError) as error:
            raise ValueError("native acceptance timestamp must be UTC ISO 8601") from error
        if selected != "macos" and (qa["host_os"] != profile["host"] or qa["target"] != manifest["target"]):
            raise ValueError("native QA was performed on the wrong OS/architecture")
        if not isinstance(qa["cases"], dict) or set(qa["cases"]) != profile["qa"] or any(value is not True for value in qa["cases"].values()):
            raise ValueError("native acceptance has missing or failed cases")
    if source is not None:
        source = Path(source)
        revision = manifest["source_sha"]
        subprocess.run(["git", "-C", str(source), "merge-base", "--is-ancestor", revision, "HEAD"], check=True)
        def blob(path):
            return subprocess.check_output(["git", "-C", str(source), "show", f"{revision}:{path}"])
        if hashlib.sha256(blob("Cargo.lock")).hexdigest() != manifest["cargo_lock_sha256"]:
            raise ValueError("candidate lock differs from selected canonical source")
        tree = subprocess.check_output(["git", "-C", str(source), "rev-parse", revision + "^{tree}"], text=True).strip()
        if tree != manifest["source_tree"] or tomllib.loads(blob("Cargo.toml").decode())["workspace"]["package"]["version"] != manifest["version"]:
            raise ValueError("candidate version/tree differs from selected canonical source")
    return manifest


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


class GitHub:
    def __init__(self, repo, token):
        if not re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", repo) or not token:
            raise ValueError("release publishing needs a repository and dedicated token")
        self.repo, self.token = repo, token

    def api(self, path, method="GET", body=None):
        data = json.dumps(body).encode() if body is not None else None
        request = urllib.request.Request("https://api.github.com/repos/" + self.repo + path, data=data, method=method,
            headers={"Authorization": "Bearer " + self.token, "Accept": "application/vnd.github+json", "Content-Type": "application/json", "X-GitHub-Api-Version": "2022-11-28"})
        with urllib.request.urlopen(request, timeout=60) as response:
            return json.load(response)

    def upload(self, release_id, path):
        path = Path(path)
        connection = http.client.HTTPSConnection("uploads.github.com", timeout=120)
        endpoint = f"/repos/{self.repo}/releases/{release_id}/assets?name=" + urllib.parse.quote(path.name)
        try:
            connection.putrequest("POST", endpoint)
            connection.putheader("Authorization", "Bearer " + self.token)
            connection.putheader("Content-Type", "application/octet-stream")
            connection.putheader("Content-Length", str(path.stat().st_size))
            connection.endheaders()
            with path.open("rb") as stream:
                for chunk in iter(lambda: stream.read(1024 * 1024), b""):
                    connection.send(chunk)
            response = connection.getresponse()
            if response.status != 201:
                raise ValueError(f"GitHub asset upload failed: HTTP {response.status}")
            return json.load(response)
        finally:
            connection.close()

    def asset_response(self, asset):
        request = urllib.request.Request(f'https://api.github.com/repos/{self.repo}/releases/assets/{asset["id"]}',
            headers={"Authorization": "Bearer " + self.token, "Accept": "application/octet-stream"})
        try:
            return urllib.request.build_opener(NoRedirect).open(request, timeout=120)
        except urllib.error.HTTPError as error:
            if error.code != 302:
                raise
            location = error.headers.get("Location", "")
            error.close()
            if urllib.parse.urlparse(location).scheme != "https":
                raise ValueError("GitHub returned a non-HTTPS asset location")
            # Never forward a private-repository token to the download CDN.
            return urllib.request.urlopen(location, timeout=120)

    def asset_hash(self, asset):
        digest = hashlib.sha256()
        with self.asset_response(asset) as response:
            for chunk in iter(lambda: response.read(1024 * 1024), b""):
                digest.update(chunk)
        return digest.hexdigest()

    def asset_json(self, asset):
        with self.asset_response(asset) as response:
            data = response.read(65537)
        if len(data) > 65536:
            raise ValueError("published release evidence exceeds size limit")
        return json.loads(data)

    def find_release(self, tag):
        try:
            return self.api("/releases/tags/" + urllib.parse.quote(tag, safe=""))
        except urllib.error.HTTPError as error:
            if error.code != 404:
                raise
        # GitHub's tag endpoint finds published releases. Include drafts when
        # retrying staging or a partially uploaded stable release.
        matches = []
        for page in range(1, 101):
            releases = self.api(f"/releases?per_page=100&page={page}")
            matches.extend(release for release in releases if release["tag_name"] == tag)
            if len(releases) < 100:
                break
        else:
            raise ValueError("release inventory exceeds lookup limit; refusing to create a duplicate")
        if len(matches) > 1:
            raise ValueError("multiple draft releases have this tag; inspect before retrying")
        return matches[0] if matches else None

    def publish(self, directory, tag, stable, platform=None, draft_only=False):
        directory = Path(directory)
        manifest = validate(directory, platform=platform, require_qa=not draft_only)
        selected = manifest["platform"]
        if stable and draft_only:
            raise ValueError("a stable release cannot bypass native QA")
        if stable and tag != "v" + manifest["version"]:
            raise ValueError("stable tag does not match candidate version")
        if not stable and not re.fullmatch(re.escape("v" + manifest["version"]) + r"-rc\.[0-9]+-" + re.escape(selected), tag):
            raise ValueError("candidate tag must be v<version>-rc.<number>-<platform>")
        release = self.find_release(tag)
        if release is None:
            release = self.api("/releases", "POST", {"tag_name": tag, "name": "NuDox " + tag, "draft": True, "prerelease": not stable, "make_latest": "false", "body": f'Verified native artifacts. Canonical Forgejo Backend source: {manifest["source_sha"]}. The GitHub tag is a distribution identifier.'})
        source_marker = re.search(r"Canonical Forgejo Backend source: ([a-f0-9]{40})", release.get("body", ""))
        if source_marker and source_marker.group(1) != manifest["source_sha"]:
            raise ValueError("all platforms in one release version must use the same source revision")
        if release["assets"] and not source_marker and not any(asset["name"].startswith("release-manifest-") for asset in release["assets"]):
            raise ValueError("existing release has no source contract; use a new version")
        assets = {asset["name"]: asset for asset in release["assets"]}
        if not release["draft"] and bool(release.get("prerelease")) == stable:
            raise ValueError("published tag has the wrong stable/prerelease status; use a new version")
        for name, asset in assets.items():
            if name.startswith("release-manifest-") and name.endswith(".json"):
                existing = self.asset_json(asset)
                if existing.get("source_sha") != manifest["source_sha"] or existing.get("version") != manifest["version"]:
                    raise ValueError("all platforms in one release version must use the same source revision")
        self.release_url = release.get("html_url", f"https://github.com/{self.repo}/releases")
        kinds = ("release-manifest",) if draft_only else ("release-manifest", "native-qa")
        names = [manifest["asset"], manifest["asset"] + ".sha256"] + [f"{kind}-{selected}.json" for kind in kinds]
        with tempfile.TemporaryDirectory(prefix="nudox-release-evidence-") as temporary:
            staging = Path(temporary)
            for kind in kinds:
                (staging / f"{kind}-{selected}.json").write_bytes(metadata_path(directory, kind, selected).read_bytes())
            paths = [directory / name for name in names[:2]] + [staging / name for name in names[2:]]
            self.publish_assets(release, paths, assets, stable)
        if release["draft"] and not draft_only:
            self.api(f'/releases/{release["id"]}', "PATCH", {"draft": False, "prerelease": not stable, "make_latest": "false"})
        return manifest

    def publish_assets(self, release, paths, assets, stable):
        for path in paths:
            name = path.name
            expected = sha256(path)
            if name in assets:
                if self.asset_hash(assets[name]) != expected:
                    raise ValueError(f"refusing to overwrite immutable asset {name} in {release["tag_name"]}")
            elif release["draft"] or stable:
                uploaded = self.upload(release["id"], path)
                if self.asset_hash(uploaded) != expected:
                    raise ValueError("uploaded release bytes failed read-back verification")
            else:
                raise ValueError("published release is missing an expected asset; use a new version")


def check_fast(revision, platform="macos"):
    token = os.environ.get("FORGEJO_TOKEN", "")
    if not token:
        raise ValueError("fast-gate verification requires Forgejo read access")
    endpoint = f"https://dev.nudox.org/api/v1/repos/Nudox/Backend/statuses/{revision}?limit=100"
    request = urllib.request.Request(endpoint, headers={"Authorization": "token " + token})
    with urllib.request.urlopen(request, timeout=30) as response:
        statuses = json.load(response)
    for context in PLATFORMS[platform]["ci"]:
        matches = [entry for entry in statuses if entry["context"] == context]
        if not matches or max(matches, key=lambda entry: entry["id"])["status"] != "success":
            raise ValueError(f"selected source revision has no passing {context} status")


def promote(manifest, host, key, known_hosts):
    command = ["ssh", "-i", str(key), "-o", "BatchMode=yes", "-o", "IdentitiesOnly=yes", "-o", "StrictHostKeyChecking=yes", "-o", "UserKnownHostsFile=" + str(known_hosts), "-o", "ConnectTimeout=10", host]
    def request(data):
        result = subprocess.run(command, input=json.dumps(data), text=True, stdout=subprocess.PIPE, check=True)
        return json.loads(result.stdout)
    current = request({"action": "show"})
    if public_metadata() != current["catalog"]:
        raise ValueError("public auth release metadata does not match the active channel catalog; deploy routing first")
    channel = {name: manifest[name] for name in ("version", "source_sha", "asset", "sha256", "minimum_os")}
    channel.update(tag="v" + manifest["version"], status="available")
    updated = request({"action": "promote", "expected_sha256": current["sha256"], "platform": manifest["platform"], "channel": channel})
    for platform, before in current["catalog"]["platforms"].items():
        if platform != manifest["platform"] and updated["catalog"]["platforms"][platform] != before:
            raise ValueError("promotion changed an unrelated platform")
    try:
        verify_public_download(manifest, updated["catalog"])
    except (ValueError, OSError) as error:
        try:
            restored = request({"action": "rollback", "expected_sha256": updated["sha256"], "platform": manifest["platform"], "history_sha256": current["sha256"]})
            if restored["catalog"] != current["catalog"] or public_metadata() != current["catalog"]:
                raise ValueError("rollback verification failed")
        except (ValueError, OSError, subprocess.CalledProcessError) as rollback_error:
            raise ValueError("public download verification failed; automatic rollback also failed, inspect the active catalog") from rollback_error
        raise ValueError("public download verification failed; previous platform channel restored") from error
    return updated["sha256"]


def public_metadata(require_versioned=False):
    with urllib.request.urlopen("https://api.nudox.org/v1/releases", timeout=30) as response:
        if require_versioned and response.headers.get("X-Nudox-Versioned-Downloads") != "1":
            raise ValueError("deploy the versioned Auth download route before publishing")
        data = response.read(65537)
    if len(data) > 65536:
        raise ValueError("public release metadata exceeds size limit")
    return json.loads(data)


def verify_public_download(manifest, expected_catalog):
    if public_metadata() != expected_catalog:
        raise ValueError("public metadata did not refresh to promoted release")
    urls = [f'https://api.nudox.org/v1/downloads/{manifest["platform"]}', versioned_download_url(manifest)]
    for url in urls:
        digest = hashlib.sha256()
        with urllib.request.urlopen(url, timeout=120) as response:
            for chunk in iter(lambda: response.read(1024 * 1024), b""):
                digest.update(chunk)
        if digest.hexdigest() != manifest["sha256"]:
            raise ValueError("public download differs from accepted release archive")


def versioned_download_url(manifest):
    return f'https://api.nudox.org/v1/downloads/{manifest["platform"]}/{manifest["version"]}/{manifest["sha256"]}'


def homebrew_cask(manifest):
    if manifest["platform"] != "macos":
        raise ValueError("Homebrew cask requires a macOS candidate")
    major = int(manifest["minimum_os"].split(".")[0])
    releases = {11: "big_sur", 12: "monterey", 13: "ventura", 14: "sonoma", 15: "sequoia", 26: "tahoe"}
    if major not in releases:
        raise ValueError("add the supported macOS release to the Homebrew cask mapping")
    return f'''cask "nudox" do
  version "{manifest["version"]}"
  sha256 "{manifest["sha256"]}"

  url "{versioned_download_url(manifest)}"
  container type: :zip
  name "Nudox"
  desc "Local-first code intelligence"
  homepage "https://nudox.org"

  depends_on arch: :arm64
  depends_on macos: :{releases[major]}
  app "Nudox.app"
  binary "#{{appdir}}/Nudox.app/Contents/MacOS/nudox-cli", target: "nudox"
  binary "#{{appdir}}/Nudox.app/Contents/MacOS/nudox-mcp"
  binary "#{{appdir}}/Nudox.app/Contents/MacOS/nudox-locald"

  caveats "Language indexing requires the corresponding native compiler/toolchain."
end
'''


def update_homebrew(manifest):
    channel = public_metadata()["platforms"][manifest["platform"]]
    expected = {name: manifest[name] for name in ("version", "source_sha", "asset", "sha256", "minimum_os")}
    if channel != {**expected, "tag": "v" + manifest["version"], "status": "available"}:
        raise ValueError("Homebrew update requires this exact artifact to be the current promoted release")
    content = homebrew_cask(manifest).encode()
    github = GitHub("nudoxorg/homebrew-tap", os.environ.get("NUDOX_HOMEBREW_TOKEN", ""))
    path = "/contents/Casks/nudox.rb"
    body = {"message": f'Publish Nudox {manifest["version"]} ({manifest["source_sha"][:12]})',
            "content": base64.b64encode(content).decode(), "branch": "main"}
    try:
        current = github.api(path + "?ref=main")
        if base64.b64decode(current["content"]) == content:
            return
        body["sha"] = current["sha"]
    except urllib.error.HTTPError as error:
        if error.code != 404:
            raise
    github.api(path, "PUT", body)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=("validate", "cask", "stage", "submit", "candidate", "publish", "homebrew"))
    parser.add_argument("--candidate", required=True, type=Path)
    parser.add_argument("--source", type=Path)
    parser.add_argument("--check-fast", "--check-ci", dest="check_fast", action="store_true", help="check exact source CI: fast plus the selected platform lane")
    parser.add_argument("--repo", default="nudoxorg/backend")
    parser.add_argument("--candidate-tag")
    parser.add_argument("--rc", type=int)
    parser.add_argument("--platform", choices=PLATFORMS)
    parser.add_argument("--output", type=Path)
    parser.add_argument("--local-preview", action="store_true", help="generate a QA-only cask using this local archive")
    parser.add_argument("--promotion-host")
    parser.add_argument("--promotion-key", type=Path)
    parser.add_argument("--known-hosts", type=Path)
    args = parser.parse_args()
    mutating = args.action not in {"validate", "cask"}
    if mutating and args.source is None:
        raise ValueError("submission/publication requires the canonical source checkout")
    manifest = validate(args.candidate, args.source, args.platform, require_qa=args.action not in {"stage", "cask"})
    if args.check_fast or mutating:
        check_fast(manifest["source_sha"], manifest["platform"])
    if args.action == "cask":
        if args.output is None:
            raise ValueError("cask generation requires --output")
        content = homebrew_cask(manifest)
        if args.local_preview:
            content = content.replace(versioned_download_url(manifest), (args.candidate / manifest["asset"]).resolve().as_uri())
        args.output.parent.mkdir(parents=True, exist_ok=True)
        # Never accidentally overwrite an existing recipe during acceptance.
        with args.output.open("x") as stream:
            stream.write(content)
    elif args.action == "homebrew":
        update_homebrew(manifest)
    elif args.action != "validate":
        github = GitHub(args.repo, os.environ.get("NUDOX_RELEASE_TOKEN", ""))
        if args.action in {"candidate", "stage", "submit"}:
            if args.action in {"stage", "submit"}:
                if args.rc is None or args.rc < 1:
                    raise ValueError("stage/submit requires --rc with a positive candidate number")
                args.candidate_tag = f'v{manifest["version"]}-rc.{args.rc}-{manifest["platform"]}'
            if not args.candidate_tag:
                raise ValueError("candidate staging requires --candidate-tag")
            github.publish(args.candidate, args.candidate_tag, False, args.platform, draft_only=args.action == "stage")
        else:
            if not all((args.promotion_host, args.promotion_key, args.known_hosts)):
                raise ValueError("publishing requires channel promotion credentials and pinned SSH host key")
            metadata = public_metadata(require_versioned=True)
            if metadata.get("schema") != 1 or set(metadata.get("platforms", {})) != {"macos", "linux-x64", "linux-arm64", "windows"}:
                raise ValueError("platform-specific auth routing must be deployed before publishing")
            github.publish(args.candidate, "v" + manifest["version"], True, args.platform)
            promote(manifest, args.promotion_host, args.promotion_key, args.known_hosts)
    result = {"action": args.action, "version": manifest["version"], "source_sha": manifest["source_sha"], "sha256": manifest["sha256"]}
    if args.action == "stage":
        result["preview_url"] = github.release_url
    print(json.dumps(result))


if __name__ == "__main__":
    try:
        main()
    except (ValueError, OSError, KeyError, zipfile.BadZipFile, tarfile.TarError, subprocess.CalledProcessError) as error:
        if isinstance(error, urllib.error.HTTPError):
            message = f"release service returned HTTP {error.code}"
        else:
            message = str(error)
        print("release refused: " + message, file=sys.stderr)
        sys.exit(1)
