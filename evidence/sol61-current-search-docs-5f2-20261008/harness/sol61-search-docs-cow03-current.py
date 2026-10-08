import pathlib,json,datetime,sys,fcntl,subprocess,hashlib,os
os.umask(0o077)
a=json.loads(pathlib.Path(sys.argv[1]).read_text());now=datetime.datetime.now(datetime.timezone.utc)
assert a['advisory_allowed'] and a['current_fleet_compiler_group_count']<16 and a['hosts']['h16001mac']['compiler_group_count']<8
assert a['destination']=='h16001mac' and a['memory_guard_scope']=='destination' and a['destination_memory_only_for_remote'] is True
assert a['minimum_available_memory_bytes_each_host'] is None and a['minimum_available_memory_bytes_destination']==8*1024**3
assert set(a['hosts'])=={'local','ilo','h16001mac'}
assert all(h['complete'] and type(h['available_memory_bytes']) is int and h['available_memory_bytes']>=0 and h['memory_floor_applies'] is (name=='h16001mac') and 0<=(now-datetime.datetime.fromisoformat(h['resource_sampled_at_utc'])).total_seconds()<=30 for name,h in a['hosts'].items())
assert a['hosts']['h16001mac']['available_memory_bytes']>=8*1024**3
assert a['hosts']['h16001mac']['root_disk_available_bytes']>=16*1024**3
config=json.loads(pathlib.Path('/private/tmp/sol61-search-docs-default-config03-current.json').read_text());root=pathlib.Path(config['root']);assert not root.exists()
lock=open(config['permit'],'a+');fcntl.flock(lock,fcntl.LOCK_EX|fcntl.LOCK_NB)
sealed=pathlib.Path('/Users/rmccrar6/nudox-functional-corpus-20261006/sol61-real-applications-199b-20261007/docs/preserved-after-remove-timeout-199b-v2');root.mkdir(mode=0o700)
for part in ['project','home','evidence']:subprocess.run(['/bin/cp','-cR',str(sealed/part),str(root/part)],check=True)
workspace=pathlib.Path(config['workspace_default']);workspace.parent.mkdir(parents=True,exist_ok=True,mode=0o700)
subprocess.run(['/bin/cp','-cR',str(sealed/'workspace'),str(workspace)],check=True)
for ancestor in [root/'home',root/'home/Library',root/'home/Library/Application Support',root/'home/Library/Application Support/Nudox',workspace.parent,workspace]:ancestor.chmod(0o700)
receipt=dict(admission=a,started=now.isoformat(),sealed=str(sealed),root=str(root),workspace_default=str(workspace),endpoint_default=config['endpoint_default'],copy_argv='/bin/cp -cR',root_untouched=True,files={})
for part,path in [('project',root/'project'),('workspace',workspace),('home',root/'home')]:
 files={}
 for p in sorted(path.rglob('*')):
  if p.is_file() and not p.is_symlink():files[str(p.relative_to(path))]=dict(bytes=p.stat().st_size,sha256=hashlib.sha256(p.read_bytes()).hexdigest())
 receipt['files'][part]=files
(root/'evidence/owned-cow-receipt.json').write_text(json.dumps(receipt,indent=2));print(json.dumps(dict(root=str(root),workspace=str(workspace),file_count=sum(map(len,receipt['files'].values())))))
