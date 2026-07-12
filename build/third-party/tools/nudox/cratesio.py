"""crates.io fetching, on-disk manifest caching and crate metadata detection.

Public API
----------
- :func:`fetch_manifest` — disk-cached Cargo.toml parse (uses :data:`paths.CACHE_DIR`).
- :func:`fetch_latest_version`
- :func:`fetch_tarball`
- :func:`sha256_hex`
- :func:`parse_cargo_toml_from_tarball`
- :func:`fetch_version_edition` — concrete edition from the crates.io index.
- :func:`detect_proc_macro`
- :func:`detect_build_script`
- :func:`detect_lib_root`
- :func:`detect_crate_name`
- :func:`detect_default_features`
"""

from __future__ import annotations

import hashlib
import io
import json
import sys
import tarfile
import tomllib
import urllib.request
from pathlib import Path
from typing import Any

from . import paths
from .resolve import normalize

USER_AGENT = "nudox-crates/2.0 (https://nudox.org)"


# ── crates.io HTTP + tarball ──────────────────────────────────────────────────

def api_get(path: str) -> dict[str, Any]:
    """GET https://crates.io<path> and parse the JSON body."""
    url = f"https://crates.io{path}"
    req = urllib.request.Request(url, headers={"User-Agent": USER_AGENT})
    with urllib.request.urlopen(req) as resp:
        return json.loads(resp.read())


def fetch_latest_version(name: str) -> str:
    """Return the newest published version for a crate."""
    data = api_get(f"/api/v1/crates/{name}")
    return data["crate"]["newest_version"]


def fetch_version_edition(name: str, version: str) -> str | None:
    """Return the concrete edition string for ``name@version`` from the crates.io index.

    The index always stores a resolved (non-workspace) edition, so this is the
    right place to look when a crate's Cargo.toml uses workspace inheritance
    (``edition = { workspace = true }``).  Returns ``None`` if the API call
    fails or the field is absent.
    """
    try:
        data = api_get(f"/api/v1/crates/{name}/{version}")
        edition = data.get("version", {}).get("edition")
        if isinstance(edition, str) and edition:
            return edition
    except Exception as exc:
        print(f"  warning: could not fetch index edition for {name}@{version}: {exc}", file=sys.stderr)
    return None


def fetch_tarball(name: str, version: str) -> bytes:
    """Download the ``.crate`` tarball bytes for ``name@version``."""
    url = f"https://static.crates.io/crates/{name}/{name}-{version}.crate"
    print(f"  Downloading {name} {version}...", file=sys.stderr)
    req = urllib.request.Request(url, headers={"User-Agent": USER_AGENT})
    with urllib.request.urlopen(req) as resp:
        return resp.read()


def sha256_hex(data: bytes) -> str:
    """Return the hex sha256 digest of ``data``."""
    return hashlib.sha256(data).hexdigest()


def parse_cargo_toml_from_tarball(tarball: bytes, name: str, version: str) -> dict[str, Any]:
    """Extract and parse the Cargo.toml from an in-memory ``.crate`` tarball."""
    prefix = f"{name}-{version}/"
    with tarfile.open(fileobj=io.BytesIO(tarball), mode="r:gz") as tar:
        # Prefer Cargo.toml.orig (pre-normalization) for richer data.
        for candidate in (f"{prefix}Cargo.toml.orig", f"{prefix}Cargo.toml"):
            for member in tar.getmembers():
                if member.name == candidate:
                    f = tar.extractfile(member)
                    if f:
                        return tomllib.loads(f.read().decode())
    return {}


# ── disk-cached manifest fetch ────────────────────────────────────────────────

def _read_manifest_cache(cache_file: Path) -> dict[str, Any] | None:
    try:
        return json.loads(cache_file.read_text(encoding="utf-8"))
    except (json.JSONDecodeError, OSError):
        return None


def _unwrap_manifest_cache(cached: dict[str, Any]) -> tuple[dict[str, Any], bool | None]:
    """Return ``(cargo_toml, has_build_rs)`` from a cache blob.

    Legacy caches store the Cargo.toml dict directly; newer caches wrap it as
    ``{"cargo": ..., "has_build_rs": bool}``.
    """
    if "cargo" in cached:
        has_build = cached.get("has_build_rs")
        return cached["cargo"], has_build if isinstance(has_build, bool) else None
    return cached, None


def fetch_manifest_meta(name: str, version: str) -> tuple[dict[str, Any], bool]:
    """Return ``(cargo_toml, has_build_rs)`` for a crate@version (cached on disk)."""
    paths.CACHE_DIR.mkdir(parents=True, exist_ok=True)
    cache_file = paths.CACHE_DIR / f"{name}-{version}.toml.json"

    cached = _read_manifest_cache(cache_file) if cache_file.exists() else None
    if cached is not None:
        manifest, has_build = _unwrap_manifest_cache(cached)
        if has_build is not None:
            return manifest, has_build

    url = f"https://static.crates.io/crates/{name}/{name}-{version}.crate"
    request = urllib.request.Request(url, headers={"User-Agent": USER_AGENT})

    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            crate_bytes = response.read()
    except Exception as exc:
        print(f"\n⚠️  Download failed for {name}-{version}: {exc}", file=sys.stderr)
        cache_file.write_text("{}", encoding="utf-8")
        return {}, False

    prefix = f"{name}-{version}/"
    manifest: dict[str, Any] = {}

    with tarfile.open(fileobj=io.BytesIO(crate_bytes), mode="r:gz") as tar:
        # Prefer the normalized Cargo.toml crates.io publishes; .orig often still
        # uses { workspace = true } and poisons feature/dep resolution.
        for candidate in (f"{prefix}Cargo.toml", f"{prefix}Cargo.toml.orig"):
            try:
                member = tar.extractfile(candidate)
                if member is not None:
                    manifest = tomllib.loads(member.read().decode("utf-8"))
                    break
            except KeyError:
                continue
            except Exception as exc:
                print(f"⚠️  Failed to parse {candidate}: {exc}", file=sys.stderr)

    has_build = detect_build_script(manifest, crate_bytes, name, version)
    cache_file.write_text(
        json.dumps({"cargo": manifest, "has_build_rs": has_build}),
        encoding="utf-8",
    )
    return manifest, has_build


def fetch_manifest(name: str, version: str) -> dict[str, Any]:
    """Return the parsed Cargo.toml for a crate@version (cached on disk)."""
    manifest, _has_build = fetch_manifest_meta(name, version)
    return manifest


# ── crate metadata detection ──────────────────────────────────────────────────

def detect_proc_macro(cargo_toml: dict[str, Any]) -> bool:
    """Return True if the crate declares ``[lib] proc-macro = true``."""
    return cargo_toml.get("lib", {}).get("proc-macro", False)


def detect_build_script(
    cargo_toml: dict[str, Any], tarball: bytes, name: str, version: str
) -> bool:
    """Return True if the crate has a build script."""
    build_field = cargo_toml.get("package", {}).get("build")
    if build_field is False:
        return False
    if isinstance(build_field, str) and build_field:
        return True
    prefix = f"{name}-{version}/"
    with tarfile.open(fileobj=io.BytesIO(tarball), mode="r:gz") as tar:
        return any(m.name == f"{prefix}build.rs" for m in tar.getmembers())


def detect_lib_root(cargo_toml: dict[str, Any]) -> str | None:
    """Return the [lib] path if non-standard, else None."""
    path = cargo_toml.get("lib", {}).get("path")
    if path and path.replace("\\", "/") != "src/lib.rs":
        return path.replace("\\", "/")
    return None


def detect_crate_name(cargo_toml: dict[str, Any], package_name: str) -> str | None:
    """Return [lib] name if it differs from normalize(package_name), else None."""
    lib_name = cargo_toml.get("lib", {}).get("name")
    if lib_name and lib_name != normalize(package_name):
        return lib_name
    return None


def detect_default_features(cargo_toml: dict[str, Any]) -> list[str]:
    """Return the features listed under [features] default (one-level expanded)."""
    default = cargo_toml.get("features", {}).get("default", [])
    features = cargo_toml.get("features", {})
    expanded: set[str] = set()
    for f in default:
        expanded.add(f)
        for sub in features.get(f, []):
            if not sub.startswith("dep:"):
                expanded.add(sub)
    return sorted(expanded)
