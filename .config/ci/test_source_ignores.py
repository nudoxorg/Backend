"""Control source inventory coverage and ownership drift without executing Rust."""
from pathlib import Path
import json
import shutil
import sys
import subprocess
import tempfile
import unittest

from quarantine import validate
from datetime import date, datetime, timedelta, timezone
from source_ignores import discover, source_entries, validate_inventory


class SourceIgnoreTests(unittest.TestCase):
    def test_literals_nested_comments_characters_and_lifetimes_are_not_attributes(self):
        source = '''
// #[ignore] fn comment() {}
/* outer /* #[ignore] fn nested() {} */ end */
const TEXT: &str = "#[ignore] fn string() {}";
const RAW: &str = r###"#[ignore] fn raw() {}"###;
const BYTE: &[u8] = br##"#[ignore] fn byte() {}"##;
fn lifetime<'a>(value: &'a str) { let bracket = ']'; }
mod tests { #[ignore = "requires service"] #[test] fn real() {} }
'''
        rows = source_entries('src/lib.rs', source)
        self.assertEqual([row['filter'] for row in rows], ['source:src/lib.rs::tests::real'])
        self.assertEqual(rows[0]['attribute'], '#[ignore = "requires service"]')

    def test_conditional_raw_identifiers_and_comment_spacing_are_owned(self):
        source = '''
mod tests {
 #[cfg_attr(any(unix, windows), cfg_attr(feature = "slow", ignore = "service"))]
 #[test] fn conditional() {}
 #[cfg_attr(unix, doc = "ignore")] #[test] fn ordinary() {}
 #[ /* note */ r#ignore = "manual" ] #[test] fn raw_name() {}
}
'''
        rows = source_entries('src/lib.rs', source)
        self.assertEqual(len(rows), 2)
        self.assertTrue(rows[0]['filter'].endswith('::conditional'))
        self.assertTrue(rows[1]['filter'].endswith('::raw_name'))

    def test_same_name_in_different_modules_and_line_movement_keep_keys(self):
        source = 'mod a { #[ignore] fn same() {} } mod b { #[ignore] fn same() {} }'
        before = source_entries('src/lib.rs', source)
        after = source_entries('src/lib.rs', '// added comment\n' + source)
        self.assertEqual([row['filter'] for row in before], [row['filter'] for row in after])
        self.assertNotEqual(before[0]['filter'], before[1]['filter'])

    def fixture(self, root):
        subprocess.run(['git', 'init', '-q', str(root)], check=True)
        (root / 'src').mkdir()
        path = root / 'src/lib.rs'
        path.write_text('#[ignore = "manual"]\n#[test]\nfn example() {}\n')
        (root / 'docs').mkdir()
        (root / 'docs/example.rs').write_text('#[ignore] fn documentation() {}')
        subprocess.run(['git', '-C', str(root), 'add', '.'], check=True)
        rows = discover(root)
        self.assertEqual(len(rows), 1)
        entries = [dict(row, category='source-ignore-intake', reason='manual',
                        owner='NuDox Backend maintainers', review='docs/testing/quarantine.md#source-intake',
                        reviewed='2026-10-14', expires='2026-11-13') for row in rows]
        return path, {'schema': 1, 'exclude': entries}

    def test_new_changed_and_removed_ignore_require_an_explicit_registry_change(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            path, document = self.fixture(root)
            self.assertEqual(validate_inventory(root, document), [])
            path.write_text('#[ignore = "changed reason"] fn example() {}')
            self.assertTrue(any('attribute changed' in error for error in validate_inventory(root, document)))
            path.write_text('#[ignore] fn added() {}')
            errors = validate_inventory(root, document)
            self.assertTrue(any('unowned' in error for error in errors))
            self.assertTrue(any('stale' in error for error in errors))
            path.write_text('fn example() {}')
            self.assertTrue(any('stale' in error for error in validate_inventory(root, document)))

    def test_expiry_and_owner_checks_apply_without_reenabling_any_test(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            path, document = self.fixture(root)
            original = path.read_bytes()
            self.assertEqual(validate(document, date(2026, 10, 14)), [])
            self.assertTrue(any('expired' in error for error in validate(document, date(2026, 11, 13))))
            document['exclude'][0]['owner'] = ''
            self.assertTrue(any('owner' in error for error in validate(document, date(2026, 10, 14))))
            self.assertEqual(path.read_bytes(), original)

    def test_git_discovery_retains_conditional_and_comment_spaced_attributes(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            path, document = self.fixture(root)
            path.write_text('#[cfg_attr(unix, ignore = "service")] fn conditional() {}\n'
                            '#[ /* note */ r#ignore ] fn spaced() {}')
            rows = discover(root)
            self.assertEqual(len(rows), 2)
            self.assertTrue(any(row['filter'].endswith('::conditional') for row in rows))
            self.assertTrue(any(row['filter'].endswith('::spaced') for row in rows))
            document['schema'] = 2
            self.assertTrue(validate_inventory(root, document))

    def test_actual_cli_refuses_expired_source_metadata_without_touching_rust(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            path, document = self.fixture(root)
            original = path.read_bytes()
            today = datetime.now(timezone.utc).date()
            document['exclude'][0].update(reviewed=str(today-timedelta(days=2)),
                                          expires=str(today-timedelta(days=1)))
            ci = root / '.config/ci'
            ci.mkdir(parents=True)
            for name in ('quarantine.py', 'source_ignores.py'):
                shutil.copyfile(Path(__file__).with_name(name), ci / name)
            (ci / 'source-ignores.json').write_text(json.dumps(document))
            platform = dict(document['exclude'][0], reviewed=str(today), expires=str(today+timedelta(days=30)))
            fields = ('filter', 'category', 'reason', 'owner', 'review', 'reviewed', 'expires')
            (ci / 'emulated-windows-exclusions.toml').write_text('[[exclude]]\n' +
                '\n'.join(key + ' = ' + json.dumps(platform[key]) for key in fields))
            result = subprocess.run([sys.executable, str(ci / 'quarantine.py'), 'windows'],
                                    capture_output=True, text=True, timeout=30)
            self.assertEqual(result.returncode, 1, result.stderr)
            self.assertIn('quarantine source:', result.stderr)
            self.assertIn('expired', result.stderr)
            self.assertEqual(path.read_bytes(), original)

    def test_tracked_rust_symlinks_are_not_followed(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.fixture(root)
            (root / 'src/link.rs').symlink_to(root / 'src/lib.rs')
            subprocess.run(['git', '-C', str(root), 'add', 'src/link.rs'], check=True)
            with self.assertRaisesRegex(ValueError, 'symlink'):
                discover(root)


if __name__ == '__main__':
    unittest.main()
