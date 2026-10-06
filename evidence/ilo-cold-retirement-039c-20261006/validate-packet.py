from pathlib import Path
import json,tarfile,hashlib,ast,shutil
base=Path('evidence/ilo-cold-retirement-039c-20261006');archive=base/'runtime.tar.gz';index=json.loads((base/'archive-sha256.json').read_text())
assert hashlib.file_digest(archive.open('rb'),'sha256').hexdigest()==index['archive_sha256'];assert archive.stat().st_size==index['archive_bytes'];files={}
with tarfile.open(archive) as tar:
    for m in tar.getmembers():
        content=tar.extractfile(m).read();assert len(content)==index['members'][m.name]['bytes'];assert hashlib.sha256(content).hexdigest()==index['members'][m.name]['sha256'];files[m.name]=content
checks={'members_verified':len(files),'archive_sha256':index['archive_sha256']}
a=json.loads(files['paired/cmdline-observer/receipt.json']);b=json.loads(files['paired/kernel-pidfd/receipt.json']);obs=next(e for e in a['events'] if e['kind']=='retirement-observation')
assert not obs['kernel_pidfd_exited'] and not obs['cmdline_matches'];assert ' Z ' in obs['snapshot']['stat'] and '\nThreads:\t5\n' in obs['snapshot']['status'];pid=obs['snapshot']['pid'];trace=files['paired/cmdline-observer/first-cold-health.strace'].decode();assert f'{{pid={pid}, uid=0, gid=0}}' in trace and 'ECONNRESET' in trace;assert 'backend-locald' not in trace
frame=files['paired/cmdline-observer/first-cold-health.strace.client-sent.bin'];assert int.from_bytes(frame[:4],'big')==len(frame)-4;body=frame[4:];request=json.loads(body);assert request['version']==20 and request['request_id']==1 and request['command']['kind']=='health'
(base/'failed-health.dto20.json').write_bytes(body);(base/'failed-health.frame.bin').write_bytes(frame);(base/'failed-health.strace').write_bytes(files['paired/cmdline-observer/first-cold-health.strace'])
checks['cause']={'old_pid':pid,'cmdline_empty':True,'leader_state':'Z','leader_threads':5,'pidfd_not_ready':True,'connected_peer_pid':pid,'recvfrom':'ECONNRESET','no_new_owner_exec':True,'request_id':1,'command':'health','request_frame_bytes':len(frame)}
checks['paired']={}
for label,r in [('cmdline',a),('kernel-pidfd',b)]:
    assert r['original_state_unchanged'] and r['original_source_unchanged'] and r['remaining_owners']==[]
    first=next(c for c in r['commands'] if c['label']=='first-cold-health');second=next(c for c in r['commands'] if c['label']=='second-cold-health');checks['paired'][label]={'first_exit':first['exit'],'first_seconds':first['seconds'],'second_exit':second['exit'],'second_seconds':second['seconds']}
assert checks['paired']['cmdline']['first_exit']==1 and checks['paired']['kernel-pidfd']['first_exit']==0
checks['repaired']={}
for label in ['quart039c','click039c']:
    r=json.loads(files['repaired/'+label+'/receipt.json']);assert r['original_state_unchanged'] and r['original_source_unchanged'] and r['remaining_owners']==[];assert all(c['exit']==0 for c in r['commands']);proof=next(e['proof'] for e in r['events'] if e['kind']=='retirement-proof');assert proof['exit_proof']=='linux-pidfd';first=next(c for c in r['commands'] if c['label']=='first-cold-health');health=json.loads(first['stdout']);statuses=[e['result']['result'] for e in r['events'] if e['kind']=='mcp-status'];assert len(statuses)==2
    for s in statuses:
        assert not s['isError'];status=s['structuredContent'];assert (status['revision'],status['source'],status['sequence'],status['rows'])==(health['revision'],health['source'],health['sequence'],health['rows'])
    checks['repaired'][label]={'first_exit':first['exit'],'first_seconds':first['seconds'],'revision':health['revision'],'sequence':health['sequence'],'rows':health['rows'],'cli_mcp_roots_equal':True,'retirement':proof,'original_state_source_unchanged':True,'remaining_owners':0}
r=json.loads(files['repaired/retirement-pidfd-timeout.json']);assert r['outcome']=='bounded-timeout' and r['child_still_alive'] and r['seconds']<1 and r['cleanup_exit']==-9;checks['linux_timeout']=r
r=json.loads((base/'mac-owned-child-timeout.json').read_text());assert r['outcome']=='bounded-timeout' and r['child_still_alive'] and r['seconds']<1 and r['cleanup_exit']==-9;checks['mac_owned_child_timeout']=r
p=Path('tests/journeys/scripts/fresh-remote-product-journey.py');ast.parse(p.read_text());checks['observer_sha256']=hashlib.file_digest(p.open('rb'),'sha256').hexdigest();assert checks['observer_sha256']==r['observer_sha256']
(base/'checks.json').write_text(json.dumps(checks,indent=2)+'\n');print(json.dumps(checks,indent=2))
