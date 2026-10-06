#!/usr/bin/env python3
"""Validate immutable Mac candidates, publish them, and promote one platform."""
from __future__ import annotations

import argparse
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
import tarfile
import zipfile
from pathlib import Path

ASSET = "nudox-macos-arm64.zip"
QA_CASES = {"finder_launch", "project_index_search", "bundled_helpers", "preferences", "cold_restart", "clean_environment", "gatekeeper", "minimum_os"}
LINUX_ASSET_RE = re.compile(r"nudox-linux-x86_64-[a-f0-9]{10,40}\.tar\.gz\Z")
LINUX_QA_CASES = {"cli_version", "project_add_search", "mcp_help", "mcp_session", "locald_sibling_discovery", "clean_environment_without_nix_paths", "ubuntu_glibc_floor"}


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


def validate(directory, source=None):
    directory = Path(directory)
    manifest = read_json(directory / "release-manifest.json")
    if manifest.get("platform") == "linux-x64":
        return validate_linux(directory, manifest, source)
    required = {"schema", "version", "source_sha", "source_tree", "cargo_lock_sha256", "platform", "target", "asset", "sha256", "size_bytes", "minimum_os", "build_manifest_sha256", "signed", "notarized"}
    if set(manifest) != required or manifest["schema"] != 1:
        raise ValueError("invalid release manifest schema")
    if not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", manifest["version"]):
        raise ValueError("invalid release version")
    for name in ("source_sha", "source_tree"):
        if not re.fullmatch(r"[a-f0-9]{40}", manifest[name]):
            raise ValueError(f"invalid {name}")
    for name in ("cargo_lock_sha256", "sha256", "build_manifest_sha256"):
        if not re.fullmatch(r"[a-f0-9]{64}", manifest[name]):
            raise ValueError(f"invalid {name}")
    if manifest["platform"] != "macos" or manifest["target"] != "aarch64-apple-darwin" or manifest["asset"] != ASSET:
        raise ValueError("this candidate lane currently accepts Apple Silicon macOS only")
    if manifest["signed"] is not True or manifest["notarized"] is not True or not re.fullmatch(r"[0-9]+\.[0-9]+(?:\.[0-9]+)?", manifest["minimum_os"]):
        raise ValueError("candidate is missing signed/notarized distribution evidence")
    archive = directory / ASSET
    if archive.is_symlink() or archive.stat().st_size != manifest["size_bytes"] or sha256(archive) != manifest["sha256"]:
        raise ValueError("candidate archive differs from its immutable manifest")
    with zipfile.ZipFile(archive) as bundle:
        names = bundle.namelist()
        if len(names) != len(set(names)) or any(name.startswith("/") or ".." in Path(name).parts for name in names):
            raise ValueError("archive contains ambiguous or unsafe paths")
        for executable in ("Nudox", "backend-desktop", "backend-cli", "backend-mcp", "backend-locald"):
            if f"Nudox.app/Contents/MacOS/{executable}" not in names:
                raise ValueError("archive omits a required product executable")
        build_name = "Nudox.app/Contents/Resources/build-manifest.json"
        if bundle.getinfo(build_name).file_size > 16 * 1024 * 1024:
            raise ValueError("bundle build manifest is too large")
        build_bytes = bundle.read(build_name)
        build = json.loads(build_bytes)
        if hashlib.sha256(build_bytes).hexdigest() != manifest["build_manifest_sha256"] or any(build["source"].get(key) != manifest[field] for key, field in (("git_revision", "source_sha"), ("git_tree", "source_tree"), ("cargo_lock_sha256", "cargo_lock_sha256"))):
            raise ValueError("archive build evidence differs from release source")
        info_name = "Nudox.app/Contents/Info.plist"
        if bundle.getinfo(info_name).file_size > 65536:
            raise ValueError("bundle Info.plist is too large")
        info = plistlib.loads(bundle.read(info_name))
        if info.get("CFBundleShortVersionString") != manifest["version"] or info.get("LSMinimumSystemVersion") != manifest["minimum_os"]:
            raise ValueError("archive version/minimum OS differs from release metadata")
    expected_checksum = f'{manifest["sha256"]}  {ASSET}\n'
    if (directory / (ASSET + ".sha256")).read_text() != expected_checksum:
        raise ValueError("checksum sidecar differs from manifest")
    qa = read_json(directory / "native-qa.json")
    if set(qa) != {"schema", "archive_sha256", "source_sha", "tested_by", "tested_at", "macos_version", "cases"} or qa["schema"] != 1:
        raise ValueError("invalid native acceptance record")
    if qa["archive_sha256"] != manifest["sha256"] or qa["source_sha"] != manifest["source_sha"]:
        raise ValueError("native QA does not attest this exact archive and source")
    if not all(isinstance(qa[name], str) and qa[name] for name in ("tested_by", "tested_at", "macos_version")):
        raise ValueError("native QA requires operator, timestamp and host OS")
    if set(qa["cases"]) != QA_CASES or any(value is not True for value in qa["cases"].values()):
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


def validate_linux(directory, manifest, source=None):
    """Validate a receipted Ubuntu x86-64 archive and its native QA record."""
    required = {"schema", "version", "source_sha", "source_tree", "cargo_lock_sha256", "platform", "target", "asset", "sha256", "size_bytes", "minimum_glibc", "build_manifest_sha256", "packaging_manifest_sha256"}
    if set(manifest) != required or manifest["schema"] != 1:
        raise ValueError("invalid Linux release manifest schema")
    if not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", manifest["version"]):
        raise ValueError("invalid Linux release version")
    for name in ("source_sha", "source_tree"):
        if not re.fullmatch(r"[a-f0-9]{40}", manifest[name]):
            raise ValueError(f"invalid Linux {name}")
    for name in ("cargo_lock_sha256", "sha256", "build_manifest_sha256", "packaging_manifest_sha256"):
        if not re.fullmatch(r"[a-f0-9]{64}", manifest[name]):
            raise ValueError(f"invalid Linux {name}")
    if manifest["platform"] != "linux-x64" or manifest["target"] != "x86_64-unknown-linux-gnu" or not LINUX_ASSET_RE.fullmatch(manifest["asset"]):
        raise ValueError("Linux candidate has an unsupported platform, target or archive name")
    if not re.fullmatch(r"[0-9]+\.[0-9]+", manifest["minimum_glibc"]):
        raise ValueError("Linux candidate has an invalid minimum glibc version")
    archive = directory / manifest["asset"]
    if archive.is_symlink() or not archive.is_file() or archive.stat().st_size != manifest["size_bytes"] or sha256(archive) != manifest["sha256"]:
        raise ValueError("Linux archive differs from its immutable manifest")
    checksum = directory / (manifest["asset"] + ".sha256")
    if checksum.is_symlink() or not checksum.is_file() or checksum.read_text() != f'{manifest["sha256"]}  {manifest["asset"]}\n':
        raise ValueError("Linux checksum sidecar differs from manifest")
    required_files = {"nudox-linux-x86_64/bin/backend-cli", "nudox-linux-x86_64/bin/backend-mcp", "nudox-linux-x86_64/bin/backend-locald", "nudox-linux-x86_64/build-manifest.json", "nudox-linux-x86_64/packaging-manifest.json", "nudox-linux-x86_64/README.txt"}
    try:
        with tarfile.open(archive, "r:gz") as bundle:
            members = bundle.getmembers()
            names = [member.name for member in members]
            if len(names) != len(set(names)) or not required_files.issubset(names):
                raise ValueError("Linux archive has duplicate paths or omits required product files")
            if len(members) > 10000:
                raise ValueError("Linux archive has too many members")
            for member in members:
                path = Path(member.name)
                if member.name.startswith("/") or "\\" in member.name or ".." in path.parts or not member.name.startswith("nudox-linux-x86_64/"):
                    raise ValueError("Linux archive contains an unsafe path")
                if member.issym() or member.islnk() or not (member.isdir() or member.isreg()):
                    raise ValueError("Linux archive contains links or special files")
                if member.size < 0 or member.size > 1024 * 1024 * 1024:
                    raise ValueError("Linux archive member exceeds the safety limit")
            for name in required_files & {"nudox-linux-x86_64/bin/backend-cli", "nudox-linux-x86_64/bin/backend-mcp", "nudox-linux-x86_64/bin/backend-locald"}:
                if not (bundle.getmember(name).mode & 0o111):
                    raise ValueError("Linux product executable lost its executable bit")
            build_bytes = bundle.extractfile("nudox-linux-x86_64/build-manifest.json").read(16 * 1024 * 1024 + 1)
            packaging_bytes = bundle.extractfile("nudox-linux-x86_64/packaging-manifest.json").read(16 * 1024 * 1024 + 1)
            if len(build_bytes) > 16 * 1024 * 1024 or len(packaging_bytes) > 16 * 1024 * 1024:
                raise ValueError("Linux package evidence exceeds the 16 MiB safety limit")
            if hashlib.sha256(build_bytes).hexdigest() != manifest["build_manifest_sha256"] or hashlib.sha256(packaging_bytes).hexdigest() != manifest["packaging_manifest_sha256"]:
                raise ValueError("Linux archive evidence hashes differ from release manifest")
            build = json.loads(build_bytes)
            package = json.loads(packaging_bytes)
            source_record = build.get("source", {})
            build_revision = build.get("source_sha", (source_record.get("commit") or source_record.get("git_revision")) if isinstance(source_record, dict) else None)
            if build_revision != manifest["source_sha"]:
                raise ValueError("Linux build manifest source differs from release manifest")
            if package.get("schema") != "nudox.linux-portable-package.v1":
                raise ValueError("Linux package manifest has an unsupported schema")
            release_tag = package.get("release_tag")
            if not isinstance(release_tag, str) or not re.fullmatch(r"checkpoint-[0-9]{8}-[a-f0-9]{10}-linux-x64", release_tag) or not release_tag.endswith("-" + manifest["source_sha"][:10] + "-linux-x64"):
                raise ValueError("Linux package has no source-bound immutable checkpoint tag")
    except (tarfile.TarError, OSError, json.JSONDecodeError) as error:
        raise ValueError(f"could not validate Linux archive: {error}") from error
    qa = read_json(directory / "native-qa-linux-x64.json")
    qa_fields = {"schema", "platform", "archive_sha256", "source_sha", "tested_by", "tested_at", "ubuntu_release", "glibc_version", "cases"}
    if set(qa) != qa_fields or qa["schema"] != 1 or qa["platform"] != "linux-x64":
        raise ValueError("invalid Linux native acceptance record")
    if qa["archive_sha256"] != manifest["sha256"] or qa["source_sha"] != manifest["source_sha"]:
        raise ValueError("Linux native QA does not attest this exact archive and source")
    for name in ("tested_by", "tested_at", "ubuntu_release", "glibc_version"):
        if not isinstance(qa[name], str) or not qa[name]:
            raise ValueError("Linux native QA requires operator, timestamp, Ubuntu release and glibc version")
    if set(qa["cases"]) != LINUX_QA_CASES or any(value is not True for value in qa["cases"].values()):
        raise ValueError("Linux native acceptance has missing or failed cases")
    installer = directory / "install-linux-x64.py"
    channel_installer = directory / "install-linux-x64-channel.py"
    if installer.is_symlink() or not installer.is_file() or channel_installer.is_symlink() or not channel_installer.is_file():
        raise ValueError("Linux candidate is missing its checkpoint or channel installer bootstrap")
    if source is not None:
        source = Path(source)
        revision = manifest["source_sha"]
        subprocess.run(["git", "-C", str(source), "merge-base", "--is-ancestor", revision, "HEAD"], check=True)
        def blob(path):
            return subprocess.check_output(["git", "-C", str(source), "show", f"{revision}:{path}"])
        if hashlib.sha256(blob("Cargo.lock")).hexdigest() != manifest["cargo_lock_sha256"]:
            raise ValueError("Linux candidate lock differs from selected canonical source")
        tree = subprocess.check_output(["git", "-C", str(source), "rev-parse", revision + "^{tree}"], text=True).strip()
        version = tomllib.loads(blob("Cargo.toml").decode())["workspace"]["package"]["version"]
        if tree != manifest["source_tree"] or version != manifest["version"]:
            raise ValueError("Linux candidate version/tree differs from selected canonical source")
        current = subprocess.check_output(["git", "-C", str(source), "rev-parse", "HEAD"], text=True).strip()
        if subprocess.check_output(["git", "-C", str(source), "status", "--porcelain"], text=True).strip():
            raise ValueError("packaging source checkout must be clean")
        expected_installer = subprocess.check_output(["git", "-C", str(source), "show", f"{current}:tools/package/install_linux.py"])
        if channel_installer.read_bytes() != expected_installer:
            raise ValueError("channel installer bootstrap differs from clean packaging source")
        pinned_installer = expected_installer.decode()
        pinned_installer = pinned_installer.replace('PINNED_RELEASE_TAG = ""', f'PINNED_RELEASE_TAG = "{release_tag}"', 1)
        pinned_installer = pinned_installer.replace('PINNED_MANIFEST_SHA256 = ""', f'PINNED_MANIFEST_SHA256 = "{sha256(directory / "release-manifest.json")}"', 1)
        if installer.read_bytes() != pinned_installer.encode():
            raise ValueError("checkpoint installer does not pin this tag and exact manifest bytes")
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

    def upload(self, release_id, path, name=None):
        path = Path(path)
        connection = http.client.HTTPSConnection("uploads.github.com", timeout=120)
        endpoint = f"/repos/{self.repo}/releases/{release_id}/assets?name=" + urllib.parse.quote(name or path.name)
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

    def asset_hash(self, asset):
        request = urllib.request.Request(f'https://api.github.com/repos/{self.repo}/releases/assets/{asset["id"]}',
            headers={"Authorization": "Bearer " + self.token, "Accept": "application/octet-stream"})
        try:
            response = urllib.request.build_opener(NoRedirect).open(request, timeout=120)
        except urllib.error.HTTPError as error:
            if error.code != 302:
                raise
            location = error.headers.get("Location", "")
            error.close()
            if urllib.parse.urlparse(location).scheme != "https":
                raise ValueError("GitHub returned a non-HTTPS asset location")
            # Never forward a private-repository token to the download CDN.
            response = urllib.request.urlopen(location, timeout=120)
        digest = hashlib.sha256()
        with response:
            for chunk in iter(lambda: response.read(1024 * 1024), b""):
                digest.update(chunk)
        return digest.hexdigest()

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

    def publish(self, directory, tag, stable):
        directory = Path(directory)
        manifest = validate(directory)
        if stable and tag != "v" + manifest["version"]:
            raise ValueError("stable tag does not match candidate version")
        if not stable and not re.fullmatch(re.escape("v" + manifest["version"]) + r"-rc\.[0-9]+", tag):
            if manifest["platform"] != "linux-x64":
                raise ValueError("candidate tag must match its immutable checkpoint or be v<version>-rc.<number>")
            with tarfile.open(directory / manifest["asset"], "r:gz") as bundle:
                package = json.load(bundle.extractfile("nudox-linux-x86_64/packaging-manifest.json"))
            if tag != package.get("release_tag"):
                raise ValueError("candidate tag must match its immutable checkpoint or be v<version>-rc.<number>")
        release = self.find_release(tag)
        if release is None:
            release = self.api("/releases", "POST", {"tag_name": tag, "name": "NuDox " + tag, "draft": True, "prerelease": not stable, "make_latest": "false", "body": f'NuDox distribution assets. Canonical Forgejo Backend source: {manifest["source_sha"]}. The GitHub tag is a distribution identifier.'})
        assets = {asset["name"]: asset for asset in release["assets"]}
        if not release["draft"] and bool(release.get("prerelease")) == stable:
            raise ValueError("published tag has the wrong stable/prerelease status; use a new version")
        if manifest["platform"] == "linux-x64":
            publish_files = (
                (manifest["asset"], manifest["asset"]),
                (manifest["asset"] + ".sha256", manifest["asset"] + ".sha256"),
                ("release-manifest-linux-x64.json", "release-manifest.json"),
                ("native-qa-linux-x64.json", "native-qa-linux-x64.json"),
                ("install-linux-x64.py", "install-linux-x64.py" if not stable else "install-linux-x64-channel.py"),
            )
        else:
            publish_files = tuple((name, name) for name in (ASSET, ASSET + ".sha256", "release-manifest.json", "native-qa.json"))
        for published_name, local_name in publish_files:
            path = directory / local_name
            expected = sha256(path)
            if published_name in assets:
                if self.asset_hash(assets[published_name]) != expected:
                    raise ValueError(f"refusing to overwrite immutable asset {published_name} in {tag}")
            elif release["draft"] or manifest["platform"] == "linux-x64":
                uploaded = self.upload(release["id"], path) if published_name == local_name else self.upload(release["id"], path, published_name)
                if self.asset_hash(uploaded) != expected:
                    raise ValueError("uploaded release bytes failed read-back verification")
            else:
                raise ValueError(f"published release is missing expected asset {published_name}; use a new version")
        if release["draft"]:
            self.api(f'/releases/{release["id"]}', "PATCH", {"draft": False, "prerelease": not stable, "make_latest": "false"})
        return manifest


def check_fast(revision):
    token = os.environ.get("FORGEJO_TOKEN", "")
    if not token:
        raise ValueError("fast-gate verification requires Forgejo read access")
    endpoint = f"https://dev.nudox.org/api/v1/repos/Nudox/Backend/statuses/{revision}?limit=100"
    request = urllib.request.Request(endpoint, headers={"Authorization": "token " + token})
    with urllib.request.urlopen(request, timeout=30) as response:
        statuses = json.load(response)
    matches = [entry for entry in statuses if entry["context"] == "concourse/backend-fast"]
    if not matches or max(matches, key=lambda entry: entry["id"])["status"] != "success":
        raise ValueError("selected source revision has no passing concourse/backend-fast status")


def promote(manifest, host, key, known_hosts):
    command = ["ssh", "-i", str(key), "-o", "BatchMode=yes", "-o", "IdentitiesOnly=yes", "-o", "StrictHostKeyChecking=yes", "-o", "UserKnownHostsFile=" + str(known_hosts), "-o", "ConnectTimeout=10", host]
    def request(data):
        result = subprocess.run(command, input=json.dumps(data), text=True, stdout=subprocess.PIPE, check=True)
        return json.loads(result.stdout)
    current = request({"action": "show"})
    if public_metadata() != current["catalog"]:
        raise ValueError("public auth release metadata does not match the active channel catalog; deploy routing first")
    channel = {name: manifest[name] for name in ("version", "source_sha", "asset", "sha256")}
    for optional in ("minimum_os", "minimum_glibc"):
        if optional in manifest:
            channel[optional] = manifest[optional]
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
        restored_channel = "Mac" if manifest["platform"] == "macos" else manifest["platform"]
        raise ValueError(f"public download verification failed; previous {restored_channel} channel restored") from error
    return updated["sha256"]


def public_metadata():
    with urllib.request.urlopen("https://api.nudox.org/v1/releases", timeout=30) as response:
        data = response.read(65537)
    if len(data) > 65536:
        raise ValueError("public release metadata exceeds size limit")
    return json.loads(data)


def verify_public_download(manifest, expected_catalog):
    if public_metadata() != expected_catalog:
        raise ValueError("public metadata did not refresh to promoted release")
    digest = hashlib.sha256()
    with urllib.request.urlopen(f"https://api.nudox.org/v1/downloads/{manifest['platform']}", timeout=120) as response:
        for chunk in iter(lambda: response.read(1024 * 1024), b""):
            digest.update(chunk)
    if digest.hexdigest() != manifest["sha256"]:
        raise ValueError("public Mac download differs from accepted release archive")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=("validate", "candidate", "publish"))
    parser.add_argument("--candidate", required=True, type=Path)
    parser.add_argument("--source", type=Path)
    parser.add_argument("--check-fast", action="store_true")
    parser.add_argument("--repo", default="nudoxorg/backend")
    parser.add_argument("--candidate-tag")
    parser.add_argument("--promotion-host")
    parser.add_argument("--promotion-key", type=Path)
    parser.add_argument("--known-hosts", type=Path)
    args = parser.parse_args()
    manifest = validate(args.candidate, args.source)
    if args.check_fast:
        check_fast(manifest["source_sha"])
    if args.action != "validate":
        github = GitHub(args.repo, os.environ.get("NUDOX_RELEASE_TOKEN", ""))
        if args.action == "candidate":
            if not args.candidate_tag:
                raise ValueError("candidate staging requires --candidate-tag")
            github.publish(args.candidate, args.candidate_tag, False)
        else:
            if not all((args.promotion_host, args.promotion_key, args.known_hosts)):
                raise ValueError("publishing requires channel promotion credentials and pinned SSH host key")
            metadata = public_metadata()
            if metadata.get("schema") != 1 or set(metadata.get("platforms", {})) != {"macos", "linux-x64", "linux-arm64", "windows"}:
                raise ValueError("platform-specific auth routing must be deployed before publishing")
            github.publish(args.candidate, "v" + manifest["version"], True)
            promote(manifest, args.promotion_host, args.promotion_key, args.known_hosts)
    print(json.dumps({"action": args.action, "version": manifest["version"], "source_sha": manifest["source_sha"], "sha256": manifest["sha256"]}))


if __name__ == "__main__":
    try:
        main()
    except (ValueError, OSError, subprocess.CalledProcessError) as error:
        if isinstance(error, urllib.error.HTTPError):
            message = f"release service returned HTTP {error.code}"
        else:
            message = str(error)
        print("release refused: " + message, file=sys.stderr)
        sys.exit(1)
