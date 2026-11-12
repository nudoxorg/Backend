#!/usr/bin/env python3
"""Report 35 exact NuGet discovery through CLI/MCP; no acquisition.

Run with verified source-bound binaries and an explicitly configured loopback provider.
``--self-check`` exercises only fixture plumbing; it is not product acceptance.
"""
import argparse, hashlib, http.client, http.server, json, os, pathlib, secrets, shutil, signal, socket, struct, subprocess, sys, tempfile, threading, time, urllib.parse

CONTRACT_COMMIT = '8b28e7eae769bb3da48c2e3fd3a466ed42649db0'
CONTRACT_TREE = '523ea36f223b13af1a970cce3609da8f9897db67'
PURL = 'pkg:nuget/Microsoft.EntityFrameworkCore@10.0.13'
NAME = 'Microsoft.EntityFrameworkCore'
DESCRIPTION = 'Entity Framework metadata from private fixture'
MAX_OUTPUT = 16 * 1024 * 1024
NAMES = ('backend-cli','backend-mcp','backend-locald')

class Failure(Exception): pass

def check(cond, text):
    if not cond: raise Failure(text)

def digest(path):
    h=hashlib.sha256()
    with open(path,'rb') as f:
        for chunk in iter(lambda:f.read(1024*1024),b''): h.update(chunk)
    return h.hexdigest()

BLAKE3_HELPER = None

def blake3_short(data):
    check(len(data)<=4096,'fixture proof input exceeds bound')
    check(BLAKE3_HELPER is not None,'native BLAKE3 helper required')
    result=subprocess.run([str(BLAKE3_HELPER)],input=data,stdout=subprocess.PIPE,stderr=subprocess.PIPE,timeout=10,check=True)
    text=result.stdout.decode('ascii').strip()
    check(len(text)==64 and all(c in '0123456789abcdef' for c in text),'invalid native BLAKE3 digest')
    return bytes.fromhex(text)

class Fixture:
    @classmethod
    def create(cls, scenario):
        fixture=cls.__new__(cls)
        try:fixture.__init__(scenario)
        except BaseException:
            fixture.close()
            raise
        return fixture
    def __init__(self, scenario):
        self.scenario=scenario; self.events=[]; self.bodies={}; self.trap_events=[]; self.nonce=secrets.token_hex(12)
        fixture=self
        class Trap(http.server.BaseHTTPRequestHandler):
            def log_message(self,*args): pass
            def do_GET(self):
                fixture.trap_events.append(self.path); self.send_response(403); self.send_header('Content-Length','0'); self.end_headers()
        self.trap=http.server.ThreadingHTTPServer(('127.0.0.1',0),Trap); self.trap.daemon_threads=True
        self.trap_thread=threading.Thread(target=self.trap.serve_forever,daemon=True); self.trap_thread.start()
        class Handler(http.server.BaseHTTPRequestHandler):
            def log_message(self,*args): pass
            def do_GET(self):
                body,status=fixture.answer(self.path)
                if status==200:fixture.bodies[self.path]=body
                fixture.events.append({'method':'GET','path':self.path,'status':status,'body_sha256':hashlib.sha256(body).hexdigest(),'bytes':len(body)})
                self.send_response(status);self.send_header('Content-Type','application/json');self.send_header('Content-Length',str(len(body)));self.send_header('Connection','close');self.end_headers()
                try:self.wfile.write(body)
                except BrokenPipeError:pass
        self.server=http.server.ThreadingHTTPServer(('127.0.0.1',0),Handler)
        self.server.daemon_threads=True;self.server.timeout=2
        self.base='http://127.0.0.1:'+str(self.server.server_port)
        self.thread=threading.Thread(target=self.server.serve_forever,daemon=True);self.thread.start()
    def answer(self,path):
        b=self.base; n=self.nonce; registration=b+'/v3/registrations/'
        page=b+'/v3/page-'+n; leaf=b+'/v3/item-'+n
        if self.scenario=='cross-origin': page='http://127.0.0.1:'+str(self.trap.server_port)+'/v3/forbidden-'+n
        if self.scenario=='escape': page=b+'/outside-'+n
        if self.scenario=='encoded-separator': page=b+'/v3/%2foutside-'+n
        if self.scenario=='cycle': page=registration+'microsoft.entityframeworkcore/index.json'
        if path=='/v3/index.json': obj={'version':'3.0.0','resources':([{'@id':registration,'@type':'irrelevant'}]*257 if self.scenario=='resource-limit' else [{'@id':registration,'@type':['RegistrationsBaseUrl/3.0.0-beta','RegistrationsBaseUrl/3.6.0']}])}
        elif path=='/v3/catalog0/index.json': obj={'commitId':'fixture-tip','commitTimeStamp':'2026-11-01T00:00:00Z','count':0,'items':[]}
        elif path=='/v3/registrations/microsoft.entityframeworkcore/index.json': obj={'count':(3 if self.scenario=='count-mismatch' else 2),'items':[{'count':1,'items':[{'catalogEntry':{'id':NAME,'version':'1.0.0','listed':True}}]},{'@id':page,'count':1,'lower':'10.0.0','upper':'10.0.13'}]}
        elif path=='/v3/page-'+n and self.scenario!='missing-page': obj={'count':1,'items':[{'catalogEntry':leaf}]}
        elif path=='/v3/item-'+n: obj={'id':('Different.Package' if self.scenario=='wrong-id' else NAME),'version':'10.0.13','listed':True,'description':DESCRIPTION,'licenseExpression':'MIT','published':'2026-11-01T00:00:00Z'}
        else:return b'{"error":"fixture not found"}',404
        return json.dumps(obj,separators=(',',':')).encode(),200
    def close(self):
        errors=[]
        for server_name,thread_name in (('server','thread'),('trap','trap_thread')):
            server=getattr(self,server_name,None);thread=getattr(self,thread_name,None)
            if server is None:continue
            try:
                if thread is not None and thread.is_alive():
                    server.shutdown();thread.join(timeout=5)
                    check(not thread.is_alive(),'fixture HTTP thread survived shutdown')
            except Exception as error:errors.append(repr(error))
            finally:
                try:server.server_close()
                except Exception as error:errors.append(repr(error))
        check(not errors,'fixture cleanup failed: '+repr(errors))


def selected_candidate(page, require_description=True):
    check(isinstance(page,dict),'package response is not an object')
    pd=page.get('package_discovery');check(isinstance(pd,dict),'missing typed package_discovery')
    check(pd.get('package')==PURL,'wrong package coordinate')
    obs=pd.get('observation');check(isinstance(obs,dict) and obs.get('state')=='observed','package observation not observed')
    candidates=[v for v in obs.get('candidates',[]) if isinstance(v,dict) and v.get('coordinate')==PURL]
    check(len(candidates)==1,'expected exactly one source candidate for exact pin')
    c=candidates[0]
    for key in ('source','proof'):
        value=c.get(key);check(isinstance(value,list) and len(value)==32 and all(type(i) is int and 0<=i<=255 for i in value),key+' must be 32 typed bytes')
    check(c.get('standing')=='published','incorrect source standing')
    check(c.get('metadata',{}).get('yanked')=={'state':'known','value':False},'exact NuGet listed state not preserved')
    if require_description:
        for key,value in (('description',DESCRIPTION),('license','MIT'),('published_at','2026-11-01T00:00:00Z')):
            check(c.get('metadata',{}).get(key)=={'state':'known','value':value},'exact source facet missing: '+key)
    check(isinstance(c.get('freshness'),dict) and c['freshness'].get('state') in ('current','historical','expired','unavailable'),'missing typed freshness')
    tags=[t for row in page.get('records',[]) if isinstance(row,dict) for t in row.get('tags',[]) if isinstance(t,str)]
    check(any('discovered' in t and 'unacquired' in t for t in tags),'discovery misrepresented as acquired')
    check('README witness' not in json.dumps(page),'readme fabricated from discovery')
    return c

def explore_candidates(page):
    check(isinstance(page,dict) and isinstance(page.get('records'),list),'explore lacks typed records')
    found=[]
    for row in page['records']:
        if not isinstance(row,dict):continue
        d=row.get('discovery')
        if isinstance(d,dict) and d.get('coordinate')==PURL:found.append(d)
        group=row.get('package_group')
        if isinstance(group,dict):
            check(group.get('release_match_scope') in ('release-matches','lineage-metadata-only'),'missing/invalid release match scope')
            for release in group.get('releases',[]):
                if isinstance(release,dict) and release.get('state')=='discovered' and isinstance(release.get('value'),dict) and release['value'].get('coordinate')==PURL:found.append(release['value'])
    return found

def mcp_decode(stdout, ids):
    wanted=set(ids);check(len(wanted)==len(ids),'duplicate request ids');found={}
    for line in stdout.splitlines():
        value=json.loads(line);check(isinstance(value,dict) and value.get('jsonrpc')=='2.0','invalid MCP envelope')
        if 'id' not in value:
            check(isinstance(value.get('method'),str),'invalid MCP notification');continue
        ident=value['id'];check(type(ident) is int and ident in wanted and ident not in found,'unexpected/duplicate MCP id')
        check('error' not in value and isinstance(value.get('result'),dict),'MCP RPC error')
        result=value['result'];check('isError' not in result or type(result['isError']) is bool,'invalid MCP isError')
        check(result.get('isError') is not True,'MCP tool refusal')
        if ident==1:check(isinstance(result.get('protocolVersion'),str) and isinstance(result.get('capabilities'),dict),'MCP initialization result malformed')
        else:check(isinstance(result.get('structuredContent'),dict),'MCP structured content absent')
        found[ident]=result
    check(set(found)==wanted,'missing MCP response');return found

def verify_receipt(path,commit,tree,lock,bins,before):
    def unique(pairs):
        result={}
        for key,value in pairs:
            check(key not in result,'duplicate build receipt key');result[key]=value
        return result
    raw=path.read_bytes();check(len(raw)<=4*1024*1024,'build receipt exceeds bound')
    record=json.loads(raw,object_pairs_hook=unique)
    check(isinstance(record,dict) and record.get('schema')=='nudox.runtime-artifact-build-receipt.v1','unsupported build receipt')
    source=record.get('source',{});check(source.get('commit')==commit and source.get('tree')==tree and source.get('cargo_lock_sha256')==lock and source.get('clean_before') is True and source.get('clean_after') is True,'build receipt source mismatch')
    check(type(record.get('exit')) is int and record['exit']==0 and record.get('check_only') is False,'build receipt did not complete a build')
    check(record.get('toolchain',{}).get('unchanged') is True,'build receipt toolchain changed')
    artifacts=record.get('artifacts');check(isinstance(artifacts,dict) and set(artifacts)==set(NAMES),'build receipt artifact set mismatch')
    for name in NAMES:
        item=artifacts[name];check(isinstance(item,dict) and item.get('sha256')==before[name] and item.get('bytes')==bins[name].stat().st_size,'build receipt binary hash/size mismatch '+name)
        check(pathlib.Path(item.get('path','')).resolve(strict=True)==bins[name],'build receipt binary path mismatch '+name)
    return hashlib.sha256(raw).hexdigest()

def retire_group(proc):
    """Retire the process group created by this driver and prove it exited."""
    try:os.killpg(proc.pid,signal.SIGTERM)
    except ProcessLookupError:pass
    deadline=time.monotonic()+10
    while time.monotonic()<deadline:
        proc.poll()
        try:os.killpg(proc.pid,0)
        except ProcessLookupError:break
        except PermissionError:pass
        time.sleep(.05)
    else:
        try:os.killpg(proc.pid,signal.SIGKILL)
        except ProcessLookupError:pass
    proc.wait(timeout=10)
    deadline=time.monotonic()+10
    while time.monotonic()<deadline:
        try:os.killpg(proc.pid,0)
        except ProcessLookupError:return
        except PermissionError:pass
        time.sleep(.05)
    raise Failure('owned process group survived cleanup: '+str(proc.pid))

class Journey:
    """Private owner/project process plumbing based on csharp_public_route.Journey."""
    def __init__(self,bins,out,fixture,timeout,root,case):
        self.bins=bins;self.out=out;self.fixture=fixture;self.timeout=timeout;self.seq=0;self.owner=None;self.owner_count=0;self.case=case
        self.root=root
        self.workspace=self.root/'workspace';self.workspace.mkdir(mode=0o700);self.project=self.root/'project';self.project.mkdir(mode=0o700)
        self.endpoint=self.root/'owner.sock';check(len(os.fsencode(self.endpoint))<100,'TMPDIR produces an overlong Unix socket path');self.secret=self.root/'authority.secret';self.secret.write_bytes(os.urandom(32));os.chmod(self.secret,0o600)
        self.env={k:os.environ[k] for k in ('PATH','LANG','LC_ALL','TZ','NUDOX_ACCOUNT_GATE') if k in os.environ}
        self.env['HOME']=str(self.root)
        for var in ('XDG_CONFIG_HOME','XDG_CACHE_HOME','XDG_DATA_HOME','XDG_STATE_HOME','XDG_RUNTIME_DIR','TMPDIR'):
            p=self.root/var;p.mkdir(mode=0o700);self.env[var]=str(p)
        self.env['BACKEND_LOCALD_AUTHORITY_SECRET_FILE']=str(self.secret)
    def start_owner(self,offline=False):
        if self.endpoint.exists():self.endpoint.unlink()
        self.owner_count+=1;log=(self.out/f'owner-{self.owner_count:03d}.log').open('wb')
        cmd=[str(self.bins['backend-locald']),'--endpoint',str(self.endpoint),'--workspace',str(self.workspace),'--authority-secret-file',str(self.secret),'--registry-endpoint',self.fixture.base+'/v3/catalog0/index.json','--registry-ecosystem','nuget','--advisory-offline','--forge-offline','--registry-discovery-source','nuget='+self.fixture.base+'/v3/catalog0/index.json']
        if offline:cmd.extend(['--registry-offline','--registry-discovery-offline'])
        self.owner=subprocess.Popen(cmd,cwd=self.project,env=self.env,stdin=subprocess.DEVNULL,stdout=log,stderr=subprocess.STDOUT,start_new_session=True);log.close()
        self.case['processes'].append({'kind':'owner','pid':self.owner.pid,'retired':False})
        end=time.monotonic()+self.timeout
        while time.monotonic()<end:
            check(self.owner.poll() is None,'owner exited before socket: '+str(self.owner.returncode))
            try:
                with socket.socket(socket.AF_UNIX) as s:s.connect(str(self.endpoint))
                return
            except OSError:time.sleep(.05)
        raise Failure('owner socket timeout')
    def stop_owner(self):
        if self.owner is None:return
        retire_group(self.owner)
        next(p for p in self.case['processes'] if p['pid']==self.owner.pid and not p['retired'])['retired']=True
        self.owner=None
    def call(self,argv,stdin=None):
        self.seq+=1;name=f'call-{self.seq:04d}';(self.out/(name+'.request.json')).write_text(json.dumps({'argv':[str(i) for i in argv],'stdin':stdin,'cwd':str(self.project)},indent=2)+'\n')
        with (self.out/(name+'.stdout')).open('wb') as stdout,(self.out/(name+'.stderr')).open('wb') as stderr:
            p=subprocess.Popen([str(i) for i in argv],cwd=self.project,env=self.env,stdin=subprocess.PIPE if stdin is not None else subprocess.DEVNULL,stdout=stdout,stderr=stderr,start_new_session=True)
            record={'kind':'client','pid':p.pid,'retired':False};self.case['processes'].append(record)
            try:
                if stdin is not None:
                    p.stdin.write(stdin.encode());p.stdin.close()
                deadline=time.monotonic()+self.timeout
                while p.poll() is None:
                    if time.monotonic()>=deadline or os.fstat(stdout.fileno()).st_size>MAX_OUTPUT or os.fstat(stderr.fileno()).st_size>MAX_OUTPUT:
                        raise Failure(name+' exceeded time/output bound')
                    time.sleep(.02)
            finally:
                retire_group(p);record['retired']=True
        check((self.out/(name+'.stdout')).stat().st_size<=MAX_OUTPUT and (self.out/(name+'.stderr')).stat().st_size<=MAX_OUTPUT,name+' exceeded output bound')
        return p.returncode,(self.out/(name+'.stdout')).read_text(errors='replace'),(self.out/(name+'.stderr')).read_text(errors='replace')
    def cli(self,*words):
        cmd=[self.bins['backend-cli'],'--format','json','--project',self.project,'--workspace',self.workspace,'--endpoint',self.endpoint,*words]
        code,out,err=self.call(cmd);check(code==0,'CLI refused or failed '+repr(words)+': '+err[:500]);return json.loads(out)
    def mcp(self,calls):
        ids=[1]+[i for i,_,_ in calls];req=[{'jsonrpc':'2.0','id':1,'method':'initialize','params':{'protocolVersion':'2025-06-18','capabilities':{},'clientInfo':{'name':'report35-nuget','version':'1'}}},{'jsonrpc':'2.0','method':'notifications/initialized','params':{}}]
        req.extend({'jsonrpc':'2.0','id':i,'method':'tools/call','params':{'name':name,'arguments':args}} for i,name,args in calls)
        code,out,err=self.call([self.bins['backend-mcp'],'--project',self.project,'--workspace',self.workspace,'--endpoint',self.endpoint],''.join(json.dumps(x,separators=(',',':'))+'\n' for x in req));check(code==0,'MCP process failed: '+err[:500]);return mcp_decode(out,ids)

def parity(j,label):
    cp=selected_candidate(j.cli('package',PURL)); j_last_cli=j.cli('explore',NAME);ce=explore_candidates(j_last_cli)
    m=j.mcp([(31,'backend.package',{'package':PURL,'detail':'full'}),(32,'backend.explore',{'query':NAME,'detail':'full'})])
    mp=selected_candidate(m[31]['structuredContent']);j_last_mcp=m[32]['structuredContent'];me=explore_candidates(j_last_mcp)
    for name,c in [('MCP package',mp)]:check(c['source']==cp['source'] and c['proof']==cp['proof'] and c['metadata']==cp['metadata'],label+' '+name+' source/proof mismatch')
    for name,rows in [('CLI explore',ce),('MCP explore',me)]:check(any(r.get('source')==cp['source'] and r.get('proof')==cp['proof'] and r.get('metadata')==cp['metadata'] for r in rows),label+' '+name+' missing exact source/proof')
    def scopes(page):return [r['package_group']['release_match_scope'] for r in page['records'] if isinstance(r,dict) and isinstance(r.get('package_group'),dict)]
    return {'stage':label,'coordinate':PURL,'source':cp['source'],'proof':cp['proof'],'metadata':cp['metadata'],'freshness':cp['freshness'],'cli_mcp_package_explore_parity':True,'cli_explore_match_scopes':scopes(j_last_cli),'mcp_explore_match_scopes':scopes(j_last_mcp)}

def poll(j,seconds):
    end=time.monotonic()+seconds;last=None;original_timeout=j.timeout
    try:
        while time.monotonic()<end:
            j.timeout=min(original_timeout,max(.01,end-time.monotonic()))
            try:return selected_candidate(j.cli('package',PURL),require_description=False)
            except (Failure,ValueError,KeyError) as e:last=str(e);time.sleep(min(.3,max(0,end-time.monotonic())))
    finally:j.timeout=original_timeout
    raise Failure('bounded discovery polling expired: '+str(last))

def negative(j,fixture):
    # An invalid delegation is unavailable/fault, never a source-proven negative.
    try:
        page=j.cli('package',PURL); obs=page.get('package_discovery',{}).get('observation',{})
        check(obs.get('state')=='unavailable','invalid delegation must be typed unavailable, not '+repr(obs.get('state')))
    except Failure as e:
        # Do not let an arbitrary CLI error turn this control green.
        raise Failure('negative case did not return typed unavailable: '+str(e)) from e
    m=j.mcp([(31,'backend.package',{'package':PURL,'detail':'full'})]);mp=m[31]['structuredContent'];mobs=mp.get('package_discovery',{}).get('observation',{})
    check(mobs.get('state')=='unavailable','MCP invalid delegation must be typed unavailable, not '+repr(mobs.get('state')))
    paths=[event['path'] for event in fixture.events]
    required=['/v3/index.json']
    if fixture.scenario!='resource-limit':required.append('/v3/registrations/microsoft.entityframeworkcore/index.json')
    if fixture.scenario in ('missing-page','wrong-id'):required.append('/v3/page-'+fixture.nonce)
    if fixture.scenario=='wrong-id':required.append('/v3/item-'+fixture.nonce)
    check(all(path in paths for path in required),'negative did not reach its intended fixture boundary: '+repr(paths))
    check(not fixture.trap_events,'cross-origin trap was reached')
    check(not any(x['path'].startswith('/outside-') or '%2f' in x['path'].lower() for x in fixture.events),'forbidden link was requested')
    return {'scenario':fixture.scenario,'result':'CLI/MCP typed-unavailable','requests':len(fixture.events),'cross_origin_requests':len(fixture.trap_events)}

def self_check():
    # No binaries or account services are started by this mode.
    for scenario in ('positive','cross-origin','escape','encoded-separator','cycle','missing-page','wrong-id','count-mismatch','resource-limit'):
        f=Fixture.create(scenario)
        try:
            h=http.client.HTTPConnection('127.0.0.1',f.server.server_port,timeout=3);h.request('GET','/v3/index.json');r=h.getresponse();service=json.loads(r.read());h.close();check(r.status==200 and service['resources'][0]['@id'].startswith(f.base+'/v3/'),'service fixture')
            reg=json.loads(f.answer('/v3/registrations/microsoft.entityframeworkcore/index.json')[0]);link=reg['items'][1]['@id'];check(f.nonce in link or scenario=='cycle','unpredictable link absent')
            parsed=urllib.parse.urlsplit(link);base=urllib.parse.urlsplit(f.base)
            if scenario=='cross-origin':check(parsed.hostname=='127.0.0.1' and parsed.port==f.trap.server_port and parsed.port!=base.port,'cross-origin trap setup')
            if scenario=='escape':check(not parsed.path.startswith('/v3/'),'escape fixture')
            if scenario=='encoded-separator':check('%2f' in parsed.path.lower(),'encoded separator fixture')
            if scenario=='cycle':check(parsed.path=='/v3/registrations/microsoft.entityframeworkcore/index.json','cycle fixture')
            if scenario=='missing-page':check(f.answer('/v3/page-'+f.nonce)[1]==404,'missing page fixture')
            if scenario=='wrong-id':check(json.loads(f.answer('/v3/item-'+f.nonce)[0])['id']!=NAME,'wrong identity fixture')
            if scenario=='count-mismatch':check(reg['count']!=len(reg['items']),'count boundary fixture')
            if scenario=='resource-limit':check(len(service['resources'])==257,'resource boundary fixture')
            check(f.answer('/v3/package.nupkg')[1]==404,'archive must not be served')
            if scenario=='positive':
                paths=['/v3/index.json','/v3/registrations/microsoft.entityframeworkcore/index.json','/v3/page-'+f.nonce,'/v3/item-'+f.nonce];proof=bytearray(b'nudox.nuget-exact-service-registration.v1\0')
                for path in paths:
                    url=(f.base+path).encode();body,status=f.answer(path);check(status==200,'proof fixture path missing');proof.extend(len(url).to_bytes(8,'big'));proof.extend(url);proof.extend(len(body).to_bytes(8,'big'));proof.extend(body)
                check(len(proof)<=1024,'proof fixture exceeds one-chunk bound');check(len(blake3_short(bytes(proof)))==32,'proof digest size')
        finally:f.close()
    mcp_decode(json.dumps({'jsonrpc':'2.0','id':1,'result':{'protocolVersion':'2025-06-18','capabilities':{}}})+'\n'+json.dumps({'jsonrpc':'2.0','id':31,'result':{'isError':False,'structuredContent':{}}}),[1,31])
    repeated=json.dumps({'jsonrpc':'2.0','id':1,'result':{'protocolVersion':'2025-06-18','capabilities':{}}})+'\n'+json.dumps({'jsonrpc':'2.0','id':31,'result':{'structuredContent':{}}})+'\n'+json.dumps({'jsonrpc':'2.0','id':31,'result':{'structuredContent':{}}})
    try:mcp_decode(repeated,[1,31])
    except Failure:pass
    else:raise Failure('duplicate MCP response accepted')
    for invalid in ('true','false',1,None,[],{}):
        try:mcp_decode(json.dumps({'jsonrpc':'2.0','id':1,'result':{'protocolVersion':'2025-06-18','capabilities':{}}})+'\n'+json.dumps({'jsonrpc':'2.0','id':31,'result':{'isError':invalid,'structuredContent':{}}}),[1,31])
        except Failure:continue
        raise Failure('malformed isError accepted')
    # Published empty/abc vectors plus a genuine prior backend-generated 80-byte
    # package index and its sealed manifest BLAKE3 check multi-block hashing.
    check(blake3_short(b'').hex()=='af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262','BLAKE3 empty vector')
    check(blake3_short(b'abc').hex()=='6437b3ac38465133ffb63b75273a8db548c558465d79db03fd359c6cd5bd9d85','BLAKE3 abc vector')
    legacy=bytes.fromhex('d9f440943d5aa3ed9269bfe6715329730f37a7c9dbc11fe417eb8a57c377bdd6df83ad493da92a564542d61a9eed1d0c6035f1523af61af193d6cd6cb99b6423000000000000000100000000000002dc')
    check(blake3_short(legacy).hex()=='f9d90452269fcb4381ff122526c49cfb4e6937c4f680e9b31597fd141dab1008','BLAKE3 multi-block sealed-backend vector')
    try:blake3_short(bytes(4097))
    except Failure:pass
    else:raise Failure('long proof input should fail closed')
    # Validate typed product projection checks without running a product.
    candidate={'coordinate':PURL,'standing':'published','source':[1]*32,'proof':[2]*32,'metadata':{'yanked':{'state':'known','value':False},'description':{'state':'known','value':DESCRIPTION},'license':{'state':'known','value':'MIT'},'published_at':{'state':'known','value':'2026-11-01T00:00:00Z'}},'freshness':{'state':'current','observed_at_millis':10,'valid_until_millis':20}}
    good={'package_discovery':{'package':PURL,'observation':{'state':'observed','candidates':[candidate]}},'records':[{'tags':['discovered · unacquired']}],'note':DESCRIPTION}
    selected_candidate(good)
    for case in ('description','license','published_at','proof','yanked','acquired'):
        malformed=json.loads(json.dumps(good))
        if case in ('description','license','published_at'):malformed['package_discovery']['observation']['candidates'][0]['metadata'].pop(case)
        if case=='proof':malformed['package_discovery']['observation']['candidates'][0]['proof']=[2]
        if case=='yanked':malformed['package_discovery']['observation']['candidates'][0]['metadata']['yanked']={'state':'unknown'}
        if case=='acquired':malformed['records'][0]['tags']=['acquired']
        try:selected_candidate(malformed)
        except Failure:continue
        raise Failure('typed negative accepted: '+case)
    # Synthetically test exact receipt source, artifact hashes, and duplicate keys.
    with tempfile.TemporaryDirectory(prefix='nuget-receipt-selfcheck-') as d:
        root=pathlib.Path(d);bins={};hashes={}
        for name in NAMES:
            path=root/name;path.write_bytes(name.encode());bins[name]=path.resolve();hashes[name]=digest(path)
        body={'schema':'nudox.runtime-artifact-build-receipt.v1','source':{'commit':'c','tree':'t','cargo_lock_sha256':'l','clean_before':True,'clean_after':True},'exit':0,'check_only':False,'toolchain':{'unchanged':True},'artifacts':{name:{'path':str(bins[name]),'bytes':bins[name].stat().st_size,'sha256':hashes[name]} for name in NAMES}}
        receipt=root/'receipt.json';receipt.write_text(json.dumps(body));verify_receipt(receipt,'c','t','l',bins,hashes)
        body['artifacts']['backend-cli']['sha256']='wrong';receipt.write_text(json.dumps(body))
        try:verify_receipt(receipt,'c','t','l',bins,hashes)
        except Failure:pass
        else:raise Failure('mismatched receipt accepted')
        receipt.write_text('{"schema":1,"schema":1}')
        try:verify_receipt(receipt,'c','t','l',bins,hashes)
        except Failure:pass
        else:raise Failure('duplicate receipt key accepted')
    print('SELF-CHECK PASS: 9 fixture modes; strict MCP, typed evidence, BLAKE3 vectors and receipt negatives; zero product processes')

def main():
    ap=argparse.ArgumentParser();ap.add_argument('--self-check',action='store_true');ap.add_argument('--blake3-helper',required=True,type=pathlib.Path);ap.add_argument('--source',type=pathlib.Path);ap.add_argument('--expected-source');ap.add_argument('--expected-tree');ap.add_argument('--expected-lock-sha256');ap.add_argument('--bin-dir',type=pathlib.Path);ap.add_argument('--build-receipt',type=pathlib.Path);ap.add_argument('--output-parent',type=pathlib.Path);ap.add_argument('--timeout',type=float,default=120);args=ap.parse_args()
    global BLAKE3_HELPER
    BLAKE3_HELPER=args.blake3_helper.resolve(strict=True)
    check(BLAKE3_HELPER.is_file() and os.access(BLAKE3_HELPER,os.X_OK),'native helper missing')
    helper_hash=digest(BLAKE3_HELPER)
    if args.self_check:self_check();return
    check(all((args.source,args.expected_source,args.expected_tree,args.expected_lock_sha256,args.bin_dir,args.build_receipt,args.output_parent)),'source/tree/lock/binaries/receipt/output required')
    check(1<=args.timeout<=300,'timeout must be 1..300 seconds')
    network={'nuget_registry':'explicit private loopback ecosystem override','discovery':'explicit private loopback source only','advisory':'offline','forge':'offline'}
    source=args.source.resolve(strict=True)
    def git(*argv):return subprocess.check_output(['git','-C',str(source),*argv],text=True).strip()
    commit=git('rev-parse','HEAD');tree=git('rev-parse','HEAD^{tree}');check(commit==args.expected_source and tree==args.expected_tree,'source HEAD/tree mismatch');check(not git('status','--porcelain'),'source tree must be clean')
    lock=digest(source/'Cargo.lock');check(lock==args.expected_lock_sha256,'Cargo.lock hash mismatch')
    bins={name:(args.bin_dir/name).resolve(strict=True) for name in NAMES}
    for name,p in bins.items():check(p.is_file() and os.access(p,os.X_OK),name+' missing executable')
    before={name:digest(p) for name,p in bins.items()};build_receipt=args.build_receipt.resolve(strict=True)
    sys.path.insert(0,str(source/'tests/journeys/scripts'))
    from runtime_build_receipt import verify_runtime_build_receipt
    verified=verify_runtime_build_receipt(build_receipt,source);build_receipt_hash=verified['receipt_sha256']
    check(all(verified['artifacts'][name]['path']==str(bins[name]) and verified['artifacts'][name]['sha256']==before[name] for name in NAMES),'verified artifact mismatch')
    output_parent=args.output_parent.resolve(strict=True)
    check(output_parent.is_dir() and output_parent!=source and source not in output_parent.parents,'output parent must be outside source checkout')
    out=pathlib.Path(tempfile.mkdtemp(prefix='report35-nuget-',dir=output_parent));os.chmod(out,0o700)
    receipt={'schema':'nudox.report35.nuget-public.v1','contract_source':CONTRACT_COMMIT,'contract_tree':CONTRACT_TREE,'source_commit':commit,'source_tree':tree,'cargo_lock_sha256':lock,'binaries_before':before,'blake3_helper':{'path':str(BLAKE3_HELPER),'sha256':helper_hash},'build_receipt_path':str(build_receipt),'build_receipt_sha256':build_receipt_hash,'receipt_verification':'full runtime build receipt verified against exact source and artifacts','network':network,'account_gate_environment_set':'NUDOX_ACCOUNT_GATE' in os.environ,'cases':[],'result':'failed','limits':['does not execute/acquire packages','no real account/fake credential','private loopback provider; external network namespace not asserted']}
    try:
        for scenario in ('positive','cross-origin','escape','encoded-separator','cycle','missing-page','wrong-id','count-mismatch','resource-limit'):
            caseout=out/scenario;caseout.mkdir(mode=0o700);fixture=None;j=None
            case={'scenario':scenario,'fixture_root':None,'processes':[],'status':'running'};receipt['cases'].append(case)
            try:
                fixture=Fixture.create(scenario)
                root=pathlib.Path(tempfile.mkdtemp(prefix='ndx-nuget-',dir=os.environ.get('TMPDIR','/tmp')));case['fixture_root']=str(root);os.chmod(root,0o700)
                j=Journey(bins,caseout,fixture,args.timeout,root,case);j.start_owner()
                if scenario=='positive':
                    candidate=poll(j,args.timeout);expected=['/v3/index.json','/v3/registrations/microsoft.entityframeworkcore/index.json','/v3/page-'+fixture.nonce,'/v3/item-'+fixture.nonce]
                    paths=[x['path'] for x in fixture.events];check(all(p in paths for p in expected),'required advertised link was not fetched: '+repr(paths))
                    first=parity(j,'online');check(first['source']==candidate['source'] and first['proof']==candidate['proof'],'poll vs parity proof mismatch')
                    proof_input=bytearray(b'nudox.nuget-exact-service-registration.v1\0')
                    for path in expected:
                        url=(fixture.base+path).encode();body=fixture.bodies[path];proof_input.extend(len(url).to_bytes(8,'big'));proof_input.extend(url);proof_input.extend(len(body).to_bytes(8,'big'));proof_input.extend(body)
                    expected_proof=blake3_short(bytes(proof_input));endpoint=(fixture.base+'/v3/catalog0/index.json').encode();expected_source=blake3_short(b'backend.registry.source.v1\0'+bytes([5])+endpoint)
                    check(first['proof']==list(expected_proof),'source proof does not match actual fetched service/page/catalog bytes');check(first['source']==list(expected_source),'source identity does not match exact configured endpoint')
                    first['source_proof_input_sha256']=hashlib.sha256(proof_input).hexdigest();first['source_proof_input_bytes']=len(proof_input)
                    before_requests=len(fixture.events);j.stop_owner();j.start_owner(offline=True);second=parity(j,'offline-reopen');check(first['source']==second['source'] and first['proof']==second['proof'] and first['metadata']==second['metadata'],'reopened source/proof/metadata changed');check(len(fixture.events)==before_requests,'offline reopen accessed fixture')
                    case.update({'checks':[first,second],'required_paths':expected})
                else:case.update(negative(j,fixture))
                check(not any(any(token in event['path'].lower() for token in ('.nupkg','archive','download','packagecontent')) for event in fixture.events),'metadata-only journey attempted archive/download')
            finally:
                try:
                    if j is not None:j.stop_owner()
                    check(all(p['retired'] for p in case['processes']),'owned process group not retired')
                    case['status']='retired'
                finally:
                    if fixture is not None:
                        fixture.close()
                        (caseout/'request-receipts.json').write_text(json.dumps({'source_requests':fixture.events,'cross_origin_trap_requests':fixture.trap_events},indent=2)+'\n')
        receipt['result']='pass'
    except Exception as exc:
        receipt['error']=repr(exc);raise
    finally:
        after={};errors={}
        for name,p in bins.items():
            try:after[name]=digest(p)
            except OSError as e:errors[name]=repr(e)
            if after.get(name)!=before[name]:errors.setdefault(name,'binary changed')
        receipt['binaries_after']=after;receipt['binary_integrity_errors']=errors
        try:
            check(git('rev-parse','HEAD')==commit and git('rev-parse','HEAD^{tree}')==tree and not git('status','--porcelain') and digest(source/'Cargo.lock')==lock,'source changed during run')
            check(verify_runtime_build_receipt(build_receipt,source)['receipt_sha256']==build_receipt_hash,'build receipt or artifacts changed during run')
            check(digest(BLAKE3_HELPER)==helper_hash,'native BLAKE3 helper changed during run')
        except (Failure,OSError,subprocess.SubprocessError) as exc:errors['source_or_receipt']=repr(exc)
        if errors:receipt['result']='failed'
        (out/'receipt.json').write_text(json.dumps(receipt,indent=2)+'\n');print('receipt='+str(out/'receipt.json'),flush=True)
        if errors:raise Failure('source, receipt, or executable changed; failure receipt retained')

if __name__=='__main__':
    try:main()
    except Exception as e:print('FAIL: '+repr(e),file=sys.stderr);sys.exit(1)
