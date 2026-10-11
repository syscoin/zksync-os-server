import copy
import json
from pathlib import Path
import unittest
from unittest.mock import patch

import audit
import coordinator as c
import dispatcher as d
import journal
import keeper as k
import period_routing
import relay as r
import roster
import service as s
import transactions
import workflow_io as io
import test_coordinator as tc
import test_dispatcher as td
from test_period_routing import PeriodRpc
from test_service import KEYS, h, sign


class EpochRegistryRpc(PeriodRpc):
    def __init__(self, settings, subscriptions):
        self.historical_subscriptions = {}
        super().__init__(settings, subscriptions)

    def finalize(self, timestamp, byte):
        super().finalize(timestamp, byte)
        self.historical_subscriptions[h(byte)] = copy.deepcopy(self.subscriptions)

    def call(self, method, params):
        if method == "eth_getBlockByNumber" and params == ["latest", False]:
            return copy.deepcopy(self.block)
        return super().call(method, params)

    def contract(self, address, signature, values, anchor):
        if signature == "isRoundFinalized(uint256)":
            timestamp = s.uint(self.blocks[anchor]["block"]["timestamp"])
            return s.word(int(timestamp >= (values[0] + 1) * 100 + 50))
        original = self.subscriptions
        self.subscriptions = self.historical_subscriptions[anchor]
        try:
            return super().contract(address, signature, values, anchor)
        finally:
            self.subscriptions = original


class CrossEpochCoordinatorTests(unittest.TestCase):
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
    publication = tc.CoordinatorTests.publication

    def handoff(self, timestamp=620, unready_peer=False):
        self.registry_rpc = EpochRegistryRpc(self.f["settings"], self.f["subscriptions"])
        if unready_peer:
            peer = copy.deepcopy(self.f["subscription"])
            peer.update(account=s.cast("wallet", "address", "--private-key", KEYS["wrapper"]).lower(),
                        operator=self.account)
            self.registry_rpc.subscriptions = [*self.f["subscriptions"], {"subscription": peer, "signature": "0x"}]
        self.registry_rpc.finalize(timestamp, 101)
        self.now = timestamp + 5
        with self.store.lock("dispatcher.lock"):
            self.current_store = period_routing.current_store(self.store, self.registry_rpc, self.now)
        self.current_state = journal.load(self.current_store.root / "state.json", list_fields=("audit.events",))
        self.roster = roster.construct(6, [self.f["proposal"]["candidate"]])
        io.immutable_json(self.base / "rosters" / "6.json", self.roster)
        self.config = copy.deepcopy(self.config)
        self.config["dispatcher_dir"] = str(self.root.resolve())
        self.config["keeper"]["enrollment"]["block_hash"] = h(101)
        self.rpc.opening_period = 6
        self.rpc.item["sidecar"]["accepted_package"]["period"] = 6
        self.rpc.item["sidecar"]["accepted_package"]["rosterRoot"] = self.roster["root"]
        self.rpc.now = self.now
        self.rpc.head["timestamp"] = hex(self.now)
        self.rpc.blocks[self.rpc.head["number"]] = copy.deepcopy(self.rpc.head)
        self.rpc.deadline = self.now + 200
        self.control_store = c.initialize(self.base / "cross-period-control", self.config)
        self.controller_reload()

    def frozen(self):
        identifier = self.acquired()
        chain, artifact = self.controller.status()
        bundle = self.controller.snapshot(identifier, chain, artifact, True)
        self.rpc.item["sidecar"] = self.controller.prepare(bundle["request"], self.f["evidence"], self.f["fri_payload"])
        self.rpc.opened = True
        return identifier, bundle

    def check_audit(self, bundle, identifier):
        request = bundle["request"]
        trust = {}
        for context in bundle["audit"]["contexts"]:
            key, identity = context["journal_id"], context["bundle"]["identity"]
            trust[key] = {"schema_version": 1, "journal_id": key,
                "enrollment_block_hash": identity["enrollment"]["block_hash"],
                "minimum_checkpoint": {"event_count": 0, "event_head": key, "assignment_cursor": key},
                "required_readiness": [], "required_duties": []}
        return audit.verify_package(self.f["settings"], self.f["evidence"], request["manifest"],
                request["subscriptions"], request["duties"], bundle["audit"], trust, self.registry_rpc,
                fri_payload=self.f["fri_payload"], enrollment=self.controller.enrollment_for(request),
                event_root=self.controller.directory(identifier) / "audit-events")

    def returned(self, work):
        op = self.controller.state["operations"][self.controller.state["active"]]
        directory = Path(op["works"][work["work_hash"]]["directory"])
        request = io.private_json(directory / "work.json")["body"]["request"]
        prepared = s.prepare_package(self.f["settings"], self.f["evidence"], request["manifest"],
                request["subscriptions"], request["duties"], request["proposal"], self.f["snark"], self.f["fri_payload"],
                enrollment=self.controller.enrollment_for(request))
        result = {"schema_version": 1, "work_hash": work["work_hash"], "proof": self.f["snark"],
                  "prepared": prepared, "wrapper_signature": sign(prepared["wrapper_request"], "wrapper")}
        signed = {"result": result, "operator_signature": sign(io.result_request(self.f["settings"], work["work_hash"],
                    result, self.f["proposal"]["candidate"]["operator"]), "wrapper")}
        io.immutable_json(directory / "result.json", signed)
        return prepared

    def test_old_accepted_range_settles_with_current_roster_and_original_credit(self):
        self.handoff()
        self.assertEqual(self.current_state["accounts"][self.account]["offered"], 0)
        identifier, bundle = self.frozen()
        request = bundle["request"]
        self.assertEqual(request["manifest"]["payload"]["schema_version"], 2)
        self.assertEqual(request["proposal"]["accepted_package"]["period"], 6)
        self.assertEqual([duty["period"] for duty in request["duties"]], [5, 5])
        self.assertEqual([context["journal_id"] for context in request["manifest"]["payload"]["contexts"]],
                         sorted(context["journal_id"] for context in request["manifest"]["payload"]["contexts"]))
        checked = self.check_audit(bundle, identifier)
        self.assertEqual(checked["covered_batches"], [1, 2])
        self.assertEqual(checked["required_duties"], request["duties"])
        work = self.controller.step(True)
        prepared = self.returned(work)
        self.native.responses.append((204, {"x-syscoin-prover-disposition": "accepted"}, b""))
        self.assertEqual(self.controller_reload().step(True)["next_action"], "await_node_publication")
        self.assertEqual(json.loads(self.native.calls[-1][2])["lease_token"], h(180))
        publication = self.publication(prepared)
        self.rpc.item["digest"] = prepared["sequencer_request"]["digest"]
        staged = self.controller_reload().step(True)
        self.assertEqual(staged["next_action"], "relay_dual_endorsed_package")
        manager = r.Relay(self.f["settings"], self.config["relay_policy"], self.rpc,
                          store=r.Store(self.control_store.root / "relay"))
        self.rpc.item = manager.load_artifact(staged["operation_id"])
        self.assertEqual(self.controller_reload().step(True)["status"], "broadcast")
        self.rpc.accept_receipt()
        self.assertEqual(self.controller_reload().step(True)["status"], "mined_unconfirmed")
        self.rpc.advance(seconds=1)
        self.now = self.rpc.now
        self.assertTrue(self.controller_reload().step(True)["written"])
        self.assertTrue((publication / "relay.json").is_file())
        self.assertEqual(self.controller.state["operations"][identifier]["status"], "settled")

    def test_expired_old_credit_stays_in_coverage_with_zero_reward_report(self):
        self.handoff(timestamp=660)
        identifier, bundle = self.frozen()
        self.assertEqual(bundle["request"]["duties"], [])
        checked = self.check_audit(bundle, identifier)
        self.assertEqual(checked["covered_batches"], [1, 2])
        self.assertEqual(checked["zero_duty_reason"], "round_closed")
        self.assertEqual(checked["unrewarded_reasons"], {1: "round_closed", 2: "round_closed"})

    def test_unready_current_subscriber_does_not_block_fully_covered_old_range(self):
        self.handoff(unready_peer=True)
        self.assertEqual(len(self.current_state["accounts"]), 2)
        self.assertTrue(all(account["offered"] == 0 and account["ready"] is None
                            for account in self.current_state["accounts"].values()))
        identifier, bundle = self.frozen()
        self.assertEqual(self.check_audit(bundle, identifier)["covered_batches"], [1, 2])
        self.assertEqual(len(bundle["request"]["duties"]), 2)

    def test_pending_old_submission_recovers_original_receipt_after_handoff(self):
        self.root = self.base / "pending-old-dispatcher"
        self.store = d.initialize(self.root, self.f["settings"], self.f["subscriptions"], 5,
                                 "https://trusted.example/", self.registry_rpc)
        self.network = td.Network()
        self.reload()
        self.accepted(1)
        operation = self.offered(2)
        proof, signature = self.signed_proof(operation, 2)
        self.network.responses.append((503, {}, b""))
        with self.assertRaisesRegex(s.Error, "submission_uncertain"):
            self.dispatcher.submit(operation, proof, signature)
        source = self.dispatcher.directory(operation)
        original_submission = (source / "submission.json").read_bytes()
        original_authority = io.private_json(source / "authority.json")
        self.handoff()
        identifier = self.acquired()
        self.native.responses += [(200, {}, {"from_batch_number": 2, "to_batch_number": 2,
            "vk_hash": self.f["settings"]["vk_hash"], "fri_proofs": [proof["proof"]]}), (200, {}, self.evidence(2))]
        chain, artifact = self.controller.status()
        bundle = self.controller.snapshot(identifier, chain, artifact, True)
        self.assertEqual(self.check_audit(bundle, identifier)["covered_batches"], [1, 2])
        self.assertEqual((source / "submission.json").read_bytes(), original_submission)
        authority = io.private_json(source / "authority.json")
        self.assertEqual(authority["lease_token"], original_authority["lease_token"])
        self.assertEqual(authority["submission_sha256"], original_authority["submission_sha256"])
        self.assertEqual(authority["status"], "accepted")
        self.assertEqual([duty["period"] for duty in bundle["request"]["duties"]], [5, 5])

    def test_grace_closure_repairs_only_reward_projection_and_survives_restart(self):
        self.handoff()
        identifier, bundle = self.frozen()
        directory = self.controller.directory(identifier)
        original = (directory / "bundle.json").read_bytes()
        original_leases = copy.deepcopy(self.controller.state["operations"][identifier]["leases"])
        self.registry_rpc.finalize(660, 102)
        self.now = 665
        revised = self.controller.refresh_report(identifier, bundle, True)
        self.assertEqual(revised["request"]["duties"], [])
        self.assertEqual(revised["request"]["manifest"], bundle["request"]["manifest"])
        self.assertEqual([context["bundle"] for context in revised["audit"]["contexts"]],
                         [context["bundle"] for context in bundle["audit"]["contexts"]])
        self.assertEqual((directory / "bundle.json").read_bytes(), original)
        self.assertEqual(self.controller.state["operations"][identifier]["leases"], original_leases)
        self.controller_reload()
        chain, artifact = self.controller.status()
        retained = self.controller.snapshot(identifier, chain, artifact, True)
        self.assertEqual(retained, revised)
        self.assertEqual(self.check_audit(retained, identifier)["covered_batches"], [1, 2])

    def test_streamed_cross_period_snapshot_recovers_immutable_prefix_after_interruption(self):
        events = list(self.dispatcher.state["audit"]["events"])
        with patch.object(journal, "MAX_INLINE_EVENTS", 1):
            self.dispatcher.save()
        self.handoff()
        identifier = self.acquired()
        chain, artifact = self.controller.status()
        export = audit.export_events

        def interrupted(source, destination, count=None):
            export(source, destination, count=2)
            raise KeyboardInterrupt

        with patch.object(audit, "SEGMENT_EVENTS", 2):
            with patch.object(audit, "export_events", interrupted), self.assertRaises(KeyboardInterrupt):
                self.controller.snapshot(identifier, chain, artifact, True)
            directory = self.controller.directory(identifier)
            self.assertTrue((directory / "snapshot-intent.json").is_file())
            self.assertFalse((directory / "snapshot.json").exists())
            worker = d.Dispatcher(self.current_store, now=self.now, registry_rpc=self.registry_rpc)
            readiness = worker.request(self.account, 690)
            worker.ready(readiness, sign(readiness, "operator"))
            self.controller_reload()
            bundle = self.controller.snapshot(identifier, chain, artifact, True)
        old = next(context for context in bundle["audit"]["contexts"]
                   if context["bundle"]["identity"]["period"] == 5)
        archive = directory / "audit-events" / old["journal_id"][2:]
        self.assertEqual(old["bundle"]["checkpoint"]["eventCount"], len(events))
        self.assertEqual(list(audit.iter_events(old["bundle"], archive)), events)
        self.assertEqual(self.check_audit(bundle, identifier)["covered_batches"], [1, 2])

    def test_grace_closes_during_wrap_repairs_and_reendorses_retained_proof(self):
        self.handoff()
        identifier, original = self.frozen()
        first_work = self.controller.step(True)
        prepared = self.returned(first_work)
        self.native.responses.append((204, {"x-syscoin-prover-disposition": "accepted"}, b""))
        self.assertEqual(self.controller_reload().step(True)["next_action"], "await_node_publication")
        self.publication(prepared)
        directory = self.controller.directory(identifier)
        original_proof = (directory / "proof.json").read_bytes()
        original_manifest = copy.deepcopy(original["request"]["manifest"])
        original_deadline, original_turn = self.rpc.deadline, self.rpc.turn
        original_leases = copy.deepcopy(self.controller.state["operations"][identifier]["leases"])
        native_calls = len(self.native.calls)
        self.registry_rpc.finalize(650, 102)
        self.now = self.rpc.now = 655
        self.rpc.head["timestamp"] = hex(self.now)
        self.rpc.blocks[self.rpc.head["number"]] = copy.deepcopy(self.rpc.head)

        repair = self.controller_reload().step(True)
        self.assertEqual(repair["status"], "broadcast")
        manager = transactions.Transactions(self.f["settings"], self.config["transaction_policy"], self.rpc,
                self.wallet, transactions.Store(self.control_store.root / "transactions"), clock=lambda: self.now)
        operation = manager.state["operations"][repair["operation_id"]]
        self.assertEqual(operation["call"]["action"], "repairPackage")
        revised = self.controller.snapshot(identifier, *self.controller.status(), True)
        self.assertEqual(revised["request"]["duties"], [])
        self.assertEqual(revised["request"]["manifest"], original_manifest)
        self.assertEqual((directory / "proof.json").read_bytes(), original_proof)
        self.assertEqual(self.controller.state["operations"][identifier]["leases"], original_leases)
        self.assertEqual((self.rpc.deadline, self.rpc.turn), (original_deadline, original_turn))

        self.rpc.receipt = {"transactionHash": operation["transaction_hash"],
            "from": self.f["settings"]["sequencer"], "to": self.f["settings"]["proof_gate"],
            "blockNumber": self.rpc.head["number"], "blockHash": self.rpc.head["hash"], "status": "0x1", "logs": []}
        self.rpc.nonce = self.rpc.pending_nonce = 1
        self.rpc.item["sidecar"] = self.controller.prepare(revised["request"], self.f["evidence"], self.f["fri_payload"])
        self.rpc.advance(seconds=1)
        self.now = self.rpc.now
        next_work = self.controller_reload().step(True)
        self.assertEqual(next_work["next_action"], "await_selected_wrapper")
        self.assertNotEqual(next_work["work_hash"], first_work["work_hash"])
        source = Path(self.controller.state["operations"][identifier]["works"][next_work["work_hash"]]["directory"])
        self.assertEqual(io.private_json(source / "work.json")["body"]["proof"], self.f["snark"])
        self.assertEqual(len(self.native.calls), native_calls)
        fresh = self.returned(next_work)
        self.rpc.receipt = None
        self.rpc.item["digest"] = fresh["sequencer_request"]["digest"]
        staged = self.controller_reload().step(True)
        self.assertEqual(staged["next_action"], "relay_dual_endorsed_package")
        relay = r.Relay(self.f["settings"], self.config["relay_policy"], self.rpc,
                        store=r.Store(self.control_store.root / "relay"))
        self.rpc.item = relay.load_artifact(staged["operation_id"])
        self.assertEqual(self.rpc.item["sidecar"]["duties"], [])
        self.assertEqual(self.controller_reload().step(True)["status"], "broadcast")
        self.rpc.accept_receipt()
        self.assertEqual(self.controller_reload().step(True)["status"], "mined_unconfirmed")
        self.rpc.advance(seconds=1)
        self.now = self.rpc.now
        self.assertTrue(self.controller_reload().step(True)["written"])
        self.assertEqual((directory / "proof.json").read_bytes(), original_proof)
        self.assertEqual(len(self.native.calls), native_calls)

    def test_already_open_old_wrapper_period_repairs_closed_report(self):
        self.registry_rpc = EpochRegistryRpc(self.f["settings"], self.f["subscriptions"])
        self.now = self.rpc.now = 550
        self.rpc.head["timestamp"] = hex(self.now)
        self.rpc.blocks[self.rpc.head["number"]] = copy.deepcopy(self.rpc.head)
        self.rpc.deadline = 750
        identifier, original = self.frozen()
        self.registry_rpc.finalize(650, 101)
        self.now = self.rpc.now = 655
        self.rpc.head["timestamp"] = hex(self.now)
        self.rpc.blocks[self.rpc.head["number"]] = copy.deepcopy(self.rpc.head)
        result = self.controller_reload().step(True)
        self.assertEqual(result["status"], "broadcast")
        manager = transactions.Transactions(self.f["settings"], self.config["transaction_policy"], self.rpc,
                self.wallet, transactions.Store(self.control_store.root / "transactions"), clock=lambda: self.now)
        self.assertEqual(manager.state["operations"][result["operation_id"]]["call"]["action"], "repairPackage")
        revised = self.controller.snapshot(identifier, *self.controller.status(), True)
        self.assertEqual(revised["request"]["duties"], [])
        self.assertEqual(revised["request"]["manifest"], original["request"]["manifest"])
        self.assertEqual(revised["audit"], original["audit"])
        self.assertEqual(revised["request"]["proposal"]["accepted_package"]["period"], 5)
        self.assertEqual(self.rpc.deadline, 750)
