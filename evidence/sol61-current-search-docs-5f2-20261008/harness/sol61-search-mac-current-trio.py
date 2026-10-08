import pathlib,subprocess,json,os,sys,fcntl,datetime,hashlib,resource,shutil,time
wt=pathlib.Path('/Users/rmccrar6/codex-worktrees/sol61-nonblocking-search-20261007'); out=pathlib.Path('/Users/rmccrar6/nudox-functional-corpus-20261006/sol61-nonblocking-search')/sys.argv[4];out.mkdir(parents=True,exist_ok=False)
lock=open(wt/'.local/search-graph-lifetime.lock','a+');fcntl.flock(lock,fcntl.LOCK_EX|fcntl.LOCK_NB)
a=json.loads(pathlib.Path(sys.argv[1]).read_text());now=datetime.datetime.now(datetime.timezone.utc)
assert a['advisory_allowed'] and a['current_fleet_compiler_group_count']<16 and a['hosts']['h16001mac']['compiler_group_count']<8
assert a['destination']=='h16001mac' and a['memory_guard_scope']=='destination' and a['destination_memory_only_for_remote'] is True
assert a['minimum_available_memory_bytes_each_host'] is None and a['minimum_available_memory_bytes_destination']==8*1024**3
assert all(h['complete'] and type(h['available_memory_bytes']) is int and h['available_memory_bytes']>=0 and h['memory_floor_applies'] is (name=='h16001mac') and 0<=(now-datetime.datetime.fromisoformat(h['resource_sampled_at_utc'])).total_seconds()<=30 for name,h in a['hosts'].items())
assert a['hosts']['h16001mac']['available_memory_bytes']>=8*1024**3
assert a['hosts']['h16001mac']['root_disk_available_bytes']>=16*1024**3
resource.setrlimit(resource.RLIMIT_NOFILE,(8192,8192))
def sha(p):return hashlib.sha256(p.read_bytes()).hexdigest()
def git(*argv):return subprocess.check_output(['/usr/bin/git','-C',str(wt),*argv],text=True).strip()
def source():return dict(commit=git('rev-parse','HEAD'),tree=git('rev-parse','HEAD^{tree}'),clean=not git('status','--porcelain'),lock_sha256=sha(wt/'Cargo.lock'))
before=source();assert before['clean'] and before['commit']==sys.argv[2] and before['tree']==sys.argv[3]
b= subprocess.check_output(['/bin/bash','-c','source '+str(wt/'.local/native-environment.sh')+'; /usr/bin/env -0'])
env=dict(x.decode().split('=',1) for x in b.split(b'\0') if x)
env.update(CARGO_HOME='/Users/rmccrar6/.cargo', CARGO_TARGET_DIR=str(wt/'.local/target'),CARGO_BUILD_BUILD_DIR=str(wt/'.local/search-mac-owned-role'),SCCACHE_DIR=str(wt/'.local/sccache'),CARGO_BUILD_JOBS='2',NIX_BUILD_CORES='2',CARGO_INCREMENTAL='0',NUDOX_CARGO_BUILD_SLOTS='6',NUDOX_CARGO_SLOT_WAIT_MS='0',NUDOX_CARGO_WORKTREE_WAIT_MS='0',RUSTC_WRAPPER=str(wt/'.local/search-managed-runner/rustc-cache.sh'))
runner=wt/'.local/search-managed-runner/cargo';argv=[str(runner),'build','--locked','--offline','-j2','-p','backend-cli','-p','backend-mcp','-p','backend-locald','--message-format=json-render-diagnostics'];receipt=dict(source_before=before,admission=a,argv=argv,runner_sha256=sha(runner),environment_sha256=sha(wt/'.local/native-environment.sh'),started_utc=now.isoformat(),lifetime_lock=str(wt/'.local/search-graph-lifetime.lock'))
receipt['supervisor_sha256']=sha(pathlib.Path(__file__))
receipt['memory_guard_scope']=a['memory_guard_scope'];receipt['destination_memory_only_for_remote']=a['destination_memory_only_for_remote']
receipt['toolchain']={name:subprocess.check_output([env['RUSTC'] if name=='rustc' else str(pathlib.Path(env['RUSTC']).with_name('cargo')),'--version'],cwd=wt,env=env,text=True).strip() for name in ['rustc','cargo']}
artifacts={};started=time.monotonic()
with (out/'cargo.log').open('wb') as log:
 child=subprocess.Popen(argv,cwd=wt,env=env,stdout=subprocess.PIPE,stderr=log,start_new_session=True);receipt['pid']=child.pid;(out/'launch.json').write_text(json.dumps(receipt,indent=2));print(json.dumps(dict(pid=child.pid,out=str(out),source=before)),flush=True)
 for line in child.stdout:
  log.write(line);log.flush()
  try:event=json.loads(line)
  except ValueError:continue
  if event.get('reason')=='compiler-artifact' and event.get('executable') and event['target']['name'] in ['backend-cli','backend-mcp','backend-locald']:artifacts[event['target']['name']]=event
 receipt['exit']=child.wait()
receipt.update(source_after=source(),ended_utc=datetime.datetime.now(datetime.timezone.utc).isoformat(),elapsed_seconds=time.monotonic()-started,retirement='owned-child-kernel-wait',artifact_events=artifacts);assert receipt['source_before']==receipt['source_after']
(out/'receipt.json').write_text(json.dumps(receipt,indent=2))
if receipt['exit']==0:
 assert set(artifacts)=={'backend-cli','backend-mcp','backend-locald'}
 images=out/'images';images.mkdir();manifest=dict(source_commit=before['commit'],source_tree=before['tree'],cargo_lock_sha256=before['lock_sha256'],artifacts={})
 for name,event in artifacts.items():
  src=pathlib.Path(event['executable']);dst=images/name;shutil.copy2(src,dst);manifest['artifacts'][name]=dict(sha256=sha(dst),bytes=dst.stat().st_size,exact_cargo_event=event)
 (out/'manifest.json').write_text(json.dumps(manifest,indent=2))
print(json.dumps(dict(exit=receipt['exit'],out=str(out))),flush=True)
sys.exit(receipt['exit'])
