#!/usr/bin/env python3
"""Durable signing-only wallet transactions for validated keeper maintenance calls."""

import copy
from contextlib import nullcontext
from pathlib import Path
import time

import relay as r
import service as s


TERMINAL = {"confirmed", "reverted"}
STATUSES = TERMINAL | {"staged", "stale_unsigned", "signing_uncertain", "signed", "send_uncertain", "broadcast",
                       "mined_unconfirmed", "nonce_conflict", "authorization_stale_reserved", "reorged"}
PACKAGE_SIGNATURE = "(" + r.abi_tuple(s.PACKAGE) + ")"
ACTIONS = {name: (name + PACKAGE_SIGNATURE, "gate", 4 + 32 * len(s.PACKAGE))
           for name in ("openPackage", "repairPackage", "openBootstrapPackage", "repairBootstrapPackage")}
ACTIONS.update({"refreshPriorityCheckpoint": ("refreshPriorityCheckpoint()", "gate", 4),
                "prepareRosterDraw": ("prepareRosterDraw(uint64)", "coordinator", 36),
                "recordRosterDraw": ("recordRosterDraw(uint64)", "coordinator", 36),
                "publishPrefixWitness": ("publishPrefixWitness(bytes32,uint256[],bytes32[],bytes32[],bytes32[])",
                                         "priority", None)})


class Store(r.Store):
    @classmethod
    def initialize(cls, root, settings, limits):
        s.config(settings)
        r.policy(limits)
        root = Path(root)
        s.require(root.is_absolute(), "absolute_state_directory_required")
        root.mkdir(mode=0o700)
        r.sync_dir(root.parent)
        store = cls(root)
        store.save({"schema_version": 1, "kind": "keeper_transactions", "config_hash": r.sha256(settings),
                    "policy_hash": r.sha256(limits), "operations": {}})
        return store

    def load(self, settings, limits):
        state = s.read_json(self.root / "state.json", private=True, maximum=r.MAX_STATE)
        s.exact(state, ("schema_version", "kind", "config_hash", "policy_hash", "operations"))
        s.require(state["schema_version"] == 1 and state["kind"] == "keeper_transactions"
                  and state["config_hash"] == r.sha256(settings) and state["policy_hash"] == r.sha256(limits),
                  "transaction_configuration_changed")
        s.require(type(state["operations"]) is dict and len(state["operations"]) <= 256, "invalid_transaction_operations")
        return state


def initialize(root, settings, policy):
    return Store.initialize(root, settings, policy)


def intent(settings, limits, call):
    s.exact(call, ("schema_version", "chain_id", "anchor", "transaction", "action", "broadcast"))
    s.require(type(call["schema_version"]) is int and call["schema_version"] == 1 and call["broadcast"] is False
              and call["chain_id"] == settings["settlement_chain_id"], "invalid_keeper_transaction")
    s.require(type(call["anchor"]) is dict, "invalid_keeper_anchor")
    s.nonzero(call["anchor"].get("hash"))
    s.uint(call["anchor"].get("number"))
    s.uint(call["anchor"].get("timestamp"))
    tx = call["transaction"]
    s.exact(tx, ("from", "to", "data", "value"))
    s.require(tx["from"] == limits["account"] and tx["value"] == "0x0", "maintenance_wallet_or_value_mismatch")
    s.nonzero(tx["to"], 20)
    s.require(type(call["action"]) is str and call["action"] in ACTIONS, "unsupported_keeper_action")
    signature, target, size = ACTIONS[call["action"]]
    data = s.raw_hex(tx["data"])
    s.require(4 <= len(data) <= r.MAX_RPC and data[:4] == r.selector(signature)
              and (size is None or len(data) == size), "keeper_action_calldata_mismatch")
    if target != "priority":
        s.require(tx["to"] == settings["proof_gate" if target == "gate" else "coordinator"], "keeper_target_mismatch")
    if target == "gate":
        s.require(tx["from"] == settings["sequencer"], "keeper_requires_sequencer_wallet")
    # Head changes do not create another authorization for the same maintenance intent.
    return {"chain_id": call["chain_id"], "action": call["action"], "transaction": copy.deepcopy(tx)}


class Transactions:
    def __init__(self, settings, policy, rpc, wallet=None, store=None, clock=time.time):
        s.config(settings)
        r.policy(policy)
        s.require(isinstance(store, Store), "transaction_store_required")
        self.settings, self.limits, self.rpc, self.wallet, self.store, self.clock = settings, policy, rpc, wallet, store, clock
        self.state = store.load(settings, policy)

    def save(self, execute):
        if execute:
            self.store.save(self.state)

    def stage(self, call):
        frozen = intent(self.settings, self.limits, call)
        operation_id = r.sha256(frozen)
        with self.store.lock():
            self.state = self.store.load(self.settings, self.limits)
            existing = self.state["operations"].get(operation_id)
            if existing is not None:
                s.require(intent(self.settings, self.limits, existing["call"]) == frozen, "maintenance_intent_changed")
                return operation_id
            s.require(len(self.state["operations"]) < 256, "transaction_operation_limit")
            self.state["operations"][operation_id] = {"call": copy.deepcopy(call), "status": "staged",
                "unsigned_transaction": None, "raw_transaction": None, "transaction_hash": None,
                "broadcast_attempts": 0, "last_broadcast_at": None, "last_error": None, "receipt": None}
            self.save(True)
        return operation_id

    def _transaction(self, call, nonce):
        return {"type": "0x2", "chainId": self.settings["settlement_chain_id"], **call["transaction"], "nonce": hex(nonce),
                "gas": hex(self.limits["gas_limit"]), "maxFeePerGas": self.limits["max_fee_per_gas"],
                "maxPriorityFeePerGas": self.limits["max_priority_fee_per_gas"], "accessList": []}

    def _validate(self, operation_id, op):
        s.exact(op, ("call", "status", "unsigned_transaction", "raw_transaction", "transaction_hash",
                     "broadcast_attempts", "last_broadcast_at", "last_error", "receipt"))
        s.require(r.sha256(intent(self.settings, self.limits, op["call"])) == operation_id, "maintenance_intent_changed")
        s.require(op["status"] in STATUSES and s.uint(op["broadcast_attempts"], 32) <= self.limits["max_broadcast_attempts"],
                  "invalid_transaction_state")
        if op["last_broadcast_at"] is not None:
            s.uint(op["last_broadcast_at"], 64)
        unsigned = op["unsigned_transaction"]
        if unsigned is not None:
            s.require(type(unsigned) is dict and unsigned == self._transaction(op["call"], s.uint(unsigned.get("nonce"))),
                      "durable_unsigned_transaction_changed")
        s.require((op["raw_transaction"] is None) == (op["transaction_hash"] is None), "invalid_durable_transaction_hash")
        if op["raw_transaction"] is not None:
            s.require(unsigned is not None and r.validate_raw_transaction(op["raw_transaction"], unsigned) == op["transaction_hash"],
                      "durable_transaction_changed")
        if op["receipt"] is not None:
            receipt = op["receipt"]
            s.exact(receipt, ("transaction_hash", "block_number", "block_hash", "status"))
            s.require(op["transaction_hash"] is not None and receipt["transaction_hash"] == op["transaction_hash"]
                      and type(receipt["status"]) is int and receipt["status"] in (0, 1), "invalid_durable_receipt")
            s.nonzero(receipt["block_hash"])
            s.uint(receipt["block_number"])
        s.require(op["status"] not in TERMINAL or op["receipt"] is not None, "terminal_transaction_requires_receipt")

    def _canonical(self, receipt, head):
        if s.uint(receipt["block_number"]) > s.uint(head["number"]):
            return False
        block = self.rpc.call("eth_getBlockByNumber", [receipt["block_number"], False])
        s.require(type(block) is dict and isinstance(block.get("hash"), str), "canonical_block_unavailable")
        return block["hash"] == receipt["block_hash"]

    def _confirmations(self, receipt, head):
        return s.uint(head["number"]) - s.uint(receipt["block_number"]) + 1

    def _receipt(self, op, head):
        if op["receipt"] is not None and not self._canonical(op["receipt"], head):
            op.update(receipt=None, status="reorged")
        if op["transaction_hash"] is None:
            return
        value = self.rpc.call("eth_getTransactionReceipt", [op["transaction_hash"]])
        if value is None:
            return
        s.require(type(value) is dict and value.get("transactionHash") == op["transaction_hash"]
                  and isinstance(value.get("from"), str) and value["from"].lower() == self.limits["account"]
                  and isinstance(value.get("to"), str) and value["to"].lower() == op["call"]["transaction"]["to"],
                  "receipt_identity_mismatch")
        s.nonzero(value.get("blockHash"))
        status = s.uint(value.get("status"))
        s.require(status in (0, 1), "invalid_receipt_status")
        receipt = {"transaction_hash": value["transactionHash"], "block_number": value["blockNumber"],
                   "block_hash": value["blockHash"], "status": status}
        if self._canonical(receipt, head):
            op["receipt"] = receipt

    def _eligible(self, call, head, anchor, preflight):
        settings, limits = self.settings, self.limits
        gate = settings["proof_gate"]
        code = s.raw_hex(self.rpc.call("eth_getCode", [gate, anchor]))
        s.require(code and s.keccak(code) == limits["gate_code_hash"], "gate_code_changed")
        for signature, kind, expected in (("chain()", "address", settings["chain_address"]),
            ("childChainId()", "uint", s.uint(settings["execution_chain_id"])), ("sequencer()", "address", settings["sequencer"]),
            ("policyHash()", "bytes32", settings["policy_hash"]), ("productionVkHash()", "bytes32", settings["vk_hash"])):
            s.require(r.call_word(self.rpc, gate, signature, anchor, kind=kind) == expected, "gate_configuration_changed")
        target = ACTIONS[call["action"]][1]
        if target == "priority":
            expected = r.call_word(self.rpc, gate, "priorityGuard()", anchor, kind="address")
            s.require(call["transaction"]["to"] == expected, "keeper_target_mismatch")
            code_hash = limits["priority_guard_code_hash"]
        elif target == "coordinator":
            s.require(r.call_word(self.rpc, gate, "coordinator()", anchor, kind="address") == settings["coordinator"],
                      "coordinator_configuration_changed")
            code_hash = limits["coordinator_code_hash"]
        else:
            code_hash = limits["gate_code_hash"]
        if target != "gate":
            code = s.raw_hex(self.rpc.call("eth_getCode", [call["transaction"]["to"], anchor]))
            s.require(code and s.keccak(code) == code_hash, "maintenance_target_code_changed")
        ready = preflight(copy.deepcopy(call), copy.deepcopy(head), copy.deepcopy(anchor))
        s.require(type(ready) is bool, "invalid_maintenance_preflight_result")
        if ready:
            try:
                self.rpc.call("eth_call", [call["transaction"], anchor])
            except r.RpcError:
                ready = False
        r.check_anchor(self.rpc, settings, head)
        return ready

    def _other_reservation(self, operation_id, head):
        for other_id, other in self.state["operations"].items():
            if other_id == operation_id or other["unsigned_transaction"] is None:
                continue
            if other["status"] in TERMINAL and self._canonical(other["receipt"], head):
                if self._confirmations(other["receipt"], head) >= self.limits["confirmations"]:
                    continue
            return True
        return False

    def summary(self, operation_id, action):
        op = self.state["operations"][operation_id]
        return {"operation_id": operation_id, "action": op["call"]["action"], "next_action": action,
                **{key: copy.deepcopy(op[key]) for key in ("status", "transaction_hash", "broadcast_attempts", "last_error", "receipt")}}

    def step(self, operation_id, execute=False, preflight=None):
        s.require(type(execute) is bool and callable(preflight), "maintenance_preflight_required")
        with self.store.lock() if execute else nullcontext():
            self.state = self.store.load(self.settings, self.limits)
            s.require(operation_id in self.state["operations"], "unknown_transaction_operation")
            for identifier, op in self.state["operations"].items():
                self._validate(identifier, op)
            result = self._step(operation_id, execute, preflight)
            self.save(execute)
            return result

    def _step(self, operation_id, execute, preflight):
        op = self.state["operations"][operation_id]
        now = int(self.clock())
        head, anchor = r.anchored_head(self.rpc, self.settings, self.limits, now)
        self._receipt(op, head)
        if op["receipt"] is not None:
            r.check_anchor(self.rpc, self.settings, head)
            if self._confirmations(op["receipt"], head) >= self.limits["confirmations"]:
                op["status"] = "confirmed" if op["receipt"]["status"] else "reverted"
                return self.summary(operation_id, "complete")
            op["status"] = "mined_unconfirmed"
            return self.summary(operation_id, "wait_for_confirmations")
        if op["unsigned_transaction"] is not None:
            latest = s.uint(self.rpc.call("eth_getTransactionCount", [self.limits["account"], anchor]))
            if latest != s.uint(op["unsigned_transaction"]["nonce"]):
                r.check_anchor(self.rpc, self.settings, head)
                op["status"] = "nonce_conflict"
                return self.summary(operation_id, "investigate_reserved_nonce_without_receipt")
        if self._other_reservation(operation_id, head):
            r.check_anchor(self.rpc, self.settings, head)
            return self.summary(operation_id, "wait_for_existing_wallet_reservation")
        if not self._eligible(op["call"], head, anchor, preflight):
            op["status"] = "stale_unsigned" if op["unsigned_transaction"] is None else "authorization_stale_reserved"
            return self.summary(operation_id, "wait_for_fresh_preflight" if op["unsigned_transaction"] is None
                                else "reconcile_reserved_nonce_no_replacement")
        if op["raw_transaction"] is None:
            account = self.limits["account"]
            latest = s.uint(self.rpc.call("eth_getTransactionCount", [account, anchor]))
            pending = s.uint(self.rpc.call("eth_getTransactionCount", [account, "pending"]))
            s.require(latest == pending, "wallet_has_other_pending_nonce")
            balance = s.uint(self.rpc.call("eth_getBalance", [account, anchor]))
            s.require(balance >= self.limits["gas_limit"] * s.uint(self.limits["max_fee_per_gas"]), "insufficient_fee_balance")
            unsigned = self._transaction(op["call"], latest)
            s.require(op["unsigned_transaction"] is None or op["unsigned_transaction"] == unsigned,
                      "reserved_transaction_or_nonce_changed")
            r.check_anchor(self.rpc, self.settings, head)
            if not execute:
                return self.summary(operation_id, "would_request_wallet_signature")
            s.require(self.wallet is not None, "trusted_wallet_rpc_required")
            op.update(unsigned_transaction=unsigned, status="signing_uncertain")
            self.save(True)
            s.require(s.uint(self.wallet.call("eth_chainId", [])) == s.uint(self.settings["settlement_chain_id"]), "wrong_wallet_chain")
            signed = self.wallet.call("eth_signTransaction", [unsigned])
            raw = signed.get("raw") if isinstance(signed, dict) else signed
            tx_hash = r.validate_raw_transaction(raw, unsigned)
            op.update(raw_transaction=raw, transaction_hash=tx_hash, status="signed")
            self.save(True)
            head, anchor = r.anchored_head(self.rpc, self.settings, self.limits, int(self.clock()))
            if not self._eligible(op["call"], head, anchor, preflight):
                op["status"] = "authorization_stale_reserved"
                return self.summary(operation_id, "reconcile_reserved_nonce_no_replacement")
        if op["broadcast_attempts"] >= self.limits["max_broadcast_attempts"]:
            return self.summary(operation_id, "broadcast_limit_reached_reconcile_only")
        if op["last_broadcast_at"] is not None:
            s.require(now >= op["last_broadcast_at"], "transaction_clock_rollback")
            if now - op["last_broadcast_at"] < self.limits["rebroadcast_interval_seconds"]:
                return self.summary(operation_id, "wait_before_identical_rebroadcast")
        if not execute:
            return self.summary(operation_id, "would_broadcast_identical_raw_transaction")
        op.update(status="send_uncertain", last_broadcast_at=int(self.clock()), last_error=None)
        op["broadcast_attempts"] += 1
        self.save(True)
        try:
            returned = self.rpc.call("eth_sendRawTransaction", [op["raw_transaction"]])
            s.require(returned == op["transaction_hash"], "broadcast_hash_mismatch")
            op["status"] = "broadcast"
        except s.Error as error:
            op["last_error"] = str(error)
        return self.summary(operation_id, "reconcile_transaction_hash")
