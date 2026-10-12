#!/usr/bin/env python3
"""Derive a diagnostic CLI archive from an already finalized SDK-only application.

No build, signing, probe or publication occurs here. The unchanged .app layout is
required by runtime SDK discovery; CLI aliases are installed into Contents/MacOS.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import stat
import tarfile

import install_macos_preview as installer


def derive(app: Path, output: Path, source: str):
    if not re.fullmatch('[a-f0-9]{40}', source):
        raise installer.InstallError('invalid selected clean source revision')
    app = Path(app)
    if app.name != 'Nudox.app' or app.is_symlink():
        raise installer.InstallError('selected app must be a real Nudox.app directory')
    root = app.parent
    records = {}
    total_bytes = 0
    total_entries = 0
    # The installer reads through owned, non-writable ancestors and verifies each
    # opened object; this derivation admits the same bytes that will be transported.
    for directory, dirs, files in os.walk(app, followlinks=False):
        total_entries += len(dirs)+len(files)
        if total_entries > 65536:
            raise installer.InstallError('SDK application exceeds finite entry inventory')
        for name in dirs:
            installer._validate_install_directory(Path(directory)/name)
        for name in sorted(files):
            path = Path(directory)/name
            relative = path.relative_to(root).as_posix()
            _, digest = installer._verified_package_file(root, relative, max_bytes=min(installer.MAX_MEMBER_BYTES, installer.MAX_ARCHIVE_BYTES-total_bytes))
            metadata = path.lstat()
            total_bytes += metadata.st_size
            records[relative] = {'sha256': digest, 'size_bytes': metadata.st_size,
                                 'mode': stat.S_IMODE(metadata.st_mode)}
            if len(records) > 65536:
                raise installer.InstallError('SDK application exceeds finite inventory')
    records = dict(sorted(records.items()))
    package = {'schema': installer.SDK_SCHEMA, 'source': source, 'production_ready': False,
               'binary_directory': installer.SDK_BIN,
               'build_manifest_sha256': records[installer.SDK_MANIFEST]['sha256'], 'files': records}
    # Does not execute fixture/probe bytes: completed native admission must have
    # been recorded by the existing post-sign SDK finalizer before this step.
    installer.verify_sdk_package(root, package, source)
    manifest_raw, manifest_digest = installer._verified_package_file(root, installer.SDK_MANIFEST, read_bytes=True, max_bytes=installer.MAX_PACKAGE_METADATA_BYTES)
    if manifest_digest != package['build_manifest_sha256']:
        raise installer.InstallError('SDK manifest changed after admission')
    selected_origin = json.loads(manifest_raw)['source']
    output = Path(output)
    installer._validate_install_directory(output.parent)
    output.mkdir(mode=0o700, parents=False, exist_ok=False)
    installer._validate_install_directory(output)
    asset = 'nudox-macos-arm64-'+source[:10]+'.tar.gz'
    archive = output/asset
    package_raw = (json.dumps(package, sort_keys=True, indent=2)+'\n').encode()
    if len(package_raw) > installer.MAX_PACKAGE_METADATA_BYTES:
        raise installer.InstallError('SDK transport manifest exceeds metadata bound')
    import io
    with tarfile.open(archive, 'w:gz', dereference=False) as target:
        metadata = tarfile.TarInfo('nudox-macos-arm64/manifest.json')
        metadata.size = len(package_raw); metadata.mode = 0o644
        target.addfile(metadata, io.BytesIO(package_raw))
        for name in records:
            # Reopen/recheck before streaming: tar never follows a replacement link.
            record = records[name]
            _, digest = installer._verified_package_file(root, name, max_bytes=record['size_bytes'])
            descriptor = os.open(root/name, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
            with os.fdopen(descriptor, 'rb') as stream:
                before = os.fstat(stream.fileno())
                if (not stat.S_ISREG(before.st_mode) or digest != record['sha256']
                        or before.st_size != record['size_bytes'] or stat.S_IMODE(before.st_mode) != record['mode']):
                    raise installer.InstallError('selected SDK application changed during packaging')
                member = tarfile.TarInfo('nudox-macos-arm64/'+name)
                member.mode = record['mode']; member.size = before.st_size
                target.addfile(member, stream)
                if installer._file_identity(before) != installer._file_identity(os.fstat(stream.fileno())) or installer._file_identity(before) != installer._file_identity((root/name).lstat()):
                    raise installer.InstallError('selected SDK application changed while streaming')
            if installer._verified_package_file(root, name, max_bytes=record['size_bytes'])[1] != digest:
                raise installer.InstallError('selected SDK bytes changed while streaming')
    with tarfile.open(archive, 'r:gz') as check:
        for member in check:
            relative = installer._safe_member(member, 'nudox-macos-arm64')
            if relative in records:
                stream = check.extractfile(member)
                actual = hashlib.sha256()
                while chunk := stream.read(1024*1024): actual.update(chunk)
                if actual.hexdigest() != records[relative]['sha256']:
                    raise installer.InstallError('transport bytes differ from selected SDK application')
    digest = hashlib.sha256()
    with archive.open('rb') as stream:
        while chunk := stream.read(1024*1024): digest.update(chunk)
    if archive.stat().st_size > installer.MAX_ARCHIVE_BYTES:
        raise installer.InstallError('SDK archive exceeds installer bound')
    release = {'schema': 1, 'platform': 'macos-arm64', 'source_sha': source,
               'asset': asset, 'sha256': digest.hexdigest(), 'size_bytes': archive.stat().st_size,
               'source_tree': selected_origin['git_tree'],
               'package_schema': installer.SDK_SCHEMA,
               'build_manifest_sha256': package['build_manifest_sha256'], 'production_ready': False}
    (output/'release-manifest.json').write_text(json.dumps(release, sort_keys=True, indent=2)+'\n')
    (output/(asset+'.sha256')).write_text(digest.hexdigest()+'  '+asset+'\n')
    (output/'install-macos-arm64.py').write_bytes(Path(installer.__file__).read_bytes())
    return release


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--app', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--source', required=True)
    args = parser.parse_args()
    print(json.dumps(derive(args.app, args.output, args.source), sort_keys=True))


if __name__ == '__main__':
    main()
