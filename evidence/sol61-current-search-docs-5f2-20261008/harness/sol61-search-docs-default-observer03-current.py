import subprocess,os,json,time,hashlib,fcntl,sys,select,shutil
from pathlib import Path
from datetime import datetime,timezone
os.umask(0o077)
config_path,phase,admission=sys.argv[1:]
config=json.loads(Path(config_path).read_text());root=Path(config['root']);destination=config['destination']
origin=json.loads((root/'evidence/source-origin.json').read_text())
assert subprocess.check_output(['git','-C',str(root/'project'),'rev-parse','HEAD'],text=True).strip()==origin['commit']
assert not subprocess.check_output(['git','-C',str(root/'project'),'status','--porcelain'],text=True).strip()
coord=Path(config['coordination'])
epoch=root/'evidence'/phase;epoch.mkdir(exist_ok=False)
project=root/'project';home=root/'home';workspace=Path(config['workspace_default'])
manifest=Path(config['manifest'])
assert hashlib.sha256(manifest.read_bytes()).hexdigest()==config['manifest_sha256']
frozen=json.loads(manifest.read_text());assert frozen['source_commit']==config['compiled_source_commit'];assert frozen['source_tree']==config['compiled_source_tree']
source_root=Path(config['compiled_source_root'])
assert subprocess.check_output(['git','-C',str(source_root),'rev-parse','HEAD'],text=True).strip()==frozen['source_commit']
assert subprocess.check_output(['git','-C',str(source_root),'rev-parse','HEAD^{tree}'],text=True).strip()==frozen['source_tree']
assert hashlib.sha256((source_root/'Cargo.lock').read_bytes()).hexdigest()==config['cargo_lock_sha256']
images=Path(config['images'])
for name,fact in frozen['artifacts'].items():assert hashlib.sha256((images/name).read_bytes()).hexdigest()==fact['sha256']
lease_path=Path(config['permit'])
lease=open(lease_path,'a+');fcntl.flock(lease,fcntl.LOCK_EX|fcntl.LOCK_NB)
env={k:v for k,v in os.environ.items() if not k.startswith(('NUDOX_','BACKEND_'))}
env.pop('XDG_RUNTIME_DIR',None)
env.update(HOME=str(home))
client_env=dict(env)
for directory in [home,workspace]:directory.mkdir(parents=True,exist_ok=True)
endpoint=Path(config['endpoint_default']);assert not endpoint.exists()
cli=[str(images/'backend-cli')]
commands=[]
def run(label,args,json_mode=False,timeout=600):
 start=time.monotonic();argv=cli+(['--json'] if json_mode else [])+args
 p=subprocess.Popen(argv,cwd=project,env=client_env,stdout=subprocess.PIPE,stderr=subprocess.PIPE)
 try:out,err=p.communicate(timeout=timeout)
 except subprocess.TimeoutExpired:
  p.terminate();out,err=p.communicate();(epoch/'needs-attention').write_text(label);raise
 (epoch/(label+'.stdout')).write_bytes(out);(epoch/(label+'.stderr')).write_bytes(err)
 row={'label':label,'argv':argv,'pid':p.pid,'exit':p.returncode,'elapsed_seconds':time.monotonic()-start,'stdout_bytes':len(out),'stderr_bytes':len(err)}
 if json_mode:
  try:row['json']=json.loads(out)
  except ValueError:pass
 commands.append(row);(epoch/'commands.json').write_text(json.dumps(commands,indent=2));return row
a=json.loads(Path(admission).read_text());assert a['advisory_allowed'] is True and a['schema']=='compiler-capacity-admission.v5'
age=max((datetime.now(timezone.utc)-datetime.fromisoformat(h['census']['sample_started_at_utc'].replace('Z','+00:00'))).total_seconds() for h in a['raw_samples'].values());assert age<=30,age
assert a['current_fleet_compiler_group_count']+1<=16 and a['hosts'][destination]['compiler_group_count']+1<=8
assert destination=='h16001mac' and a['memory_guard_scope']=='destination' and a['destination_memory_only_for_remote'] is True
assert a['minimum_available_memory_bytes_each_host'] is None and a['minimum_available_memory_bytes_destination']==8*1024**3
assert set(a['hosts'])=={'local','ilo','h16001mac'}
assert all(h['complete'] and type(h['available_memory_bytes']) is int and h['available_memory_bytes']>=0 and h['memory_floor_applies'] is (name==destination) for name,h in a['hosts'].items())
assert a['hosts'][destination]['available_memory_bytes']>=8*1024**3 and a['hosts'][destination]['root_disk_available_bytes']>=16*1024**3
(epoch/'fleet.json').write_text(json.dumps(a,indent=2))
argv=[str(images/'backend-locald'),'--registry-offline','--registry-discovery-offline','--advisory-offline','--forge-offline','--idle-timeout-ms','15000']
owner_out=open(epoch/'locald.stdout','wb');owner_err=open(epoch/'locald.stderr','wb');start=time.monotonic()
owner=subprocess.Popen(argv,cwd=project,env=env,stdout=owner_out,stderr=owner_err,start_new_session=True)
receipt={'observer_sha256':hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),'config_sha256':hashlib.sha256(Path(config_path).read_bytes()).hexdigest(),'memory_guard_scope':a['memory_guard_scope'],'destination_memory_only_for_remote':a['destination_memory_only_for_remote'],'observer_pid':os.getpid(),'owner_pid':owner.pid,'permit_path':str(lease_path),'allocation':'Root authorized ONE independent remote Python runtime group for this worker','argv':argv,'started':datetime.now(timezone.utc).isoformat(),'admission_age_seconds':age,'manifest_sha256':hashlib.sha256(manifest.read_bytes()).hexdigest(),'source_commit':frozen['source_commit'],'source_tree':frozen['source_tree'],'artifact_sha256':{k:v['sha256'] for k,v in frozen['artifacts'].items()},'configured_owner_idle_timeout_ms':15000,'ordinary_default_owner_idle_timeout_ms':600000,'retained_authentic_state_replay':True,'fresh_empty_home_acceptance':False,'platform_default_workspace_and_endpoint':True,'no_workspace_endpoint_or_data_env_override':True,'default_checker_environment':True,'checker_overrides':{},'private_home':str(home),'project':str(project),'workspace':str(workspace),'endpoint':str(endpoint),'path':env.get('PATH')}
(epoch/'owner.launch.json').write_text(json.dumps(receipt,indent=2))
mcp=None;wire=None
try:
 deadline=time.monotonic()+120
 while not endpoint.exists():
  assert owner.poll() is None,'owner exited';assert time.monotonic()<deadline;time.sleep(.05)
 receipt['endpoint_startup_seconds']=time.monotonic()-start
 receipt['observed_process']=subprocess.check_output(['ps','-p',str(owner.pid),'-o','pid,ppid,lstart,command'],text=True)
 (epoch/'owner.launch.json').write_text(json.dumps(receipt,indent=2))
 # Keep a real MCP session open throughout the ordinary CLI journey and idle later.
 mcp=subprocess.Popen([str(images/'backend-mcp')],cwd=project,env=client_env,stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=open(epoch/'mcp.stderr','wb'))
 wire=open(epoch/'mcp.wire.jsonl','wb');seq=0;tool_calls=[]
 def rpc(method,params):
  global seq
  seq+=1;request={'jsonrpc':'2.0','id':seq,'method':method,'params':params};encoded=(json.dumps(request)+'\n').encode();wire.write(b'SEND '+encoded);wire.flush();mcp.stdin.write(encoded);mcp.stdin.flush();beg=time.monotonic()
  while time.monotonic()-beg<120:
   if not select.select([mcp.stdout],[],[],1)[0]:continue
   line=mcp.stdout.readline();wire.write(b'RECV '+line);wire.flush();assert line,'MCP stream closed';response=json.loads(line)
   if response.get('id')==seq:
    response['_acceptance_wire_bytes']=len(line);return response
  raise RuntimeError('MCP timeout '+method)
 def tool(label,name,args):
  beg=time.monotonic();result=rpc('tools/call',{'name':name,'arguments':args});encoded=json.dumps(result,indent=2)
  (epoch/(label+'.json')).write_text(encoded);tool_calls.append({'label':label,'name':name,'arguments':args,'elapsed_seconds':time.monotonic()-beg,'response_bytes':len(encoded.encode()),'actual_wire_bytes':result.get('_acceptance_wire_bytes'),'approx_wire_tokens':(result.get('_acceptance_wire_bytes',0)+3)//4,'rpc_error':result.get('error'),'isError':result.get('result',{}).get('isError')});(epoch/'mcp-calls.json').write_text(json.dumps(tool_calls,indent=2));return result
 rpc('initialize',{'protocolVersion':'2024-11-05','capabilities':{},'clientInfo':{'name':'real-large-app-acceptance','version':'1'}})
 encoded=(json.dumps({'jsonrpc':'2.0','method':'notifications/initialized','params':{}})+'\n').encode();wire.write(b'SEND '+encoded);wire.flush();mcp.stdin.write(encoded);mcp.stdin.flush()
 catalog=rpc('tools/list',{});(epoch/'mcp.tools.json').write_text(json.dumps(catalog,indent=2))
 run('versions-before-any-reindex',['semantic-versions',str(project)],True)
 probe=config['probe'];query=probe['name']
 search_argv=cli+['--json','--detail','full','--limit','50','search',query]
 search_started=time.monotonic();search_utc=datetime.now(timezone.utc).isoformat()
 search_process=subprocess.Popen(search_argv,cwd=project,env=client_env,stdout=open(epoch/'cold-search.stdout','wb'),stderr=open(epoch/'cold-search.stderr','wb'))
 (epoch/'cold-search.launch.json').write_text(json.dumps({'argv':search_argv,'pid':search_process.pid,'started':search_utc}))
 controls=[];sample_children=[];duplicate_search=None
 for at in range(180):
  if at==1 and search_process.poll() is None:
   duplicate_argv=cli+['--json','--limit','1','search',query]
   duplicate_search=subprocess.Popen(duplicate_argv,cwd=project,env=client_env,stdout=open(epoch/'shared-abandoned-search.stdout','wb'),stderr=open(epoch/'shared-abandoned-search.stderr','wb'))
   (epoch/'shared-abandoned-search.launch.json').write_text(json.dumps({'pid':duplicate_search.pid,'argv':duplicate_argv,'started':datetime.now(timezone.utc).isoformat(),'keeper_pid':search_process.pid}))
  if at==5 and duplicate_search is not None:
   if duplicate_search.poll() is None:duplicate_search.terminate()
   duplicate_search.wait(timeout=15)
   (epoch/'shared-abandoned-search.retired.json').write_text(json.dumps({'pid':duplicate_search.pid,'exit':duplicate_search.returncode,'kernel_wait':True,'keeper_still_pending':search_process.poll() is None,'transport_abandoned':duplicate_search.returncode<0}))
  if at in [0,5,20] and search_process.poll() is None:
   sample_file=epoch/('cold-owner-'+str(at)+'.sample.txt')
   sample_children.append(subprocess.Popen(['/usr/bin/sample',str(owner.pid),'1','1','-file',str(sample_file)],stdout=open(epoch/('sample-'+str(at)+'.stdout'),'wb'),stderr=open(epoch/('sample-'+str(at)+'.stderr'),'wb')))
  row=run('concurrent-health-'+str(at),['--passive','health'],True,timeout=10);row['since_search_seconds']=time.monotonic()-search_started;row['search_pending_after']=search_process.poll() is None;controls.append(row)
  if at in [0,3,10]:
   status=run('concurrent-status-'+str(at),['--passive','status'],True,timeout=10);status['since_search_seconds']=time.monotonic()-search_started;status['search_pending_after']=search_process.poll() is None;controls.append(status)
   version=run('concurrent-revision-'+str(at),['semantic-versions',str(project)],True,timeout=10);version['since_search_seconds']=time.monotonic()-search_started;version['search_pending_after']=search_process.poll() is None;controls.append(version)
  (epoch/'control-responsiveness.json').write_text(json.dumps(controls,indent=2))
  if search_process.poll() is not None:break
  time.sleep(.25 if at<10 else 1)
 search_exit=search_process.wait(timeout=600)
 if duplicate_search is not None and duplicate_search.poll() is None:
  duplicate_search.wait(timeout=90)
  (epoch/'shared-abandoned-search.retired.json').write_text(json.dumps({'pid':duplicate_search.pid,'exit':duplicate_search.returncode,'kernel_wait':True,'completed_before_cancellation':True}))
 for child in sample_children:child.wait(timeout=15)
 search=json.loads((epoch/'cold-search.stdout').read_bytes()) if search_exit==0 else {}
 (epoch/'cold-search.retired.json').write_text(json.dumps({'pid':search_process.pid,'exit':search_exit,'elapsed_seconds':time.monotonic()-search_started,'argv':search_argv,'kernel_wait':True,'health_successes_while_search_pending':sum(r['exit']==0 and r['search_pending_after'] for r in controls),'max_control_seconds':max(r['elapsed_seconds'] for r in controls)}))
 assert search_exit==0, 'actual cold search failed'
 rows=[r for r in search.get('records',[]) if r.get('identity',{}).get('name')==query and r.get('identity',{}).get('path')==probe['path'] and r.get('identity',{}).get('line')==probe['line']]
 assert len(rows)==1, 'sealed Docs source occurrence not uniquely admitted'
 coordinate=rows[0]['identity']['coordinate']
 (epoch/'chosen-coordinate.json').write_text(json.dumps({'probe':probe,'exact_source_match_count':len(rows),'coordinate':coordinate,'authoritative_selected_occurrence':True},indent=2))
 for name,args in [('backend.status',{}),('backend.search',{'query':query}),('backend.resolve',{'query':query}),('backend.document',{'coordinate':coordinate}),('backend.source',{'coordinate':coordinate}),('backend.read',{'coordinate':[coordinate]}),('backend.references',{'coordinate':coordinate}),('backend.graph',{'coordinate':coordinate}),('backend.semantic_versions',{'package':str(project)})]:tool(name,name,args)
 for command in ['resolve','source','graph']:run('ordinary-'+command,[command,coordinate],True)
 run('final-health',['--passive','health'],True)
 mcp.stdin.close();mcp.wait(timeout=30);(epoch/'mcp.retired.json').write_text(json.dumps({'pid':mcp.pid,'kernel_wait':mcp.returncode}));mcp=None;wire.close();wire=None
 code=owner.wait(timeout=90);assert code==0,'owner did not idle exit';receipt['retirement']='actual idle exit without operator signal'
except Exception as error:
 (epoch/'observer.failure.json').write_text(json.dumps({'error':repr(error),'owner_live':owner.poll() is None}));raise
finally:
 if 'duplicate_search' in globals() and duplicate_search is not None and duplicate_search.poll() is None:duplicate_search.terminate();duplicate_search.wait(timeout=15)
 if 'search_process' in globals() and search_process.poll() is None:search_process.terminate();search_process.wait(timeout=15)
 if mcp is not None:mcp.terminate();mcp.wait(timeout=15)
 if wire is not None:wire.close()
 if owner.poll() is None:owner.terminate();owner.wait(timeout=90);receipt['retirement']='owned failure cleanup, not idle proof'
 receipt.update(kernel_wait=owner.returncode,ended=datetime.now(timezone.utc).isoformat());(epoch/'owner.retired.json').write_text(json.dumps(receipt,indent=2));owner_out.close();owner_err.close()
 origin=json.loads((root/'evidence/source-origin.json').read_text());unchanged=all(hashlib.sha256((project/f).read_bytes()).hexdigest()==s for f,s in origin['file_sha256'].items());(epoch/'source-unchanged.json').write_text(json.dumps({'all_original_tracked_bytes_unchanged':unchanged,'git_status':subprocess.check_output(['git','-C',str(project),'status','--porcelain'],text=True)}))
print(json.dumps({'phase':phase,'owner_pid':owner.pid,'wait':owner.returncode,'epoch':str(epoch),'permit_path':str(lease_path)}))
