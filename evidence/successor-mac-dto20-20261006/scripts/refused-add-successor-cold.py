from pathlib import Path
import json,subprocess,time,signal,os,uuid
local=Path(__file__).resolve().parent;base=local/'successor-3a3-fresh-configured';out=local/'successor-3a3-refused-add-cold';out.mkdir(mode=0o700);prior=json.loads((base/'typescript/receipt.json').read_text());setup=json.loads((base/'setup-receipt.json').read_text());project=base/'projects/typescript';env=prior['environment'];cli=setup['installed_links']['cli']['path'];daemon=Path(setup['installed_links']['locald']['path']).resolve();state=out/'state';endpoint='/tmp/nudox-refused-'+uuid.uuid4().hex+'.sock';common=['--project',str(project),'--workspace',str(state),'--endpoint',endpoint];r={'scope':'minimalactualfreshschema health→singleTSauthorityrefusedadd→SIGTERM→coldhealth only, configuredprojectlocal7.0.2','source':setup['source'],'events':[]}
def call(args):
 argv=[cli,'--json','--detail','full',*common,*args];p=subprocess.run(argv,env=env,cwd=project,capture_output=True,text=True,timeout=60);d={'argv':argv,'exit':p.returncode,'stdout':p.stdout,'stderr':p.stderr}
 try:d['payload']=json.loads(p.stdout)
 except ValueError:pass
 r['events'].append(d);return d
def owners():return [x for x in subprocess.check_output(['/bin/ps','-ww','-axo','pid,ppid,rss,command'],text=True).splitlines() if str(daemon)+' ' in x and endpoint in x and str(state) in x]
def stop():
 rows=owners();assert len(rows)==1,rows;r.setdefault('owners',[]).append(rows[0]);pid=int(rows[0].split()[0]);os.kill(pid,signal.SIGTERM);deadline=time.monotonic()+20
 while owners():assert time.monotonic()<deadline;time.sleep(.1)
try:r['warm_health']=call(['health']);r['single_refused_add']=call(['add',str(project)]);stop();r['cold_health']=call(['health']);r['cold_pass']=r['cold_health']['exit']==0
finally:
 if owners():stop()
 r['remaining_owners']=owners();(out/'receipt.json').write_text(json.dumps(r,indent=2)+'\n');print(json.dumps({'cold_pass':r.get('cold_pass'),'singleaddexit':r.get('single_refused_add',{}).get('exit'),'remaining':len(r['remaining_owners'])}))
