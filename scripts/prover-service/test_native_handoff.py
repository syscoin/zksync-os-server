from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import native_handoff
import service as s


class PublicationDirectoryTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name).resolve()
        (self.root / ".lock").touch()

    def test_large_node_journal_is_discoverable(self):
        expected = [self.root / f"work-{index:04d}" for index in range(1025)]
        for directory in reversed(expected):
            directory.mkdir()
        self.assertEqual(native_handoff.publication_directories(self.root), expected)

    def test_only_the_node_lock_is_exempt_from_the_record_limit(self):
        work = self.root / "work"
        work.mkdir()
        (self.root / "symlink").symlink_to(work, target_is_directory=True)
        (self.root / ".other").mkdir()
        (self.root / "ordinary-file").touch()
        with patch.object(native_handoff, "MAX_PUBLICATION_RECORDS", 4):
            self.assertEqual(native_handoff.publication_directories(self.root), [work])
            (self.root / "new-record").mkdir()
            with self.assertRaisesRegex(s.Error, "^publication_scan_capacity$"):
                native_handoff.publication_directories(self.root)
