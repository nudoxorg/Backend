#!/usr/bin/env python3
"""Compare a generated SDK ResolveReferences project with the Rust fixture.

Pass --help for inputs. Runs only the generated project, never the user project.
All candidate paths and assets must already have been admitted by the caller.
"""
import argparse
import hashlib
import html
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile

p = argparse.ArgumentParser()
p.add_argument('--dotnet', required=True)
p.add_argument('--sdk-dir', required=True, help='observed MSBuildSDKsPath')
p.add_argument('--target', required=True)
p.add_argument('--pack-list', required=True, help='path-separated absolute DLL paths')
p.add_argument('--package-list', default='')
p.add_argument('--assets', required=True)
p.add_argument('--fixture', required=True, help='prebuilt Rust sdk_resolver_fixture binary')
p.add_argument('--global-json')
p.add_argument('--sdk-kind', choices=['Standard', 'Web'], default='Standard')
p.add_argument('--nullable', default='enable')
p.add_argument('--language', default='default')
p.add_argument('--output', default='Exe')
p.add_argument('--frameworks', default='', help='explicit project framework references, comma separated')
p.add_argument('--inherited-frameworks', default='', help='component-only inherited project context, comma separated')
p.add_argument('--exclude-project-output', help='exact project output path excluded from this component-only comparison')
a = p.parse_args()

def run_checked(label, args, cwd, env):
    # Files avoid retaining unbounded subprocess output in Python memory.
    with tempfile.TemporaryFile() as stdout, tempfile.TemporaryFile() as stderr:
        try:
            completed = subprocess.run(args, cwd=cwd, env=env, stdout=stdout,
                                       stderr=stderr, timeout=120, check=False)
        except subprocess.TimeoutExpired as error:
            for name, stream in [('stdout', stdout), ('stderr', stderr)]:
                stream.seek(0, os.SEEK_END)
                size = stream.tell()
                stream.seek(max(0, size - 16384))
                print(f'{label} {name} tail:', stream.read().decode('utf-8', errors='replace'), flush=True)
            raise SystemExit(f'{label} timed out after {error.timeout}s') from error
        if completed.returncode:
            print(f'{label} exited with status {completed.returncode}; '
                  f'LD_LIBRARY_PATH is {"set" if env.get("LD_LIBRARY_PATH") else "absent"}', flush=True)
            for name, stream in [('stdout', stdout), ('stderr', stderr)]:
                stream.seek(0, os.SEEK_END)
                size = stream.tell()
                stream.seek(max(0, size - 16384))
                print(f'{label} {name} tail:', stream.read().decode('utf-8', errors='replace'), flush=True)
            raise SystemExit(f'{label} failed')
        stdout.seek(0, os.SEEK_END)
        if stdout.tell() > 4 * 1024 * 1024:
            raise SystemExit(f'{label} stdout exceeds bound')
        stdout.seek(0)
        return stdout.read().decode('utf-8')
pack = a.pack_list.split(os.pathsep)
packages = a.package_list.split(os.pathsep) if a.package_list else []
inherited = list(filter(None, a.inherited_frameworks.split(',')))
if inherited:
    print('component context receipt:', json.dumps(dict(provenance='inherited project context', frameworks=inherited), sort_keys=True), flush=True)
    a.frameworks = ','.join(filter(None, [a.frameworks, a.inherited_frameworks]))
excluded = Path(a.exclude_project_output).resolve() if a.exclude_project_output else None
if excluded:
    # Tie the exclusion to one project library, its compile asset, and the
    # restore graph's direct project-reference metadata. A matching filename
    # alone is not sufficient to exclude an output.
    raw_assets = Path(a.assets).read_bytes()
    if not raw_assets or len(raw_assets) > 64 * 1024 * 1024:
        raise SystemExit('assets unavailable or exceeds bound')
    assets = json.loads(raw_assets)
    project_dir = Path(a.assets).resolve().parent.parent
    graph_refs = assets.get('project', {}).get('restore', {}).get('frameworks', {}).get(a.target, {}).get('projectReferences', {})
    if not isinstance(graph_refs, dict):
        raise SystemExit('project-reference metadata unavailable')
    direct_projects = {(project_dir / name).resolve() for name in graph_refs}
    matches = []
    for identity, item in assets.get('targets', {}).get(a.target, {}).items():
        library = assets.get('libraries', {}).get(identity, {})
        if library.get('type') != 'project' or not isinstance(library.get('path'), str):
            continue
        project = (project_dir / library['path']).resolve()
        if project not in direct_projects:
            continue
        framework = item.get('framework', a.target)
        if framework.startswith('.NETCoreApp,Version=v'):
            framework = 'net' + framework.removeprefix('.NETCoreApp,Version=v')
        if framework != a.target:
            continue
        for asset in item.get('compile', {}):
            relative = Path(asset)
            if relative.is_absolute() or '..' in relative.parts or relative.suffix != '.dll':
                raise SystemExit('invalid project compile asset')
            base = project.parent
            if relative.parts[:2] == ('bin', 'placeholder'):
                possible = [base / 'obj' / 'Debug' / framework / 'ref' / relative.name,
                            base / 'bin' / 'Debug' / framework / relative.name]
            else:
                possible = [base / relative]
            if excluded in (path.resolve() for path in possible):
                matches.append((identity, str(project), asset))
    if len(matches) != 1:
        raise SystemExit('project output exclusion lacks unique direct project and compile-asset evidence')
    print('scope: component-only; excluding exact project output', excluded,
          'assets evidence:', json.dumps(matches[0]), flush=True)
    packages = [path for path in packages if Path(path).resolve() != excluded]
    a.package_list = os.pathsep.join(packages)
print('SDK options:', json.dumps(dict(target=a.target, sdk_kind=a.sdk_kind, nullable=a.nullable,
    language=a.language, output=a.output, explicit_frameworks=a.frameworks), sort_keys=True), flush=True)
for path in pack + packages:
    if not Path(path).is_absolute() or not Path(path).is_file():
        raise SystemExit('candidate unavailable: ' + path)
# Only the same restricted property grammar accepted by the Rust resolver.
for value in [a.target, a.nullable, a.language, a.output] + list(filter(None, a.frameworks.split(','))):
    if not value or not all(c.isascii() and (c.isalnum() or c == '.') for c in value):
        raise SystemExit('invalid SDK property')
with tempfile.TemporaryDirectory(prefix='csharp-baseline-') as scratch:
    root = Path(scratch)
    (root / 'obj').mkdir()
    shutil.copyfile(a.assets, root / 'obj/project.assets.json')
    if a.global_json:
        shutil.copyfile(a.global_json, root / 'global.json')
    sdk = 'Microsoft.NET.Sdk.Web' if a.sdk_kind == 'Web' else 'Microsoft.NET.Sdk'
    esc = lambda value: html.escape(value, quote=True)
    refs = []
    # The SDK supplies framework assemblies through implicit FrameworkReference
    # resolution. Only restored package compile references are explicit here.
    for path in packages:
        name = Path(path).stem
        if not name or not all(c.isascii() and (c.isalnum() or c in '.-_') for c in name):
            raise SystemExit('invalid candidate name')
        refs.append(f'<Reference Include="{esc(name)}"><HintPath>{esc(path)}</HintPath><Private>false</Private></Reference>')
    frameworks = ''.join(f'<FrameworkReference Include="{esc(name)}" />' for name in a.frameworks.split(',') if name)
    project = root / 'resolve.csproj'
    project.write_text(f'''<Project Sdk="{sdk}"><PropertyGroup><TargetFramework>{esc(a.target)}</TargetFramework><Nullable>{esc(a.nullable)}</Nullable><LangVersion>{esc(a.language)}</LangVersion><OutputType>{esc(a.output)}</OutputType><ImportDirectoryBuildProps>false</ImportDirectoryBuildProps><ImportDirectoryBuildTargets>false</ImportDirectoryBuildTargets></PropertyGroup><ItemGroup>{''.join(refs)}{frameworks}</ItemGroup><Target Name="NudoxReferenceProjection" DependsOnTargets="ResolveReferences"><ItemGroup><NudoxReference Include="@(ReferencePath)" KeepMetadata="FusionName" /></ItemGroup></Target></Project>''')
    env = {k: v for k, v in os.environ.items() if k in ('PATH', 'LD_LIBRARY_PATH', 'DYLD_LIBRARY_PATH', 'DOTNET_SYSTEM_GLOBALIZATION_INVARIANT')}
    env.update(DOTNET_CLI_HOME=scratch, NUGET_PACKAGES=scratch, DOTNET_CLI_TELEMETRY_OPTOUT='1', DOTNET_SKIP_FIRST_TIME_EXPERIENCE='1', DOTNET_NOLOGO='1')
    args = [a.dotnet, 'msbuild', str(project), '-nologo', '-noAutoResponse', '-nodeReuse:false', '-target:NudoxReferenceProjection', '-getItem:NudoxReference', '-getProperty:MSBuildSDKsPath', '-p:RestoreSources=', '-p:MSBuildEnableWorkloadResolver=false']
    for stem in ('MicrosoftCommonProps', 'MicrosoftCommonTargets', 'MicrosoftCSharpTargets', 'MicrosoftCommonCrossTargetingTargets', 'MicrosoftNetFrameworkProps', 'MicrosoftNetFrameworkTargets', 'MicrosoftVisualBasicTargets'):
        for when in ('Before', 'After'):
            for prefix in ('ImportByWildcard', 'ImportUserLocationsByWildcard'):
                args.append(f'-p:{prefix}{when}{stem}=false')
    args += ['-p:ImportDirectoryBuildProps=false', '-p:ImportDirectoryBuildTargets=false', '-p:ImportDirectoryPackagesProps=false']
    observed = json.loads(run_checked('official SDK baseline', args, root, env))
    if Path(observed['Properties']['MSBuildSDKsPath']).resolve() != Path(a.sdk_dir).resolve():
        raise SystemExit('baseline invoked a different SDK')
    baseline = []
    excluded_seen = False
    for item in observed['Items']['NudoxReference']:
        path = Path(item.get('FullPath') or item['Identity']).resolve()
        if excluded and path == excluded:
            excluded_seen = True
            print('baseline excluded project output:', path, flush=True)
            continue
        data = path.read_bytes()
        baseline.append(dict(path=str(path), name=path.name, fusion_name=item['FusionName'], length=len(data), sha256=hashlib.sha256(data).hexdigest()))
    if excluded and not excluded_seen:
        print('baseline omitted the evidenced project output:', excluded, flush=True)
    env.update(CSHARP_FIXTURE_PACKAGE_CANDIDATES=a.package_list, CSHARP_FIXTURE_SDK_KIND=a.sdk_kind, CSHARP_FIXTURE_NULLABLE=a.nullable, CSHARP_FIXTURE_LANGUAGE=a.language, CSHARP_FIXTURE_OUTPUT=a.output, CSHARP_FIXTURE_FRAMEWORKS=a.frameworks)
    fixture_args = [a.fixture, a.dotnet, a.sdk_dir, a.target, a.pack_list, a.assets]
    if a.global_json:
        fixture_args.append(a.global_json)
    result = run_checked('Rust SDK resolver fixture', fixture_args, root, env)
    selected = [json.loads(line) for line in result.splitlines() if line.startswith('{')]
    def identities(rows):
        return sorted((row['name'], row['fusion_name'], row['length'], row['sha256']) for row in rows)
    for row in baseline:
        print('baseline', json.dumps(row, sort_keys=True))
    for row in selected:
        print('resolver', json.dumps(row, sort_keys=True))
    if not baseline or identities(baseline) != identities(selected):
        raise SystemExit('SDK baseline and resolver selections differ')
    print('SDK baseline and resolver selections match')
