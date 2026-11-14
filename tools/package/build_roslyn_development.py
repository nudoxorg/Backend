#!/usr/bin/env python3
"""Publish the locked Roslyn development helper without writing into its source tree.

This produces a framework-dependent helper and a shell environment file for
local development. It is not a release packaging or admission receipt.
"""
from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import tempfile

SOURCE = Path(__file__).resolve().parents[2]
PROJECT = Path('frontends/csharp/src/legacy/helper/oracle.csproj')
REQUIRED = ('oracle.dll', 'oracle.deps.json', 'oracle.runtimeconfig.json',
            'Microsoft.CodeAnalysis.dll', 'Microsoft.CodeAnalysis.CSharp.dll')


def build(source: Path, output: Path, dotnet: Path, *, allow_downloads: bool = False) -> Path:
    source = source.resolve(strict=True)
    dotnet = dotnet.resolve(strict=True)
    output = output.absolute()
    if output.exists() or output.is_symlink():
        raise ValueError('output must be a new directory')
    project = source / PROJECT
    if not project.is_file() or not project.with_name('packages.lock.json').is_file():
        raise ValueError('Roslyn source project and package lock are required')
    output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='.roslyn-build-', dir=output.parent) as temporary:
        work = Path(temporary)
        publish = work / 'publish'
        intermediate = f'-p:BaseIntermediateOutputPath={work / "obj"}{os.sep}'
        environment = dict(os.environ, DOTNET_CLI_TELEMETRY_OPTOUT='1', DOTNET_NOLOGO='1')
        restore = [str(dotnet), 'restore', str(project), '--locked-mode', '--nologo', intermediate]
        if not allow_downloads:
            # NuGet still uses its existing global package cache. A sole empty
            # feed makes a missing package fail without contacting a network.
            feed = work / 'offline-feed'
            feed.mkdir()
            restore.extend(['--source', str(feed)])
        subprocess.run(restore, cwd=source, env=environment, check=True)
        subprocess.run(
            [str(dotnet), 'publish', str(project), '--no-restore', '-c', 'Release', '--nologo',
             '-p:UseSharedCompilation=false', intermediate,
             f'-p:BaseOutputPath={work / "bin"}{os.sep}', '-o', str(publish)],
            cwd=source, env=environment, check=True,
        )
        for name in REQUIRED:
            file = publish / name
            if not file.is_file() or file.is_symlink() or file.stat().st_size == 0:
                raise ValueError(f'Roslyn publish is incomplete: {name}')
        # Keep the entire publish closure; shipping just oracle.dll loses Roslyn
        # and the runtime configuration and cannot execute the helper.
        environment_file = publish / 'environment.sh'
        environment_file.write_text(
            '# Development helper; requires the selected .NET runtime.\n'
            f'export NUDOX_DOTNET={shlex.quote(str(dotnet))}\n'
            f'export NUDOX_ROSLYN_HELPER={shlex.quote(str(output / "oracle.dll"))}\n'
        )
        (publish / 'development-build.json').write_text(json.dumps({
            'scope': 'development helper; no distribution authority',
            'dotnet': str(dotnet), 'project': str(PROJECT), 'locked_restore': True,
            'downloads_allowed': allow_downloads,
        }, indent=2) + '\n')
        # Refuse an output created while dotnet was running.
        if output.exists() or output.is_symlink():
            raise ValueError('output appeared during build')
        publish.rename(output)
    return output / 'environment.sh'


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--source', type=Path, default=SOURCE)
    parser.add_argument('--output', required=True, type=Path, help='new development output directory')
    parser.add_argument('--dotnet', type=Path, default=shutil.which('dotnet'))
    parser.add_argument('--allow-downloads', action='store_true', help='allow locked NuGet restore to use configured feeds')
    args = parser.parse_args()
    if args.dotnet is None:
        parser.error('dotnet SDK is unavailable; supply --dotnet')
    try:
        environment = build(args.source, args.output, Path(args.dotnet), allow_downloads=args.allow_downloads)
    except (ValueError, OSError, subprocess.CalledProcessError) as error:
        parser.exit(1, f'Roslyn helper build failed: {error}\n')
    print(f'source {shlex.quote(str(environment))}')


if __name__ == '__main__':
    main()
