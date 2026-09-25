#!/usr/bin/env python3
"""Download the published tarball of 100 widely used npm packages.

Writes /tmp/npm-corpus/manifest.json and one extracted directory per package.
Versions are the registry's `latest` dist-tag at download time.
"""

from __future__ import annotations

import json
import tarfile
import urllib.request
from pathlib import Path

ROOT = Path("/tmp/npm-corpus")

# A mix of frameworks, libraries, type packages, and tiny utilities that
# TypeScript agents actually open. Order is roughly "how often an agent
# reaches for it", not a download-count ranking.
PACKAGES = [
    "next",
    "react",
    "react-dom",
    "typescript",
    "vue",
    "@angular/core",
    "rxjs",
    "zod",
    "express",
    "axios",
    "lodash",
    "lodash-es",
    "date-fns",
    "dayjs",
    "moment",
    "immer",
    "zustand",
    "jotai",
    "redux",
    "@reduxjs/toolkit",
    "react-redux",
    "@tanstack/react-query",
    "swr",
    "vite",
    "vitest",
    "esbuild",
    "rollup",
    "webpack",
    "prettier",
    "eslint",
    "jest",
    "mocha",
    "ts-node",
    "tsx",
    "tsup",
    "@types/node",
    "@types/react",
    "@types/react-dom",
    "@types/express",
    "@types/lodash",
    "type-fest",
    "fp-ts",
    "ramda",
    "commander",
    "yargs",
    "inquirer",
    "chalk",
    "picocolors",
    "debug",
    "ms",
    "uuid",
    "nanoid",
    "clsx",
    "semver",
    "glob",
    "fs-extra",
    "dotenv",
    "minimist",
    "ws",
    "socket.io",
    "graphql",
    "@apollo/client",
    "@nestjs/common",
    "@nestjs/core",
    "fastify",
    "koa",
    "hono",
    "@trpc/server",
    "@trpc/client",
    "yup",
    "joi",
    "class-validator",
    "class-transformer",
    "reflect-metadata",
    "mongoose",
    "typeorm",
    "pg",
    "ioredis",
    "pino",
    "winston",
    "execa",
    "got",
    "node-fetch",
    "undici",
    "ky",
    "cors",
    "helmet",
    "cookie",
    "qs",
    "path-to-regexp",
    "mime",
    "left-pad",
    "p-limit",
    "tslib",
    "svelte",
    "solid-js",
    "preact",
    "three",
    "d3",
    "openai",
]


def safe_dir(name: str, version: str) -> str:
    return f"{name.replace('/', '__')}-{version}"


def fetch_meta(name: str) -> dict:
    url = "https://registry.npmjs.org/" + name.replace("/", "%2f")
    with urllib.request.urlopen(url, timeout=60) as resp:
        packument = json.load(resp)
    version = packument["dist-tags"]["latest"]
    meta = packument["versions"][version]
    return {
        "name": name,
        "version": version,
        "tarball": meta["dist"]["tarball"],
        "dir": safe_dir(name, version),
    }


def flatten_single_root(dest: Path) -> None:
    """Lift a tarball that nests the package under one directory.

    npm's own packager uses `package/`. DefinitelyTyped publishes `@types/*`
    under the unscoped name (`node/`, `react/`) instead, so a flattener that
    only knows the `package/` spelling leaves `package.json` one level down
    and the producer never sees a manifest.
    """
    if (dest / "package.json").is_file():
        return
    children = [child for child in dest.iterdir() if child.name != ".DS_Store"]
    if len(children) != 1 or not children[0].is_dir():
        return
    nested = children[0]
    if not (nested / "package.json").is_file():
        return
    for child in nested.iterdir():
        child.rename(dest / child.name)
    nested.rmdir()


def extract(meta: dict) -> None:
    dest = ROOT / meta["dir"]
    if (dest / "package.json").is_file():
        print(f"have {meta['name']}@{meta['version']}")
        return
    if dest.exists():
        # A previous extract left a single nested root. Flatten in place
        # before paying for another download.
        flatten_single_root(dest)
        if (dest / "package.json").is_file():
            print(f"flat {meta['name']}@{meta['version']}")
            return
    dest.mkdir(parents=True, exist_ok=True)
    print(f"get  {meta['name']}@{meta['version']}")
    with urllib.request.urlopen(meta["tarball"], timeout=180) as resp:
        blob = resp.read()
    tar_path = ROOT / (meta["dir"] + ".tgz")
    tar_path.write_bytes(blob)
    with tarfile.open(tar_path) as tar:
        tar.extractall(dest, filter="data")
    flatten_single_root(dest)
    tar_path.unlink(missing_ok=True)


def main() -> None:
    assert len(PACKAGES) == 100, len(PACKAGES)
    ROOT.mkdir(parents=True, exist_ok=True)
    manifest = []
    for name in PACKAGES:
        meta = fetch_meta(name)
        extract(meta)
        manifest.append({k: meta[k] for k in ("name", "version", "dir")})
        (ROOT / "manifest.json").write_text(json.dumps(manifest, indent=2))
    print(f"manifest {len(manifest)} -> {ROOT / 'manifest.json'}")


if __name__ == "__main__":
    main()
