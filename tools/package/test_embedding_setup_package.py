"""Source/byte custody controls only; fixtures never execute a helper or infer vectors."""
import copy
import hashlib
import json
from pathlib import Path
import tempfile
import unittest
import linux_release_package as package
import install_linux as installer

class EmbeddingPackageControls(unittest.TestCase):
    def test_helper_receipt_requires_successful_same_source_cpu_build_and_exact_bytes(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            helper = root / 'helper'
            helper.write_bytes(b'non-executable metadata fixture')
            receipt = {'schema':'nudox.embedding-helper-build-receipt.v1','preset':'candle-minilm-cpu-v1',
                       'source':{'commit':'a'*40,'tree':'b'*40,'cargo_lock_sha256':'c'*64,'clean_before':True,'clean_after':True},
                       'toolchain':{'unchanged':True},'exit':0,
                       'argv':['cargo','build','--release','--locked','--no-default-features','--features','cpu','-p','backend-embedding-candle'],
                       'artifact':{'bytes':helper.stat().st_size,'sha256':hashlib.sha256(helper.read_bytes()).hexdigest()}}
            path = root / 'receipt.json'
            def admit(value):
                path.write_text(json.dumps(value))
                return package.admit_embedding_helper(helper,path,'a'*40,'b'*40,'c'*64)
            record, digest = admit(receipt)
            self.assertEqual(record, receipt['artifact'])
            self.assertEqual(digest, hashlib.sha256(path.read_bytes()).hexdigest())
            for mutation in ('exit','source','argv','bytes'):
                changed = copy.deepcopy(receipt)
                if mutation == 'exit': changed['exit'] = False
                if mutation == 'source': changed['source']['tree'] = 'd'*40
                if mutation == 'argv': changed['argv'].remove('--locked')
                if mutation == 'bytes': changed['artifact']['sha256'] = 'd'*64
                with self.subTest(mutation=mutation), self.assertRaises(ValueError): admit(changed)

    def test_installed_optional_receipt_refuses_hash_or_dependency_change_without_launch(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            def record(relative, raw):
                path = root / relative
                path.parent.mkdir(parents=True,exist_ok=True)
                path.write_bytes(raw)
                path.chmod(0o755)
                return {'path':relative,'bytes':len(raw),'sha256':hashlib.sha256(raw).hexdigest()}
            source_lock = record('share/nudox/embedding/Cargo.lock',b'fixture Cargo.lock')
            build_receipt = record('share/nudox/embedding/build-receipt.json',b'fixture build receipt')
            source = {'commit':'a'*40,'tree':'b'*40,'cargo_lock_sha256':source_lock['sha256']}
            receipt = {'schema':'nudox.local-embedding-setup.v1','preset':'candle-minilm-cpu-v1','source':source,
                       'cli':record('bin/backend-cli',b'fixture CLI'),
                       'helper':record('share/nudox/embedding/bin/backend-embedding-candle',b'fixture helper'),
                       'dependencies':[record('lib/dependency.so',b'fixture dependency')],
                       'build_receipt_sha256':build_receipt['sha256'],'build_receipt':build_receipt,'source_lock':source_lock,'helper_identity':'e'*64}
            path = root / 'share/nudox/embedding/setup.json'
            path.write_text(json.dumps(receipt))
            metadata = {'source':source,'embedding_setup':{'path':'share/nudox/embedding/setup.json','sha256':hashlib.sha256(path.read_bytes()).hexdigest()}}
            installer.verify_embedding_setup(root,metadata)
            (root/'lib/dependency.so').write_bytes(b'changed')
            with self.assertRaises(installer.InstallError): installer.verify_embedding_setup(root,metadata)
            installer.verify_embedding_setup(root,{'embedding_setup':None})
            metadata['embedding_setup']['sha256'] = 'f'*64
            with self.assertRaises(installer.InstallError): installer.verify_embedding_setup(root,metadata)

if __name__ == '__main__':
    unittest.main()
