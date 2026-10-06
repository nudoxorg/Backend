from pathlib import Path
import socket,threading,subprocess,json,os,signal,time,uuid,hashlib
base=Path(__file__).resolve().parent/'successor-3a3-runtime';out=base.parent/'successor-3a3-wire-capture';out.mkdir(mode=0o700);r=json.loads((base/'receipt.json').read_text());env=r['environment'];query=next(e for e in r['events'] if e['kind']=='command' and 'resolve' in e['argv']);original=list(query['argv']);endpoint=original[original.index('--endpoint')+1];state=original[original.index('--workspace')+1];image=Path(r['images']['locald']['path']).resolve();result={'original_first_query':query,'source':r['source'],'images':{},'connections':[]};threads=[];stop=threading.Event();proxy='/tmp/nudox-wire-'+uuid.uuid4().hex+'.sock'
for k,v in r['images'].items():
 p=Path(v['path']);result['images'][k]={'symlink':str(p),'canonical':str(p.resolve()),'sha256':hashlib.file_digest(p.open('rb'),'sha256').hexdigest(),'receipt_sha256':v['sha256']};assert result['images'][k]['sha256']==v['sha256']
def owned():return [x for x in subprocess.check_output(['/bin/ps','-ww','-axo','pid,ppid,rss,command'],text=True).splitlines() if str(image)+' ' in x and endpoint in x and state in x]
def connect(client,n):
 upstream=socket.socket(socket.AF_UNIX);upstream.connect(endpoint);d={'connection':n,'streams':{}};result['connections'].append(d)
 def relay(a,b,name):
  path=out/f'{n}-{name}.bin';total=0
  with path.open('wb') as f:
   try:
    while True:
     chunk=a.recv(65536)
     if not chunk:break
     total+=len(chunk);assert total<=8*1024*1024;f.write(chunk);b.sendall(chunk)
   except (ConnectionError,OSError):pass
   finally:
    try:b.shutdown(socket.SHUT_WR)
    except OSError:pass
  d['streams'][name]={'path':str(path),'bytes':total,'sha256':hashlib.file_digest(path.open('rb'),'sha256').hexdigest()}
 a=threading.Thread(target=relay,args=(client,upstream,'request'));b=threading.Thread(target=relay,args=(upstream,client,'reply'));a.start();b.start();a.join();b.join();client.close();upstream.close()
listener=socket.socket(socket.AF_UNIX);listener.bind(proxy);os.chmod(proxy,0o600);listener.listen();listener.settimeout(.2)
def accept():
 n=0
 while not stop.is_set():
  try:client,_=listener.accept()
  except socket.timeout:continue
  n+=1;t=threading.Thread(target=connect,args=(client,n));threads.append(t);t.start()
try:
 health=original[:original.index('resolve')]+['health'];p=subprocess.run(health,env=env,capture_output=True,text=True,timeout=60);result['health']={'exit':p.returncode,'stdout':p.stdout,'stderr':p.stderr};result['actual_owners_before']=owned();assert len(result['actual_owners_before'])==1
 acceptor=threading.Thread(target=accept);acceptor.start();argv=original[:];argv[argv.index('--endpoint')+1]=proxy;result['proxy_query_argv']=argv;p=subprocess.run(argv,env=env,capture_output=True,text=True,timeout=60);result['proxy_query']={'exit':p.returncode,'stdout':p.stdout,'stderr':p.stderr};stop.set();acceptor.join();listener.close()
 for t in threads:t.join(timeout=10);assert not t.is_alive()
finally:
 stop.set();listener.close()
 if os.path.exists(proxy):os.unlink(proxy)
 for row in owned():
  pid=int(row.split()[0]);os.kill(pid,signal.SIGTERM)
  deadline=time.monotonic()+20
  while any(int(x.split()[0])==pid for x in owned()):assert time.monotonic()<deadline;time.sleep(.1)
 result['remaining_owners']=owned();(out/'receipt.json').write_text(json.dumps(result,indent=2)+'\n');print(json.dumps({'proxy_query':result.get('proxy_query'),'connections':result['connections'],'remaining':len(result['remaining_owners'])},indent=2))
