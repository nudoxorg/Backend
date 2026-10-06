#!/usr/bin/env python3
"""Real matched macOS CLI/MCP installed-symlink journeys; never starts the GUI.

All runtime state is newly allocated. Installed native Homebrew compilers are
recorded rather than represented as absent. No developer compiler pins are used.
"""
import argparse, concurrent.futures, hashlib, json, math, os, pathlib, selectors
import shutil, signal, subprocess, threading, time, uuid

def digest(path):
    with open(path, 'rb') as f:
        return hashlib.file_digest(f, 'sha256').hexdigest()

def main():
    p = argparse.ArgumentParser()
    p.add_argument('--build-receipt', type=pathlib.Path, required=True)
    p.add_argument('--output', type=pathlib.Path, required=True)
    p.add_argument('--skip-rust', action='store_true')
    p.add_argument('--minimal', action='store_true')
    p.add_argument('--languages', nargs='+', choices=['typescript','python','go'], default=['typescript','python','go'])
    p.add_argument('--no-global-ts', action='store_true')
    p.add_argument('--acquire-go-helper', action='store_true', help='Acquire exact matched vendored Go helper dependencies into this private HOME via ordinary go mod download; no alternate authority')
    p.add_argument('--images', type=pathlib.Path, required=True)
    p.add_argument('--configured', action='store_true', help='Explicit real installed native-tool paths only; no alternate authority')
    p.add_argument('--install-authorities', action='store_true', help='Install ordinary latest npm global tsc, pip Pyrefly, and a real Go module in the private HOME')
    args = p.parse_args()
    root = args.output.resolve()
    root.mkdir(parents=True, exist_ok=False, mode=0o700)
    home = root / 'home'; home.mkdir(mode=0o700)
    (home/'Applications').mkdir()
    env = {'HOME': str(home), 'PATH': '/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin',
           'TMPDIR': str(root/'tmp'), 'LANG': 'en_US.UTF-8'}
    pathlib.Path(env['TMPDIR']).mkdir(mode=0o700)
    def command(argv, cwd=root, timeout=90, child_env=env):
        start = time.monotonic()
        try:
            r = subprocess.run(argv, env=child_env, cwd=cwd, capture_output=True, timeout=timeout)
            return {'argv': list(map(str, argv)), 'exit': r.returncode, 'seconds': time.monotonic()-start,
                    'stdout': r.stdout.decode(errors='replace'), 'stderr': r.stderr.decode(errors='replace')}
        except subprocess.TimeoutExpired as e:
            return {'argv': list(map(str, argv)), 'timeout': timeout, 'seconds': time.monotonic()-start,
                    'stdout': (e.stdout or b'').decode(errors='replace'), 'stderr': (e.stderr or b'').decode(errors='replace')}
    build=json.loads(args.build_receipt.read_text())
    assert build['exit']==0 and build['source_unchanged'] and build['controls_unchanged']
    receipt={'scope':'actual local Mac matched current debug candidate through stock installer in fresh HOME; not a release or shipped app bundle', 'source':build['source_before']['revision'], 'profile':build['profile'], 'build_receipt_sha256':digest(args.build_receipt),'environment':env, 'configured':args.configured,'host':command(['/usr/bin/uname','-a']), 'macos':command(['/usr/bin/sw_vers']), 'resources':command(['/usr/sbin/sysctl','hw.ncpu','hw.memsize']), 'runner_sha256':digest(__file__)}
    assert not pathlib.Path('/Applications/Nudox.app').exists(), 'Preserve global user app; stock installer prefers it'
    program_dir=home/'.local/libexec/nudox-current';program_dir.mkdir(parents=True)
    for name,identity in build['images'].items():
        source=args.images/name
        assert digest(source)==identity['sha256']
        shutil.copy2(source,program_dir/name)
        assert digest(program_dir/name)==identity['sha256']
    (home/'.local/bin').mkdir()
    bins={key:home/'.local/bin'/value for key,value in [('cli','nudox'),('mcp','nudox-mcp'),('locald','nudox-locald')]}
    for key,binary in [('cli','backend-cli'),('mcp','backend-mcp'),('locald','backend-locald')]:bins[key].symlink_to(program_dir/binary)
    receipt['scope']='actual local Mac matched current unbundled debug candidate through fresh HOME installed symlinks, stock or explicit discovery; not app bundle/release or stock app installer acceptance'
    receipt['installation']={'kind':'manual three-binary CLI installation, finite HOME .local/libexec + .local/bin symlinks','program_dir':str(program_dir)}
    receipt['installed_links']={k:{'path':str(v),'target':os.readlink(v),'sha256':digest(v)} for k,v in bins.items()}
    receipt['help'] = {k: command([str(v), '--help']) for k,v in bins.items()}
    receipt['ordinary_native_tools'] = {tool: command([path, *opts]) for tool,path,opts in [
        ('node','/opt/homebrew/bin/node',['--version']), ('npm','/opt/homebrew/bin/npm',['--version']),
        ('python','/opt/homebrew/bin/python3',['--version']), ('go','/opt/homebrew/bin/go',['version']),
        ('rustc','/opt/homebrew/bin/rustc',['--version'])] if pathlib.Path(path).exists()}
    projects = {}
    fixtures = {
      'typescript': {'package.json': json.dumps({'name':'fresh-web-app','version':'1.0.0','private':True,'type':'module','devDependencies':{'typescript':'latest'}}),
        'tsconfig.json': json.dumps({'compilerOptions':{'strict':True,'target':'ES2022','module':'NodeNext','moduleResolution':'NodeNext','lib':['ES2022','DOM']},'include':['src']}),
        'src/math.ts': 'export function welcome(name: string): string { return `Hello ${name}`; }\nexport interface Item { id: number; label: string; }\nexport function total(items: Item[]): number { return items.reduce((n, x) => n + x.id, 0); }\n',
        'src/app.ts': 'import { welcome, total, type Item } from "./math.js";\nexport class Dashboard { items: Item[] = []; render(root: HTMLElement): void { root.textContent = welcome("visitor") + total(this.items); } }\n',
        'src/index.ts': 'import { Dashboard } from "./app.js";\nexport function mount(root: HTMLElement): Dashboard { const app = new Dashboard(); app.render(root); return app; }\n'},
      'python': {'pyproject.toml': '[project]\nname = "fresh_service"\nversion = "1.0.0"\nrequires-python = ">=3.11"\n',
        'fresh_service/__init__.py': 'from .model import Invoice, total\n',
        'fresh_service/model.py': 'from dataclasses import dataclass\n@dataclass\nclass Invoice:\n    amount: float\n    customer: str\ndef total(invoices: list[Invoice]) -> float:\n    return sum(item.amount for item in invoices)\ndef welcome(name: str) -> str:\n    return f"Hello {name}"\n',
        'fresh_service/app.py': 'import json\nfrom .model import Invoice, total, welcome\ndef response() -> str:\n    return json.dumps({"message": welcome("visitor"), "total": total([Invoice(10.0, "sample")])})\nif __name__ == "__main__":\n    print(response())\n'},
      'go': {'go.mod':'module example.com/freshservice\n\ngo 1.24\n',
        'model/model.go':'package model\ntype Invoice struct { Amount float64; Customer string }\nfunc Total(items []Invoice) float64 { var sum float64; for _, item := range items { sum += item.Amount }; return sum }\nfunc Welcome(name string) string { return "Hello " + name }\n',
        'cmd/service/main.go':'package main\nimport ("encoding/json"; "fmt"; "example.com/freshservice/model")\nfunc main() { b, _ := json.Marshal(map[string]any{"message":model.Welcome("visitor"), "total": model.Total([]model.Invoice{{Amount:10, Customer:"sample"}})}); fmt.Println(string(b)) }\n'},
      'rust': {'Cargo.toml':'[package]\nname = "fresh_light_rust"\nversion = "0.1.0"\nedition = "2024"\n',
        'src/lib.rs':'pub fn welcome(name: &str) -> String { format!("Hello {name}") }\npub fn greeting() -> String { welcome("visitor") }\n'}
    }
    if args.minimal:
        fixtures['typescript']={k:v for k,v in fixtures['typescript'].items() if k in ('package.json','tsconfig.json')}
        fixtures['typescript']['src/index.ts']='export function welcome(name: string): string { return `Hello ${name}`; }\nexport function greeting(): string { return welcome("visitor"); }\n'
        fixtures['python']={'pyproject.toml':fixtures['python']['pyproject.toml'],'fresh_service/__init__.py':'','fresh_service/model.py':'def welcome(name: str) -> str:\n    return "Hello " + name\ndef greeting() -> str:\n    return welcome("visitor")\n','fresh_service/app.py':'from .model import greeting\nprint(greeting())\n'}
        fixtures['go']={'go.mod':'module example.com/freshminimal\n\ngo 1.24\n','minimal.go':'package freshminimal\nfunc Welcome(name string) string { return "Hello " + name }\nfunc Greeting() string { return Welcome("visitor") }\n'}
    for name, files in fixtures.items():
        project = root/'projects'/name; project.mkdir(parents=True)
        for relative, body in files.items():
            path = project/relative; path.parent.mkdir(parents=True, exist_ok=True); path.write_text(body)
        projects[name] = project
    receipt['npm_install'] = command(['/opt/homebrew/bin/npm', 'install', '--ignore-scripts', '--no-audit', '--no-fund'], projects['typescript'], 120)
    if receipt['npm_install'].get('exit') == 0:
        receipt['typescript_native_check'] = command([str(projects['typescript']/'node_modules/.bin/tsc'), '--noEmit'], projects['typescript'])
        receipt['fresh_typescript_version'] = json.loads((projects['typescript']/'node_modules/typescript/package.json').read_text())['version']
    if args.install_authorities:
        if not args.no_global_ts:
            receipt['npm_global_install'] = command(['/opt/homebrew/bin/npm','install','--global','--prefix',str(home/'.local'),'--ignore-scripts','--no-audit','--no-fund','typescript'],timeout=120)
        receipt['python_tools_venv'] = command(['/opt/homebrew/bin/python3','-m','venv',str(home/'python-tools')])
        receipt['pyrefly_install'] = command([str(home/'python-tools/bin/python3'),'-m','pip','install','pyrefly'],timeout=120)
        if receipt['pyrefly_install'].get('exit') == 0:
            (home/'.local/bin/pyrefly').symlink_to(home/'python-tools/bin/pyrefly')
            receipt['pyrefly_version'] = command([str(home/'.local/bin/pyrefly'),'--version'])
        if not args.minimal:receipt['go_module_install'] = command(['/opt/homebrew/bin/go','get','github.com/go-chi/chi/v5'],projects['go'],120)
        if receipt.get('go_module_install',{}).get('exit') == 0:
            (projects['go']/'cmd/service/router.go').write_text('package main\nimport ("net/http"; "github.com/go-chi/chi/v5"; "example.com/freshservice/model")\nfunc Router() http.Handler { router := chi.NewRouter(); router.Get("/", func(w http.ResponseWriter, r *http.Request) { w.Write([]byte(model.Welcome("visitor"))) }); return router }\n')
        receipt['discovered_helper_files'] = {str(f.relative_to(home)): {'resolved_path':str(f.resolve()),'sha256':digest(f)} for f in [home/'.local/bin/tsc',home/'.local/bin/pyrefly',home/'.local/lib/node_modules/typescript/package.json'] if f.is_file()}
    if args.acquire_go_helper:
        helper_source=pathlib.Path(__file__).resolve().parents[1]/'frontends/go/src/legacy/oracle'
        helper_copy=root/'matched-go-helper-source';shutil.copytree(helper_source,helper_copy)
        receipt['go_helper_source_hashes']={str(f.relative_to(helper_source)):digest(f) for f in helper_source.iterdir() if f.is_file()}
        receipt['go_helper_dependency_acquisition']=command(['/opt/homebrew/bin/go','mod','download'],helper_copy,120)
    receipt['python_native_check'] = command(['/opt/homebrew/bin/python3', '-m', 'fresh_service.app'], projects['python'])
    receipt['go_native_check'] = command(['/opt/homebrew/bin/go', *(['test','./...'] if args.minimal else ['run','./cmd/service'])], projects['go'], 120)
    receipt['fixture_hashes'] = {name:{str(f.relative_to(path)):digest(f) for f in path.rglob('*') if f.is_file() and 'node_modules' not in f.parts} for name,path in projects.items()}
    (root/'setup-receipt.json').write_text(json.dumps(receipt, indent=2)+'\n')
    def journey(name):
        project = projects[name]; lane = root/name; lane.mkdir(mode=0o700); owned = set(); mcp = None; wire = open(lane/'wire.jsonl','w')
        endpoint = '/tmp/nudox-fresh-'+uuid.uuid4().hex+'.sock'
        common = ['--project',str(project),'--workspace',str(lane/'state'),'--endpoint',endpoint]
        (lane/'mcp-config.json').write_text(json.dumps({'mcpServers':{'nudox':{'command':str(bins['mcp']), 'args':common}}},indent=2)+'\n')
        lane_env = dict(env); lane_env['PATH'] = str(project/'node_modules/.bin')+':'+str(home/'.local/bin')+':'+env['PATH']
        if args.configured:
            for key,path in {'NUDOX_PYTHON':pathlib.Path('/opt/homebrew/bin/python3'),'NUDOX_PYREFLY':home/'.local/bin/pyrefly','NUDOX_GO':pathlib.Path('/opt/homebrew/bin/go'),'NUDOX_GO_ROOT':home/'go/pkg/mod','NUDOX_TSC':project/'node_modules/typescript/bin/tsc','NUDOX_TYPESCRIPT_NODE':pathlib.Path('/opt/homebrew/bin/node'),'NUDOX_TYPESCRIPT_MODULE_ROOT':project/'node_modules'}.items():
                if path.exists():lane_env[key]=str(path.resolve())
        (lane/'mcp-config.json').write_text(json.dumps({'mcpServers':{'nudox':{'command':str(bins['mcp']), 'args':common,'env':lane_env}}},indent=2)+'\n')
        result = {'language':name,'endpoint':endpoint,'environment':lane_env,'events':[], 'runtime_complete':False}
        def record(kind, **data):
            entry={'kind':kind, **data}; wire.write(json.dumps(entry)+'\n'); wire.flush(); result['events'].append(entry)
        def cli(argv):
            value=command([str(bins['cli']),'--json','--detail','full',*common,*argv],project,90,lane_env)
            try: value['payload']=json.loads(value['stdout'])
            except json.JSONDecodeError: pass
            record('cli',**value); return value.get('payload')
        def census():
            lines=subprocess.check_output(['/bin/ps','-ww','-axo','pid,ppid,rss,command'],text=True).splitlines()
            return [line for line in lines if str(bins['locald'].resolve())+' ' in line and endpoint in line and str(lane/'state') in line]
        def owner():
            rows=census(); assert len(rows)==1,rows
            pid=int(rows[0].split()[0]); owned.add(pid); record('owner',pid=pid,row=rows[0]); return pid
        def stop(pid):
            assert any(int(row.split()[0])==pid for row in census())
            os.kill(pid,signal.SIGTERM); deadline=time.monotonic()+20
            while any(int(row.split()[0])==pid for row in census()):
                if time.monotonic()>deadline: raise TimeoutError('private owner shutdown')
                time.sleep(.1)
            owned.remove(pid); record('owner-stopped',pid=pid)
        def rpc(method,params=None,notify=False):
            nonlocal sequence,buffer
            sequence+=1; request={'jsonrpc':'2.0','method':method,'params':params or {}}
            if not notify: request['id']=sequence
            record('mcp-request',payload=request); mcp.stdin.write((json.dumps(request)+'\n').encode());mcp.stdin.flush()
            if notify: return
            started=time.monotonic(); deadline=started+60
            while b'\n' not in buffer:
                if not selector.select(max(0,deadline-time.monotonic())):raise TimeoutError(method)
                chunk=os.read(mcp.stdout.fileno(),65536)
                if not chunk:raise RuntimeError('MCP EOF')
                buffer+=chunk
            line,buffer=buffer.split(b'\n',1);reply=json.loads(line);assert reply['id']==sequence
            record('mcp-reply',method=method,bytes=len(line)+1,estimated_tokens=math.ceil((len(line)+1)/4),seconds=time.monotonic()-started,payload=reply);return reply
        def coordinates(value):
            if isinstance(value,dict):
                if isinstance(value.get('coordinate'),str):yield value
                for child in value.values():yield from coordinates(child)
            elif isinstance(value,list):
                for child in value:yield from coordinates(child)
        try:
            result['warm_health']=cli(['health']);result['startup_passive_health']=cli(['--passive','health']);warm=owner()
            result['add']=cli(['add',str(project)])
            result['packages']=cli(['packages']);result['outline']=cli(['outline',str(project)])
            def purls(value):
                if isinstance(value,str) and value.startswith('pkg:'):yield value.split('#')[0]
                elif isinstance(value,dict):
                    for child in value.values():yield from purls(child)
                elif isinstance(value,list):
                    for child in value:yield from purls(child)
            packages=list(dict.fromkeys(purls(result['packages'])))
            if not packages and isinstance(result['packages'],dict) and result['packages'].get('projects'):packages=[str(project)]
            result['index_control_attempts']=[]
            for package in packages[:1]:
                started=cli(['index_start',package,'--execution-intent','background'])
                tickets=[]
                def find_tickets(value):
                    if isinstance(value,dict):
                        if isinstance(value.get('ticket'),dict):tickets.append(value['ticket'])
                        for child in value.values():find_tickets(child)
                    elif isinstance(value,list):
                        for child in value:find_tickets(child)
                    elif isinstance(value,str) and value.startswith('{'):
                        try:
                            decoded=json.loads(value)
                            if isinstance(decoded,dict) and ('basis' in decoded or 'package' in decoded):tickets.append(decoded)
                        except json.JSONDecodeError:pass
                find_tickets(started)
                control={'package':package,'start':started,'tickets':tickets}
                for ticket in tickets[:1]:
                    encoded=json.dumps(ticket,separators=(',',':'))
                    control['cancel']=cli(['index_cancel',encoded])
                    control['progress']=cli(['index_progress',encoded])
                    control['await']=cli(['index_await',encoded])
                result['index_control_attempts'].append(control)
            result['retry_add']=cli(['add',str(project)])
            result['unchanged_add']=cli(['add',str(project)])
            result['search']=cli(['search','welcome' if name!='go' else 'Welcome','--limit','25'])
            result['resolve']=cli(['resolve','welcome' if name!='go' else 'Welcome','--limit','25'])
            targets=list(coordinates(result['resolve']));result['targets']=targets
            for target in targets[:2]:
                for operation in ['source','references','graph','show']:cli([operation,target['coordinate']])
            mcp=subprocess.Popen([str(bins['mcp']),*common],env=lane_env,cwd=project,stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=open(lane/'mcp.stderr','wb'),start_new_session=True)
            sequence=0;buffer=b'';selector=selectors.DefaultSelector();selector.register(mcp.stdout,selectors.EVENT_READ)
            result['initialize']=rpc('initialize',{'protocolVersion':'2025-11-25','capabilities':{},'clientInfo':{'name':'Fresh Mac manual flow','version':'1'}})
            rpc('notifications/initialized',notify=True);result['tools_list']=rpc('tools/list')
            for tool,arguments in [('status',{}),('packages',{}),('search',{'query':'welcome' if name!='go' else 'Welcome','limit':25}),('resolve',{'query':'welcome' if name!='go' else 'Welcome','limit':25})]:
                rpc('tools/call',{'name':'backend.'+tool,'arguments':{'detail':'full',**arguments}})
            for target in targets[:2]:
                for tool in ['source','references','graph','document']:
                    rpc('tools/call',{'name':'backend.'+tool,'arguments':{'coordinate':target['coordinate'],'detail':'full'}})
            mcp.stdin.close();record('mcp-stop',pid=mcp.pid,exit=mcp.wait(timeout=20));mcp=None;selector.close()
            stop(warm);result['cold_health']=cli(['health']);cold=owner();assert cold!=warm
            result['cold_search']=cli(['search','welcome' if name!='go' else 'Welcome','--limit','25'])
            result['cold_resolve']=cli(['resolve','welcome' if name!='go' else 'Welcome','--limit','25'])
            for target in targets[:2]:
                for operation in ['source','references','graph']:cli([operation,target['coordinate']])
            result['cold_packages']=cli(['packages']);result['cold_owner_pid']=cold;result['warm_owner_pid']=warm;stop(cold)
            result['runtime_complete']=True
        except Exception as error:
            result['exception']=repr(error)
        finally:
            if mcp is not None:
                mcp.terminate()
                try:mcp.wait(timeout=10)
                except subprocess.TimeoutExpired:mcp.kill();mcp.wait()
            for row in census():owned.add(int(row.split()[0]))
            for pid in list(owned):
                try:stop(pid)
                except Exception as error:record('cleanup-error',pid=pid,error=repr(error))
            result['owned_remaining']=census();wire.close()
            (lane/'receipt.json').write_text(json.dumps(result,indent=2)+'\n')
        print(name, 'complete',result['runtime_complete'],'targets',len(result.get('targets',[])),'remaining',len(result['owned_remaining']),flush=True)
        return result
    samples=[]; finished=threading.Event(); began=time.monotonic()
    def sampler():
        while not finished.is_set():
            rows=[]
            for line in subprocess.check_output(['/bin/ps','-ww','-axo','pid,ppid,rss,command'],text=True).splitlines()[1:]:
                fields=line.strip().split(None,3)
                if len(fields)==4:rows.append({'pid':int(fields[0]),'ppid':int(fields[1]),'rss_kib':int(fields[2]),'command':fields[3]})
            image_prefixes=[str(v.resolve())+' ' for v in bins.values()]
            descendants={os.getpid()}
            descendants.update(r['pid'] for r in rows if any(r['command'].startswith(prefix) for prefix in image_prefixes) and str(root) in r['command'])
            while True:
                new={r['pid'] for r in rows if r['ppid'] in descendants}
                if new.issubset(descendants):break
                descendants.update(new)
            # locald deliberately detaches to PPID 1, so include the exact
            # private bundle/image prefix as well as current descendants.
            image_prefixes=[str(v.resolve())+' ' for v in bins.values()]
            own=[r for r in rows if (r['pid'] in descendants or any(r['command'].startswith(prefix) for prefix in image_prefixes) and str(root) in r['command']) and not r['command'].startswith('/bin/ps ')]
            samples.append({'elapsed':time.monotonic()-began,'processes':own})
            finished.wait(.25)
    sampling=threading.Thread(target=sampler);sampling.start()
    try:
        with concurrent.futures.ThreadPoolExecutor(max_workers=3) as pool:
            results=list(pool.map(journey,args.languages))
        if not args.skip_rust:results.append(journey('rust'))
    finally:
        finished.set();sampling.join()
    (root/'resource-census.json').write_text(json.dumps({'samples':samples,'sample_interval_seconds':.25,'elapsed_seconds':time.monotonic()-began,'peak_rss_kib':max((sum(x['rss_kib'] for x in s['processes']) for s in samples),default=0),'peak_process_count':max((len(s['processes']) for s in samples),default=0),'note':'Own runner, descendants, and exact private detached owners; sampling does not prove transient peaks'},indent=2)+'\n')
    summary={'scope':receipt['scope'],'setup':str(root/'setup-receipt.json'),'languages':{r['language']:{'runtime_complete':r['runtime_complete'],'targets':len(r.get('targets',[])),'owned_remaining':r['owned_remaining'],'exception':r.get('exception')} for r in results},'acceptance':False,'note':'A complete protocol journey is not semantic acceptance. Inspect publication and content.'}
    (root/'summary.json').write_text(json.dumps(summary,indent=2)+'\n')
    print(json.dumps(summary,indent=2))
if __name__=='__main__':main()
