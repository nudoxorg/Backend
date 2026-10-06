from pathlib import Path
import json,hashlib,re,ast,tarfile,io,shutil
BASE=Path('/root/nudox-corpus-20261006');OUT=BASE/'artifacts/sol-mac-first-cold-039c-packet-v2';OUT.mkdir(mode=0o700)
def digest(p):
    with p.open('rb') as f:return hashlib.file_digest(f,'sha256').hexdigest()
sources={};index={}
def add(name,content,source=None):
    p=OUT/name;p.parent.mkdir(parents=True,exist_ok=True);p.write_bytes(content);index[name]={'bytes':len(content),'sha256':hashlib.sha256(content).hexdigest()}
    if source:sources[name]={'path':str(source),'source_bytes':source.stat().st_size,'source_sha256':digest(source)}
def copy(name,p):add(name,p.read_bytes(),p)
for family,folder in [('baseline','sol-mac-independent-cold-039c-20261006'),('paired','sol-mac-independent-retirement-pair-039c-20261006'),('repaired','sol-mac-independent-repaired-retirement-039c-20261006')]:
    root=BASE/'runs'/folder
    for p in sorted(root.rglob('*')):
        if not p.is_file() or 'state' in p.relative_to(root).parts or 'home' in p.relative_to(root).parts or 'tmp' in p.relative_to(root).parts:continue
        relative=str(p.relative_to(root));name=family+'/'+relative
        if p.suffix in ['.json','.stdout','.stderr'] or p.name=='logged-locald':copy(name,p)
        elif p.suffix=='.strace':
            rows=p.read_bytes().splitlines(keepends=True)
            cli=None
            for row in rows:
                if b'execve("'+str(BASE/'artifacts/images-dto20-039c360d28-20261006T111152Z/backend-cli').encode()+b'"' in row:cli=row.split()[0];break
            # Keep all failed trace bytes. Successful slices retain every
            # real CLI syscall plus owner exec/bind/listen/exit and actual
            # local-control request/reply bytes, excluding unrelated traffic.
            if p.stat().st_size<10000:copy(name,p)
            else:
                selected=[row for row in rows if (cli and row.startswith(cli+b' ')) or b'ECONNRESET' in row or b'backend-locald' in row and b'execve(' in row or b'nudox-' in row and (b'bind(' in row or b'listen(' in row or b'SO_PEERCRED' in row or b'sendto(' in row or b'recvfrom(' in row)]
                add(name+'.selected',b''.join(selected),p)
            # Decode exact C-escaped syscall buffers; no DTO reserialization.
            frame=[];packets=[]
            for row in rows:
                text=row.decode(errors='replace')
                match=re.search(r'(?:sendto|recvfrom)\([^,]+, ("(?:\\.|[^"\\])*"),',text)
                if not match:continue
                try:body=ast.literal_eval('b'+match.group(1))
                except (ValueError,SyntaxError):continue
                if body.startswith(b'{"version":20,'):
                    parsed=json.loads(body);packets.append({'syscall_line':text.rstrip(),'body_sha256':hashlib.sha256(body).hexdigest(),'body_bytes':len(body),'request_id':parsed.get('request_id'),'body_file':name+'.wire-'+str(len(packets))+'.json'});add(packets[-1]['body_file'],body,p)
                if cli and row.startswith(cli+b' ') and b'sendto(' in row and b'UNIX-STREAM' in row:frame.append(body)
            if frame:add(name+'.client-sent.bin',b''.join(frame),p)
            add(name+'.wire-index.json',(json.dumps(packets,indent=2)+'\n').encode(),p)
for label,suffix in [('quart','20261006T112754Z-python-configured-0c54656f'),('click','20261006T113100Z-python-configured-e02a971f'),('typing-old9f','20261006T112150Z-python-configured-fc243dd7')]:
    run=BASE/'runs/fresh-remote-runtime'/suffix
    copy('originals/'+label+'/setup.json',run/'setup.json')
    for p in run.glob('cold-health*'):copy('originals/'+label+'/'+p.name,p)
    rows=[]
    for line in (run/'wire.jsonl').read_bytes().splitlines(keepends=True):
        d=json.loads(line)
        if d.get('kind')=='owner-stopped' and d.get('label')=='explicit-cold-boundary' or d.get('label') in ['cold-health','cold-health-explicit-retry']:rows.append(line)
    add('originals/'+label+'/cold-wire.jsonl',b''.join(rows),run/'wire.jsonl')
manifest=BASE/'receipts/dto20-039c360d28-runtime-build-20261006T111152Z-attempt03/runtime-build-manifest.json';copy('runtime-build-manifest.json',manifest)
for name in ['sol-mac-independent-first-cold-039c.py','sol-mac-independent-retirement-pair-039c.py','sol-mac-independent-repaired-retirement-039c.py','sol-mac-kernel-exit-journey-observer.py']:copy('harnesses/'+name,BASE/'artifacts'/name)
add('source-provenance.json',(json.dumps(sources,indent=2)+'\n').encode())
archive=OUT.with_suffix('.tar.gz')
with tarfile.open(archive,'w:gz') as tar:
    for name in sorted(index):
        body=(OUT/name).read_bytes();info=tarfile.TarInfo(name);info.size=len(body);info.mode=0o600;info.mtime=0;tar.addfile(info,io.BytesIO(body))
receipt={'archive':str(archive),'archive_bytes':archive.stat().st_size,'archive_sha256':digest(archive),'members':index,'full_unsliced_trace_provenance':'source-provenance.json','state_secrets_and_images_excluded':True};OUT.with_suffix('.sha256.json').write_text(json.dumps(receipt,indent=2)+'\n')
with tarfile.open(archive) as tar:
    for member in tar.getmembers():body=tar.extractfile(member).read();assert len(body)==index[member.name]['bytes'] and hashlib.sha256(body).hexdigest()==index[member.name]['sha256']
print(json.dumps({k:v for k,v in receipt.items() if k!='members'}),len(index),'members verified')
