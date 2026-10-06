from pathlib import Path
import subprocess,json,os,signal,time,uuid,hashlib
root=Path(__file__).resolve().parent.parent;local=root/'.local';build=json.loads((local/'successor-3a3-build/receipt.json').read_text());assert build['exit']==0 and build['source_unchanged'];out=local/'successor-3a3-host-boundary';out.mkdir(mode=0o700);programs=local/'successor-3a3-images';env=json.loads((local/'current-minimal/python/receipt.json').read_text())['environment'];home=out/'home';home.mkdir(mode=0o700);tmp=out/'tmp';tmp.mkdir(mode=0o700);env.update(HOME=str(home),TMPDIR=str(tmp),PATH='/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin');project=out/'project';project.mkdir();(project/'pyproject.toml').write_text('[project]\nname="closed_boundary"\nversion="1.0.0"\n');(project/'source.py').write_text('def welcome(name: str) -> str:\n    return "Hello " + name\n');events=[]
def call(argv,e):
 t=time.monotonic();p=subprocess.run(list(map(str,argv)),cwd=project,env=e,capture_output=True,text=True,timeout=90);d={'argv':list(map(str,argv)),'exit':p.returncode,'stdout':p.stdout,'stderr':p.stderr,'seconds':time.monotonic()-t};events.append(d);return d
modes={'malformed':'not-json','unknown-field':'{"version":1,"paths":[],"unexpected":"value-must-not-be-echoed"}','unknown-role':'{"version":1,"paths":[{"variable":"NUDOX_NOT_A_ROLE","path":{"encoding":"unix","units":[47]}}]}','explicit-empty':'{"version":1,"paths":[]}','absent-installed':None};receipt={'source':build['source_before'],'build_receipt_sha256':hashlib.file_digest((local/'successor-3a3-build/receipt.json').open('rb'),'sha256').hexdigest(),'environment':env,'scope':'actual matched locald closed compiler host incoming environment vs absent operator mode; configured diagnostics on localMac dev candidate','modes':{}}
for name,value in modes.items():
 lane=out/name;lane.mkdir(mode=0o700);state=lane/'state';state.mkdir(mode=0o700);secret=state/'authority.secret';fd=os.open(secret,os.O_CREAT|os.O_EXCL|os.O_WRONLY,0o600);os.write(fd,os.urandom(32));os.close(fd);endpoint='/tmp/nudox-closed-'+uuid.uuid4().hex+'.sock';e=dict(env)
 if value is not None:e['BACKEND_LOCALD_COMPILER_ENVIRONMENT']=value
 argv=[programs/'backend-locald','--endpoint',endpoint,'--workspace',state,'--authority-secret-file',secret];stderr=(lane/'locald.stderr').open('wb');stdout=(lane/'locald.stdout').open('wb');t=time.monotonic();p=subprocess.Popen(list(map(str,argv)),cwd=project,env=e,stdout=stdout,stderr=stderr,start_new_session=True);mode={'argv':list(map(str,argv)),'compiler_snapshot':value,'pid':p.pid};ops=[]
 try:
  deadline=time.monotonic()+12
  while not Path(endpoint).exists() and p.poll() is None and time.monotonic()<deadline:time.sleep(.05)
  mode['socket_ready']=Path(endpoint).exists();mode['early_exit']=p.poll()
  if mode['socket_ready'] and p.poll() is None:
   common=['--project',str(project),'--workspace',str(state),'--endpoint',endpoint];ops.append(call([programs/'backend-cli','--json','--detail','full',*common,'--passive','health'],e));ops.append(call([programs/'backend-cli','--json','--detail','full',*common,'add',str(project)],e));ops.append(call([programs/'backend-cli','--json','--detail','full',*common,'resolve','welcome'],e))
 finally:
  if p.poll() is None:p.terminate()
  try:p.wait(timeout=15)
  except subprocess.TimeoutExpired:os.killpg(p.pid,signal.SIGKILL);p.wait()
  stderr.close();stdout.close();mode.update(exit=p.returncode,seconds=time.monotonic()-t,operations=ops,stderr=(lane/'locald.stderr').read_text(),stdout=(lane/'locald.stdout').read_text());receipt['modes'][name]=mode;(lane/'receipt.json').write_text(json.dumps(mode,indent=2)+'\n');print(name,mode['exit'],mode['socket_ready'],mode['stderr'][:500],flush=True)
receipt['remaining_owners']=[x for x in subprocess.check_output(['/bin/ps','-ww','-axo','pid,ppid,rss,command'],text=True).splitlines() if str(programs/'backend-locald')+' ' in x and str(out) in x];(out/'receipt.json').write_text(json.dumps(receipt,indent=2)+'\n')
