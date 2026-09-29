import sys
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import run_common_lane as common


class CommonQualityUndefinedMetricTests(unittest.TestCase):
    def test_empty_hits_and_all_zero_qrels_keep_nulls_without_crashing(self):
        data = {
            "qrels": {
                "zero-hit-positive": {"pkg:one": 3},
                "all-zero-empty": {"pkg:two": 0},
                "all-zero-hit": {"pkg:three": 0},
            }
        }
        queries = [
            {"query_id": "zero-hit-positive"},
            {"query_id": "all-zero-empty"},
            {"query_id": "all-zero-hit"},
        ]
        rankings = {
            "zero-hit-positive": [],
            "all-zero-empty": [],
            "all-zero-hit": ["pkg:three"],
        }

        report = common.common_quality(data, queries, rankings)

        self.assertIsNone(report["per_query"]["zero-hit-positive"]["precision@1"])
        self.assertIsNone(report["per_query"]["all-zero-empty"]["ndcg@1"])
        self.assertIsNone(report["per_query"]["all-zero-empty"]["recall@1"])
        self.assertEqual(report["per_query"]["all-zero-hit"]["precision@1"], 0.0)
        self.assertEqual(report["macro_defined_query_count"]["precision@1"], 1)
        self.assertEqual(report["macro_defined_query_count"]["recall@1"], 1)
        self.assertEqual(report["macro_defined_query_count"]["ndcg@1"], 1)

    def test_shared_candidate_filter_removes_unindexed_qrels_and_hits(self):
        data = {"qrels": {"q": {"pkg:indexed": 3, "pkg:older": 1}}}
        report = common.common_quality(
            data,
            [{"query_id": "q"}],
            {"q": ["pkg:older", "pkg:indexed"]},
            candidate_ids={"pkg:indexed"},
        )

        self.assertEqual(report["candidate_document_count"], 1)
        self.assertEqual(report["qrel_row_count"], 1)
        self.assertEqual(report["positive_qrel_row_count"], 1)
        self.assertEqual(report["per_query"]["q"]["result_count"], 1)
        self.assertEqual(report["per_query"]["q"]["mrr"], 1.0)


if __name__ == "__main__":
    unittest.main()
