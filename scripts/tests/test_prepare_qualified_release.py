"""Exercise private staging and SHA-bound release publication with no GitHub writes."""

import importlib.util
from pathlib import Path
import unittest
from unittest.mock import Mock, patch


ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location(
    "prepare_qualified_release", ROOT / "scripts/release/prepare-qualified-release.py",
)
RELEASE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(RELEASE)
SOURCE = "a" * 40
MAIN = "b" * 40
TAG = "v0.24.5"
DRAFT_ID = "17"
DRAFT = {"id": 17, "draft": True, "target_commitish": SOURCE, "tag_name": TAG}
PR = {"number": 390, "merge_commit_sha": SOURCE, "merged_at": "2026-11-21T21:00:00Z",
      "base": {"ref": "main", "repo": {"full_name": RELEASE.REPO}},
      "head": {"ref": RELEASE.RELEASE_BRANCH, "repo": {"full_name": RELEASE.REPO}}}
REF = {"ref": f"refs/tags/{TAG}", "object": {"type": "commit", "sha": SOURCE}}
PENDING = "autorelease: pending"
TAGGED = "autorelease: tagged"
LABELS_PATH = f"issues/{PR['number']}/labels"
PENDING_PATH = f"{LABELS_PATH}/autorelease%3A%20pending"


class ReleaseApi:
    def __init__(self, *, draft=None, ref=None, main=MAIN):
        self.draft = draft
        self.ref = ref
        self.main = main
        self.writes = []
        self.labels = {PENDING, "keep-me"}

    def __call__(self, method, path, data=None, *, missing=False):
        if method == "GET":
            if path == f"releases/tags/{TAG}":
                return self.draft if self.draft and not self.draft["draft"] else None
            pr = {**PR, "labels": [{"name": label} for label in sorted(self.labels)]}
            if path.startswith(f"commits/{SOURCE}/pulls"):
                return [pr]
            if path == f"pulls/{PR['number']}":
                return pr
            if path == "releases?per_page=100&page=1":
                return [self.draft] if self.draft else []
            if path == "git/ref/heads/main":
                return {"object": {"sha": self.main}}
            if path == f"git/ref/tags/{TAG}":
                if self.ref is None and not missing:
                    raise ValueError("missing tag")
                return self.ref
            if path == f"releases/{DRAFT_ID}":
                return self.draft
        else:
            self.writes.append((method, path, data))
            if (method, path) == ("POST", "releases"):
                self.draft = {**DRAFT, **data}
                return self.draft
            if (method, path) == ("POST", "git/refs"):
                if self.ref is not None:
                    raise ValueError("tag was created concurrently")
                self.ref = {"ref": data["ref"], "object": {"type": "commit", "sha": data["sha"]}}
                return self.ref
            if (method, path) == ("PATCH", f"releases/{DRAFT_ID}"):
                self.draft = {**self.draft, **data}
                return self.draft
            if (method, path) == ("POST", LABELS_PATH):
                self.labels.update(data["labels"])
                return [{"name": label} for label in sorted(self.labels)]
            if (method, path) == ("DELETE", PENDING_PATH):
                self.labels.discard(PENDING)
                return [{"name": label} for label in sorted(self.labels)]
        raise AssertionError(f"unexpected request: {method} {path}")


class ReleasePreparationTests(unittest.TestCase):
    def stage(self, api, checked=SOURCE):
        with patch.object(RELEASE, "git", side_effect=[checked, SOURCE]), patch.object(
            RELEASE, "manifest_tag", return_value=TAG
        ), patch.object(RELEASE, "release_notes", return_value="Test notes"), patch("builtins.print"):
            return RELEASE.stage(api, checked)

    def test_auto_stages_private_draft_without_git_ref_or_public_release(self):
        api = ReleaseApi()
        self.assertEqual(self.stage(api), {"release_id": DRAFT_ID, "tag": TAG, "source_sha": SOURCE})
        self.assertEqual(api.writes, [("POST", "releases", {
            "tag_name": TAG, "name": TAG, "target_commitish": SOURCE, "body": "Test notes",
            "draft": True, "prerelease": False,
        })])
        self.assertIsNone(api.ref)

    def test_later_main_recovers_missing_draft_but_does_not_attempt_publication(self):
        api = ReleaseApi()
        self.assertEqual(self.stage(api, MAIN), {})
        self.assertEqual(api.draft["target_commitish"], SOURCE)
        self.assertTrue(api.draft["draft"])
        self.assertEqual(self.stage(api, MAIN), {})
        self.assertEqual(len(api.writes), 1)

    def test_published_previous_version_allows_next_release_pr(self):
        api = ReleaseApi(draft={**DRAFT, "draft": False})
        self.assertEqual(self.stage(api, MAIN), {"prepare_pr": "true"})
        self.assertEqual(self.stage(api, SOURCE), {"prepare_pr": "false"})
        self.assertEqual(api.writes, [])

    def test_different_draft_or_existing_public_ref_blocks_replacement(self):
        for api in (ReleaseApi(draft={**DRAFT, "target_commitish": MAIN}), ReleaseApi(ref=REF)):
            with self.subTest(api=api), self.assertRaises(ValueError):
                self.stage(api)
            self.assertEqual(api.writes, [])

    def test_manual_selection_requires_exact_id_tag_and_source(self):
        api = ReleaseApi(draft=DRAFT)
        self.assertEqual(RELEASE.selected(api, DRAFT_ID, TAG, SOURCE), DRAFT)
        for wrong in ((DRAFT_ID, TAG, "main"), (DRAFT_ID, TAG, MAIN),
                      ("", TAG, SOURCE), (DRAFT_ID, "v0.24.6", SOURCE)):
            with self.subTest(wrong=wrong), self.assertRaises(ValueError):
                RELEASE.selected(api, *wrong)
        self.assertEqual(api.writes, [])


class ReleasePublicationTests(unittest.TestCase):
    def promote(self, api, checked=SOURCE):
        with patch.object(RELEASE, "git", side_effect=[checked, ""]), patch.object(
            RELEASE, "manifest_tag", return_value=TAG
        ):
            return RELEASE.promote(api, DRAFT_ID, TAG, SOURCE)

    def test_main_advance_cannot_change_the_qualified_release_tag(self):
        api = ReleaseApi(draft=DRAFT, main=MAIN)
        self.assertEqual(self.promote(api), {"tag": TAG, "source_sha": SOURCE})
        self.assertEqual(api.writes, [
            ("POST", "git/refs", {"ref": f"refs/tags/{TAG}", "sha": SOURCE}),
            ("PATCH", f"releases/{DRAFT_ID}", {"draft": False, "tag_name": TAG, "target_commitish": SOURCE}),
            ("POST", LABELS_PATH, {"labels": [TAGGED]}),
            ("DELETE", PENDING_PATH, None),
        ])
        self.assertEqual(api.ref, REF)
        self.assertEqual(api.labels, {TAGGED, "keep-me"})

    def test_existing_wrong_ref_or_different_checkout_blocks_mutation(self):
        wrong_ref = {**REF, "object": {"type": "commit", "sha": MAIN}}
        for api, checked in ((ReleaseApi(draft=DRAFT, ref=wrong_ref), SOURCE),
                             (ReleaseApi(draft=DRAFT), MAIN)):
            with self.subTest(api=api), self.assertRaises(ValueError):
                self.promote(api, checked)
            self.assertEqual(api.writes, [])

    def test_existing_exact_ref_allows_retry_and_republishing_is_a_noop(self):
        api = ReleaseApi(draft=DRAFT, ref=REF)
        self.promote(api)
        self.assertEqual([method for method, _, _ in api.writes], ["PATCH", "POST", "DELETE"])
        self.promote(api)
        self.assertEqual(len(api.writes), 3)

    def test_publication_retry_finishes_incomplete_label_transition(self):
        api = ReleaseApi(draft=DRAFT, ref=REF)

        def fail_removal(method, path, data=None, **kwargs):
            if (method, path) == ("DELETE", PENDING_PATH):
                raise ValueError("label update unavailable")
            return api(method, path, data, **kwargs)

        with self.assertRaisesRegex(ValueError, "label update unavailable"):
            self.promote(Mock(side_effect=fail_removal))
        self.assertFalse(api.draft["draft"])
        self.assertEqual(api.labels, {PENDING, TAGGED, "keep-me"})

        api.writes.clear()
        self.promote(api)
        self.assertEqual(api.writes, [("DELETE", PENDING_PATH, None)])
        self.assertEqual(api.labels, {TAGGED, "keep-me"})

    def test_pending_label_removal_requires_successful_readback(self):
        api = ReleaseApi(draft={**DRAFT, "draft": False}, ref=REF)
        api.labels.add(TAGGED)

        def empty_removal(method, path, data=None, **kwargs):
            if (method, path) == ("DELETE", PENDING_PATH):
                return None
            return api(method, path, data, **kwargs)

        with self.assertRaisesRegex(ValueError, "did not transition"):
            self.promote(Mock(side_effect=empty_removal))

        def concurrently_removed(method, path, data=None, **kwargs):
            if (method, path) == ("DELETE", PENDING_PATH):
                api.labels.remove(PENDING)
                return None
            return api(method, path, data, **kwargs)

        self.assertEqual(self.promote(Mock(side_effect=concurrently_removed)),
                         {"tag": TAG, "source_sha": SOURCE})
        self.assertEqual(api.labels, {TAGGED, "keep-me"})

    def test_conflicting_ref_created_concurrently_never_reaches_publish(self):
        api = ReleaseApi(draft=DRAFT)

        def concurrent(method, path, data=None, **kwargs):
            if method == "POST":
                api.ref = {**REF, "object": {"type": "commit", "sha": MAIN}}
            return api(method, path, data, **kwargs)

        with self.assertRaisesRegex(ValueError, "concurrently"):
            self.promote(Mock(side_effect=concurrent))
        self.assertTrue(api.draft["draft"])
        self.assertEqual([method for method, _, _ in api.writes], ["POST"])


if __name__ == "__main__":
    unittest.main()
