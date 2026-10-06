from pathlib import Path
import subprocess,json,hashlib,time,os,signal,threading,shutil
root=Path(__file__).resolve().parent.parent;out=root/'.local/refused-capture-reconcile-test-retry';out.mkdir();source=root/'crates/local-service/src/builtin/commands/adapter.rs';before=hashlib.file_digest(source.open('rb'),'sha256').hexdigest();assert shutil.disk_usage(root).free>=18*2**30;argv=['/Users/mileswirht/Downloads/nudox-active-20261005/build-tools/run-cargo','mcp','test','--locked','--offline','-p','backend-local-service','--lib','refused_capture_reconciles_resident_view_before_terminal_reply','--','--nocapture'];r={'argv':argv,'profile':'test/dev opt-levels/debugassertions true, not release/imagebuild','source_revision':subprocess.check_output(['git','rev-parse','HEAD'],cwd=root,text=True).strip(),'patch_sha256':before,'bounds':{'launch_free_gib':18,'stop_free_gib':16,'own_build_plus_target_cap_gib':10},'scope':'one guarded j2 existing private warm Cargo graph; source regression only'};start=time.monotonic()
with (out/'cargo.log').open('wb') as log:
 p=subprocess.Popen(argv,cwd=root,stdout=log,stderr=subprocess.STDOUT,start_new_session=True);r['pid']=p.pid
 def guard():
  with (out/'disk-guard.jsonl').open('w') as f:
   while p.poll() is None:
    free=shutil.disk_usage(root).free;target=int(subprocess.check_output(['/usr/bin/du','-sk',str(root/'.local/target')],text=True).split()[0])*1024;build=int(subprocess.check_output(['/usr/bin/du','-sk',str(root/'.local/build')],text=True).split()[0])*1024;d={'elapsed':time.monotonic()-start,'free_bytes':free,'target_bytes':target,'build_bytes':build,'aggregate_bytes':build+target};f.write(json.dumps(d)+'\n');f.flush()
    if free<16*2**30 or build+target>10*2**30:
     r['guard_stop']=d;os.killpg(p.pid,signal.SIGTERM);return
    time.sleep(5)
 t=threading.Thread(target=guard,daemon=True);t.start();r['exit']=p.wait();t.join(timeout=6)
r['seconds']=time.monotonic()-start;r['source_unchanged']=before==hashlib.file_digest(source.open('rb'),'sha256').hexdigest();(out/'receipt.json').write_text(json.dumps(r,indent=2)+'\n');print(json.dumps(r,indent=2))
