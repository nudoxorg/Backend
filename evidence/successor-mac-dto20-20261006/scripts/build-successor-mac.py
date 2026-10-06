import datetime, hashlib, json, pathlib, shutil, subprocess, time, os, signal, threading
root=pathlib.Path(__file__).resolve().parent.parent
out=root/'.local/successor-3a3-build';out.mkdir(parents=True,exist_ok=False)
def sha(path):
 with open(path,'rb') as f:return hashlib.file_digest(f,'sha256').hexdigest()
def identity():
 return {'revision':subprocess.check_output(['git','rev-parse','HEAD'],cwd=root,text=True).strip(),'tree':subprocess.check_output(['git','rev-parse','HEAD^{tree}'],cwd=root,text=True).strip(),'status':subprocess.check_output(['git','status','--porcelain'],cwd=root,text=True).strip(),'cargo_lock_sha256':sha(root/'Cargo.lock')}
before=identity();assert not before['status'],before
runner=pathlib.Path('/Users/mileswirht/Downloads/nudox-active-20261005/build-tools/run-cargo')
controls=[runner,runner.parent/'cargo-wrapped',runner.parent/'cargo-rustc-cache',pathlib.Path('/Users/mileswirht/Downloads/backend/.local/devenv/development.sh')]
receipt={'scope':'actual private-target local Mac debug build, opt-level s, no release/bundle acceptance','source_before':before,'controls_before':{str(p):sha(p) for p in controls},'profile':'dev','opt_level':'s','jobs':2,'shared_slots':6,'host':subprocess.check_output(['/usr/bin/uname','-a'],text=True).strip(),'started_utc':datetime.datetime.now(datetime.timezone.utc).isoformat(),'recorder_sha256':sha(__file__)}
argv=[str(runner),'mcp','build','--locked','--offline','-p','backend-cli','-p','backend-mcp','-p','backend-locald','--message-format=json-render-diagnostics'];receipt['argv']=argv
log=out/'cargo.log';started=time.monotonic();artifacts={}
with log.open('wb') as file:
 proc=subprocess.Popen(argv,cwd=root,stdout=subprocess.PIPE,stderr=subprocess.STDOUT,start_new_session=True)
 def disk_guard():
  with (out/'disk-guard.jsonl').open('w') as guard_log:
   while proc.poll() is None:
    free=shutil.disk_usage(root).free
    target_bytes=int(subprocess.check_output(['/usr/bin/du','-sk',str(root/'.local/target')],text=True).split()[0])*1024
    entry={'utc':datetime.datetime.now(datetime.timezone.utc).isoformat(),'free_bytes':free,'target_bytes':target_bytes}
    guard_log.write(json.dumps(entry)+'\n');guard_log.flush()
    if free<24*2**30 or target_bytes>6*2**30:
     entry['action']='stop-own-build-process-group';receipt['guard_stop']=entry
     try:os.killpg(proc.pid,signal.SIGTERM)
     except ProcessLookupError:pass
     return
    time.sleep(5)
 guard=threading.Thread(target=disk_guard,daemon=True);guard.start()
 receipt['pid']=proc.pid;(out/'pending.json').write_text(json.dumps(receipt,indent=2)+'\n')
 for line in proc.stdout:
  file.write(line);file.flush()
  try:event=json.loads(line)
  except (json.JSONDecodeError,UnicodeDecodeError):continue
  if event.get('reason')=='compiler-artifact' and event.get('executable'):
   name=event['target']['name']
   if name in ['backend-cli','backend-mcp','backend-locald']:artifacts[name]=event
 receipt['exit']=proc.wait();guard.join(timeout=6)
receipt.update(elapsed_seconds=time.monotonic()-started,finished_utc=datetime.datetime.now(datetime.timezone.utc).isoformat(),source_after=identity(),controls_after={str(p):sha(p) for p in controls},log_sha256=sha(log),log_bytes=log.stat().st_size,artifacts=artifacts)
receipt['source_unchanged']=receipt['source_before']==receipt['source_after'];receipt['controls_unchanged']=receipt['controls_before']==receipt['controls_after']
if receipt['exit']==0:
 assert receipt['source_unchanged'] and receipt['controls_unchanged']
 assert set(artifacts)=={'backend-cli','backend-mcp','backend-locald'},artifacts
 images=root/'.local/successor-3a3-images';images.mkdir(exist_ok=False)
 receipt['images']={}
 for name,event in artifacts.items():
  source=pathlib.Path(event['executable']);target=images/name;shutil.copy2(source,target)
  assert sha(source)==sha(target)
  receipt['images'][name]={'path':str(target),'build_path':str(source),'sha256':sha(target),'bytes':target.stat().st_size,'artifact_profile':event['profile'],'dylib_loads':subprocess.check_output(['/usr/bin/otool','-L',str(target)],text=True)}
(out/'receipt.json').write_text(json.dumps(receipt,indent=2)+'\n')
print(json.dumps({k:v for k,v in receipt.items() if k in ['exit','elapsed_seconds','source_before','source_unchanged','controls_unchanged','images']},indent=2),flush=True)
