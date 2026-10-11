import copy
import unittest

import coordinator as c
import dispatcher as d
import keeper as k
import service as s
import workflow_io as io
import test_coordinator as tc
import test_coordinator_cross_epoch as ce
import test_dispatcher as td
from test_service import h, sign


class BootstrapControlTests(unittest.TestCase):
    setUpClass = classmethod(tc.CoordinatorTests.setUpClass.__func__)
    reload = tc.CoordinatorTests.reload
    evidence = tc.CoordinatorTests.evidence
    pick = tc.CoordinatorTests.pick
    offered = tc.CoordinatorTests.offered
    signed_proof = tc.CoordinatorTests.signed_proof
    accepted = tc.CoordinatorTests.accepted
    controller_reload = tc.CoordinatorTests.controller_reload

    def ready(self, account=None, key="operator", expires=None):
        request = self.dispatcher.request(account or self.account, expires or self.dispatcher.now + 40)
        self.dispatcher.ready(request, sign(request, key))
        return request

    def setUp(self):
        tc.CoordinatorTests.setUp(self)
        self.registry_rpc = ce.EpochRegistryRpc(self.f["settings"], self.f["subscriptions"])
        self.registry_rpc.active = False
        self.registry_rpc.finalize(440, 100)
        self.root = self.base / "launch-bootstrap-journal"
        self.store = d.initialize(self.root, self.f["settings"], self.f["subscriptions"], 5,
                                 "https://trusted.example/", self.registry_rpc)
        self.network = td.Network()
        self.reload(now=450)
        self.accepted(1)
        self.accepted(2)
        self.config = copy.deepcopy(self.config)
        self.config["dispatcher_dir"] = str(self.root)
        self.control_store = c.initialize(self.base / "launch-control", self.config)
        self.now = self.rpc.now = 450
        self.rpc.head["timestamp"] = hex(self.now)
        self.rpc.blocks[self.rpc.head["number"]] = copy.deepcopy(self.rpc.head)
        self.rpc.deadline = self.now + 200
        self.rpc.control = True
        self.controller_reload()
        self.original_journal = (self.root / "state.json").read_bytes()

    def native_range(self):
        self.native.responses += [(200, {}, {**self.f["fri_payload"], "lease_token": h(180)}),
                                  (200, {}, self.f["evidence"])]

    def assert_opened(self, result, bundle):
        self.assertEqual(result["status"], "broadcast")
        store = c.transactions.Store(self.control_store.root / "transactions")
        row = store.load(self.f["settings"], self.config["transaction_policy"])["operations"][result["operation_id"]]
        self.assertEqual(row["call"]["action"], "openPackage")
        request = bundle["request"]
        self.assertEqual(request["duties"], [])
        prepared = self.controller.prepare(request, self.f["evidence"], self.f["fri_payload"])
        self.assertEqual(prepared["accepted_package"]["reportHash"], s.report_hash([]))
        self.assertEqual(prepared["accepted_package"]["period"], 5)
        self.assertEqual(row["call"]["transaction"]["data"], k.package_call(self.config["keeper"], self.rpc,
            request, self.f["evidence"], self.f["fri_payload"], self.now, "open",
            enrollment=self.controller.enrollment_for(request))["transaction"]["data"])
        self.assertEqual((self.root / "state.json").read_bytes(), self.original_journal)
        self.assertFalse((self.root / "periods").exists())
        self.assertEqual(len(self.rpc.sent), 1)

    def test_honest_prestart_activation_control_range_acquires_snapshots_and_opens(self):
        chain = self.controller.status()[0]
        self.assertTrue(chain["service_active"])
        self.assertTrue(chain["control_work"])
        self.assertFalse(self.registry_rpc.active)
        self.native_range()
        result = self.controller.step(True)
        identifier = self.controller.state["active"]
        bundle = self.controller.snapshot(identifier, *self.controller.status(), True)
        self.assert_opened(result, bundle)
        self.assertEqual(bundle["audit"]["identity"]["enrollment"]["phase"], "bootstrap")
        self.assertEqual(sum(event["kind"] == "accepted" for event in bundle["audit"]["events"]), 2)
        self.assertEqual(len(self.native.calls), 2)

    def test_bootstrap_journal_cannot_admit_ordinary_paid_service_range(self):
        self.rpc.control = False
        with self.assertRaisesRegex(s.Error, "enrollment_authority_anchor_mismatch"):
            self.controller.step(True)
        self.assertEqual((self.native.calls, self.wallet.calls, self.rpc.sent), ([], [], []))
        self.assertEqual((self.root / "state.json").read_bytes(), self.original_journal)
        self.assertFalse((self.root / "periods").exists())

    def test_bootstrap_control_cutoff_uses_canonical_head_when_local_clock_lags(self):
        chain = self.controller.status()[0]
        self.rpc.now = 505
        self.rpc.head["timestamp"] = hex(self.rpc.now)
        self.rpc.blocks[self.rpc.head["number"]] = copy.deepcopy(self.rpc.head)
        chain["head"] = copy.deepcopy(self.rpc.head)
        with self.assertRaisesRegex(s.Error, "enrollment_authority_anchor_mismatch"):
            with self.controller.dispatcher_for(chain):
                pass
        self.assertEqual((self.native.calls, self.wallet.calls, self.rpc.sent), ([], [], []))

    def test_recovered_aggregate_control_enrollment_uses_canonical_prestart_status(self):
        self.native_range()
        identifier = self.controller.reserve()
        self.assertIsNone(self.controller.acquire(identifier, True))
        bundle = self.controller.snapshot(identifier, *self.controller.status(), True)
        original = bundle["request"]
        journal_id = bundle["audit"]["journal_id"]
        contexts = [{"journal_id": journal_id, "manifest": original["manifest"],
                     "subscriptions": original["subscriptions"]}]
        payload = {"schema_version": 2, "chain_id": self.f["settings"]["execution_chain_id"],
            "chain_address": self.f["settings"]["chain_address"], "sequencer": self.f["settings"]["sequencer"],
            "period": 5, "current_journal_id": journal_id,
            "subscription_snapshot_hash": s.keccak(s.canonical(original["subscriptions"])), "contexts": contexts}
        request = {**original, "manifest": {"payload": payload,
                   "sequencer_signature": sign(s.manifest_request(self.f["settings"], payload), "sequencer")}}
        aggregate = {"request": request, "audit": {"schema_version": 3, "period": 5,
                     "current_journal_id": journal_id,
                     "contexts": [{"journal_id": journal_id, "request": original, "bundle": bundle["audit"]}]}}
        directory = self.controller.directory(identifier)
        io.immutable_json(directory / "aggregate-bundle.json", aggregate)
        op = self.controller.state["operations"][identifier]
        op.update(bundle=io.digest(aggregate), bundle_file="aggregate-bundle.json")
        self.controller.save()
        self.controller_reload()
        self.assertIsNone(self.controller.enrollment)
        self.assert_opened(self.controller.step(True), aggregate)
        self.assertEqual(type(self.controller.enrollment), s.EnrollmentGroup)
        self.assertFalse(self.controller.enrollment.rewardable(journal_id))


if __name__ == "__main__":
    unittest.main()
