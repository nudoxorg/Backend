"""Bound retained registry metadata and archive bytes to an acquired source tree.

This observer does not extract archives or invent registry acquisition evidence.
"""
from __future__ import annotations

import base64
import hashlib
import io
import json
import re
import tarfile
import zipfile
from email.parser import BytesParser
from pathlib import PurePosixPath
from urllib.parse import unquote, urlsplit


class OriginError(ValueError):
    pass


def canonical_json(value):
    return json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode()


def official_url(value, host, path=None):
    if not isinstance(value, str):
        raise OriginError("registry artifact URL is invalid")
    parsed = urlsplit(value)
    if parsed.scheme != "https" or parsed.netloc != host or parsed.query or parsed.fragment or (path is not None and parsed.path != path):
        raise OriginError("registry artifact URL is outside its official origin")


def normalized_pypi(value):
    return re.sub(r"[-_.]+", "-", value).lower() if isinstance(value, str) else None


def escaped_go_module(value):
    return "".join("!" + character.lower() if character.isupper() else character for character in value)


def go_hash1(files, prefix=""):
    # Go x/mod/sumdb/dirhash.Hash1 hashes sorted content-digest/name lines.
    if any("\n" in prefix + row["path"] for row in files):
        raise OriginError("Go content sum filenames cannot contain newlines")
    payload = "".join(row["sha256"] + "  " + prefix + row["path"] + "\n"
                      for row in sorted(files, key=lambda row: prefix + row["path"]))
    return "h1:" + base64.b64encode(hashlib.sha256(payload.encode()).digest()).decode()


def verify_go_origin(package, receipt, metadata, members, special, read_artifact, decode_json):
    module = receipt.get("module")
    if not isinstance(module, dict) or set(module) != {"go_mod", "sumdb_lookup", "download", "process", "go_list"}:
        raise OriginError("Go origin lacks its closed module acquisition evidence")
    payload = {key: read_artifact(value) for key, value in module.items()}
    name, version = package["id"], package["version"]
    escaped = escaped_go_module(name)
    prefix = name + "@" + version + "/"
    official_url(receipt["registry_metadata"]["url"], "proxy.golang.org", f"/{escaped}/@v/{version}.info")
    official_url(receipt["archive"]["url"], "proxy.golang.org", f"/{escaped}/@v/{version}.zip")
    if metadata.get("Version") != version or receipt["unpack"]["strip_prefix"] != prefix:
        raise OriginError("Go proxy version or archive prefix differs from the module pin")
    if special.get("go.mod") != payload["go_mod"]:
        raise OriginError("Go proxy module declaration differs from the actual archive")
    declaration = re.search(rb"(?m)^module[ \t]+([^\s]+)[ \t]*(?://[^\n]*)?$", payload["go_mod"])
    if declaration is None or declaration[1].decode() != name:
        raise OriginError("Go requested module differs from the archived module declaration; aliases are unsupported")
    download = decode_json(payload["download"], "original Go download output")
    process = decode_json(payload["process"], "original Go download process witness")
    if not isinstance(download, dict) or not isinstance(process, dict):
        raise OriginError("Go acquisition witness is not an object")
    if download.get("Path") != name or download.get("Version") != version or download.get("Error") or download.get("Info") != receipt["registry_metadata"]["path"] or download.get("Zip") != receipt["archive"]["path"] or download.get("GoMod") != module["go_mod"]["path"]:
        raise OriginError("original Go download belongs to another module or version")
    expected_sum = go_hash1(members, prefix)
    mod_sum = go_hash1([{"path": "go.mod", "sha256": hashlib.sha256(payload["go_mod"]).hexdigest()}])
    if download.get("Sum") != expected_sum or download.get("GoModSum") != mod_sum:
        raise OriginError("Go module h1 differs from the retained archive or module file")
    lookup = payload["sumdb_lookup"].decode("utf-8")
    if name + " " + version + " " + expected_sum not in lookup.splitlines() or name + " " + version + "/go.mod " + mod_sum not in lookup.splitlines() or "— sum.golang.org " not in lookup:
        raise OriginError("retained SumDB lookup does not bind the exact module and content sums")
    request, executable, environment = (process.get(key, {}) for key in ("request", "executable", "environment"))
    if not all(isinstance(value, dict) for value in (request, executable, environment)):
        raise OriginError("Go original acquisition process has malformed typed facts")
    query = request.get("query")
    if process.get("schema") != "nudox.go-module-download-process.v1" or type(process.get("exit_code")) is not int or process["exit_code"] != 0 or request.get("registry_request_path") != name or request.get("resolved_version") != version or query not in {version, "latest"}:
        raise OriginError("Go original acquisition process lacks its exact successful pin")
    if process.get("argv") != [executable.get("path"), "mod", "download", "-json", name + "@" + query] or environment.get("GOPROXY") != "https://proxy.golang.org" or environment.get("GOSUMDB") != "sum.golang.org":
        raise OriginError("Go original acquisition did not use the admitted proxy and SumDB")
    if read_artifact(process.get("stdout")) != payload["download"] or read_artifact(process.get("stderr")):
        raise OriginError("Go original process output differs from its successful acquisition capture")
    read_artifact({"path": executable.get("path"), "sha256": executable.get("sha256")})
    read_artifact({"path": executable.get("snapshot_path"), "sha256": executable.get("snapshot_sha256")})
    for key in ("timeout_pid_file", "acquisition_status"):
        read_artifact(process.get(key))
    drivers = process.get("driver_files")
    if not isinstance(drivers, list) or not 1 <= len(drivers) <= 8:
        raise OriginError("Go original acquisition has no bounded driver witness")
    for row in drivers:
        read_artifact(row)
    proxy = process.get("proxy_artifacts", {})
    if not isinstance(proxy, dict):
        raise OriginError("Go original acquisition lacks its proxy artifact facts")
    for key, expected in (("go_mod", payload["go_mod"]), ("sumdb_lookup", payload["sumdb_lookup"]),
                          ("info", read_artifact(receipt["registry_metadata"])),
                          ("zip", read_artifact(receipt["archive"]))):
        if read_artifact(proxy.get(key)) != expected:
            raise OriginError("Go original acquisition proxy artifacts differ from the verified module")
    if read_artifact(proxy.get("ziphash")).decode().strip() != expected_sum or proxy.get("sum") != expected_sum or proxy.get("go_mod_sum") != mod_sum:
        raise OriginError("Go original acquisition content sums differ from the verified module")
    return {"go_list": decode_json(payload["go_list"], "retained Go target witness"),
            "authentication_source": "original successful Go mod download with GOSUMDB=sum.golang.org; retained signed lookup compared, no independent Python signature verification",
            "process_sha256": hashlib.sha256(payload["process"]).hexdigest(),
            "sumdb_lookup_sha256": hashlib.sha256(payload["sumdb_lookup"]).hexdigest(),
            "go_list_sha256": hashlib.sha256(payload["go_list"]).hexdigest()}


def archive_members(archive, prefix, maximum_files, maximum_file_bytes, maximum_bytes, check=lambda: None):
    if not isinstance(prefix, str) or not prefix.endswith("/") or not prefix[:-1] or PurePosixPath(prefix[:-1]).as_posix() != prefix[:-1] or prefix.startswith("/") or "\\" in prefix or any(part in {".", ".."} for part in PurePosixPath(prefix).parts):
        raise OriginError("archive extraction prefix is not canonical")
    files, special, total, entries = [], {}, 0, 0

    def record(name, size, regular, directory, open_stream):
        nonlocal total, entries
        entries += 1
        check()
        if entries > 2 * maximum_files:
            raise OriginError("archive traversal exceeds its member bound")
        if not isinstance(name, str) or "\\" in name or name.startswith("/") or any(part in {".", ".."} for part in name.split("/") if part):
            raise OriginError("archive contains an unconfined member")
        if name.rstrip("/") == prefix.rstrip("/") and directory:
            return
        if not name.startswith(prefix):
            raise OriginError("archive contains a member outside its declared extraction root")
        relative = name[len(prefix):]
        if directory:
            return
        if not regular or not relative or PurePosixPath(relative).as_posix() != relative or relative.split("/")[0] == ".git":
            raise OriginError("archive contains a link, special, or noncanonical source member")
        if size < 0 or size > maximum_file_bytes or len(files) >= maximum_files:
            raise OriginError("archive file exceeds its size or member bound")
        total += size
        if total > maximum_bytes:
            raise OriginError("archive exceeds its uncompressed byte bound")
        digest, captured, count = hashlib.sha256(), bytearray(), 0
        with open_stream() as stream:
            while chunk := stream.read(min(65536, maximum_file_bytes + 1 - count)):
                check()
                count += len(chunk)
                if count > size or count > maximum_file_bytes:
                    raise OriginError("archive member exceeds its declared size")
                digest.update(chunk)
                if relative in {"package.json", "PKG-INFO", "go.mod"}:
                    if len(captured) + len(chunk) > 1024 * 1024:
                        raise OriginError("archive identity metadata exceeds its byte bound")
                    captured.extend(chunk)
        if count != size:
            raise OriginError("archive member ended before its declared size")
        files.append({"path": relative, "bytes": size, "sha256": digest.hexdigest()})
        if relative in {"package.json", "PKG-INFO", "go.mod"}:
            special[relative] = bytes(captured)

    try:
        if zipfile.is_zipfile(io.BytesIO(archive)):
            with zipfile.ZipFile(io.BytesIO(archive)) as source:
                for entry in source.infolist():
                    mode = entry.external_attr >> 16
                    regular = not entry.is_dir() and (mode & 0o170000) in {0, 0o100000}
                    record(entry.filename, entry.file_size, regular, entry.is_dir(), lambda e=entry: source.open(e))
        else:
            with tarfile.open(fileobj=io.BytesIO(archive), mode="r|*") as source:
                for entry in source:
                    record(entry.name, entry.size, entry.isfile(), entry.isdir(), lambda e=entry: source.extractfile(e))
    except (tarfile.TarError, zipfile.BadZipFile, OSError, EOFError) as error:
        raise OriginError("registry archive could not be safely read") from error
    files.sort(key=lambda row: row["path"])
    if not files or len({row["path"] for row in files}) != len(files):
        raise OriginError("registry archive has empty or duplicate file membership")
    return files, special


def verify_registry_metadata(package, metadata, metadata_url, archive, archive_url, special):
    ecosystem, name, version = (package[key] for key in ("ecosystem", "id", "version"))
    if ecosystem == "pypi":
        official_url(metadata_url, "pypi.org")
        metadata_path = urlsplit(metadata_url).path.split("/")
        project_segment = unquote(metadata_path[2]) if len(metadata_path) == 4 else ""
        if metadata_path[:2] != ["", "pypi"] or metadata_path[-1] != "json" or not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._-]*", project_segment) or normalized_pypi(project_segment) != name:
            raise OriginError("PyPI metadata URL belongs to another project")
        official_url(archive_url, "files.pythonhosted.org")
        if normalized_pypi(metadata.get("info", {}).get("name")) != name:
            raise OriginError("PyPI metadata belongs to another project")
        releases = metadata.get("releases", {}).get(version)
        matches = [row for row in releases or [] if isinstance(row, dict)
                   and row.get("packagetype") == "sdist" and row.get("url") == archive_url]
        if len(matches) != 1 or matches[0].get("size") != len(archive) or matches[0].get("digests", {}).get("sha256") != hashlib.sha256(archive).hexdigest():
            raise OriginError("PyPI release does not bind the exact retained archive")
        info = BytesParser().parsebytes(special.get("PKG-INFO", b""))
        if normalized_pypi(info.get("Name")) != name or info.get("Version") != version:
            raise OriginError("sdist package metadata belongs to another project or version")
    elif ecosystem == "npm":
        official_url(metadata_url, "registry.npmjs.org")
        metadata_path = unquote(urlsplit(metadata_url).path)
        package_path = "/" + name
        if metadata_path == package_path:
            release = metadata.get("versions", {}).get(version)
        elif metadata_path in {package_path + "/" + version, package_path + "/latest"}:
            release = metadata
        else:
            raise OriginError("NPM metadata URL belongs to another package or release")
        official_url(archive_url, "registry.npmjs.org")
        if metadata.get("name") != name or not isinstance(release, dict) or release.get("name") != name or release.get("version") != version:
            raise OriginError("NPM metadata belongs to another package or version")
        dist = release.get("dist", {})
        if dist.get("tarball") != archive_url:
            raise OriginError("NPM release does not bind the retained archive URL")
        integrity = dist.get("integrity")
        if integrity:
            candidates = []
            for token in integrity.split():
                algorithm, separator, encoded = token.partition("-")
                if separator and algorithm in {"sha512", "sha384", "sha256"}:
                    try:
                        candidates.append(base64.b64decode(encoded, validate=True) == hashlib.new(algorithm, archive).digest())
                    except ValueError as error:
                        raise OriginError("NPM integrity value is malformed") from error
            if not candidates or not all(candidates):
                raise OriginError("NPM release integrity differs from the retained archive")
        elif dist.get("shasum") != hashlib.sha1(archive).hexdigest():
            raise OriginError("NPM release shasum differs from the retained archive")
        try:
            info = json.loads(special.get("package.json", b""))
        except (ValueError, UnicodeDecodeError) as error:
            raise OriginError("NPM archive has no valid package identity metadata") from error
        if info.get("name") != name or info.get("version") != version:
            raise OriginError("NPM archive belongs to another package or version")
    else:
        raise OriginError("Go origin requires its separate authenticated module acquisition binding")
