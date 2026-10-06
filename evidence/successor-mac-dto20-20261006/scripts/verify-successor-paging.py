import pathlib,subprocess,json,hashlib,shutil,os,signal,time,uuid,selectors,math,ast,threading
root=pathlib.Path(__file__).resolve().parent.parent;local=root/'.local';build=json.loads((local/'successor-3a3-build/receipt.json').read_text());assert build['exit']==0 and build['source_unchanged'] and build['controls_unchanged']
out=local/'successor-3a3-runtime';out.mkdir(mode=0o700);home=out/'home';home.mkdir(mode=0o700);tmp=out/'tmp';tmp.mkdir(mode=0o700);programs=home/'.local/libexec/nudox';programs.mkdir(parents=True);bindir=home/'.local/bin';bindir.mkdir();bins={}
def sha(p):return hashlib.file_digest(pathlib.Path(p).open('rb'),'sha256').hexdigest()
for key,name in [('cli','backend-cli'),('mcp','backend-mcp'),('locald','backend-locald')]:
 p=local/'successor-3a3-images'/name;assert sha(p)==build['images'][name]['sha256'];shutil.copy2(p,programs/name);q=bindir/('nudox' if key=='cli' else 'nudox-'+key);q.symlink_to(programs/name);bins[key]=q
provider=json.loads((local/'current-minimal/python/receipt.json').read_text());env=dict(provider['environment']);env.update(HOME=str(home),TMPDIR=str(tmp),PATH=str(bindir)+':/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin');env.pop('NUDOX_TYPESCRIPT_MODULE_ROOT',None);env.pop('NUDOX_TSC',None)
receipt={'source':build['source_before'],'build_receipt_sha256':sha(local/'successor-3a3-build/receipt.json'),'profile':build['profile'],'scope':'actual local matched debug Mac installed symlinks with reused real native provider files; not fresh OS/release/tool bootstrap acceptance','environment':env,'images':{k:{'path':str(v),'sha256':sha(v)} for k,v in bins.items()},'provider_sha256':{k:sha(v) for k,v in env.items() if k in ('NUDOX_PYTHON','NUDOX_PYREFLY','NUDOX_GO','NUDOX_TYPESCRIPT_NODE') and pathlib.Path(v).is_file()},'events':[],'checks':{}};wire=(out/'wire.jsonl').open('w');mcp=None;selector=None;owners_seen=set();samples=[];done=threading.Event();began=time.monotonic()
def log(kind,**kw):
 event={'kind':kind,**kw};receipt['events'].append(event);wire.write(json.dumps(event)+'\n');wire.flush()
def command(argv,childenv=env,cwd=None,timeout=90):
 t=time.monotonic()
 try:v=subprocess.run(list(map(str,argv)),env=childenv,cwd=cwd or out,capture_output=True,text=True,timeout=timeout);d={'argv':list(map(str,argv)),'exit':v.returncode,'stdout':v.stdout,'stderr':v.stderr,'seconds':time.monotonic()-t}
 except subprocess.TimeoutExpired as e:d={'argv':list(map(str,argv)),'timeout':timeout,'stdout':str(e.stdout),'stderr':str(e.stderr),'seconds':time.monotonic()-t}
 try:d['payload']=json.loads(d['stdout'])
 except (ValueError,TypeError):pass
 log('command',**d);return d
project=out/'projects/python';project.mkdir(parents=True);(project/'pyproject.toml').write_text('[project]\nname="fresh_paging"\nversion="1.0.0"\nrequires-python=">=3.11"\n');body=''
for i in range(36):body+=f'def pagemark_{i:02}(name: str) -> str:\n    """Paging marker {i:02}."""\n    return "Hello " + name\n\n'
body+='def pagemark_calls() -> str:\n    return pagemark_00("visitor")\n';(project/'model.py').write_text(body);prefix='class Requests:\n    """';suffix='"""\n    def first(self) -> str:\n        return "hello"\n    def last(self) -> str:\n        return self.first()';long=prefix+'x'*(19028-len(prefix)-len(suffix))+suffix;assert len(long.encode())==19028;(project/'requests.py').write_text(long+'\n');node=ast.parse(long).body[0];assert len(ast.get_source_segment(long,node).encode())==19028
lane=out/'paging';lane.mkdir(mode=0o700);state=lane/'state';endpoint='/tmp/nudox-page-'+uuid.uuid4().hex+'.sock';common=['--project',str(project),'--workspace',str(state),'--endpoint',endpoint]
def cli(args):return command([bins['cli'],'--json','--detail','full',*common,*args],cwd=project)
def census():
 rows=subprocess.check_output(['/bin/ps','-ww','-axo','pid,ppid,rss,command'],text=True).splitlines();return [x for x in rows if str(programs/'backend-locald')+' ' in x and str(out) in x]
def current_owner():
 rows=[x for x in census() if endpoint in x and str(state) in x];assert len(rows)==1,rows;pid=int(rows[0].split()[0]);owners_seen.add(pid);log('owner',pid=pid,row=rows[0]);return pid
def stop(pid):
 assert any(int(x.split()[0])==pid for x in census());os.kill(pid,signal.SIGTERM);until=time.monotonic()+20
 while any(int(x.split()[0])==pid for x in census()):
  assert time.monotonic()<until,'owner shutdown timeout';time.sleep(.1)
 owners_seen.discard(pid);log('owner-stopped',pid=pid)
def payload(d):return d.get('payload',d)
def mcp_data(d):return d.get('result',{}).get('structuredContent',d.get('result',d))
def record_names(p):return [r.get('identity',{}).get('name') for r in p.get('records',[]) if isinstance(r,dict)]
def coordinates(p):
 if isinstance(p,dict):
  if isinstance(p.get('coordinate'),str):yield p['coordinate']
  for v in p.values():yield from coordinates(v)
 elif isinstance(p,list):
  for v in p:yield from coordinates(v)
def rpc(method,params=None,notify=False):
 global sequence,buffer
 sequence+=1;q={'jsonrpc':'2.0','method':method,'params':params or {}}
 if not notify:q['id']=sequence
 log('mcp-request',payload=q);mcp.stdin.write((json.dumps(q)+'\n').encode());mcp.stdin.flush()
 if notify:return
 t=time.monotonic();deadline=t+60
 while True:
  while b'\n' not in buffer:
   assert selector.select(max(0,deadline-time.monotonic())),'MCP response timeout';data=os.read(mcp.stdout.fileno(),65536);assert data,'MCP EOF';buffer+=data
  raw,buffer=buffer.split(b'\n',1);reply=json.loads(raw)
  log('mcp-reply',payload=reply,bytes=len(raw)+1,estimated_tokens=math.ceil((len(raw)+1)/4),seconds=time.monotonic()-t)
  if reply.get('id')==sequence:return reply
  assert 'id' not in reply,'unexpected MCP id'
def tool(name,args):return rpc('tools/call',{'name':'backend.'+name,'arguments':{'detail':'full',**args}})
def page_cli():
 seen=[];pages=[];token=None
 for n in range(40):
  args=['resolve','pagemark','--limit','3']+(['--cursor',token] if token else []);d=cli(args);p=payload(d);pages.append(p);assert d.get('exit')==0,p;seen.extend(record_names(p));token=p.get('nextCursor')
  if not token:break
 return {'pages':pages,'names':seen,'complete':not token,'unique':len(set(seen)),'expected':37,'pass':not token and len(seen)==37 and len(set(seen))==37}
def page_mcp(name):
 seen=[];pages=[];token=None
 for n in range(40):
  args={'query':'pagemark','limit':3};
  if token:args['cursor']=token
  reply=tool(name,args);p=mcp_data(reply);pages.append(p)
  if 'error' in reply or reply.get('result',{}).get('isError'):return {'pages':pages,'names':seen,'error':reply,'pass':False}
  seen.extend(record_names(p));token=p.get('nextCursor')
  if not token:break
 return {'pages':pages,'names':seen,'complete':not token,'unique':len(set(seen)),'expected':37,'pass':not token and len(seen)==37 and len(set(seen))==37}
def sampler():
 while not done.is_set():
  rows=[]
  for line in subprocess.check_output(['/bin/ps','-ww','-axo','pid,ppid,rss,command'],text=True).splitlines()[1:]:
   f=line.strip().split(None,3)
   if len(f)==4:rows.append({'pid':int(f[0]),'ppid':int(f[1]),'rss_kib':int(f[2]),'command':f[3]})
  ids={os.getpid()};ids.update(x['pid'] for x in rows if x['command'].startswith(str(programs/'backend-locald')+' ') and str(out) in x['command'])
  while True:
   added={x['pid'] for x in rows if x['ppid'] in ids}-ids
   if not added:break
   ids.update(added)
  selected=[x for x in rows if x['pid'] in ids];samples.append({'seconds':time.monotonic()-began,'rss_kib':sum(x['rss_kib'] for x in selected),'processes':selected});done.wait(.25)
thread=threading.Thread(target=sampler,daemon=True);thread.start()
try:
 receipt['fixture_hashes']={str(p.relative_to(project)):sha(p) for p in project.rglob('*') if p.is_file()};receipt['native_python']=command([env['NUDOX_PYTHON'],'-m','py_compile',str(project/'model.py'),str(project/'requests.py')],cwd=project)
 receipt['warm_health']=cli(['health']);warm=current_owner();receipt['add']=cli(['add',str(project)]);receipt['published_health']=cli(['health']);receipt['packages']=cli(['packages']);receipt['outline']=cli(['outline',str(project)]);receipt['checks']['cli_names_paging']=page_cli();receipt['cli_search']=cli(['search','pagemark','--limit','3'])
 mcp=subprocess.Popen([str(bins['mcp']),*common],env=env,cwd=project,stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=(out/'mcp.stderr').open('wb'),start_new_session=True);sequence=0;buffer=b'';selector=selectors.DefaultSelector();selector.register(mcp.stdout,selectors.EVENT_READ);receipt['mcp_pid']=mcp.pid;rpc('initialize',{'protocolVersion':'2025-11-25','capabilities':{},'clientInfo':{'name':'Current Mac continuation proof','version':'1'}});rpc('notifications/initialized',notify=True);receipt['tools']=rpc('tools/list');receipt['checks']['mcp_names_paging']=page_mcp('resolve');receipt['checks']['mcp_search_paging']=page_mcp('search')
 first=mcp_data(tool('resolve',{'query':'pagemark','limit':3}));old_token=first.get('nextCursor');receipt['cursor_before_restart']=old_token;receipt['before_cold_status']=tool('status',{});stop(warm);receipt['cold_health']=cli(['health']);cold=current_owner();receipt['checks']['cold_new_owner']=cold!=warm;receipt['after_cold_status_same_mcp']=tool('status',{});assert mcp.pid==receipt['mcp_pid']
 if old_token:receipt['old_cursor_after_cold']=tool('resolve',{'query':'pagemark','limit':3,'cursor':old_token})
 receipt['checks']['cold_cli_names_paging']=page_cli();receipt['checks']['cold_same_mcp_names_paging']=page_mcp('resolve');receipt['checks']['cold_same_mcp_search_paging']=page_mcp('search');receipt['requests_resolve']=cli(['resolve','Requests','--limit','5']);targets=list(dict.fromkeys(coordinates(payload(receipt['requests_resolve']))));receipt['requests_source']=[cli(['source',x]) for x in targets[:2]]
 for x in targets[:1]:receipt['requests_mcp_source_full']=tool('source',{'coordinate':x});receipt['requests_mcp_range_negative']=tool('source',{'coordinate':x,'start_line':1,'end_line':200})
 receipt['retry_add']=cli(['add',str(project)]);receipt['unchanged_add']=cli(['add',str(project)]);receipt['post_retry_search']=cli(['search','pagemark','--limit','3']);receipt['complete']=True
except Exception as e:receipt['exception']=repr(e)
finally:
 if mcp is not None:
  try:mcp.stdin.close();log('mcp-stop',pid=mcp.pid,exit=mcp.wait(timeout=20))
  except Exception:mcp.terminate();mcp.wait(timeout=10)
 if selector is not None:selector.close()
 for row in census():
  try:stop(int(row.split()[0]))
  except Exception as e:log('cleanup-error',error=repr(e),row=row)
 receipt['remaining_owners']=census();done.set();thread.join(timeout=2);wire.close();receipt['runtime_seconds']=time.monotonic()-began;(out/'resource-census.json').write_text(json.dumps({'samples':samples,'peak_rss_kib':max(x['rss_kib'] for x in samples),'peak_process_count':max(len(x['processes']) for x in samples)},indent=2)+'\n');(out/'receipt.json').write_text(json.dumps(receipt,indent=2)+'\n');print(json.dumps({'checks':receipt['checks'],'exception':receipt.get('exception'),'remaining':len(receipt['remaining_owners'])},indent=2))
