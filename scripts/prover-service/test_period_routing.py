import copy
from contextlib import redirect_stdout
import io
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

import dispatcher as d
import journal
import period_routing as p
import service as s
import test_dispatcher as td
from test_service import fixture, h, sign


class PeriodRpc(td.Rpc):
    def __init__(self, settings, subscriptions):
        super().__init__(settings, subscriptions)
        self.blocks, self.actual_anchors = {}, []
        self.finalize(550, 100)
        self.advance_on_finalized_read = None

    def finalize(self, timestamp, byte):
        self.block = {"hash": h(byte), "timestamp": hex(timestamp)}
        self.blocks[h(byte)] = {"block": copy.deepcopy(self.block), "active": self.active}

    def call(self, method, params):
        if method == "eth_getBlockByNumber":
            assert params == ["finalized", False]
            block = copy.deepcopy(self.block)
            if self.advance_on_finalized_read:
                timestamp, byte = self.advance_on_finalized_read
                self.advance_on_finalized_read = None
                self.finalize(timestamp, byte)
            return block
        if method == "eth_getBlockByHash":
            return copy.deepcopy(self.blocks[params[0]]["block"])
        return super().call(method, params)

    def contract(self, address, signature, values, anchor):
        self.actual_anchors.append(anchor)
        active = self.active
        self.active = self.blocks[anchor]["active"]
        try:
            return super().contract(address, signature, values, h(100))
        finally:
            self.active = active


class PeriodRoutingTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.template = fixture()

    def setUp(self):
        self.f = copy.deepcopy(self.template)
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name) / "dispatcher"
        self.rpc = PeriodRpc(self.f["settings"], self.f["subscriptions"])
        self.network = td.Network()
        self.store = d.initialize(self.root, self.f["settings"], self.f["subscriptions"], 5,
                                  "https://trusted.example/", self.rpc)
        self.dispatcher = d.Dispatcher(self.store, self.network, "Basic host-only", now=550, registry_rpc=self.rpc)
        self.account = self.f["subscription"]["account"]

    ready = td.DispatcherTests.ready
    evidence = td.DispatcherTests.evidence
    pick = td.DispatcherTests.pick
    offered = td.DispatcherTests.offered
    signed_proof = td.DispatcherTests.signed_proof

    def state(self, store):
        return journal.load(store.root / "state.json", list_fields=("audit.events",))

    def test_finalized_period_handoff_preserves_pending_lease_and_starts_new_credit_round(self):
        operation = self.offered()
        proof, signature = self.signed_proof(operation)
        self.network.responses.append((503, {}, b""))
        with self.assertRaisesRegex(s.Error, "submission_uncertain"):
            self.dispatcher.submit(operation, proof, signature)
        authority = s.read_json(self.dispatcher.directory(operation) / "authority.json", private=True)
        self.rpc.finalize(620, 101)
        following = p.current_store(self.store, self.rpc, 625)
        next_state, old_state = self.state(following), self.state(self.store)
        self.assertEqual(following.root, self.root / "periods" / "6-service")
        self.assertEqual(next_state["enrollment"]["block_hash"], h(101))
        self.assertNotEqual(next_state["journal_id"], old_state["journal_id"])
        self.assertEqual(next_state["accounts"][self.account], {"nonce": 0, "offered": 0, "ready": None})
        self.assertEqual(old_state["operations"][operation]["status"], "submission_pending")
        self.assertEqual(s.read_json(self.root / operation / "authority.json", private=True), authority)
        self.assertEqual(next_state["subscriptions"][0]["signature"], "0x")
        self.dispatcher = d.Dispatcher(self.store, self.network, "Basic host-only", now=625, registry_rpc=self.rpc)
        self.network.responses += [(200, {}, {"from_batch_number": 1, "to_batch_number": 1,
            "vk_hash": proof["vk_hash"], "fri_proofs": [proof["proof"]]}), (200, {}, self.evidence())]
        self.assertEqual(self.dispatcher.submit(operation, proof, signature), "accepted")
        self.assertEqual(self.state(self.store)["operations"][operation]["status"], "accepted")

    def test_repeated_handoff_keeps_original_snapshot_and_readiness_identity(self):
        self.rpc.finalize(620, 101)
        first = p.current_store(self.store, self.rpc, 625)
        original = (first.root / "state.json").read_bytes()
        self.rpc.finalize(630, 102)
        second = p.current_store(self.store, self.rpc, 635)
        self.assertEqual(first.root, second.root)
        self.assertEqual((second.root / "state.json").read_bytes(), original)
        self.assertEqual(p.current_store(self.store, self.rpc, 635, requested_period=5).root, self.store.root)

    def test_finality_lag_and_unfinalized_requested_epoch_do_not_create_a_round(self):
        self.rpc.finalize(599, 101)
        for kwargs in ({}, {"requested_period": 6}):
            with self.subTest(kwargs=kwargs), self.assertRaisesRegex(s.Error, "period_boundary_not_finalized"):
                p.current_store(self.store, self.rpc, 605, **kwargs)
        self.assertFalse((self.root / "periods").exists())

    def test_transition_from_bootstrap_to_service_uses_distinct_same_period_journals(self):
        self.rpc.active = False
        self.rpc.finalize(480, 102)
        bootstrap = d.initialize(Path(self.tmp.name) / "bootstrap", self.f["settings"], self.f["subscriptions"], 5,
                                 "https://trusted.example/", self.rpc)
        old = self.state(bootstrap)
        self.rpc.active = True
        self.rpc.finalize(550, 103)
        service = p.current_store(bootstrap, self.rpc, 555)
        self.assertEqual(service.root, bootstrap.root / "periods" / "5-service")
        self.assertNotEqual(self.state(service)["journal_id"], old["journal_id"])
        resolved = p.current_store(bootstrap, self.rpc, 555, requested_period=5, requested_journal_id=old["journal_id"])
        self.assertEqual(resolved.root, bootstrap.root)

    def test_new_snapshot_reads_only_one_finalized_anchor(self):
        self.rpc.finalize(620, 101)
        self.rpc.actual_anchors.clear()
        self.rpc.advance_on_finalized_read = (640, 102)
        following = p.current_store(self.store, self.rpc, 645)
        self.assertEqual(self.state(following)["period"], 6)
        self.assertEqual(set(self.rpc.actual_anchors), {h(101)})

    def test_missing_enrollment_or_changed_registry_binding_fails_without_new_journal(self):
        self.rpc.finalize(620, 101)
        self.rpc.ineligible.add(self.account)
        with self.assertRaisesRegex(s.Error, "period_has_no_eligible_subscribers"):
            p.current_store(self.store, self.rpc, 625)
        self.rpc.ineligible.clear()
        self.rpc.fail = "subscription(bytes32)"
        with self.assertRaisesRegex(s.Error, "invalid_stored_subscription"):
            p.current_store(self.store, self.rpc, 625)
        self.assertFalse((self.root / "periods").exists())

    def args(self, command, **fields):
        return SimpleNamespace(command=command, state=str(self.root), fixed_epoch=False, **fields)

    def test_cli_work_and_pick_routes_share_current_finalized_period(self):
        self.rpc.finalize(620, 101)
        requested = d.routed_store(self.args("work-request"), self.store, self.rpc, 625)
        picked = d.routed_store(self.args("pick"), self.store, self.rpc, 625)
        self.assertEqual(requested.root, self.root / "periods" / "6-service")
        self.assertEqual(picked.root, requested.root)

    def test_cli_readiness_routes_exact_signed_journal_when_chain_moves_on(self):
        self.rpc.finalize(620, 101)
        following = p.current_store(self.store, self.rpc, 625)
        worker = d.Dispatcher(following, self.network, "Basic host-only", now=625, registry_rpc=self.rpc)
        request = worker.request(self.account, 690)
        path = Path(self.tmp.name) / "readiness.json"
        s.write_new(path, request)
        self.rpc.finalize(720, 102)
        selected = d.routed_store(self.args("ready", request=str(path)), self.store, self.rpc, 725)
        self.assertEqual(selected.root, following.root)
        self.assertFalse((self.root / "periods" / "7-service").exists())

    def test_cli_scoped_operation_routes_and_strips_only_valid_handle(self):
        self.rpc.finalize(620, 101)
        following = p.current_store(self.store, self.rpc, 625)
        args = self.args("submit", operation="periods/6-service/job-000007")
        self.assertEqual(d.routed_store(args, self.store, self.rpc, 625).root, following.root)
        self.assertEqual(args.operation, "job-000007")
        args = self.args("manifest-request", operations=["periods/6-service/job-000007", "periods/6-service/job-000008"])
        self.assertEqual(d.routed_store(args, self.store, self.rpc, 625).root, following.root)
        self.assertEqual(args.operations, ["job-000007", "job-000008"])

    def test_cli_operation_traversal_and_mixed_journal_lists_are_rejected(self):
        for handle in ("../job-000000", "periods/6-service/../job-000000", "/tmp/job-000000", "job-000000/extra"):
            with self.subTest(handle=handle), self.assertRaisesRegex(s.Error, "invalid_operation_handle"):
                d.routed_store(self.args("submit", operation=handle), self.store, self.rpc, 550)
        for handles in (["job-000000", "periods/6-service/job-000001"],
                        ["periods/6-service/job-000000", "periods/7-service/job-000001"]):
            with self.subTest(handles=handles), self.assertRaisesRegex(s.Error, "one_period_per_operation_command"):
                d.routed_store(self.args("manifest-request", operations=handles), self.store, self.rpc, 550)

    def test_cli_child_state_normalizes_root_but_bare_operations_stay_in_child(self):
        self.rpc.finalize(620, 101)
        following = p.current_store(self.store, self.rpc, 625)
        self.assertEqual(p.root_store(following).root, self.store.root)
        args = self.args("submit", operation="job-000007")
        args.state = str(following.root)
        self.assertEqual(d.routed_store(args, p.root_store(following), self.rpc, 625).root, following.root)
        self.assertEqual(args.operation, "job-000007")

    def test_cli_fixed_epoch_recovery_never_auto_initializes_a_later_round(self):
        self.rpc.finalize(620, 101)
        args = self.args("recover-pick")
        args.fixed_epoch = True
        self.assertEqual(d.routed_store(args, self.store, None, 625).root, self.store.root)
        self.assertFalse((self.root / "periods").exists())

    def test_cli_recovered_pick_keeps_its_new_period_scope(self):
        self.rpc.finalize(620, 101)
        following = p.current_store(self.store, self.rpc, 625)
        self.dispatcher = d.Dispatcher(following, self.network, "Basic host-only", now=625, registry_rpc=self.rpc)
        self.ready(expires=690)
        self.network.responses += [(200, {}, {"batch_number": 1, "vk_hash": self.f["settings"]["vk_hash"],
            "prover_input": "AQAAAA==", "lease_token": h(102)}), (503, {}, b"")]
        with self.assertRaisesRegex(s.Error, "pending_evidence_unavailable"):
            self.dispatcher.pick()
        self.network.responses.append((200, {}, self.evidence()))
        auth = Path(self.tmp.name) / "auth.txt"
        auth.write_text("test:token")
        auth.chmod(0o600)
        command = ["dispatcher.py", "--execute", "--state", str(self.root), "--registry-rpc",
                   "https://trusted.example/", "--auth-file", str(auth), "recover-pick"]
        output = io.StringIO()
        with patch("sys.argv", command), patch.object(d, "Rpc", return_value=self.rpc), \
                patch.object(d.job, "NativeNetwork", return_value=self.network), \
                patch.object(d.time, "time", return_value=625), redirect_stdout(output):
            d.main()
        self.assertEqual(output.getvalue().strip(), "periods/6-service/job-000000")
