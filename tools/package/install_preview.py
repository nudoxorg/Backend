#!/usr/bin/env python3
"""Install/update the diagnostic NuDox CLI preview for Linux x64 or Mac arm64."""
import argparse, hashlib, json, platform, re, subprocess, sys, tempfile
from pathlib import Path
import urllib.request, urllib.parse, urllib.error
CHANNEL='https://github.com/nudoxorg/Backend/releases/download/nudox-preview-installer/preview-channel.json'
MAX_SCRIPT=1024*1024
class InstallError(Exception): pass

def check_url(url,redirect=False):
    p=urllib.parse.urlsplit(url)
    host=p.hostname=='github.com' or (redirect and (p.hostname or '').endswith('.githubusercontent.com'))
    if p.scheme!='https' or not host or p.username or p.password or (not redirect and not p.path.startswith('/nudoxorg/Backend/releases/download/')):
        raise InstallError('preview assets must use HTTPS release URLs in nudoxorg/Backend')
class HTTPSRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self,req,fp,code,msg,headers,url):
        check_url(url,True)
        return super().redirect_request(req,fp,code,msg,headers,url)
def fetch(url,limit):
    check_url(url)
    with urllib.request.build_opener(HTTPSRedirect()).open(urllib.request.Request(url,headers={'User-Agent':'nudox-preview/1'}),timeout=60) as response:
        check_url(response.geturl(),True); body=response.read(limit+1)
    if len(body)>limit: raise InstallError('preview metadata/script exceeds limit')
    return body
def artifact(value,limit):
    if not isinstance(value,dict) or set(value)!={'url','sha256','bytes'}: raise InstallError('invalid preview artifact')
    check_url(value['url'])
    h=value['sha256']
    if not isinstance(h,str) or len(h)!=64 or any(c not in '0123456789abcdef' for c in h): raise InstallError('invalid SHA-256')
    if type(value['bytes']) is not int or not 0<value['bytes']<=limit: raise InstallError('invalid artifact size')
    return value
def verified(value,limit):
    artifact(value,limit); body=fetch(value['url'],value['bytes'])
    if len(body)!=value['bytes'] or hashlib.sha256(body).hexdigest()!=value['sha256']: raise InstallError('preview size/SHA-256 mismatch; nothing installed')
    return body
def platform_key(system=None,machine=None):
    system=system or platform.system(); machine=(machine or platform.machine()).lower()
    if system=='Linux' and machine in {'x86_64','amd64'}: return 'linux-x64'
    if system=='Darwin' and machine in {'arm64','aarch64'}: return 'macos-arm64'
    raise InstallError('preview unavailable for '+system+'/'+machine+'; supports Linux x64 and macOS arm64')
def validate_channel(channel):
    if not isinstance(channel,dict) or channel.get('schema')!='nudox.diagnostic-preview-channel.v1' or channel.get('production_ready') is not False: raise InstallError('explicit diagnostic-preview channel required')
    artifact(channel.get('bootstrap'),MAX_SCRIPT)
    platforms=channel.get('platforms')
    if not isinstance(platforms,dict) or set(platforms)!={'linux-x64','macos-arm64'}: raise InstallError('invalid preview platforms')
    for key,entry in platforms.items():
        if not isinstance(entry,dict) or entry.get('status')!='diagnostic-preview' or not entry.get('known_failures'): raise InstallError('diagnostic status/known failures required')
        artifact(entry.get('installer'),MAX_SCRIPT); artifact(entry.get('archive'),2*1024**3)
        source=entry.get('source')
        if not isinstance(source,str) or len(source)!=40 or any(c not in '0123456789abcdef' for c in source): raise InstallError('invalid source revision')
        if not isinstance(entry.get('tag'),str) or not re.fullmatch(r'checkpoint-[0-9]{8}-'+source[:10]+'-'+re.escape(key),entry['tag']): raise InstallError('tag/source/platform mismatch')
        if not isinstance(entry['known_failures'],list) or any(not isinstance(x,str) for x in entry['known_failures']): raise InstallError('invalid failure descriptions')
    return channel
def main(argv=None):
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--prefix',type=Path,help='install prefix (default ~/.local)')
    parser.add_argument('--allow-downgrade',action='store_true',help='allow an older or different same-date checkpoint')
    parser.add_argument('--channel-url',default=CHANNEL,help=argparse.SUPPRESS)
    args=parser.parse_args(argv); selected=platform_key()
    channel=validate_channel(json.loads(fetch(args.channel_url,65536)))
    with tempfile.TemporaryDirectory(prefix='nudox-preview-') as temp:
        script=Path(globals().get('__file__',''))
        current_sha=hashlib.sha256(script.read_bytes()).hexdigest() if script.is_file() else None
        if current_sha!=channel['bootstrap']['sha256']:
            path=Path(temp)/'install.py'; path.write_bytes(verified(channel['bootstrap'],MAX_SCRIPT))
            command=[sys.executable,str(path),'--channel-url',args.channel_url]
            if args.prefix: command+=['--prefix',str(args.prefix)]
            if args.allow_downgrade: command+=['--allow-downgrade']
            return subprocess.call(command)
        entry=channel['platforms'][selected]
        print('Diagnostic preview; whole-project acceptance is not established.',flush=True)
        for failure in entry['known_failures']: print('Known failure: '+failure,flush=True)
        path=Path(temp)/'platform-installer.py'; path.write_bytes(verified(entry['installer'],MAX_SCRIPT))
        command=[sys.executable,str(path)]
        if args.prefix: command+=['--prefix',str(args.prefix)]
        if args.allow_downgrade: command+=['--allow-downgrade']
        if selected=='macos-arm64': command+=['--archive-url',entry['archive']['url'],'--archive-sha256',entry['archive']['sha256'],'--archive-size',str(entry['archive']['bytes']),'--source',entry['source'],'--tag',entry['tag']]
        return subprocess.call(command)
if __name__=='__main__':
    try: raise SystemExit(main())
    except (InstallError,OSError,ValueError,urllib.error.URLError) as error:
        print('nudox preview installer: '+str(error),file=sys.stderr); raise SystemExit(1)
