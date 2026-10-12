#!/usr/bin/env python3
"""Install immutable macOS arm64 diagnostic CLI preview; no stable-release claim."""
from __future__ import annotations
# Helpers reused unchanged from pinned install-linux-x64.py
# SHA 61837bb39ca18cd0a39e0f63c05f80bc35a34a798aa686f50af7cb85a46c6a90.
import argparse, datetime, fcntl, hashlib, json, os, re, shutil, stat, subprocess, sys, tarfile, tempfile, time
from pathlib import Path, PurePosixPath
from contextlib import contextmanager
PLATFORM="macos-arm64"
import urllib.request, urllib.parse, urllib.error
MAX_ARCHIVE_BYTES=2*1024**3
MAX_MEMBER_BYTES=1024**3
MAX_PACKAGE_METADATA_BYTES=16*1024**2
DOWNLOAD_TIMEOUT_SECONDS=20
DOWNLOAD_PROGRESS_INTERVAL_SECONDS=5
DOWNLOAD_PROGRESS_BYTES=8*1024**2
SDK_SCHEMA="nudox.mac-sdk-cli-package.v1"
SDK_BIN="Nudox.app/Contents/MacOS"
SDK_MANIFEST="Nudox.app/Contents/Resources/build-manifest.json"
REQUIRED_BINARIES=('backend-cli','backend-mcp','backend-locald')
class InstallError(Exception): pass
class HTTPSOnlyRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        if urllib.parse.urlsplit(newurl).scheme != "https":
            raise InstallError("release download redirected away from HTTPS")
        return super().redirect_request(req, fp, code, msg, headers, newurl)

def fetch_bytes(url: str, limit: int, label: str) -> bytes:
    if urllib.parse.urlsplit(url).scheme != "https":
        raise InstallError(f"refusing non-HTTPS {label} URL")
    request = urllib.request.Request(url, headers={"User-Agent": "nudox-installer/1", "Accept": "application/json, application/octet-stream"})
    try:
        with HTTPS.open(request, timeout=60) as response:
            body = response.read(limit + 1)
    except urllib.error.HTTPError as error:
        raise InstallError(f"{label} request failed with HTTP {error.code}") from error
    except (urllib.error.URLError, TimeoutError, OSError) as error:
        raise InstallError(f"could not fetch {label}: {error}") from error
    if len(body) > limit:
        raise InstallError(f"{label} exceeds the {limit}-byte limit")
    return body

def download_archive(url: str, expected_sha256: str, expected_size: int, destination: Path) -> None:
    if urllib.parse.urlsplit(url).hostname != "github.com" or urllib.parse.urlsplit(url).scheme != "https":
        raise InstallError("release archive URL is outside the expected HTTPS GitHub release host")
    request = urllib.request.Request(url, headers={"User-Agent": "nudox-installer/1", "Accept": "application/octet-stream"})
    digest = hashlib.sha256()
    count = 0
    last_progress_at = time.monotonic()
    last_progress_count = 0
    print(f"Downloading pinned macOS arm64 release archive: 0/{expected_size:,} bytes (0%).", file=sys.stderr, flush=True)
    try:
        with HTTPS.open(request, timeout=DOWNLOAD_TIMEOUT_SECONDS) as response, destination.open("xb") as output:
            final_host = urllib.parse.urlsplit(response.geturl()).hostname or ""
            if final_host != "github.com" and not final_host.endswith(".githubusercontent.com"):
                raise InstallError("GitHub release download redirected to an unexpected host")
            while True:
                chunk = response.read(1024 * 1024)
                if not chunk:
                    break
                count += len(chunk)
                if count > MAX_ARCHIVE_BYTES:
                    raise InstallError("release archive exceeds the 2 GiB safety limit")
                output.write(chunk)
                digest.update(chunk)
                now = time.monotonic()
                if (count == expected_size
                        or count - last_progress_count >= DOWNLOAD_PROGRESS_BYTES
                        or now - last_progress_at >= DOWNLOAD_PROGRESS_INTERVAL_SECONDS):
                    percent = min(100, count * 100 // max(expected_size, 1))
                    print(f"Downloading pinned macOS arm64 release archive: {count:,}/{expected_size:,} bytes ({percent}%).", file=sys.stderr, flush=True)
                    last_progress_at = now
                    last_progress_count = count
    except urllib.error.HTTPError as error:
        raise InstallError(f"release archive request failed with HTTP {error.code}") from error
    except (urllib.error.URLError, TimeoutError, OSError) as error:
        raise InstallError(f"could not download the release archive: {error}") from error
    print(f"Downloaded {count:,} bytes; verifying the pinned SHA-256.", file=sys.stderr, flush=True)
    if count != expected_size:
        raise InstallError(f"release archive size mismatch (expected {expected_size}, received {count})")
    if digest.hexdigest() != expected_sha256:
        raise InstallError("release archive SHA-256 mismatch; nothing was installed")

def _safe_member(member: tarfile.TarInfo, topdir: str) -> str | None:
    name = member.name
    if not name or "\\" in name or name.startswith("/"):
        raise InstallError(f"archive has an unsafe path: {name!r}")
    stripped = name[:-1] if name.endswith("/") and member.isdir() else name
    raw_parts = stripped.split("/")
    if any(part in {"", ".", ".."} for part in raw_parts):
        raise InstallError(f"archive has an unsafe path: {name!r}")
    path = PurePosixPath(stripped)
    if path.parts[0] != topdir:
        raise InstallError(f"archive member is outside expected directory {topdir!r}: {name!r}")
    if member.issym() or member.islnk() or not (member.isdir() or member.isreg()):
        raise InstallError(f"archive contains a link or special file: {name!r}")
    if member.size < 0 or member.size > MAX_MEMBER_BYTES:
        raise InstallError(f"archive member exceeds the per-file safety limit: {name!r}")
    relative = PurePosixPath(*path.parts[1:])
    if not relative.parts:
        if member.isdir():
            return None
        raise InstallError("archive root must be a directory")
    return relative.as_posix()

def _file_sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()

def _verified_package_file(root: Path, relative: str, *, read_bytes: bool = False, max_bytes: int | None = None):
    """Hash one installed package file without following links or accepting shared files."""
    if (not isinstance(relative, str) or not relative or relative.startswith("/")
            or "\\" in relative or any(part in {"", ".", ".."} for part in relative.split("/"))):
        raise InstallError("invalid package file path")
    root = Path(root)
    try:
        root_stat = root.lstat()
    except OSError as error:
        raise InstallError("package root is unavailable") from error
    if (not stat.S_ISDIR(root_stat.st_mode) or root_stat.st_uid != os.geteuid()
            or stat.S_IMODE(root_stat.st_mode) & 0o022):
        raise InstallError("package root is not a private owned directory")
    current = root
    parts = relative.split("/")
    try:
        for part in parts[:-1]:
            current = current / part
            metadata = current.lstat()
            if (not stat.S_ISDIR(metadata.st_mode) or metadata.st_uid != os.geteuid()
                    or stat.S_IMODE(metadata.st_mode) & 0o022):
                raise InstallError("package path contains an unsafe directory")
        target = current / parts[-1]
        descriptor = os.open(target, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    except InstallError:
        raise
    except OSError as error:
        raise InstallError("package file is missing or linked") from error
    try:
        before = os.fstat(descriptor)
        if (not stat.S_ISREG(before.st_mode) or before.st_uid != os.geteuid()
                or before.st_nlink != 1 or stat.S_IMODE(before.st_mode) & 0o022):
            raise InstallError("package file is not a private owned single-link regular file")
        if max_bytes is not None and before.st_size > max_bytes:
            raise InstallError("package metadata exceeds its size limit")
        digest = hashlib.sha256()
        contents = bytearray() if read_bytes else None
        count = 0
        while True:
            chunk = os.read(descriptor, 1024 * 1024)
            if not chunk:
                break
            count += len(chunk)
            if max_bytes is not None and count > max_bytes:
                raise InstallError("package metadata exceeds its size limit")
            digest.update(chunk)
            if contents is not None:
                contents.extend(chunk)
        after = os.fstat(descriptor)
        try:
            path_after = target.lstat()
        except OSError as error:
            raise InstallError("package file changed while verifying") from error
        if (count != before.st_size or _file_identity(before) != _file_identity(after)
                or _file_identity(after) != _file_identity(path_after)):
            raise InstallError("package file changed while verifying")
        return (bytes(contents) if contents is not None else None), digest.hexdigest()
    finally:
        os.close(descriptor)

def _version_root(prefix: Path) -> Path:
    return prefix / "lib" / "nudox" / "versions"

def _managed_link(path: Path, managed_root: Path) -> bool:
    if not path.is_symlink():
        return False
    try:
        resolved = path.resolve(strict=False)
        resolved.relative_to(managed_root.resolve(strict=False))
        return True
    except (OSError, ValueError):
        return False

def _replace_symlink(link: Path, target: str) -> None:
    temp = link.with_name(link.name + ".nudox-new-" + str(os.getpid()) + "-" + str(time.time_ns()))
    try:
        os.symlink(target, temp)
        os.replace(temp, link)
    finally:
        try:
            temp.unlink()
        except FileNotFoundError:
            pass

def verify_existing_install(final: Path, expected_marker: dict, entry: dict, manifest: dict) -> str:
    """Reuse a prior release only after rechecking the files on disk."""
    try:
        marker_bytes, _ = _verified_package_file(final, ".installed-release.json", read_bytes=True, max_bytes=16 * 1024)
        existing_marker = json.loads(marker_bytes)
    except (InstallError, OSError, UnicodeDecodeError, json.JSONDecodeError) as error:
        raise InstallError(f"refusing to reuse existing unverified install at {final}") from error
    if existing_marker != expected_marker:
        raise InstallError(f"release {entry['tag']} is already installed with different bytes")
    try:
        layout = verify_package(final, entry, manifest)
        if layout == SDK_BIN:
            sdk_command_link(final)
        return layout
    except (InstallError, OSError, KeyError, TypeError, ValueError) as error:
        raise InstallError(f"existing release at {final} failed its integrity/startup recheck; refusing to reuse it: {error}") from error

def command_links(managed_root: Path) -> dict[str, str]:
    binaries = {
        "nudox": "backend-cli",
        "nudox-mcp": "backend-mcp",
        "nudox-locald": "backend-locald",
        # Keep the executable names used by current MCP setup guidance.
        "backend-cli": "backend-cli",
        "backend-mcp": "backend-mcp",
        "backend-locald": "backend-locald",
    }
    return {name: str(managed_root / "current" / "bin" / binary) for name, binary in binaries.items()}

def _validate_install_directory(path):
    metadata = path.lstat()
    if (not stat.S_ISDIR(metadata.st_mode) or metadata.st_uid != os.geteuid()
            or stat.S_IMODE(metadata.st_mode) & 0o022):
        raise InstallError("managed install directory is not a real private owned directory: " + str(path))


def _file_identity(value):
    return (value.st_dev, value.st_ino, value.st_size, value.st_uid, value.st_nlink,
            value.st_mode, value.st_mtime_ns, value.st_ctime_ns)


def _validate_marker(marker):
    fields = {"version", "tag", "source_sha", "asset", "sha256"}
    if not isinstance(marker, dict) or set(marker) != fields:
        raise InstallError("release marker fields are invalid")
    if not isinstance(marker["version"], str) or not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", marker["version"]):
        raise InstallError("release marker version is invalid")
    if not isinstance(marker["source_sha"], str) or not re.fullmatch(r"[a-f0-9]{40}", marker["source_sha"]):
        raise InstallError("release marker source is invalid")
    if not isinstance(marker["sha256"], str) or not re.fullmatch(r"[a-f0-9]{64}", marker["sha256"]):
        raise InstallError("release marker archive hash is invalid")
    asset_prefix = "nudox-macos-arm64-" if PLATFORM == "macos-arm64" else "nudox-linux-x86_64-"
    asset = re.fullmatch(re.escape(asset_prefix) + r"([a-f0-9]{10,40})\.tar\.gz", marker["asset"] if isinstance(marker["asset"], str) else "")
    if asset is None or not marker["source_sha"].startswith(asset[1]):
        raise InstallError("release marker archive does not match its platform/source")
    tag = marker["tag"]
    if not isinstance(tag, str):
        raise InstallError("release marker tag is invalid")
    checkpoint = re.fullmatch(r"checkpoint-([0-9]{8})-([a-f0-9]{10})-" + re.escape(PLATFORM), tag)
    if checkpoint:
        if checkpoint[2] != marker["source_sha"][:10]:
            raise InstallError("release marker checkpoint does not match its source")
        try:
            return datetime.date(int(checkpoint[1][:4]), int(checkpoint[1][4:6]), int(checkpoint[1][6:8]))
        except ValueError as error:
            raise InstallError("release marker checkpoint date is invalid") from error
    if PLATFORM == "linux-x64" and tag == "v" + marker["version"]:
        return None
    raise InstallError("release marker has an unknown checkpoint identity")


def _read_active_marker(current):
    try:
        release_root = current.resolve(strict=True)
        raw, _ = _verified_package_file(release_root, ".installed-release.json", read_bytes=True, max_bytes=16 * 1024)
        return json.loads(raw)
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as error:
        raise InstallError("cannot verify active checkpoint before update") from error


def check_checkpoint_update(current, incoming, allow_downgrade=False):
    """Admit the selected checkpoint; dates order days, not opaque revisions.

    A digest-pinned installer or promoted channel selects the exact incoming
    revision. Different revisions on the same day are ordinary replacements,
    not evidence of a downgrade. Identity and byte checks remain mandatory.
    """
    incoming_date = _validate_marker(incoming)
    if not current.exists():
        if current.is_symlink():
            raise InstallError("active install pointer is dangling")
        return
    installed = _read_active_marker(current)
    active_date = _validate_marker(installed)
    if incoming["tag"] == installed["tag"]:
        if incoming != installed:
            raise InstallError("active release tag is already installed with different bytes")
        return
    if allow_downgrade:
        return
    if incoming_date is None or active_date is None:
        if incoming_date is None and active_date is None and tuple(map(int, incoming["version"].split("."))) > tuple(map(int, installed["version"].split("."))):
            return
        raise InstallError("refusing unordered checkpoint/stable replacement; use --allow-downgrade explicitly")
    if incoming_date < active_date:
        raise InstallError(f"refusing older checkpoint {incoming['tag']}; active is {installed['tag']}. Use --allow-downgrade to replace it explicitly")


@contextmanager
def _install_lease(managed_root, *, create=True):
    """Hold one owned inode from checkpoint admission through publication."""
    _validate_install_directory(managed_root)
    path = managed_root / ".install.lock"
    flags = os.O_RDWR | os.O_NOFOLLOW | os.O_NONBLOCK
    if create:
        flags |= os.O_CREAT
    descriptor = os.open(path, flags, 0o600)
    try:
        before = os.fstat(descriptor)
        if (not stat.S_ISREG(before.st_mode) or before.st_size != 0
                or before.st_uid != os.geteuid() or before.st_nlink != 1
                or stat.S_IMODE(before.st_mode) != 0o600):
            raise InstallError("install lease is not a private owned single-link regular file")
        fcntl.flock(descriptor, fcntl.LOCK_EX)
        if _file_identity(before) != _file_identity(os.fstat(descriptor)) or _file_identity(before) != _file_identity(path.lstat()):
            raise InstallError("install lease identity changed while waiting")
        yield
    finally:
        fcntl.flock(descriptor, fcntl.LOCK_UN)
        os.close(descriptor)


def _lstat_or_none(path):
    try:
        return path.lstat()
    except FileNotFoundError:
        return None


def _check_command_aliases(prefix, managed_root, *, create_bin):
    bin_dir = prefix / "bin"
    if bin_dir.is_symlink():
        raise InstallError(f"command install directory must not be a symlink: {bin_dir}")
    if _lstat_or_none(bin_dir) is None:
        if not create_bin:
            return None
        bin_dir.mkdir(mode=0o700, exist_ok=True)
    _validate_install_directory(bin_dir)
    links = command_links(managed_root)
    for name, target in links.items():
        link = bin_dir / name
        if link.exists() and not link.is_symlink():
            raise InstallError(f"refusing to replace existing non-symlink command: {link}")
        if link.is_symlink() and os.readlink(link) != target and not _managed_link(link, managed_root):
            raise InstallError(f"refusing to replace command symlink outside the managed NuDox install: {link}")
    current = managed_root / "current"
    if current.exists() and not current.is_symlink():
        raise InstallError(f"refusing to replace non-symlink active install pointer: {current}")
    if current.is_symlink() and not _managed_link(current, managed_root):
        raise InstallError(f"refusing to replace active install pointer outside the managed NuDox install: {current}")
    return bin_dir, links, current


def _publish_command_aliases(bin_dir, links, current, managed_root, final):
    for name, target in links.items():
        link = bin_dir / name
        if not link.is_symlink() or os.readlink(link) != target:
            _replace_symlink(link, target)
    current_target = os.path.relpath(final, managed_root)
    if not current.is_symlink() or os.readlink(current) != current_target:
        _replace_symlink(current, current_target)


def reuse_existing_install(prefix: Path, entry: dict, manifest: dict, *, allow_downgrade: bool = False) -> bool:
    """Reuse a fully verified selected version before fetching its archive again."""
    prefix = Path(prefix)
    prefix_stat = _lstat_or_none(prefix)
    if prefix_stat is None:
        return False
    if stat.S_ISLNK(prefix_stat.st_mode):
        raise InstallError(f"managed install prefix must not be a symlink: {prefix}")
    _validate_install_directory(prefix)
    lib_dir = prefix / "lib"
    lib_stat = _lstat_or_none(lib_dir)
    if lib_stat is None:
        return False
    if stat.S_ISLNK(lib_stat.st_mode):
        raise InstallError(f"managed install parent must not be a symlink: {lib_dir}")
    _validate_install_directory(lib_dir)
    managed_root = lib_dir / "nudox"
    root_stat = _lstat_or_none(managed_root)
    if root_stat is None:
        return False
    if stat.S_ISLNK(root_stat.st_mode):
        raise InstallError(f"managed install root must not be a symlink: {managed_root}")
    _validate_install_directory(managed_root)
    try:
        with _install_lease(managed_root, create=False):
            return _reuse_existing_install_locked(prefix, entry, manifest, allow_downgrade)
    except FileNotFoundError:
        return False


def _reuse_existing_install_locked(prefix, entry, manifest, allow_downgrade):
    managed_root = prefix / "lib" / "nudox"
    version_root = _version_root(prefix)
    version_stat = _lstat_or_none(version_root)
    if version_stat is None:
        return False
    if stat.S_ISLNK(version_stat.st_mode) or not stat.S_ISDIR(version_stat.st_mode):
        raise InstallError("managed install versions path must be a real directory")
    _validate_install_directory(version_root)
    install_id = entry.get("tag")
    if not isinstance(install_id, str) or not re.fullmatch(r"[A-Za-z0-9.-]{1,100}", install_id):
        raise InstallError("release has an unsafe install identity")
    final = version_root / install_id
    marker = {"version": entry["version"], "tag": entry["tag"], "source_sha": entry["source_sha"], "asset": entry["asset"], "sha256": manifest["sha256"]}
    current = managed_root / "current"
    if current.exists() and not current.is_symlink():
        raise InstallError(f"refusing to replace non-symlink active install pointer: {current}")
    if current.is_symlink() and not _managed_link(current, managed_root):
        raise InstallError(f"refusing to replace active install pointer outside the managed NuDox install: {current}")
    check_checkpoint_update(current, marker, allow_downgrade)
    final_stat = _lstat_or_none(final)
    if final_stat is None:
        return False
    if stat.S_ISLNK(final_stat.st_mode) or not stat.S_ISDIR(final_stat.st_mode):
        raise InstallError(f"version path exists and is not an install directory: {final}")
    binary_directory = verify_existing_install(final, marker, entry, manifest)
    aliases = _check_command_aliases(prefix, managed_root, create_bin=True)
    bin_dir, links, current = aliases
    _publish_command_aliases(bin_dir, links, current, managed_root, final)
    return True


def install(prefix: Path, entry: dict, manifest: dict, archive: Path, *, allow_downgrade: bool = False) -> None:
    os.umask(0o077)
    prefix.mkdir(mode=0o700, parents=True, exist_ok=True)
    _validate_install_directory(prefix)
    lib_dir = prefix / "lib"
    if lib_dir.is_symlink():
        raise InstallError(f"managed install parent must not be a symlink: {lib_dir}")
    lib_dir.mkdir(mode=0o700, exist_ok=True)
    _validate_install_directory(lib_dir)
    if not lib_dir.is_dir():
        raise InstallError(f"managed install parent is not a directory: {lib_dir}")
    managed_root = prefix / "lib" / "nudox"
    if managed_root.is_symlink():
        raise InstallError(f"managed install root must not be a symlink: {managed_root}")
    managed_root.mkdir(mode=0o700, exist_ok=True)
    _validate_install_directory(managed_root)
    if not managed_root.is_dir():
        raise InstallError("managed install root is not a directory")
    with _install_lease(managed_root):
        _install_locked(prefix, entry, manifest, archive, allow_downgrade=allow_downgrade)


def _install_locked(prefix, entry, manifest, archive, *, allow_downgrade=False):
    managed_root = prefix / "lib" / "nudox"
    version_root = _version_root(prefix)
    version_root.mkdir(mode=0o700, exist_ok=True)
    if version_root.is_symlink() or not version_root.is_dir():
        raise InstallError("managed install versions path must be a real directory")
    install_id = entry.get("tag")
    if not isinstance(install_id, str) or not re.fullmatch(r"[A-Za-z0-9.-]{1,100}", install_id):
        raise InstallError("release has an unsafe install identity")
    final = version_root / install_id
    bin_dir, links, current = _check_command_aliases(prefix, managed_root, create_bin=True)
    current = managed_root / "current"
    marker = {"version": entry["version"], "tag": entry["tag"], "source_sha": entry["source_sha"], "asset": entry["asset"], "sha256": manifest["sha256"]}
    check_checkpoint_update(current, marker, allow_downgrade)
    stage = Path(tempfile.mkdtemp(prefix=".staging-", dir=version_root))
    try:
        safe_extract(archive, stage)
        binary_directory = verify_package(stage, entry, manifest)
        if binary_directory == SDK_BIN:
            sdk_command_link(stage, create=True)
        (stage / ".installed-release.json").write_text(json.dumps(marker, sort_keys=True) + "\n")
        os.chmod(stage, 0o755)
        if final.exists() or final.is_symlink():
            if final.is_symlink() or not final.is_dir():
                raise InstallError(f"version path already exists and is not an install directory: {final}")
            binary_directory = verify_existing_install(final, marker, entry, manifest)
            shutil.rmtree(stage)
        else:
            os.replace(stage, final)
    except Exception:
        shutil.rmtree(stage, ignore_errors=True)
        raise

    bin_dir, links, current = _check_command_aliases(prefix, managed_root, create_bin=True)
    _publish_command_aliases(bin_dir, links, current, managed_root, final)

def shlex_quote(value: str) -> str:
    import shlex
    return shlex.quote(value)
HTTPS=urllib.request.build_opener(HTTPSOnlyRedirect())

def safe_extract(archive,destination,*,archive_file=None):
    with tarfile.open(name=archive if archive_file is None else None, fileobj=archive_file, mode='r:gz') as bundle:
        members=bundle.getmembers()
        if not members or len(members)>65536: raise InstallError('invalid member count')
        seen=set(); parsed=[]; total=0
        for member in members:
            relative=_safe_member(member,'nudox-macos-arm64')
            if relative in seen: raise InstallError('duplicate archive member')
            seen.add(relative); total+=member.size
            if total>MAX_ARCHIVE_BYTES: raise InstallError('archive expands beyond limit')
            parsed.append((member,relative))
        metadata = next((m for m,p in parsed if p == 'manifest.json' and m.isreg()), None)
        if metadata is None or metadata.size > MAX_PACKAGE_METADATA_BYTES:
            raise InstallError('missing or oversized package manifest')
        with bundle.extractfile(metadata) as stream:
            try: package = json.loads(stream.read(MAX_PACKAGE_METADATA_BYTES + 1))
            except (UnicodeDecodeError, json.JSONDecodeError) as error:
                raise InstallError('package manifest is not valid JSON') from error
        sdk = isinstance(package, dict) and package.get('schema') == SDK_SCHEMA
        modes = sdk_inventory(package) if sdk else None
        required = ({SDK_BIN+'/'+n for n in REQUIRED_BINARIES}|{SDK_MANIFEST, 'manifest.json'} if sdk
                    else {'bin/'+n for n in REQUIRED_BINARIES}|{'manifest.json','native-build-manifest.json','README.txt'})
        if sdk and {p for m,p in parsed if m.isreg()} != set(modes)|{'manifest.json'}:
            raise InstallError('SDK archive differs from its complete file inventory')
        if sdk:
            expected_dirs = {parent.as_posix() for name in modes for parent in PurePosixPath(name).parents if parent.as_posix() != '.'}
            if any(m.isdir() and p is not None and p not in expected_dirs for m,p in parsed):
                raise InstallError('SDK archive contains unreceipted directory')
            for member, relative in parsed:
                if member.isreg() and relative != 'manifest.json' and (member.mode & 0o7777) != modes[relative]['mode']:
                    raise InstallError('SDK archive mode differs from its receipt')
        if not required<={p for m,p in parsed if m.isreg()}: raise InstallError('missing product binaries/manifests')
        for member,relative in parsed:
            if relative is None: continue
            target=destination.joinpath(*PurePosixPath(relative).parts)
            target.parent.mkdir(mode=0o700,parents=True,exist_ok=True)
            if member.isdir(): target.mkdir(mode=0o700,exist_ok=True); continue
            with bundle.extractfile(member) as source,target.open('xb') as output: shutil.copyfileobj(source,output,1024*1024)
            os.chmod(target,modes[relative]['mode'] if sdk and relative != 'manifest.json' else (0o755 if relative.startswith('bin/') else 0o644))
def verify_package(root,entry,manifest):
    package_bytes,_=_verified_package_file(root,'manifest.json',read_bytes=True,max_bytes=MAX_PACKAGE_METADATA_BYTES)
    try: package=json.loads(package_bytes)
    except (UnicodeDecodeError,json.JSONDecodeError) as error: raise InstallError('package manifest is not valid JSON') from error
    if isinstance(package, dict) and package.get('schema') == SDK_SCHEMA:
        if set(os.listdir(root)) - {'Nudox.app', 'manifest.json', '.installed-release.json', 'bin'}:
            raise InstallError('SDK install contains files outside its closed transport root')
        if _lstat_or_none(root/'bin') is not None:
            sdk_command_link(root)
        verify_sdk_package(root, package, entry['source_sha'])
        check_startup(root, SDK_BIN)
        return SDK_BIN
    if not isinstance(package,dict) or package.get('schema')!='nudox.mac-cli-diagnostic-preview.v1' or package.get('source')!=entry['source_sha'] or package.get('production_ready') is not False: raise InstallError('exact diagnostic source identity required')
    runtime_digest=package.get('runtime_manifest_sha256')
    if not isinstance(runtime_digest,str) or not re.fullmatch(r'[a-f0-9]{64}',runtime_digest): raise InstallError('invalid native build receipt digest')
    if _verified_package_file(root,'native-build-manifest.json')[1]!=runtime_digest: raise InstallError('native build receipt hash mismatch')
    records=package.get('files')
    if not isinstance(records,list): raise InstallError('invalid packaged inventory')
    paths=set()
    for record in records:
        if not isinstance(record,dict): raise InstallError('invalid packaged file record')
        path=record.get('packaged')
        expected_digest=record.get('packaged_sha256')
        if (not isinstance(path,str) or path in paths or path.startswith('/')
                or any(p in {'','..','.'} for p in path.split('/')) or '\\' in path
                or not isinstance(expected_digest,str) or not re.fullmatch(r'[a-f0-9]{64}',expected_digest)):
            raise InstallError('invalid packaged inventory')
        paths.add(path)
        if _verified_package_file(root,path)[1]!=expected_digest: raise InstallError('packaged file hash mismatch')
    if not {'bin/'+n for n in REQUIRED_BINARIES}<=paths: raise InstallError('incomplete executable inventory')
    check_startup(root, 'bin')
    return 'bin'

def check_startup(root, binary_directory):
    clean={k:v for k,v in os.environ.items() if not k.startswith(('NUDOX_','DYLD_'))}
    for name in REQUIRED_BINARIES:
        if subprocess.run([str(root/binary_directory/name),'--help'],env=clean,stdout=subprocess.PIPE,stderr=subprocess.PIPE,timeout=15).returncode: raise InstallError('packaged startup failed: '+name)
    if binary_directory == SDK_BIN:
        receipt_path = 'Nudox.app/Contents/Resources/Build Evidence/compiler-helpers-receipt.json'
        receipt = json.loads(_verified_package_file(root, receipt_path, read_bytes=True, max_bytes=MAX_PACKAGE_METADATA_BYTES)[0])
        helper_root = root/'Nudox.app/Contents/Resources/Helpers/typescript'
        node = helper_root/'node/bin/node'
        probe = subprocess.run([str(node), '--version'], env=clean, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=15)
        if probe.returncode or probe.stdout.strip() != receipt['tools']['node']['version'].encode():
            raise InstallError('installed SDK Node version probe failed')
        probe = subprocess.run([str(node), '-e', "const ts=require(process.argv[1]);if(typeof ts.createProgram!=='function')process.exit(2);process.stdout.write(ts.version)",
                                str(helper_root/'node_modules/typescript')], env=clean, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=15)
        if probe.returncode or probe.stdout != receipt['tools']['typescript']['version'].encode():
            raise InstallError('installed SDK TypeScript Compiler API probe failed')


def sdk_command_link(root, *, create=False):
    """Generated outside the sealed .app; keeps aliases stable across layout updates."""
    link = root/'bin'
    before = _lstat_or_none(link)
    if before is None and create:
        os.symlink(SDK_BIN, link)
        before = link.lstat()
    if (before is None or not stat.S_ISLNK(before.st_mode) or before.st_uid != os.geteuid()
            or before.st_nlink != 1 or os.readlink(link) != SDK_BIN
            or link.resolve(strict=True) != (root/SDK_BIN).resolve(strict=True)
            or _file_identity(before) != _file_identity(link.lstat())):
        raise InstallError('installed SDK command compatibility link is missing or changed')


def sdk_inventory(package):
    records = package.get('files')
    if not isinstance(records, dict) or not 0 < len(records) <= 65536:
        raise InstallError('invalid complete SDK package inventory')
    total = 0
    for name, record in records.items():
        if (not isinstance(name, str) or not name.startswith('Nudox.app/') or len(name) > 4096 or '\\' in name
                or any(part in {'', '.', '..'} for part in name.split('/'))
                or not isinstance(record, dict) or set(record) != {'sha256', 'size_bytes', 'mode'}
                or not isinstance(record['sha256'], str) or not re.fullmatch('[a-f0-9]{64}', record['sha256'])
                or type(record['size_bytes']) is not int or not 0 <= record['size_bytes'] <= MAX_MEMBER_BYTES
                or type(record['mode']) is not int or record['mode'] not in {0o400, 0o444, 0o600, 0o644, 0o755}):
            raise InstallError('invalid SDK package file record')
        total += record['size_bytes']
    if total > MAX_ARCHIVE_BYTES:
        raise InstallError('SDK package inventory exceeds byte limit')
    return records

def verify_sdk_package(root, package, source):
    """Retain the existing .app SDK authority; this envelope only binds transport bytes."""
    records = sdk_inventory(package)
    if (package.get('source') != source or package.get('production_ready') is not False
            or package.get('binary_directory') != SDK_BIN):
        raise InstallError('SDK CLI package differs from selected source/layout')
    observed = set()
    _validate_install_directory(root/'Nudox.app')
    for directory, dirs, files in os.walk(root/'Nudox.app', followlinks=False):
        for name in dirs:
            _validate_install_directory(Path(directory)/name)
        for name in files:
            relative = (Path(directory)/name).relative_to(root).as_posix()
            if relative not in {'manifest.json', '.installed-release.json'}:
                observed.add(relative)
    if observed != set(records):
        raise InstallError('SDK installed file set differs from complete inventory')
    expected_dirs = {parent.as_posix() for name in records for parent in PurePosixPath(name).parents if parent.as_posix() != '.'}
    for directory, dirs, _ in os.walk(root/'Nudox.app', followlinks=False):
        if any((Path(directory)/name).relative_to(root).as_posix() not in expected_dirs for name in dirs):
            raise InstallError('SDK install contains unreceipted directory')
    for name, record in records.items():
        _, digest = _verified_package_file(root, name, max_bytes=record['size_bytes'])
        metadata = (root/name).lstat()
        if digest != record['sha256'] or metadata.st_size != record['size_bytes'] or stat.S_IMODE(metadata.st_mode) != record['mode']:
            raise InstallError('SDK package file hash/size/mode mismatch: '+name)
    raw, digest = _verified_package_file(root, SDK_MANIFEST, read_bytes=True, max_bytes=MAX_PACKAGE_METADATA_BYTES)
    if digest != package.get('build_manifest_sha256'):
        raise InstallError('SDK application manifest hash mismatch')
    try: evidence = json.loads(raw)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise InstallError('invalid SDK application manifest') from error
    if not isinstance(evidence, dict) or not isinstance(evidence.get('source'), dict):
        raise InstallError('invalid SDK application identity')
    if not isinstance(evidence.get('bundle'), dict) or evidence['bundle'].get('identifier') != 'dev.nudox.desktop':
        raise InstallError('unrecognized SDK application bundle identity')
    origin = evidence['source']
    if (evidence.get('schema') != 1 or evidence.get('product') != 'Nudox'
            or origin.get('git_revision') != source or origin.get('working_tree') != 'clean'
            or not isinstance(origin.get('git_tree'), str) or not re.fullmatch('[a-f0-9]{40}', origin['git_tree'])
            or not isinstance(origin.get('cargo_lock_sha256'), str) or not re.fullmatch('[a-f0-9]{64}', origin['cargo_lock_sha256'])
            or evidence.get('target') != {'triple': 'aarch64-apple-darwin', 'architecture': 'arm64'}):
        raise InstallError('SDK application clean source/target mismatch')
    build_raw, build_digest = _verified_package_file(root, 'Nudox.app/Contents/Resources/Build Evidence/application-build-receipt.json', read_bytes=True, max_bytes=MAX_PACKAGE_METADATA_BYTES)
    try: build = json.loads(build_raw)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise InstallError('invalid application build receipt') from error
    application_build = evidence.get('application_build')
    images = build.get('executables') if isinstance(build, dict) else None
    if (not isinstance(application_build, dict) or not isinstance(build, dict) or build.get('schema') != 1
            or build.get('source') != origin or build.get('target') != evidence['target']['triple']
            or build_digest != application_build.get('receipt_sha256')
            or not isinstance(images, dict) or set(images) != {*REQUIRED_BINARIES, 'backend-desktop'}
            or application_build.get('executables') != images
            or any(not isinstance(value, str) or not re.fullmatch('[a-f0-9]{64}', value) for value in images.values())):
        raise InstallError('application build receipt differs from selected bundle provenance')
    helpers = evidence.get('compiler_helpers', {})
    if not isinstance(helpers, dict) or not isinstance(evidence.get('sdk_finalization'), dict):
        raise InstallError('invalid SDK finalization identity')
    if (helpers.get('assembly_mode') != 'typescript-sdk-only'
            or helpers.get('runtime_probe_status') != 'passed post-sign Node and Compiler API probes'
            or evidence.get('sdk_finalization', {}).get('native_probes_stage') != 'after inner signing, before outer app seal'):
        raise InstallError('SDK application has no completed post-sign native admission')
    inventory = evidence.get('files')
    if not isinstance(inventory, dict) or not inventory:
        raise InstallError('missing SDK application inventory')
    for name, record in inventory.items():
        if not isinstance(name, str):
            raise InstallError('invalid SDK application inventory path')
        actual = records.get('Nudox.app/'+name)
        if not isinstance(record, dict) or record.get('kind') != 'file' or actual != {k: record.get(k) for k in ('sha256', 'size_bytes', 'mode')}:
            raise InstallError('SDK application inventory differs from transport: '+str(name))
    extra = set(records) - {'Nudox.app/'+name for name in inventory} - {SDK_MANIFEST}
    if any(not name.startswith('Nudox.app/Contents/_CodeSignature/') for name in extra):
        raise InstallError('SDK transport contains unreceipted application files')
    receipt_name = 'Nudox.app/Contents/Resources/Build Evidence/compiler-helpers-receipt.json'
    receipt_raw, receipt_digest = _verified_package_file(root, receipt_name, read_bytes=True, max_bytes=MAX_PACKAGE_METADATA_BYTES)
    try: receipt = json.loads(receipt_raw)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise InstallError('invalid SDK helper receipt') from error
    if not isinstance(receipt, dict) or not isinstance(receipt.get('tools'), dict):
        raise InstallError('invalid SDK helper identity')
    if (receipt.get('schema') != 1 or receipt.get('source') != origin
            or receipt.get('target') != evidence['target']['triple'] or receipt_digest != helpers.get('receipt_sha256')
            or any(receipt.get(key) != helpers.get(key) for key in ('files', 'tools', 'notices'))
            or set(receipt.get('tools', {})) != {'node', 'typescript'}):
        raise InstallError('SDK helper receipt differs from application authority')
    files = receipt.get('files')
    modes = receipt.get('file_modes')
    if not isinstance(files, dict) or not 0 < len(files) <= 512 or not isinstance(modes, dict) or set(files) != set(modes):
        raise InstallError('invalid SDK helper file/mode receipt')
    if any(not isinstance(name, str) for name in files):
        raise InstallError('invalid SDK helper path')
    sdk_prefix = 'Nudox.app/Contents/Resources/Helpers/'
    if {sdk_prefix+name for name in files} != {name for name in records if name.startswith(sdk_prefix)}:
        raise InstallError('SDK payload membership differs from helper receipt')
    for name, digest in files.items():
        actual = records.get(sdk_prefix+name)
        if (actual is None or actual['sha256'] != digest or actual['mode'] != modes[name]
                or modes[name] != (0o755 if name == 'typescript/node/bin/node' else 0o644)
                or (name not in {'typescript/node/bin/node', 'typescript/node/LICENSE'} and not name.startswith('typescript/node_modules/typescript/'))):
            raise InstallError('SDK helper identity differs from transport inventory')
    if (sum(records[sdk_prefix+name]['size_bytes'] for name in files) > 512*1024**2
            or sum(records[sdk_prefix+name]['size_bytes'] for name in files if name.startswith('typescript/node_modules/typescript/')) > 96*1024**2
            or records.get(sdk_prefix+'typescript/node/LICENSE', {}).get('size_bytes', 1024**2+1) > 1024**2):
        raise InstallError('SDK helper/package/notice byte bound exceeded')
    node = 'typescript/node/bin/node'
    node_record = records.get(sdk_prefix+node)
    tools = receipt['tools']
    if not all(isinstance(tools.get(name), dict) for name in ('node', 'typescript')):
        raise InstallError('invalid SDK tool identities')
    if (node_record is None or node_record['mode'] != 0o755
            or tools['node'].get('sha256') != files.get(node)
            or not isinstance(tools['node'].get('version'), str) or not tools['node']['version']
            or not isinstance(tools['typescript'].get('version'), str) or not tools['typescript']['version']):
        raise InstallError('SDK Node/TypeScript tool identity is incomplete')
    notices = receipt.get('notices', {})
    expected_notices = {'node': 'typescript/node/LICENSE', 'typescript': 'typescript/node_modules/typescript/LICENSE.txt',
                        'typescript_third_party': 'typescript/node_modules/typescript/ThirdPartyNoticeText.txt'}
    required = {'typescript/node_modules/typescript/package.json', 'typescript/node_modules/typescript/bin/tsc',
                'typescript/node_modules/typescript/lib/typescript.js', *expected_notices.values()}
    if notices != expected_notices or not required <= set(files):
        raise InstallError('SDK compiler/notice inventory is incomplete')
    ts_raw, _ = _verified_package_file(root, sdk_prefix+'typescript/node_modules/typescript/package.json', read_bytes=True, max_bytes=1024*1024)
    try: ts = json.loads(ts_raw)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise InstallError('invalid TypeScript package identity') from error
    if not isinstance(ts, dict) or ts.get('name') != 'typescript' or ts.get('version') != tools['typescript']['version']:
        raise InstallError('TypeScript package identity differs from receipt')
    for name in (*REQUIRED_BINARIES, 'backend-desktop', 'Nudox'):
        image = records.get(SDK_BIN+'/'+name)
        if image is None or image['mode'] != 0o755:
            raise InstallError('SDK CLI executable inventory is incomplete')

def main(argv=None):
    parser=argparse.ArgumentParser(description=__doc__); parser.add_argument('--prefix',type=Path)
    parser.add_argument('--allow-downgrade',action='store_true',help='allow an older checkpoint or an unordered checkpoint/stable replacement')
    for flag in ['archive-url','archive-sha256','archive-size','source','tag']: parser.add_argument('--'+flag,required=True)
    args=parser.parse_args(argv)
    if sys.platform!='darwin' or os.uname().machine.lower() not in {'arm64','aarch64'}: raise InstallError('Mac preview supports arm64 only')
    if not re.fullmatch('[a-f0-9]{64}',args.archive_sha256) or not re.fullmatch('[a-f0-9]{40}',args.source): raise InstallError('invalid digest/source')
    if not re.fullmatch(r'checkpoint-[0-9]{8}-'+args.source[:10]+'-macos-arm64',args.tag): raise InstallError('tag/source mismatch')
    size=int(args.archive_size)
    if not 0<size<=MAX_ARCHIVE_BYTES: raise InstallError('invalid archive size')
    parsed=urllib.parse.urlsplit(args.archive_url)
    if parsed.scheme!='https' or parsed.hostname!='github.com' or not parsed.path.startswith('/nudoxorg/Backend/releases/download/'+args.tag+'/') or parsed.username or parsed.password: raise InstallError('invalid immutable archive URL')
    prefix=(args.prefix or Path.home()/'.local').expanduser().resolve()
    entry={'version':'0.0.0','tag':args.tag,'source_sha':args.source,'asset':Path(parsed.path).name}
    manifest={'sha256':args.archive_sha256}
    if reuse_existing_install(prefix,entry,manifest,allow_downgrade=args.allow_downgrade):
        print('Verified existing release '+args.tag+'; reused it without downloading the archive.',file=sys.stderr)
        print('Installed diagnostic Mac CLI preview in '+str(prefix)+'; add '+str(prefix/'bin')+' to PATH.')
        print('Diagnostic CLI package; native whole-application acceptance remains separate. Commands: nudox, nudox-mcp, nudox-locald.')
        return 0
    with tempfile.TemporaryDirectory(prefix='nudox-mac-preview-') as temporary:
        archive=Path(temporary)/'archive.tar.gz'
        download_archive(args.archive_url,args.archive_sha256,size,archive)
        install(prefix,entry,manifest,archive,allow_downgrade=args.allow_downgrade)
    print('Installed diagnostic Mac CLI preview in '+str(prefix)+'; add '+str(prefix/'bin')+' to PATH.')
    print('Diagnostic CLI package; native whole-application acceptance remains separate. Commands: nudox, nudox-mcp, nudox-locald.')
    return 0
if __name__=='__main__':
    try: raise SystemExit(main())
    except (InstallError,OSError,ValueError,KeyError,tarfile.TarError,urllib.error.URLError,subprocess.TimeoutExpired) as error:
        print('nudox Mac preview installer: '+str(error),file=sys.stderr); raise SystemExit(1)
