import subprocess,json
from pathlib import Path
python='/nix/store/llk2h8rxqzv7zh53bi413ffibjrxskxw-python3-3.14.6/bin/python3'
report=Path('/private/tmp/httpie-context-0cf-claim-fresh-admission.json')
collector='/Users/mileswirht/Downloads/nudox-active-20261005/corpus-control/.config/scripts/fleet-capacity-census.py'
subprocess.run([python,'-B',collector,'--destination','h16001mac','--jobs','4','--allow-local-over-cap-for-remote','--output',str(report)],check=True,stdout=open('/private/tmp/httpie-context-0cf-claim-fresh-census.stdout','wb'))
assert json.loads(report.read_text())['advisory_allowed'] is True
coord='/Users/rmccrar6/nudox-functional-corpus-20261006/sol61-coordination/'
subprocess.run(['scp','-F','/Users/mileswirht/.ssh/config_external',str(report),'h16001mac:'+coord],check=True)
command='/usr/bin/python3 '+coord+'locked-httpie-claim.py '+coord+report.name+' /bin/bash '+coord+'run-httpie-claim.sh'
result=subprocess.run(['ssh','-F','/Users/mileswirht/.ssh/config_external','-o','BatchMode=yes','h16001mac',command],stdout=open('/private/tmp/httpie-context-0cf-claim-fresh-ssh.stdout','wb'),stderr=open('/private/tmp/httpie-context-0cf-claim-fresh-ssh.stderr','wb'))
raise SystemExit(result.returncode)
