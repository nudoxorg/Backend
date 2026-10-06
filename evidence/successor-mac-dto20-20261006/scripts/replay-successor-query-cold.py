from pathlib import Path
import json,subprocess,selectors,os,signal,time,uuid,math
root=Path(__file__).resolve().parent.parent;local=root/'.local';old=json.loads((local/'successor-3a3-runtime/receipt.json').read_text());base=local/'successor-3a3-runtime';out=local/'successor-3a3-query-cold-replay';out.mkdir(mode=0o700);env=old['environment'];project=base/'projects/python';state=base/'paging/state';endpoint=next(e['argv'][e['argv'].index('--endpoint')+1] for e in old['events'] if e['kind']=='command' and '--endpoint' in e['argv']);bins={k:Path(v['path']) for k,v in old['images'].items()};common=['--project',str(project),'--workspace',str(state),'--endpoint',endpoint];result={'scope':'same new3a3state actual CLI/MCP replay after first names refusal; original59fd failure states untouched','source':old['source'],'environment':env,'events':[]};wire=(out/'wire.jsonl').open('w');mcp=None;selector=None

def log(kind,**kw):
 d={'kind':kind,**kw};result['events'].append(d);wire.write(json.dumps(d)+'\n');wire.flush()
def cli(args):
 t=time.monotonic();argv=[str(bins['cli']),'--json','--detail','full',*common,*args];p=subprocess.run(argv,env=env,cwd=project,capture_output=True,text=True,timeout=90);d={'argv':argv,'exit':p.returncode,'stdout':p.stdout,'stderr':p.stderr,'seconds':time.monotonic()-t}
 try:d['payload']=json.loads(p.stdout)
 except ValueError:pass
 log('cli',**d);return d

def owners():return [x for x in subprocess.check_output(['/bin/ps','-ww','-axo','pid,ppid,rss,command'],text=True).splitlines() if str(bins['locald'].resolve())+' ' in x and endpoint in x and str(state) in x]
def owner():
 rows=owners();assert len(rows)==1,rows;pid=int(rows[0].split()[0]);log('owner',pid=pid,row=rows[0]);return pid
def stop(pid):
 assert any(int(x.split()[0])==pid for x in owners());os.kill(pid,signal.SIGTERM);deadline=time.monotonic()+20
 while owners():
  assert time.monotonic()<deadline;time.sleep(.1)
 log('owner-stopped',pid=pid)
def rpc(method,params=None,notify=False):
 global seq,buf
 seq+=1;q={'jsonrpc':'2.0','method':method,'params':params or {}}
 if not notify:q['id']=seq
 log('mcp-request',payload=q);mcp.stdin.write((json.dumps(q)+'\n').encode());mcp.stdin.flush()
 if notify:return
 began=time.monotonic();deadline=began+60
 while b'\n' not in buf:
  assert selector.select(max(0,deadline-time.monotonic())),'MCP timeout';chunk=os.read(mcp.stdout.fileno(),65536);assert chunk,'MCP EOF';buf+=chunk
 raw,buf=buf.split(b'\n',1);p=json.loads(raw);assert p['id']==seq;log('mcp-reply',method=method,payload=p,seconds=time.monotonic()-began,bytes=len(raw)+1,estimated_tokens=math.ceil((len(raw)+1)/4));return p

def tool(name,args):return rpc('tools/call',{'name':'backend.'+name,'arguments':{'detail':'full',**args}})
def body(d):return d.get('result',{}).get('structuredContent',d.get('result',d))
def page(name):
 pages=[];token=None
 for i in range(40):
  args={'query':'pagemark','limit':3}
  if token:args['cursor']=token
  reply=tool(name,args);p=body(reply);pages.append(p)
  if 'error' in reply or reply.get('result',{}).get('isError'):return {'pages':pages,'error':reply,'complete':False,'pass':False}
  token=p.get('nextCursor')
  if not token:return {'pages':pages,'complete':True,'page_count':len(pages),'continuation_exercised':len(pages)>1}
 return {'pages':pages,'complete':False,'pass':False}
try:
 result['initial_cold_health']=cli(['health']);first=owner();result['cli_names']=cli(['resolve','pagemark','--limit','3']);result['cli_search']=cli(['search','pagemark','--limit','3']);outline=cli(['outline',str(project)]);result['outline']=outline
 def classcoords(v):
  if isinstance(v,dict):
   if v.get('name')=='Requests' and isinstance(v.get('coordinate'),str):yield v['coordinate']
   for x in v.values():yield from classcoords(x)
  elif isinstance(v,list):
   for x in v:yield from classcoords(x)
 coords=list(dict.fromkeys(classcoords(outline.get('payload',{}))));result['Requests_coordinates_from_actual_outline']=coords;result['class_ast_bytes']=19028
 for c in coords[:2]:result.setdefault('source_full',[]).append(cli(['source',c]))
 mcp=subprocess.Popen([str(bins['mcp']),*common],cwd=project,env=env,stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=(out/'mcp.stderr').open('wb'),start_new_session=True);seq=0;buf=b'';selector=selectors.DefaultSelector();selector.register(mcp.stdout,selectors.EVENT_READ);result['mcp_pid']=mcp.pid;rpc('initialize',{'protocolVersion':'2025-11-25','capabilities':{},'clientInfo':{'name':'Sameprocesscold continuation','version':'1'}});rpc('notifications/initialized',notify=True);result['tools']=rpc('tools/list');result['warm_names']=page('resolve');result['warm_search']=page('search')
 for c in coords[:1]:result['mcp_source_full']=tool('source',{'coordinate':c});result['mcp_source_range_negative']=tool('source',{'coordinate':c,'start_line':1,'end_line':200})
 stop(first);result['cold_health']=cli(['health']);second=owner();result['owner_changed']=first!=second;result['same_mcp_alive']=mcp.poll() is None;result['cold_mcp_status']=tool('status',{});result['cold_names']=page('resolve');result['cold_search']=page('search');result['cold_cli_names']=cli(['resolve','pagemark','--limit','3']);result['cold_cli_search']=cli(['search','pagemark','--limit','3']);result['retry_add']=cli(['add',str(project)]);result['unchanged_add']=cli(['add',str(project)]);result['post_retry_names']=cli(['resolve','pagemark','--limit','3']);result['complete']=True
except Exception as e:result['exception']=repr(e)
finally:
 if mcp is not None:
  try:mcp.stdin.close();log('mcp-stop',pid=mcp.pid,exit=mcp.wait(timeout=20))
  except Exception:mcp.terminate();mcp.wait(timeout=10)
 if selector is not None:selector.close()
 for row in owners():
  try:stop(int(row.split()[0]))
  except Exception as e:log('cleanup-error',row=row,error=repr(e))
 result['remaining_owners']=owners();wire.close();(out/'receipt.json').write_text(json.dumps(result,indent=2)+'\n');print(json.dumps({'complete':result.get('complete'),'exception':result.get('exception'),'same_mcp_alive':result.get('same_mcp_alive'),'owner_changed':result.get('owner_changed'),'remaining':len(result['remaining_owners'])},indent=2))
