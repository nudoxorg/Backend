import contextlib, importlib.util, io, json, hashlib, os, pathlib, tarfile, tempfile, types, unittest
from unittest.mock import patch
ROOT=pathlib.Path(__file__).parent
def load(name):
 spec=importlib.util.spec_from_file_location(name,ROOT/(name+'.py')); module=importlib.util.module_from_spec(spec); spec.loader.exec_module(module); return module
boot=load('install_preview'); mac=load('install_macos_preview'); linux=load('install_linux')
class Tests(unittest.TestCase):
 def test_unsupported_platforms(self):
  for system,machine in [('Windows','amd64'),('Linux','arm64'),('Darwin','x86_64')]:
   with self.assertRaises(boot.InstallError): boot.platform_key(system,machine)
  self.assertEqual(boot.platform_key('Linux','x86_64'),'linux-x64')
  self.assertEqual(boot.platform_key('Darwin','arm64'),'macos-arm64')
 def test_https_and_hash_gate(self):
  for url in ['http://github.com/nudoxorg/Backend/releases/download/x/a','https://example.org/a']:
   with self.assertRaises(boot.InstallError): boot.check_url(url)
  with patch.object(boot,'fetch',return_value=b'bad'):
   with self.assertRaises(boot.InstallError): boot.verified({'url':boot.CHANNEL,'sha256':'a'*64,'bytes':3},100)
 def archive(self,root,extra=None,source='a'*40):
  native=b'{"source":{"commit":"'+source.encode()+b'"}}'
  files={**{'bin/'+n:b'#!/bin/sh\nexit 0\n' for n in mac.REQUIRED_BINARIES},'native-build-manifest.json':native,'README.txt':b'diagnostic'}
  manifest={'schema':'nudox.mac-cli-diagnostic-preview.v1','source':source,'production_ready':False,'runtime_manifest_sha256':hashlib.sha256(native).hexdigest(),'files':[{'packaged':p,'packaged_sha256':hashlib.sha256(b).hexdigest()} for p,b in files.items() if p.startswith('bin/')]}
  files['manifest.json']=json.dumps(manifest).encode(); archive=root/'test.tar.gz'
  with tarfile.open(archive,'w:gz') as t:
   for name,body in files.items():
    info=tarfile.TarInfo('nudox-macos-arm64/'+name); info.size=len(body); t.addfile(info,io.BytesIO(body))
   if extra: t.addfile(extra,io.BytesIO(b'x') if extra.isreg() else None)
  return archive
 def mac_release(self,root,source='a'*40,date='20261009'):
  root.mkdir(parents=True,exist_ok=True)
  archive=self.archive(root,source=source)
  tag='checkpoint-'+date+'-'+source[:10]+'-macos-arm64'
  entry={'tag':tag,'version':'0.0.0','source_sha':source,'asset':'nudox-macos-arm64-'+source[:10]+'.tar.gz'}
  manifest={'sha256':hashlib.sha256(archive.read_bytes()).hexdigest()}
  args=['--archive-url','https://github.com/nudoxorg/Backend/releases/download/'+tag+'/'+entry['asset'],
        '--archive-sha256',manifest['sha256'],'--archive-size',str(archive.stat().st_size),
        '--source',source,'--tag',tag]
  return entry,manifest,archive,args
 @contextlib.contextmanager
 def mac_host(self):
  with patch.object(mac.sys,'platform','darwin'),patch.object(mac.os,'uname',return_value=types.SimpleNamespace(machine='arm64')):
   yield
 def install_fixture(self,prefix,entry,manifest,archive):
  old_umask=os.umask(0o077)
  try: mac.install(prefix,entry,manifest,archive)
  finally: os.umask(old_umask)
 def test_traversal_and_links_and_duplicate_rejected(self):
  for name,kind in [('nudox-macos-arm64/../escape',tarfile.REGTYPE),('nudox-macos-arm64/link',tarfile.SYMTYPE),('nudox-macos-arm64/README.txt',tarfile.REGTYPE)]:
   with tempfile.TemporaryDirectory() as temp:
    root=pathlib.Path(temp); member=tarfile.TarInfo(name); member.type=kind; member.size=1 if kind==tarfile.REGTYPE else 0; member.linkname='/etc/passwd'
    archive=self.archive(root,member); stage=root/'stage'; stage.mkdir()
    with self.assertRaises(mac.InstallError): mac.safe_extract(archive,stage)
    self.assertFalse((root/'escape').exists())
 def test_reinstall_integrity_and_unowned_command(self):
  with tempfile.TemporaryDirectory() as temp:
   root=pathlib.Path(temp); archive=self.archive(root); prefix=root/'prefix'; entry={'tag':'checkpoint-20261009-'+('a'*10)+'-macos-arm64','version':'0.0.0','source_sha':'a'*40,'asset':'nudox-macos-arm64-'+('a'*10)+'.tar.gz'}; manifest={'sha256':hashlib.sha256(archive.read_bytes()).hexdigest()}
   mac.install(prefix,entry,manifest,archive); current=prefix/'lib/nudox/current'
   before=current.resolve(); mac.install(prefix,entry,manifest,archive); self.assertEqual(current.resolve(),before)
   (before/'bin/backend-cli').write_bytes(b'tampered')
   with self.assertRaises(mac.InstallError): mac.install(prefix,entry,manifest,archive)
   self.assertEqual(current.resolve(),before)
   other=root/'other'; (other/'bin').mkdir(parents=True); (other/'bin/nudox').write_bytes(b'user command')
   with self.assertRaises(mac.InstallError): mac.install(other,entry,manifest,archive)
   self.assertEqual((other/'bin/nudox').read_bytes(),b'user command')
 def test_channel_update_dispatch(self):
  channel=json.loads((ROOT/'preview-channel.json').read_text())
  channel['bootstrap']['sha256']='b'*64; channel['bootstrap']['bytes']=3
  with patch.object(boot,'platform_key',return_value='linux-x64'),patch.object(boot,'fetch',return_value=json.dumps(channel).encode()),patch.object(boot,'verified',return_value=b'new') as download,patch.object(boot.subprocess,'call',return_value=0) as execute:
   self.assertEqual(boot.main(['--prefix','/tmp/preview-test-prefix']),0)
   self.assertEqual(download.call_count,1); self.assertIn('--prefix',execute.call_args.args[0])
 def test_platform_dispatch_forwards_explicit_downgrade(self):
  channel=json.loads((ROOT/'preview-channel.json').read_text())
  channel['bootstrap']['sha256']=hashlib.sha256((ROOT/'install_preview.py').read_bytes()).hexdigest()
  for platform in ['linux-x64','macos-arm64']:
   with self.subTest(platform=platform),patch.object(boot,'platform_key',return_value=platform),patch.object(boot,'fetch',return_value=json.dumps(channel).encode()),patch.object(boot,'verified',return_value=b'platform') as download,patch.object(boot.subprocess,'call',return_value=0) as execute:
    self.assertEqual(boot.main(['--prefix','/tmp/preview-test-prefix','--allow-downgrade']),0)
    self.assertEqual(download.call_args.args[0],channel['platforms'][platform]['installer'])
    self.assertIn('--allow-downgrade',execute.call_args.args[0]); self.assertIn('/tmp/preview-test-prefix',execute.call_args.args[0])
 def test_checkpoint_guard_preserves_current_before_extraction_on_both_platforms(self):
  for module,platform in [(mac,'macos-arm64'),(linux,'linux-x64')]:
   with self.subTest(platform=platform),tempfile.TemporaryDirectory() as temp:
    prefix=pathlib.Path(temp).resolve()/'prefix'; managed=prefix/'lib/nudox'
    tag='checkpoint-20261009-'+('a'*10)+'-'+platform
    active=managed/'versions'/tag; active.mkdir(parents=True)
    marker=active/'.installed-release.json'; installed={'tag':tag,'version':'0.0.0','source_sha':'a'*40,'asset':('nudox-macos-arm64-' if platform=='macos-arm64' else 'nudox-linux-x86_64-')+('a'*10)+'.tar.gz','sha256':'c'*64}; marker.write_text(json.dumps(installed))
    current=managed/'current'; current.symlink_to(pathlib.Path('versions')/tag)
    before=marker.read_bytes(); pointer=os.readlink(current)
    for incoming,reason in [('checkpoint-20261008-'+('b'*10)+'-'+platform,'older'),
                            ('checkpoint-20261009-'+('b'*10)+'-'+platform,'different same-date')]:
     with patch.object(module,'safe_extract') as extract:
      with self.assertRaisesRegex(module.InstallError,reason):
       module.install(prefix,{**installed,'tag':incoming,'source_sha':'b'*40,'asset':installed['asset'].replace('a'*10,'b'*10)},{'sha256':'d'*64},pathlib.Path(temp)/'not-downloaded.tar.gz')
      extract.assert_not_called()
     self.assertEqual(os.readlink(current),pointer); self.assertEqual(marker.read_bytes(),before)
    module.check_checkpoint_update(current,installed)
    module.check_checkpoint_update(current,{**installed,'tag':'checkpoint-20261010-'+('b'*10)+'-'+platform,'source_sha':'b'*40,'asset':installed['asset'].replace('a'*10,'b'*10)})
    module.check_checkpoint_update(current,{**installed,'tag':'checkpoint-20261008-'+('b'*10)+'-'+platform,'source_sha':'b'*40,'asset':installed['asset'].replace('a'*10,'b'*10)},True)
 def test_actual_same_day_override_and_idempotent_reinstall_keep_verified_files(self):
  with tempfile.TemporaryDirectory() as temp:
   root=pathlib.Path(temp).resolve(); prefix=root/'prefix'
   def package(source,date):
    directory=root/(source[0]+date); directory.mkdir()
    archive=self.archive(directory,source=source)
    entry={'tag':'checkpoint-'+date+'-'+source[:10]+'-macos-arm64','version':'0.0.0','source_sha':source,'asset':'nudox-macos-arm64-'+source[:10]+'.tar.gz'}
    return entry,{'sha256':hashlib.sha256(archive.read_bytes()).hexdigest()},archive
   first=package('a'*40,'20261009'); other=package('b'*40,'20261009')
   mac.install(prefix,*first); current=prefix/'lib/nudox/current'
   original=current.resolve(); files={p.name:p.read_bytes() for p in (original/'bin').iterdir()}
   mac.install(prefix,*first); self.assertEqual(current.resolve(),original)
   self.assertEqual({p.name:p.read_bytes() for p in (original/'bin').iterdir()},files)
   with self.assertRaisesRegex(mac.InstallError,'different same-date'): mac.install(prefix,*other)
   self.assertEqual(current.resolve(),original)
   mac.install(prefix,*other,allow_downgrade=True); changed=current.resolve()
   self.assertNotEqual(changed,original); self.assertTrue(original.is_dir())
   mac.install(prefix,*other); self.assertEqual(current.resolve(),changed)
 def test_main_reuses_verified_existing_release_before_archive_fetch(self):
  with tempfile.TemporaryDirectory() as temp:
   root=pathlib.Path(temp).resolve(); entry,manifest,archive,args=self.mac_release(root/'archive')
   prefix=root/'prefix'; self.install_fixture(prefix,entry,manifest,archive)
   current=prefix/'lib/nudox/current'; before=current.lstat(); target=current.resolve()
   with self.mac_host(),patch.object(mac,'download_archive') as download,contextlib.redirect_stdout(io.StringIO()) as stdout,contextlib.redirect_stderr(io.StringIO()) as stderr:
    self.assertEqual(mac.main(args+['--prefix',str(prefix)]),0)
   download.assert_not_called()
   self.assertIn('reused it without downloading the archive',stderr.getvalue())
   self.assertIn(str(prefix),stdout.getvalue())
   self.assertEqual(os.readlink(current),'versions/'+entry['tag'])
   self.assertEqual((current.lstat().st_dev,current.lstat().st_ino),(before.st_dev,before.st_ino))
   for alias,binary in mac.command_links(prefix/'lib/nudox').items():
    self.assertEqual(os.readlink(prefix/'bin'/alias),binary)
   self.assertEqual(current.resolve(),target)
 def test_main_reuse_rejects_corrupt_file_manifest_and_package_symlink_before_fetch(self):
  for corruption in ['binary','manifest','package-symlink']:
   with self.subTest(corruption=corruption),tempfile.TemporaryDirectory() as temp:
    root=pathlib.Path(temp).resolve(); entry,manifest,archive,args=self.mac_release(root/'archive')
    prefix=root/'prefix'; self.install_fixture(prefix,entry,manifest,archive)
    release=(prefix/'lib/nudox/current').resolve()
    if corruption=='binary':
     (release/'bin/backend-cli').write_bytes(b'changed after install')
    elif corruption=='manifest':
     (release/'manifest.json').write_text('{"schema":"tampered"}')
    else:
     binary=release/'bin/backend-mcp'; body=binary.read_bytes(); binary.unlink()
     external=root/'external'; external.write_bytes(body); binary.symlink_to(external)
    with self.mac_host(),patch.object(mac,'download_archive') as download:
     with self.assertRaises(mac.InstallError): mac.main(args+['--prefix',str(prefix)])
    download.assert_not_called()
    self.assertEqual((prefix/'lib/nudox/current').resolve(),release)
 def test_main_reuse_refuses_an_unowned_command_alias_before_fetch(self):
  with tempfile.TemporaryDirectory() as temp:
   root=pathlib.Path(temp).resolve(); entry,manifest,archive,args=self.mac_release(root/'archive')
   prefix=root/'prefix'; self.install_fixture(prefix,entry,manifest,archive)
   alias=prefix/'bin/nudox'; alias.unlink(); external=root/'user-command'; external.write_text('keep')
   alias.symlink_to(external)
   with self.mac_host(),patch.object(mac,'download_archive') as download:
    with self.assertRaisesRegex(mac.InstallError,'outside the managed'): mac.main(args+['--prefix',str(prefix)])
   download.assert_not_called()
   self.assertEqual(alias.resolve(),external)
 def test_main_downloads_new_release_but_rejects_downgrade_before_fetch(self):
  with tempfile.TemporaryDirectory() as temp:
   root=pathlib.Path(temp).resolve()
   old,old_manifest,old_archive,_=self.mac_release(root/'old','a'*40,'20261009')
   new,new_manifest,new_archive,new_args=self.mac_release(root/'new','b'*40,'20261010')
   prefix=root/'prefix'; self.install_fixture(prefix,old,old_manifest,old_archive)
   old_path=(prefix/'lib/nudox/versions'/old['tag'])
   def deliver(url,digest,size,destination):
    self.assertEqual((url,digest,size),(new_args[1],new_manifest['sha256'],new_archive.stat().st_size))
    destination.write_bytes(new_archive.read_bytes())
   with self.mac_host(),patch.object(mac,'download_archive',side_effect=deliver) as download,contextlib.redirect_stdout(io.StringIO()):
    self.assertEqual(mac.main(new_args+['--prefix',str(prefix)]),0)
   download.assert_called_once()
   current=prefix/'lib/nudox/current'; new_path=current.resolve()
   self.assertEqual(new_path.name,new['tag']); self.assertTrue(old_path.is_dir())
   old_args=['--archive-url','https://github.com/nudoxorg/Backend/releases/download/'+old['tag']+'/'+old['asset'],
             '--archive-sha256',old_manifest['sha256'],'--archive-size',str(old_archive.stat().st_size),
             '--source',old['source_sha'],'--tag',old['tag'],'--prefix',str(prefix)]
   with self.mac_host(),patch.object(mac,'download_archive') as download:
    with self.assertRaisesRegex(mac.InstallError,'older'): mac.main(old_args)
   download.assert_not_called(); self.assertEqual(current.resolve(),new_path)
   with self.mac_host(),patch.object(mac,'download_archive') as download,contextlib.redirect_stdout(io.StringIO()):
    self.assertEqual(mac.main(old_args+['--allow-downgrade']),0)
   download.assert_not_called(); self.assertEqual(current.resolve(),old_path)
 def test_network_failure_keeps_fresh_prefix_uncreated(self):
  with tempfile.TemporaryDirectory() as temp:
   root=pathlib.Path(temp).resolve(); entry,manifest,archive,args=self.mac_release(root/'archive')
   prefix=root/'prefix'
   with self.mac_host(),patch.object(mac,'download_archive',side_effect=mac.InstallError('simulated network failure')):
    with self.assertRaisesRegex(mac.InstallError,'simulated network failure'): mac.main(args+['--prefix',str(prefix)])
   self.assertFalse(prefix.exists())
 def test_download_reports_bounded_byte_progress_for_real_archive_reads(self):
  class Response(io.BytesIO):
   def geturl(self): return 'https://github.com/nudoxorg/Backend/releases/download/checkpoint-20261009-aaaaaaaaaa-macos-arm64/archive.tar.gz'
  class Opener:
   def __init__(self,body): self.response=Response(body); self.timeouts=[]
   def open(self,request,timeout): self.timeouts.append(timeout); return self.response
  with tempfile.TemporaryDirectory() as temp:
   body=b'x'*(mac.DOWNLOAD_PROGRESS_BYTES+17); opener=Opener(body); destination=pathlib.Path(temp)/'archive.tar.gz'
   with patch.object(mac,'HTTPS',opener),contextlib.redirect_stderr(io.StringIO()) as stderr:
    mac.download_archive('https://github.com/nudoxorg/Backend/releases/download/checkpoint-20261009-aaaaaaaaaa-macos-arm64/archive.tar.gz',
                         hashlib.sha256(body).hexdigest(),len(body),destination)
   output=stderr.getvalue()
   self.assertIn('0%',output); self.assertIn('8,388,608/',output); self.assertIn('100%',output)
   self.assertIn('verifying the pinned SHA-256',output)
   self.assertEqual(opener.timeouts,[mac.DOWNLOAD_TIMEOUT_SECONDS])
   self.assertEqual(destination.read_bytes(),body)
if __name__=='__main__': unittest.main()

class PipeTests(unittest.TestCase):
 def test_actual_stdin_bootstrap_dispatches_verified_child(self):
  import subprocess,sys
  channel=json.loads((ROOT/'preview-channel.json').read_text())
  child=b"import sys; print('VERIFIED CHILD', sys.argv[1:])\n"
  channel['bootstrap']['sha256']=hashlib.sha256(child).hexdigest(); channel['bootstrap']['bytes']=len(child)
  for key,entry in channel['platforms'].items(): entry['tag']='checkpoint-20270101-'+entry['source'][:10]+'-'+key
  source=(ROOT/'install_preview.py').read_text()
  override="\ndef fetch(url,limit):\n    return "+repr(json.dumps(channel).encode())+" if url==CHANNEL else "+repr(child)+"\n"
  source=source.replace("if __name__=='__main__':",override+"\nif __name__=='__main__':")
  with tempfile.TemporaryDirectory() as temp:
   script=pathlib.Path(temp)/'install.py'; script.write_text(source)
   for mode in ['stdin','file']:
    for allow in [False,True]:
     with self.subTest(mode=mode,allow=allow):
      command=[sys.executable,'-' if mode=='stdin' else str(script),'--prefix','/tmp/nudox-pipe-control']+(['--allow-downgrade'] if allow else [])
      result=subprocess.run(command,input=source if mode=='stdin' else None,text=True,capture_output=True,timeout=10)
      self.assertEqual(result.returncode,0,result.stderr)
      self.assertIn('VERIFIED CHILD',result.stdout); self.assertIn('/tmp/nudox-pipe-control',result.stdout)
      self.assertEqual('--allow-downgrade' in result.stdout,allow)
