import copy
import unittest
from unittest.mock import patch

import audit
import dispatcher as d
import journal
import workflow_io as io
import test_coordinator as tc
import test_coordinator_cross_epoch as ce
import test_dispatcher as td
from test_service import h, sign


class SnapshotGraceTests(unittest.TestCase):
    setUpClass = classmethod(ce.CrossEpochCoordinatorTests.setUpClass.__func__)
    reload = ce.CrossEpochCoordinatorTests.reload
    evidence = ce.CrossEpochCoordinatorTests.evidence
    pick = ce.CrossEpochCoordinatorTests.pick
    offered = ce.CrossEpochCoordinatorTests.offered
    signed_proof = ce.CrossEpochCoordinatorTests.signed_proof
    accepted = ce.CrossEpochCoordinatorTests.accepted
    controller_reload = ce.CrossEpochCoordinatorTests.controller_reload
    acquired = ce.CrossEpochCoordinatorTests.acquired
    handoff = ce.CrossEpochCoordinatorTests.handoff
    check_audit = ce.CrossEpochCoordinatorTests.check_audit

    def ready(self, account=None, key="operator", expires=None):
        request = self.dispatcher.request(account or self.account, expires or self.dispatcher.now + 40)
        self.dispatcher.ready(request, sign(request, key))
        return request

    def setUp(self):
        ce.CrossEpochCoordinatorTests.setUp(self)
        self.root = self.base / "snapshot-old-dispatcher"
        self.store = d.initialize(self.root, self.f["settings"], self.f["subscriptions"], 5,
                                 "https://trusted.example/", self.registry_rpc)
        self.network = td.Network()
        self.reload()
        self.accepted(1)
        self.handoff()
        self.network = td.Network()
        self.dispatcher = d.Dispatcher(self.current_store, self.network, "Basic host-only", now=self.now,
                                       registry_rpc=self.registry_rpc)
        self.accepted(2)
        self.identifier = self.acquired()
        self.directory = self.controller.directory(self.identifier)
        io.immutable_json(self.directory / "proof.json", self.f["snark"])
        self.controller.state["operations"][self.identifier]["proof"] = io.digest(self.f["snark"])
        self.controller.save()
        self.proof_bytes = (self.directory / "proof.json").read_bytes()
        self.leases = copy.deepcopy(self.controller.state["operations"][self.identifier]["leases"])
        self.native_calls = len(self.native.calls)

    def close_grace(self):
        self.registry_rpc.finalize(660, 102)
        self.now = self.rpc.now = 665
        self.rpc.head["timestamp"] = hex(self.now)
        self.rpc.blocks[self.rpc.head["number"]] = copy.deepcopy(self.rpc.head)

    def recover(self, original_snapshot):
        self.close_grace()
        controller = self.controller_reload()
        bundle = controller.snapshot(self.identifier, *controller.status(), True)
        request = bundle["request"]
        self.assertEqual([duty["period"] for duty in request["duties"]], [6])
        self.assertEqual([duty["batchNumber"] for duty in request["duties"]], [2])
        self.assertEqual(request["manifest"]["payload"], original_snapshot["manifest"])
        for retained, original in zip(bundle["audit"]["contexts"], original_snapshot["audit"]["contexts"]):
            self.assertEqual(retained["journal_id"], original["journal_id"])
            self.assertEqual(retained["request"]["manifest"], original["request"]["manifest"])
            self.assertEqual({key: value for key, value in retained["bundle"].items() if key != "sequencer_signature"},
                             {key: value for key, value in original["bundle"].items() if key != "sequencer_signature"})
        prepared = controller.prepare(request, self.f["evidence"], self.f["fri_payload"])
        self.assertEqual(prepared["duties"], request["duties"])
        checked = self.check_audit(bundle, self.identifier)
        self.assertEqual([duty["batchNumber"] for duty in checked["accepted_receipts"]], [1, 2])
        self.assertEqual(checked["required_duties"], request["duties"])
        self.assertEqual(checked["unrewarded_reasons"][1], "round_closed")
        self.assertEqual((self.directory / "proof.json").read_bytes(), self.proof_bytes)
        self.assertEqual(controller.state["operations"][self.identifier]["leases"], self.leases)
        self.assertEqual(len(self.native.calls), self.native_calls)
        return bundle

    def test_snapshot_json_restart_projects_expired_credit_before_preparation(self):
        with patch.object(self.controller, "prepare", side_effect=KeyboardInterrupt), self.assertRaises(KeyboardInterrupt):
            self.controller.snapshot(self.identifier, *self.controller.status(), True)
        snapshot_path = self.directory / "snapshot.json"
        original = snapshot_path.read_bytes()
        snapshot = io.private_json(snapshot_path)
        self.assertEqual([duty["period"] for duty in snapshot["duties"]], [5, 6])
        self.assertFalse((self.directory / "bundle.json").exists())
        self.assertIsNone(self.controller.state["operations"][self.identifier]["bundle"])
        self.recover(snapshot)
        self.assertEqual(snapshot_path.read_bytes(), original)

    def test_snapshot_intent_export_restart_projects_expired_credit_without_changing_archive(self):
        with patch.object(journal, "MAX_INLINE_EVENTS", 1):
            d.Dispatcher(self.store, now=self.now, registry_rpc=self.registry_rpc).save()
            self.dispatcher.save()
        export = audit.export_events

        def interrupted(source, destination, count=None):
            export(source, destination, count=2)
            raise KeyboardInterrupt

        with patch.object(audit, "SEGMENT_EVENTS", 2):
            with patch.object(audit, "export_events", interrupted), self.assertRaises(KeyboardInterrupt):
                self.controller.snapshot(self.identifier, *self.controller.status(), True)
            intent_path = self.directory / "snapshot-intent.json"
            original = intent_path.read_bytes()
            snapshot = io.private_json(intent_path)["snapshot"]
            self.assertEqual([duty["period"] for duty in snapshot["duties"]], [5, 6])
            self.assertFalse((self.directory / "snapshot.json").exists())
            self.assertFalse((self.directory / "bundle.json").exists())
            partial = {path: path.read_bytes() for path in (self.directory / "audit-events").rglob("*.jsonl")}
            bundle = self.recover(snapshot)
            for context in bundle["audit"]["contexts"]:
                archive = self.directory / "audit-events" / context["journal_id"][2:]
                self.assertEqual(len(list(audit.iter_events(context["bundle"], archive))),
                                 context["bundle"]["checkpoint"]["eventCount"])
        self.assertEqual(intent_path.read_bytes(), original)
        self.assertEqual(io.private_json(self.directory / "snapshot.json"), snapshot)
        self.assertTrue(partial)
        self.assertTrue(all(path.read_bytes() == data for path, data in partial.items()))

    def test_published_bundle_restart_preserves_original_and_selects_revised_bundle(self):
        with patch.object(self.controller, "save", side_effect=KeyboardInterrupt), self.assertRaises(KeyboardInterrupt):
            self.controller.snapshot(self.identifier, *self.controller.status(), True)
        original_path = self.directory / "bundle.json"
        original = original_path.read_bytes()
        snapshot = io.private_json(self.directory / "snapshot.json")
        self.assertEqual([duty["period"] for duty in io.private_json(original_path)["request"]["duties"]], [5, 6])
        bundle = self.recover(snapshot)
        self.assertEqual(original_path.read_bytes(), original)
        state = self.controller_reload().state["operations"][self.identifier]
        self.assertNotEqual(state["bundle_file"], "bundle.json")
        self.assertEqual(io.private_json(self.directory / state["bundle_file"]), bundle)
        self.assertEqual(state["bundle"], io.digest(bundle))


class SingleSnapshotGraceTests(unittest.TestCase):
    setUpClass = classmethod(tc.CoordinatorTests.setUpClass.__func__)
    setUp = tc.CoordinatorTests.setUp
    reload = tc.CoordinatorTests.reload
    ready = tc.CoordinatorTests.ready
    evidence = tc.CoordinatorTests.evidence
    pick = tc.CoordinatorTests.pick
    offered = tc.CoordinatorTests.offered
    signed_proof = tc.CoordinatorTests.signed_proof
    accepted = tc.CoordinatorTests.accepted
    controller_reload = tc.CoordinatorTests.controller_reload
    acquired = tc.CoordinatorTests.acquired

    def test_single_journal_snapshot_restart_preserves_receipts_after_current_round_closes(self):
        self.registry_rpc = ce.EpochRegistryRpc(self.f["settings"], self.f["subscriptions"])
        self.now = self.rpc.now = 555
        self.rpc.head["timestamp"] = hex(self.now)
        self.rpc.blocks[self.rpc.head["number"]] = copy.deepcopy(self.rpc.head)
        self.controller_reload()
        identifier = self.acquired()
        directory = self.controller.directory(identifier)
        with patch.object(self.controller, "prepare", side_effect=KeyboardInterrupt), self.assertRaises(KeyboardInterrupt):
            self.controller.snapshot(identifier, *self.controller.status(), True)
        original = (directory / "snapshot.json").read_bytes()
        snapshot = io.private_json(directory / "snapshot.json")
        self.assertEqual([duty["period"] for duty in snapshot["duties"]], [5, 5])
        self.registry_rpc.finalize(650, 101)
        self.now = self.rpc.now = 655
        self.rpc.head["timestamp"] = hex(self.now)
        self.rpc.blocks[self.rpc.head["number"]] = copy.deepcopy(self.rpc.head)
        controller = self.controller_reload()
        bundle = controller.snapshot(identifier, *controller.status(), True)
        request = bundle["request"]
        self.assertEqual(request["duties"], [])
        self.assertEqual(request["manifest"]["payload"], snapshot["manifest"])
        self.assertEqual(controller.prepare(request, self.f["evidence"], self.f["fri_payload"])["duties"], [])
        journal_id = bundle["audit"]["journal_id"]
        trust = {"schema_version": 1, "journal_id": journal_id, "enrollment_block_hash": h(100),
                 "minimum_checkpoint": {"event_count": 0, "event_head": journal_id, "assignment_cursor": journal_id},
                 "required_readiness": [], "required_duties": []}
        checked = audit.verify_package(self.f["settings"], self.f["evidence"], request["manifest"],
                    request["subscriptions"], request["duties"], bundle["audit"], trust, self.registry_rpc,
                    fri_payload=self.f["fri_payload"], enrollment=controller.enrollment_for(request))
        self.assertEqual([duty["batchNumber"] for duty in checked["accepted_receipts"]], [1, 2])
        self.assertEqual(checked["required_duties"], [])
        self.assertEqual(checked["zero_duty_reason"], "round_closed")
        self.assertEqual((directory / "snapshot.json").read_bytes(), original)


if __name__ == "__main__":
    unittest.main()
