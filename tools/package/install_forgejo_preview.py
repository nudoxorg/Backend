#!/usr/bin/env python3
"""Install a diagnostic mirror using unchanged, digest-pinned platform libraries."""
import argparse
import datetime
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import platform
import re
import shlex
import subprocess
import sys
import tarfile
import tempfile
import urllib.error
import urllib.parse
import urllib.request

ORIGIN = "https://dev.nudox.org"
RELEASES = "/git/philocalyst/Backend/releases/download/"
BOOTSTRAP_TAG = "nudox-diagnostic-installer-20261009"
CHANNEL = ORIGIN + RELEASES + BOOTSTRAP_TAG + "/preview-channel.json"
MAX_SCRIPT = 1024 * 1024
MAX_METADATA = 65536
MAX_ARCHIVE = 2 * 1024**3
# These are the final reviewed libraries, including only the original Linux pins.
LIBRARIES = {
    "macos-arm64": ("82f71f98a05a2b13e05afd7185e35c5f8d252b9973ab4ce6d32ab22688794f25", 22094, "install-macos-arm64"),
    "linux-x64": ("8baa4a60be98ee4e41e26c3b1cf0d4800e8ec4b8c80758128f38c19a06c799c9", 46973, "install-linux-x64"),
}


class InstallError(Exception):
    pass


def release_url(tag, asset):
    if not isinstance(tag, str) or not re.fullmatch(r"[A-Za-z0-9.-]{1,100}", tag):
        raise InstallError("unsafe release tag")
    if not isinstance(asset, str) or not re.fullmatch(r"[A-Za-z0-9._-]{1,150}", asset):
        raise InstallError("unsafe release asset")
    return ORIGIN + RELEASES + tag + "/" + asset


def check_url(url, initial, *, redirect=False):
    if not isinstance(url, str):
        raise InstallError("invalid mirror URL")
    parsed = urllib.parse.urlsplit(url)
    if (parsed.scheme != "https" or parsed.netloc != "dev.nudox.org"
            or parsed.username or parsed.password or parsed.query or parsed.fragment):
        raise InstallError("mirror transport must remain on the exact trusted HTTPS origin")
    attachment = re.fullmatch(r"/git/attachments/[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}", parsed.path)
    if url != initial and not (redirect and attachment):
        raise InstallError("mirror URL does not match its repository, tag and asset role")


class HTTPSRedirect(urllib.request.HTTPRedirectHandler):
    def __init__(self, initial):
        super().__init__()
        self.initial = initial

    def redirect_request(self, req, fp, code, msg, headers, url):
        check_url(url, self.initial, redirect=True)
        return super().redirect_request(req, fp, code, msg, headers, url)


def download(url, destination, limit, *, expected_size=None, expected_sha256=None):
    """One bounded transport for channel, scripts, manifest and streamed archive."""
    check_url(url, url)
    if not url.startswith(ORIGIN + RELEASES):
        raise InstallError("initial mirror URL must name a trusted repository release asset")
    digest = hashlib.sha256()
    count = 0
    request = urllib.request.Request(url, headers={"User-Agent": "nudox-diagnostic-mirror/1"})
    opener = urllib.request.build_opener(HTTPSRedirect(url))
    descriptor = os.open(destination, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    try:
        with os.fdopen(descriptor, "wb") as output, opener.open(request, timeout=120) as response:
            check_url(response.geturl(), url, redirect=True)
            while chunk := response.read(65536):
                count += len(chunk)
                if count > limit or (expected_size is not None and count > expected_size):
                    raise InstallError("mirror download exceeds its byte budget")
                output.write(chunk)
                digest.update(chunk)
        if expected_size is not None and count != expected_size:
            raise InstallError("mirror artifact size mismatch")
        if expected_sha256 is not None and digest.hexdigest() != expected_sha256:
            raise InstallError("mirror artifact SHA-256 mismatch")
    except Exception:
        Path(destination).unlink(missing_ok=True)
        raise


def artifact(value, tag, asset, limit):
    if not isinstance(value, dict) or set(value) != {"url", "sha256", "bytes"}:
        raise InstallError("invalid mirror artifact")
    digest = value["sha256"]
    if not isinstance(digest, str) or not re.fullmatch(r"[0-9a-f]{64}", digest):
        raise InstallError("invalid mirror artifact SHA-256")
    if type(value["bytes"]) is not int or not 0 < value["bytes"] <= limit:
        raise InstallError("invalid mirror artifact byte count")
    check_url(value["url"], release_url(tag, asset))
    return value


def verified(value, destination, limit):
    download(value["url"], destination, limit,
             expected_size=value["bytes"], expected_sha256=value["sha256"])
    return destination


def platform_key(system=None, machine=None):
    system = system or platform.system()
    machine = (machine or platform.machine()).lower()
    if system == "Darwin" and machine in {"arm64", "aarch64"}:
        return "macos-arm64"
    if system == "Linux" and machine in {"x86_64", "amd64"}:
        return "linux-x64"
    raise InstallError("diagnostic mirror supports macOS arm64 and Linux x64 only")


def validate_channel(channel):
    if (not isinstance(channel, dict) or set(channel) != {"schema", "production_ready", "bootstrap", "platforms"}
            or channel["schema"] != "nudox.diagnostic-forgejo-channel.v1" or channel["production_ready"] is not False):
        raise InstallError("explicit diagnostic mirror channel required")
    bootstrap = channel["bootstrap"]
    if not isinstance(bootstrap, dict) or not isinstance(bootstrap.get("sha256"), str):
        raise InstallError("invalid mirror bootstrap")
    artifact(bootstrap, BOOTSTRAP_TAG, "install-forgejo-bootstrap-" + bootstrap["sha256"][:16] + ".py", MAX_SCRIPT)
    entries = channel["platforms"]
    if not isinstance(entries, dict) or set(entries) != set(LIBRARIES):
        raise InstallError("invalid mirror platforms")
    for key, entry in entries.items():
        fields = {"status", "source", "tag", "known_failures", "installer", "archive"}
        if key == "linux-x64":
            fields.add("manifest")
        if (not isinstance(entry, dict) or set(entry) != fields or entry["status"] != "diagnostic-preview"
                or not isinstance(entry["known_failures"], list) or not entry["known_failures"]
                or any(not isinstance(x, str) or not x.strip() for x in entry["known_failures"])):
            raise InstallError("diagnostic status and known failures required")
        source, tag = entry["source"], entry["tag"]
        if not isinstance(source, str) or not re.fullmatch(r"[0-9a-f]{40}", source):
            raise InstallError("invalid mirror source")
        if not isinstance(tag, str) or not re.fullmatch(r"checkpoint-[0-9]{8}-" + source[:10] + "-" + key, tag):
            raise InstallError("mirror tag/source/platform mismatch")
        try:
            datetime.datetime.strptime(tag.split("-")[1], "%Y%m%d")
        except ValueError as error:
            raise InstallError("invalid checkpoint calendar date") from error
        library_sha, library_size, library_name = LIBRARIES[key]
        installer = artifact(entry["installer"], tag, library_name + "-" + library_sha[:16] + ".py", MAX_SCRIPT)
        if installer["sha256"] != library_sha or installer["bytes"] != library_size:
            raise InstallError("mirror platform library differs from the reviewed bytes")
        archive_name = ("nudox-macos-arm64-" if key == "macos-arm64" else "nudox-linux-x86_64-") + source[:10] + ".tar.gz"
        artifact(entry["archive"], tag, archive_name, MAX_ARCHIVE)
        if key == "linux-x64":
            artifact(entry["manifest"], tag, "release-manifest-linux-x64.json", MAX_METADATA)
    return channel


def load_library(path, key):
    body = path.read_bytes()
    if (hashlib.sha256(body).hexdigest(), len(body)) != LIBRARIES[key][:2]:
        raise InstallError("platform library changed before import")
    spec = importlib.util.spec_from_file_location("nudox_verified_" + key.replace("-", "_"), path)
    module = importlib.util.module_from_spec(spec)
    exec(compile(body, str(path), "exec"), module.__dict__)
    if module.PLATFORM != key:
        raise InstallError("verified library platform mismatch")
    return module


def install_verified(entry, key, library_path, archive, manifest_path, prefix, allow_downgrade):
    """Keep mirror URLs outside the archived platform provenance and install API."""
    module = load_library(library_path, key)
    source, tag = entry["source"], entry["tag"]
    if key == "linux-x64":
        if (module.PINNED_RELEASE_TAG != tag
                or module.PINNED_MANIFEST_SHA256 != entry["manifest"]["sha256"]):
            raise InstallError("Linux mirror differs from the original pinned checkpoint")
        try:
            product, manifest = module.parse_pinned_manifest(tag, manifest_path.read_bytes())
        except module.InstallError as error:
            raise InstallError(str(error)) from error
        if (product["source_sha"] != source or manifest["sha256"] != entry["archive"]["sha256"]
                or manifest["size_bytes"] != entry["archive"]["bytes"]
                or release_url(tag, product["asset"]) != entry["archive"]["url"]):
            raise InstallError("Linux mirror metadata disagrees with original release provenance")
    else:
        product = {"version": "0.0.0", "tag": tag, "source_sha": source,
                   "asset": "nudox-macos-arm64-" + source[:10] + ".tar.gz"}
        manifest = {"sha256": entry["archive"]["sha256"]}
    # Normal unchanged install(): full lease, ownership, archive/package checks,
    # existing-install validation, startup probes and atomic pointer publication.
    try:
        module.install(prefix, product, manifest, archive, allow_downgrade=allow_downgrade)
    except module.InstallError as error:
        raise InstallError(str(error)) from error


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--prefix", type=Path)
    parser.add_argument("--allow-downgrade", action="store_true")
    parser.add_argument("--channel-url", default=CHANNEL, help=argparse.SUPPRESS)
    args = parser.parse_args(argv)
    check_url(args.channel_url, CHANNEL)
    key = platform_key()
    prefix = (args.prefix or Path.home() / ".local").expanduser().resolve()
    with tempfile.TemporaryDirectory(prefix="nudox-diagnostic-mirror-") as temporary:
        private = Path(temporary)
        if private.stat().st_mode & 0o077 or private.stat().st_uid != os.geteuid():
            raise InstallError("download workspace is not private and owned")
        channel_path = private / "channel.json"
        download(args.channel_url, channel_path, MAX_METADATA)
        channel = validate_channel(json.loads(channel_path.read_bytes()))
        script_name = globals().get("__file__", "")
        script = Path(script_name)
        current_sha = hashlib.sha256(script.read_bytes()).hexdigest() if script_name not in {"", "<stdin>", "-"} and script.is_file() else None
        if current_sha != channel["bootstrap"]["sha256"]:
            updated = verified(channel["bootstrap"], private / "bootstrap.py", MAX_SCRIPT)
            command = [sys.executable, str(updated), "--channel-url", args.channel_url, "--prefix", str(prefix)]
            if args.allow_downgrade:
                command.append("--allow-downgrade")
            return subprocess.call(command)
        entry = channel["platforms"][key]
        print("Diagnostic mirror; whole-project acceptance is not established.", flush=True)
        for failure in entry["known_failures"]:
            print("Known failure: " + failure, flush=True)
        library = verified(entry["installer"], private / "platform.py", MAX_SCRIPT)
        manifest = verified(entry["manifest"], private / "manifest.json", MAX_METADATA) if key == "linux-x64" else None
        archive = verified(entry["archive"], private / "archive.tar.gz", MAX_ARCHIVE)
        install_verified(entry, key, library, archive, manifest, prefix, args.allow_downgrade)
    print("Installed diagnostic CLI, MCP and local daemon in " + str(prefix) + ".")
    print("MCP executable: " + str(prefix / "bin/backend-mcp"))
    if str(prefix / "bin") not in os.environ.get("PATH", "").split(os.pathsep):
        print("Add to your shell profile: export PATH=" + shlex.quote(str(prefix / "bin")) + ':"$PATH"')
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (InstallError, OSError, ValueError, urllib.error.URLError,
            tarfile.TarError, subprocess.TimeoutExpired) as error:
        print("nudox diagnostic mirror: " + str(error), file=sys.stderr)
        raise SystemExit(1)
