"""Check safe release asset recovery without contacting GitHub."""

import importlib.util
import gzip
import io
import json
from pathlib import Path
import tarfile
from unittest import TestCase, main
from unittest.mock import patch
import subprocess
import tempfile

SCRIPT = Path(__file__).resolve().parents[2] / "scripts/release/publish-release-assets.py"
SPEC = importlib.util.spec_from_file_location("publish_assets", SCRIPT)
PUBLISH = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(PUBLISH)


class PublishReleaseAssetsTests(TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.paths = [self.root / "fri-v1.tar.gz", self.root / "snark-v1.tar.gz"]
        for path in self.paths:
            path.write_bytes(path.name.encode())
        self.remote = {}
        self.uploaded = []

    def run_gh(self, args, **kwargs):
        if args[1:3] == ["release", "view"]:
            return subprocess.CompletedProcess(args, 0, stdout=json.dumps({
                "assets": [{"name": name} for name in self.remote]
            }))
        if args[1:3] == ["release", "download"]:
            name = args[args.index("--pattern") + 1]
            destination = Path(args[args.index("--dir") + 1]) / name
            destination.write_bytes(self.remote[name])
            return subprocess.CompletedProcess(args, 0)
        if args[1:3] == ["release", "upload"]:
            path = Path(args[-1])
            if path.name in self.remote:
                raise subprocess.CalledProcessError(1, args)
            self.remote[path.name] = path.read_bytes()
            self.uploaded.append(path.name)
            return subprocess.CompletedProcess(args, 0)
        self.fail("unexpected command: " + repr(args))

    def publish(self):
        with patch.object(PUBLISH.subprocess, "run", side_effect=self.run_gh):
            PUBLISH.publish("v1", self.paths)

    def test_publication_tooling_is_bound_and_clean_before_use(self):
        workflow = (SCRIPT.parents[2] / ".github/workflows/release-bins.yml").read_text()
        step = workflow.split("      - name: Upload immutable release artifacts\n", 1)[1].split("      - name: Update release\n", 1)[0]
        checks = (
            'TOOLING_SHA: ${{ job.workflow_sha }}',
            '[[ "${TOOLING_SHA}" =~ ^[0-9a-f]{40}$ ]]',
            'git -C .release-publication-tooling rev-parse HEAD',
            'git -C .release-publication-tooling status --porcelain --untracked-files=all',
        )
        execute = step.index("publish-release-assets.py")
        for check in checks:
            self.assertIn(check, step)
            self.assertLess(step.index(check), execute)

    def test_fresh_publish_uploads_every_file(self):
        self.publish()
        self.assertEqual(self.uploaded, [p.name for p in self.paths])

    def test_partial_publish_resumes_only_missing_files(self):
        self.remote[self.paths[0].name] = self.paths[0].read_bytes()
        self.publish()
        self.assertEqual(self.uploaded, [self.paths[1].name])

    def test_retry_after_interrupted_upload(self):
        def interrupted(args, **kwargs):
            if args[1:3] == ["release", "upload"] and Path(args[-1]) == self.paths[1]:
                raise subprocess.CalledProcessError(1, args)
            return self.run_gh(args, **kwargs)
        with patch.object(PUBLISH.subprocess, "run", side_effect=interrupted):
            with self.assertRaises(subprocess.CalledProcessError):
                PUBLISH.publish("v1", self.paths)
        self.assertEqual(self.uploaded, [self.paths[0].name])
        self.uploaded.clear()
        self.publish()
        self.assertEqual(self.uploaded, [self.paths[1].name])

    def test_retry_uses_staged_original_archives_after_rebuild(self):
        def make_archive(path, mtime):
            payload = b"same built binary"
            with path.open("wb") as stream, gzip.GzipFile(fileobj=stream, mode="wb", mtime=0) as compressed:
                with tarfile.open(fileobj=compressed, mode="w") as archive:
                    member = tarfile.TarInfo("server")
                    member.size = len(payload)
                    member.mtime = mtime
                    archive.addfile(member, io.BytesIO(payload))

        staged = self.root / "staged"
        rebuilt = self.root / "rebuilt"
        staged.mkdir()
        rebuilt.mkdir()
        original = [staged / path.name for path in self.paths]
        replacement = [rebuilt / path.name for path in self.paths]
        for path in original:
            make_archive(path, 1700000000)
        for path in replacement:
            make_archive(path, 1700000100)
        self.assertNotEqual(original[0].read_bytes(), replacement[0].read_bytes())

        def interrupted(args, **kwargs):
            if args[1:3] == ["release", "upload"] and Path(args[-1]) == original[1]:
                raise subprocess.CalledProcessError(1, args)
            return self.run_gh(args, **kwargs)

        with patch.object(PUBLISH.subprocess, "run", side_effect=interrupted):
            with self.assertRaises(subprocess.CalledProcessError):
                PUBLISH.publish("v1", original)
        self.assertEqual(self.remote, {original[0].name: original[0].read_bytes()})
        self.uploaded.clear()
        with patch.object(PUBLISH.subprocess, "run", side_effect=self.run_gh):
            with self.assertRaisesRegex(ValueError, "existing release asset differs"):
                PUBLISH.publish("v1", replacement)
            self.assertEqual(self.uploaded, [])
            PUBLISH.publish("v1", original)
        self.assertEqual(self.uploaded, [original[1].name])
        self.assertEqual(self.remote, {path.name: path.read_bytes() for path in original})

    def test_divergent_existing_asset_aborts_before_any_upload(self):
        self.remote[self.paths[1].name] = b"different"
        with self.assertRaisesRegex(ValueError, "existing release asset differs"):
            self.publish()
        self.assertEqual(self.uploaded, [])
        self.assertEqual(self.remote[self.paths[1].name], b"different")

    def test_identical_rerun_does_not_upload(self):
        self.remote = {p.name: p.read_bytes() for p in self.paths}
        self.publish()
        self.assertEqual(self.uploaded, [])

    def test_invalid_tag_or_glob_name_rejected(self):
        with self.assertRaisesRegex(ValueError, "invalid release tag"):
            PUBLISH.publish("bad/tag", self.paths)
        bad = self.root / "*.tar.gz"
        bad.write_bytes(b"test")
        with self.assertRaisesRegex(ValueError, "invalid release asset name"):
            PUBLISH.publish("v1", [bad])


if __name__ == "__main__":
    main()
