import copy
import json
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

import dispatcher
import service as s
from test_dispatcher import Network, Rpc
from test_service import a, fixture, h, sign


class DispatcherEnrollmentReuseTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.fixture = fixture()

    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name) / "dispatcher"
        self.settings = self.fixture["settings"]
        self.subscriptions = self.fixture["subscriptions"]
        self.rpc = Rpc(self.settings, self.subscriptions)
        self.rpc.gateway = dict(self.settings, execution_chain_id=self.settings["settlement_chain_id"],
                                settlement_chain_id="0x1", chain_address=a(26), vk_hash=h(27))
        dispatcher.initialize(self.root, self.settings, self.subscriptions, 5, "https://child.example/",
                              self.rpc, {"settings": self.rpc.gateway, "endpoint": "https://gateway.example/"})
        self.marker = self.root / "verified-enrollment.json"
        self.rpc.views.clear()

    def reopen(self, rpc=None):
        return dispatcher.Dispatcher(dispatcher.Store(self.root), now=550, registry_rpc=self.rpc if rpc is None else rpc)

    def test_ready_cli_reuses_initialized_two_lane_roster(self):
        request_file = self.root.parent / "request.json"
        signature_file = self.root.parent / "signature.json"
        account = self.subscriptions[0]["subscription"]["account"]
        cli = ["dispatcher", "--execute", "--state", str(self.root),
               "--registry-rpc", "https://registry.example/"]
        with patch.object(dispatcher, "Rpc", return_value=self.rpc), patch.object(dispatcher.time, "time", return_value=550):
            with patch.object(sys, "argv", cli + ["work-request", "--account", account, "--expires-at", "590",
                                                  "--output", str(request_file)]):
                dispatcher.main()
            s.write_new(signature_file, {"signature": sign(s.read_json(request_file), "operator")})
            with patch.object(sys, "argv", cli + ["ready", "--request", str(request_file),
                                                  "--signature", str(signature_file)]):
                dispatcher.main()
        state = s.read_json(self.root / "state.json", private=True)
        self.assertIsNotNone(state["accounts"][account]["ready"])
        self.assertEqual(set(state["lanes"]), {"child", "gateway"})
        self.assertNotIn("friSubscriberAt(address,uint64,uint256)", self.rpc.views)
        self.assertNotIn("subscription(bytes32)", self.rpc.views)
        self.assertIn("policyHash()", self.rpc.views)

    def test_wrong_chain_and_noncanonical_anchor_stop_warm_reuse(self):
        with patch.object(self.rpc, "call", return_value="0x1"), self.assertRaisesRegex(s.Error, "wrong_rpc_chain"):
            self.reopen()
        network = Network()
        network.responses.extend([
            (200, {}, {"id": 1, "result": self.settings["registry_chain_id"]}),
            (200, {}, {"id": 1, "error": {"code": -32000, "message": "block is not canonical"}}),
        ])
        with self.assertRaisesRegex(s.Error, "rpc_failed"):
            self.reopen(dispatcher.Rpc("https://registry.example/", network))
        request = json.loads(network.calls[-1][2])
        self.assertEqual(request["method"], "eth_call")
        self.assertEqual(request["params"], [
            {"to": self.settings["registry"], "data": s.cast("calldata", "policyHash()")},
            {"blockHash": h(100), "requireCanonical": True},
        ])

    def test_older_journal_is_reverified_once(self):
        self.marker.unlink()
        self.reopen()
        self.assertIn("friSubscriberAt(address,uint64,uint256)", self.rpc.views)
        self.assertIn("operatorAccountAt(address,uint64)", self.rpc.views)
        self.assertTrue(self.marker.is_file())
        self.rpc.views.clear()
        self.reopen()
        self.assertNotIn("friSubscriberAt(address,uint64,uint256)", self.rpc.views)
        self.assertNotIn("operatorAccountAt(address,uint64)", self.rpc.views)

    def test_changes_to_live_journal_identity_cannot_reuse_marker(self):
        path = self.root / "state.json"
        original = s.read_json(path, private=True)
        edits = [
            (("subscriptions", 0, "subscription", "beneficiary"), a(28)),
            (("lanes", "gateway", "settings", "chain_address"), a(29)),
            (("lanes", "child", "endpoint"), "https://changed.example/"),
            (("enrollment", "block_hash"), h(101)),
            (("enrollment", "ends_at"), 601),
            (("period",), 6),
        ]
        for keys, new_value in edits:
            with self.subTest(keys=keys):
                edited = copy.deepcopy(original)
                target = edited
                for key in keys[:-1]:
                    target = target[key]
                target[keys[-1]] = new_value
                dispatcher.atomic_json(path, edited)
                with self.assertRaises(s.Error):
                    self.reopen()

    def test_independent_enrollment_authority_still_checks_complete_roster(self):
        s.EnrollmentAuthority(self.settings, self.subscriptions, 5, self.rpc, h(100))
        self.assertIn("subscription(bytes32)", self.rpc.views)
        self.rpc.views.clear()
        self.rpc.extra_enrolled.append(a(99))
        with self.assertRaisesRegex(s.Error, "subscription_snapshot_omits_or_adds_eligible_accounts"):
            s.EnrollmentAuthority(self.settings, self.subscriptions, 5, self.rpc, h(100))
        self.assertIn("friSubscriberAt(address,uint64,uint256)", self.rpc.views)


if __name__ == "__main__":
    unittest.main()
