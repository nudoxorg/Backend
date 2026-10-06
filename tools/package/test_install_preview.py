import importlib.util, io, json, hashlib, pathlib, tarfile, tempfile, unittest
from unittest.mock import patch
ROOT=pathlib.Path(__file__).parent
def load(name):
 spec=importlib.util.spec_from_file_location(name,ROOT/(name+'.py')); module=importlib.util.module_from_spec(spec); spec.loader.exec_module(module); return module
boot=load('install_preview'); mac=load('install_macos_preview')
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
 def archive(self,root,extra=None):
  native=b'{"source":{"commit":"'+b'a'*40+b'"}}'
  files={**{'bin/'+n:b'#!/bin/sh\nexit 0\n' for n in mac.REQUIRED_BINARIES},'native-build-manifest.json':native,'README.txt':b'diagnostic'}
  manifest={'schema':'nudox.mac-cli-diagnostic-preview.v1','source':'a'*40,'production_ready':False,'runtime_manifest_sha256':hashlib.sha256(native).hexdigest(),'files':[{'packaged':p,'packaged_sha256':hashlib.sha256(b).hexdigest()} for p,b in files.items() if p.startswith('bin/')]}
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
if __name__=='__main__': unittest.main()
