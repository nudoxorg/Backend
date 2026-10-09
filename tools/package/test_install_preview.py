import importlib.util, io, json, hashlib, os, pathlib, tarfile, tempfile, unittest
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
 def test_traversal_and_links_and_duplicate_rejected(self):
  for name,kind in [('nudox-macos-arm64/../escape',tarfile.REGTYPE),('nudox-macos-arm64/link',tarfile.SYMTYPE),('nudox-macos-arm64/README.txt',tarfile.REGTYPE)]:
   with tempfile.TemporaryDirectory() as temp:
    root=pathlib.Path(temp); member=tarfile.TarInfo(name); member.type=kind; member.size=1 if kind==tarfile.REGTYPE else 0; member.linkname='/etc/passwd'
    archive=self.archive(root,member); stage=root/'stage'; stage.mkdir()
    with self.assertRaises(mac.InstallError): mac.safe_extract(archive,stage)
    self.assertFalse((root/'escape').exists())
 def test_reinstall_integrity_and_unowned_command(self):
  with tempfile.TemporaryDirectory() as temp:
   root=pathlib.Path(temp); archive=self.archive(root); prefix=root/'prefix'; entry={'tag':'checkpoint-test','version':'0.0.0','source_sha':'a'*40,'asset':'test.tar.gz'}; manifest={'sha256':hashlib.sha256(archive.read_bytes()).hexdigest()}
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
    marker=active/'.installed-release.json'; marker.write_text(json.dumps({'tag':tag}))
    current=managed/'current'; current.symlink_to(pathlib.Path('versions')/tag)
    before=marker.read_bytes(); pointer=os.readlink(current)
    for incoming,reason in [('checkpoint-20261008-'+('b'*10)+'-'+platform,'older'),
                            ('checkpoint-20261009-'+('b'*10)+'-'+platform,'different same-date')]:
     with patch.object(module,'safe_extract') as extract:
      with self.assertRaisesRegex(module.InstallError,reason):
       module.install(prefix,{'tag':incoming},{},pathlib.Path(temp)/'not-downloaded.tar.gz')
      extract.assert_not_called()
     self.assertEqual(os.readlink(current),pointer); self.assertEqual(marker.read_bytes(),before)
    module.check_checkpoint_update(current,tag)
    module.check_checkpoint_update(current,'checkpoint-20261010-'+('b'*10)+'-'+platform)
    module.check_checkpoint_update(current,'checkpoint-20261008-'+('b'*10)+'-'+platform,True)
 def test_actual_same_day_override_and_idempotent_reinstall_keep_verified_files(self):
  with tempfile.TemporaryDirectory() as temp:
   root=pathlib.Path(temp).resolve(); prefix=root/'prefix'
   def package(source,date):
    directory=root/(source[0]+date); directory.mkdir()
    archive=self.archive(directory,source=source)
    entry={'tag':'checkpoint-'+date+'-'+source[:10]+'-macos-arm64','version':'0.0.0','source_sha':source,'asset':'test.tar.gz'}
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
