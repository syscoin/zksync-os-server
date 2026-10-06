#!/usr/bin/env python3
"""One-step durable service relay. Signing and broadcast require --execute."""

import argparse
from contextlib import contextmanager, nullcontext
import copy
import fcntl
import functools
import hashlib
import http.client
import json
import os
from pathlib import Path
import stat
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid

import service as s


MAX_RPC = 2 * 1024 * 1024
MAX_STATE = 16 * 1024 * 1024
TERMINAL = {"accepted", "reverted", "accepted_elsewhere", "stale_unsigned"}


class RpcError(s.Error):
    def __init__(self, code, data=None):
        self.code, self.data = code, data
        super().__init__("rpc_rejected_request")


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, request, fp, code, msg, headers, newurl):
        return None


class Rpc:
    def __init__(self, url, authorization=None, timeout=15):
        parsed = urllib.parse.urlsplit(url)
        s.require(parsed.scheme == "https" or parsed.scheme == "http" and parsed.hostname in ("127.0.0.1", "::1", "localhost"),
                  "rpc_requires_https_or_loopback")
        s.require(parsed.hostname and not parsed.username and not parsed.password and not parsed.fragment
                  and not any(ord(c) < 33 for c in url), "invalid_rpc_url")
        s.require(authorization is None or isinstance(authorization, str) and "\r" not in authorization
                  and "\n" not in authorization, "invalid_rpc_authorization")
        self.url, self.authorization, self.timeout, self.request_id = url, authorization, timeout, 0
        self.opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), NoRedirect())

    def call(self, method, params):
        self.request_id += 1
        request_id = self.request_id
        payload = s.canonical({"jsonrpc": "2.0", "id": request_id, "method": method, "params": params})
        headers = {"Content-Type": "application/json", "Accept-Encoding": "identity"}
        if self.authorization:
            headers["Authorization"] = self.authorization
        request = urllib.request.Request(self.url, payload, headers=headers)
        deadline = time.monotonic() + self.timeout
        try:
            with self.opener.open(request, timeout=self.timeout) as response:
                s.require(response.status == 200, "rpc_http_failure")
                data = bytearray()
                while part := response.read1(min(65536, MAX_RPC - len(data) + 1)):
                    s.require(time.monotonic() < deadline and len(data) + len(part) <= MAX_RPC, "rpc_response_limit")
                    data.extend(part)
            result = json.loads(data, object_pairs_hook=s._unique)
        except (urllib.error.URLError, OSError, http.client.HTTPException, ValueError, UnicodeError):
            raise s.Error("rpc_transport_failure") from None
        s.require(type(result) is dict and result.get("jsonrpc") == "2.0" and type(result.get("id")) is int
                  and result["id"] == request_id,
                  "invalid_rpc_response")
        if "error" in result:
            error = result["error"]
            raise RpcError(error.get("code") if isinstance(error, dict) else None,
                           error.get("data") if isinstance(error, dict) else None)
        s.require("result" in result, "missing_rpc_result")
        return result["result"]


def policy(value):
    s.exact(value, ("schema_version", "account", "gate_code_hash", "coordinator_code_hash", "priority_guard_code_hash", "gas_limit",
                    "max_fee_per_gas", "max_priority_fee_per_gas", "max_total_fee_wei", "confirmations",
                    "min_turn_seconds", "max_head_age_seconds", "rpc_timeout_seconds",
                    "rebroadcast_interval_seconds", "max_broadcast_attempts"))
    s.require(value["schema_version"] == 1, "unsupported_relay_policy")
    s.nonzero(value["account"], 20)
    s.raw_hex(value["coordinator_code_hash"], 32)
    for field in ("gate_code_hash", "priority_guard_code_hash"):
        s.nonzero(value[field])
    for field in ("gas_limit", "confirmations", "min_turn_seconds", "max_head_age_seconds", "rpc_timeout_seconds",
                  "rebroadcast_interval_seconds", "max_broadcast_attempts"):
        s.require(0 < s.uint(value[field], 64), "positive_relay_limit_required")
    s.require(value["rpc_timeout_seconds"] <= 30 and value["confirmations"] <= 100000
              and value["max_broadcast_attempts"] <= 1000, "relay_limit_too_large")
    for field in ("max_fee_per_gas", "max_priority_fee_per_gas", "max_total_fee_wei"):
        s.require(isinstance(value[field], str) and (s.uint(value[field]) >= 0 if field == "max_priority_fee_per_gas"
                  else s.uint(value[field]) > 0), "invalid_fee_limit")
    s.require(s.uint(value["max_priority_fee_per_gas"]) <= s.uint(value["max_fee_per_gas"])
              and value["gas_limit"] * s.uint(value["max_fee_per_gas"]) <= s.uint(value["max_total_fee_wei"]),
              "fee_budget_exceeded")
    return value


def sha256(value):
    return hashlib.sha256(s.canonical(value)).hexdigest()


def sync_dir(path):
    fd = os.open(path, os.O_RDONLY | os.O_DIRECTORY)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def atomic_json(path, value):
    data = s.canonical(value)
    s.require(len(data) <= MAX_STATE, "relay_state_capacity")
    temporary = path.with_name("." + path.name + "." + uuid.uuid4().hex + ".tmp")
    try:
        fd = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
        with os.fdopen(fd, "wb") as output:
            output.write(data)
            output.flush()
            os.fsync(output.fileno())
        os.replace(temporary, path)
        sync_dir(path.parent)
    finally:
        temporary.unlink(missing_ok=True)


class Store:
    def __init__(self, root):
        self.root = Path(root)
        info = self.root.lstat()
        s.require(self.root.is_absolute() and stat.S_ISDIR(info.st_mode) and info.st_uid == os.getuid()
                  and not info.st_mode & 0o077, "relay_state_requires_private_directory")

    @classmethod
    def initialize(cls, root, settings, limits):
        root = Path(root)
        s.require(root.is_absolute(), "absolute_state_directory_required")
        root.mkdir(mode=0o700)
        sync_dir(root.parent)
        store = cls(root)
        store.save({"schema_version": 1, "config_hash": sha256(settings), "policy_hash": sha256(limits), "operations": {}})
        return store

    @contextmanager
    def lock(self):
        path = self.root / "relay.lock"
        fd = os.open(path, os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW, 0o600)
        try:
            info = os.fstat(fd)
            s.require(stat.S_ISREG(info.st_mode) and info.st_uid == os.getuid() and not info.st_mode & 0o077,
                      "unsafe_relay_lock")
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
            yield
        except BlockingIOError:
            raise s.Error("relay_already_running") from None
        finally:
            os.close(fd)

    def load(self, settings, limits):
        state = s.read_json(self.root / "state.json", private=True, maximum=MAX_STATE)
        s.exact(state, ("schema_version", "config_hash", "policy_hash", "operations"))
        s.require(state["schema_version"] == 1 and state["config_hash"] == sha256(settings)
                  and state["policy_hash"] == sha256(limits), "relay_configuration_changed")
        s.require(type(state["operations"]) is dict and len(state["operations"]) <= 256, "invalid_relay_operations")
        return state

    def save(self, state):
        atomic_json(self.root / "state.json", state)


@functools.lru_cache(maxsize=256)
def selector(signature):
    return s.raw_hex(s.keccak(signature.encode()))[:4]


def abi_tuple(fields):
    return "(" + ",".join(kind for _, kind in fields) + ")"


def static_array(fields, values):
    return s.word(len(values)) + b"".join(s.encode_fields(fields, value) for value in values)


def gate_calldata(sidecar, proof_data, mode):
    accepted = s.encode_fields(s.PACKAGE, sidecar["accepted_package"])
    duties = sidecar["duties"]
    encoded_duties = s.word(len(duties)) + s.encode_dynamic_tuple([(s.encode_duty(duty), True) for duty in duties])
    outputs = static_array(s.OUTPUT, sidecar["batch_outputs"])
    parts = [(accepted, False), (encoded_duties, True), (outputs, True), (s.encode_bytes(s.raw_hex(proof_data)), True)]
    package_type = abi_tuple(s.PACKAGE)
    duty_type = abi_tuple(s.DUTY + [("operatorSignature", "bytes")])
    output_type = abi_tuple(s.OUTPUT)
    if mode == "bootstrap":
        signature = f"submitBootstrap({package_type},{duty_type}[],{output_type}[],bytes,bytes)"
        parts.append((s.encode_bytes(s.raw_hex(sidecar["sequencer_signature"])), True))
    else:
        signature = f"submit({package_type},{duty_type}[],{output_type}[],bytes,{abi_tuple(s.CANDIDATE)},bytes32[],bytes,bytes)"
        parts += [(s.encode_fields(s.CANDIDATE, sidecar["candidate"]), False),
                  (s.word(len(sidecar["candidate_proof"])) + b"".join(s.raw_hex(p, 32) for p in sidecar["candidate_proof"]), True),
                  (s.encode_bytes(s.raw_hex(sidecar["sequencer_signature"])), True),
                  (s.encode_bytes(s.raw_hex(sidecar["wrapper_signature"])), True)]
    return "0x" + (selector(signature) + s.encode_dynamic_tuple(parts)).hex()


def artifact(settings, sidecar, native_data):
    mode = "bootstrap" if sidecar.get("candidate") is None else "service"
    accepted = sidecar["accepted_package"]
    request = s.typed_request("AcceptedPackageV1", s.PACKAGE, accepted,
                              "ZkSysProofGate" if mode == "bootstrap" else "ZkSysWrapperCoordinator",
                              settings["settlement_chain_id"], settings["proof_gate"] if mode == "bootstrap" else settings["coordinator"],
                              settings["sequencer"])
    unsigned = {**sidecar, "sequencer_signature": "0x", "wrapper_signature": "0x"}
    prepared = {"schema_version": 1, "mode": mode, "sidecar": unsigned, "sequencer_request": request,
                "wrapper_request": None if mode == "bootstrap" else {**request, "signer": accepted["wrapper"]},
                "proof_data": native_data}
    s.complete_package(settings, prepared, sidecar["sequencer_signature"], sidecar["wrapper_signature"])
    return {"sidecar": sidecar, "proof_data": native_data, "mode": mode, "package_hash": request["struct_hash"],
            "digest": request["digest"], "calldata": gate_calldata(sidecar, native_data, mode)}


def rlp_encode(value):
    if isinstance(value, list):
        payload = b"".join(rlp_encode(item) for item in value)
        base = 0xC0
    else:
        s.require(type(value) is bytes, "invalid_rlp_value")
        if len(value) == 1 and value[0] < 0x80:
            return value
        payload, base = value, 0x80
    if len(payload) < 56:
        return bytes([base + len(payload)]) + payload
    length = len(payload).to_bytes((len(payload).bit_length() + 7) // 8, "big")
    return bytes([base + 55 + len(length)]) + length + payload


def rlp_decode(data):
    def decode(at, depth):
        s.require(at < len(data) and depth <= 4, "invalid_rlp")
        prefix = data[at]
        if prefix < 0x80:
            return data[at:at + 1], at + 1
        is_list = prefix >= 0xC0
        base = 0xC0 if is_list else 0x80
        length = prefix - base
        at += 1
        if length > 55:
            width = length - 55
            s.require(at + width <= len(data) and data[at] != 0, "invalid_rlp_length")
            length = int.from_bytes(data[at:at + width], "big")
            at += width
            s.require(length >= 56, "noncanonical_rlp_length")
        end = at + length
        s.require(end <= len(data), "truncated_rlp")
        if not is_list:
            s.require(length != 1 or data[at] >= 0x80, "noncanonical_rlp_string")
            return data[at:end], end
        values = []
        while at < end:
            value, at = decode(at, depth + 1)
            values.append(value)
        s.require(at == end, "invalid_rlp_list")
        return values, end
    result, end = decode(0, 0)
    s.require(end == len(data) and rlp_encode(result) == data, "noncanonical_rlp")
    return result


def integer_bytes(value):
    value = s.uint(value)
    return value.to_bytes((value.bit_length() + 7) // 8, "big")


def unsigned_fields(tx):
    return [integer_bytes(tx[key]) for key in ("chainId", "nonce", "maxPriorityFeePerGas", "maxFeePerGas", "gas")] + [
        s.raw_hex(tx["to"], 20), integer_bytes(tx["value"]), s.raw_hex(tx["data"]), []]


def validate_raw_transaction(raw, intended):
    data = s.raw_hex(raw)
    s.require(0 < len(data) <= MAX_RPC and data[0] == 2, "only_eip1559_transactions_supported")
    fields = rlp_decode(data[1:])
    s.require(type(fields) is list and len(fields) == 12 and fields[:9] == unsigned_fields(intended),
              "wallet_changed_transaction")
    parity, r, signature_s = fields[9:]
    s.require(type(parity) is bytes and parity in (b"", b"\x01") and type(r) is bytes and type(signature_s) is bytes
              and 0 < len(r) <= 32 and 0 < len(signature_s) <= 32 and r[0] != 0 and signature_s[0] != 0,
              "invalid_transaction_signature")
    signature = "0x" + (r.rjust(32, b"\0") + signature_s.rjust(32, b"\0") + bytes([27 + int.from_bytes(parity, "big")])).hex()
    digest = s.keccak(b"\x02" + rlp_encode(fields[:9]))
    s.verify_eoa({"signer": intended["from"], "digest": digest}, signature)
    return s.keccak(data)


def call_word(rpc, address, signature, anchor, encoded=b"", kind="uint"):
    data = "0x" + (selector(signature) + encoded).hex()
    raw = s.raw_hex(rpc.call("eth_call", [{"to": address, "data": data}, anchor]), 32)
    if kind == "bytes32":
        return "0x" + raw.hex()
    if kind == "address":
        s.require(raw[:12] == bytes(12), "invalid_address_response")
        return "0x" + raw[12:].hex()
    result = int.from_bytes(raw, "big")
    if kind == "bool":
        s.require(result in (0, 1), "invalid_boolean_response")
        return bool(result)
    return result


def anchored_head(rpc, settings, limits, now):
    s.require(s.uint(rpc.call("eth_chainId", [])) == s.uint(settings["settlement_chain_id"]), "wrong_rpc_chain")
    head = rpc.call("eth_getBlockByNumber", ["latest", False])
    s.require(type(head) is dict, "missing_canonical_head")
    s.nonzero(head.get("hash"))
    s.uint(head.get("number"))
    timestamp = s.uint(head.get("timestamp"))
    s.require(0 <= now - timestamp <= limits["max_head_age_seconds"], "stale_or_future_rpc_head")
    return head, {"blockHash": head["hash"], "requireCanonical": True}


def check_anchor(rpc, settings, head):
    canonical = rpc.call("eth_getBlockByNumber", [head["number"], False])
    s.require(isinstance(canonical, dict) and canonical.get("hash") == head["hash"]
              and s.uint(rpc.call("eth_chainId", [])) == s.uint(settings["settlement_chain_id"]), "rpc_reorg_or_chain_change")


def priority_context(rpc, settings, limits, item, head, anchor):
    gate, accepted = settings["proof_gate"], item["sidecar"]["accepted_package"]
    guard = call_word(rpc, gate, "priorityGuard()", anchor, kind="address")
    s.nonzero(guard, 20)
    s.require(s.keccak(s.raw_hex(rpc.call("eth_getCode", [guard, anchor]))) == limits["priority_guard_code_hash"],
              "priority_guard_code_changed")
    for signature, kind, expected in (("acceptanceGate()", "address", gate), ("chain()", "address", settings["chain_address"]),
                                       ("policyHash()", "bytes32", settings["policy_hash"])):
        s.require(call_word(rpc, guard, signature, anchor, kind=kind) == expected, "priority_guard_configuration_changed")
    work_id = s.keccak(s.raw_hex(accepted["parent"]) + s.word(accepted["batchFrom"]))
    current_id = call_word(rpc, gate, "priorityWorkId()", anchor, kind="bytes32")
    data = s.raw_hex(rpc.call("eth_call", [{"to": guard, "data": "0x" + selector("work()").hex()}, anchor]), 9 * 32)
    words = [data[i:i + 32] for i in range(0, len(data), 32)]
    numbers = [s.uint(int.from_bytes(value, "big"), 64) for value in words[3:]]
    batch_from, opened, _, _, required_end, max_end = numbers
    cursor = call_word(rpc, guard, "priorityCursor()", anchor)
    maximum_seconds = call_word(rpc, guard, "maxProofWorkSeconds()", anchor)
    deadline = opened + maximum_seconds
    result = {"guard": guard, "work_id": current_id, "checkpoint_hash": "0x" + words[1].hex(),
              "cursor": cursor, "required_end": required_end, "max_end": max_end, "deadline": deadline,
              "reason": "ready"}
    if current_id != work_id or words[0] != s.raw_hex(work_id) or batch_from != accepted["batchFrom"] or opened == 0:
        result["reason"] = "priority_work_not_frozen"
        return result
    if maximum_seconds == 0 or deadline - s.uint(head["timestamp"]) < limits["min_turn_seconds"]:
        result["reason"] = "priority_checkpoint_needs_refresh"
        return result
    counts = [s.uint(output["l1TxCount"]) for output in item["sidecar"]["batch_outputs"]]
    hashes = [output["priorityOperationsHash"] for output in item["sidecar"]["batch_outputs"]]
    if not required_end <= cursor + sum(counts) <= max_end:
        result["reason"] = "priority_prefix_range_mismatch"
    elif sum(counts) == 0:
        if any(value != s.keccak(b"") for value in hashes):
            result["reason"] = "priority_empty_hash_mismatch"
    else:
        encoded = s.encode_dynamic_tuple([(s.raw_hex(work_id), False), (words[1], False),
                                           (s.word(len(counts)) + b"".join(s.word(count) for count in counts), True),
                                           (s.word(len(hashes)) + b"".join(s.raw_hex(value) for value in hashes), True)])
        witness = call_word(rpc, guard, "prefixWitness(bytes32)", anchor, s.raw_hex(s.keccak(encoded)), "bool")
        if not witness:
            result["reason"] = "priority_prefix_witness_missing"
    return result


def context(rpc, settings, limits, item, now, simulate=True):
    head, anchor = anchored_head(rpc, settings, limits, now)
    gate, accepted = settings["proof_gate"], item["sidecar"]["accepted_package"]
    s.require(s.keccak(s.raw_hex(rpc.call("eth_getCode", [gate, anchor]))) == limits["gate_code_hash"], "gate_code_changed")
    expected = (("chain()", "address", settings["chain_address"]), ("childChainId()", "uint", s.uint(settings["execution_chain_id"])),
                ("sequencer()", "address", settings["sequencer"]), ("policyHash()", "bytes32", settings["policy_hash"]),
                ("productionVkHash()", "bytes32", settings["vk_hash"]))
    for signature, kind, value in expected:
        s.require(call_word(rpc, gate, signature, anchor, kind=kind) == value, "gate_configuration_changed")
    parent = call_word(rpc, gate, "lastAcceptedPackage()", anchor, kind="bytes32")
    active = call_word(rpc, gate, "serviceActive()", anchor, kind="bool")
    coordinator = call_word(rpc, gate, "coordinator()", anchor, kind="address")
    result = {"head": {k: head[k] for k in ("number", "hash", "timestamp")}, "parent": parent,
              "eligible": False, "reason": "unknown", "selected_wrapper_index": None, "current_turn": None, "turn_deadline": None,
              "period": accepted["period"], "batch_from": accepted["batchFrom"], "batch_to": accepted["batchTo"],
              "candidate_operator": accepted["wrapper"], "priority": None}
    if parent == item["package_hash"]:
        result["reason"] = "package_already_accepted"
    elif parent != accepted["parent"]:
        result["reason"] = "accepted_parent_changed"
    elif item["mode"] == "bootstrap" and (active or coordinator != s.ZERO_ADDRESS):
        result["reason"] = "bootstrap_phase_closed"
    elif item["mode"] == "service" and (not active or coordinator != settings["coordinator"]):
        result["reason"] = "service_phase_not_active"
    else:
        result["reason"] = "ready"
        if item["mode"] == "service":
            s.nonzero(limits["coordinator_code_hash"])
            s.require(s.keccak(s.raw_hex(rpc.call("eth_getCode", [coordinator, anchor]))) == limits["coordinator_code_hash"],
                      "coordinator_code_changed")
            for signature, kind, expected_value in (("acceptanceGate()", "address", gate),
                                                     ("acceptedParent()", "bytes32", accepted["parent"]),
                                                     ("nextBatch()", "uint", accepted["batchFrom"])):
                s.require(call_word(rpc, coordinator, signature, anchor, kind=kind) == expected_value,
                          "coordinator_context_changed")
            opened = call_word(rpc, coordinator, "packageOpen()", anchor, kind="bool")
            if not opened:
                result["reason"] = "package_not_open"
            else:
                normalized = {**accepted, "turn": 0, "proofHash": s.ZERO, "wrapper": s.ZERO_ADDRESS, "wrapperBeneficiary": s.ZERO_ADDRESS}
                frozen = call_word(rpc, coordinator, "frozenPackageHash()", anchor, kind="bytes32")
                turn = call_word(rpc, coordinator, "currentTurn()", anchor)
                selected = call_word(rpc, coordinator, "selectedWrapperIndex()", anchor)
                deadline = call_word(rpc, coordinator, "turnDeadline()", anchor)
                root = call_word(rpc, coordinator, "rosterRoot()", anchor, kind="bytes32")
                result.update(current_turn=turn, selected_wrapper_index=selected, turn_deadline=deadline)
                if frozen != s.struct_hash("AcceptedPackageV1", s.PACKAGE, normalized):
                    result["reason"] = "frozen_package_repaired"
                elif root != accepted["rosterRoot"]:
                    result["reason"] = "wrapper_roster_changed"
                elif turn != accepted["turn"] or selected != item["sidecar"]["candidate"]["index"]:
                    result["reason"] = "stale_wrapper_turn"
                elif deadline - s.uint(head["timestamp"]) < limits["min_turn_seconds"]:
                    result["reason"] = "insufficient_turn_time"
        if result["reason"] == "ready":
            result["priority"] = priority_context(rpc, settings, limits, item, head, anchor)
            result["reason"] = result["priority"]["reason"]
        if result["reason"] == "ready":
            contract = gate if item["mode"] == "bootstrap" else coordinator
            signature = ("bootstrapDigest" if item["mode"] == "bootstrap" else "packageDigest") + "(" + abi_tuple(s.PACKAGE) + ")"
            s.require(call_word(rpc, contract, signature, anchor, s.encode_fields(s.PACKAGE, accepted), "bytes32") == item["digest"],
                      "onchain_signing_domain_mismatch")
            if simulate:
                try:
                    result_data = rpc.call("eth_call", [{"from": limits["account"], "to": gate, "data": item["calldata"],
                                                         "value": "0x0", "gas": hex(limits["gas_limit"])}, anchor])
                    s.require(result_data == "0x", "unexpected_gate_simulation_output")
                except RpcError:
                    result["reason"] = "gate_simulation_reverted"
            if result["reason"] == "ready":
                result["eligible"] = True
    check_anchor(rpc, settings, head)
    return result


def acceptance_topics(item):
    accepted = item["sidecar"]["accepted_package"]
    return [s.keccak(b"ServicePackageAccepted(bytes32,uint64,uint64,bool)"), item["package_hash"],
            "0x" + s.word(accepted["batchFrom"]).hex(), "0x" + s.word(accepted["batchTo"]).hex()]


def matching_log(log, settings, item):
    return (isinstance(log, dict) and isinstance(log.get("address"), str)
            and log["address"].lower() == settings["proof_gate"]
            and log.get("topics") == acceptance_topics(item) and not log.get("removed", False)
            and log.get("data") == "0x" + s.word(1 if item["mode"] == "bootstrap" else 0).hex())


class Relay:
    def __init__(self, settings, limits, rpc, wallet=None, store=None, clock=time.time):
        self.settings, self.limits, self.rpc, self.wallet, self.store, self.clock = settings, limits, rpc, wallet, store, clock
        self.state = store.load(settings, limits) if store else None

    def save(self, execute):
        if execute:
            self.store.save(self.state)

    def stage(self, item):
        s.require(self.store is not None, "relay_store_required")
        operation_id = item["package_hash"][2:]
        existing = self.state["operations"].get(operation_id)
        if existing:
            s.require(existing["artifact_hash"] == sha256(item), "package_authorization_already_frozen")
            return operation_id
        s.require(len(self.state["operations"]) < 256, "relay_operation_limit")
        observed = context(self.rpc, self.settings, self.limits, item, int(self.clock()), simulate=False)
        destination = self.store.root / (operation_id + ".json")
        if destination.exists():
            s.require(s.read_json(destination, private=True) == item, "orphan_artifact_mismatch")
        else:
            s.write_new(destination, item)
        self.state["operations"][operation_id] = {
            "status": "staged", "artifact_hash": sha256(item), "package_hash": item["package_hash"],
            "mode": item["mode"], "first_head": observed["head"], "unsigned_transaction": None,
            "raw_transaction": None, "transaction_hash": None, "broadcast_attempts": 0,
            "last_broadcast_at": None, "last_error": None, "receipt": None, "acceptance": None,
            "context": observed}
        self.save(True)
        return operation_id

    def load_artifact(self, operation_id):
        s.require(operation_id in self.state["operations"], "unknown_relay_operation")
        op = self.state["operations"][operation_id]
        item = s.read_json(self.store.root / (operation_id + ".json"), private=True)
        s.require(sha256(item) == op["artifact_hash"] and item["package_hash"] == op["package_hash"], "relay_artifact_changed")
        expected = artifact(self.settings, item["sidecar"], item["proof_data"])
        s.require(expected == item, "invalid_frozen_relay_artifact")
        if op["raw_transaction"] is not None:
            s.require(validate_raw_transaction(op["raw_transaction"], op["unsigned_transaction"]) == op["transaction_hash"],
                      "durable_transaction_changed")
        return item

    def export_handoff(self, operation_id, work_directory, execute=False):
        """Export a receipt hint; the node independently authenticates every chain claim."""
        item = self.load_artifact(operation_id)
        op = self.state["operations"][operation_id]
        s.require(op["status"] in ("accepted", "accepted_elsewhere"), "relay_acceptance_not_confirmed")
        acceptance = op["acceptance"]
        s.require(isinstance(acceptance, dict) and acceptance.get("transaction_hash"),
                  "acceptance_transaction_hash_required")
        s.nonzero(acceptance["transaction_hash"])
        directory = Path(work_directory)
        s.require(directory.is_absolute(), "absolute_work_directory_required")
        for component in (directory, *directory.parents):
            s.require(not component.is_symlink(), "work_directory_symlink_forbidden")
        info = directory.lstat()
        s.require(stat.S_ISDIR(info.st_mode) and info.st_uid == os.getuid() and not info.st_mode & 0o077,
                  "private_work_directory_required")
        path = directory / "work.json"
        s.require(path.lstat().st_nlink == 1, "linked_work_file_forbidden")
        data = s.read(path, private=True, maximum=2 * 1024 * 1024)
        try:
            work = json.loads(data, object_pairs_hook=s._unique)
        except (ValueError, UnicodeError):
            raise s.Error("invalid_node_work") from None
        s.exact(work, ("schema_version", "execution_chain_id", "settlement_chain_id", "chain_address",
                       "gate", "gate_code_hash", "coordinator", "coordinator_code_hash", "policy_hash",
                       "production_vk_hash", "sequencer", "timelock", "required_confirmations",
                       "batch_from", "batch_to", "proof_data", "batch_outputs"))
        accepted = item["sidecar"]["accepted_package"]
        expected = {"execution_chain_id": self.settings["execution_chain_id"],
                    "settlement_chain_id": self.settings["settlement_chain_id"],
                    "chain_address": self.settings["chain_address"], "gate": self.settings["proof_gate"],
                    "gate_code_hash": self.limits["gate_code_hash"], "policy_hash": self.settings["policy_hash"],
                    "production_vk_hash": self.settings["vk_hash"], "sequencer": self.settings["sequencer"],
                    "batch_from": accepted["batchFrom"], "batch_to": accepted["batchTo"],
                    "proof_data": item["proof_data"], "batch_outputs": item["sidecar"]["batch_outputs"]}
        s.require(work["schema_version"] == 1 and all(work[key] == value for key, value in expected.items()),
                  "node_work_does_not_match_accepted_artifact")
        if item["mode"] == "bootstrap":
            s.require(work["coordinator"] == s.ZERO_ADDRESS and work["coordinator_code_hash"] == s.ZERO,
                      "bootstrap_node_work_requires_no_coordinator")
        else:
            s.require(work["coordinator"] == self.settings["coordinator"]
                      and work["coordinator_code_hash"] == self.limits["coordinator_code_hash"],
                      "node_coordinator_pin_mismatch")
        s.nonzero(work["timelock"], 20)
        s.uint(work["required_confirmations"], 64)
        hint = {"schema_version": 1, "work_hash": s.keccak(data),
                "transaction_hash": acceptance["transaction_hash"], "sidecar": item["sidecar"]}
        destination = directory / "relay.json"
        if execute:
            if destination.exists() or destination.is_symlink():
                s.read(destination, private=True, maximum=2 * 1024 * 1024)
                s.require(destination.lstat().st_nlink == 1, "linked_handoff_file_forbidden")
            atomic_json(destination, hint)
        return {"work_hash": hint["work_hash"], "transaction_hash": hint["transaction_hash"],
                "handoff": str(destination), "written": execute,
                "next_action": "node_verifies_canonical_native_gate_receipt"}

    def _canonical(self, block_number, block_hash, head):
        s.require(s.uint(block_number) <= s.uint(head["number"]), "receipt_after_observed_head")
        block = self.rpc.call("eth_getBlockByNumber", [block_number, False])
        s.require(isinstance(block, dict) and isinstance(block.get("hash"), str), "canonical_block_unavailable")
        return block["hash"] == block_hash

    def _confirmations(self, block_number, head):
        return s.uint(head["number"]) - s.uint(block_number) + 1

    def _observe_receipt(self, op, item, observed):
        head = observed["head"]
        receipt = self.rpc.call("eth_getTransactionReceipt", [op["transaction_hash"]]) if op["transaction_hash"] else None
        if op["receipt"] and not self._canonical(op["receipt"]["block_number"], op["receipt"]["block_hash"], head):
            op["receipt"] = None
        if receipt is not None:
            s.require(isinstance(receipt, dict) and receipt.get("transactionHash") == op["transaction_hash"]
                      and isinstance(receipt.get("from"), str) and isinstance(receipt.get("to"), str)
                      and receipt["from"].lower() == self.limits["account"]
                      and receipt["to"].lower() == self.settings["proof_gate"], "receipt_identity_mismatch")
            s.nonzero(receipt.get("blockHash"))
            s.require(s.uint(receipt.get("status")) in (0, 1), "invalid_receipt_status")
            if self._canonical(receipt["blockNumber"], receipt["blockHash"], head):
                if s.uint(receipt["status"]) == 1:
                    s.require(type(receipt.get("logs")) is list
                              and any(matching_log(log, self.settings, item) for log in receipt["logs"]),
                              "successful_receipt_missing_acceptance")
                op["receipt"] = {"transaction_hash": op["transaction_hash"], "block_number": receipt["blockNumber"],
                                 "block_hash": receipt["blockHash"], "status": s.uint(receipt["status"])}
        acceptance = op["acceptance"]
        if acceptance and not self._canonical(acceptance["block_number"], acceptance["block_hash"], head):
            op["acceptance"] = None
        if op["receipt"] and op["receipt"]["status"] == 1:
            op["acceptance"] = {**op["receipt"], "source": "receipt_event"}
        elif observed["parent"] == item["package_hash"] and op["acceptance"] is None:
            op["acceptance"] = {"transaction_hash": None, "block_number": head["number"], "block_hash": head["hash"],
                                "source": "gate_parent"}
        if (op["acceptance"] is None or op["acceptance"].get("transaction_hash") is None) and observed["parent"] != item["sidecar"]["accepted_package"]["parent"]:
            logs = self.rpc.call("eth_getLogs", [{"address": self.settings["proof_gate"],
                                                 "fromBlock": op["first_head"]["number"], "toBlock": head["number"],
                                                 "topics": acceptance_topics(item)}])
            s.require(type(logs) is list and len(logs) <= 1, "ambiguous_acceptance_log")
            if logs:
                log = logs[0]
                s.require(matching_log(log, self.settings, item), "acceptance_log_mismatch")
                s.nonzero(log.get("transactionHash"))
                s.nonzero(log.get("blockHash"))
                if self._canonical(log["blockNumber"], log["blockHash"], head):
                    op["acceptance"] = {"transaction_hash": log["transactionHash"], "block_number": log["blockNumber"],
                                        "block_hash": log["blockHash"], "source": "gate_event"}
        check_anchor(self.rpc, self.settings, head)

    def _new_transaction(self, op, item, observed):
        anchor = {"blockHash": observed["head"]["hash"], "requireCanonical": True}
        account = self.limits["account"]
        latest = s.uint(self.rpc.call("eth_getTransactionCount", [account, anchor]))
        pending = s.uint(self.rpc.call("eth_getTransactionCount", [account, "pending"]))
        s.require(latest == pending, "wallet_has_other_pending_nonce")
        balance = s.uint(self.rpc.call("eth_getBalance", [account, anchor]))
        s.require(balance >= self.limits["gas_limit"] * s.uint(self.limits["max_fee_per_gas"]), "insufficient_fee_balance")
        unsigned = {"type": "0x2", "chainId": self.settings["settlement_chain_id"], "from": account,
                    "to": self.settings["proof_gate"], "value": "0x0", "data": item["calldata"], "nonce": hex(latest),
                    "gas": hex(self.limits["gas_limit"]), "maxFeePerGas": self.limits["max_fee_per_gas"],
                    "maxPriorityFeePerGas": self.limits["max_priority_fee_per_gas"], "accessList": []}
        if op["unsigned_transaction"] is not None:
            s.require(op["unsigned_transaction"] == unsigned, "reserved_transaction_or_nonce_changed")
        check_anchor(self.rpc, self.settings, observed["head"])
        return unsigned

    def inspect(self, item):
        observed = context(self.rpc, self.settings, self.limits, item, int(self.clock()))
        return {"package_hash": item["package_hash"], "mode": item["mode"], "context": observed,
                "next_action": "stage_and_sign" if observed["eligible"] else "wait_or_refresh_authorization"}

    def step(self, operation_id, execute=False):
        s.require(self.store is not None, "relay_store_required")
        item = self.load_artifact(operation_id)
        op = self.state["operations"][operation_id]
        now = int(self.clock())
        observed = context(self.rpc, self.settings, self.limits, item, now, simulate=False)
        op["context"] = observed
        self._observe_receipt(op, item, observed)
        receipt, acceptance = op["receipt"], op["acceptance"]
        confirmations = self.limits["confirmations"]
        if receipt:
            if self._confirmations(receipt["block_number"], observed["head"]) >= confirmations:
                op["status"] = "accepted" if receipt["status"] else "reverted"
                if receipt["status"] == 0 and acceptance and self._confirmations(acceptance["block_number"], observed["head"]) >= confirmations:
                    op["status"] = "accepted_elsewhere"
                action = "complete"
            else:
                op["status"], action = "mined_unconfirmed", "wait_for_confirmations"
            self.save(execute)
            return self.summary(operation_id, action)
        if acceptance:
            if op["raw_transaction"]:
                op["status"], action = "accepted_with_pending_nonce", "wait_for_own_nonce_receipt"
            elif self._confirmations(acceptance["block_number"], observed["head"]) >= confirmations:
                op["status"], action = "accepted_elsewhere", "complete"
            else:
                op["status"], action = "accepted_unconfirmed", "wait_for_confirmations"
            self.save(execute)
            return self.summary(operation_id, action)
        if op["raw_transaction"]:
            anchor = {"blockHash": observed["head"]["hash"], "requireCanonical": True}
            latest_nonce = s.uint(self.rpc.call("eth_getTransactionCount", [self.limits["account"], anchor]))
            if latest_nonce > s.uint(op["unsigned_transaction"]["nonce"]):
                op["status"] = "nonce_conflict"
                self.save(execute)
                return self.summary(operation_id, "investigate_nonce_without_receipt")
            s.require(latest_nonce == s.uint(op["unsigned_transaction"]["nonce"]), "reserved_nonce_ahead_of_chain")
        if not observed["eligible"]:
            if op["raw_transaction"]:
                op["status"] = "authorization_stale_inflight"
                action = "wait_for_receipt_no_resign"
            elif observed["reason"] in ("stale_wrapper_turn", "frozen_package_repaired", "accepted_parent_changed", "bootstrap_phase_closed"):
                op["status"] = "stale_unsigned"
                action = "new_artifact_requires_fresh_endorsements"
            else:
                op["status"], action = "waiting_context", "wait_for_eligible_context"
            self.save(execute)
            return self.summary(operation_id, action)
        for other_id, other in self.state["operations"].items():
            if other_id != operation_id and other["status"] not in TERMINAL and other["unsigned_transaction"] is not None:
                return self.summary(operation_id, "wait_for_existing_wallet_reservation")
        observed = context(self.rpc, self.settings, self.limits, item, now, simulate=True)
        op["context"] = observed
        if not observed["eligible"]:
            self.save(execute)
            return self.summary(operation_id, "wait_for_gate_preflight")
        if not op["raw_transaction"]:
            unsigned = self._new_transaction(op, item, observed)
            if not execute:
                return self.summary(operation_id, "would_request_wallet_signature")
            s.require(self.wallet is not None, "trusted_wallet_rpc_required")
            op["unsigned_transaction"], op["status"] = unsigned, "signing_uncertain"
            self.save(True)
            # eth_signTransaction must be a signing-only wallet method. No send method is
            # used until the independently decoded raw bytes and their hash are durable.
            s.require(s.uint(self.wallet.call("eth_chainId", [])) == s.uint(self.settings["settlement_chain_id"]),
                      "wrong_wallet_chain")
            signed = self.wallet.call("eth_signTransaction", [unsigned])
            raw = signed.get("raw") if isinstance(signed, dict) else signed
            tx_hash = validate_raw_transaction(raw, unsigned)
            op.update(raw_transaction=raw, transaction_hash=tx_hash, status="signed")
            self.save(True)
            observed = context(self.rpc, self.settings, self.limits, item, int(self.clock()), simulate=False)
            op["context"] = observed
            if not observed["eligible"]:
                op["status"] = "authorization_stale_inflight"
                self.save(True)
                return self.summary(operation_id, "wait_for_receipt_no_resign")
        if op["broadcast_attempts"] >= self.limits["max_broadcast_attempts"]:
            self.save(execute)
            return self.summary(operation_id, "broadcast_limit_reached_reconcile_only")
        if op["last_broadcast_at"] is not None:
            s.require(now >= op["last_broadcast_at"], "relay_clock_rollback")
            if now - op["last_broadcast_at"] < self.limits["rebroadcast_interval_seconds"]:
                self.save(execute)
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
        self.save(True)
        return self.summary(operation_id, "reconcile_transaction_hash")

    def summary(self, operation_id, action=None):
        op = self.state["operations"][operation_id]
        fields = ("status", "package_hash", "mode", "transaction_hash", "broadcast_attempts", "last_error", "receipt", "acceptance", "context")
        result = {"operation_id": operation_id, **{field: copy.deepcopy(op[field]) for field in fields}}
        if action:
            result["next_action"] = action
        return result


def connections(path, timeout, need_wallet):
    value = s.read_json(path, private=True)
    s.exact(value, ("read_url", "read_authorization", "wallet_url", "wallet_authorization"))
    rpc = Rpc(value["read_url"], value["read_authorization"], timeout)
    wallet = Rpc(value["wallet_url"], value["wallet_authorization"], timeout) if need_wallet else None
    return rpc, wallet


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--config", required=True)
    parser.add_argument("--policy", required=True)
    parser.add_argument("--rpc-file")
    parser.add_argument("--state-dir")
    parser.add_argument("--execute", action="store_true")
    commands = parser.add_subparsers(dest="command", required=True)
    for name in ("inspect", "stage"):
        command = commands.add_parser(name)
        command.add_argument("--sidecar", required=True)
        command.add_argument("--proof-data", required=True, help="Text file containing exact 0x-prefixed native proofData")
    step = commands.add_parser("step")
    step.add_argument("--operation-id", required=True)
    export = commands.add_parser("export-handoff")
    export.add_argument("--operation-id", required=True)
    export.add_argument("--work-dir", required=True)
    commands.add_parser("status")
    args = parser.parse_args(argv)
    try:
        settings, limits = s.config(s.read_json(args.config)), policy(s.read_json(args.policy))
        if args.command in ("inspect", "stage"):
            item = artifact(settings, s.read_json(args.sidecar), s.read(args.proof_data).decode().strip())
        if args.command in ("status", "export-handoff"):
            s.require(args.state_dir, "state_directory_required")
            relay = Relay(settings, limits, None, store=Store(args.state_dir))
            if args.command == "status":
                result = [relay.summary(key) for key in relay.state["operations"]]
            else:
                with relay.store.lock() if args.execute else nullcontext():
                    relay = Relay(settings, limits, None, store=relay.store)
                    result = relay.export_handoff(args.operation_id, args.work_dir, args.execute)
        else:
            s.require(args.rpc_file, "trusted_rpc_file_required")
            rpc, wallet = connections(args.rpc_file, limits["rpc_timeout_seconds"], args.execute and args.command == "step")
            if args.command == "inspect" or args.command == "stage" and not args.execute:
                result = Relay(settings, limits, rpc).inspect(item)
            else:
                s.require(args.state_dir, "state_directory_required")
                if args.command == "stage" and not Path(args.state_dir).exists():
                    store = Store.initialize(args.state_dir, settings, limits)
                else:
                    store = Store(args.state_dir)
                with store.lock() if args.execute else nullcontext():
                    relay = Relay(settings, limits, rpc, wallet, store)
                    if args.command == "stage":
                        operation_id = relay.stage(item)
                        result = relay.summary(operation_id, "step_requires_execute_for_signing")
                    else:
                        result = relay.step(args.operation_id, args.execute)
        print(json.dumps(result, sort_keys=True))
    except (s.Error, OSError, UnicodeError) as error:
        print(json.dumps({"error": str(error) if isinstance(error, s.Error) else "relay_file_io_failure"}), file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
