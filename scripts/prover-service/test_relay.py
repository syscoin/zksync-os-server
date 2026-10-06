from contextlib import redirect_stdout
import copy
import io
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import relay as r
import service as s
from test_service import KEYS, fixture, prepare, sign, h, a, abi_type, cast_value, struct_values


class RpcErrorTests(unittest.TestCase):
    def test_transport_retains_revert_bytes_without_exposing_them_in_error_text(self):
        rpc = r.Rpc("http://127.0.0.1:8545")
        response = io.BytesIO(s.canonical({"jsonrpc": "2.0", "id": 1,
            "error": {"code": 3, "message": "private RPC detail", "data": "0x1234"}}))
        response.status = 200
        with patch.object(rpc.opener, "open", return_value=response), self.assertRaises(r.RpcError) as caught:
            rpc.call("eth_call", [])
        self.assertEqual((caught.exception.code, caught.exception.data), (3, "0x1234"))
        self.assertEqual(str(caught.exception), "rpc_rejected_request")

    def test_existing_code_only_callers_remain_compatible(self):
        error = r.RpcError(3)
        self.assertEqual(error.code, 3)
        self.assertIsNone(error.data)
        self.assertEqual(str(error), "rpc_rejected_request")


def relay_policy(settings):
    return {"schema_version": 1, "account": s.cast("wallet", "address", "--private-key", KEYS["account"]).lower(),
            "gate_code_hash": s.keccak(b"gate-code"), "coordinator_code_hash": s.keccak(b"coordinator-code"),
            "priority_guard_code_hash": s.keccak(b"priority-code"),
            "gas_limit": 1000000, "max_fee_per_gas": "0x64", "max_priority_fee_per_gas": "0x2",
            "max_total_fee_wei": hex(100000000), "confirmations": 2, "min_turn_seconds": 30,
            "max_head_age_seconds": 120, "rpc_timeout_seconds": 15,
            "rebroadcast_interval_seconds": 30, "max_broadcast_attempts": 3}


def signed_raw(tx, key=KEYS["account"]):
    fields = r.unsigned_fields(tx)
    digest = s.keccak(b"\x02" + r.rlp_encode(fields))
    signature = s.raw_hex(s.cast("wallet", "sign", "--private-key", key, "--no-hash", digest))
    fields += [r.integer_bytes(signature[64] - 27), r.integer_bytes(int.from_bytes(signature[:32], "big")),
               r.integer_bytes(int.from_bytes(signature[32:64], "big"))]
    return "0x02" + r.rlp_encode(fields).hex()


class FakeWallet:
    def __init__(self, settings):
        self.settings, self.calls, self.mutate, self.interrupt = settings, [], None, False

    def call(self, method, params):
        self.calls.append((method, copy.deepcopy(params)))
        if method == "eth_chainId":
            return self.settings["settlement_chain_id"]
        if method == "eth_signTransaction":
            if self.interrupt:
                raise KeyboardInterrupt
            tx = copy.deepcopy(params[0])
            if self.mutate:
                tx.update(self.mutate)
            return {"raw": signed_raw(tx)}
        raise AssertionError(method)


class FakeRpc:
    def __init__(self, settings, limits, item):
        self.settings, self.limits, self.item = settings, limits, item
        self.calls, self.sent, self.logs = [], [], []
        self.now = 1000
        self.head = {"number": "0x14", "hash": h(110), "timestamp": hex(self.now)}
        self.blocks = {self.head["number"]: copy.deepcopy(self.head)}
        self.nonce, self.pending_nonce, self.receipt = 0, 0, None
        self.send_error, self.send_interrupt, self.simulation_error = None, False, False
        self.parent = item["sidecar"]["accepted_package"]["parent"]
        self.turn = item["sidecar"]["accepted_package"]["turn"]
        self.selected, self.deadline, self.opened = 0, self.now + 200, True
        self.frozen_override, self.chain_override, self.reorg_anchor, self.stage_store = None, None, False, None
        self.guard_work_missing, self.guard_expired, self.witness = False, False, True

    def advance(self, seconds=30, blocks=1):
        self.now += seconds
        number = s.uint(self.head["number"]) + blocks
        self.head = {"number": hex(number), "hash": h(110 + number - 20), "timestamp": hex(self.now)}
        self.blocks[self.head["number"]] = copy.deepcopy(self.head)

    def accept_receipt(self, status=1, logs=True):
        tx_hash = s.keccak(s.raw_hex(self.sent[-1]))
        log = {"address": self.settings["proof_gate"], "topics": r.acceptance_topics(self.item),
               "data": "0x" + s.word(1 if self.item["mode"] == "bootstrap" else 0).hex()}
        self.receipt = {"transactionHash": tx_hash, "from": self.limits["account"], "to": self.settings["proof_gate"],
                        "blockNumber": self.head["number"], "blockHash": self.head["hash"], "status": hex(status),
                        "logs": [log] if status and logs else []}
        self.nonce = self.pending_nonce = 1
        if status:
            self.parent = self.item["package_hash"]

    def call(self, method, params):
        self.calls.append((method, copy.deepcopy(params)))
        if method == "eth_chainId":
            return self.chain_override or self.settings["settlement_chain_id"]
        if method == "eth_getBlockByNumber":
            if params[0] == "latest":
                return copy.deepcopy(self.head)
            value = copy.deepcopy(self.blocks.get(params[0]))
            if self.reorg_anchor and value:
                value["hash"] = h(99)
            return value
        if method == "eth_getCode":
            codes = {self.settings["proof_gate"]: b"gate-code", self.settings["coordinator"]: b"coordinator-code", a(123): b"priority-code"}
            return "0x" + codes[params[0]].hex()
        if method == "eth_getTransactionCount":
            return hex(self.pending_nonce if params[1] == "pending" else self.nonce)
        if method == "eth_getBalance":
            return hex(10**30)
        if method == "eth_getTransactionReceipt":
            return copy.deepcopy(self.receipt)
        if method == "eth_getLogs":
            return copy.deepcopy(self.logs)
        if method == "eth_sendRawTransaction":
            if self.stage_store:
                state = self.stage_store.load(self.settings, self.limits)
                op = next(iter(state["operations"].values()))
                assert op["status"] == "send_uncertain"
                assert op["raw_transaction"] == params[0]
                assert op["transaction_hash"] == s.keccak(s.raw_hex(params[0]))
            self.sent.append(params[0])
            if self.send_interrupt:
                raise KeyboardInterrupt
            if self.send_error:
                raise self.send_error
            return s.keccak(s.raw_hex(params[0]))
        if method == "eth_call":
            tx = params[0]
            s.require(type(params[1]) is dict and params[1].get("requireCanonical") is True, "unanchored_test_call")
            if tx["data"] == self.item["calldata"]:
                if self.simulation_error:
                    raise r.RpcError(3)
                return "0x"
            accepted = self.item["sidecar"]["accepted_package"]
            work_id = s.keccak(s.raw_hex(accepted["parent"]) + s.word(accepted["batchFrom"]))
            if tx["data"] == "0x" + r.selector("work()").hex():
                return "0x" + (s.raw_hex(work_id) + s.raw_hex(h(124)) + s.raw_hex(h(125))
                                + b"".join(s.word(value) for value in (accepted["batchFrom"], 1 if self.guard_expired else 1000,
                                                                        0, 4, 2, 2))).hex()
            normalized = {**accepted, "turn": 0, "proofHash": s.ZERO, "wrapper": s.ZERO_ADDRESS, "wrapperBeneficiary": s.ZERO_ADDRESS}
            values = {
                "chain()": self.settings["chain_address"], "childChainId()": s.uint(self.settings["execution_chain_id"]),
                "sequencer()": self.settings["sequencer"], "policyHash()": self.settings["policy_hash"],
                "productionVkHash()": self.settings["vk_hash"], "lastAcceptedPackage()": self.parent,
                "serviceActive()": self.item["mode"] == "service",
                "coordinator()": self.settings["coordinator"] if self.item["mode"] == "service" else s.ZERO_ADDRESS,
                "acceptanceGate()": self.settings["proof_gate"], "acceptedParent()": self.parent,
                "nextBatch()": accepted["batchFrom"], "packageOpen()": self.opened,
                "frozenPackageHash()": self.frozen_override or s.struct_hash("AcceptedPackageV1", s.PACKAGE, normalized),
                "currentTurn()": self.turn, "selectedWrapperIndex()": self.selected, "turnDeadline()": self.deadline,
                "rosterRoot()": accepted["rosterRoot"],
                "priorityGuard()": a(123), "priorityWorkId()": s.ZERO if self.guard_work_missing else work_id,
                "priorityCursor()": 0, "maxProofWorkSeconds()": 500, "prefixWitness(bytes32)": self.witness,
                "bootstrapDigest(" + r.abi_tuple(s.PACKAGE) + ")": self.item["digest"],
                "packageDigest(" + r.abi_tuple(s.PACKAGE) + ")": self.item["digest"]}
            for signature, value in values.items():
                if tx["data"][:10] == "0x" + r.selector(signature).hex():
                    if type(value) is bool:
                        return "0x" + s.word(int(value)).hex()
                    if type(value) is int:
                        return "0x" + s.word(value).hex()
                    data = s.raw_hex(value)
                    return "0x" + data.rjust(32, b"\0").hex()
            raise AssertionError(tx)
        raise AssertionError(method)


class RelayTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.f = fixture()
        cls.settings = cls.f["settings"]
        cls.limits = r.policy(relay_policy(cls.settings))
        prepared = prepare(cls.f)
        cls.sidecar = s.complete_package(cls.settings, prepared, sign(prepared["sequencer_request"], "sequencer"),
                                         sign(prepared["wrapper_request"], "wrapper"))
        cls.item = r.artifact(cls.settings, cls.sidecar, prepared["proof_data"])

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name) / "relay"
        self.store = r.Store.initialize(self.root, self.settings, self.limits)
        self.rpc = FakeRpc(self.settings, self.limits, self.item)
        self.rpc.stage_store = self.store
        self.wallet = FakeWallet(self.settings)
        self.operation_id = self.relayer().stage(self.item)

    def relayer(self):
        return r.Relay(self.settings, self.limits, self.rpc, self.wallet, self.store, lambda: self.rpc.now)

    def signatures(self):
        return [call for call in self.wallet.calls if call[0] == "eth_signTransaction"]

    def node_work(self):
        accepted = self.item["sidecar"]["accepted_package"]
        return {"schema_version": 1, "execution_chain_id": self.settings["execution_chain_id"],
                "settlement_chain_id": self.settings["settlement_chain_id"],
                "chain_address": self.settings["chain_address"], "gate": self.settings["proof_gate"],
                "gate_code_hash": self.limits["gate_code_hash"], "coordinator": self.settings["coordinator"],
                "coordinator_code_hash": self.limits["coordinator_code_hash"],
                "policy_hash": self.settings["policy_hash"], "production_vk_hash": self.settings["vk_hash"],
                "sequencer": self.settings["sequencer"], "timelock": a(90), "required_confirmations": 3,
                "batch_from": accepted["batchFrom"], "batch_to": accepted["batchTo"],
                "proof_data": self.item["proof_data"], "batch_outputs": self.item["sidecar"]["batch_outputs"]}

    def confirmed_relayer(self):
        self.relayer().step(self.operation_id, True)
        self.rpc.accept_receipt()
        self.rpc.advance()
        self.relayer().step(self.operation_id, True)
        return self.relayer()

    def test_export_handoff_binds_exact_node_work_without_rpc_or_wallet(self):
        relay = self.confirmed_relayer()
        work = Path(self.temp.name).resolve() / "node-work"
        work.mkdir(mode=0o700)
        s.write_new(work / "work.json", self.node_work())
        calls, signatures = len(self.rpc.calls), len(self.wallet.calls)
        preview = relay.export_handoff(self.operation_id, work)
        self.assertFalse(preview["written"])
        self.assertFalse((work / "relay.json").exists())
        result = relay.export_handoff(self.operation_id, work, True)
        hint = s.read_json(work / "relay.json", private=True)
        self.assertTrue(result["written"])
        self.assertEqual(hint["work_hash"], s.keccak((work / "work.json").read_bytes()))
        self.assertEqual(hint["sidecar"], self.item["sidecar"])
        self.assertEqual(hint["transaction_hash"], relay.state["operations"][self.operation_id]["transaction_hash"])
        self.assertEqual((len(self.rpc.calls), len(self.wallet.calls)), (calls, signatures))

    def test_export_handoff_rejects_foreign_work_and_unknown_acceptance_tx(self):
        relay = self.confirmed_relayer()
        work = Path(self.temp.name).resolve() / "node-work"
        work.mkdir(mode=0o700)
        for field, value in (("execution_chain_id", "0x1234"), ("proof_data", "0x00"),
                             ("batch_to", 99), ("gate_code_hash", h(99))):
            document = self.node_work()
            document[field] = value
            r.atomic_json(work / "work.json", document)
            with self.assertRaises(s.Error):
                relay.export_handoff(self.operation_id, work, True)
            self.assertFalse((work / "relay.json").exists())
        r.atomic_json(work / "work.json", self.node_work())
        relay.state["operations"][self.operation_id]["acceptance"]["transaction_hash"] = None
        with self.assertRaisesRegex(s.Error, "acceptance_transaction_hash_required"):
            relay.export_handoff(self.operation_id, work, True)

    def test_external_parent_hint_resolves_accepting_transaction_when_logs_arrive(self):
        self.rpc.parent = self.item["package_hash"]
        self.relayer().step(self.operation_id, True)
        self.assertIsNone(self.relayer().state["operations"][self.operation_id]["acceptance"]["transaction_hash"])
        included = copy.deepcopy(self.rpc.head)
        self.rpc.advance()
        self.rpc.logs = [{"address": self.settings["proof_gate"], "topics": r.acceptance_topics(self.item),
                          "data": "0x" + s.word(0).hex(), "transactionHash": h(90),
                          "blockNumber": included["number"], "blockHash": included["hash"]}]
        result = self.relayer().step(self.operation_id, True)
        self.assertEqual(result["status"], "accepted_elsewhere")
        self.assertEqual(self.relayer().state["operations"][self.operation_id]["acceptance"]["transaction_hash"], h(90))
        self.assertFalse(self.signatures())

    def test_export_handoff_rejects_symlinked_node_directory(self):
        relay = self.confirmed_relayer()
        work = Path(self.temp.name).resolve() / "node-work"
        work.mkdir(mode=0o700)
        s.write_new(work / "work.json", self.node_work())
        link = work.parent / "link"
        link.symlink_to(work, target_is_directory=True)
        with self.assertRaisesRegex(s.Error, "symlink"):
            relay.export_handoff(self.operation_id, link, True)

    def test_gate_calldata_matches_cast_for_both_modes(self):
        sidecar, proof = self.sidecar, self.item["proof_data"]
        fields = s.DUTY + [("operatorSignature", "bytes")]
        signature = f"submit({abi_type(s.PACKAGE)},{abi_type(fields)}[],{abi_type(s.OUTPUT)}[],bytes,{abi_type(s.CANDIDATE)},bytes32[],bytes,bytes)"
        actual = s.cast("calldata", signature, cast_value(struct_values(s.PACKAGE, sidecar["accepted_package"])),
                        cast_value([struct_values(fields, d) for d in sidecar["duties"]]),
                        cast_value([struct_values(s.OUTPUT, output) for output in sidecar["batch_outputs"]]), proof,
                        cast_value(struct_values(s.CANDIDATE, sidecar["candidate"])), "[]", sidecar["sequencer_signature"], sidecar["wrapper_signature"])
        self.assertEqual(actual, self.item["calldata"])
        bootstrap = copy.deepcopy(self.f)
        bootstrap["proposal"].update(mode="bootstrap", candidate=None, candidate_proof=[])
        bootstrap["proposal"]["accepted_package"].update(turn=0, rosterRoot=s.ZERO, wrapper=s.ZERO_ADDRESS, wrapperBeneficiary=s.ZERO_ADDRESS)
        prepared = prepare(bootstrap)
        sidecar = s.complete_package(self.settings, prepared, sign(prepared["sequencer_request"], "sequencer"), "0x")
        signature = f"submitBootstrap({abi_type(s.PACKAGE)},{abi_type(fields)}[],{abi_type(s.OUTPUT)}[],bytes,bytes)"
        actual = s.cast("calldata", signature, cast_value(struct_values(s.PACKAGE, sidecar["accepted_package"])),
                        cast_value([struct_values(fields, d) for d in sidecar["duties"]]),
                        cast_value([struct_values(s.OUTPUT, output) for output in sidecar["batch_outputs"]]), proof, sidecar["sequencer_signature"])
        self.assertEqual(actual, r.gate_calldata(sidecar, proof, "bootstrap"))

    def test_dry_run_never_signs_sends_or_changes_state(self):
        before = (self.root / "state.json").read_bytes()
        result = self.relayer().step(self.operation_id)
        self.assertEqual(result["next_action"], "would_request_wallet_signature")
        self.assertEqual(self.wallet.calls, [])
        self.assertEqual(self.rpc.sent, [])
        self.assertEqual(before, (self.root / "state.json").read_bytes())

    def test_sign_persist_send_and_confirm(self):
        result = self.relayer().step(self.operation_id, execute=True)
        self.assertEqual(result["status"], "broadcast")
        self.assertEqual(len(self.signatures()), 1)
        self.rpc.accept_receipt()
        self.assertEqual(self.relayer().step(self.operation_id, execute=True)["status"], "mined_unconfirmed")
        self.rpc.advance()
        self.assertEqual(self.relayer().step(self.operation_id, execute=True)["status"], "accepted")
        self.assertEqual(len(self.signatures()), 1)
        self.assertEqual(len(self.rpc.sent), 1)

    def test_ambiguous_send_restart_rebroadcasts_only_same_raw(self):
        self.rpc.send_error = s.Error("rpc_transport_failure")
        self.assertEqual(self.relayer().step(self.operation_id, execute=True)["status"], "send_uncertain")
        self.rpc.send_error = None
        self.rpc.advance()
        self.relayer().step(self.operation_id, execute=True)
        self.assertEqual(len(self.signatures()), 1)
        self.assertEqual(self.rpc.sent, [self.rpc.sent[0], self.rpc.sent[0]])

    def test_crash_after_send_intent_keeps_raw_hash_for_recovery(self):
        self.rpc.send_interrupt = True
        with self.assertRaises(KeyboardInterrupt):
            self.relayer().step(self.operation_id, execute=True)
        op = self.store.load(self.settings, self.limits)["operations"][self.operation_id]
        self.assertEqual(op["status"], "send_uncertain")
        self.assertEqual(op["transaction_hash"], s.keccak(s.raw_hex(self.rpc.sent[0])))
        self.rpc.send_interrupt = False
        self.rpc.accept_receipt()
        self.rpc.advance()
        self.assertEqual(self.relayer().step(self.operation_id, execute=True)["status"], "accepted")
        self.assertEqual(len(self.rpc.sent), 1)

    def test_signing_interruption_reuses_identical_unsigned_intent(self):
        self.wallet.interrupt = True
        with self.assertRaises(KeyboardInterrupt):
            self.relayer().step(self.operation_id, execute=True)
        op = self.store.load(self.settings, self.limits)["operations"][self.operation_id]
        self.assertEqual(op["status"], "signing_uncertain")
        self.assertIsNone(op["raw_transaction"])
        self.wallet.interrupt = False
        self.relayer().step(self.operation_id, execute=True)
        self.assertEqual(self.signatures()[0][1], self.signatures()[1][1])
        self.assertEqual(len(self.rpc.sent), 1)

    def test_wallet_changed_destination_or_fees_never_broadcast(self):
        for change in ({"to": a(99)}, {"maxFeePerGas": "0x100000"}, {"data": "0x1234"}, {"chainId": "0x1"}):
            with self.subTest(change=change):
                self.wallet.mutate = change
                with self.assertRaisesRegex(s.Error, "wallet_changed_transaction"):
                    self.relayer().step(self.operation_id, execute=True)
                self.assertEqual(self.rpc.sent, [])

    def test_stale_turn_and_repair_refuse_unsigned_artifact(self):
        for field, value, reason in (("turn", self.rpc.turn + 1, "stale_wrapper_turn"),
                                      ("frozen_override", h(99), "frozen_package_repaired")):
            with self.subTest(field=field):
                setattr(self.rpc, field, value)
                result = self.relayer().step(self.operation_id, execute=True)
                self.assertEqual(result["status"], "stale_unsigned")
                self.assertEqual(result["context"]["reason"], reason)
                self.assertEqual(self.signatures(), [])
                self.rpc.turn = self.item["sidecar"]["accepted_package"]["turn"]
                self.rpc.frozen_override = None

    def test_stale_inflight_never_resigns_or_rebroadcasts(self):
        self.relayer().step(self.operation_id, execute=True)
        self.rpc.turn += 1
        self.rpc.advance()
        result = self.relayer().step(self.operation_id, execute=True)
        self.assertEqual(result["status"], "authorization_stale_inflight")
        self.assertEqual(len(self.signatures()), 1)
        self.assertEqual(len(self.rpc.sent), 1)

    def test_unknown_consumed_nonce_does_not_replace_authorization(self):
        self.relayer().step(self.operation_id, execute=True)
        self.rpc.nonce = self.rpc.pending_nonce = 1
        result = self.relayer().step(self.operation_id, execute=True)
        self.assertEqual(result["status"], "nonce_conflict")
        self.assertEqual(len(self.signatures()), 1)

    def test_acceptance_elsewhere_with_pending_own_nonce_keeps_reservation(self):
        self.relayer().step(self.operation_id, execute=True)
        self.rpc.parent = self.item["package_hash"]
        self.rpc.advance()
        result = self.relayer().step(self.operation_id, execute=True)
        self.assertEqual(result["status"], "accepted_with_pending_nonce")
        self.assertEqual(result["next_action"], "wait_for_own_nonce_receipt")
        self.assertEqual(len(self.rpc.sent), 1)

    def test_external_acceptance_before_signing_finishes_without_wallet(self):
        self.rpc.parent = self.item["package_hash"]
        self.assertEqual(self.relayer().step(self.operation_id, execute=True)["status"], "accepted_unconfirmed")
        self.rpc.advance()
        self.assertEqual(self.relayer().step(self.operation_id, execute=True)["status"], "accepted_elsewhere")
        self.assertEqual(self.wallet.calls, [])

    def test_receipt_reorg_is_not_final_acceptance(self):
        self.relayer().step(self.operation_id, execute=True)
        self.rpc.accept_receipt()
        self.relayer().step(self.operation_id, execute=True)
        self.rpc.blocks[self.rpc.receipt["blockNumber"]]["hash"] = h(99)
        self.rpc.advance()
        self.rpc.parent = self.item["sidecar"]["accepted_package"]["parent"]
        self.rpc.nonce = self.rpc.pending_nonce = 0
        result = self.relayer().step(self.operation_id, execute=True)
        self.assertNotEqual(result["status"], "accepted")
        self.assertIsNone(result["receipt"])
        self.assertIsNone(result["acceptance"])

    def test_success_without_gate_event_and_wrong_network_fail_closed(self):
        self.relayer().step(self.operation_id, execute=True)
        self.rpc.accept_receipt(logs=False)
        with self.assertRaisesRegex(s.Error, "successful_receipt_missing_acceptance"):
            self.relayer().step(self.operation_id, execute=True)
        self.rpc.chain_override = "0x1"
        with self.assertRaisesRegex(s.Error, "wrong_rpc_chain"):
            self.relayer().step(self.operation_id, execute=True)

    def test_simulation_failure_never_requests_wallet_signature(self):
        self.rpc.simulation_error = True
        result = self.relayer().step(self.operation_id, execute=True)
        self.assertEqual(result["next_action"], "wait_for_gate_preflight")
        self.assertEqual(self.wallet.calls, [])

    def test_policy_change_or_artifact_replacement_is_rejected(self):
        limits = {**self.limits, "max_broadcast_attempts": 4}
        with self.assertRaisesRegex(s.Error, "relay_configuration_changed"):
            r.Relay(self.settings, limits, self.rpc, self.wallet, self.store)
        destination = self.root / (self.operation_id + ".json")
        item = copy.deepcopy(self.item)
        item["proof_data"] += "00"
        r.atomic_json(destination, item)
        with self.assertRaisesRegex(s.Error, "relay_artifact_changed"):
            self.relayer().step(self.operation_id, execute=True)

    def test_rejected_receipt_holds_nonce_until_confirmed(self):
        self.relayer().step(self.operation_id, execute=True)
        self.rpc.accept_receipt(status=0)
        self.assertEqual(self.relayer().step(self.operation_id, execute=True)["status"], "mined_unconfirmed")
        self.rpc.advance()
        self.assertEqual(self.relayer().step(self.operation_id, execute=True)["status"], "reverted")

    def test_reorg_during_context_reads_never_signs(self):
        self.rpc.reorg_anchor = True
        with self.assertRaisesRegex(s.Error, "rpc_reorg_or_chain_change"):
            self.relayer().step(self.operation_id, execute=True)
        self.assertEqual(self.wallet.calls, [])

    def test_rpc_and_state_permissions_fail_closed(self):
        for url in ("http://example.com/", "https://name:secret@example.com/", "https://example.com/#fragment"):
            with self.assertRaises(s.Error):
                r.Rpc(url)
        self.root.chmod(0o755)
        with self.assertRaisesRegex(s.Error, "private_directory"):
            r.Store(self.root)

    def test_raw_transaction_encoding_and_sender_match_foundry(self):
        self.relayer().step(self.operation_id, execute=True)
        op = self.store.load(self.settings, self.limits)["operations"][self.operation_id]
        decoded = json.loads(s.cast("decode-transaction", "--json", op["raw_transaction"]))
        if isinstance(decoded, str):
            decoded = json.loads(decoded)
        for field in ("type", "chainId", "nonce", "gas", "maxFeePerGas", "maxPriorityFeePerGas", "to", "value", "accessList"):
            self.assertEqual(decoded[field], op["unsigned_transaction"][field])
        self.assertEqual(decoded["signer"], self.limits["account"])
        self.assertEqual(decoded["input"], self.item["calldata"])
        self.assertEqual(decoded["hash"], op["transaction_hash"])
        with self.assertRaises(s.Error):
            r.validate_raw_transaction(signed_raw(op["unsigned_transaction"], KEYS["operator"]), op["unsigned_transaction"])

    def test_raw_transaction_fsync_failure_never_reaches_broadcast(self):
        original = self.store.save

        def fail_on_raw(state):
            if state["operations"][self.operation_id]["raw_transaction"]:
                raise OSError("simulated disk full")
            original(state)

        with patch.object(self.store, "save", side_effect=fail_on_raw), self.assertRaises(OSError):
            self.relayer().step(self.operation_id, execute=True)
        self.assertEqual(self.rpc.sent, [])
        op = self.store.load(self.settings, self.limits)["operations"][self.operation_id]
        self.assertEqual(op["status"], "signing_uncertain")
        self.assertIsNone(op["raw_transaction"])

    def test_canonical_durable_receipt_survives_transient_rpc_absence(self):
        self.relayer().step(self.operation_id, execute=True)
        self.rpc.accept_receipt()
        self.relayer().step(self.operation_id, execute=True)
        self.rpc.receipt = None
        self.rpc.advance()
        result = self.relayer().step(self.operation_id, execute=True)
        self.assertEqual(result["status"], "accepted")
        self.assertIsNotNone(result["receipt"])
        self.assertEqual(len(self.rpc.sent), 1)

    def test_another_package_cannot_take_reserved_wallet_nonce(self):
        self.relayer().step(self.operation_id, execute=True)
        sidecar = copy.deepcopy(self.sidecar)
        sidecar["accepted_package"]["manifestHash"] = h(150)
        request = s.typed_request("AcceptedPackageV1", s.PACKAGE, sidecar["accepted_package"], "ZkSysWrapperCoordinator",
                                  self.settings["settlement_chain_id"], self.settings["coordinator"], self.settings["sequencer"])
        sidecar["sequencer_signature"] = sign(request, "sequencer")
        sidecar["wrapper_signature"] = sign({**request, "signer": sidecar["accepted_package"]["wrapper"]}, "wrapper")
        item = r.artifact(self.settings, sidecar, self.item["proof_data"])
        self.rpc.item = item
        second = self.relayer().stage(item)
        result = self.relayer().step(second, execute=True)
        self.assertEqual(result["next_action"], "wait_for_existing_wallet_reservation")
        self.assertEqual(len(self.signatures()), 1)
        self.assertEqual(len(self.rpc.sent), 1)

    def test_missing_expired_or_unsatisfied_priority_work_never_signs(self):
        for flag, value, reason in (("guard_work_missing", True, "priority_work_not_frozen"),
                                    ("guard_expired", True, "priority_checkpoint_needs_refresh"),
                                    ("witness", False, "priority_prefix_witness_missing")):
            with self.subTest(flag=flag):
                setattr(self.rpc, flag, value)
                result = self.relayer().step(self.operation_id, execute=True)
                self.assertEqual(result["context"]["reason"], reason)
                self.assertEqual(self.signatures(), [])
                self.rpc.guard_work_missing = self.rpc.guard_expired = False
                self.rpc.witness = True


if __name__ == "__main__":
    unittest.main()
