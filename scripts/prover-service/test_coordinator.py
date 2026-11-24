import base64
import copy
from contextlib import nullcontext
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch
import urllib.parse

import audit
import coordinator as c
import enrollment
import journal
import keeper as k
import relay as r
import roster
import service as s
import workflow_io as io
import test_audit as ta
import test_dispatcher as td
import test_transactions as tt
from test_keeper import setup
from test_proof_check import ProofRpc
from test_relay import FakeWallet, relay_policy, signed_raw
from test_service import KEYS, a, h, sign, fixture
from test_adapter import release


def seed_retained_proof(controller, identifier, proof):
    directory = controller.directory(identifier)
    work_hash = h(192)
    result = {"schema_version": 1, "work_hash": work_hash, "proof": proof, "prepared": None, "wrapper_signature": "0x"}
    io.immutable_json(directory / (work_hash[2:] + ".result.json"), result)
    io.immutable_json(directory / "proof.json", proof)
    controller.state["operations"][identifier]["proof"] = {
        "work_hash": work_hash, "result_sha256": io.digest(result), "proof_sha256": io.digest(proof)}


class Wallet:
    def __init__(self, settings):
        self.settings, self.calls = settings, []

    def call(self, method, params):
        self.calls.append((method, copy.deepcopy(params)))
        if method == "eth_chainId":
            return self.settings["settlement_chain_id"]
        if method == "eth_signTransaction":
            return {"raw": signed_raw(params[0], KEYS["sequencer"])}
        if method == "eth_signTypedData_v4":
            data = json.loads(params[1])
            return s.cast("wallet", "sign", "--private-key", KEYS["sequencer"], "--data", json.dumps(data))
        raise AssertionError(method)


class CoordinatorTests(unittest.TestCase):
    setUpClass = classmethod(td.DispatcherTests.setUpClass.__func__)
    reload = td.DispatcherTests.reload
    ready = td.DispatcherTests.ready
    evidence = td.DispatcherTests.evidence
    pick = td.DispatcherTests.pick
    offered = td.DispatcherTests.offered
    signed_proof = td.DispatcherTests.signed_proof
    accepted = ta.AuditTests.accepted

    def setUp(self):
        td.DispatcherTests.setUp(self)
        self.accepted(1)
        self.accepted(2)
        self.registry_rpc = self.rpc
        registry_connection = patch.object(k, "registry_rpc_for", side_effect=lambda _: self.registry_rpc)
        registry_connection.start()
        self.addCleanup(registry_connection.stop)
        self.base = Path(self.tmp.name).resolve()
        self.roster = roster.construct(5, [self.f["proposal"]["candidate"]])
        self.f["proposal"]["accepted_package"]["rosterRoot"] = self.roster["root"]
        self.f["proposal"]["candidate_proof"] = self.roster["proofs"][0]
        keeper, _, item = setup(self.f)
        keeper["policy"]["expected_operator"] = self.f["settings"]["sequencer"]
        self.rpc = ProofRpc(self.f, keeper, item)
        self.rpc.opened = False
        self.native = td.Network()
        for name in ("rosters", "inbox", "publications"):
            io.directory(self.base / name, create=True)
        io.immutable_json(self.base / "rosters" / "5.json", self.roster)
        snark_release = release("SNARK")
        snark_release["vk_hash"] = self.f["settings"]["vk_hash"]
        io.immutable_json(self.base / "release.json", snark_release)
        io.immutable_bytes(self.base / "auth", b"user:password")
        for name in ("sequencer-wallet", "relay-wallet"):
            io.immutable_json(self.base / name, {"url": "http://127.0.0.1:8545", "authorization": None})
        policy = relay_policy(self.f["settings"])
        self.config = {"schema_version": 1, "keeper": keeper, "endpoint": "https://trusted.example/",
            "native_auth_file": str(self.base / "auth"), "release_file": str(self.base / "release.json"),
            "native_lease_seconds": 600, "dispatcher_dir": str(self.root.resolve()), "roster_dir": str(self.base / "rosters"),
            "operator_inboxes": {self.f["proposal"]["candidate"]["operator"]: str(self.base / "inbox")},
            "publication_dir": str(self.base / "publications"),
            "sequencer_wallet_file": str(self.base / "sequencer-wallet"), "relay_wallet_file": str(self.base / "relay-wallet"),
            "transaction_policy": {**policy, "account": self.f["settings"]["sequencer"]}, "relay_policy": policy,
            "sequencer_beneficiary": self.f["proposal"]["accepted_package"]["sequencerBeneficiary"], "poll_interval_seconds": 1}
        self.control_store = c.initialize(self.base / "control", self.config)
        self.wallet, self.relay_wallet = Wallet(self.f["settings"]), FakeWallet(self.f["settings"])
        self.now = 1000
        self.controller = self.controller_reload()

    def controller_reload(self):
        self.controller = c.Coordinator(self.control_store, self.rpc, self.wallet, self.relay_wallet,
                                        self.native, clock=lambda: self.now)
        return self.controller

    def continuous_dispatch(self, unready_peer=False, quota=1, batch_count=2):
        if batch_count != 2:
            self.f = fixture(batch_count=batch_count)
            self.f["proposal"]["accepted_package"]["rosterRoot"] = self.roster["root"]
            self.f["proposal"]["candidate_proof"] = self.roster["proofs"][0]
        self.f["settings"]["duties_per_round"] = quota
        if unready_peer:
            subscription = copy.deepcopy(self.f["subscription"])
            subscription["account"] = s.cast("wallet", "address", "--private-key", KEYS["wrapper"]).lower()
            subscription["operator"] = self.account
            self.f["subscriptions"].append({"subscription": subscription,
                "signature": sign(s.subscription_request(self.f["settings"], subscription), "wrapper")})
        self.root = self.base / "continuous-dispatcher"
        self.rpc = td.Rpc(self.f["settings"], self.f["subscriptions"])
        self.registry_rpc = self.rpc
        self.network = td.Network()
        self.store = td.d.initialize(self.root, self.f["settings"], self.f["subscriptions"], 5,
                                     "https://trusted.example/", self.rpc)
        self.f["subscriptions"] = s.read_json(self.root / "state.json", private=True)["subscriptions"]
        self.reload()
        operations = [self.accepted(number) for number in range(1, batch_count + 1)]
        manifest = self.dispatcher.range_manifest_payload("child", 1, batch_count)
        self.f["manifest"] = {"payload": manifest,
                              "sequencer_signature": sign(s.manifest_request(self.f["settings"], manifest), "sequencer")}
        self.f["duties"] = [io.private_json(self.dispatcher.directory(operation) / "duty.json")
                             for operation in operations if self.dispatcher.state["operations"][operation]["assignment"]["slot"] < quota]
        self.registry_rpc = self.rpc
        keeper, _, item = setup(self.f)
        keeper["policy"]["expected_operator"] = self.f["settings"]["sequencer"]
        self.rpc = ProofRpc(self.f, keeper, item)
        self.rpc.opened = False
        self.native = td.Network()
        self.config = {**self.config, "keeper": keeper, "dispatcher_dir": str(self.root)}
        self.control_store = c.initialize(self.base / "continuous-control", self.config)
        self.controller_reload()

    def acquired(self):
        self.native.responses += [(200, {}, {**self.f["fri_payload"], "lease_token": h(180)}),
                                  (200, {}, self.f["evidence"])]
        identifier = self.controller.reserve()
        self.assertIsNone(self.controller.acquire(identifier, True))
        return identifier

    def test_missing_enrollment_rejects_configuration_before_state_creation(self):
        config = copy.deepcopy(self.config)
        del config["keeper"]["enrollment"]
        root = self.base / "unconfigured-control"
        with self.assertRaisesRegex(s.Error, "configured_enrollment_authority_required"):
            c.initialize(root, config)
        self.assertFalse(root.exists())
        self.assertEqual(self.native.calls, [])

    def test_acquire_authenticates_registry_and_pin_before_lease_mutation(self):
        identifier = self.controller.reserve()
        before = copy.deepcopy(self.controller.state)
        files = {path.name: path.read_bytes() for path in self.controller.directory(identifier).iterdir()}
        with patch.object(self.registry_rpc, "call", return_value="0x1"):
            with self.assertRaisesRegex(s.Error, "wrong_rpc_chain"):
                self.controller.acquire(identifier, True)
        self.assertEqual(self.controller.state, before)
        self.assertEqual(self.native.calls, [])
        self.controller.keeper["enrollment"]["block_hash"] = h(199)
        with self.assertRaisesRegex(s.Error, "enrollment_authority_anchor_mismatch"):
            self.controller.acquire(identifier, True)
        self.assertEqual(self.controller.state, before)
        self.assertEqual(self.native.calls, [])
        self.assertEqual({path.name: path.read_bytes() for path in self.controller.directory(identifier).iterdir()}, files)

    def test_acquire_requires_registry_connection_before_native_pick(self):
        identifier = self.controller.reserve()
        before = copy.deepcopy(self.controller.state)
        with patch.object(k, "registry_rpc_for", side_effect=s.Error("registry_rpc_unavailable")):
            with self.assertRaisesRegex(s.Error, "registry_rpc_unavailable"):
                self.controller.acquire(identifier, True)
        self.assertEqual(self.controller.state, before)
        self.assertEqual(self.native.calls, [])

    def test_snapshot_reuses_authenticated_immutable_enrollment(self):
        identifier = self.acquired()
        count = len(self.registry_rpc.views)
        chain, artifact = self.controller.status()
        self.controller.snapshot(identifier, chain, artifact, True)
        self.assertTrue(all(signature == "isRoundFinalized(uint256)"
                            for signature in self.registry_rpc.views[count:]))

    def frozen(self):
        identifier = self.acquired()
        chain, roster_artifact = self.controller.status()
        bundle = self.controller.snapshot(identifier, chain, roster_artifact, True)
        prepared = k.prepare(self.f["settings"], bundle["request"], self.f["evidence"], self.f["fri_payload"])
        self.rpc.item["sidecar"] = prepared
        self.rpc.opened = True
        return identifier, bundle

    def returned(self, work, bare=False, *, endorsement_key="wrapper", envelope_key="wrapper"):
        directory = Path(self.controller.state["operations"][self.controller.state["active"]]["works"][work["work_hash"]]["directory"])
        envelope = io.private_json(directory / "work.json")
        request = envelope["body"]["request"]
        prepared = s.prepare_package(self.f["settings"], self.f["evidence"], request["manifest"],
                        request["subscriptions"], request["duties"], request["proposal"], self.f["snark"], self.f["fri_payload"])
        result = {"schema_version": 1, "work_hash": work["work_hash"], "proof": self.f["snark"],
                  "prepared": None if bare else prepared,
                  "wrapper_signature": "0x" if bare else sign(prepared["wrapper_request"], endorsement_key)}
        signed = {"result": result, "operator_signature": sign(io.result_request(self.f["settings"], work["work_hash"],
                                        result, self.f["proposal"]["candidate"]["operator"]), envelope_key)}
        io.immutable_json(directory / "result.json", signed)
        return prepared

    def publication(self, prepared):
        root = io.directory(self.base / "publications" / "native-proof", create=True)
        settings, limits = self.f["settings"], self.config["relay_policy"]
        io.immutable_json(root / "work.json", {"schema_version": 1,
            "execution_chain_id": settings["execution_chain_id"], "settlement_chain_id": settings["settlement_chain_id"],
            "chain_address": settings["chain_address"], "gate": settings["proof_gate"], "gate_code_hash": limits["gate_code_hash"],
            "coordinator": settings["coordinator"], "coordinator_code_hash": limits["coordinator_code_hash"],
            "policy_hash": settings["policy_hash"], "production_vk_hash": settings["vk_hash"], "sequencer": settings["sequencer"],
            "timelock": a(132), "required_confirmations": 2, "batch_from": 1,
            "batch_to": self.f["snark"]["to_batch_number"],
            "proof_data": prepared["proof_data"], "batch_outputs": prepared["sidecar"]["batch_outputs"]})
        return root

    def test_dry_run_never_picks_signs_or_writes(self):
        before = {str(path): path.read_bytes() for path in self.base.rglob("*") if path.is_file()}
        self.assertEqual(self.controller.step()["next_action"], "would_pick_native_snark")
        self.assertEqual(self.native.calls, [])
        self.assertEqual(self.wallet.calls, [])
        self.assertEqual(before, {str(path): path.read_bytes() for path in self.base.rglob("*") if path.is_file()})

    def test_native_observation_recovers_after_restart_and_supersedes_overlapping_lease(self):
        identifier = self.acquired()
        self.rpc.native_proved = 1
        self.assertEqual(self.controller.step(False)["next_action"], "would_observe_native_settlement")
        self.assertNotIn("native_observation", self.controller.state)
        self.assertEqual(self.controller.step(True)["status"], "broadcast")
        observation = self.controller.state["native_observation"]
        self.assertEqual(observation["invocation"]["scope"]["native_batch"], 1)
        self.assertEqual(observation["call"]["action"], "observeNativeSettlement")
        self.assertEqual(self.controller_reload().step(True)["next_action"], "wait_before_identical_rebroadcast")
        self.rpc.observed_native = 1
        self.rpc.nonce = self.rpc.pending_nonce = 1
        self.rpc.receipt = {"transactionHash": s.keccak(s.raw_hex(self.rpc.sent[-1])),
                            "from": self.f["settings"]["sequencer"], "to": self.f["settings"]["proof_gate"],
                            "blockNumber": self.rpc.head["number"], "blockHash": self.rpc.head["hash"],
                            "status": "0x1", "logs": []}
        self.assertEqual(self.controller_reload().step(True)["status"], "mined_unconfirmed")
        self.rpc.advance()
        self.now = self.rpc.now
        self.assertEqual(self.controller_reload().step(True)["status"], "confirmed")
        self.assertNotIn("native_observation", self.controller.state)
        self.assertEqual(self.controller_reload().step(True)["next_action"], "native_advance_requires_new_range")
        self.assertEqual(self.controller.state["operations"][identifier]["status"], "superseded")
        self.assertIsNone(self.controller.state["active"])
        self.assertEqual(len(self.native.calls), 2)
        self.assertEqual(len(self.rpc.sent), 1)

    def test_another_keeper_confirms_native_observation_and_restart_picks_new_proof_work(self):
        self.continuous_dispatch(batch_count=4)
        old = self.acquired()
        self.rpc.native_proved = 2
        self.assertEqual(self.controller.step(True)["status"], "broadcast")
        observation = self.controller.state["native_observation"]
        store = c.transactions.Store(self.control_store.root / "transactions")

        def observation_row():
            return store.load(self.f["settings"], self.config["transaction_policy"])["operations"][observation["operation_id"]]

        original = observation_row()
        self.assertEqual(self.rpc.sent, [original["raw_transaction"]])
        self.assertIsNone(original["receipt"])
        before_external_observation = self.rpc.head["hash"]
        self.rpc.advance()
        self.now = self.rpc.now
        self.rpc.observed_native = 2
        observed_selector = "0x" + r.selector("observedNativeBatch()").hex()
        original_call = self.rpc.call

        def historical_call(method, params):
            if (method == "eth_call" and params[0]["data"] == observed_selector
                    and params[1]["blockHash"] == before_external_observation):
                return "0x" + s.word(0).hex()
            return original_call(method, params)

        with patch.object(self.rpc, "call", side_effect=historical_call):
            self.controller_reload().step(True)
            self.assertEqual(self.controller.state["active"], old)
            self.assertEqual(self.controller.state["native_observation"], observation)
            self.assertEqual(len(self.native.calls), 2)

        self.rpc.advance()
        self.now = self.rpc.now
        full = self.f["evidence"]
        payload = {**self.f["fri_payload"], "from_batch_number": 3, "fri_proofs": self.f["fri_payload"]["fri_proofs"][2:]}
        evidence = {**full, "previous_batch": full["batches"][1]["stored"], "batches": full["batches"][2:]}
        self.native.responses += [(200, {}, {**payload, "lease_token": h(191)}), (200, {}, evidence)]
        self.assertEqual(self.controller_reload().step(True)["next_action"], "resume_after_confirmed_native_observation")
        self.assertEqual(self.controller_reload().step(True)["next_action"], "native_advance_requires_new_range")
        self.assertEqual(self.controller_reload().step(True)["next_action"], "wait_for_existing_wallet_reservation")
        new = self.controller.state["active"]
        self.assertNotEqual(new, old)
        directory = self.controller.directory(new)
        self.assertEqual(io.private_json(directory / "payload.json"), payload)
        self.assertEqual(io.private_json(directory / "evidence.json"), evidence)
        self.assertIsNotNone(self.controller.state["operations"][new]["bundle"])
        self.assertIsNone(self.controller.state["operations"][new]["maintenance"])
        self.assertEqual(len(self.native.calls), 4)
        self.assertIsNone(observation_row()["receipt"])
        self.assertEqual(self.rpc.sent, [original["raw_transaction"]])

        self.rpc.receipt = {"transactionHash": original["transaction_hash"], "from": self.f["settings"]["sequencer"],
                            "to": self.f["settings"]["proof_gate"], "blockNumber": self.rpc.head["number"],
                            "blockHash": self.rpc.head["hash"], "status": "0x1", "logs": []}
        self.rpc.nonce = self.rpc.pending_nonce = 1
        self.assertEqual(self.controller_reload().step(True)["next_action"], "wait_for_existing_wallet_reservation")
        self.assertEqual(observation_row()["status"], "mined_unconfirmed")
        self.rpc.advance()
        self.now = self.rpc.now
        self.assertEqual(self.controller_reload().step(True)["status"], "broadcast")
        row = observation_row()
        self.assertEqual(row["status"], "confirmed")
        self.assertEqual(self.controller_reload().state["active"], new)
        for field in ("unsigned_transaction", "raw_transaction", "transaction_hash"):
            self.assertEqual(row[field], original[field])
        signatures = [params[0] for method, params in self.wallet.calls if method == "eth_signTransaction"]
        self.assertEqual(len(signatures), 2)
        self.assertEqual(signatures[0], original["unsigned_transaction"])
        self.assertEqual(s.uint(signatures[1]["nonce"]), 1)
        self.assertEqual(self.controller.state["operations"][new]["maintenance"]["call"]["action"], "openPackage")
        self.assertEqual(len(self.rpc.sent), 2)
        self.assertNotEqual(self.rpc.sent[1], original["raw_transaction"])

    def test_reserved_observation_keeps_its_lease_on_changed_batch_hash_or_native_rewind(self):
        old = self.acquired()
        self.rpc.native_proved = 2
        self.assertEqual(self.controller.step(True)["status"], "broadcast")
        observation = self.controller.state["native_observation"]
        store = c.transactions.Store(self.control_store.root / "transactions")
        original = store.load(self.f["settings"], self.config["transaction_policy"])["operations"][observation["operation_id"]]
        self.rpc.advance()
        confirmed_hash = self.rpc.head["hash"]
        self.rpc.observed_native = 2
        self.rpc.advance()
        self.now = self.rpc.now
        original_call = self.rpc.call
        stored_call = "0x" + (r.selector("storedBatchHash(uint256)") + s.word(2)).hex()
        for block_hash in (confirmed_hash, self.rpc.head["hash"]):
            with self.subTest(block_hash=block_hash):
                def changed_hash(method, params):
                    if (method == "eth_call" and params[0]["data"] == stored_call
                            and params[1]["blockHash"] == block_hash):
                        return h(205)
                    return original_call(method, params)
                with patch.object(self.rpc, "call", side_effect=changed_hash), self.assertRaisesRegex(
                        s.Error, "native_observation_batch_hash_changed"):
                    self.controller_reload().step(True)
                self.assertEqual(self.controller_reload().state["native_observation"], observation)
                self.assertEqual(self.controller.state["active"], old)

        frontiers = {"0x" + r.selector(signature).hex() for signature in ("observedNativeBatch()", "getTotalBatchesVerified()")}
        def rewound_tip(method, params):
            if (method == "eth_call" and params[0]["data"] in frontiers
                    and params[1]["blockHash"] == self.rpc.head["hash"]):
                return "0x" + s.word(1).hex()
            return original_call(method, params)
        with patch.object(self.rpc, "call", side_effect=rewound_tip):
            self.assertEqual(self.controller_reload().step(True)["status"], "authorization_stale_reserved")
        self.assertEqual(self.controller_reload().state["native_observation"], observation)
        self.assertEqual(self.controller.state["active"], old)
        row = store.load(self.f["settings"], self.config["transaction_policy"])["operations"][observation["operation_id"]]
        self.assertEqual(row["unsigned_transaction"], original["unsigned_transaction"])
        self.assertEqual(row["raw_transaction"], original["raw_transaction"])
        self.assertIsNone(row["receipt"])
        self.assertEqual(self.rpc.sent, [original["raw_transaction"]])

    def test_pending_package_open_cannot_hold_the_native_observation_slot_after_restart(self):
        self.check_pending_package_open_recovery(poll_before_observation=True)

    def test_replacement_package_cannot_adopt_the_old_package_open_reservation(self):
        self.check_pending_package_open_recovery(poll_before_observation=False)

    def check_pending_package_open_recovery(self, *, poll_before_observation):
        self.continuous_dispatch(batch_count=4)
        old = self.acquired()
        self.assertEqual(self.controller.step(True)["status"], "broadcast")
        pending = self.controller.state["operations"][old]["maintenance"]
        self.assertEqual(pending["call"]["action"], "openPackage")
        store = c.transactions.Store(self.control_store.root / "transactions")
        def reservation():
            return store.load(self.f["settings"], self.config["transaction_policy"])["operations"][pending["operation_id"]]
        original = reservation()

        self.rpc.native_proved = 2
        if poll_before_observation:
            self.controller_reload().step(True)
            self.assertIsNone(self.controller_reload().state.get("native_observation"))
            self.assertEqual(self.controller.state["maintenance_receipt"], pending["operation_id"])
            self.assertEqual(self.rpc.sent, [original["raw_transaction"]])
        self.rpc.advance()
        self.rpc.observed_native = 2
        self.rpc.advance()
        self.now = self.rpc.now
        full = self.f["evidence"]
        payload = {**self.f["fri_payload"], "from_batch_number": 3, "fri_proofs": self.f["fri_payload"]["fri_proofs"][2:]}
        evidence = {**full, "previous_batch": full["batches"][1]["stored"], "batches": full["batches"][2:]}
        self.native.responses += [(200, {}, {**payload, "lease_token": h(191)}), (200, {}, evidence)]
        for _ in range(4):
            self.controller_reload().step(True)
            active = self.controller.state["active"]
            if active is not None and active != old:
                break
        self.assertIsNotNone(active)
        self.assertNotEqual(active, old)
        self.assertEqual(io.private_json(self.controller.directory(active) / "payload.json"), payload)
        self.assertEqual(io.private_json(self.controller.directory(active) / "evidence.json"), evidence)
        self.assertIsNotNone(self.controller.state["operations"][active]["bundle"])
        self.assertNotEqual(self.controller.state["operations"][active]["maintenance"], pending)
        self.assertNotIn("native_observation", self.controller_reload().state)
        self.assertEqual(self.controller.state["maintenance_receipt"], pending["operation_id"])
        self.assertEqual(len(self.native.calls), 4)
        self.assertIsNone(reservation()["receipt"])
        for field in ("unsigned_transaction", "raw_transaction", "transaction_hash"):
            self.assertEqual(reservation()[field], original[field])
        self.assertEqual(self.rpc.sent, [original["raw_transaction"]])
        self.assertEqual([args[0] for method, args in self.wallet.calls if method == "eth_signTransaction"],
                         [original["unsigned_transaction"]])

    def publish_signed_three_batch_relay(self):
        self.continuous_dispatch(batch_count=3)
        identifier, _ = self.frozen()
        prepared = self.returned(self.controller.step(True))
        self.native.responses.append((204, {"x-syscoin-prover-disposition": "accepted"}, b""))
        self.assertEqual(self.controller_reload().step(True)["next_action"], "await_node_publication")
        publication = self.publication(prepared)
        self.rpc.item["digest"] = prepared["sequencer_request"]["digest"]
        stage = self.controller_reload().step(True)
        self.assertEqual(stage["next_action"], "relay_dual_endorsed_package")
        relay = r.Relay(self.f["settings"], self.config["relay_policy"], self.rpc,
                        store=r.Store(self.control_store.root / "relay"))
        relay_id = stage["operation_id"]
        self.rpc.item = relay.load_artifact(relay_id)
        self.assertEqual(self.controller_reload().step(True)["status"], "broadcast")
        return identifier, publication, relay.store, relay_id

    def native_prefix_observation(self, publication):
        self.rpc.advance(seconds=1)
        event_block = copy.deepcopy(self.rpc.head)
        self.now = self.rpc.now
        self.rpc.opened = False
        self.rpc.observed_native = self.rpc.native_proved = 2
        gate = self.f["settings"]["proof_gate"]
        self.rpc.status_overrides[(gate, "initialNativeBatch()")] = 0
        signature = s.keccak(b"NativeSettlementAdvanced(uint64,uint64,bytes32)")
        log = {"address": gate, "blockNumber": event_block["number"], "blockHash": event_block["hash"],
               "topics": [signature, "0x" + s.word(1).hex(), "0x" + s.word(2).hex()],
               "data": s.keccak(s.encode_fields(s.STORED, self.f["evidence"]["batches"][1]["stored"])),
               "transactionHash": h(183), "logIndex": "0x0", "removed": False}
        # Rust writes the canonical event block hash as a JSON string, independently
        # of the service relay. Its NativeSettlementAdvanced event grants no duty credits.
        io.immutable_json(publication / "superseded.json", event_block["hash"])
        original = self.rpc.call
        frontier_selectors = {"0x" + r.selector(signature).hex() for signature in
                              ("getTotalBatchesVerified()", "observedNativeBatch()")}
        def history(method, params):
            if method == "eth_getBlockByHash":
                return next((copy.deepcopy(b) for b in self.rpc.blocks.values() if b["hash"] == params[0]), None)
            if method == "eth_getLogs" and params[0].get("topics", [None])[0] == signature:
                self.assertEqual(params[0]["blockHash"], event_block["hash"])
                self.assertEqual(params[0]["address"], gate)
                return [copy.deepcopy(log)]
            if method == "eth_call" and params[0]["data"] in frontier_selectors:
                block = next(b for b in self.rpc.blocks.values() if b["hash"] == params[1]["blockHash"])
                if s.uint(block["number"]) < s.uint(event_block["number"]):
                    return "0x" + s.word(0).hex()
            return original(method, params)
        mock = patch.object(self.rpc, "call", side_effect=history)
        mock.start()
        self.addCleanup(mock.stop)
        return log

    def test_signed_relay_detaches_for_confirmed_native_prefix_and_picks_remaining_range(self):
        old, publication, store, relay_id = self.publish_signed_three_batch_relay()
        def relay_row():
            return store.load(self.f["settings"], self.config["relay_policy"])["operations"][relay_id]
        original = copy.deepcopy(relay_row())
        self.assertEqual(self.rpc.sent, [original["raw_transaction"]])
        self.native_prefix_observation(publication)
        self.rpc.advance()
        self.now = self.rpc.now
        result = self.controller_reload().step(True)
        self.assertEqual(result["next_action"], "native_advance_requires_new_range")
        controller = self.controller_reload()
        self.assertEqual(controller.state["operations"][old]["status"], "superseded")
        self.assertIsNone(controller.state["active"])
        self.assertIn(old, controller.state["regrouped_pending"])
        self.assertEqual(controller.step(False)["next_action"], "would_pick_native_snark")

        native = fixture(batch_count=5)
        payload = {**native["fri_payload"], "from_batch_number": 3, "fri_proofs": native["fri_payload"]["fri_proofs"][2:]}
        full = native["evidence"]
        evidence = {**full, "previous_batch": full["batches"][1]["stored"], "batches": full["batches"][2:]}
        self.rpc.evidence = full
        self.native.responses += [(200, {}, {**payload, "lease_token": h(191)}), (200, {}, evidence)]
        # Stop at the endorsement stage so the next polling cycle must actually
        # reserve the new slot, request the replacement and persist its evidence.
        with patch.object(controller, "snapshot", return_value=None) as snapshot:
            controller.step(True)
        snapshot.assert_called_once()
        new = controller.state["active"]
        self.assertEqual(snapshot.call_args.args[0], new)
        self.assertEqual(io.private_json(controller.directory(new) / "payload.json"), payload)
        self.assertEqual(io.private_json(controller.directory(new) / "evidence.json"), evidence)
        self.assertNotEqual(new, old)
        self.controller_reload().reconcile_regrouped(True)
        self.assertIn(old, self.controller.state["regrouped_pending"])
        self.rpc.accept_receipt(status=0)
        self.rpc.advance()
        self.now = self.rpc.now
        self.controller_reload().reconcile_regrouped(True)
        self.assertEqual(self.controller_reload().state["active"], new)
        self.assertNotIn(old, self.controller.state["regrouped_pending"])
        row = relay_row()
        for field in ("unsigned_transaction", "raw_transaction", "transaction_hash"):
            self.assertEqual(row[field], original[field])
        self.assertEqual(row["status"], "reverted")
        self.assertIsNone(row["acceptance"])
        self.assertFalse((publication / "relay.json").exists())
        signed_transactions = [params[0] for method, params in self.relay_wallet.calls if method == "eth_signTransaction"]
        self.assertEqual(signed_transactions, [original["unsigned_transaction"]])
        self.assertEqual(self.rpc.sent, [original["raw_transaction"]])

    def test_superseded_marker_needs_confirmed_matching_native_event(self):
        old, publication, store, relay_id = self.publish_signed_three_batch_relay()
        signed = copy.deepcopy(self.rpc.sent)
        log = self.native_prefix_observation(publication)
        self.assertEqual(self.controller_reload().step(True)["next_action"], "wait_for_receipt_no_resign")
        self.assertEqual(self.controller_reload().state["active"], old)
        self.rpc.advance()
        self.now = self.rpc.now
        log["data"] = h(208)
        with self.assertRaisesRegex(s.Error, "superseded_native_event_hash_changed"):
            self.controller.step(True)
        self.assertEqual(self.controller_reload().state["active"], old)
        self.assertNotIn(old, self.controller.state.get("regrouped_pending", []))
        row = store.load(self.f["settings"], self.config["relay_policy"])["operations"][relay_id]
        self.assertEqual(row["raw_transaction"], signed[0])
        self.assertIsNone(row["acceptance"])
        self.assertFalse((publication / "relay.json").exists())
        self.assertEqual(self.rpc.sent, signed)

    def test_fully_covered_signed_relay_exports_its_late_service_receipt_for_the_original_work(self):
        old, publication, store, relay_id = self.publish_signed_three_batch_relay()
        signed = copy.deepcopy(self.rpc.sent)
        self.rpc.advance()
        self.rpc.accept_receipt()
        historical_receipt, self.rpc.receipt = self.rpc.receipt, None
        # The node can see a subsequent service parent while the original transaction
        # receipt is temporarily unavailable. It must assign any eventual credit to the
        # original accepted publication and keep the newer active lease intact.
        self.rpc.advance()
        self.rpc.parent = h(209)
        self.rpc.native_proved = self.rpc.observed_native = 5
        self.rpc.advance()
        self.now = self.rpc.now
        self.assertFalse((publication / "superseded.json").exists())
        self.assertEqual(self.controller_reload().step(True)["next_action"], "range_already_proven")
        controller = self.controller_reload()
        self.assertEqual(controller.state["operations"][old]["status"], "covered")
        self.assertIsNone(controller.state["active"])
        self.assertIn(old, controller.state["regrouped_pending"])
        self.assertFalse((publication / "relay.json").exists())
        new = controller.reserve()
        self.rpc.receipt = historical_receipt
        self.controller_reload().reconcile_regrouped(True)
        self.assertEqual(self.controller_reload().state["active"], new)
        self.assertEqual(self.controller.state["operations"][old]["status"], "settled")
        self.assertNotIn(old, self.controller.state["regrouped_pending"])
        row = store.load(self.f["settings"], self.config["relay_policy"])["operations"][relay_id]
        self.assertEqual(io.private_json(publication / "relay.json")["transaction_hash"], row["transaction_hash"])
        self.assertEqual(row["raw_transaction"], signed[0])
        self.assertEqual(self.rpc.sent, signed)

    def regrouped_publication(self, retained):
        native = fixture(batch_count=5)
        full = native["evidence"]
        self.rpc.evidence = full
        self.rpc.advance()
        self.now = self.rpc.now
        self.rpc.native_proved = self.rpc.observed_native = 2
        identifier = self.controller.reserve()
        directory = self.controller.directory(identifier)
        payload = {**native["fri_payload"], "from_batch_number": 4, "fri_proofs": native["fri_payload"]["fri_proofs"][3:]}
        io.immutable_json(directory / "payload.json", payload)
        evidence = {**full, "previous_batch": full["batches"][2]["stored"], "batches": full["batches"][3:]}
        proof = {**native["snark"], "from_batch_number": 4}
        root = io.directory(self.base / "publications" / "old-4-5", create=True)
        work = {**c.native_handoff.publication_pins(self.f["settings"], self.config["relay_policy"]),
                "batch_from": 4, "batch_to": 5, "timelock": a(132), "required_confirmations": 2,
                "proof_data": "0x" + s.proof_data(evidence, s.decode_proof(proof["proof"])).hex(),
                "batch_outputs": [b["output"] for b in evidence["batches"]]}
        io.immutable_json(root / "work.json", work)
        io.immutable_json(root / "regrouped.json", {"schema_version": 1, "work_hash": s.keccak(s.canonical(work)),
            "native_proved": 2, "native_batch_hash": s.keccak(s.encode_fields(s.STORED, full["batches"][1]["stored"]))})
        if retained:
            io.immutable_json(directory / "evidence.json", evidence)
            seed_retained_proof(self.controller, identifier, proof)
            self.controller.state["operations"][identifier]["publication"] = str(root)
            self.controller.save()
        return identifier, native, root

    def test_regrouped_published_proof_restarts_with_new_three_batch_lease(self):
        old, native, root = self.regrouped_publication(retained=True)
        self.assertEqual(self.controller_reload().step(False)["next_action"], "would_retire_regrouped_native_range")
        self.assertEqual(self.controller_reload().state["active"], old)
        self.assertEqual(self.controller.step(True)["next_action"], "pick_regrouped_native_range")
        self.assertEqual(self.controller_reload().state["operations"][old]["status"], "regrouped")
        self.assertEqual(self.controller.step(False)["next_action"], "would_pick_native_snark")
        self.assertFalse((root / "relay.json").exists())
        self.assertFalse(self.wallet.calls or self.relay_wallet.calls or self.rpc.sent)
        payload = {**native["fri_payload"], "from_batch_number": 3, "fri_proofs": native["fri_payload"]["fri_proofs"][2:]}
        full = native["evidence"]
        evidence = {**full, "previous_batch": full["batches"][1]["stored"], "batches": full["batches"][2:]}
        self.native.responses += [(200, {}, {**payload, "lease_token": h(191)}), (200, {}, evidence)]
        new = self.controller.reserve()
        self.assertIsNone(self.controller.acquire(new, True))
        self.assertEqual(io.private_json(self.controller.directory(new) / "payload.json"), payload)
        self.assertIsNone(self.controller.state["operations"][new]["bundle"])
        self.assertEqual(self.controller.state["operations"][new]["works"], {})

    def test_unpublished_regrouped_lease_requires_canonical_marker_after_restart(self):
        old, _, root = self.regrouped_publication(retained=False)
        self.rpc.bad_stored = True
        with self.assertRaisesRegex(s.Error, "regrouped_native_hash_changed"):
            self.controller_reload().step(True)
        self.assertEqual(self.controller_reload().state["active"], old)
        self.rpc.bad_stored = False
        self.assertEqual(self.controller.step(True)["next_action"], "pick_regrouped_native_range")
        self.assertEqual(self.controller_reload().state["operations"][old]["publication"], str(root))
        self.assertFalse(self.native.calls or self.rpc.sent)

    def test_unpublished_regrouped_lease_can_hold_another_proof_of_the_same_batches(self):
        old, native, root = self.regrouped_publication(retained=False)
        full = native["evidence"]
        evidence = {**full, "previous_batch": full["batches"][2]["stored"], "batches": full["batches"][3:]}
        proof = {**native["snark"], "from_batch_number": 4, "proof": base64.b64encode(b"b" * (44 * 32)).decode()}
        directory = self.controller.directory(old)
        io.immutable_json(directory / "evidence.json", evidence)
        seed_retained_proof(self.controller, old, proof)
        op = self.controller.state["operations"][old]
        payload = io.private_json(directory / "payload.json")
        op["publication"] = str(root)
        self.assertIsNone(self.controller.retire_regrouped(old, payload, False))
        op["publication"] = None
        self.controller.save()
        self.assertEqual(self.controller_reload().step(True)["next_action"], "pick_regrouped_native_range")
        self.assertFalse(self.wallet.calls or self.relay_wallet.calls or self.rpc.sent)

    def test_regrouped_publication_requires_the_frontier_at_confirmation_depth(self):
        old, _, _ = self.regrouped_publication(retained=True)
        original = self.rpc.call
        safe_hash = self.rpc.blocks[hex(s.uint(self.rpc.head["number"]) - 1)]["hash"]
        def unconfirmed(method, params):
            if (method == "eth_call" and params[1]["blockHash"] == safe_hash
                    and params[0]["data"] == "0x" + r.selector("getTotalBatchesVerified()").hex()):
                return "0x" + s.word(1).hex()
            return original(method, params)
        with patch.object(self.rpc, "call", side_effect=unconfirmed):
            with self.assertRaisesRegex(s.Error, "regrouped_native_frontier_unconfirmed"):
                self.controller_reload().step(True)
        self.assertEqual(self.controller_reload().state["active"], old)

    def test_markerless_future_pick_restarts_after_expiry_and_preserves_lease_records(self):
        native = fixture(batch_count=5)
        self.rpc.evidence = native["evidence"]
        self.rpc.native_proved = self.rpc.observed_native = 2
        payload = {**native["fri_payload"], "from_batch_number": 4, "fri_proofs": native["fri_payload"]["fri_proofs"][3:]}
        self.native.responses.append((200, {}, {**payload, "lease_token": h(192)}))
        self.assertEqual(self.controller.step(True)["next_action"], "await_native_frontier_for_picked_lease")
        old = self.controller.state["active"]
        op = self.controller.state["operations"][old]
        lease = self.controller.directory(old) / op["lease"] / "authority.json"
        original = lease.read_bytes()
        self.assertEqual(self.controller_reload().step(True)["next_action"], "await_native_frontier_for_picked_lease")
        self.rpc.advance(seconds=self.config["native_lease_seconds"] + 1)
        self.now = self.rpc.now
        self.assertEqual(self.controller.step(False)["next_action"], "would_release_expired_future_lease")
        self.assertEqual(self.controller_reload().state["active"], old)
        self.assertEqual(self.controller.step(True)["next_action"], "pick_after_expired_future_lease")
        self.assertEqual(self.controller_reload().step(False)["next_action"], "would_pick_native_snark")
        self.assertEqual(self.controller.state["operations"][old]["status"], "expired")
        self.assertEqual(lease.read_bytes(), original)
        self.assertEqual(len(self.native.calls), 1)
        self.assertFalse(self.rpc.sent or self.wallet.calls or self.relay_wallet.calls)

    def test_regroup_marker_distinguishes_old_publication_from_newly_eligible_lease(self):
        old, _, root = self.regrouped_publication(retained=False)
        self.rpc.native_proved = self.rpc.observed_native = 3
        payload = io.private_json(self.controller.directory(old) / "payload.json")
        self.assertIsNone(self.controller.retire_regrouped(old, payload, False))
        self.controller.state["operations"][old]["publication"] = str(root)
        self.controller.save()
        self.assertEqual(self.controller_reload().step(True)["next_action"], "pick_regrouped_native_range")

    def test_regrouped_relay_receipt_after_restart_keeps_the_new_operation_active(self):
        old = self.controller.reserve()
        settings = self.f["settings"]
        prepared = s.prepare_package(settings, self.f["evidence"], self.f["manifest"], self.f["subscriptions"],
                                     self.f["duties"], self.f["proposal"], self.f["snark"], self.f["fri_payload"])
        publication = self.publication(prepared)
        sidecar = s.complete_package(settings, prepared, sign(prepared["sequencer_request"], "sequencer"),
                                     sign(prepared["wrapper_request"], "wrapper"))
        self.rpc.item = r.artifact(settings, sidecar, prepared["proof_data"])
        self.rpc.opened = True
        relay = r.Relay(settings, self.config["relay_policy"], self.rpc, self.relay_wallet,
                        r.Store(self.control_store.root / "relay"), self.controller.clock)
        relay_id = relay.stage(self.rpc.item)
        self.assertEqual(relay.step(relay_id, True)["status"], "broadcast")
        # Restart after the coordinator atomically detached an already signed relay.
        self.controller.state["operations"][old].update(status="regrouped", publication=str(publication), relay=relay_id)
        self.controller.state["regrouped_pending"] = [old]
        new = self.controller.reserve()
        self.controller_reload().reconcile_regrouped(True)
        self.assertFalse((publication / "relay.json").exists())
        self.assertEqual(self.controller.state["regrouped_pending"], [old])
        self.rpc.accept_receipt()
        self.rpc.advance()
        self.now = self.rpc.now
        self.controller_reload().reconcile_regrouped(True)
        self.assertEqual(self.controller_reload().state["active"], new)
        self.assertEqual(self.controller.state["operations"][old]["status"], "settled")
        self.assertEqual(self.controller.state["regrouped_pending"], [])
        self.assertEqual(io.private_json(publication / "relay.json")["transaction_hash"], self.rpc.receipt["transactionHash"])
        self.assertEqual(len(self.rpc.sent), 1)

    def test_archived_delivered_handoff_reconciles_own_receipt_after_restart(self):
        old, publication, store, relay_id = self.publish_signed_three_batch_relay()
        signed = list(self.rpc.sent)
        accepted_tx = h(90)
        self.rpc.parent = self.rpc.item["package_hash"]
        self.rpc.logs = [{"address": self.f["settings"]["proof_gate"], "topics": r.acceptance_topics(self.rpc.item),
                          "data": "0x" + s.word(0).hex(), "transactionHash": accepted_tx,
                          "blockNumber": self.rpc.head["number"], "blockHash": self.rpc.head["hash"]}]
        self.rpc.advance()
        self.now = self.rpc.now
        self.assertEqual(self.controller_reload().step(True)["transaction_hash"], accepted_tx)
        self.assertEqual(io.private_json(publication / "relay.json")["transaction_hash"], accepted_tx)
        self.assertEqual(self.controller.state["operations"][old]["status"], "settled_with_pending_nonce")
        publication.rename(self.base / "archived-publication")
        new = self.controller.reserve()
        self.controller_reload().reconcile_regrouped(True)
        self.assertNotIn("handoff_restore_required", self.controller.state["operations"][old])
        self.assertEqual(self.controller.state["active"], new)

        self.rpc.accept_receipt(status=0, logs=False)
        self.rpc.advance()
        self.now = self.rpc.now
        self.controller_reload().reconcile_regrouped(True)
        self.assertEqual(self.controller.state["operations"][old]["status"], "settled")
        self.assertNotIn(old, self.controller.state["regrouped_pending"])
        self.assertEqual(self.controller.state["active"], new)
        row = store.load(self.f["settings"], self.config["relay_policy"])["operations"][relay_id]
        self.assertEqual(row["raw_transaction"], signed[0])
        self.assertEqual(row["receipt"]["status"], 0)
        self.assertEqual(self.rpc.sent, signed)

    def test_completed_operation_history_does_not_block_new_range(self):
        for index in range(1001):
            self.controller.state["operations"][str(index)] = {"status": "settled", "test": index}
        self.controller.save()
        self.assertTrue((self.control_store.root / "coordinator.json.sqlite3").is_file())
        controller = self.controller_reload()
        identifier = controller.reserve()
        recovered = self.controller_reload()
        self.assertEqual(len(recovered.state["operations"]), 1002)
        self.assertEqual(recovered.state["active"], identifier)
        self.assertEqual(recovered.state["operations"]["0"], {"status": "settled", "test": 0})

    def test_signed_snapshot_replays_real_dispatch_audit_and_exact_duties(self):
        identifier, bundle = self.frozen()
        trust = {"schema_version": 1, "journal_id": bundle["audit"]["journal_id"], "enrollment_block_hash": h(100),
                 "minimum_checkpoint": {"event_count": 0, "event_head": bundle["audit"]["journal_id"],
                                        "assignment_cursor": bundle["audit"]["journal_id"]},
                 "required_readiness": [], "required_duties": []}
        request = bundle["request"]
        checked = audit.verify_package(self.f["settings"], self.f["evidence"], request["manifest"],
                    request["subscriptions"], request["duties"], bundle["audit"], trust, self.registry_rpc,
                    fri_payload=self.f["fri_payload"])
        self.assertEqual(checked["required_duties"], request["duties"])
        self.assertEqual(len(request["duties"]), 2)
        self.assertEqual(self.controller.state["operations"][identifier]["bundle"], io.digest(bundle))

    def test_streamed_snapshot_delivers_checkpoint_bound_archive(self):
        events = list(self.dispatcher.state["audit"]["events"])
        with patch.object(journal, "MAX_INLINE_EVENTS", 1):
            self.dispatcher.save()
        identifier, bundle = self.frozen()
        self.assertEqual(bundle["audit"]["schema_version"], 2)
        archive = self.controller.directory(identifier) / "audit-events"
        self.assertEqual(list(audit.iter_events(bundle["audit"], archive)), events)
        request = bundle["request"]
        journal_id = bundle["audit"]["journal_id"]
        trust = {"schema_version": 1, "journal_id": journal_id, "enrollment_block_hash": h(100),
                 "minimum_checkpoint": {"event_count": 0, "event_head": journal_id, "assignment_cursor": journal_id},
                 "required_readiness": [], "required_duties": []}
        checked = audit.verify_package(self.f["settings"], self.f["evidence"], request["manifest"],
                    request["subscriptions"], request["duties"], bundle["audit"], trust, self.registry_rpc,
                    fri_payload=self.f["fri_payload"], event_root=archive)
        self.assertEqual(checked["required_duties"], request["duties"])
        work = self.controller.step(True)
        source = Path(self.controller.state["operations"][identifier]["works"][work["work_hash"]]["directory"])
        envelope = io.private_json(source / "work.json")
        self.assertEqual(io.validate_work(self.f["settings"], envelope, self.f["fri_payload"]), work["work_hash"])
        self.assertEqual(list(audit.iter_events(envelope["body"]["audit"], source / "audit-events")), events)

    def test_interrupted_archive_export_recovers_frozen_prefix_after_new_dispatch_events(self):
        with patch.object(journal, "MAX_INLINE_EVENTS", 1):
            self.dispatcher.save()
        events = list(self.dispatcher.state["audit"]["events"])
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
            self.reload()
            self.ready()
            self.assertGreater(len(self.dispatcher.state["audit"]["events"]), len(events))
            bundle = self.controller.snapshot(identifier, chain, artifact, True)
        self.assertEqual(bundle["audit"]["checkpoint"]["eventCount"], len(events))
        self.assertEqual(list(audit.iter_events(bundle["audit"], directory / "audit-events")), events)

    def test_snapshot_keeps_overflow_assignments_and_receipts_without_rewarding_them(self):
        self.continuous_dispatch()
        _, bundle = self.frozen()
        request = bundle["request"]
        journal = bundle["audit"]["journal_id"]
        trust = {"schema_version": 1, "journal_id": journal, "enrollment_block_hash": h(100),
                 "minimum_checkpoint": {"event_count": 0, "event_head": journal, "assignment_cursor": journal},
                 "required_readiness": [], "required_duties": []}
        checked = audit.verify_package(self.f["settings"], self.f["evidence"], request["manifest"],
                    request["subscriptions"], request["duties"], bundle["audit"], trust, self.registry_rpc,
                    fri_payload=self.f["fri_payload"])
        self.assertEqual(len(request["duties"]), 1)
        self.assertEqual(len(request["manifest"]["payload"]["assignments"]), 2)
        self.assertEqual(sum(event["kind"] == "accepted" for event in bundle["audit"]["events"]), 2)
        self.assertEqual(checked["unrewarded_batches"], [2])

    def test_snapshot_checks_native_proof_hash_even_for_uncredited_overflow(self):
        self.continuous_dispatch()
        self.f["fri_payload"]["fri_proofs"][1] = base64.b64encode(bytes([200]) * 96).decode()
        identifier = self.acquired()
        chain, roster_artifact = self.controller.status()
        with self.assertRaisesRegex(s.Error, "accepted_duty_native_payload_changed"):
            self.controller.snapshot(identifier, chain, roster_artifact, True)

    def test_snapshot_accepts_exact_overflow_work_while_a_peer_remains_unready(self):
        self.continuous_dispatch(unready_peer=True)
        _, bundle = self.frozen()
        self.assertEqual(len(bundle["request"]["duties"]), 1)
        self.assertEqual(sorted(account["offered"] for account in self.dispatcher.state["accounts"].values()), [0, 1])

    def test_automatic_delivery_keeps_native_capabilities_private_and_restart_is_idempotent(self):
        identifier, _ = self.frozen()
        first = self.controller.step(True)
        self.assertEqual(first["next_action"], "await_selected_wrapper")
        calls = len(self.wallet.calls)
        second = self.controller_reload().step(True)
        self.assertEqual(first, second)
        self.assertEqual(calls, len(self.wallet.calls))
        self.assertEqual(len(self.native.calls), 2)
        work_dir = Path(self.controller.state["operations"][identifier]["works"][first["work_hash"]]["directory"])
        encoded = (work_dir / "work.json").read_bytes() + (work_dir / "payload.json").read_bytes()
        for secret in (b"lease_token", h(180).encode(), b"user:password", b"Basic "):
            self.assertNotIn(secret, encoded)
        self.assertEqual(io.validate_work(self.f["settings"], io.private_json(work_dir / "work.json"),
                                          io.private_json(work_dir / "payload.json")), first["work_hash"])

    def test_turn_change_rebinds_same_frozen_report_without_new_native_pick(self):
        identifier, bundle = self.frozen()
        first = self.controller.step(True)
        self.rpc.turn += 1
        second = self.controller_reload().step(True)
        self.assertNotEqual(first["work_hash"], second["work_hash"])
        works = self.controller.state["operations"][identifier]["works"]
        bodies = [io.private_json(Path(meta["directory"]) / "work.json")["body"] for meta in works.values()]
        self.assertEqual([body["request"]["duties"] for body in bodies], [bundle["request"]["duties"]] * 2)
        self.assertEqual([body["audit"] for body in bodies], [bundle["audit"]] * 2)
        self.assertEqual(len(self.native.calls), 2)

    def test_returned_proof_native_submission_relay_and_confirmed_node_handoff(self):
        identifier, _ = self.frozen()
        work = self.controller.step(True)
        prepared = self.returned(work)
        self.native.responses.append((204, {"x-syscoin-prover-disposition": "accepted"}, b""))
        self.assertEqual(self.controller_reload().step(True)["next_action"], "await_node_publication")
        submission = json.loads(self.native.calls[-1][2])
        self.assertEqual(submission["lease_token"], h(180))
        self.assertEqual(submission["proof"], self.f["snark"]["proof"])
        publication = self.publication(prepared)
        self.rpc.item["digest"] = prepared["sequencer_request"]["digest"]
        staged = self.controller_reload().step(True)
        self.assertEqual(staged["next_action"], "relay_dual_endorsed_package")
        relay = r.Relay(self.f["settings"], self.config["relay_policy"], self.rpc,
                        store=r.Store(self.control_store.root / "relay"))
        self.rpc.item = relay.load_artifact(staged["operation_id"])
        self.assertEqual(self.controller_reload().step(True)["status"], "broadcast")
        self.rpc.accept_receipt()
        # Receipt export must finish even while the operator restores the roster for new work.
        (self.base / "rosters" / "5.json").unlink()
        self.assertEqual(self.controller_reload().step(True)["status"], "mined_unconfirmed")
        self.rpc.advance()
        self.now = self.rpc.now
        result = self.controller_reload().step(True)
        self.assertTrue(result["written"])
        self.assertTrue((publication / "relay.json").is_file())
        self.assertIsNone(self.controller.state["active"])
        self.assertEqual(self.controller.state["operations"][identifier]["status"], "settled")

    def test_one_prover_six_batches_reach_confirmed_settlement_with_four_rewarded_duties(self):
        self.continuous_dispatch(quota=4, batch_count=6)
        identifier, bundle = self.frozen()
        request = bundle["request"]
        self.assertEqual(len(request["duties"]), 4)
        self.assertEqual(len(request["manifest"]["payload"]["assignments"]), 6)
        self.assertEqual(sum(event["kind"] == "accepted" for event in bundle["audit"]["events"]), 6)
        work = self.controller.step(True)
        prepared = self.returned(work)
        self.native.responses.append((204, {"x-syscoin-prover-disposition": "accepted"}, b""))
        self.assertEqual(self.controller_reload().step(True)["next_action"], "await_node_publication")
        submission = json.loads(self.native.calls[-1][2])
        self.assertEqual((submission["from_batch_number"], submission["to_batch_number"]), (1, 6))
        publication = self.publication(prepared)
        self.rpc.item["digest"] = prepared["sequencer_request"]["digest"]
        staged = self.controller_reload().step(True)
        self.assertEqual(staged["next_action"], "relay_dual_endorsed_package")
        relay = r.Relay(self.f["settings"], self.config["relay_policy"], self.rpc,
                        store=r.Store(self.control_store.root / "relay"))
        self.rpc.item = relay.load_artifact(staged["operation_id"])
        self.assertEqual(self.controller_reload().step(True)["status"], "broadcast")
        self.rpc.accept_receipt()
        self.assertEqual(self.controller_reload().step(True)["status"], "mined_unconfirmed")
        self.rpc.advance()
        self.now = self.rpc.now
        self.assertTrue(self.controller_reload().step(True)["written"])
        self.assertTrue((publication / "relay.json").is_file())
        self.assertEqual(self.controller.state["operations"][identifier]["status"], "settled")

    def test_late_proof_is_reused_for_new_turn_endorsement(self):
        identifier, _ = self.frozen()
        work = self.controller.step(True)
        prepared = self.returned(work, bare=True)
        self.publication(prepared)
        self.rpc.turn += 1
        result = self.controller_reload().step(True)
        self.assertEqual(result["next_action"], "await_selected_wrapper")
        source = Path(self.controller.state["operations"][identifier]["works"][result["work_hash"]]["directory"])
        body = io.private_json(source / "work.json")["body"]
        self.assertEqual(body["proof"], self.f["snark"])
        self.assertEqual(body["request"]["proposal"]["accepted_package"]["turn"], self.rpc.turn)
        self.assertEqual(len(self.native.calls), 2)

    def test_same_proof_relay_skips_unusable_endorsement_and_retries_local_verification(self):
        identifier, _ = self.frozen()
        bad = self.controller.step(True)
        prepared = self.returned(bad, endorsement_key="operator")
        self.publication(prepared)
        self.rpc.item["digest"] = prepared["sequencer_request"]["digest"]
        controller = self.controller_reload()
        with patch.object(controller, "sign", wraps=controller.sign) as signer:
            good = controller.step(True)
        self.assertEqual(good["next_action"], "await_selected_wrapper")
        self.assertNotEqual(good["work_hash"], bad["work_hash"])
        self.assertNotIn(prepared["sequencer_request"], [args[0] for args, _ in signer.call_args_list])
        op = controller.state["operations"][identifier]
        selected = copy.deepcopy(op["proof"])
        self.assertIsNone(op["relay"])
        self.assertEqual(self.returned(good), prepared)
        controller.result(identifier, True)
        def bad_first(identifier, page):
            items = c.Coordinator.wrapper_work_items(controller, identifier, page)
            return sorted(items, key=lambda entry: entry[0] != bad["work_hash"])
        verify = s.verify_eoa
        def unavailable(request, signature):
            if request == prepared["wrapper_request"]:
                raise s.Error("cast_failed")
            return verify(request, signature)
        with patch.object(controller, "wrapper_work_items", side_effect=bad_first), \
                patch.object(s, "verify_eoa", side_effect=unavailable), \
                patch.object(controller, "sign", wraps=controller.sign) as signer:
            self.assertEqual(controller.step(True)["next_action"], "await_selected_wrapper")
        self.assertNotIn(prepared["sequencer_request"], [args[0] for args, _ in signer.call_args_list])
        controller = self.controller_reload()
        with patch.object(controller, "wrapper_work_items", side_effect=bad_first), \
                patch.object(controller, "sign", wraps=controller.sign) as signer:
            self.assertEqual(controller.step(True)["next_action"], "relay_dual_endorsed_package")
        self.assertIn(prepared["sequencer_request"], [args[0] for args, _ in signer.call_args_list])
        self.assertEqual(controller.state["operations"][identifier]["proof"], selected)

    def test_ambiguous_pick_blocks_second_acquisition_after_restart(self):
        self.native.responses.append(OSError("connection dropped"))
        with self.assertRaises(OSError):
            self.controller.step(True)
        self.assertEqual(len(self.native.calls), 1)
        with self.assertRaisesRegex(s.Error, "native_pick_uncertain"):
            self.controller_reload().step(True)
        self.assertEqual(len(self.native.calls), 1)

    def test_pre_authority_restart_refreshes_lease_and_preserves_exact_renewal_range(self):
        for renewal in (False, True):
            for window in ("empty", "partial", "complete"):
                with self.subTest(renewal=renewal, window=window):
                    self.control_store = c.initialize(self.base / f"restart-{renewal}-{window}", self.config)
                    self.native, self.now = td.Network(), self.rpc.now
                    self.controller_reload()
                    identifier = self.acquired() if renewal else self.controller.reserve()
                    op = self.controller.state["operations"][identifier]
                    if renewal:
                        op["lease"], op["status"] = None, "picking"
                        self.controller.save()
                    prior_leases = copy.deepcopy(op["leases"])
                    def interrupted_authority(path, value):
                        self.assertEqual(path.name, "authority.json")
                        if window != "empty":
                            raw = c.job.encode(value)
                            if window == "partial":
                                raw = raw[:len(raw) // 2]
                            c.job.write_new(path.with_name(".authority.json." + "a" * 32 + ".tmp"), raw)
                        raise KeyboardInterrupt()
                    with patch.object(c.sentry, "atomic_json", side_effect=interrupted_authority), \
                            self.assertRaises(KeyboardInterrupt):
                        self.controller.acquire(identifier, True)
                    original = copy.deepcopy(op)
                    count = len(self.native.calls)
                    self.rpc.advance(self.config["native_lease_seconds"] + 7)
                    self.now = self.rpc.now
                    self.controller_reload()
                    self.native.responses.append((200, {}, {**self.f["fri_payload"], "lease_token": h(181)}))
                    if not renewal:
                        self.native.responses.append((200, {}, self.f["evidence"]))
                    request = self.native.request
                    def checked_request(url, *args, **kwargs):
                        if "/pick?" in url:
                            retained = c.read_private_json(self.control_store.root / "coordinator.json")["operations"][identifier]
                            self.assertEqual(retained["lease"], original["lease"])
                            self.assertEqual(retained["leases"][:-1], prior_leases)
                            self.assertEqual(retained["leases"][-1], {"name": original["lease"], "started_at": self.now,
                                "deadline": self.now + self.config["native_lease_seconds"]})
                            authority = c.read_private_json(self.controller.directory(identifier) / original["lease"] / "authority.json")
                            self.assertEqual(authority["status"], "pick_uncertain")
                            self.assertEqual(authority["job_id"], identifier + ":" + original["lease"])
                            query = urllib.parse.parse_qs(urllib.parse.urlsplit(url).query)
                            if renewal:
                                expected = {key: self.f["fri_payload"][key] for key in ("from_batch_number", "to_batch_number")}
                                self.assertEqual(authority["expected_bounds"], expected)
                                self.assertEqual(query["snark_batch_from"], [str(expected["from_batch_number"])])
                                self.assertEqual(query["snark_batch_to"], [str(expected["to_batch_number"])])
                            else:
                                self.assertNotIn("expected_bounds", authority)
                                self.assertNotIn("snark_batch_from", query)
                                self.assertNotIn("snark_batch_to", query)
                        return request(url, *args, **kwargs)
                    with patch.object(self.native, "request", side_effect=checked_request):
                        self.assertIsNone(self.controller.acquire(identifier, True))
                    self.assertEqual(len(self.native.calls), count + (1 if renewal else 2))
                    self.assertEqual(self.controller.state["operations"][identifier]["status"], "working")
                    lease = self.controller.directory(identifier) / original["lease"]
                    self.assertFalse(any(lease.glob("*.tmp")))
                    self.assertEqual(io.private_json(self.controller.directory(identifier) / "payload.json"), self.f["fri_payload"])

    def test_existing_authority_or_unknown_evidence_blocks_pick_restart_and_deadline_refresh(self):
        for leftover in ("authority", "picked-wire.json", "unknown.json"):
            with self.subTest(leftover=leftover):
                self.control_store = c.initialize(self.base / ("blocked-" + leftover), self.config)
                self.native, self.now = td.Network(), 1000
                self.controller_reload()
                identifier = self.controller.reserve()
                publish_authority = c.sentry.atomic_json
                def interrupted_authority(path, value):
                    if leftover == "authority":
                        self.assertEqual(path.name, "authority.json")
                        publish_authority(path, value)
                    raise KeyboardInterrupt()
                with patch.object(c.sentry, "atomic_json", side_effect=interrupted_authority), \
                        self.assertRaises(KeyboardInterrupt):
                    self.controller.acquire(identifier, True)
                original = copy.deepcopy(self.controller.state)
                lease = self.controller.directory(identifier) / original["operations"][identifier]["lease"]
                if leftover != "authority":
                    c.job.write_new(lease / leftover, b"retained-evidence")
                files = {path.name: path.read_bytes() for path in lease.iterdir()}
                self.now += self.config["native_lease_seconds"] + 7
                expected_error = s.Error if leftover == "authority" else c.sentry.Error
                message = "native_pick_uncertain" if leftover == "authority" else "pick_initialization_contains_unknown_artifacts"
                with self.assertRaisesRegex(expected_error, message):
                    self.controller_reload().acquire(identifier, True)
                self.assertEqual(self.controller.state, original)
                self.assertEqual(c.read_private_json(self.control_store.root / "coordinator.json"), original)
                self.assertEqual({path.name: path.read_bytes() for path in lease.iterdir()}, files)
                self.assertEqual(self.native.calls, [])

    def test_expired_known_lease_renews_without_changing_frozen_payload(self):
        identifier, bundle = self.frozen()
        self.controller.step(True)
        self.rpc.advance(601)
        self.rpc.deadline = 2000
        self.rpc.status_overrides[(a(123), "maxProofWorkSeconds()")] = 2000
        self.now = self.rpc.now
        result = self.controller.step(True)
        self.assertEqual(result["next_action"], "renew_expired_native_lease_same_frozen_payload")
        self.native.responses.append((200, {}, {**self.f["fri_payload"], "lease_token": h(181)}))
        self.assertIsNone(self.controller.acquire(identifier, True))
        op = self.controller.state["operations"][identifier]
        self.assertEqual(len(op["leases"]), 2)
        self.assertEqual(op["bundle"], io.digest(bundle))
        self.assertEqual(io.private_json(self.controller.directory(identifier) / "payload.json"), self.f["fri_payload"])

    def test_frozen_range_renews_after_256_empty_attempts_and_interrupted_archival(self):
        identifier, bundle = self.frozen()
        self.controller.step(True)
        op = self.controller.state["operations"][identifier]
        original_leases, original_works = copy.deepcopy(op["leases"]), copy.deepcopy(op["works"])
        directory = self.controller.directory(identifier)
        self.rpc.advance(601)
        self.rpc.deadline = 2000
        self.rpc.status_overrides[(a(123), "maxProofWorkSeconds()")] = 2000
        self.now = self.rpc.now
        self.assertEqual(self.controller.step(True)["next_action"], "renew_expired_native_lease_same_frozen_payload")

        self.native.responses += [(204, {"x-syscoin-prover-pick-outcome": "unleased"}, b"")] * 256
        self.assertEqual(self.controller.acquire(identifier, True), "native_queue_empty")
        # Keep the authenticated chain context fixed while exercising the durable pick path.
        chain = self.controller.status()
        with patch.object(self.controller, "status", return_value=chain), \
                patch.object(self.controller, "dispatcher_for", side_effect=lambda _: nullcontext()):
            for _ in range(254):
                self.assertEqual(self.controller.acquire(identifier, True), "native_queue_empty")
            save = self.controller.save
            def interrupt_after_archive():
                if op["lease"] is None:
                    raise InterruptedError("before clearing the archived lease from the journal")
                save()
            with patch.object(self.controller, "save", side_effect=interrupt_after_archive):
                with self.assertRaises(InterruptedError):
                    self.controller.acquire(identifier, True)
        calls = len(self.native.calls)
        self.assertEqual(self.controller_reload().acquire(identifier, True), "native_queue_empty")
        self.assertEqual(len(self.native.calls), calls)
        op = self.controller.state["operations"][identifier]
        self.assertEqual(op["leases"], original_leases)
        self.assertIsNone(op["lease"])
        archives = sorted(directory.glob("lease-*/attempt.json"))
        self.assertEqual(len(archives), 256)
        for path in archives:
            authority = c.read_private_json(path.parent / "authority.json")
            self.assertEqual(authority["status"], "no_job")
            self.assertEqual(authority["job_id"], identifier + ":" + path.parent.name)
            self.assertEqual(authority["expected_bounds"], {"from_batch_number": 1, "to_batch_number": 2})
        for url, _, _, _ in self.native.calls[-256:]:
            query = urllib.parse.parse_qs(urllib.parse.urlsplit(url).query)
            self.assertEqual((query["snark_batch_from"], query["snark_batch_to"]), (["1"], ["2"]))

        self.native.responses.append((200, {}, {**self.f["fri_payload"], "lease_token": h(181)}))
        self.assertIsNone(self.controller_reload().acquire(identifier, True))
        op = self.controller.state["operations"][identifier]
        self.assertEqual(op["leases"][:-1], original_leases)
        self.assertEqual(op["leases"][-1]["name"], op["lease"])
        self.assertNotIn(directory / op["lease"] / "attempt.json", archives)
        self.assertEqual(len(self.native.calls), 259)
        self.assertTrue(all(path.is_file() for path in archives))
        self.assertEqual(op["bundle"], io.digest(bundle))
        self.assertEqual(op["works"], original_works)
        self.assertEqual(io.private_json(directory / "payload.json"), self.f["fri_payload"])

    def test_frozen_range_renews_after_256_granted_leases_expire_across_restart(self):
        self.config["native_lease_seconds"] = 1
        self.control_store = c.initialize(self.base / "short-lease-control", self.config)
        self.controller_reload()
        identifier, bundle = self.frozen()
        directory = self.controller.directory(identifier)
        chain, artifact = self.controller.status()
        rebound = k.rebind_request(self.controller.settings, bundle["request"], chain, artifact)
        # Keep the open package and wrapper turn fixed while real native leases roll over.
        with patch.object(self.controller, "status", return_value=(chain, artifact)), \
                patch.object(self.controller, "dispatcher_for", side_effect=lambda _: nullcontext()), \
                patch.object(self.controller, "refresh_report", return_value=bundle), \
                patch.object(k, "rebind_request", return_value=rebound), patch.object(self.controller, "prepare"):
            for index in range(256):
                if index:
                    token = f"0x{4096 + index:064x}"
                    self.native.responses.append((200, {}, {**self.f["fri_payload"], "lease_token": token}))
                    self.assertIsNone(self.controller.acquire(identifier, True))
                self.now += 1
                self.assertEqual(self.controller.step(True)["next_action"], "renew_expired_native_lease_same_frozen_payload")

        old_leases = self.controller.state["operations"][identifier]["leases"]
        self.assertEqual(len(old_leases), 256)
        original = {item["name"]: (directory / item["name"] / "authority.json").read_bytes() for item in old_leases}
        self.rpc.advance(seconds=256)
        self.rpc.deadline = 2000
        self.rpc.status_overrides[(a(123), "maxProofWorkSeconds()")] = 2000
        self.native.responses.append((200, {}, {**self.f["fri_payload"], "lease_token": h(181)}))
        self.assertEqual(self.controller_reload().step(True)["next_action"], "await_selected_wrapper")
        op = self.controller.state["operations"][identifier]
        self.assertEqual(len(op["leases"]), 1)
        self.assertNotIn(op["lease"], original)
        self.assertEqual(io.private_json(directory / op["lease"] / "authority.json")["lease_token"], h(181))
        for attempt in old_leases:
            lease = directory / attempt["name"]
            self.assertEqual(io.private_json(lease / "attempt.json"), attempt)
            self.assertEqual((lease / "authority.json").read_bytes(), original[attempt["name"]])
        self.assertEqual(op["bundle"], io.digest(bundle))
        self.assertEqual(io.private_json(directory / "payload.json"), self.f["fri_payload"])

    def test_expired_rejected_lease_archives_only_for_its_frozen_range(self):
        identifier = self.acquired()
        attempt = self.controller.state["operations"][identifier]["leases"][0]
        lease = self.controller.directory(identifier) / attempt["name"]
        self.native.responses.append((422, {"x-syscoin-prover-disposition": "rejected"}, b""))
        self.assertEqual(c.native_handoff.submit(lease, self.f["snark"], "private-auth", self.native), "rejected")
        original = {name: (lease / name).read_bytes() for name in ("authority.json", "submission.json")}
        self.assertFalse(self.controller.archive_finished_attempt(identifier, attempt, (1, 2)))
        self.now = attempt["deadline"]
        with self.assertRaisesRegex(s.Error, "archived_lease_range_changed"):
            self.controller.archive_finished_attempt(identifier, attempt, (2, 3))
        self.assertFalse((lease / "attempt.json").exists())
        self.assertTrue(self.controller.archive_finished_attempt(identifier, attempt, (1, 2)))
        self.assertEqual(io.private_json(lease / "attempt.json"), attempt)
        self.assertEqual({name: (lease / name).read_bytes() for name in original}, original)

    def test_expired_uncertain_submission_retries_the_same_lease_and_bytes(self):
        identifier, _ = self.frozen()
        self.returned(self.controller.step(True))
        self.native.responses.append(OSError("lost response"))
        with self.assertRaises(OSError):
            self.controller_reload().step(True)
        op = self.controller.state["operations"][identifier]
        lease, history = op["lease"], copy.deepcopy(op["leases"])
        directory = self.controller.directory(identifier) / lease
        self.assertEqual(c.read_private_json(directory / "authority.json")["status"], "submission_pending")
        submitted = self.native.calls[-1]
        self.rpc.advance(601)
        self.rpc.deadline = 2000
        self.rpc.status_overrides[(a(123), "maxProofWorkSeconds()")] = 2000
        self.now = self.rpc.now
        self.assertFalse(self.controller.archive_finished_attempt(identifier, history[-1], (1, 2)))
        self.native.responses.append((204, {"x-syscoin-prover-disposition": "accepted"}, b""))
        self.assertEqual(self.controller_reload().step(True)["next_action"], "await_node_publication")
        self.assertEqual(self.native.calls[-1], submitted)
        op = self.controller.state["operations"][identifier]
        self.assertEqual((op["lease"], op["leases"]), (lease, history))
        self.assertFalse((directory / "attempt.json").exists())

    def test_cached_work_signature_does_not_allow_dry_run_to_publish(self):
        identifier, _ = self.frozen()
        result = self.controller.step(True)
        source = Path(self.controller.state["operations"][identifier]["works"][result["work_hash"]]["directory"])
        (source / "work.json").unlink()
        self.assertEqual(self.controller_reload().step(False)["next_action"], "would_deliver_selected_wrapper")
        self.assertFalse((source / "work.json").exists())

    def test_open_maintenance_wallet_recovery_reuses_reserved_transaction(self):
        with patch.object(enrollment, "enrollment_snapshot", wraps=enrollment.enrollment_snapshot) as scan:
            identifier = self.acquired()
            self.assertEqual(scan.call_count, 1)
            authority = self.controller.enrollment
            registry_reads = len(self.registry_rpc.views)
            chain, artifact = self.controller.status()
            self.controller.snapshot(identifier, chain, artifact, True)
            first = self.controller.step(True)
            self.assertEqual(first["status"], "broadcast")
            self.assertEqual(scan.call_count, 1)
            self.assertGreater(len(self.registry_rpc.views), registry_reads)
            self.assertTrue(all(signature == "isRoundFinalized(uint256)"
                                for signature in self.registry_rpc.views[registry_reads:]))
            self.assertIs(self.controller.enrollment, authority)
            transaction = self.rpc.sent[-1]
            transaction_store = c.transactions.Store(self.control_store.root / "transactions")
            reserved = copy.deepcopy(transaction_store.load(self.f["settings"], self.config["transaction_policy"])
                                     ["operations"][first["operation_id"]]["unsigned_transaction"])
            wallet_signatures = [call for call in self.wallet.calls if call[0] == "eth_signTransaction"]
            self.rpc.advance(31)
            self.now = self.rpc.now
            self.controller_reload()
            self.assertIsNone(self.controller.enrollment)
            second = self.controller.step(True)
            self.assertEqual(second["status"], "broadcast")
            self.assertEqual(scan.call_count, 2)
            self.assertIsNot(self.controller.enrollment, authority)
            registry_reads = len(self.registry_rpc.views)
            third = self.controller.step(True)
            self.assertEqual(third["next_action"], "wait_before_identical_rebroadcast")
            self.assertEqual(scan.call_count, 2)
            self.assertTrue(all(signature == "isRoundFinalized(uint256)"
                                for signature in self.registry_rpc.views[registry_reads:]))
            recovered = transaction_store.load(self.f["settings"], self.config["transaction_policy"])["operations"]
            self.assertEqual(list(recovered), [first["operation_id"]])
            self.assertEqual(recovered[first["operation_id"]]["unsigned_transaction"], reserved)
            self.assertEqual([call for call in self.wallet.calls if call[0] == "eth_signTransaction"], wallet_signatures)
        self.assertEqual(self.rpc.sent, [transaction, transaction])
        self.assertEqual(len([call for call in self.wallet.calls if call[0] == "eth_signTransaction"]), 1)

    def check_opening_enrollment_mismatch(self, binding, after_sign):
        identifier = self.acquired()
        chain, artifact = self.controller.status()
        bundle = self.controller.snapshot(identifier, chain, artifact, True)
        registry_reads = len(self.registry_rpc.views)
        opening = {}
        package_call, stage, wallet_call = k.package_call, c.transactions.Transactions.stage, self.wallet.call

        def capture_package(*args, **kwargs):
            opening["request"] = args[2]
            return package_call(*args, **kwargs)

        def change_binding():
            if binding == "pin":
                self.controller.keeper["enrollment"]["block_hash"] = h(199)
            else:
                opening["request"]["subscriptions"][0]["signature"] = "0xabcd"

        def stage_then_change(manager, *args, **kwargs):
            result = stage(manager, *args, **kwargs)
            if not after_sign:
                change_binding()
            return result

        def sign_then_change(method, params):
            result = wallet_call(method, params)
            if after_sign and method == "eth_signTransaction":
                change_binding()
            return result

        with patch.object(k, "package_call", side_effect=capture_package), \
                patch.object(c.transactions.Transactions, "stage", new=stage_then_change), \
                patch.object(self.wallet, "call", side_effect=sign_then_change):
            result = self.controller.step(True)
        expected = "authorization_stale_reserved" if after_sign else "stale_unsigned"
        self.assertEqual(result["status"], expected)
        self.assertEqual(result["next_action"], "reconcile_reserved_nonce_no_replacement" if after_sign
                         else "wait_for_fresh_preflight")
        self.assertEqual(len([call for call in self.wallet.calls if call[0] == "eth_signTransaction"]), int(after_sign))
        self.assertEqual(self.rpc.sent, [])
        self.assertTrue(all(signature == "isRoundFinalized(uint256)"
                            for signature in self.registry_rpc.views[registry_reads:]))
        reason = "enrollment_authority_anchor_mismatch" if binding == "pin" else "enrollment_authority_scope_mismatch"
        with self.assertRaisesRegex(s.Error, reason):
            self.controller.enrollment_for(opening["request"])
        store = c.transactions.Store(self.control_store.root / "transactions")
        operations = store.load(self.controller.settings, self.controller.config["transaction_policy"])["operations"]
        self.assertEqual(len(operations), 1)
        retained = operations[result["operation_id"]]
        if after_sign:
            self.assertIsNotNone(retained["raw_transaction"])
            self.assertEqual(retained["unsigned_transaction"]["nonce"], "0x0")
            bundle["request"] = opening["request"]
            recovered = self.controller.recover_maintenance(identifier, True, bundle, self.f["evidence"], self.f["fri_payload"])
            self.assertEqual(recovered["status"], "authorization_stale_reserved")
            operations = store.load(self.controller.settings, self.controller.config["transaction_policy"])["operations"]
            self.assertEqual(operations, {result["operation_id"]: retained})
            self.assertEqual(len([call for call in self.wallet.calls if call[0] == "eth_signTransaction"]), 1)
            self.assertEqual(self.rpc.sent, [])
            self.assertTrue(all(signature == "isRoundFinalized(uint256)"
                            for signature in self.registry_rpc.views[registry_reads:]))
        else:
            for field in ("unsigned_transaction", "raw_transaction", "transaction_hash"):
                self.assertIsNone(retained[field])

    def test_open_maintenance_rejects_changed_enrollment_pin_before_signing(self):
        self.check_opening_enrollment_mismatch("pin", False)

    def test_open_maintenance_rejects_changed_enrollment_snapshot_before_signing(self):
        self.check_opening_enrollment_mismatch("snapshot", False)

    def test_open_maintenance_rejects_changed_enrollment_pin_after_signing(self):
        self.check_opening_enrollment_mismatch("pin", True)

    def test_open_maintenance_rejects_changed_enrollment_snapshot_after_signing(self):
        self.check_opening_enrollment_mismatch("snapshot", True)

    def test_unresolved_dispatcher_receipt_does_not_freeze_or_sign_snapshot(self):
        identifier = self.acquired()
        operation = next(iter(self.dispatcher.state["operations"].values()))
        operation["status"] = "submission_pending"
        self.dispatcher.save()
        self.native.responses.append((404, {}, b""))
        chain, artifact = self.controller.status()
        with self.assertRaisesRegex(s.Error, "wait_for_dispatcher_range_reconciliation"):
            self.controller.snapshot(identifier, chain, artifact, True)
        self.assertFalse((self.controller.directory(identifier) / "snapshot.json").exists())
        self.assertEqual(self.wallet.calls, [])

    def test_wrapper_journal_precedes_visible_work_and_retries_interrupted_delivery(self):
        identifier, _ = self.frozen()
        inbox = self.base / "inbox"
        def interrupt(*args, **kwargs):
            op = self.controller_reload().state["operations"][identifier]
            self.assertEqual(len(op["works"]), 1)
            source = Path(next(iter(op["works"].values()))["directory"])
            self.assertEqual(source.parent, inbox)
            self.assertFalse((source / "work.json").exists())
            raise InterruptedError("before publication")
        with patch.object(io, "publish_work", side_effect=interrupt):
            with self.assertRaises(InterruptedError):
                self.controller.step(True)
        delivered = self.controller_reload().step(True)
        op = self.controller.state["operations"][identifier]
        self.assertEqual(list(op["works"]), [delivered["work_hash"]])
        self.returned(delivered, bare=True)
        self.rpc.turn += 1
        self.native.responses.append((204, {"x-syscoin-prover-disposition": "accepted"}, b""))
        self.assertEqual(self.controller_reload().step(True)["next_action"], "await_node_publication")

    def test_257th_wrapper_request_is_delivered_and_its_response_survives_restart(self):
        identifier, _ = self.frozen()
        first = self.controller.step(True)
        operator = first["operator"]
        inbox = self.base / "inbox"
        for index in range(255):
            work_hash = h(index + 1)
            self.assertTrue(self.controller.record_wrapper_work(identifier, work_hash, inbox / work_hash[2:], operator))
        self.controller.save()
        self.rpc.turn += 1
        work = self.controller_reload().step(True)
        op = self.controller.state["operations"][identifier]
        self.assertEqual(list(op["works"]), [work["work_hash"]])
        history = io.private_json(self.controller.directory(identifier) / "wrapper-work-0.json")
        self.assertEqual(len(history), 256)
        self.assertIn(first["work_hash"], history)
        self.returned(work, bare=True)
        self.native.responses.append((204, {"x-syscoin-prover-disposition": "accepted"}, b""))
        self.assertEqual(self.controller_reload().step(True)["next_action"], "await_node_publication")
        self.assertTrue(self.controller.state["operations"][identifier]["works"][work["work_hash"]]["checked"])

    def test_late_archived_wrapper_proof_is_checked_across_restart(self):
        identifier, _ = self.frozen()
        with patch.object(c, "WRAPPER_WORK_PAGE_SIZE", 1):
            first = self.controller.step(True)
            path = self.base / "inbox" / first["work_hash"][2:] / "result.json"
            for _ in range(2):
                self.rpc.turn += 1
                self.controller_reload().step(True)
            self.controller_reload().result(identifier, True)
            op = self.controller.state["operations"][identifier]
            self.assertEqual((op["work_pages"], op["work_cursor"]), (2, 1))
            result = {"schema_version": 1, "work_hash": first["work_hash"], "proof": self.f["snark"],
                      "prepared": None, "wrapper_signature": "0x"}
            io.immutable_json(path, {"result": result, "operator_signature": sign(
                io.result_request(self.f["settings"], first["work_hash"], result, first["operator"]), "wrapper")})
            self.controller_reload().result(identifier, True)
            self.assertIsNone(self.controller.state["operations"][identifier]["proof"])
            self.native.responses.append((204, {"x-syscoin-prover-disposition": "accepted"}, b""))
            self.assertEqual(self.controller_reload().step(True)["next_action"], "await_node_publication")
            history = io.private_json(self.controller.directory(identifier) / "wrapper-work-0.json")
            self.assertTrue(history[first["work_hash"]]["checked"])

    def test_restart_submits_selected_archived_proof_with_uncertain_competing_result(self):
        identifier, _ = self.frozen()
        selected = self.controller.step(True)
        self.returned(selected, bare=True)
        inbox_result = self.base / "inbox" / selected["work_hash"][2:] / "result.json"
        response = io.private_json(inbox_result)
        inbox_result.unlink()
        with patch.object(c, "WRAPPER_WORK_PAGE_SIZE", 1):
            self.rpc.turn += 1
            competing = self.controller_reload().step(True)
        io.immutable_json(inbox_result, response)
        alternate = {**self.f["snark"], "proof": base64.b64encode(
            b"distinct-valid-wrapper-fixture".ljust(c.proof_check.SNARK_BYTES, b"\0")).decode()}
        directory = self.controller.directory(identifier)
        proof_path = directory / "proof.json"
        write = io.immutable_json
        def interrupt(path, value, *args, **kwargs):
            if path == proof_path:
                raise InterruptedError("before selected proof materialization")
            return write(path, value, *args, **kwargs)
        with patch.object(io, "immutable_json", side_effect=interrupt), self.assertRaises(InterruptedError):
            self.controller_reload().result(identifier, True)
        op = self.controller_reload().state["operations"][identifier]
        self.assertEqual(op["proof"], {"work_hash": selected["work_hash"], "result_sha256": io.digest(response["result"]),
                                      "proof_sha256": io.digest(self.f["snark"])})
        self.assertFalse(proof_path.exists())
        self.assertFalse(io.private_json(directory / "wrapper-work-0.json")[selected["work_hash"]]["checked"])
        inbox_result.unlink()
        with patch.dict(self.f, snark=alternate):
            self.returned(competing, bare=True)
        statements, alternate_bytes, _ = c.proof_check.inputs(
            self.f["settings"], self.f["evidence"], self.f["fri_payload"], alternate)
        alternate_calldata = c.proof_check.verifier_calldata(statements, alternate_bytes)
        original = self.rpc.call
        def call(method, params):
            if method == "eth_call" and params[0]["data"] == alternate_calldata:
                raise r.RpcError(3, "0x")
            return original(method, params)
        with patch.object(self.rpc, "call", side_effect=call) as rpc_call:
            self.assertEqual(self.controller_reload().step(False)["next_action"], "would_submit_verified_native_snark")
            self.assertFalse(proof_path.exists())
            self.native.responses.append((204, {"x-syscoin-prover-disposition": "accepted"}, b""))
            self.assertEqual(self.controller_reload().step(True)["next_action"], "await_node_publication")
        self.assertFalse(any(method == "eth_call" and params[0]["data"] == alternate_calldata
                             for (method, params), _ in rpc_call.call_args_list))
        self.assertEqual(io.private_json(proof_path), self.f["snark"])
        self.assertEqual(json.loads(self.native.calls[-1][2])["proof"], self.f["snark"]["proof"])
        self.assertTrue(io.private_json(directory / "wrapper-work-0.json")[selected["work_hash"]]["checked"])
        competitor = self.controller.state["operations"][identifier]["works"][competing["work_hash"]]
        self.assertTrue(competitor["ignored"])
        self.assertNotIn("error", competitor)

    def test_invalid_old_result_cannot_starve_current_valid_proof(self):
        identifier, _ = self.frozen()
        old = self.controller.step(True)
        old_path = Path(self.controller.state["operations"][identifier]["works"][old["work_hash"]]["directory"])
        io.immutable_json(old_path / "result.json", {"malformed": True})
        self.rpc.turn += 1
        current = self.controller_reload().step(True)
        self.returned(current)
        self.native.responses.append((204, {"x-syscoin-prover-disposition": "accepted"}, b""))
        self.assertEqual(self.controller_reload().step(True)["next_action"], "await_node_publication")
        self.assertTrue(self.controller.state["operations"][identifier]["works"][old["work_hash"]]["ignored"])

    def test_restart_submits_same_wrapper_result_after_envelope_verifier_timeout(self):
        identifier, _ = self.frozen()
        work = self.controller.step(True)
        self.returned(work)
        result_path = self.base / "inbox" / work["work_hash"][2:] / "result.json"
        signed_bytes = result_path.read_bytes()
        signature = json.loads(signed_bytes)["operator_signature"]
        native_calls = list(self.native.calls)
        verifier_calls = []
        run = subprocess.run

        def timeout_on_envelope(command, *args, **kwargs):
            if command[:3] == ["cast", "wallet", "verify"] and command[-1] == signature:
                verifier_calls.append(command)
                raise subprocess.TimeoutExpired(command, kwargs["timeout"])
            return run(command, *args, **kwargs)

        with patch.object(subprocess, "run", side_effect=timeout_on_envelope):
            pending = self.controller.step(True)
            self.assertEqual((pending["next_action"], pending["work_hash"]), ("await_selected_wrapper", work["work_hash"]))
        self.assertEqual(len(verifier_calls), 1)
        op = self.controller_reload().state["operations"][identifier]
        self.assertEqual(list(op["works"]), [work["work_hash"]])
        self.assertFalse(op["works"][work["work_hash"]]["checked"])
        self.assertIsNone(op["proof"])
        self.assertEqual(self.native.calls, native_calls)

        self.native.responses.append((204, {"x-syscoin-prover-disposition": "accepted"}, b""))
        self.assertEqual(self.controller.step(True)["next_action"], "await_node_publication")
        op = self.controller_reload().state["operations"][identifier]
        self.assertEqual(op["proof"]["work_hash"], work["work_hash"])
        self.assertEqual(list(op["works"]), [work["work_hash"]])
        self.assertTrue(op["works"][work["work_hash"]]["checked"])
        self.assertEqual(result_path.read_bytes(), signed_bytes)
        self.assertEqual(self.native.calls[:-1], native_calls)
        self.assertEqual(json.loads(self.native.calls[-1][2])["proof"], self.f["snark"]["proof"])

    def test_transient_read_of_older_result_allows_newer_result_in_same_poll(self):
        identifier, _ = self.frozen()
        old = self.controller.step(True)
        self.rpc.turn += 1
        current = self.controller_reload().step(True)
        self.returned(old)
        self.returned(current)
        controller = self.controller_reload()
        old_result = self.base / "inbox" / old["work_hash"][2:] / "result.json"
        current_result = self.base / "inbox" / current["work_hash"][2:] / "result.json"
        attempts = []
        read = s.read

        def old_first(identifier, page):
            items = c.Coordinator.wrapper_work_items(controller, identifier, page)
            return sorted(items, key=lambda item: item[0] != old["work_hash"])

        def temporary_read_failure(path, *args, **kwargs):
            if path in (old_result, current_result):
                attempts.append(path)
            if path == old_result:
                raise BlockingIOError("wrapper result temporarily unavailable")
            return read(path, *args, **kwargs)

        self.native.responses.append((204, {"x-syscoin-prover-disposition": "accepted"}, b""))
        with patch.object(controller, "wrapper_work_items", side_effect=old_first), \
                patch.object(s, "read", side_effect=temporary_read_failure):
            self.assertEqual(controller.step(True)["next_action"], "await_node_publication")
        self.assertEqual(attempts, [old_result, current_result])
        op = self.controller_reload().state["operations"][identifier]
        self.assertFalse(op["works"][old["work_hash"]]["checked"])
        self.assertTrue(op["works"][current["work_hash"]]["checked"])
        self.assertEqual(op["proof"]["work_hash"], current["work_hash"])

    def test_wrong_outer_signer_is_definitively_ignored_without_selecting_a_proof(self):
        identifier, _ = self.frozen()
        work = self.controller.step(True)
        self.returned(work, envelope_key="operator")
        self.controller.result(identifier, True)
        op = self.controller_reload().state["operations"][identifier]
        meta = op["works"][work["work_hash"]]
        self.assertEqual((meta["checked"], meta["ignored"], meta["error"]), (True, True, "invalid_authenticated_result"))
        self.assertIsNone(op["proof"])
        self.assertEqual(self.rpc.verifications, [])
        with patch.object(io, "validate_result", side_effect=AssertionError("definitive rejection was retried")):
            self.assertIsNone(self.controller.result(identifier, True))

    def test_canonical_revert_is_retired_across_restart_and_turn_rotation(self):
        identifier, _ = self.frozen()
        old = self.controller.step(True)
        self.returned(old, bare=True)
        self.rpc.verification_error = r.RpcError(3, next(iter(c.proof_check.NATIVE_PROOF_REJECTIONS)))
        self.controller.step(True)
        meta = self.controller.state["operations"][identifier]["works"][old["work_hash"]]
        self.assertEqual((meta["checked"], meta["ignored"], meta["error"]),
                         (True, True, "native_snark_rejected"))
        self.rpc.verification_error = None
        self.rpc.turn += 1
        current = self.controller_reload().step(True)
        self.assertEqual(current["next_action"], "await_selected_wrapper")
        self.assertNotEqual(current["work_hash"], old["work_hash"])
        self.assertTrue(self.controller.state["operations"][identifier]["works"][old["work_hash"]]["checked"])

    def test_old_canonical_revert_does_not_block_new_verified_proof_submission(self):
        identifier, _ = self.frozen()
        old = self.controller.step(True)
        self.rpc.turn += 1
        current = self.controller_reload().step(True)
        original_proof = self.f["snark"]
        self.f["snark"] = {**original_proof, "proof": base64.b64encode(
            b"old-result-test-fixture".ljust(c.proof_check.SNARK_BYTES, b"\0")).decode()}
        self.returned(old, bare=True)
        old_calldata = c.proof_check.verifier_calldata(
            [entry["statement"] for entry in s.validate_evidence(self.f["settings"], self.f["evidence"]).values()],
            s.decode_proof(self.f["snark"]["proof"]))
        self.f["snark"] = original_proof
        self.returned(current)
        original = self.rpc.call
        verifications = 0
        def call(method, params):
            nonlocal verifications
            if method == "eth_call" and params[0]["data"].startswith(
                    "0x" + r.selector(c.proof_check.VERIFY_SIGNATURE).hex()):
                verifications += 1
                if params[0]["data"] == old_calldata:
                    raise r.RpcError(3, next(iter(c.proof_check.NATIVE_PROOF_REJECTIONS)))
            return original(method, params)
        self.native.responses.append((204, {"x-syscoin-prover-disposition": "accepted"}, b""))
        with patch.object(self.rpc, "call", side_effect=call):
            self.assertEqual(self.controller_reload().step(True)["next_action"], "await_node_publication")
        self.assertEqual(verifications, 2)
        self.assertTrue(self.controller.state["operations"][identifier]["works"][old["work_hash"]]["ignored"])
        self.assertIsNotNone(self.controller.state["operations"][identifier]["proof"])

    def test_canonical_revert_does_not_block_expired_known_lease_renewal(self):
        identifier, bundle = self.frozen()
        work = self.controller.step(True)
        self.returned(work, bare=True)
        self.rpc.verification_error = r.RpcError(3, next(iter(c.proof_check.NATIVE_PROOF_REJECTIONS)))
        self.rpc.advance(601)
        self.rpc.deadline = 2000
        self.rpc.status_overrides[(a(123), "maxProofWorkSeconds()")] = 2000
        self.now = self.rpc.now
        result = self.controller_reload().step(True)
        self.assertEqual(result["next_action"], "renew_expired_native_lease_same_frozen_payload")
        op = self.controller.state["operations"][identifier]
        self.assertTrue(op["works"][work["work_hash"]]["ignored"])
        self.assertEqual(op["bundle"], io.digest(bundle))
        self.assertEqual(io.private_json(self.controller.directory(identifier) / "payload.json"), self.f["fri_payload"])

    def test_uncertain_revert_keeps_result_retryable(self):
        identifier, _ = self.frozen()
        work = self.controller.step(True)
        self.returned(work, bare=True)
        original = copy.deepcopy(self.controller.state)
        with patch.dict(self.rpc.status_overrides, {(self.f["settings"]["chain_address"], "getVerifier()"): a(250)}):
            with self.assertRaisesRegex(s.Error, "native_verifier_changed"):
                self.controller.result(identifier, True)
            self.assertEqual(self.controller_reload().state, original)
        for error in (r.RpcError(3), r.RpcError(3, "0x"), s.Error("rpc_transport_failure")):
            with self.subTest(error=error):
                self.rpc.verification_error = error
                self.assertIsNone(self.controller.result(identifier, True))
                self.assertEqual(self.controller.state, original)
        self.rpc.turn += 1
        following = self.controller_reload().step(True)
        self.assertEqual(following["next_action"], "await_selected_wrapper")
        self.assertNotEqual(following["work_hash"], work["work_hash"])
        self.assertFalse(self.controller.state["operations"][identifier]["works"][work["work_hash"]]["checked"])
        self.rpc.verification_error = None
        self.controller_reload().result(identifier, True)
        self.assertTrue(self.controller.state["operations"][identifier]["works"][work["work_hash"]]["checked"])
        self.assertIsNotNone(self.controller.state["operations"][identifier]["proof"])

    def test_dry_run_canonical_revert_does_not_write_or_retire_result(self):
        identifier, _ = self.frozen()
        work = self.controller.step(True)
        self.returned(work, bare=True)
        original = copy.deepcopy(self.controller.state)
        before = {str(path): path.read_bytes() for path in self.base.rglob("*") if path.is_file()}
        self.rpc.verification_error = r.RpcError(3, next(iter(c.proof_check.NATIVE_PROOF_REJECTIONS)))
        self.assertIsNone(self.controller.result(identifier, False))
        self.assertEqual(self.controller.state, original)
        self.assertEqual(before, {str(path): path.read_bytes() for path in self.base.rglob("*") if path.is_file()})

    def test_signed_wrong_range_result_is_rejected_without_rpc_verification(self):
        identifier, _ = self.frozen()
        work = self.controller.step(True)
        meta = self.controller.state["operations"][identifier]["works"][work["work_hash"]]
        result = {"schema_version": 1, "work_hash": work["work_hash"],
                  "proof": {**self.f["snark"], "to_batch_number": 3}, "prepared": None, "wrapper_signature": "0x"}
        io.immutable_json(Path(meta["directory"]) / "result.json", {"result": result,
            "operator_signature": sign(io.result_request(self.f["settings"], work["work_hash"], result,
                                                           meta["operator"]), "wrapper")})
        self.controller_reload().step(True)
        self.assertTrue(self.controller.state["operations"][identifier]["works"][work["work_hash"]]["ignored"])
        self.assertEqual(self.rpc.verifications, [])

    def test_known_empty_picks_are_archived_without_consuming_active_journal_capacity(self):
        self.native.responses.append((204, {"x-syscoin-prover-pick-outcome": "unleased"}, b""))
        self.assertEqual(self.controller.step(True)["next_action"], "native_queue_empty")
        self.assertIsNone(self.controller.state["active"])
        self.assertEqual(self.controller.state["operations"], {})
        self.assertEqual(len(list((self.control_store.root / "jobs").glob("*/completed.json"))), 1)

    def test_existing_next_period_journal_is_selected_without_replacing_trusted_anchor(self):
        subscriptions = copy.deepcopy(self.f["subscriptions"])
        for signed in subscriptions:
            signed["subscription"] = {key: signed["subscription"][key] for key, _ in s.SUBSCRIPTION}
            signed["subscription"].update(firstPeriod=6, lastPeriod=6)
            signed["signature"] = sign(s.subscription_request(self.f["settings"], signed["subscription"]), "account")
        class NextPeriodRpc(td.Rpc):
            def call(self, method, params):
                if method in ("eth_getBlockByNumber", "eth_getBlockByHash"):
                    return {"hash": h(100), "timestamp": "0x28a"}
                return super().call(method, params)
        registry_rpc = NextPeriodRpc(self.f["settings"], subscriptions)
        periods = io.directory(self.root.resolve() / "periods", create=True)
        child = td.d.initialize(periods / "6-service", self.f["settings"], subscriptions, 6,
                                 self.config["endpoint"], registry_rpc)
        self.controller.registry_rpc = registry_rpc
        original = copy.deepcopy(self.controller.keeper["enrollment"])
        for directory in (self.root.resolve(), child.root):
            with self.subTest(directory=directory):
                self.controller.config["dispatcher_dir"] = str(directory)
                with self.controller.dispatcher_for({"roster": {"period": 6}}) as dispatch:
                    self.assertEqual(dispatch.store.root, child.root)
                    self.assertEqual(dispatch.state["period"], 6)
        self.assertEqual(self.controller.keeper["enrollment"], original)
        self.controller.keeper["enrollment"]["block_hash"] = h(101)
        with self.assertRaisesRegex(s.Error, "unknown_period_journal"):
            with self.controller.dispatcher_for({"roster": {"period": 6}}):
                pass
        self.assertEqual(sorted(path.name for path in periods.iterdir()), ["6-service"])

    def test_missing_next_period_journal_is_not_created_by_coordinator(self):
        with self.assertRaisesRegex(s.Error, "unknown_period_journal"):
            with self.controller.dispatcher_for({"roster": {"period": 6}}):
                pass
        self.assertFalse((self.root / "periods").exists())

    def test_same_nonce_account_is_rejected(self):
        bad = copy.deepcopy(self.config)
        bad["relay_policy"]["account"] = self.f["settings"]["sequencer"]
        with self.assertRaisesRegex(s.Error, "separate_relay_nonce_account"):
            c.configuration(bad)


class CoordinatorMaintenanceTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.base_fixture = fixture()
        cls.base_keeper, _, _ = setup(cls.base_fixture)
        cls.base_keeper["policy"]["expected_operator"] = cls.base_fixture["settings"]["sequencer"]
        cls.base_policy = relay_policy(cls.base_fixture["settings"])

    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.base = Path(temporary.name).resolve()
        self.f = copy.deepcopy(self.base_fixture)
        keeper, policy = copy.deepcopy(self.base_keeper), copy.deepcopy(self.base_policy)
        for name in ("dispatcher", "rosters", "inbox", "publications"):
            io.directory(self.base / name, create=True)
        snark_release = release("SNARK")
        snark_release["vk_hash"] = self.f["settings"]["vk_hash"]
        io.immutable_json(self.base / "release.json", snark_release)
        self.config = {"schema_version": 1, "keeper": keeper, "endpoint": "https://trusted.example/",
            "native_auth_file": str(self.base / "auth"), "release_file": str(self.base / "release.json"),
            "native_lease_seconds": 600, "dispatcher_dir": str(self.base / "dispatcher"), "roster_dir": str(self.base / "rosters"),
            "operator_inboxes": {self.f["proposal"]["candidate"]["operator"]: str(self.base / "inbox")},
            "publication_dir": str(self.base / "publications"),
            "sequencer_wallet_file": str(self.base / "sequencer-wallet"), "relay_wallet_file": str(self.base / "relay-wallet"),
            "transaction_policy": {**policy, "account": self.f["settings"]["sequencer"]}, "relay_policy": policy,
            "sequencer_beneficiary": self.f["proposal"]["accepted_package"]["sequencerBeneficiary"], "poll_interval_seconds": 1}
        self.start("control")

    def start(self, name):
        self.store = c.initialize(self.base / name, self.config)
        self.rpc = tt.Rpc(self.f["settings"], self.config["transaction_policy"])
        self.rpc.store = c.transactions.Store(self.store.root / "transactions")
        self.wallet = tt.Wallet(self.f["settings"], self.config["transaction_policy"], self.rpc.store)
        self.reload()
        self.identifier = self.controller.reserve()

    def reload(self):
        self.controller = c.Coordinator(self.store, self.rpc, self.wallet, clock=lambda: self.rpc.now)
        return self.controller

    def observe(self):
        return k.maintenance(self.config["keeper"], self.rpc, "observe-native", {}, self.rpc.now)

    def pending(self):
        return self.controller.state["operations"][self.identifier]["maintenance"]

    def maintain(self, producer=None):
        producer = producer or self.observe
        original_stage = c.transactions.Transactions.stage
        def stage(manager, *args, **kwargs):
            identifier = original_stage(manager, *args, **kwargs)
            self.rpc.operation_id = identifier
            return identifier
        with patch.object(c.transactions.Transactions, "stage", new=stage):
            return self.controller.maintenance(self.identifier, producer(), producer, True)

    def recover(self, execute=True):
        pending = self.pending()
        if pending is not None:
            self.rpc.operation_id = pending["operation_id"]
        return self.controller.recover_maintenance(self.identifier, execute, None, self.f["evidence"], self.f["fri_payload"])

    def confirm(self, status=1):
        self.rpc.mine(status)
        self.rpc.advance()
        self.reload()
        self.assertIsNone(self.recover())
        self.assertIsNone(self.pending())
        self.rpc.receipt = None

    def signatures(self):
        return [args[0] for method, args in self.wallet.calls if method == "eth_signTransaction"]

    def test_successive_native_frontiers_survive_coordinator_restart(self):
        identifiers = []
        for observed, tip in ((100, 110), (110, 112), (112, 123)):
            self.rpc.observed_batch, self.rpc.native_batch, self.rpc.native_hash = observed, tip, h(tip)
            self.assertEqual(self.maintain()["status"], "broadcast")
            pending = copy.deepcopy(self.pending())
            identifiers.append(pending["operation_id"])
            self.reload()
            self.assertEqual(self.pending(), pending)
            self.confirm()
            self.rpc.observed_batch = tip
            with self.assertRaisesRegex(s.Error, "no_forward_native_advance"):
                self.observe()
        self.assertEqual(len(set(identifiers)), 3)
        self.assertEqual([s.uint(tx["nonce"]) for tx in self.signatures()], [0, 1, 2])
        self.assertEqual(len(self.rpc.sent), 3)

    def test_descriptor_is_durable_before_stage_and_reconstructs_after_either_crash(self):
        original_stage = c.transactions.Transactions.stage
        for after_stage in (False, True):
            with self.subTest(after_stage=after_stage):
                self.start("crash-" + str(after_stage))
                def interrupted(manager, *args, **kwargs):
                    disk = io.private_json(self.store.root / "coordinator.json")["operations"][self.identifier]["maintenance"]
                    self.assertEqual(disk["call"], args[0])
                    self.assertEqual(disk["invocation"], args[1])
                    if after_stage:
                        original_stage(manager, *args, **kwargs)
                    raise KeyboardInterrupt()
                with patch.object(c.transactions.Transactions, "stage", new=interrupted), self.assertRaises(KeyboardInterrupt):
                    self.controller.maintenance(self.identifier, self.observe(), self.observe, True)
                pending = copy.deepcopy(self.pending())
                self.assertEqual(self.signatures(), [])
                self.assertEqual(len(self.rpc.store.load(self.f["settings"], self.config["transaction_policy"])["operations"]), int(after_stage))
                self.reload()
                before = {str(path): path.read_bytes() for path in self.store.root.rglob("*") if path.is_file()}
                self.recover(False)
                self.assertEqual(before, {str(path): path.read_bytes() for path in self.store.root.rglob("*") if path.is_file()})
                self.assertEqual(self.recover()["status"], "broadcast")
                self.assertEqual(self.pending(), pending)
                self.reload()
                self.assertEqual(self.recover()["status"], "broadcast")
                self.assertEqual(len(self.signatures()), 1)
                self.assertEqual(len(self.rpc.sent), 1)
                self.assertEqual(len(self.rpc.store.load(self.f["settings"], self.config["transaction_policy"])["operations"]), 1)

    def test_stale_unsigned_descriptor_keeps_scope_during_restart(self):
        with patch.object(c.transactions.Transactions, "stage", side_effect=KeyboardInterrupt()), self.assertRaises(KeyboardInterrupt):
            self.controller.maintenance(self.identifier, self.observe(), self.observe, True)
        old = copy.deepcopy(self.pending())
        self.rpc.native_hash = h(152)
        self.reload()
        self.assertIsNone(self.recover())
        self.assertEqual(self.signatures(), [])
        self.assertIsNone(self.pending())
        self.assertEqual(self.maintain()["status"], "broadcast")
        self.assertNotEqual(self.pending()["operation_id"], old["operation_id"])
        old_row = self.rpc.store.load(self.f["settings"], self.config["transaction_policy"])["operations"][old["operation_id"]]
        self.assertEqual(old_row["status"], "stale_unsigned")
        self.assertIsNone(old_row["unsigned_transaction"])
        self.assertEqual(len(self.signatures()), 1)

    def test_ambiguous_send_changed_scope_keeps_nonce_until_canonical_receipt(self):
        self.rpc.send_error = s.Error("rpc_transport_failure")
        self.assertEqual(self.maintain()["status"], "send_uncertain")
        pending = copy.deepcopy(self.pending())
        self.rpc.send_error, self.rpc.native_hash = None, h(152)
        self.rpc.advance()
        self.reload()
        self.assertEqual(self.recover()["status"], "authorization_stale_reserved")
        self.assertEqual(self.pending(), pending)
        self.assertEqual(len(self.signatures()), 1)
        self.assertEqual(len(self.rpc.sent), 1)
        self.confirm()
        self.assertEqual(self.maintain()["status"], "broadcast")
        self.assertEqual([s.uint(tx["nonce"]) for tx in self.signatures()], [0, 1])

    def test_orphaned_reserved_native_observation_recovers_original_descriptor(self):
        self.assertEqual(self.maintain()["status"], "broadcast")
        reserved = copy.deepcopy(self.pending())
        self.controller.state["operations"][self.identifier]["maintenance"] = None
        self.controller.save()
        self.rpc.native_batch += 1
        self.rpc.advance()
        self.reload()
        self.assertEqual(self.maintain()["status"], "authorization_stale_reserved")
        self.assertEqual(self.pending(), reserved)
        self.assertEqual(len(self.signatures()), 1)
        self.assertEqual(len(self.rpc.sent), 1)
        self.confirm()
        self.assertEqual(self.maintain()["status"], "broadcast")
        self.assertNotEqual(self.pending()["operation_id"], reserved["operation_id"])
        self.assertEqual(len(self.signatures()), 2)

    def test_revert_retry_descriptor_survives_restart_and_reorg_blocks_new_nonce(self):
        self.maintain()
        first = copy.deepcopy(self.pending())
        self.rpc.mine(0)
        included, block_hash = self.rpc.receipt["blockNumber"], self.rpc.receipt["blockHash"]
        self.rpc.advance()
        self.reload()
        self.assertIsNone(self.recover())
        self.rpc.receipt = None
        with patch.object(c.transactions.Transactions, "stage", side_effect=KeyboardInterrupt()), self.assertRaises(KeyboardInterrupt):
            self.controller.maintenance(self.identifier, self.observe(), self.observe, True)
        second = copy.deepcopy(self.pending())
        self.assertEqual(second["invocation"]["generation"], 1)
        self.assertEqual(first["invocation"]["scope"], second["invocation"]["scope"])
        self.rpc.blocks[included]["hash"] = h(99)
        self.reload()
        self.assertIsNone(self.recover())
        self.assertEqual(len(self.signatures()), 1)
        self.assertEqual(self.maintain()["next_action"], "investigate_reserved_nonce_without_receipt")
        self.rpc.blocks[included]["hash"] = block_hash
        self.rpc.receipt = copy.deepcopy(self.rpc.receipts[self.rpc.store.load(self.f["settings"], self.config["transaction_policy"])["operations"][first["operation_id"]]["transaction_hash"]])
        self.reload()
        self.assertIsNone(self.recover())
        self.rpc.receipt = None
        self.assertEqual(self.maintain()["status"], "broadcast")
        self.assertEqual(self.pending(), second)
        self.assertEqual([s.uint(tx["nonce"]) for tx in self.signatures()], [0, 1])


if __name__ == "__main__":
    unittest.main()
