from pathlib import Path
import concurrent.futures,json,subprocess,os,signal,time,hashlib
local=Path(__file__).resolve().parent;base=local/'successor-3a3-fresh-configured';out=local/'successor-3a3-fresh-source';out.mkdir(mode=0o700);setup=json.loads((base/'setup-receipt.json').read_text());cli=Path(setup['installed_links']['cli']['path']);daemon=Path(setup['installed_links']['locald']['path']).resolve()
def lane(name):
 prior=json.loads((base/name/'receipt.json').read_text());env=prior['environment'];project=base/'projects'/name;state=base/name/'state';endpoint=prior['endpoint'];common=['--project',str(project),'--workspace',str(state),'--endpoint',endpoint];r={'language':name,'environment':env,'source':setup['source'],'events':[]}
 def call(args):
  a=[str(cli),'--json','--detail','full',*common,*args];p=subprocess.run(a,env=env,cwd=project,capture_output=True,text=True,timeout=60);d={'argv':a,'exit':p.returncode,'stdout':p.stdout,'stderr':p.stderr}
  try:d['payload']=json.loads(p.stdout)
  except ValueError:pass
  r['events'].append(d);return d
 def owners():return [x for x in subprocess.check_output(['/bin/ps','-ww','-axo','pid,ppid,rss,command'],text=True).splitlines() if str(daemon)+' ' in x and endpoint in x and str(state) in x]
 def stop():
  for row in owners():
   pid=int(row.split()[0]);r.setdefault('owner_rows',[]).append(row);os.kill(pid,signal.SIGTERM);deadline=time.monotonic()+20
   while any(int(x.split()[0])==pid for x in owners()):assert time.monotonic()<deadline;time.sleep(.1)
 def coords(v):
  if isinstance(v,dict):
   if v.get('name') in ['welcome','Welcome'] and isinstance(v.get('coordinate'),str):yield v['coordinate']
   for x in v.values():yield from coords(x)
  elif isinstance(v,list):
   for x in v:yield from coords(x)
 try:
  r['health']=call(['health']);r['outline']=call(['outline',str(project)]);targets=list(dict.fromkeys(coords(r['outline'].get('payload',{}))));r['targets']=targets;r['warm']={}
  for c in targets[:2]:r['warm'][c]={op:call([op,c]) for op in ['source','references','graph']}
  stop();r['cold_health']=call(['health']);r['cold']={}
  for c in targets[:2]:r['cold'][c]={op:call([op,c]) for op in ['source','references','graph']}
 except Exception as e:r['exception']=repr(e)
 finally:
  stop();r['remaining_owners']=owners();(out/f'{name}.json').write_text(json.dumps(r,indent=2)+'\n')
 print(name,'targets',len(r.get('targets',[])),'remaining',len(r['remaining_owners']),flush=True);return r
with concurrent.futures.ThreadPoolExecutor(max_workers=3) as p:results=list(p.map(lane,['typescript','python','go']))
(out/'summary.json').write_text(json.dumps({r['language']:{'targets':r.get('targets'),'exception':r.get('exception'),'remaining':r['remaining_owners']} for r in results},indent=2)+'\n')
