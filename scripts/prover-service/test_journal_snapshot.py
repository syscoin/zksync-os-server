from pathlib import Path
import sqlite3
import tempfile
import unittest
from unittest.mock import patch

import journal
import service as s


class JournalSnapshotTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.path = Path(self.temporary.name) / "state.json"
        self.state = journal.State({"schema_version": 1, "nonce": 1,
            "operations": {"one": {"lane": "child", "assignment": {"batch_number": 1}, "status": "offered"}},
            "audit": {"events": [{"kind": "old"}]}})
        journal.save(self.path, self.state, list_fields=("audit.events",))

    def migrate(self):
        with patch.object(journal, "MAX_ROWS", 1):
            journal.save(self.path, self.state, list_fields=("audit.events",))

    def test_inline_does_not_create_database(self):
        with journal.snapshot(self.path, list_fields=("audit.events",)) as state:
            self.assertEqual(state["operations"]["one"]["status"], "offered")
        self.assertFalse(self.path.with_name(self.path.name + ".sqlite3").exists())

    def test_snapshot_is_consistent_across_concurrent_write(self):
        self.migrate()
        database = self.path.with_name(self.path.name + ".sqlite3")
        with journal.snapshot(self.path, list_fields=("audit.events",)) as state:
            self.assertEqual(state["nonce"], 1)
            self.state["nonce"] = 2
            self.state["operations"]["one"]["status"] = "accepted"
            self.state.database.execute("PRAGMA busy_timeout=0")
            with self.assertRaises(sqlite3.OperationalError):
                journal.save(self.path, self.state, list_fields=("audit.events",))
            self.assertEqual(journal.peek(state["operations"], "one")["status"], "offered")
            self.assertEqual(len(list(journal.range_items(state["operations"], "child", 1, 1))), 1)
            self.assertEqual(list(state["audit"]["events"]), [{"kind": "old"}])
            with self.assertRaises(TypeError):
                state["operations"]["two"] = {}
            with self.assertRaises(TypeError):
                state["audit"]["events"].append({})
        journal.save(self.path, self.state, list_fields=("audit.events",))
        with journal.snapshot(self.path) as state:
            self.assertEqual(state["nonce"], 2)
            self.assertEqual(journal.peek(state["operations"], "one")["status"], "accepted")
        self.state.database.close()
        self.assertTrue(database.exists())

    def test_migration_marker_validated_and_reads_do_not_modify_files(self):
        with patch.object(journal, "MAX_ROWS", 1), patch.object(journal, "_atomic", side_effect=OSError("interrupted")):
            with self.assertRaises(OSError):
                journal.save(self.path, self.state, list_fields=("audit.events",))
        self.state.database.close()
        database = self.path.with_name(self.path.name + ".sqlite3")
        before = {path: path.read_bytes() for path in (self.path, database)}
        with journal.snapshot(self.path, map_fields=("operations", "optional")) as state:
            self.assertEqual(journal.peek(state["operations"], "one")["status"], "offered")
            self.assertEqual(state.get("optional", {}), {})
        self.assertEqual({path: path.read_bytes() for path in before}, before)
        changed = s.read_json(self.path, private=True)
        changed["nonce"] = 99
        journal._atomic(self.path, changed)
        with self.assertRaisesRegex(s.Error, "journal_migration_source_changed"):
            with journal.snapshot(self.path):
                pass

    def test_missing_and_unsafe_database_fail_closed(self):
        self.migrate()
        self.state.database.close()
        database = self.path.with_name(self.path.name + ".sqlite3")
        database.chmod(0o644)
        with self.assertRaisesRegex(s.Error, "private_journal_database_required"):
            with journal.snapshot(self.path):
                pass
        database.unlink()
        with self.assertRaisesRegex(s.Error, "journal_database_missing"):
            with journal.snapshot(self.path):
                pass
        self.assertFalse(database.exists())


if __name__ == "__main__":
    unittest.main()
