import fcntl,json,os,subprocess,sys
from pathlib import Path
from datetime import datetime,timezone
coord=Path('/Users/rmccrar6/nudox-functional-corpus-20261006/sol61-coordination')
lease=open(coord/'compiler-index.lock','a+');fcntl.flock(lease,fcntl.LOCK_EX|fcntl.LOCK_NB)
admission=Path(sys.argv[1]);a=json.loads(admission.read_text());assert a['advisory_allowed'] is True
age=max((datetime.now(timezone.utc)-datetime.fromisoformat(h['census']['sample_started_at_utc'].replace('Z','+00:00'))).total_seconds() for h in a['raw_samples'].values());assert age<60
prefix=coord/'pending-capture-ceefaed-attempt01'
assert not prefix.with_suffix('.launch.json').exists()
command=sys.argv[2:]
with prefix.with_suffix('.stdout').open('wb') as out,prefix.with_suffix('.stderr').open('wb') as err:
 p=subprocess.Popen(command,stdout=out,stderr=err,start_new_session=True)
 r={'permit_pid':os.getpid(),'launcher_pid':p.pid,'source_commit':'ceefaed472f920eebf791cf8a204878294926773','started':datetime.now(timezone.utc).isoformat(),'lock':str(coord/'compiler-index.lock'),'admission':str(admission),'admission_age_seconds':age,'argv':command}
 prefix.with_suffix('.launch.json').write_text(json.dumps(r,indent=2)+'\n');code=p.wait()
r.update(exit=code,ended=datetime.now(timezone.utc).isoformat());prefix.with_suffix('.complete.json').write_text(json.dumps(r,indent=2)+'\n');raise SystemExit(code)
