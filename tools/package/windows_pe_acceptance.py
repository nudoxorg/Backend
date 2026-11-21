#!/usr/bin/env python3
"""Native Windows release check for exported images and recursive delayed imports.

This builds real MSVC PE files. The fixture is not a product binary or product QA.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import struct
import subprocess
import sys
import tempfile
import zipfile

sys.dont_write_bytecode = True
import windows_package as package


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--objdump', required=True, type=Path)
    parser.add_argument('--dll-dir', required=True, type=Path)
    args = parser.parse_args()
    if os.name != 'nt':
        raise ValueError('native Windows is required')
    root = Path(__file__).resolve().parents[2]
    fixture = root / 'tests/keel/fixtures/windows-pe'
    objdump = args.objdump.resolve(strict=True)
    reader = objdump.with_name('llvm-readobj.exe')
    redist = args.dll_dir.resolve(strict=True)
    system = Path(os.environ['SystemRoot']) / 'System32'
    with tempfile.TemporaryDirectory(prefix='nudox-pe-acceptance-') as temp:
        scratch = Path(temp)
        def rustc(*args):
            subprocess.run(['rustc', '+1.97.1', '--edition', '2024', *map(str, args)], check=True)
        def dll(name):
            rustc('--crate-type', 'cdylib', fixture / (name + '.rs'), '-L', scratch, '-o', scratch / (name + '.dll'))
            libs = list(scratch.glob(name + '*.lib'))
            if len(libs) != 1:
                raise ValueError('expected one native import library')
            if libs[0] != scratch / (name + '.lib'):
                shutil.copyfile(libs[0], scratch / (name + '.lib'))
        dll('leaf')
        dll('private')
        image = scratch / 'fixture.exe'
        rustc(fixture / 'fixture.rs', '-L', scratch, '-C', 'link-arg=/DELAYLOAD:private.dll',
              '-C', 'link-arg=delayimp.lib', '-C', 'link-arg=/EXPORT:fixture_export', '-o', image)
        output = subprocess.check_output([str(reader), '--coff-imports', str(image)], text=True)
        directories = package.pe_import_directories(image)
        imports = package.parse_coff_imports(output, directories)
        if directories != (True, True) or 'private.dll' not in [x.lower() for x in imports] or any(x.lower().endswith('.exe') for x in imports):
            raise ValueError('native export or delayed-import classification failed')
        def refuses(text, dirs):
            try:
                package.parse_coff_imports(text, dirs)
            except ValueError:
                return
            raise ValueError('malformed or inconsistent COFF output was accepted')
        refuses(output.replace('Name: private.dll', 'Name: ../private.dll'), directories)
        refuses(output.rsplit('}', 1)[0], directories)
        refuses(output, (True, False))
        corrupt = scratch / 'corrupt.exe'
        data = bytearray(image.read_bytes())
        offset = struct.unpack_from('<I', data, 0x3c)[0]
        struct.pack_into('<I', data, offset + 24 + 112 + 13 * 8 + 4, 0)
        corrupt.write_bytes(data)
        try:
            package.pe_import_directories(corrupt)
        except ValueError:
            pass
        else:
            raise ValueError('malformed delay directory was accepted')
        build = scratch / 'build'
        bins = build / 'bin'
        bins.mkdir(parents=True)
        names = ['backend-cli', 'backend-mcp', 'backend-locald', 'backend-desktop',
                 'backend-metrics', 'backend-stylo-worker', 'backend-sqlite-worker']
        for name in names:
            shutil.copyfile(image, bins / (name + '.exe'))
        git = lambda *args: subprocess.check_output(['git', *args], cwd=root, text=True).strip()
        records = {p.name: {'bytes': p.stat().st_size, 'sha256': sha(p)} for p in bins.iterdir()}
        receipt = {'schema': 'nudox.windows-unsigned-preview-build.v1',
                   'source': {'git_revision': git('rev-parse', 'HEAD'), 'git_tree': git('rev-parse', 'HEAD^{tree}'),
                              'cargo_lock_sha256': sha(root / 'Cargo.lock')},
                   'target': 'x86_64-pc-windows-msvc', 'profile': 'release', 'signed': False,
                   'workers': {'stylo': '1' * 64, 'sqlite': '2' * 64},
                   'sqlite_basis': {'engine': 'Syntaqlite4e4de76',
                       'native_revision': '4e4de76312429fb2235ae093ab2125914320d5bd',
                       'recipe': 'syntaqlite-sqlite-fmt-analysis-dqs-default-v1',
                       'lock_sha256': list(bytes.fromhex(sha(root / 'frontends/sqlite/Cargo.lock'))),
                       'support_sha256': [1] * 32,
                       'settings': {'Sqlite': {'version': 2147483647, 'cflags': 0, 'analyzer_dqs_fallback': True}}},
                   'executables': records}
        (build / 'build-receipt.json').write_text(json.dumps(receipt), encoding='utf-8')
        base = [sys.executable, str(root / 'tools/package/windows_package.py'), '--source', str(root),
                '--build', str(build), '--objdump', str(objdump), '--system-dll-dir', str(system),
                '--dll-dir', str(redist), '--candidate', '--minimum-os', 'Windows Server 2022']
        private_dir = scratch / 'dlls'
        private_dir.mkdir()
        def reject_missing(label, extra):
            candidate = scratch / label / 'nudox-windows-x64.zip'
            result = subprocess.run(base + extra + ['--output', str(candidate)], capture_output=True, text=True)
            if result.returncode == 0 or candidate.exists() or 'cannot uniquely resolve non-system DLL' not in result.stderr:
                raise ValueError('missing private or transitive DLL was not refused')
        reject_missing('missing-private', [])
        shutil.copyfile(scratch / 'private.dll', private_dir / 'private.dll')
        reject_missing('missing-transitive', ['--dll-dir', str(private_dir)])
        shutil.copyfile(scratch / 'leaf.dll', private_dir / 'leaf.dll')
        archive = scratch / 'candidate' / 'nudox-windows-x64.zip'
        subprocess.run(base + ['--dll-dir', str(private_dir), '--output', str(archive)], check=True)
        with zipfile.ZipFile(archive) as bundle:
            manifest = json.loads(bundle.read('Nudox/resources/build-manifest.json'))
            for name in ('private.dll', 'leaf.dll'):
                if hashlib.sha256(bundle.read('Nudox/bin/' + name)).hexdigest() != sha(private_dir / name) or manifest['dlls'][name] != sha(private_dir / name):
                    raise ValueError('private or transitive DLL differs from bundled receipt')
            if 'leaf.dll' not in [x.lower() for x in manifest['imports']['private.dll']]:
                raise ValueError('recursive private DLL dependency is missing')
            if any('private.dll' not in [x.lower() for x in manifest['imports'][name + '.exe']] for name in names):
                raise ValueError('delayed import is missing from manifest')
        print('PASS native PE exports, delayed and recursive DLL closure, and refusal controls')


if __name__ == '__main__':
    main()
