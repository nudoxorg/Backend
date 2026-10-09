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
    try:
        with HTTPS.open(request, timeout=120) as response, destination.open("xb") as output:
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
    except urllib.error.HTTPError as error:
        raise InstallError(f"release archive request failed with HTTP {error.code}") from error
    except (urllib.error.URLError, TimeoutError, OSError) as error:
        raise InstallError(f"could not download the release archive: {error}") from error
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

def verify_existing_install(final: Path, expected_marker: dict, entry: dict, manifest: dict) -> None:
    """Reuse a prior release only after rechecking the files on disk."""
    installed_marker = final / ".installed-release.json"
    try:
        existing_marker = json.loads(installed_marker.read_text())
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as error:
        raise InstallError(f"refusing to reuse existing unverified install at {final}") from error
    if existing_marker != expected_marker:
        raise InstallError(f"release {entry['tag']} is already installed with different bytes")
    try:
        verify_package(final, entry, manifest)
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
    marker = current.resolve(strict=True) / ".installed-release.json"
    try:
        descriptor = os.open(marker, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
        with os.fdopen(descriptor, "rb") as stream:
            before = os.fstat(stream.fileno())
            if (not stat.S_ISREG(before.st_mode) or before.st_size > 16 * 1024
                    or before.st_uid != os.geteuid() or before.st_nlink != 1
                    or stat.S_IMODE(before.st_mode) & 0o022):
                raise InstallError("active release marker is not a bounded private owned regular file")
            raw = stream.read(16 * 1024 + 1)
            after = os.fstat(stream.fileno())
            if len(raw) != before.st_size or _file_identity(before) != _file_identity(after) or _file_identity(after) != _file_identity(marker.lstat()):
                raise InstallError("active release marker changed during admission")
        return json.loads(raw)
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as error:
        raise InstallError("cannot verify active checkpoint before update") from error


def check_checkpoint_update(current, incoming, allow_downgrade=False):
    """Admit exact marker identities; the explicit flag changes ordering only."""
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
    if incoming_date <= active_date:
        reason = "older" if incoming_date < active_date else "different same-date"
        raise InstallError(f"refusing {reason} checkpoint {incoming['tag']}; active is {installed['tag']}. Use --allow-downgrade to replace it explicitly")


@contextmanager
def _install_lease(managed_root):
    """Hold one owned inode from checkpoint admission through publication."""
    _validate_install_directory(managed_root)
    path = managed_root / ".install.lock"
    descriptor = os.open(path, os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW | os.O_NONBLOCK, 0o600)
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
    bin_dir = prefix / "bin"
    if bin_dir.is_symlink():
        raise InstallError(f"command install directory must not be a symlink: {bin_dir}")
    bin_dir.mkdir(mode=0o700, exist_ok=True)
    if not bin_dir.is_dir():
        raise InstallError(f"command install path is not a directory: {bin_dir}")
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
    marker = {"version": entry["version"], "tag": entry["tag"], "source_sha": entry["source_sha"], "asset": entry["asset"], "sha256": manifest["sha256"]}
    check_checkpoint_update(current, marker, allow_downgrade)
    stage = Path(tempfile.mkdtemp(prefix=".staging-", dir=version_root))
    try:
        safe_extract(archive, stage)
        verify_package(stage, entry, manifest)
        (stage / ".installed-release.json").write_text(json.dumps(marker, sort_keys=True) + "\n")
        os.chmod(stage, 0o755)
        if final.exists() or final.is_symlink():
            if final.is_symlink() or not final.is_dir():
                raise InstallError(f"version path already exists and is not an install directory: {final}")
            verify_existing_install(final, marker, entry, manifest)
            shutil.rmtree(stage)
        else:
            os.replace(stage, final)
    except Exception:
        shutil.rmtree(stage, ignore_errors=True)
        raise

    for name, target in links.items():
        link = bin_dir / name
        if not link.is_symlink() or os.readlink(link) != target:
            _replace_symlink(link, target)

    _replace_symlink(current, os.path.relpath(final, managed_root))

def shlex_quote(value: str) -> str:
    import shlex
    return shlex.quote(value)
HTTPS=urllib.request.build_opener(HTTPSOnlyRedirect())

def safe_extract(archive,destination):
    with tarfile.open(archive,'r:gz') as bundle:
        members=bundle.getmembers()
        if not members or len(members)>10000: raise InstallError('invalid member count')
        seen=set(); parsed=[]; total=0
        for member in members:
            relative=_safe_member(member,'nudox-macos-arm64')
            if relative in seen: raise InstallError('duplicate archive member')
            seen.add(relative); total+=member.size
            if total>MAX_ARCHIVE_BYTES: raise InstallError('archive expands beyond limit')
            parsed.append((member,relative))
        required={'bin/'+n for n in REQUIRED_BINARIES}|{'manifest.json','native-build-manifest.json','README.txt'}
        if not required<={p for m,p in parsed if m.isreg()}: raise InstallError('missing product binaries/manifests')
        for member,relative in parsed:
            if relative is None: continue
            target=destination.joinpath(*PurePosixPath(relative).parts)
            target.parent.mkdir(mode=0o700,parents=True,exist_ok=True)
            if member.isdir(): target.mkdir(mode=0o700,exist_ok=True); continue
            with bundle.extractfile(member) as source,target.open('xb') as output: shutil.copyfileobj(source,output,1024*1024)
            os.chmod(target,0o755 if relative.startswith('bin/') else 0o644)
def verify_package(root,entry,manifest):
    package=json.loads((root/'manifest.json').read_bytes())
    if package.get('schema')!='nudox.mac-cli-diagnostic-preview.v1' or package.get('source')!=entry['source_sha'] or package.get('production_ready') is not False: raise InstallError('exact diagnostic source identity required')
    if _file_sha256(root/'native-build-manifest.json')!=package['runtime_manifest_sha256']: raise InstallError('native build receipt hash mismatch')
    paths=set()
    for record in package.get('files',[]):
        path=record['packaged']
        if not isinstance(path,str) or path in paths or path.startswith('/') or any(p in {'','..','.'} for p in path.split('/')) or '\\' in path: raise InstallError('invalid packaged inventory')
        paths.add(path)
        if _file_sha256(root/path)!=record['packaged_sha256']: raise InstallError('packaged file hash mismatch')
    if not {'bin/'+n for n in REQUIRED_BINARIES}<=paths: raise InstallError('incomplete executable inventory')
    clean={k:v for k,v in os.environ.items() if not k.startswith(('NUDOX_','DYLD_'))}
    for name in REQUIRED_BINARIES:
        if subprocess.run([str(root/'bin'/name),'--help'],env=clean,stdout=subprocess.PIPE,stderr=subprocess.PIPE,timeout=15).returncode: raise InstallError('packaged startup failed: '+name)
def main(argv=None):
    parser=argparse.ArgumentParser(description=__doc__); parser.add_argument('--prefix',type=Path)
    parser.add_argument('--allow-downgrade',action='store_true',help='allow an older or different same-date checkpoint')
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
    with tempfile.TemporaryDirectory(prefix='nudox-mac-preview-') as temporary:
        archive=Path(temporary)/'archive.tar.gz'
        download_archive(args.archive_url,args.archive_sha256,size,archive)
        entry={'version':'0.0.0','tag':args.tag,'source_sha':args.source,'asset':Path(parsed.path).name}
        install(prefix,entry,{'sha256':args.archive_sha256},archive,allow_downgrade=args.allow_downgrade)
    print('Installed diagnostic Mac CLI preview in '+str(prefix)+'; add '+str(prefix/'bin')+' to PATH.')
    print('Known publication failures remain; ad-hoc signed, not notarized. Commands: nudox, nudox-mcp, nudox-locald.')
    return 0
if __name__=='__main__':
    try: raise SystemExit(main())
    except (InstallError,OSError,ValueError,KeyError,tarfile.TarError,urllib.error.URLError,subprocess.TimeoutExpired) as error:
        print('nudox Mac preview installer: '+str(error),file=sys.stderr); raise SystemExit(1)
