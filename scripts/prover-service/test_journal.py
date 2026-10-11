import copy
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import journal
import service as s


class JournalTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.path = Path(self.temporary.name) / "state.json"
        self.state = journal.State({"schema_version": 1, "nonce": 0, "operations": {},
                                    "audit": {"events": [], "head": "genesis"}})
        journal.save(self.path, self.state, list_fields=("audit.events",))

    def save(self):
        journal.save(self.path, self.state, list_fields=("audit.events",))

    def reload(self):
        self.state = journal.load(self.path, list_fields=("audit.events",))

    def test_interrupted_marker_publication_recovers_committed_rows_and_counters(self):
        self.state["operations"]["pending"] = {"status": "submission_pending", "lease": "original"}
        self.state["nonce"] = 7
        self.state["audit"]["events"].append({"event": "retained"})
        with patch.object(journal, "MAX_ROWS", 1), patch.object(journal, "_atomic", side_effect=OSError("interrupted")):
            with self.assertRaises(OSError):
                self.save()
        self.reload()
        self.assertEqual(self.state["nonce"], 7)
        self.assertEqual(self.state["operations"]["pending"]["lease"], "original")
        self.assertEqual(list(self.state["audit"]["events"]), [{"event": "retained"}])
        self.state["nonce"] = 8
        self.save()
        self.reload()
        self.assertEqual(self.state["nonce"], 8)

    def test_database_transaction_rolls_back_history_and_scheduling_together(self):
        self.state["operations"]["pending"] = {"status": "offered", "lease": "original"}
        with patch.object(journal, "MAX_ROWS", 1):
            self.save()
        self.state["operations"]["pending"]["status"] = "accepted"
        self.state["nonce"] = 1
        self.state["audit"]["events"].append({"event": "accepted"})
        with patch.object(journal.SQLiteEvents, "flush", side_effect=OSError("interrupted")):
            with self.assertRaises(OSError):
                self.save()
        prior = journal.load(self.path, list_fields=("audit.events",))
        self.assertEqual(prior["operations"]["pending"]["status"], "offered")
        self.assertEqual(prior["nonce"], 0)
        self.assertEqual(len(prior["audit"]["events"]), 0)
        self.save()
        self.reload()
        self.assertEqual(self.state["operations"]["pending"]["status"], "accepted")
        self.assertEqual(self.state["nonce"], 1)
        self.assertEqual(len(self.state["audit"]["events"]), 1)

    def test_mutable_live_row_survives_migration_and_later_save(self):
        row = {"status": "submission_pending", "lease": "original"}
        self.state["operations"]["pending"] = row
        with patch.object(journal, "MAX_ROWS", 1):
            self.save()
        self.assertEqual(s.read_json(self.path, private=True)["schema_version"], 2)
        row["status"] = "accepted"
        self.save()
        self.reload()
        self.assertEqual(self.state["operations"]["pending"], row)

    def test_legacy_migration_does_not_invent_an_authenticated_audit(self):
        self.state.pop("audit")
        self.state["operations"]["pending"] = {"status": "submission_pending", "lease": "original"}
        with patch.object(journal, "MAX_ROWS", 1):
            journal.save(self.path, self.state)
        restored = journal.load(self.path, list_fields=("audit.events",))
        self.assertNotIn("audit", restored)
        self.assertEqual(restored["operations"]["pending"]["lease"], "original")

    def test_completed_history_does_not_grow_working_metadata_or_read_cache(self):
        with patch.object(journal, "MAX_ROWS", 2):
            for number in range(2100):
                self.state["operations"][str(number)] = {"status": "accepted", "account": "worker",
                    "lane": "child", "assignment": {"batch_number": number + 1}}
                self.save()
        self.reload()
        self.assertEqual(len(self.state["operations"]), 2100)
        self.assertLess(self.path.stat().st_size, 1024)
        self.assertEqual([key for key, _ in journal.range_items(self.state["operations"], "child", 2099, 2100)],
                         ["2098", "2099"])
        for number in range(2100):
            self.assertEqual(journal.peek(self.state["operations"], str(number))["status"], "accepted")
        self.assertEqual(len(self.state["operations"].cache), 0)

    def test_uncertain_wallet_row_stays_owned_across_history_cache_eviction(self):
        row = {"status": "signing_uncertain", "nonce": 7}
        self.state["operations"]["wallet"] = row
        with patch.object(journal, "MAX_ROWS", 1):
            self.save()
        for number in range(journal.MAX_CACHE + 5):
            self.state["operations"][str(number)] = {"status": "accepted"}
            self.save()
        row.update(status="send_uncertain", raw="original-signed-bytes")
        self.save()
        self.reload()
        self.assertEqual(self.state["operations"]["wallet"], row)


if __name__ == "__main__":
    unittest.main()
