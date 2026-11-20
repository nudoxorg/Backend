"""Controls for the Vue public journey's lexical readiness gate."""
import unittest
from unittest.mock import patch

import report35_vue_packages_public as vue


class Clock:
    def __init__(self):
        self.now = 0.0

    def monotonic(self):
        return self.now

    def sleep(self, seconds):
        self.now += seconds


class Journey:
    def __init__(self, replies):
        self.replies = iter(replies)
        self.last = None
        self.labels = []

    def cli(self, label, args, *, deadline):
        self.labels.append(label)
        self.last = next(self.replies, self.last)
        return 0, self.last, repr(self.last)


def require(label, condition, detail=''):
    if not condition:
        raise AssertionError(f'{label}: {detail}')


def search(**extra):
    return {'answer': 'records', 'query': 'mounted', 'records': [], 'more': False, **extra}


class ReadinessTest(unittest.TestCase):
    def test_names_fallback_is_recorded_but_never_accepted_as_full(self):
        clock = Clock()
        fallback = search(search_scope='published_names_only', full_search='preparing')
        full = search()
        journey = Journey([fallback, fallback, full])
        observations = []
        with patch.object(vue.time, 'monotonic', clock.monotonic), patch.object(vue.time, 'sleep', clock.sleep):
            actual = vue.wait_full_search(journey, 'warm', 'mounted', observations, require)
        self.assertEqual(actual, full)
        self.assertEqual([row['full_lexical'] for row in observations], [False, False, True])
        self.assertEqual(len(set(journey.labels)), 3)
        self.assertGreater(observations[-1]['elapsed_seconds'], 0)

    def test_never_ready_fails_at_original_bound(self):
        clock = Clock()
        fallback = search(search_scope='published_names_only', full_search='preparing')
        observations = []
        with patch.object(vue.time, 'monotonic', clock.monotonic), patch.object(vue.time, 'sleep', clock.sleep):
            with self.assertRaises(TimeoutError):
                vue.wait_full_search(Journey([fallback]), 'cold', 'mounted', observations, require)
        self.assertEqual(clock.now, 600)
        self.assertTrue(observations)
        self.assertTrue(all(not row['full_lexical'] for row in observations))

    def test_fault_or_wrong_query_cannot_become_preparation(self):
        clock = Clock()
        with patch.object(vue.time, 'monotonic', clock.monotonic), patch.object(vue.time, 'sleep', clock.sleep):
            for reply in ({'answer': 'fault'}, search(query='wrong')):
                with self.subTest(reply=reply), self.assertRaises(AssertionError):
                    vue.wait_full_search(Journey([reply]), 'edit', 'mounted', [], require)


if __name__ == '__main__':
    unittest.main()
