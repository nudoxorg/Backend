#!/usr/bin/env python3
"""Fail-closed local resource preflight for the bounded T1 native build."""

from __future__ import annotations

import argparse
import shutil
import sys
from pathlib import Path


GIB = 1024**3
# Match the workspace's native admission floors; T1 always caps Cargo at two jobs.
MIN_AVAILABLE_MEMORY_BYTES = 8 * GIB
MIN_FREE_DISK_BYTES = 16 * GIB


class CapacityError(ValueError):
    """The host cannot safely admit the T1 build, or its state is unknown."""


def parse_mem_available(meminfo: str) -> int:
    """Read one unambiguous Linux MemAvailable value and return bytes."""
    values: list[int] = []
    for line in meminfo.splitlines():
        if line.startswith("MemAvailable:"):
            fields = line.split()
            if len(fields) != 3 or fields[0] != "MemAvailable:" or fields[2] != "kB":
                raise CapacityError("/proc/meminfo has a malformed MemAvailable value")
            try:
                value = int(fields[1])
            except ValueError as error:
                raise CapacityError("/proc/meminfo has a nonnumeric MemAvailable value") from error
            if value < 0:
                raise CapacityError("/proc/meminfo has a negative MemAvailable value")
            values.append(value * 1024)
    if len(values) != 1:
        raise CapacityError("/proc/meminfo must contain exactly one MemAvailable value")
    return values[0]


def existing_directory(path: Path) -> Path:
    """Find the filesystem ancestor where a not-yet-created target will live."""
    candidate = path
    while not candidate.exists():
        parent = candidate.parent
        if parent == candidate:
            raise CapacityError("cannot resolve a filesystem for the T1 target directory")
        candidate = parent
    if not candidate.is_dir():
        raise CapacityError("the T1 target path is not a directory")
    return candidate


def validate_cache_separation(
    t1_target_dir: Path,
    fast_cache_root: str,
    t1_cache_root: str = "",
) -> None:
    """Keep T1's target and cache outside the fast lane's eviction subtree."""
    if not fast_cache_root:
        return
    try:
        fast = Path(fast_cache_root).resolve(strict=False)
        t1_paths = [t1_target_dir.resolve(strict=False)]
        if t1_cache_root:
            t1_paths.append(Path(t1_cache_root).resolve(strict=False))
    except (OSError, RuntimeError) as error:
        raise CapacityError(f"cannot resolve configured cache roots: {error}") from error
    for t1 in t1_paths:
        if t1 == fast or t1 in fast.parents or fast in t1.parents:
            raise CapacityError("the T1 target/cache and CI_FAST_CACHE_ROOT must be disjoint trees")


def validate_capacity(free_disk_bytes: int, available_memory_bytes: int) -> None:
    if type(free_disk_bytes) is not int or free_disk_bytes < 0:
        raise CapacityError("disk availability sample is invalid")
    if type(available_memory_bytes) is not int or available_memory_bytes < 0:
        raise CapacityError("memory availability sample is invalid")
    if free_disk_bytes < MIN_FREE_DISK_BYTES:
        raise CapacityError(
            f"disk: only {free_disk_bytes} bytes are free; T1 requires at least {MIN_FREE_DISK_BYTES}"
        )
    if available_memory_bytes < MIN_AVAILABLE_MEMORY_BYTES:
        raise CapacityError(
            f"memory: only {available_memory_bytes} bytes are available; "
            f"T1 requires at least {MIN_AVAILABLE_MEMORY_BYTES}"
        )


def sample_capacity(target_dir: Path, meminfo_path: Path = Path("/proc/meminfo")) -> tuple[int, int]:
    try:
        available_memory_bytes = parse_mem_available(meminfo_path.read_text(encoding="ascii"))
        free_disk_bytes = shutil.disk_usage(existing_directory(target_dir)).free
    except (OSError, UnicodeError) as error:
        raise CapacityError(f"unable to sample T1 host capacity: {error}") from error
    validate_capacity(free_disk_bytes, available_memory_bytes)
    return free_disk_bytes, available_memory_bytes


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("target_dir", type=Path)
    parser.add_argument("--fast-cache-root", default="")
    parser.add_argument("--t1-cache-root", default="")
    parser.add_argument("--separation-only", action="store_true")
    args = parser.parse_args()
    try:
        validate_cache_separation(args.target_dir, args.fast_cache_root, args.t1_cache_root)
        if args.separation_only:
            print("t1 cache: target and cache roots are disjoint")
            return 0
        free_disk_bytes, available_memory_bytes = sample_capacity(args.target_dir)
    except CapacityError as error:
        print(f"t1 capacity: deferred: {error}", file=sys.stderr)
        return 75
    print(
        "t1 capacity: admitted "
        f"free_disk_bytes={free_disk_bytes} available_memory_bytes={available_memory_bytes}"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
