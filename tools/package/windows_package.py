#!/usr/bin/env python3
"""Package a source-bound unsigned Windows preview or native candidate.

Static PE imports are resolved against the Windows system folder and explicit
dependency folders. A candidate still requires native QA and release CI; this
script does not publish or claim verification of dynamically loaded libraries.
"""
from __future__ import annotations
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import struct
import subprocess
import sys
import tomllib
import zipfile

from windows_build import sqlite_basis

EXPECTED = {name + '.exe' for name in ('backend-cli', 'backend-mcp', 'backend-locald',
            'backend-desktop', 'backend-metrics', 'backend-stylo-worker', 'backend-sqlite-worker')}
SAFE_DLL = re.compile(r'^[A-Za-z0-9_.+-]+\.dll$', re.I)
# These Visual C++ redistributables can be installed on the build host but
# are not part of a clean Windows installation. Package them explicitly.
VC_RUNTIME = re.compile(r'^(?:vcruntime|msvcp|concrt|vcomp)[0-9].*\.dll$', re.I)


def read_file(path):
    if path.is_symlink() or not path.is_file():
        raise ValueError(f'expected a regular file: {path}')
    return path.read_bytes()


def digest(data):
    return hashlib.sha256(data).hexdigest()


def file_digest(path):
    if path.is_symlink() or not path.is_file() or not 0 < path.stat().st_size <= 512 * 1024 * 1024:
        raise ValueError(f'not a bounded regular image: {path}')
    h = hashlib.sha256()
    with path.open('rb') as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b''):
            h.update(chunk)
    return h.hexdigest()


def require_amd64_pe(path):
    image_hash = file_digest(path)
    with path.open('rb') as stream:
        header = stream.read(64)
        if len(header) < 64 or header[:2] != b'MZ':
            raise ValueError(f'not a PE image: {path}')
        offset = struct.unpack_from('<I', header, 0x3c)[0]
        if offset + 24 > path.stat().st_size:
            raise ValueError(f'invalid PE header: {path}')
        stream.seek(offset)
        pe = stream.read(24)
    if pe[:4] != b'PE\0\0' or struct.unpack_from('<H', pe, 4)[0] != 0x8664:
        raise ValueError(f'not an x64 PE image: {path}')
    return image_hash


def pe_import_directories(path):
    """Read PE32+ import directory presence independently of the LLVM output."""
    with path.open('rb') as stream:
        dos = stream.read(64)
        if len(dos) != 64 or dos[:2] != b'MZ':
            raise ValueError('invalid DOS header')
        offset = struct.unpack_from('<I', dos, 0x3c)[0]
        if offset + 24 > path.stat().st_size:
            raise ValueError('invalid PE offset')
        stream.seek(offset)
        header = stream.read(24)
        if len(header) != 24 or header[:4] != b'PE\0\0' or struct.unpack_from('<H', header, 4)[0] != 0x8664:
            raise ValueError('invalid x64 PE header')
        size = struct.unpack_from('<H', header, 20)[0]
        if size < 112 or offset + 24 + size > path.stat().st_size:
            raise ValueError('invalid PE optional header size')
        optional = stream.read(size)
    if len(optional) != size or struct.unpack_from('<H', optional, 0)[0] != 0x20b:
        raise ValueError('expected PE32+ optional header')
    count = struct.unpack_from('<I', optional, 108)[0]
    if count < 14 or count > 128 or 112 + count * 8 > size:
        raise ValueError('incomplete PE data directory')
    result = []
    for index in (1, 13):
        address, length = struct.unpack_from('<II', optional, 112 + index * 8)
        if bool(address) != bool(length):
            raise ValueError('malformed PE import directory')
        result.append(bool(address))
    return tuple(result)


def parse_coff_imports(text, directories):
    """Read only complete top-level COFF import records, including delay imports."""
    records = {'Import': [], 'DelayImport': []}
    expected_headers = {'Format: COFF-x86-64', 'Arch: x86_64', 'AddressSize: 64bit'}
    headers = set()
    kind = None
    depth = 0
    name = None
    for line in text.splitlines():
        stripped = line.strip()
        if kind is None:
            if not stripped or stripped.startswith('File: '):
                continue
            if stripped in expected_headers:
                if stripped in headers:
                    raise ValueError('duplicate COFF reader header')
                headers.add(stripped)
                continue
            if stripped in ('Import {', 'DelayImport {'):
                kind = stripped.split()[0]
                depth = 1
                name = None
                continue
            raise ValueError('unrecognized COFF reader output')
        if stripped.endswith('{'):
            depth += 1
        elif stripped == '}':
            depth -= 1
            if depth == 0:
                if name is None:
                    raise ValueError('COFF import lacks a DLL name')
                records[kind].append(name)
                kind = None
        elif depth == 1 and re.match(r'^Name\b', stripped):
            match = re.fullmatch(r'Name:\s*(\S+)', stripped)
            if name is not None or match is None or not SAFE_DLL.fullmatch(match.group(1)):
                raise ValueError('invalid COFF DLL import name')
            name = match.group(1)
    if kind is not None or depth != 0 or headers != expected_headers:
        raise ValueError('incomplete COFF reader output')
    if tuple(bool(records[kind]) for kind in ('Import', 'DelayImport')) != directories:
        raise ValueError('COFF imports do not match PE directories')
    if not records['Import'] and not records['DelayImport']:
        raise ValueError('PE image has no reported imports')
    return records['Import'] + records['DelayImport']


def index_folder(folder):
    result = {}
    for item in folder.iterdir():
        if item.name.lower().endswith('.dll'):
            key = item.name.casefold()
            if key in result:
                raise ValueError(f'ambiguous DLL spelling: {item.name}')
            result[key] = item
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--build', required=True, type=Path)
    parser.add_argument('--source', required=True, type=Path)
    parser.add_argument('--output', required=True, type=Path)
    parser.add_argument('--objdump', required=True, type=Path)
    parser.add_argument('--system-dll-dir', required=True, type=Path)
    parser.add_argument('--dll-dir', type=Path, action='append', default=[])
    parser.add_argument('--candidate', action='store_true', help='stage an unsigned release candidate on native Windows')
    parser.add_argument('--minimum-os', help='supported Windows minimum, to be verified in native QA')
    args = parser.parse_args()
    build, source, output = (p.resolve() for p in (args.build, args.source, args.output))
    expected_name = 'nudox-windows-x64.zip' if args.candidate else 'nudox-windows-x64-unsigned-preview.zip'
    if output.exists() or output.name != expected_name:
        raise ValueError('output must be new and use the expected asset name')
    if args.candidate:
        if os.name != 'nt' or not args.minimum_os or not re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9 ._+-]{0,63}', args.minimum_os):
            raise ValueError('candidate requires native Windows and a stated minimum OS')
        system_root = os.environ.get('SystemRoot')
        if not system_root or os.path.normcase(str(args.system_dll_dir.resolve())) != os.path.normcase(str((Path(system_root) / 'System32').resolve())):
            raise ValueError('candidate system DLL directory must be the native Windows System32')
        if (output.parent / 'release-manifest.json').exists() or (output.parent / (output.name + '.sha256')).exists():
            raise ValueError('candidate metadata must not already exist')
    if source in output.parents or build in output.parents:
        raise ValueError('output must be outside source and build directories')
    receipt = json.loads(read_file(build / 'build-receipt.json'))
    if receipt.get('schema') != 'nudox.windows-unsigned-preview-build.v1' or receipt.get('target') != 'x86_64-pc-windows-msvc' or receipt.get('profile') != 'release' or receipt.get('signed') is not False:
        raise ValueError('expected a native unsigned Windows preview build receipt')
    records = receipt.get('executables')
    if not isinstance(records, dict) or set(records) != EXPECTED:
        raise ValueError('build receipt omits or adds executable roles')
    workers = receipt.get('workers')
    if (not isinstance(workers, dict) or set(workers) != {'stylo', 'sqlite'}
            or any(not isinstance(value, str) or not re.fullmatch(r'[0-9a-f]{64}', value) or value == '0' * 64 for value in workers.values())):
        raise ValueError('build receipt has invalid worker identities')
    sqlite_basis({'state': 'observed-build-helper', 'artifact_identity': workers['sqlite'],
                  'basis': receipt.get('sqlite_basis')}, hashlib.sha256(read_file(source / 'frontends/sqlite/Cargo.lock')).hexdigest())
    revision = subprocess.check_output(['git', '-c', 'core.autocrlf=false', 'rev-parse', 'HEAD'], cwd=source, text=True).strip()
    tree = subprocess.check_output(['git', '-c', 'core.autocrlf=false', 'rev-parse', 'HEAD^{tree}'], cwd=source, text=True).strip()
    if subprocess.check_output(['git', '-c', 'core.autocrlf=false', 'status', '--porcelain', '--untracked-files=all'], cwd=source, text=True).strip():
        raise ValueError('source checkout must be clean')
    source_info = {'git_revision': revision, 'git_tree': tree,
                   'cargo_lock_sha256': digest(read_file(source / 'Cargo.lock'))}
    for path in ('Cargo.lock', 'frontends/sqlite/Cargo.lock'):
        blob = subprocess.check_output(['git', '-c', 'core.autocrlf=false', 'show', revision + ':' + path], cwd=source)
        if digest(blob) != digest(read_file(source / path)):
            raise ValueError('checkout lock bytes differ from Git source')
    if receipt.get('source') != source_info:
        raise ValueError('build receipt differs from source checkout')
    system = index_folder(args.system_dll_dir.resolve(strict=True))
    folders = [index_folder(p.resolve(strict=True)) for p in args.dll_dir]
    readobj = args.objdump.resolve(strict=True).with_name('llvm-readobj.exe' if os.name == 'nt' else 'llvm-readobj')
    if readobj.is_symlink() or not readobj.is_file():
        raise ValueError('LLVM COFF import reader is unavailable beside objdump')
    images = {}
    queue = []
    imports_by_image = {}
    for name in sorted(EXPECTED):
        path = build / 'bin' / name
        image_hash = require_amd64_pe(path)
        if records[name] != {'bytes': path.stat().st_size, 'sha256': image_hash}:
            raise ValueError(f'build executable differs from receipt: {name}')
        images[name] = (path, image_hash)
        queue.append((name, path))
    while queue:
        image_name, image = queue.pop()
        directories = pe_import_directories(image)
        text = subprocess.check_output([str(readobj), '--coff-imports', str(image)], text=True, errors='strict')
        imports = parse_coff_imports(text, directories)
        imports_by_image[image_name] = sorted(set(imports), key=str.casefold)
        for name in imports:
            if not SAFE_DLL.fullmatch(name):
                raise ValueError(f'unsafe DLL import: {name}')
            key = name.casefold()
            if not VC_RUNTIME.fullmatch(name) and (key.startswith(('api-ms-win-', 'ext-ms-win-')) or key in system):
                continue
            matches = [folder[key] for folder in folders if key in folder]
            if len(matches) != 1:
                raise ValueError(f'cannot uniquely resolve non-system DLL: {name}')
            existing = next((x for x in images if x.casefold() == key), None)
            if existing:
                if images[existing][1] != file_digest(matches[0]):
                    raise ValueError(f'DLL differs across imports: {name}')
                continue
            images[name] = (matches[0], require_amd64_pe(matches[0]))
            queue.append((name, matches[0]))
    build_manifest = {'schema': 'nudox.windows-build-manifest.v1' if args.candidate else 'nudox.windows-unsigned-preview.v1', 'source': source_info,
                      'target': receipt['target'], 'signed': False, 'workers': receipt['workers'],
                      'sqlite_basis': receipt['sqlite_basis'],
                      'build_receipt_sha256': hashlib.sha256(read_file(build / 'build-receipt.json')).hexdigest(),
                      'imports': imports_by_image,
                      'executables': records, 'dlls': {k: v[1] for k, v in images.items() if k.lower().endswith('.dll')}}
    output.parent.mkdir(parents=True, exist_ok=True)
    installer = read_file(source / 'tools/package/install-windows.ps1')
    if not installer or len(installer) > 1024 * 1024:
        raise ValueError('Windows installer is empty or oversized')
    build_bytes = (json.dumps(build_manifest, indent=2, sort_keys=True) + '\n').encode()
    readme = (('NuDox Windows x64, unsigned. Windows may show publisher or trust warnings. '
               'Verify the published SHA-256 before installation. Run install-windows.ps1 '
               'with the archive path and trusted SHA-256; it installs per user. '
               'Do not move the executables apart from their companion workers and DLLs.\n')
              if args.candidate else
              'UNSIGNED PREVIEW: not for release or installation. Native QA, dynamic DLL loads and minimum OS remain unverified.\n')
    with zipfile.ZipFile(output, 'x', compression=zipfile.ZIP_DEFLATED, compresslevel=6) as bundle:
        for name, (path, expected_hash) in sorted(images.items()):
            bundle.write(path, 'Nudox/bin/' + name)
            if file_digest(path) != expected_hash:
                raise ValueError(f'packaged image changed during staging: {name}')
        bundle.writestr('Nudox/resources/build-manifest.json', build_bytes)
        bundle.writestr('Nudox/install-windows.ps1', installer)
        bundle.writestr('Nudox/README.txt', readme)
    checksum_hash = hashlib.sha256()
    with output.open('rb') as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b''):
            checksum_hash.update(chunk)
    checksum = checksum_hash.hexdigest()
    output.with_suffix(output.suffix + '.sha256').write_text(f'{checksum}  {output.name}\n')
    if args.candidate:
        version = tomllib.loads(read_file(source / 'Cargo.toml').decode('utf-8'))['workspace']['package']['version']
        if not re.fullmatch(r'[0-9]+\.[0-9]+\.[0-9]+', version):
            raise ValueError('source workspace version is invalid')
        manifest = {'schema': 1, 'version': version, 'source_sha': revision, 'source_tree': tree,
                    'cargo_lock_sha256': source_info['cargo_lock_sha256'], 'platform': 'windows',
                    'target': receipt['target'], 'asset': output.name, 'sha256': checksum,
                    'size_bytes': output.stat().st_size, 'minimum_os': args.minimum_os,
                    'build_manifest_sha256': digest(build_bytes), 'signed': False, 'notarized': False}
        (output.parent / 'release-manifest.json').write_text(json.dumps(manifest, indent=2, sort_keys=True) + '\n')
    print(json.dumps({'candidate' if args.candidate else 'preview': str(output), 'sha256': checksum, 'signed': False}))


if __name__ == '__main__':
    try:
        main()
    except (OSError, ValueError, subprocess.CalledProcessError, json.JSONDecodeError) as error:
        print(f'Windows preview packaging failed: {error}', file=sys.stderr)
        sys.exit(1)
