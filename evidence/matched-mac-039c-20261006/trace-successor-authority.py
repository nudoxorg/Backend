from pathlib import Path
import socket,threading,json,subprocess,os,signal,time,uuid,selectors,struct,hashlib
local=Path(__file__).resolve().parent;base=Path(os.environ['NUDOX_VERIFY_FRESH_SETUP']);out=Path(os.environ['NUDOX_VERIFY_TRACE_OUTPUT']);out.mkdir(mode=0o700);setup=json.loads((base/'setup-receipt.json').read_text());cli=Path(setup['installed_links']['cli']['path']);mcpbin=Path(setup['installed_links']['mcp']['path']);daemon=Path(setup['installed_links']['locald']['path']).resolve()
for name in ['typescript','go']:
 lane=out/name;lane.mkdir(mode=0o700);prior=json.loads((base/name/'receipt.json').read_text());env=prior['environment'];project=base/'projects'/name;state=lane/'state';endpoint='/tmp/nudox-stale-'+uuid.uuid4().hex+'.sock';proxy='/tmp/nudox-stale-proxy-'+uuid.uuid4().hex+'.sock';common=['--project',str(project),'--workspace',str(state),'--endpoint',endpoint];r={'source':setup['source'],'language':name,'environment':env,'images':{k:{'canonical':str(Path(v['path']).resolve()),'sha256':hashlib.file_digest(Path(v['path']).open('rb'),'sha256').hexdigest()} for k,v in setup['installed_links'].items()},'events':[],'connections':[]};threads=[];done=threading.Event();mcp=None;selector=None;acceptor=None;listener=socket.socket(socket.AF_UNIX);listener.bind(proxy);os.chmod(proxy,0o600);listener.listen();listener.settimeout(.2);stage='setup'
 def cli_call(args,via=True):
  argv=[str(cli),'--json','--detail','full',*common,*args]
  if via:argv[argv.index('--endpoint')+1]=proxy
  p=subprocess.run(argv,env=env,cwd=project,capture_output=True,text=True,timeout=60);d={'kind':'cli','stage':stage,'argv':argv,'exit':p.returncode,'stdout':p.stdout,'stderr':p.stderr}
  try:d['payload']=json.loads(p.stdout)
  except ValueError:pass
  r['events'].append(d);return d
 def owned():return [x for x in subprocess.check_output(['/bin/ps','-ww','-axo','pid,ppid,rss,command'],text=True).splitlines() if str(daemon)+' ' in x and endpoint in x and str(state) in x]
 def stopowner():
  rows=owned();assert len(rows)==1,rows;r['events'].append({'kind':'owner','stage':stage,'row':rows[0]});pid=int(rows[0].split()[0]);os.kill(pid,signal.SIGTERM);deadline=time.monotonic()+20
  while owned():assert time.monotonic()<deadline;time.sleep(.1)
 def connection(client,n):
  d={'connection':n,'stage_open':stage,'streams':{}};r['connections'].append(d);up=socket.socket(socket.AF_UNIX)
  try:up.connect(endpoint)
  except OSError as e:d['connect_error']=repr(e);client.close();up.close();return
  def relay(a,b,direction):
   p=lane/f'{n}-{direction}.bin';count=0
   with p.open('wb') as f:
    try:
     while True:
      chunk=a.recv(65536)
      if not chunk:break
      count+=len(chunk);assert count<=8*1024*1024;f.write(chunk);b.sendall(chunk)
    except (ConnectionError,OSError):pass
    finally:
     try:b.shutdown(socket.SHUT_WR)
     except OSError:pass
   d['streams'][direction]={'path':str(p),'bytes':count,'sha256':hashlib.file_digest(p.open('rb'),'sha256').hexdigest()}
  a=threading.Thread(target=relay,args=(client,up,'request'));b=threading.Thread(target=relay,args=(up,client,'reply'));a.start();b.start();a.join();b.join();client.close();up.close()
 def accept():
  n=0
  while not done.is_set():
   try:c,_=listener.accept()
   except socket.timeout:continue
   n+=1;t=threading.Thread(target=connection,args=(c,n));threads.append(t);t.start()
 def rpc(method,params=None,notify=False):
  global sequence,buf
  sequence+=1;q={'jsonrpc':'2.0','method':method,'params':params or {}}
  if not notify:q['id']=sequence
  r['events'].append({'kind':'mcp-request','stage':stage,'payload':q});mcp.stdin.write((json.dumps(q)+'\n').encode());mcp.stdin.flush()
  if notify:return
  deadline=time.monotonic()+60
  while b'\n' not in buf:
   assert selector.select(max(0,deadline-time.monotonic())),'MCP timeout';b=os.read(mcp.stdout.fileno(),65536);assert b,'MCP EOF';buf+=b
  line,buf=buf.split(b'\n',1);v=json.loads(line);assert v['id']==sequence;r['events'].append({'kind':'mcp-reply','stage':stage,'payload':v});return v
 def tool(n,a={}):return rpc('tools/call',{'name':'backend.'+n,'arguments':{'detail':'full',**a}})
 try:
  cli_call(['health'],via=False);acceptor=threading.Thread(target=accept);acceptor.start();args=[str(mcpbin),*common];args[args.index('--endpoint')+1]=proxy;mcp=subprocess.Popen(args,env=env,cwd=project,stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=(lane/'mcp.stderr').open('wb'),start_new_session=True);selector=selectors.DefaultSelector();selector.register(mcp.stdout,selectors.EVENT_READ);sequence=0;buf=b'';r['mcp_pid']=mcp.pid;rpc('initialize',{'protocolVersion':'2025-11-25','capabilities':{},'clientInfo':{'name':'Exact refusedauthority binding trace','version':'1'}});rpc('notifications/initialized',notify=True)
  stage='before-add';tool('status');tool('search',{'query':'welcome' if name=='typescript' else 'Welcome','limit':3});stage='single-refused-add';cli_call(['add',str(project)]);stage='after-refused-add';tool('status');tool('search',{'query':'welcome' if name=='typescript' else 'Welcome','limit':3});cli_call(['health']);cli_call(['search','welcome' if name=='typescript' else 'Welcome','--limit','3']);stage='cold';stopowner();cli_call(['health'],via=False);r['same_mcp_alive']=mcp.poll() is None;tool('status');tool('search',{'query':'welcome' if name=='typescript' else 'Welcome','limit':3});cli_call(['search','welcome' if name=='typescript' else 'Welcome','--limit','3']);r['complete']=True
 except Exception as e:r['exception']=repr(e)
 finally:
  if mcp is not None:
   mcp.stdin.close()
   try:r['mcp_exit']=mcp.wait(timeout=10)
   except subprocess.TimeoutExpired:mcp.terminate();r['mcp_exit']=mcp.wait(timeout=10)
  if owned():stopowner()
  done.set()
  if acceptor is not None:acceptor.join(timeout=5)
  listener.close()
  for t in threads:t.join(timeout=5);assert not t.is_alive()
  if selector is not None:selector.close()
  os.unlink(proxy);r['remaining_owners']=owned();(lane/'receipt.json').write_text(json.dumps(r,indent=2)+'\n');print(name,'complete',r.get('complete'),'exception',r.get('exception'),'remaining',len(r['remaining_owners']),flush=True)
  frameindex=[]
  for d in r['connections']:
   for direction,s in d['streams'].items():
    b=Path(s['path']).read_bytes();at=0;n=0
    while at<len(b):
     length=struct.unpack('>I',b[at:at+4])[0];raw=b[at+4:at+4+length];assert len(raw)==length;at+=4+length;n+=1
     try:v=json.loads(raw)
     except (ValueError,UnicodeError):continue
     p=lane/f'{d["connection"]}-{direction}-{n}-dto.json';p.write_bytes(raw);frameindex.append({'path':str(p),'direction':direction,'connection':d['connection'],'stage_open':d['stage_open'],'version':v.get('version'),'request_id':v.get('request_id'),'command':v.get('command'),'reply_kind':v.get('reply',{}).get('kind'),'reply_error':v.get('reply',{}).get('data',{}).get('message')})
  (lane/'frame-index.json').write_text(json.dumps(frameindex,indent=2)+'\n')
