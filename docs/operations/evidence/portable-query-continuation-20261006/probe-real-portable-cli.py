import pathlib,subprocess,json,hashlib,shutil,os,signal,time,uuid,socket,selectors,sys
base=pathlib.Path('/Users/mileswirht/Downloads/nudox-active-20261005/sol-current-mac-runtime/.local')
original=base/'successor-039c-positive'; witness=json.loads((original/'receipt.json').read_text()); project=original/'projects/python'
out=pathlib.Path(os.environ['PC2_OUTPUT']);out.mkdir(mode=0o700);state=out/'state';shutil.copytree(original/'paging/state',state)
env=dict(witness['environment']);env.update(HOME=str(out/'home'),TMPDIR=str(out/'tmp'))
for key in ('HOME','TMPDIR'):pathlib.Path(env[key]).mkdir(mode=0o700)
endpoint='/tmp/nudox-pc2-'+uuid.uuid4().hex[:16]+'.sock'
references={}
locald=base/'successor-039c-images/backend-locald';oldcli=base/'successor-039c-images/backend-cli';cli=pathlib.Path(os.environ['PC2_CLI']);mcp=pathlib.Path(os.environ['PC2_MCP'])
def sha(p):
 h=hashlib.sha256()
 with open(p,'rb') as f:
  for b in iter(lambda:f.read(1024*1024),b''):h.update(b)
 return h.hexdigest()
assert sha(locald)=='8e67a3a9b7d13341a5a149780b826e90773f6f32508ae48e7752c065c992e66f'
common=['--project',str(project),'--workspace',str(state),'--endpoint',endpoint]
receipt={'source_base':'039c360d286962a6cf19488fb86caabd1d8c93de','fixture_receipt_sha256':sha(original/'receipt.json'),'original_project':str(project),'state_copy':str(state),'endpoint':endpoint,'environment':env,'images':{k:{'path':str(p),'sha256':sha(p)} for k,p in [('locald',locald),('old_cli',oldcli),('cli',cli),('mcp',mcp)]},'checks':{},'owners':[]};wire=(out/'wire.jsonl').open('w');owner=None;serial=0

def log(kind,**data):wire.write(json.dumps({'kind':kind,**data})+'\n');wire.flush()
def command(binary,args,label):
 global serial
 serial+=1;argv=[str(binary),'--json','--detail','full',*common,'--passive',*args];t=time.monotonic();p=subprocess.Popen(argv,cwd=project,env=env,stdout=subprocess.PIPE,stderr=subprocess.PIPE,start_new_session=True)
 try:stdout,stderr=p.communicate(timeout=60)
 except subprocess.TimeoutExpired:
  os.killpg(p.pid,signal.SIGTERM);stdout,stderr=p.communicate(timeout=10);raise AssertionError('CLI60s timeout '+label)
 stem=out/(str(serial).zfill(3)+'-'+label);stem.with_suffix('.stdout').write_bytes(stdout);stem.with_suffix('.stderr').write_bytes(stderr)
 d={'pid':p.pid,'argv':argv,'exit':p.returncode,'seconds':time.monotonic()-t,'stdout_bytes':len(stdout),'stderr_bytes':len(stderr),'stdout_file':str(stem.with_suffix('.stdout')),'stderr_file':str(stem.with_suffix('.stderr'))}
 try:d['payload']=json.loads(stdout)
 except ValueError:pass
 log('cli',**d);return d

def start(generation):
 global owner
 argv=[str(locald),'--workspace',str(state),'--endpoint',endpoint,'--idle-timeout-ms','0'];t=time.monotonic();owner=subprocess.Popen(argv,cwd=project,env=env,stdout=(out/('owner'+str(generation)+'.stdout')).open('wb'),stderr=(out/('owner'+str(generation)+'.stderr')).open('wb'),start_new_session=True)
 until=t+60
 while time.monotonic()<until:
  assert owner.poll() is None,'owner exited before ready'
  s=socket.socket(socket.AF_UNIX,socket.SOCK_STREAM)
  try:s.connect(endpoint);s.close();break
  except OSError:s.close();time.sleep(.1)
 else:raise AssertionError('owner live accept readiness60s exceeded')
 d={'pid':owner.pid,'argv':argv,'generation':generation,'ready_seconds':time.monotonic()-t};receipt['owners'].append(d);log('owner-ready',**d)
def stop():
 global owner
 if owner is not None:
  pid=owner.pid;owner.terminate();exit=owner.wait(timeout=20);log('owner-stop',pid=pid,exit=exit);owner=None

def ids(p):return [r['identity']['coordinate'] for r in p.get('records',[])]
def cli_page(route,token=None,limit=3,text='pagemark',label=None):return command(cli,[route,text,'--limit',str(limit)]+(['--cursor',token] if token else []),label or route)
def cli_walk(route,generation):
 token=None;pages=[];coordinates=[];pids=[]
 for i in range(30):
  d=cli_page(route,token,label=route+'-'+str(generation)+'-'+str(i));assert d['exit']==0,d;p=d['payload'];pages.append(p);coordinates+=ids(p);pids.append(d['pid']);token=p.get('nextCursor')
  if token is None:
   assert p.get('more') is False,p;break
  assert token.startswith('pc2-') and len(token)<=32768,'bounded pc2'
 else:raise AssertionError('unresolved CLI token')
 expected=references[route];passed=coordinates==expected and len(set(coordinates))==37 and len(pages)==13 and len(set(pids))==13
 receipt['checks']['cli-'+route+'-'+str(generation)]={'pass':passed,'identities':coordinates,'page_count':len(pages),'pids':pids,'token_sizes':[len(p['nextCursor']) for p in pages if p.get('nextCursor')]};assert passed,'CLI complete/order/fresh-process'
 return pages

mcp_state={}
def mcp_page(route,token,index):
 new=not mcp_state
 if new:
  argv=[str(mcp),*common];proc=subprocess.Popen(argv,cwd=project,env=env,stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=(out/'mcp.stderr').open('wb'),start_new_session=True);sel=selectors.DefaultSelector();sel.register(proc.stdout,selectors.EVENT_READ);mcp_state.update(proc=proc,sel=sel,buf=b'',seq=0)
 proc=mcp_state['proc'];sel=mcp_state['sel'];buf=mcp_state['buf'];seq=mcp_state['seq']
 def rpc(method,params=None,notify=False):
  nonlocal seq,buf
  seq+=1;req={'jsonrpc':'2.0','method':method,'params':params or {}}
  if not notify:req['id']=seq
  log('mcp-request',pid=proc.pid,payload=req);proc.stdin.write((json.dumps(req)+'\n').encode());proc.stdin.flush()
  if notify:return
  until=time.monotonic()+60
  while True:
   while b'\n' not in buf:
    assert sel.select(max(0,until-time.monotonic())),'MCP response60s timeout';data=os.read(proc.stdout.fileno(),65536);assert data,'MCP EOF';buf+=data
   raw,buf=buf.split(b'\n',1);reply=json.loads(raw);log('mcp-reply',pid=proc.pid,bytes=len(raw),payload=reply)
   if reply.get('id')==seq:return reply
 try:
  if new:
   rpc('initialize',{'protocolVersion':'2025-11-25','capabilities':{},'clientInfo':{'name':'Sol portable proof','version':'1'}});rpc('notifications/initialized',notify=True)
  args={'query':'pagemark','limit':3,'detail':'full'}
  if token:args['cursor']=token
  reply=rpc('tools/call',{'name':'backend.'+route,'arguments':args});assert 'error' not in reply and not reply.get('result',{}).get('isError'),reply
  return reply['result']['structuredContent'],proc.pid
 finally:mcp_state.update(buf=buf,seq=seq)

def mcp_walk(route):
 token=None;coordinates=[];pids=[];pages=[]
 for i in range(30):
  p,pid=mcp_page(route,token,i);pids.append(pid);pages.append(p);coordinates+=ids(p);token=p.get('nextCursor')
  if not token:assert p.get('more') is False;break
 else:raise AssertionError('unresolved MCP token')
 expected=references[route];passed=coordinates==expected and len(set(coordinates))==37 and len(pages)==13 and len(set(pids))==1
 receipt['checks']['persistent-mcp-'+route]={'pass':passed,'identities':coordinates,'page_count':len(pages),'pids':pids};assert passed,'MCP persistent process parity'

def mutate(token,change):
 data=json.loads(bytes.fromhex(token[4:]));change(data);return 'pc2-'+json.dumps(data,separators=(',',':')).encode().hex()
try:
 start(1);old=command(oldcli,['resolve','pagemark','--limit','3'],'before-old-cli');receipt['before']=old;assert old['exit']==0 and old['payload']['more'] and not old['payload'].get('nextCursor'),'before failure'
 for route in ['resolve','search']:
  ref=cli_page(route,limit=200,label='independent-reference-'+route);assert ref['exit']==0 and ref['payload'].get('more') is False,ref
  references[route]=ids(ref['payload']);expected=witness['names_reference' if route=='resolve' else 'search_reference']['identities']
  assert len(references[route])==37 and set(references[route])==set(expected),'preserved source-coordinate fidelity'
 receipt['independent_references']=references
 names=cli_walk('resolve',1);search=cli_walk('search',1);token=names[0]['nextCursor'];receipt['token_before_cold']=token
 for label,route,text,limit,raw in [('family','search','pagemark',3,token),('text','resolve','pagemark_other',3,token),('credit-down','resolve','pagemark',1,token),('credit-up','resolve','pagemark',4,token),('malformed','resolve','pagemark',3,'pc2-zz'),('oversize','resolve','pagemark',3,'pc2-'+'00'*16385)]:
  d=cli_page(route,raw,limit,text,'negative-'+label);passed=d['exit']!=0 and d['stdout_bytes']+d['stderr_bytes']<=8192;receipt['checks']['negative-'+label]={'pass':passed,'exit':d['exit'],'body_bytes':d['stdout_bytes']+d['stderr_bytes']};assert passed,label
 for label,change in [
  ('zero-offset',lambda d:d['command']['data']['cursor'].__setitem__('query_offset',0)),
  ('forward-offset',lambda d:d['command']['data']['cursor'].__setitem__('query_offset',6)),
  ('forward-sequence',lambda d:d['command']['data']['cursor'].__setitem__('sequence',999999)),
  ('manifest',lambda d:d['command']['data'].__setitem__('read_manifest',[])),
  ('scope-context',lambda d:next(c for c in d['certificate']['claims'] if c['kind']=='coverage')['data'].__setitem__('context','12'*32)),
 ]:
  changed=mutate(token,change);d=cli_page('resolve',changed,label='tamper-'+label);passed=d['exit']!=0 and d['stdout_bytes']+d['stderr_bytes']<=8192;receipt['checks']['tamper-'+label]={'pass':passed,'exit':d['exit'],'body_bytes':d['stdout_bytes']+d['stderr_bytes']};assert passed,label
 stop();start(2);resumed=cli_page('resolve',token,label='pre-cold-token');passed=resumed['exit']==0 and ids(resumed['payload'])==ids(names[1]);receipt['checks']['pre-cold-token']={'pass':passed};assert passed,'pre-cold token'
 cli_walk('resolve',2);cli_walk('search',2);mcp_walk('resolve');mcp_walk('search');receipt['complete']=True;receipt['pass']=all(v['pass'] for v in receipt['checks'].values())
except Exception as e:receipt['exception']=repr(e);receipt['pass']=False
finally:
 if mcp_state:
  mcp_state['proc'].stdin.close();mcp_state['proc'].wait(timeout=20);mcp_state['sel'].close()
 stop();wire.close();receipt['remaining_owned_owners']=0;(out/'receipt.json').write_text(json.dumps(receipt,indent=2)+'\n');print(json.dumps({'pass':receipt.get('pass'),'exception':receipt.get('exception'),'checks':{k:v['pass'] for k,v in receipt['checks'].items()},'output':str(out)},indent=2))
