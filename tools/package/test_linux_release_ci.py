"""Planning, waiver and tag controls for the automated linux-x64 release."""
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

import linux_release_ci as ci


def source_repo(root: Path, version: str = "0.3.0", waiver: dict | None = None) -> str:
    subprocess.run(["git", "-C", str(root), "init", "-q"], check=True)
    (root / "Cargo.toml").write_text(f'[workspace.package]\nversion = "{version}"\n')
    if waiver is not None:
        path = root / ci.WAIVER_SOURCE
        path.parent.mkdir(parents=True)
        path.write_text(json.dumps(waiver))
    subprocess.run(["git", "-C", str(root), "add", "."], check=True)
    subprocess.run(["git", "-C", str(root), "-c", "user.email=t@t.t", "-c", "user.name=t", "commit", "-q", "-m", "init"], check=True)
    return ci.git(root, "rev-parse", "HEAD")


def release(tag: str, source_sha: str | None = None) -> dict:
    body = f"Canonical Forgejo Backend source: {source_sha}." if source_sha else "binaries"
    return {"tag_name": tag, "body": body}


WAIVER = {"version": "0.3.0", "contexts": ["concourse/backend-linux"], "reason": "T3 linux red on vendored gpui tests",
          "approved_by": "Robert", "approved_at": "2026-11-10T03:00:00Z"}


class PlanTests(unittest.TestCase):
    def plan(self, releases, version="0.3.0", waiver=None):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            revision = source_repo(root, version, waiver)
            with patch.object(ci, "github_releases", return_value=releases(revision)), \
                    patch.object(ci, "check_fast") as check_fast:
                result = ci.plan(root, root / "plan.json")
            self.assertEqual(json.loads((root / "plan.json").read_text()), result)
            return result, check_fast, revision

    def test_first_candidate_of_a_new_version(self):
        result, check_fast, revision = self.plan(lambda sha: [release("v0.2.0", "a" * 40)])
        self.assertEqual((result["rc"], result["tag"], result["skip"]), (1, "v0.3.0-rc.1-linux-x64", None))
        check_fast.assert_called_once_with(revision, "linux-x64", None)

    def test_next_candidate_number_counts_only_this_version_and_platform(self):
        result, _, _ = self.plan(lambda sha: [
            release("v0.3.0-rc.2-linux-x64", "a" * 40), release("v0.3.0-rc.7-macos", "b" * 40),
            release("v0.2.0-rc.9-linux-x64", "c" * 40),
        ])
        self.assertEqual(result["tag"], "v0.3.0-rc.3-linux-x64")

    def test_released_version_and_repeated_source_are_skipped_without_ci_checks(self):
        for releases in (lambda sha: [release("v0.3.0", "a" * 40)],
                         lambda sha: [release("v0.3.0-rc.1-linux-x64", sha)]):
            result, check_fast, _ = self.plan(releases)
            self.assertIsNotNone(result["skip"])
            self.assertIsNone(result["tag"])
            check_fast.assert_not_called()

    def test_committed_waiver_is_bound_to_the_released_revision(self):
        result, check_fast, revision = self.plan(lambda sha: [], waiver=WAIVER)
        waiver = check_fast.call_args.args[2]
        self.assertEqual(waiver["source_sha"], revision)
        self.assertEqual(waiver["contexts"], ["concourse/backend-linux"])
        self.assertEqual(result["rc"], 1)

    def test_a_waiver_for_another_version_is_ignored(self):
        _, check_fast, _ = self.plan(lambda sha: [], version="0.3.1", waiver=WAIVER)
        self.assertIsNone(check_fast.call_args.args[2])

    def test_a_waiver_the_contract_would_reject_stops_before_the_build(self):
        bad = {**WAIVER, "contexts": ["concourse/backend-windows-wine"]}
        with self.assertRaisesRegex(ValueError, "CI waiver contexts"):
            self.plan(lambda sha: [], waiver=bad)

    def test_rehearsal_plans_candidate_zero_without_github_or_ci(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source_repo(root, "0.2.0")
            with patch.object(ci, "github_releases") as releases, patch.object(ci, "check_fast") as check_fast:
                result = ci.plan(root, root / "plan.json", rehearse=True)
            releases.assert_not_called()
            check_fast.assert_not_called()
            self.assertEqual((result["tag"], result["rehearse"]), ("v0.2.0-rc.0-linux-x64", True))


class TagTests(unittest.TestCase):
    def test_refuses_malformed_tags_and_revisions_before_any_request(self):
        with patch.dict(ci.os.environ, {"FORGEJO_TOKEN": "t"}), patch.object(ci.urllib.request, "urlopen") as urlopen:
            for tag, revision in (("v0.3.0-rc.0-linux-x64", "a" * 40), ("latest", "a" * 40),
                                  ("v0.3.0; rm", "a" * 40), ("v0.3.0", "HEAD")):
                with self.subTest(tag=tag), self.assertRaises(ValueError):
                    ci.create_tag(tag, revision)
            urlopen.assert_not_called()

    def test_an_existing_tag_at_another_revision_is_refused(self):
        class Response:
            def __enter__(self):
                return self

            def __exit__(self, *args):
                return False

            def read(self, *args):
                return json.dumps({"commit": {"sha": "b" * 40}}).encode()

        with patch.dict(ci.os.environ, {"FORGEJO_TOKEN": "t"}), patch.object(ci.urllib.request, "urlopen", return_value=Response()):
            with self.assertRaisesRegex(ValueError, "another revision"):
                ci.create_tag("v0.3.0", "a" * 40)


if __name__ == "__main__":
    unittest.main()
