import copy
from pathlib import Path
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
        link, calls = io.os.link, []
        def interrupted(source, destination, **kwargs):
            calls.append(destination)
            if len(calls) == 2:
                raise KeyboardInterrupt
            return link(source, destination, **kwargs)
        with patch.object(io.os, "link", interrupted), self.assertRaises(KeyboardInterrupt):
            io.publish_work(self.inbox, self.envelope, self.payload, event_root=self.source)
        root = self.inbox / s.keccak(s.canonical(self.envelope["body"]))[2:]
        self.assertFalse((root / "work.json").exists())
        self.assertFalse(any(path.name.startswith(".audit-copy-") for path in root.iterdir()))
        io.publish_work(self.inbox, self.envelope, self.payload, event_root=self.source)
        self.assertEqual(list(audit.iter_events(self.bundle, root / "audit-events")), self.events)

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


if __name__ == "__main__":
    unittest.main()
