"""Controlled SQLite companion custody tests; no native build or signing."""
import hashlib
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import sqlite_macos_development as recipe


def observation(lock):
    return {'state':'observed-build-helper', 'artifact_identity':'1'*64,
            'basis':{'engine':'Syntaqlite4e4de76','native_revision':recipe.REVISION,'recipe':recipe.RECIPE,
                     'lock_sha256':list(bytes.fromhex(lock)), 'support_sha256':[7]*32,
                     'settings':{'Sqlite':{'version':2147483647,'cflags':0,'analyzer_dqs_fallback':True}}}}


class SqliteDevelopmentTests(unittest.TestCase):
    def test_basis_rejects_wrong_lock_support_or_native_settings(self):
        lock = 'a'*64
        good = observation(lock)
        identity, basis = recipe.validate_observation(good, lock)
        self.assertEqual(identity, '1'*64)
        self.assertEqual(json.loads(basis), good['basis'])
        for field, value in [('lock_sha256',[0]*32), ('support_sha256',[0]*32),
                             ('settings',{'Sqlite':{'version':1,'cflags':0,'analyzer_dqs_fallback':True}})]:
            item = observation(lock)
            item['basis'][field] = value
            with self.subTest(field=field), self.assertRaises(ValueError):
                recipe.validate_observation(item, lock)
        item = observation(lock); item['artifact_identity'] = '0'*64
        with self.assertRaises(ValueError): recipe.validate_observation(item, lock)
        item = observation(lock); item['basis']['settings']['Sqlite']['cflags'] = False
        with self.assertRaises(ValueError): recipe.validate_observation(item, lock)

    def test_worker_and_observer_share_build_and_post_sign_tamper_fails(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            source, macos, target, build, logs = [root/x for x in ('source','macos','target','build','logs')]
            for p in (source,macos,target,build): p.mkdir()
            sqlite = source/'frontends/sqlite'; sqlite.mkdir(parents=True)
            (sqlite/'Cargo.toml').write_text('[package]\n')
            (sqlite/'Cargo.lock').write_bytes(b'lock')
            lock_hash=hashlib.sha256(b'lock').hexdigest()
            mutation = {'target': None}
            class Companion:
                @staticmethod
                def digest_regular(path, maximum=1<<29, **kwargs):
                    data=path.read_bytes()
                    if len(data)>maximum: raise ValueError('too large')
                    return len(data),hashlib.sha256(data).hexdigest()
                @staticmethod
                def inspect_system_executable(path, target, **kwargs):
                    size, h = Companion.digest_regular(path)
                    return {'bytes':size,'sha256':h,'architecture':'arm64','system_loads':[]}
                @staticmethod
                def run_tool(command, **kwargs):
                    if command[0].endswith('installed_basis_observer'):
                        if mutation['target'] == 'observer': Path(command[0]).write_bytes(b'changed observer')
                        if mutation['target'] == 'worker': Path(command[1]).write_bytes(b'changed worker')
                        return json.dumps(observation(lock_hash)).encode()
                    return b''
            def execute(command, cwd, env, logs, deadline, maximum):
                self.assertIn('--locked',command);self.assertIn('--offline',command)
                self.assertIn('sqlite-worker',command);self.assertIn('installed_basis_observer',command)
                release=Path(env['CARGO_TARGET_DIR'])/'aarch64-apple-darwin/release';(release/'examples').mkdir(parents=True)
                (release/'sqlite-worker').write_bytes(b'worker')
                (release/'examples/installed_basis_observer').write_bytes(b'observer')
                return {'exit':0,'process_group_retired':True}
            def copy(src,dst,deadline): dst.write_bytes(src.read_bytes())
            with patch.object(recipe.common,'execute',side_effect=execute):
                record=recipe.prepare(source=source,macos=macos,target=target,build=build,
                    env={'CARGO_BUILD_JOBS':'2'},target_triple='aarch64-apple-darwin',deadline=999999999,
                    logs=logs,companion=Companion,copy_exact=copy,reject_private_paths=lambda *a:None,
                    private_paths=[source,target,build])
                self.assertEqual(record['owner_environment']['NUDOX_BUILD_SQLITE_WORKER_ID'],'1'*64)
                self.assertEqual(json.loads(record['owner_environment']['NUDOX_BUILD_SQLITE_BASIS_JSON']),observation(lock_hash)['basis'])
                recipe.verify_after_signing(record,source=source,target_triple='aarch64-apple-darwin',deadline=999999999,companion=Companion)
                mutation['target']='observer'
                with self.assertRaisesRegex(ValueError,'observer changed while rechecking'):
                    recipe.verify_after_signing(record,source=source,target_triple='aarch64-apple-darwin',deadline=999999999,companion=Companion)
                Path(record['observer_path']).write_bytes(b'observer')
                mutation['target']='worker'
                with self.assertRaisesRegex(ValueError,'worker changed while rechecking'):
                    recipe.verify_after_signing(record,source=source,target_triple='aarch64-apple-darwin',deadline=999999999,companion=Companion)
                mutation['target']=None
                (macos/'sqlite-worker').write_bytes(b'tampered')
                with self.assertRaisesRegex(ValueError,'changed SQLite worker'):
                    recipe.verify_after_signing(record,source=source,target_triple='aarch64-apple-darwin',deadline=999999999,companion=Companion)


if __name__ == '__main__':
    unittest.main()
