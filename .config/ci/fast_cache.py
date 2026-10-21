#!/usr/bin/env python3
"""Evict an owned CI accelerator without removing its mounted root."""
import os
import shutil
import stat
import sys


def clear_cache(path):
    descriptor = os.open(path, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    try:
        metadata = os.fstat(descriptor)
        filesystem_root = os.stat('/')
        if (metadata.st_dev, metadata.st_ino) == (filesystem_root.st_dev, filesystem_root.st_ino):
            raise ValueError('refusing to evict the filesystem root')
        # Pin the directory while removing children. In Concourse this is a
        # mount point: unlinking the root itself fails with EBUSY. Hidden
        # files count too, and symlinks must never lead outside the cache.
        with os.scandir(descriptor) as entries:
            for entry in entries:
                if entry.is_dir(follow_symlinks=False):
                    shutil.rmtree(entry.name, dir_fd=descriptor)
                else:
                    os.unlink(entry.name, dir_fd=descriptor)
    finally:
        os.close(descriptor)


def ensure_cargo_home(cache_root, cargo_home):
    """Admit only the exact Cargo-home child of the mounted fast cache.

    Cargo creates files inside CARGO_HOME but does not create its parent. An
    unexpectedly absent child therefore makes Cargo's package-cache lock fail
    with ENOENT before useful work. This repairs only that direct child under
    the configured cache root; it never clears or follows a path.
    """
    root = os.fspath(cache_root)
    home = os.fspath(cargo_home)
    if not os.path.isabs(root) or not os.path.isabs(home):
        raise ValueError('fast cache and Cargo home must be absolute paths')
    if os.path.normpath(home) != os.path.join(os.path.normpath(root), 'cargo-home'):
        raise ValueError('Cargo home is not the exact fast-cache child')

    directory = os.open(root, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    try:
        root_stat = os.fstat(directory)
        if not stat.S_ISDIR(root_stat.st_mode):
            raise ValueError('fast cache root is not a directory')
        filesystem_root = os.stat('/')
        if (root_stat.st_dev, root_stat.st_ino) == (
            filesystem_root.st_dev, filesystem_root.st_ino
        ):
            raise ValueError('refusing to create Cargo home under the filesystem root')
        try:
            before = os.stat('cargo-home', dir_fd=directory, follow_symlinks=False)
            created = False
        except FileNotFoundError:
            os.mkdir('cargo-home', 0o700, dir_fd=directory)
            before = os.stat('cargo-home', dir_fd=directory, follow_symlinks=False)
            created = True
        if not stat.S_ISDIR(before.st_mode):
            raise ValueError('Cargo home is not a real directory')

        child = os.open(
            'cargo-home', os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=directory
        )
        try:
            home_stat = os.fstat(child)
            after = os.stat('cargo-home', dir_fd=directory, follow_symlinks=False)
            if (before.st_dev, before.st_ino) != (home_stat.st_dev, home_stat.st_ino):
                raise ValueError('Cargo home changed while being admitted')
            if (after.st_dev, after.st_ino) != (home_stat.st_dev, home_stat.st_ino):
                raise ValueError('Cargo home name changed while being admitted')
            if home_stat.st_dev != root_stat.st_dev:
                raise ValueError('Cargo home is mounted outside the fast cache')
        finally:
            os.close(child)
        named_root = os.stat(root, follow_symlinks=False)
        if (named_root.st_dev, named_root.st_ino) != (root_stat.st_dev, root_stat.st_ino):
            raise ValueError('fast cache root changed while Cargo home was admitted')
    finally:
        os.close(directory)
    print(
        'CI-FAST-CARGO-HOME '
        f'state=ready created={str(created).lower()} '
        f'root_dev={root_stat.st_dev} root_ino={root_stat.st_ino} '
        f'home_dev={home_stat.st_dev} home_ino={home_stat.st_ino}'
    )


def observe_cargo_home_parent(cargo_home):
    """Best-effort diagnostics when no fast-cache root was configured.

    This branch must not change Cargo's existing behavior: unlike the
    configured-cache guard, observations here are never admission checks.
    """
    home = os.fspath(cargo_home)
    if not home:
        print(
            'CI-FAST-CARGO-HOME state=skipped reason=cache-root-unset '
            'cargo_home=unset cache_parent=unavailable home_parent=unavailable'
        )
        return
    if not os.path.isabs(home):
        print(
            'CI-FAST-CARGO-HOME state=skipped reason=cache-root-unset '
            'cargo_home=relative cache_parent=unavailable home_parent=unavailable'
        )
        return

    def directory_status(path):
        try:
            metadata = os.stat(path, follow_symlinks=False)
        except FileNotFoundError:
            return 'absent'
        except OSError as error:
            return f'unavailable_errno_{error.errno}'
        if not stat.S_ISDIR(metadata.st_mode):
            return 'not_directory'
        return f'present_dev_{metadata.st_dev}_ino_{metadata.st_ino}'

    normalized = os.path.normpath(home)
    cache_parent = os.path.dirname(normalized)
    home_parent = normalized
    print(
        'CI-FAST-CARGO-HOME state=skipped reason=cache-root-unset '
        f'cargo_home=absolute cache_parent={directory_status(cache_parent)} '
        f'home_parent={directory_status(home_parent)}'
    )


if __name__ == '__main__':
    if len(sys.argv) == 4 and sys.argv[1] == '--ensure-cargo-home':
        ensure_cargo_home(sys.argv[2], sys.argv[3])
    elif len(sys.argv) == 3 and sys.argv[1] == '--observe-cargo-home-parent':
        observe_cargo_home_parent(sys.argv[2])
    elif len(sys.argv) == 2:
        clear_cache(sys.argv[1])
    else:
        raise SystemExit('usage: fast_cache.py CACHE_ROOT | --ensure-cargo-home CACHE_ROOT CARGO_HOME')
