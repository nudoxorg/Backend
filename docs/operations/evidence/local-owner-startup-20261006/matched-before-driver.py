import json,os,pathlib,shutil,signal,socket,subprocess,time,uuid,hashlib
P=pathlib.Path
original=P('/root/nudox-corpus-20261006/runs/sol-tantivy-real/20261006T094603Z-python-configured-b8d7d646');setup=json.load(open(original/'setup.json'));manifest=json.load(open(setup['runtime_manifest']));images={n:r['path'] for n,r in manifest['executables'].items()}
for n,r in manifest['executables'].items():
 with open(r['path'],'rb') as f:assert hashlib.file_digest(f,'sha256').hexdigest()==r['sha256']
root=original.parent/('owner-pgid-'+uuid.uuid4().hex[:8]);root.mkdir(mode=0o700);project=original/'project';env=dict(setup['env']);env['BACKEND_LOCALD_BIN']=images['backend-locald'];r={'root':str(root),'manifest':manifest,'cases':[]}
def snapshot(pid):
 try:
  proc=P('/proc')/str(pid);return {'pid':pid,'pgid':os.getpgid(pid),'sid':os.getsid(pid),'stat':(proc/'stat').read_text(),'cmdline':(proc/'cmdline').read_bytes().decode().split('\0'),'exe':os.readlink(proc/'exe')}
 except OSError:return {'pid':pid,'absent':True}
def terminal(pid):
 try:return (P('/proc')/str(pid)/'stat').read_text().split(') ',1)[1][0]=='Z'
 except OSError:return True
def owners(state,endpoint):
 out=[]
 for proc in P('/proc').glob('[0-9]*'):
  try:
   w=(proc/'cmdline').read_bytes().split(b'\0')
   if w[0].decode()==images['backend-locald'] and str(state).encode() in w and endpoint.encode() in w:out.append(int(proc.name))
  except OSError:pass
 return out
for mode in ['auto','direct']:
 folder=root/mode;folder.mkdir(mode=0o700);state=folder/'state';shutil.copytree(original/'state',state);home=folder/'home';home.mkdir(mode=0o700);localenv=dict(env,HOME=str(home),XDG_CONFIG_HOME=str(home/'.config'),XDG_DATA_HOME=str(home/'.local/share'),XDG_STATE_HOME=str(home/'.local/state'));endpoint='/tmp/nudox-pgid-'+uuid.uuid4().hex[:16]+'.sock'
 words=[images['backend-cli'],'--json','--detail','full','--project',str(project),'--workspace',str(state),'--endpoint',endpoint,'health'];direct=None
 if mode=='direct':direct=subprocess.Popen([images['backend-locald'],'--workspace',str(state),'--endpoint',endpoint,'--idle-timeout-ms','0'],cwd=project,env=localenv,stdout=(folder/'owner.stdout').open('wb'),stderr=(folder/'owner.stderr').open('wb'),start_new_session=True)
 cli=subprocess.Popen(words,cwd=project,env=localenv,stdout=subprocess.PIPE,stderr=subprocess.PIPE,start_new_session=True);deadline=time.monotonic()+10;found=[]
 while not found and time.monotonic()<deadline:
  found=owners(state,endpoint)
  if cli.poll() is not None:break
 if not found:
  if cli.poll() is None:os.killpg(cli.pid,signal.SIGTERM)
  out,err=cli.communicate(timeout=5);(folder/'launch.stdout').write_bytes(out);(folder/'launch.stderr').write_bytes(err);print('LAUNCH-FAIL',cli.returncode,out.decode(errors='replace'),err.decode(errors='replace'),flush=True)
 assert found
 owner=direct.pid if direct else found[0];row={'mode':mode,'cli':snapshot(cli.pid),'owner':snapshot(owner),'other_contenders':[snapshot(pid) for pid in found if pid!=owner]};assert not terminal(owner)
 trace=subprocess.Popen(['/run/current-system/sw/bin/strace','-ttt','-e','trace=none','-e','signal=SIGTERM','-p',str(owner),'-o',str(folder/'owner-signal.strace')],stdout=subprocess.DEVNULL,stderr=(folder/'strace.stderr').open('wb'),start_new_session=True)
 deadline=time.monotonic()+3
 while time.monotonic()<deadline:
  text=(P('/proc')/str(owner)/'status').read_text()
  if any(l.startswith('TracerPid:') and int(l.split()[1]) for l in text.splitlines()):break
 row['signal_target_pgid']=cli.pid;os.killpg(cli.pid,signal.SIGTERM);out,err=cli.communicate(timeout=5);(folder/'first-health.stdout').write_bytes(out);(folder/'first-health.stderr').write_bytes(err);row['cli_exit']=cli.returncode
 deadline=time.monotonic()+3
 while not terminal(owner) and time.monotonic()<deadline and mode=='auto':pass
 row['owner_after_client_cancel']=snapshot(owner);row['owner_terminal_after_client_cancel']=terminal(owner)
 if not terminal(owner):os.kill(owner,signal.SIGTERM)
 if direct:direct.wait(timeout=10)
 try:trace.wait(timeout=5)
 except subprocess.TimeoutExpired:trace.terminate();trace.wait(timeout=5)
 deadline=time.monotonic()+5
 while owners(state,endpoint) and time.monotonic()<deadline:time.sleep(.02)
 assert not owners(state,endpoint);r['cases'].append(row);print(json.dumps(row),flush=True)
(root/'receipt.json').write_text(json.dumps(r,indent=2)+'\n');print(str(root),flush=True)
