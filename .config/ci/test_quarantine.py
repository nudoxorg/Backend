#!/usr/bin/env python3
"""Check that exclusions cannot silently outlive their review or lose ownership."""
from datetime import date
import unittest
from quarantine import validate


class QuarantineTest(unittest.TestCase):
    def entry(self):
        return dict(filter='binary_id(foo) and test(=bar)', category='emulation-timing',
                    reason='Native passes; emulated timeout: build 123', owner='Backend maintainers',
                    review='docs/testing/quarantine.md', reviewed='2026-10-14', expires='2026-11-13')

    def test_valid_until_expiry_then_fails_without_changing_selection(self):
        entry = self.entry()
        self.assertEqual(validate({'exclude': [entry]}, date(2026, 11, 12)), [])
        self.assertTrue(any('expired' in e for e in validate({'exclude': [entry]}, date(2026, 11, 13))))
        self.assertEqual(entry['filter'], self.entry()['filter'])

    def test_every_ownership_and_evidence_field_is_required(self):
        for field in ('filter', 'category', 'reason', 'owner', 'review'):
            with self.subTest(field=field):
                entry = self.entry()
                entry[field] = ' '
                self.assertTrue(validate({'exclude': [entry]}, date(2026, 10, 14)))

    def test_dates_cannot_be_missing_invalid_future_or_indefinite(self):
        for field, value in [('reviewed', None), ('expires', 'bad'), ('reviewed', '2026-10-15'),
                             ('expires', '2027-10-14'), ('expires', '2026-10-14')]:
            with self.subTest(field=field, value=value):
                entry = self.entry()
                entry[field] = value
                self.assertTrue(validate({'exclude': [entry]}, date(2026, 10, 14)))

    def test_empty_malformed_or_duplicate_catalog_is_rejected(self):
        for document in ({}, {'exclude': []}, {'exclude': [None]},
                         {'exclude': [self.entry(), self.entry()]}):
            with self.subTest(document=document):
                self.assertTrue(validate(document, date(2026, 10, 14)))


if __name__ == '__main__':
    unittest.main()
