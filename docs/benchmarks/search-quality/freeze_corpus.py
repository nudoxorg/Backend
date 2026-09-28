#!/usr/bin/env python3
"""Freeze the real package identities used by the search benchmark."""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import sys
import tomllib
from pathlib import Path
from typing import Any
from urllib.parse import urlsplit


ROOT = Path(__file__).resolve().parents[3]
HERE = Path(__file__).resolve().parent
CAPTURED_AT = "2026-09-28T06:58:07Z"
CARGO_LOCK_CAPTURED_AT = "2026-09-28T08:51:28Z"
CARGO_LOCK_SNAPSHOT = "docs/benchmarks/search-quality/snapshots/workspace-cargo-lock-2026-09-28.lock"
LIB_RS_REVISION = "4642a01664e14f4ae30a3804a55556b0770119d9"

PIN_COORDINATES = {
    "cargo:thiserror@2.0.20",
    "cargo:serde@1.0.229",
    "cargo:bincode@3.0.0",
    "npm:@tanstack/query-core@5.62.8",
    "npm:zod@3.25.76",
    "pypi:requests@2.32.3",
    "pypi:attrs@25.3.0",
    "pypi:attrs@26.1.0",
    "golang:github.com/go-chi/chi/v5@v5.0.12",
    "golang:golang.org/x/sync@v0.10.0",
    "golang:golang.org/x/sync@v0.11.0",
    "maven:com.fasterxml.jackson.core:jackson-databind@2.16.1",
    "maven:com.fasterxml.jackson.core:jackson-databind@2.17.1",
    "maven:jakarta.servlet:jakarta.servlet-api@5.0.0",
    "maven:jakarta.servlet:jakarta.servlet-api@6.0.0",
    "nuget:esp-net-source@0.2.3",
    "nuget:esp-net-source@0.6.4",
    "nuget:morelinq.source.moreenumerable.distinctby@1.0.0",
    "nuget:morelinq.source.moreenumerable.distinctby@1.0.2",
}
FORGE_COORDINATES = {"yaml-cpp", "json-c"}
INDEX_CASES = {
    "serde": {"1.0.31", "1.0.95", "1.0.229"},
    "bincode": {"1.3.0", "1.3.3", "3.0.0"},
    "thiserror": {"2.0.20"},
}
INDEX_PATHS = {"serde": "se/rd/serde", "bincode": "bi/nc/bincode", "thiserror": "th/is/thiserror"}
SOURCE_PATHS = [
    ".config/nix/corpus-pins.json",
    "Cargo.toml",
    "crates/advisory/fixtures/rustsec/crates/bincode/RUSTSEC-2025-0141.md",
    "crates/engine/src/forge/mod.rs",
    "crates/engine/src/registry/discovery.rs",
    "crates/local-service/src/builtin/discovery_search.rs",
    "crates/local-service/src/builtin/discovery_search/benchmark.rs",
    "crates/local-service/src/discovery.rs",
    "crates/local-service/Cargo.toml",
    "crates/local-service/src/builtin.rs",
    "crates/local-service/src/lib.rs",
    "docs/benchmarks/search-quality/adapter/Cargo.toml",
    "docs/benchmarks/search-quality/adapter/src/main.rs",
    "docs/benchmarks/search-quality/adapter/tests/production_ranker.rs",
    "crates/semantic/src/vocabulary/package.rs",
    "tests/journeys/tests/index_tentpole.rs",
    "tests/journeys/tests/registry_discovery.rs",
]
OPTIONAL_SOURCE_PATHS = ["docs/benchmarks/search-quality/adapter/Cargo.lock"]
FIXTURE_SOURCE_LABELS = {
    "workspace-fixture:discovery-search-typed-facets": "crates/local-service/src/builtin/discovery_search.rs",
    "workspace-fixture:registry-discovery-cargo-yank": "tests/journeys/tests/registry_discovery.rs",
    "workspace-fixture:tentpole-manifest-dependencies": "tests/journeys/tests/index_tentpole.rs",
}


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def file_sha256(path: Path) -> str:
    return sha256(path.read_bytes())


def read_json(path: Path) -> Any:
    return json.loads(path.read_text(encoding="utf-8"))


def split_coordinate(coordinate: str) -> tuple[str, str, str | None]:
    if "@" in coordinate:
        package, version = coordinate.rsplit("@", 1)
    else:
        package, version = coordinate, None
    if ":" in package:
        prefix, name = package.split(":", 1)
    else:
        prefix, name = "clang", package
    if prefix == "golang":
        ecosystem = "go"
    elif prefix == "maven":
        ecosystem = "maven"
        name = name.rsplit(":", 1)[-1]
    elif prefix == "nuget":
        ecosystem = "nuget"
    elif prefix == "npm":
        ecosystem = "npm"
    elif prefix == "pypi":
        ecosystem = "pypi"
    elif prefix == "cargo":
        ecosystem = "cargo"
    else:
        ecosystem = "forge"
    return ecosystem, name, version


def package_name_from_pin(coordinate: str) -> tuple[str, str | None, str | None]:
    ecosystem, name, version = split_coordinate(coordinate)
    if ecosystem == "forge":
        name = coordinate
    return ecosystem, name, version


def parse_index_snapshots() -> tuple[dict[tuple[str, str], dict[str, Any]], list[dict[str, Any]]]:
    entries: dict[tuple[str, str], dict[str, Any]] = {}
    snapshots = []
    for name in sorted(INDEX_CASES):
        path = HERE / "snapshots" / f"crates-io-{name}.ndjson"
        body = path.read_bytes()
        snapshot_id = f"crates-io-index:{name}:sha256:{sha256(body)}"
        snapshots.append(
            {
                "snapshot_id": snapshot_id,
                "source": "https://index.crates.io/" + INDEX_PATHS[name],
                "captured_at": CAPTURED_AT,
                "local_file": path.relative_to(ROOT).as_posix(),
                "bytes": len(body),
                "sha256": sha256(body),
            }
        )
        for raw_line in body.splitlines(keepends=True):
            row = json.loads(raw_line)
            version = row["vers"]
            if version in INDEX_CASES[name]:
                entries[(name, version)] = {
                    "row": row,
                    "raw_line_sha256": sha256(raw_line),
                    "snapshot_id": snapshot_id,
                }
    return entries, snapshots


def source_manifest(index_snapshots: list[dict[str, Any]]) -> dict[str, Any]:
    sources = []
    source_paths = [
        *SOURCE_PATHS,
        *(relative for relative in OPTIONAL_SOURCE_PATHS if (ROOT / relative).is_file()),
    ]
    for relative in source_paths:
        path = ROOT / relative
        digest = file_sha256(path)
        if relative == ".config/nix/corpus-pins.json":
            logical_id = f"nix-corpus-pins:sha256:{digest}"
        elif relative == "docs/benchmarks/search-quality/adapter/Cargo.lock":
            logical_id = f"search-adapter-cargo-lock:sha256:{digest}"
        else:
            logical_id = f"workspace-file:sha256:{digest}"
        sources.append(
            {
                "snapshot_id": logical_id,
                "path": relative,
                "sha256": digest,
            }
        )
        generic_id = f"workspace-file:sha256:{digest}"
        if generic_id != logical_id:
            sources.append(
                {
                    "snapshot_id": generic_id,
                    "path": relative,
                    "sha256": digest,
                    "alias_of": logical_id,
                }
            )
    sources.extend(index_snapshots)
    for snapshot_id, relative in FIXTURE_SOURCE_LABELS.items():
        digest = file_sha256(ROOT / relative)
        sources.append(
            {
                "snapshot_id": snapshot_id,
                "path": relative,
                "sha256": digest,
                "note": "Fixture source; fixture values are not claims about current live registry metadata.",
            }
        )
    sources.append(
        {
            "snapshot_id": f"lib-rs-mirror:git:{LIB_RS_REVISION}",
            "source": "https://github.com/Protryon/lib-rs-mirror/tree/" + LIB_RS_REVISION,
            "files": [
                {
                    "path": "search_index/src/lib_search_index.rs",
                    "sha256": "24705598c93b49933013125e69e7cfdb39e7b4f47d4cdaa68ae4c47367f6e301",
                },
                {
                    "path": "ranking/src/lib_ranking.rs",
                    "sha256": "45e2b9fd0de65db22e6c3067f57be6e1c788c482571124992a162bd732fafcda",
                },
            ],
        }
    )
    lock_snapshot_path = ROOT / CARGO_LOCK_SNAPSHOT
    lock_snapshot_digest = file_sha256(lock_snapshot_path)
    sources.append(
        {
            "snapshot_id": f"cargo-lock-snapshot:sha256:{lock_snapshot_digest}",
            "source": "workspace Cargo.lock captured as immutable benchmark evidence; not a live build lock",
            "captured_at": CARGO_LOCK_CAPTURED_AT,
            "local_file": CARGO_LOCK_SNAPSHOT,
            "bytes": lock_snapshot_path.stat().st_size,
            "sha256": lock_snapshot_digest,
        }
    )
    return {
        "schema_version": 1,
        "benchmark_snapshot_id": "search-quality-v1-2026-09-28",
        "captured_at": CAPTURED_AT,
        "source_snapshots": sources,
    }


def tri_state(status: str, values: list[Any] | None = None, source_snapshot_id: str | None = None) -> dict[str, Any]:
    return {"status": status, "values": values or [], "source_snapshot_id": source_snapshot_id}


def base_doc(
    document_id: str,
    ecosystem: str,
    name: str,
    version: str | None,
    source_kind: str,
    source_snapshot_id: str,
    coordinate: str,
    archive_url: str | None = None,
    archive_hash: str | None = None,
    forge_source_coordinate: str | None = None,
) -> dict[str, Any]:
    document = {
        "document_id": document_id,
        "ecosystem": ecosystem,
        "name": name,
        "version": version,
        "coordinate": coordinate,
        "source_kind": source_kind,
        "source_snapshot_id": source_snapshot_id,
        "archive_url": archive_url,
        "archive_digest": {
            "algorithm": "sha256",
            "value": archive_hash,
            "representation": "registry-hex" if archive_hash and archive_hash.startswith("sha256:") else "nix-sri",
        }
        if archive_hash
        else None,
        "fields": {
            "aliases": tri_state("unknown"),
            "keywords": tri_state("unknown"),
            "description": tri_state("unknown"),
            "readme": tri_state("unknown"),
            "dependencies": tri_state("unknown"),
            "advisories": tri_state("unknown"),
            "yanked": tri_state("unknown"),
        },
    }
    if forge_source_coordinate is not None:
        document["forge_source_coordinate"] = forge_source_coordinate
    return document


def forge_coordinate_from_archive_url(archive_url: str) -> str:
    parsed = urlsplit(archive_url)
    if parsed.scheme != "https" or parsed.netloc.lower() != "github.com":
        raise ValueError(f"unsupported pinned forge archive URL: {archive_url}")
    parts = [part for part in parsed.path.split("/") if part]
    if len(parts) < 4 or parts[2] != "archive":
        raise ValueError(f"pinned GitHub archive URL has no repository ref: {archive_url}")
    ref_parts = parts[3:]
    if ref_parts[:2] == ["refs", "tags"]:
        ref_parts = ref_parts[2:]
    ref = "/".join(ref_parts)
    for suffix in (".tar.gz", ".tgz", ".zip"):
        if ref.endswith(suffix):
            ref = ref[: -len(suffix)]
            break
    if not ref:
        raise ValueError(f"pinned GitHub archive URL has an empty ref: {archive_url}")
    owner, repository = parts[0], parts[1]
    return f"https://github.com/{owner}/{repository}@tag:{ref}"


def cargo_lock_packages() -> tuple[dict[tuple[str, str], dict[str, Any]], str]:
    lock_path = ROOT / CARGO_LOCK_SNAPSHOT
    lock_digest = file_sha256(lock_path)
    lock = tomllib.loads(lock_path.read_text(encoding="utf-8"))
    packages = {(row["name"], row["version"]): row for row in lock["package"]}
    return packages, f"cargo-lock-snapshot:sha256:{lock_digest}"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true", help="verify generated files without rewriting")
    parser.add_argument("--write", action="store_true", help="write the frozen primary corpus and source manifest")
    args = parser.parse_args()
    if args.check == args.write:
        parser.error("choose exactly one of --check or --write")

    pins_path = ROOT / ".config/nix/corpus-pins.json"
    pins_bytes = pins_path.read_bytes()
    pins_digest = sha256(pins_bytes)
    pin_rows = read_json(pins_path)
    index_rows, index_snapshots = parse_index_snapshots()
    lock_packages, lock_snapshot_id = cargo_lock_packages()
    rustsec_path = ROOT / "crates/advisory/fixtures/rustsec/crates/bincode/RUSTSEC-2025-0141.md"
    rustsec_snapshot_id = f"workspace-file:sha256:{file_sha256(rustsec_path)}"

    docs: list[dict[str, Any]] = []
    found_pins = set()
    for row_number, pin in enumerate(pin_rows):
        coordinate = pin["coordinate"]
        if coordinate not in PIN_COORDINATES and coordinate not in FORGE_COORDINATES:
            continue
        ecosystem, name, version = package_name_from_pin(coordinate)
        if ecosystem == "forge":
            version_match = re.search(r"(?<!\d)(\d+\.\d+(?:\.\d+){0,2})(?!\d)", pin["url"].rsplit("/", 1)[-1])
            version = version_match.group(1) if version_match else None
            document_id = f"forge:clang/{name}@{version or pin['hash']}"
            source_kind = "forge"
            forge_source_coordinate = forge_coordinate_from_archive_url(pin["url"])
        else:
            document_id = coordinate
            source_kind = "registry"
            found_pins.add(coordinate)
            forge_source_coordinate = None
        pin_snapshot_id = f"nix-corpus-pins:sha256:{pins_digest}#row:{row_number}"
        doc = base_doc(
            document_id,
            ecosystem,
            name,
            version,
            source_kind,
            pin_snapshot_id,
            coordinate,
            pin["url"],
            pin["hash"],
            forge_source_coordinate,
        )
        if ecosystem == "cargo" and version and (name, version) in lock_packages:
            locked = lock_packages[(name, version)]
            doc["fields"]["dependencies"] = tri_state(
                "known",
                locked.get("dependencies", []),
                lock_snapshot_id,
            )
        if name == "bincode":
            doc["fields"]["advisories"] = tri_state(
                "known",
                ["RUSTSEC-2025-0141"],
                rustsec_snapshot_id,
            )
        index_record = index_rows.get((name, version or ""))
        if index_record:
            doc["fields"]["yanked"] = tri_state(
                "known",
                [bool(index_record["row"].get("yanked", False))],
                index_record["snapshot_id"],
            )
            doc["registry_index_line_sha256"] = index_record["raw_line_sha256"]
            doc["registry_index_snapshot_id"] = index_record["snapshot_id"]
        docs.append(doc)

    missing = PIN_COORDINATES - found_pins
    if missing:
        raise ValueError("selected coordinates absent from corpus pins: " + ", ".join(sorted(missing)))

    for name, version in [("serde", "1.0.31"), ("serde", "1.0.95"), ("bincode", "1.3.0"), ("bincode", "1.3.3")]:
        entry = index_rows[(name, version)]
        row = entry["row"]
        coordinate = f"cargo:{name}@{version}"
        document_id = coordinate
        doc = base_doc(
            document_id,
            "cargo",
            name,
            version,
            "registry",
            entry["snapshot_id"],
            coordinate,
            f"https://static.crates.io/crates/{name}/{name}-{version}.crate",
            "sha256:" + row["cksum"],
        )
        doc["fields"]["yanked"] = tri_state("known", [bool(row.get("yanked", False))], entry["snapshot_id"])
        doc["registry_index_line_sha256"] = entry["raw_line_sha256"]
        doc["registry_index_snapshot_id"] = entry["snapshot_id"]
        if (name, version) in lock_packages:
            doc["fields"]["dependencies"] = tri_state(
                "known",
                lock_packages[(name, version)].get("dependencies", []),
                lock_snapshot_id,
            )
        if name == "bincode":
            doc["fields"]["advisories"] = tri_state(
                "known",
                ["RUSTSEC-2025-0141"],
                rustsec_snapshot_id,
            )
        docs.append(doc)

    docs.sort(key=lambda item: item["document_id"])
    source_doc = source_manifest(index_snapshots)
    corpus_text = "".join(json.dumps(doc, sort_keys=True, separators=(",", ":")) + "\n" for doc in docs)
    source_text = json.dumps(source_doc, indent=2, sort_keys=True) + "\n"
    corpus_path = HERE / "primary-corpus.jsonl"
    sources_path = HERE / "source-snapshots.json"
    if args.check:
        if not corpus_path.exists() or corpus_path.read_text(encoding="utf-8") != corpus_text:
            print("primary-corpus.jsonl is not the deterministic frozen output", file=sys.stderr)
            return 1
        if not sources_path.exists() or sources_path.read_text(encoding="utf-8") != source_text:
            print("source-snapshots.json is not the deterministic frozen output", file=sys.stderr)
            return 1
        print(f"verified {len(docs)} primary documents; source snapshot {source_doc['benchmark_snapshot_id']}")
        return 0

    corpus_path.write_text(corpus_text, encoding="utf-8")
    sources_path.write_text(source_text, encoding="utf-8")
    print(f"wrote {len(docs)} primary documents and {len(source_doc['source_snapshots'])} source snapshot records")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
