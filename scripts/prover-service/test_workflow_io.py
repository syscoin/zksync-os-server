import copy
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

import audit
import service as s
import workflow_io as io


class AuditTransportTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name).resolve()
        self.source = self.root / "source"
        self.inbox = io.directory(self.root / "inbox", create=True)
        self.events = [{"sequence": index, "kind": "example", "payload": {"index": index}}
                       for index in range(5)]
        with patch.object(audit, "SEGMENT_EVENTS", 2):
            descriptor = audit.export_events(self.events, self.source)
        self.bundle = {"schema_version": 2, "events": descriptor}
        self.envelope = {"body": {"audit": self.bundle}, "sequencer_signature": "0x"}
        self.payload = {"test": "payload"}

    def test_segmented_work_is_durable_before_envelope_and_can_be_copied_again(self):
        root = io.publish_work(self.inbox, self.envelope, self.payload, event_root=self.source)
        self.assertEqual(list(audit.iter_events(self.bundle, root / "audit-events")), self.events)
        self.assertEqual(io.private_json(root / "work.json"), self.envelope)
        self.assertEqual({path.name for path in (root / "audit-events").iterdir()},
                         {f"{index:016d}.jsonl" for index in range(3)})
        self.assertTrue(all(path.stat().st_nlink == 1 for path in (root / "audit-events").iterdir()))
        self.assertEqual(io.publish_work(self.inbox, self.envelope, self.payload, event_root=self.source), root)
        self.assertEqual(list(audit.iter_events(self.bundle, root / "audit-events")), self.events)

    def test_interrupted_copy_has_no_discoverable_envelope_and_retries_exactly(self):
        replace, calls = io.os.replace, []
        def interrupted(source, destination, **kwargs):
            if Path(destination).suffix == ".jsonl":
                calls.append(destination)
                if len(calls) == 2:
                    raise KeyboardInterrupt
            return replace(source, destination, **kwargs)
        with patch.object(io.os, "replace", interrupted), self.assertRaises(KeyboardInterrupt):
            io.publish_work(self.inbox, self.envelope, self.payload, event_root=self.source)
        root = self.inbox / s.keccak(s.canonical(self.envelope["body"]))[2:]
        self.assertFalse((root / "work.json").exists())
        self.assertFalse(any(path.name.startswith(".audit-copy-") for path in root.iterdir()))
        io.publish_work(self.inbox, self.envelope, self.payload, event_root=self.source)
        self.assertEqual(list(audit.iter_events(self.bundle, root / "audit-events")), self.events)

    def test_abrupt_exit_after_segment_rename_recovers_without_hardlinks(self):
        child = subprocess.run([sys.executable, "-c", """
import json, os, sys
from pathlib import Path
sys.path.insert(0, sys.argv[1])
import workflow_io as io
replace = io.os.replace
def crash_after_segment(source, destination):
    replace(source, destination)
    if Path(destination).suffix == ".jsonl":
        os._exit(97)
io.os.replace = crash_after_segment
io.publish_work(Path(sys.argv[2]), json.loads(sys.argv[4]), json.loads(sys.argv[5]),
                event_root=Path(sys.argv[3]))
""", str(Path(io.__file__).parent), str(self.inbox), str(self.source),
            s.canonical(self.envelope).decode(), s.canonical(self.payload).decode()], capture_output=True)
        self.assertEqual(child.returncode, 97, child.stderr.decode())
        root = self.inbox / s.keccak(s.canonical(self.envelope["body"]))[2:]
        self.assertFalse((root / "work.json").exists())
        self.assertEqual((root / "audit-events" / "0000000000000000.jsonl").stat().st_nlink, 1)
        io.publish_work(self.inbox, self.envelope, self.payload, event_root=self.source)
        self.assertEqual(list(audit.iter_events(self.bundle, root / "audit-events")), self.events)
        self.assertEqual(io.private_json(root / "work.json"), self.envelope)

    def test_existing_segment_mismatch_is_never_replaced(self):
        root = io.publish_work(self.inbox, self.envelope, self.payload, event_root=self.source)
        segment = root / "audit-events" / "0000000000000000.jsonl"
        segment.write_bytes(b"changed")
        with self.assertRaisesRegex(s.Error, "audit_event_segment_changed"):
            io.publish_work(self.inbox, self.envelope, self.payload, event_root=self.source)
        self.assertEqual(segment.read_bytes(), b"changed")

    def test_missing_truncated_or_additional_segment_never_publishes_work(self):
        segment = self.source / "0000000000000000.jsonl"
        segment.write_bytes(segment.read_bytes()[:-1])
        with self.assertRaises(s.Error):
            io.publish_work(self.inbox, self.envelope, self.payload, event_root=self.source)
        self.assertFalse(any(self.inbox.rglob("work.json")))
        segment.write_bytes(b"".join(s.canonical(event) + b"\n" for event in self.events[:2]))
        io.immutable_bytes(self.source / "unexpected", b"extra")
        with self.assertRaises(s.Error):
            io.publish_work(self.inbox, self.envelope, self.payload, event_root=self.source)
        self.assertFalse(any(self.inbox.rglob("work.json")))

    def test_symlink_or_hardlinked_segment_never_publishes_work(self):
        segment = self.source / "0000000000000000.jsonl"
        original = self.root / "original.jsonl"
        segment.rename(original)
        segment.symlink_to(original)
        with self.assertRaises(s.Error):
            io.publish_work(self.inbox, self.envelope, self.payload, event_root=self.source)
        segment.unlink()
        io.os.link(original, segment)
        with self.assertRaises(s.Error):
            io.publish_work(self.inbox, self.envelope, self.payload, event_root=self.source)
        self.assertFalse(any(self.inbox.rglob("work.json")))

    def test_descriptor_cannot_select_a_path_and_archive_is_required(self):
        envelope = copy.deepcopy(self.envelope)
        envelope["body"]["audit"]["events"]["path"] = "../outside"
        with self.assertRaises(s.Error):
            io.publish_work(self.inbox, envelope, self.payload, event_root=self.source)
        with self.assertRaisesRegex(s.Error, "audit_event_archive_required"):
            io.publish_work(self.inbox, self.envelope, self.payload)

    def test_inline_work_does_not_require_or_create_an_archive(self):
        envelope = {"body": {"audit": {"schema_version": 1, "events": []}}, "sequencer_signature": "0x"}
        root = io.publish_work(self.inbox, envelope, self.payload)
        self.assertFalse((root / "audit-events").exists())
        self.assertEqual(io.private_json(root / "work.json"), envelope)

    def test_aggregate_work_copies_each_stream_under_its_journal_id(self):
        keys = ["0x" + bytes([index]).hex() * 32 for index in (1, 2)]
        source = io.directory(self.root / "aggregate-source", create=True)
        contexts = []
        for key in keys:
            with patch.object(audit, "SEGMENT_EVENTS", 2):
                descriptor = audit.export_events(self.events, source / key[2:])
            contexts.append({"journal_id": key, "request": {},
                             "bundle": {"schema_version": 2, "journal_id": key, "events": descriptor}})
        bundle = {"schema_version": 3, "period": 6, "current_journal_id": keys[1], "contexts": contexts}
        envelope = {"body": {"audit": bundle}, "sequencer_signature": "0x"}
        root = io.publish_work(self.inbox, envelope, self.payload, event_root=source)
        for context in contexts:
            self.assertEqual(list(audit.iter_events(context["bundle"], root / "audit-events" / context["journal_id"][2:])),
                             self.events)
        self.assertEqual(io.private_json(root / "work.json"), envelope)

    def test_aggregate_archive_rejects_traversal_duplicate_or_nested_context(self):
        key = "0x" + "01" * 32
        original = {"schema_version": 1, "journal_id": key, "events": []}
        bundle = {"schema_version": 3, "period": 6, "current_journal_id": key,
                  "contexts": [{"journal_id": key, "request": {}, "bundle": original}]}
        for bad in ("path", "duplicate", "nested"):
            candidate = copy.deepcopy(bundle)
            if bad == "path":
                candidate["contexts"][0]["journal_id"] = "../outside"
            elif bad == "duplicate":
                candidate["contexts"].append(copy.deepcopy(candidate["contexts"][0]))
            else:
                candidate["contexts"][0]["bundle"]["schema_version"] = 3
            with self.subTest(bad=bad), self.assertRaises(s.Error):
                io.publish_work(self.inbox, {"body": {"audit": candidate}, "sequencer_signature": "0x"}, self.payload)
        self.assertFalse(any(self.inbox.rglob("work.json")))


if __name__ == "__main__":
    unittest.main()
