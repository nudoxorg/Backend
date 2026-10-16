#!/usr/bin/env python3
"""Evict an owned CI accelerator without removing its mounted root."""
import os
import shutil
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


if __name__ == '__main__':
    clear_cache(sys.argv[1])
