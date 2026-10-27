"""Exercise release recovery validation without contacting GitHub."""

import copy
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

SCRIPT = Path(__file__).resolve().parents[2] / "scripts/release/resume-release-bins.py"
SPEC = importlib.util.spec_from_file_location("resume_release_bins", SCRIPT)
RESUME = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(RESUME)
REPO = "syscoin/zksync-os-server"
TAG = "v1.2.3"
SHA = "a" * 40
RUN_ID = "1234"


def signed_predicate(tag=TAG, sha=SHA, fixture="true"):
    return [{"verificationResult": {"statement": {"predicate": {"buildDefinition": {
        "buildType": "https://actions.github.io/buildtypes/workflow/v1",
        "internalParameters": {"syscoinCanonicalFixture": fixture},
        "resolvedDependencies": [{"uri": f"git+https://github.com/{REPO}@refs/tags/{tag}",
                                  "digest": {"gitCommit": sha}}],
    }}}}}]


class RunValidationTests(unittest.TestCase):
    def setUp(self):
        self.run = {"id": int(RUN_ID), "status": "completed", "head_branch": "main",
                    "repository": {"full_name": REPO}, "head_repository": {"full_name": REPO},
                    "path": ".github/workflows/release-please.yml", "event": "push"}
        self.jobs = [{"jobs": [{"name": "release-bins / release-bins", "status": "completed",
                                "conclusion": "success"}]}]

    def gh(self, *args):
        return json.dumps(self.jobs if "--paginate" in args else self.run)

    def test_accepts_completed_main_release_with_successful_packaging(self):
        with patch.object(RESUME, "gh", side_effect=self.gh):
            RESUME.validate_run(REPO, RUN_ID)
            self.run["path"] = ".github/workflows/release-bins.yml"
            self.run["event"] = "workflow_dispatch"
            self.jobs[0]["jobs"][0]["name"] = "release-bins"
            RESUME.validate_run(REPO, RUN_ID)

    def test_rejects_wrong_branch_workflow_or_repository(self):
        for field, value in (("head_branch", "feature"), ("path", ".github/workflows/test.yml"),
                             ("status", "in_progress"), ("id", 5678),
                             ("repository", {"full_name": "other/repo"}),
                             ("head_repository", {"full_name": "other/repo"})):
            with self.subTest(field=field):
                old = self.run[field]
                self.run[field] = value
                with patch.object(RESUME, "gh", side_effect=self.gh):
                    with self.assertRaisesRegex(ValueError, "trusted main-branch"):
                        RESUME.validate_run(REPO, RUN_ID)
                self.run[field] = old

    def test_rejects_missing_or_failed_packaging_job(self):
        for jobs in ([], [{"name": "release-bins", "status": "completed", "conclusion": "failure"}],
                     [{"name": "release-bins", "status": "in_progress", "conclusion": None}]):
            with self.subTest(jobs=jobs):
                self.jobs = [{"jobs": jobs}]
                with patch.object(RESUME, "gh", side_effect=self.gh):
                    with self.assertRaisesRegex(ValueError, "packaging job did not succeed"):
                        RESUME.validate_run(REPO, RUN_ID)


class AttestationAndBundleTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        (self.root / "dist").mkdir()
        self.metadata = {"tag": TAG, "source_sha": SHA, "fixture_ready": "true", "run_id": RUN_ID}
        (self.root / "metadata.json").write_text(json.dumps(self.metadata))
        self.url = f"https://github.com/{REPO}/attestations/123"
        (self.root / "attestation-url.txt").write_text(self.url + "\n")
        for name in RESUME.expected_files(TAG, "true"):
            (self.root / "dist" / name).write_bytes(b"original archive")
        self.verified = []

    def gh(self, *args):
        self.assertEqual(args[:2], ("attestation", "verify"))
        self.assertIn("--deny-self-hosted-runners", args)
        self.assertEqual(args[args.index("--source-ref") + 1], "refs/heads/main")
        self.assertEqual(args[args.index("--signer-workflow") + 1],
                         f"{REPO}/.github/workflows/release-bins.yml")
        self.verified.append(Path(args[2]).name)
        return json.dumps(signed_predicate())

    def verify(self):
        with patch.object(RESUME, "gh", side_effect=self.gh):
            return RESUME.verify_bundle(self.root, REPO, TAG, SHA, RUN_ID)

    def test_valid_complete_bundle_verifies_each_archive(self):
        files, fixture, url = self.verify()
        self.assertEqual({p.name for p in files}, RESUME.expected_files(TAG, "true"))
        self.assertEqual(set(self.verified), RESUME.expected_files(TAG, "true"))
        self.assertEqual(len(self.verified), len(files))
        self.assertEqual((fixture, url), ("true", self.url))

    def test_attestation_requires_exact_source_tag_and_fixture(self):
        RESUME.validate_attestation(signed_predicate(), REPO, TAG, SHA, "true")
        for result in (signed_predicate(tag="v-other"), signed_predicate(sha="b" * 40),
                       signed_predicate(fixture="false"), [], [{}]):
            with self.subTest(result=result):
                with self.assertRaisesRegex(ValueError, "does not bind"):
                    RESUME.validate_attestation(result, REPO, TAG, SHA, "true")

    def test_rejects_incomplete_and_unexpected_bundle_before_attestation(self):
        path = self.root / "dist" / next(iter(RESUME.expected_files(TAG, "true")))
        path.unlink()
        with self.assertRaisesRegex(ValueError, "incomplete or unexpected"):
            self.verify()
        self.assertEqual(self.verified, [])
        path.write_bytes(b"restored")
        (self.root / "dist" / "extra.tar.gz").write_bytes(b"unexpected")
        with self.assertRaisesRegex(ValueError, "incomplete or unexpected"):
            self.verify()
        self.assertEqual(self.verified, [])

    def test_rejects_tampered_metadata_or_url_before_attestation(self):
        for key, wrong in (("tag", "v-other"), ("source_sha", "b" * 40),
                           ("run_id", "5678"), ("fixture_ready", "invalid")):
            with self.subTest(key=key):
                metadata = copy.copy(self.metadata)
                metadata[key] = wrong
                (self.root / "metadata.json").write_text(json.dumps(metadata))
                with self.assertRaises(ValueError):
                    self.verify()
                self.assertEqual(self.verified, [])
        (self.root / "metadata.json").write_text(json.dumps(self.metadata))
        (self.root / "attestation-url.txt").write_text("https://example.com/attestations/123")
        with self.assertRaisesRegex(ValueError, "invalid original attestation URL"):
            self.verify()
        self.assertEqual(self.verified, [])

    def test_invalid_signed_predicate_blocks_recovery(self):
        with patch.object(RESUME, "gh", return_value=json.dumps(signed_predicate(sha="b" * 40))):
            with self.assertRaisesRegex(ValueError, "does not bind"):
                RESUME.verify_bundle(self.root, REPO, TAG, SHA, RUN_ID)


if __name__ == "__main__":
    unittest.main()
