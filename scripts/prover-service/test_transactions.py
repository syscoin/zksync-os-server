import copy
from pathlib import Path
import tempfile
import unittest

import relay as r
import service as s
import transactions as t
from test_relay import relay_policy, signed_raw
from test_service import fixture, KEYS, a, h


class Rpc:
    def __init__(self, settings, limits):
        self.settings, self.limits = settings, limits
        self.now = 1000
        self.head = {"number": "0x14", "hash": h(110), "timestamp": hex(self.now)}
        self.blocks = {self.head["number"]: copy.deepcopy(self.head)}
        self.calls, self.sent, self.receipt = [], [], None
        self.nonce, self.pending_nonce, self.balance = 0, 0, 10**30
        self.send_error, self.send_interrupt, self.reorg, self.bad_code = None, False, False, False
        self.chain = settings["settlement_chain_id"]
        self.store, self.operation_id = None, None

    def advance(self):
        self.now += 30
        number = s.uint(self.head["number"]) + 1
        self.head = {"number": hex(number), "hash": h(110 + number - 20), "timestamp": hex(self.now)}
        self.blocks[self.head["number"]] = copy.deepcopy(self.head)

    def mine(self, status=1):
        op = self.store.load(self.settings, self.limits)["operations"][self.operation_id]
        self.receipt = {"transactionHash": op["transaction_hash"], "from": self.limits["account"],
                        "to": op["call"]["transaction"]["to"], "blockNumber": self.head["number"],
                        "blockHash": self.head["hash"], "status": hex(status)}
        self.nonce = self.pending_nonce = s.uint(op["unsigned_transaction"]["nonce"]) + 1

    def call(self, method, params):
        self.calls.append((method, copy.deepcopy(params)))
        if method == "eth_chainId":
            return self.chain
        if method == "eth_getBlockByNumber":
            if params[0] == "latest":
                return copy.deepcopy(self.head)
            block = copy.deepcopy(self.blocks.get(params[0]))
            if self.reorg and block is not None:
                block["hash"] = h(99)
            return block
        if method == "eth_getCode":
            assert params[1] == {"blockHash": self.head["hash"], "requireCanonical": True}
            if self.bad_code:
                return "0x00"
            return "0x" + {self.settings["proof_gate"]: b"gate-code", self.settings["coordinator"]: b"coordinator-code",
                            a(123): b"priority-code"}[params[0]].hex()
        if method == "eth_getTransactionCount":
            return hex(self.pending_nonce if params[1] == "pending" else self.nonce)
        if method == "eth_getBalance":
            return hex(self.balance)
        if method == "eth_getTransactionReceipt":
            return copy.deepcopy(self.receipt)
        if method == "eth_sendRawTransaction":
            op = self.store.load(self.settings, self.limits)["operations"][self.operation_id]
            assert op["status"] == "send_uncertain" and op["raw_transaction"] == params[0]
            assert op["transaction_hash"] == s.keccak(s.raw_hex(params[0]))
            self.sent.append(params[0])
            if self.send_interrupt:
                raise KeyboardInterrupt
            if self.send_error is not None:
                raise self.send_error
            return s.keccak(s.raw_hex(params[0]))
        if method == "eth_call":
            tx, anchor = params
            assert anchor == {"blockHash": self.head["hash"], "requireCanonical": True}
            data = s.raw_hex(tx["data"])
            values = {"chain()": self.settings["chain_address"], "childChainId()": s.uint(self.settings["execution_chain_id"]),
                      "sequencer()": self.settings["sequencer"], "policyHash()": self.settings["policy_hash"],
                      "productionVkHash()": self.settings["vk_hash"], "priorityGuard()": a(123),
                      "coordinator()": self.settings["coordinator"]}
            for signature, value in values.items():
                if data == r.selector(signature):
                    return "0x" + (s.word(value) if type(value) is int else s.raw_hex(value).rjust(32, b"\0")).hex()
            if data[:4] in [r.selector(value[0]) for value in t.ACTIONS.values()]:
                return "0x"
        raise AssertionError((method, params))


class Wallet:
    def __init__(self, settings, limits, store):
        self.settings, self.limits, self.store = settings, limits, store
        self.calls, self.interrupt, self.mutate, self.key = [], False, None, KEYS["sequencer"]

    def call(self, method, params):
        self.calls.append((method, copy.deepcopy(params)))
        if method == "eth_chainId":
            return self.settings["settlement_chain_id"]
        if method == "eth_signTransaction":
            state = self.store.load(self.settings, self.limits)
            op = next(value for value in state["operations"].values() if value["status"] == "signing_uncertain")
            assert op["unsigned_transaction"] == params[0] and op["raw_transaction"] is None
            if self.interrupt:
                raise KeyboardInterrupt
            tx = copy.deepcopy(params[0])
            if self.mutate:
                tx.update(self.mutate)
            return {"raw": signed_raw(tx, self.key)}
        raise AssertionError(method)


class TransactionsTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.settings = fixture()["settings"]
        cls.limits = relay_policy(cls.settings)
        cls.limits["account"] = cls.settings["sequencer"]

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve() / "transactions"
        self.store = t.initialize(self.root, self.settings, self.limits)
        self.rpc = Rpc(self.settings, self.limits)
        self.rpc.store = self.store
        self.wallet = Wallet(self.settings, self.limits, self.store)
        self.call = {"schema_version": 1, "chain_id": self.settings["settlement_chain_id"],
                     "anchor": copy.deepcopy(self.rpc.head), "action": "refreshPriorityCheckpoint", "broadcast": False,
                     "transaction": {"from": self.settings["sequencer"], "to": self.settings["proof_gate"], "value": "0x0",
                                     "data": "0x" + r.selector("refreshPriorityCheckpoint()").hex()}}
        self.identifier = self.runner().stage(self.call)
        self.rpc.operation_id = self.identifier

    def runner(self):
        return t.Transactions(self.settings, self.limits, self.rpc, self.wallet, self.store, lambda: self.rpc.now)

    def step(self, execute=True, preflight=None, identifier=None):
        return self.runner().step(identifier or self.identifier, execute, preflight or (lambda call, head, anchor: True))

    def signatures(self):
        return [call for call in self.wallet.calls if call[0] == "eth_signTransaction"]

    def another(self):
        call = copy.deepcopy(self.call)
        call.update(action="prepareRosterDraw")
        call["transaction"].update(to=self.settings["coordinator"],
                                   data="0x" + (r.selector("prepareRosterDraw(uint64)") + s.word(5)).hex())
        return self.runner().stage(call)

    def test_same_intent_is_idempotent_across_canonical_heads(self):
        self.rpc.advance()
        changed = {**self.call, "anchor": copy.deepcopy(self.rpc.head)}
        self.assertEqual(self.runner().stage(changed), self.identifier)
        self.assertEqual(len(self.store.load(self.settings, self.limits)["operations"]), 1)

    def test_dry_run_never_signs_sends_or_persists(self):
        (self.root / "relay.lock").unlink()
        before = {path.name: path.read_bytes() for path in self.root.iterdir()}
        observed = []
        def preflight(call, head, anchor):
            observed.append((call, head, anchor))
            return True
        result = self.step(False, preflight)
        self.assertEqual(result["next_action"], "would_request_wallet_signature")
        self.assertEqual(observed[0][2], {"blockHash": self.rpc.head["hash"], "requireCanonical": True})
        self.assertEqual(self.wallet.calls, [])
        self.assertEqual(self.rpc.sent, [])
        self.assertEqual(before, {path.name: path.read_bytes() for path in self.root.iterdir()})

    def test_sign_persist_send_receipt_and_confirm_without_requiring_open_preflight(self):
        self.assertEqual(self.step()["status"], "broadcast")
        self.rpc.mine()
        self.assertEqual(self.step(preflight=lambda *args: False)["status"], "mined_unconfirmed")
        self.rpc.advance()
        self.assertEqual(self.step(preflight=lambda *args: False)["status"], "confirmed")
        self.assertEqual(len(self.signatures()), 1)
        self.assertEqual(len(self.rpc.sent), 1)

    def test_signing_interruption_preserves_exact_unsigned_and_blocks_other_nonce(self):
        self.wallet.interrupt = True
        with self.assertRaises(KeyboardInterrupt):
            self.step()
        op = self.store.load(self.settings, self.limits)["operations"][self.identifier]
        self.assertEqual(op["status"], "signing_uncertain")
        other = self.another()
        self.assertEqual(self.step(identifier=other)["next_action"], "wait_for_existing_wallet_reservation")
        self.wallet.interrupt = False
        self.step()
        self.assertEqual(self.signatures()[0][1], self.signatures()[1][1])
        self.assertEqual(len(self.rpc.sent), 1)

    def test_ambiguous_send_restarts_with_only_identical_raw_bytes(self):
        self.rpc.send_error = s.Error("rpc_transport_failure")
        self.assertEqual(self.step()["status"], "send_uncertain")
        self.rpc.send_error = None
        self.rpc.advance()
        self.step()
        self.assertEqual(len(self.signatures()), 1)
        self.assertEqual(self.rpc.sent, [self.rpc.sent[0], self.rpc.sent[0]])

    def test_crash_at_broadcast_recovers_by_persisted_transaction_hash(self):
        self.rpc.send_interrupt = True
        with self.assertRaises(KeyboardInterrupt):
            self.step()
        op = self.store.load(self.settings, self.limits)["operations"][self.identifier]
        self.assertEqual(op["status"], "send_uncertain")
        self.rpc.send_interrupt = False
        self.rpc.mine()
        self.rpc.advance()
        self.assertEqual(self.step()["status"], "confirmed")
        self.assertEqual(len(self.rpc.sent), 1)

    def test_stale_before_signing_has_no_reservation(self):
        self.assertEqual(self.step(preflight=lambda *args: False)["status"], "stale_unsigned")
        self.assertEqual(self.wallet.calls, [])
        self.rpc.operation_id = self.another()
        self.assertEqual(self.step(identifier=self.rpc.operation_id)["status"], "broadcast")

    def test_stale_after_signing_never_releases_or_replaces_reserved_nonce(self):
        observed = []
        def preflight(*args):
            observed.append(True)
            return len(observed) == 1
        result = self.step(preflight=preflight)
        self.assertEqual(result["status"], "authorization_stale_reserved")
        self.assertEqual(self.rpc.sent, [])
        self.assertEqual(self.step(identifier=self.another())["next_action"], "wait_for_existing_wallet_reservation")
        self.step()
        self.assertEqual(len(self.signatures()), 1)

    def test_unknown_consumed_nonce_blocks_replacement(self):
        self.step()
        self.rpc.nonce = self.rpc.pending_nonce = 1
        self.assertEqual(self.step()["status"], "nonce_conflict")
        self.assertEqual(self.step(identifier=self.another())["next_action"], "wait_for_existing_wallet_reservation")
        self.assertEqual(len(self.signatures()), 1)

    def test_wallet_cannot_change_intent_or_sign_as_another_account(self):
        for change in ({"to": a(99)}, {"chainId": "0x1"}, {"data": "0x1234"}, {"maxFeePerGas": "0x100"}):
            with self.subTest(change=change):
                self.wallet.mutate = change
                with self.assertRaisesRegex(s.Error, "wallet_changed_transaction"):
                    self.step()
        self.wallet.mutate, self.wallet.key = None, KEYS["account"]
        with self.assertRaises(s.Error):
            self.step()
        self.assertEqual(self.rpc.sent, [])

    def test_receipt_identity_and_status_are_required(self):
        self.step()
        self.rpc.mine()
        valid = copy.deepcopy(self.rpc.receipt)
        for field, value in (("transactionHash", h(99)), ("from", a(99)), ("to", a(99)), ("status", "0x2")):
            with self.subTest(field=field):
                self.rpc.receipt = {**valid, field: value}
                with self.assertRaises(s.Error):
                    self.step()

    def test_reverted_transaction_keeps_reservation_until_confirmations(self):
        self.step()
        self.rpc.mine(status=0)
        self.assertEqual(self.step()["status"], "mined_unconfirmed")
        other = self.another()
        self.assertEqual(self.step(identifier=other)["next_action"], "wait_for_existing_wallet_reservation")
        self.rpc.advance()
        self.assertEqual(self.step()["status"], "reverted")
        self.rpc.receipt, self.rpc.operation_id = None, other
        self.assertEqual(self.step(identifier=other)["status"], "broadcast")

    def test_reorg_of_confirmed_receipt_blocks_a_new_reservation(self):
        self.step()
        self.rpc.mine()
        included = self.rpc.receipt["blockNumber"]
        self.rpc.advance()
        self.assertEqual(self.step()["status"], "confirmed")
        self.rpc.blocks[included]["hash"] = h(99)
        self.rpc.receipt, self.rpc.nonce, self.rpc.pending_nonce = None, 0, 0
        self.assertEqual(self.step(identifier=self.another())["next_action"], "wait_for_existing_wallet_reservation")
        self.assertNotEqual(self.step()["status"], "confirmed")
        self.assertEqual(self.rpc.sent, [self.rpc.sent[0], self.rpc.sent[0]])

    def test_reorg_chain_change_or_code_change_never_requests_wallet(self):
        for field, value, reason in (("reorg", True, "rpc_reorg_or_chain_change"), ("chain", "0x1", "wrong_rpc_chain"),
                                     ("bad_code", True, "gate_code_changed")):
            original = getattr(self.rpc, field)
            setattr(self.rpc, field, value)
            with self.assertRaisesRegex(s.Error, reason):
                self.step()
            setattr(self.rpc, field, original)
        self.assertEqual(self.wallet.calls, [])

    def test_call_cannot_select_arbitrary_target_selector_or_value(self):
        for field, value in (("to", a(99)), ("from", a(99)), ("value", "0x1"), ("data", "0x12345678")):
            call = copy.deepcopy(self.call)
            call["transaction"][field] = value
            with self.assertRaises(s.Error):
                self.runner().stage(call)
        call = {**self.call, "action": "transfer"}
        with self.assertRaisesRegex(s.Error, "unsupported_keeper_action"):
            self.runner().stage(call)

    def test_priority_target_is_authenticated_before_signing(self):
        call = copy.deepcopy(self.call)
        call["action"] = "publishPrefixWitness"
        call["transaction"].update(to=a(99), data="0x" + r.selector(t.ACTIONS[call["action"]][0]).hex())
        identifier = self.runner().stage(call)
        with self.assertRaisesRegex(s.Error, "keeper_target_mismatch"):
            self.step(identifier=identifier)
        self.assertEqual(self.wallet.calls, [])

    def test_policy_change_missing_preflight_and_external_pending_nonce_fail_closed(self):
        changed = {**self.limits, "max_broadcast_attempts": 4}
        with self.assertRaisesRegex(s.Error, "transaction_configuration_changed"):
            t.Transactions(self.settings, changed, self.rpc, self.wallet, self.store)
        with self.assertRaisesRegex(s.Error, "maintenance_preflight_required"):
            self.runner().step(self.identifier, True)
        with self.assertRaisesRegex(s.Error, "invalid_maintenance_preflight_result"):
            self.step(preflight=lambda *args: None)
        self.rpc.pending_nonce = 1
        with self.assertRaisesRegex(s.Error, "wallet_has_other_pending_nonce"):
            self.step()
        self.assertEqual(self.wallet.calls, [])

    def test_broadcast_limit_reconciles_without_resigning(self):
        for _ in range(self.limits["max_broadcast_attempts"]):
            self.step()
            self.rpc.advance()
        result = self.step()
        self.assertEqual(result["next_action"], "broadcast_limit_reached_reconcile_only")
        self.assertEqual(len(self.signatures()), 1)
        self.assertEqual(len(self.rpc.sent), self.limits["max_broadcast_attempts"])
        self.rpc.mine()
        self.rpc.advance()
        self.assertEqual(self.step()["status"], "confirmed")


if __name__ == "__main__":
    unittest.main()
