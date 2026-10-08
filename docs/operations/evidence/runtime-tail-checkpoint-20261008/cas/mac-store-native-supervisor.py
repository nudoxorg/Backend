import datetime,fcntl,hashlib,json,os,pathlib,re,resource,shlex,shutil,signal,subprocess,sys,threading,time,traceback
P=pathlib.Path
root=P('/Users/rmccrar6/sol61-fresh-cas-seal-20261008');out=root/'native-01';source=P('/Users/rmccrar6/codex-worktrees/sol61-fresh-cas-seal-20261008');graph=P('/Users/rmccrar6/codex-worktrees/sol61-nonblocking-search-20261007')
def sha(p):
 h=hashlib.sha256()
 with P(p).open('rb') as f:
  while b:=f.read(1024*1024):h.update(b)
 return h.hexdigest()
def save(n,v):(out/n).write_text(json.dumps(v,indent=2,sort_keys=True)+'\n')
def git(p,*args):return subprocess.check_output(['/usr/bin/git','-C',str(p),*args],text=True).strip()
def identity(p):return {'commit':git(p,'rev-parse','HEAD'),'tree':git(p,'rev-parse','HEAD^{tree}'),'clean':not git(p,'status','--porcelain'),'cargo_lock_sha256':sha(p/'Cargo.lock')}
loanpath=root/'search-debug-graph-loan-930dbc-20261008.json';amendment=root/'loan-amendment-lib-only.json'
assert sha(loanpath)=='1bd9fb59d64f589b0254bdeeb4282fd8cdf338c01c21ee9c06822b351b287e9e'
assert sha(amendment)=='7a2cf212727d8b754755584aca20d88d1893e6089eeaab9065b4c66c24e548f7'
loan=json.loads(loanpath.read_text());change=json.loads(amendment.read_text());held=[]
for name in loan['lifetime_locks']:
 p=P(name);assert not p.is_symlink();f=p.open('a+b');fcntl.flock(f,fcntl.LOCK_EX|fcntl.LOCK_NB);held.append(f)
before=identity(source);owner_before=identity(graph);assert before==loan['borrower_source'] and owner_before==loan['owner_source']
role=P(loan['build_role']);leases=role/'.nudox-cargo/leases';assert not list(leases.iterdir())
assert all(P(x['path']).read_text()==x['original'] for x in loan['stamps'])
assert all(sha(p)==h for p,h in loan['tools'].items())
paths=git(source,'ls-files','--','Cargo.toml','Cargo.lock','.cargo','crates/store','crates/platform','crates/version').splitlines()
def inputs():return {p:sha(source/p) for p in paths}
source_inputs=inputs()
raw=subprocess.check_output(['/bin/bash','-c','set -a; source "$1"; /usr/bin/env -0','store-native-env',loan['environment_file']]);env=dict(x.split('=',1) for x in os.fsdecode(raw).split('\0') if x)
env.update(loan['environment_overlay'])
for k in loan['unset_environment']:env.pop(k,None)
runner=str(graph/'.local/search-managed-runner/cargo');argv=[runner]+change['build_argv']
admission=P(sys.argv[1]);a=json.loads(admission.read_text());now=datetime.datetime.now(datetime.timezone.utc)
assert a['advisory_allowed'] and a['destination']=='h16001mac' and a['requested_cargo_jobs']==2
assert a['destination_memory_only_for_remote'] and a['allow_local_over_cap_for_remote']
assert all(h['complete'] and 0<=(now-datetime.datetime.fromisoformat(h['resource_sampled_at_utc'])).total_seconds()<30 for h in a['hosts'].values())
assert a['current_fleet_compiler_group_count']+1<=16 and a['hosts']['h16001mac']['compiler_group_count']+1<=8
assert a['hosts']['h16001mac']['available_memory_bytes']>=8*1024**3 and a['hosts']['h16001mac']['root_disk_available_bytes']>=16*1024**3
out.mkdir(mode=0o700);save('admission.json',a);save('source-inputs.json',source_inputs)
receipt={'schema':'nudox.focused-store-native-receipt.v1','source_before':before,'owner_before':owner_before,'loan_sha256':sha(loanpath),'amendment_sha256':sha(amendment),'admission_sha256':sha(admission),'argv':argv,'cwd':str(source),'supervisor_pid':os.getpid(),'rustc_processes':[],'pressure':{'linux_psi':'unavailable on Darwin','vm_stat_before':subprocess.check_output(['/usr/bin/vm_stat'],text=True)},'started_utc':now.isoformat()}
stop=threading.Event();child=None;watcher=None;error=None
def group_members(pgid):
 rows=[]
 for line in subprocess.check_output(['/bin/ps','-axo','pid=,pgid=,stat=,args='],text=True).splitlines():
  parts=line.strip().split(None,3)
  if len(parts)>=3 and parts[1]==str(pgid):rows.append(line.strip())
 return rows
def watch():
 seen=set()
 with (out/'process-samples.jsonl').open('w') as f:
  while not stop.wait(.4):
   try:
    rows=subprocess.check_output(['/bin/ps','-axo','pid=,pgid=,pcpu=,time=,rss=,stat=,args='],text=True)
    own=[x.strip() for x in rows.splitlines() if len(x.split(None,2))>=2 and x.split(None,2)[1]==str(child.pid)]
    f.write(json.dumps({'utc':datetime.datetime.now(datetime.timezone.utc).isoformat(),'group':own})+'\n');f.flush()
    for row in own:
     parts=row.split(None,6)
     if len(parts)!=7 or parts[0] in seen or '--crate-name backend_store ' not in parts[6] or ' --test ' not in parts[6]:continue
     pid=parts[0];tokens=shlex.split(parts[6])
     if P(tokens[0]).name not in ['rustc','rustc-real']:continue
     result=subprocess.run(['/usr/sbin/lsof','-a','-p',pid,'-d','cwd','-Fn'],text=True,capture_output=True)
     cwd=next(x[1:] for x in result.stdout.splitlines() if x.startswith('n'));sources=[str((P(cwd)/x).resolve()) for x in tokens if x.endswith('.rs')]
     if not sources or not all(P(x).is_relative_to(source) for x in sources):continue
     seen.add(pid);receipt['rustc_processes'].append({'pid':int(pid),'argv':tokens,'cwd':cwd,'source_paths':sources})
   except (OSError,ValueError,StopIteration):pass
try:
 for x in loan['stamps']:P(x['path']).write_text(x['loan_value'])
 soft,hard=resource.getrlimit(resource.RLIMIT_NOFILE);resource.setrlimit(resource.RLIMIT_NOFILE,(loan['file_descriptor_limit'],hard))
 with (out/'stdout.log').open('wb') as stdout,(out/'stderr.log').open('wb') as stderr:
  child=subprocess.Popen(argv,cwd=source,env=env,stdout=stdout,stderr=stderr,start_new_session=True);receipt['pid']=child.pid;receipt['process_start']=subprocess.check_output(['/bin/ps','-p',str(child.pid),'-o','lstart='],text=True).strip();save('launch.json',receipt);print(json.dumps({'pid':child.pid,'out':str(out)}),flush=True)
  watcher=threading.Thread(target=watch,daemon=True);watcher.start();receipt['exit_code']=child.wait();stop.set();watcher.join()
 receipt['retirement']='owned-child-kernel-wait';receipt['remaining_group']=group_members(child.pid);assert not receipt['remaining_group']
 receipt['managed_leases_empty']=not list(leases.iterdir());assert receipt['managed_leases_empty']
 receipt['source_after']=identity(source);receipt['owner_after']=identity(graph)
 assert receipt['source_after']==before and receipt['owner_after']==owner_before and inputs()==source_inputs and all(sha(p)==h for p,h in loan['tools'].items())
 text=(out/'stdout.log').read_text();summary=re.findall(r'test result: (\w+)\. (\d+) passed; (\d+) failed; (\d+) ignored;',text);receipt['test_summary']=summary
 receipt['compiler_source_bound']=bool(receipt['rustc_processes'])
 artifacts=[]
 for row in receipt['rustc_processes']:
  args=row['argv'];directory=P(args[args.index('--out-dir')+1]);extra=next(x.split('=',1)[1] for x in args if x.startswith('extra-filename='));image=directory/('backend_store'+extra)
  assert image.is_relative_to(P(loan['target'])) or image.is_relative_to(role)
  if image.is_file():
   frozen=out/image.name;shutil.copy2(image,frozen);artifacts.append({'compiler_artifact_path':str(image),'frozen_path':str(frozen),'sha256':sha(frozen),'bytes':frozen.stat().st_size});assert sha(image)==sha(frozen)
 receipt['artifacts']=artifacts
 if receipt['exit_code']==0:assert summary==[('ok','4','0','0')] and receipt['compiler_source_bound'] and len(artifacts)==1
except BaseException:
 error=traceback.format_exc();receipt['observer_error']=error
finally:
 if child is not None and child.poll() is None:
  os.killpg(child.pid,signal.SIGTERM)
  try:receipt['exit_code']=child.wait(timeout=10)
  except subprocess.TimeoutExpired:os.killpg(child.pid,signal.SIGKILL);receipt['exit_code']=child.wait()
 stop.set()
 if watcher is not None:watcher.join(timeout=5)
 if child is not None:
  remains=group_members(child.pid)
  if remains:os.killpg(child.pid,signal.SIGKILL)
  for _ in range(20):
   if not group_members(child.pid):break
   time.sleep(.1)
  receipt['remaining_group_after_retirement']=group_members(child.pid);assert not receipt['remaining_group_after_retirement']
 assert not list(leases.iterdir())
 for x in loan['stamps']:P(x['path']).write_text(x['original'])
 receipt['original_stamps_restored']=all(P(x['path']).read_text()==x['original'] for x in loan['stamps'])
 receipt['pressure']['vm_stat_after']=subprocess.check_output(['/usr/bin/vm_stat'],text=True);receipt['ended_utc']=datetime.datetime.now(datetime.timezone.utc).isoformat();receipt['stdout_sha256']=sha(out/'stdout.log');receipt['stderr_sha256']=sha(out/'stderr.log');save('receipt.json',receipt)
 print(json.dumps({'exit':receipt.get('exit_code'),'tests':receipt.get('test_summary'),'source_bound':receipt.get('compiler_source_bound'),'error':error,'receipt_sha256':sha(out/'receipt.json')}),flush=True)
if error:raise SystemExit(1)
raise SystemExit(receipt['exit_code'])
