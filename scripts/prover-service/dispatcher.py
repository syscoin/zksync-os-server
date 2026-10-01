#!/usr/bin/env python3
"""Opt-in trusted FRI dispatcher. Run one protected journal per sequencer/period/phase."""

import argparse
import copy
import hashlib
import json
import os
from pathlib import Path
import sys
import tempfile
import time
import urllib.parse

import service as s

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "prover-rental"))
import job
import sentry
from runpod import Error as TransportError, Store, atomic_json, sync_dir

REQUEST = [("journalId", "bytes32"), ("account", "address"), ("subscriptionHash", "bytes32"),
           ("period", "uint64"), ("nonce", "uint64"), ("expiresAt", "uint64")]
MAX_OPERATIONS = 2000


def decode(raw):
    try:
        return json.loads(raw, object_pairs_hook=s._unique,
                          parse_constant=lambda _: s.require(False, "nonfinite_json_value"))
    except (ValueError, UnicodeError):
        raise s.Error("invalid_json") from None


class Rpc:
    def __init__(self, url, network):
        self.url = sentry.endpoint_url(url)
        self.network = network

    def call(self, method, params):
        status, _, raw = self.network.request(self.url, "POST", s.canonical(
            {"jsonrpc": "2.0", "id": 1, "method": method, "params": params}), maximum=s.MAX_FILE)
        s.require(status == 200, "rpc_transport_failed")
        value = decode(raw)
        s.require(type(value) is dict and value.get("id") == 1 and "error" not in value
                  and "result" in value, "rpc_failed")
        return value["result"]

    def contract(self, address, signature, values, anchor):
        data = s.cast("calldata", signature, *map(str, values))
        result = self.call("eth_call", [{"to": address, "data": data},
                                      {"blockHash": anchor, "requireCanonical": True}])
        return s.raw_hex(result)


def fixed_word(raw):
    s.require(len(raw) == 32, "invalid_rpc_word")
    return int.from_bytes(raw, "big")


def enrollment_snapshot(settings, subscriptions, period, rpc):
    s.config(settings)
    s.uint(period, 64)
    s.require(type(subscriptions) is list and 0 < len(subscriptions) <= 256, "invalid_subscription_count")
    s.require(rpc.call("eth_chainId", []) == settings["registry_chain_id"], "wrong_rpc_chain")
    block = rpc.call("eth_getBlockByNumber", ["finalized", False])
    s.require(type(block) is dict, "finalized_block_required")
    anchor = s.nonzero(block["hash"])
    timestamp = s.uint(block["timestamp"])
    registry = settings["registry"]
    def view(signature, *values):
        return rpc.contract(registry, signature, values, anchor)
    s.require(view("policyHash()") == s.raw_hex(settings["policy_hash"]), "registry_policy_mismatch")
    lane = view("supportedLane(uint256)", s.uint(settings["execution_chain_id"]))
    s.require(len(lane) == 64 and lane[:32] == b"\0" * 12 + s.raw_hex(settings["chain_address"]),
              "registry_chain_address_mismatch")
    s.require(fixed_word(view("dutiesPerRound()")) == settings["duties_per_round"], "registry_quota_mismatch")
    start, seconds = fixed_word(view("startTime()")), fixed_word(view("periodSeconds()"))
    first = fixed_word(view("firstServicePeriod()"))
    active = fixed_word(view("serviceActive()"))
    s.require(seconds > 0 and active in (0, 1), "invalid_registry_clock")
    pinned_vk = lane[32:]
    # The first native-accepted bootstrap package installs this pin. The configured nonzero VK
    # and canonical native evidence still bind the initial work before that package exists.
    s.require(pinned_vk == s.raw_hex(settings["vk_hash"]) or not active and pinned_vk == bytes(32),
              "registry_vk_mismatch")
    phase = "service" if active else "bootstrap"
    if active:
        s.require(timestamp >= start and period == (timestamp - start) // seconds,
                  "current_service_period_required")
    else:
        s.require(period == first and timestamp < start + first * seconds, "bootstrap_window_closed")
    end = start + (period + 1) * seconds if active else start + period * seconds
    enrolled_count = fixed_word(view("friSubscriberCount(address,uint64)", settings["sequencer"], period))
    s.require(enrolled_count <= 256, "enrollment_exceeds_dispatcher_capacity")
    eligible_accounts, enumerated = set(), set()
    for index in range(enrolled_count):
        encoded = view("friSubscriberAt(address,uint64,uint256)", settings["sequencer"], period, index)
        s.require(len(encoded) == 32 and encoded[:12] == bytes(12), "invalid_enrollment_account")
        account = s.nonzero("0x" + encoded[12:].hex(), 20)
        s.require(account not in enumerated, "duplicate_enrollment_account")
        enumerated.add(account)
        eligible = fixed_word(view("isEligibleFriSubscriber(address,address,uint64)", account,
                                   settings["sequencer"], period))
        s.require(eligible in (0, 1), "invalid_enrollment_eligibility")
        if eligible:
            eligible_accounts.add(account)
    normalized, accounts, operators = [], set(), set()
    for signed in sorted(subscriptions, key=lambda entry: entry["subscription"]["account"]):
        s.exact(signed, ("subscription", "signature"))
        subscription = signed["subscription"]
        request = s.subscription_request(settings, subscription)
        s.verify_eoa(request, signed["signature"])
        account, operator = subscription["account"], subscription["operator"]
        s.require(account not in accounts and operator not in operators, "duplicate_subscription_identity")
        s.require(subscription["firstPeriod"] <= period <= subscription["lastPeriod"]
                  and subscription["services"] & 1, "inactive_subscription")
        hashed = request["struct_hash"]
        s.require(view("subscriptionAt(address,address,uint64)", account, settings["sequencer"], period)
                  == s.raw_hex(hashed), "subscription_not_enrolled")
        s.require(view("operatorAccountAt(address,uint64)", operator, period)
                  == b"\0" * 12 + s.raw_hex(account), "operator_not_enrolled")
        s.require(view("subscription(bytes32)", hashed) == s.encode_fields(s.SUBSCRIPTION, subscription),
                  "stored_subscription_mismatch")
        s.require(fixed_word(view("seniorBonus(address)", account)) > 0, "senior_membership_required")
        accounts.add(account)
        operators.add(operator)
        normalized.append(copy.deepcopy(signed))
    s.require(accounts == eligible_accounts, "subscription_snapshot_omits_or_adds_eligible_accounts")
    return normalized, {"block_hash": anchor, "timestamp": timestamp, "phase": phase, "ends_at": end}


def initialize(root, settings, subscriptions, period, endpoint, rpc, gateway=None):
    subscriptions, anchor = enrollment_snapshot(settings, subscriptions, period, rpc)
    s.require(len(subscriptions) * settings["duties_per_round"] <= MAX_OPERATIONS,
              "full_roster_quota_exceeds_dispatcher_capacity")
    endpoint = sentry.endpoint_url(endpoint)
    lanes = {"child": {"settings": settings, "endpoint": endpoint}}
    if gateway is not None:
        s.exact(gateway, ("settings", "endpoint"))
        other = s.config(gateway["settings"])
        for field in ("registry_chain_id", "registry", "policy_hash", "sequencer", "duties_per_round"):
            s.require(other[field] == settings[field], "shared_service_identity_required")
        s.require(settings["execution_chain_id"] == settings["registry_chain_id"]
                  and settings["settlement_chain_id"] == other["execution_chain_id"]
                  and other["execution_chain_id"] != settings["execution_chain_id"]
                  and other["settlement_chain_id"] not in (settings["execution_chain_id"], other["execution_chain_id"]),
                  "invalid_child_gateway_topology")
        other_subscriptions, other_anchor = enrollment_snapshot(other, subscriptions, period, rpc)
        s.require(other_anchor == anchor and other_subscriptions == subscriptions, "enrollment_snapshot_changed")
        lanes["gateway"] = {"settings": other, "endpoint": sentry.endpoint_url(gateway["endpoint"])}
        s.require(lanes["gateway"]["endpoint"] != endpoint, "independent_lane_endpoint_required")
    root = Path(root)
    s.require(root.is_absolute(), "absolute_state_directory_required")
    root.mkdir(mode=0o700)
    sync_dir(root.parent)
    store = Store(root)
    identity = {"settings": settings, "subscriptions": subscriptions, "period": period,
                "endpoint": endpoint, "lanes": lanes, "enrollment": anchor}
    journal_id = s.keccak(s.canonical(identity))
    state = {"schema_version": 1, **identity, "journal_id": journal_id, "cursor": journal_id,
             "next_account": 0, "next_lane": 0, "next_operation": 0, "pending_pick": None, "operations": {},
             "accounts": {item["subscription"]["account"]: {"nonce": 0, "offered": 0, "ready": None}
                          for item in subscriptions}}
    s.write_new(root / "state.json", state)
    return store


class Dispatcher:
    def __init__(self, store, network=None, auth=None, now=None):
        self.store, self.network, self.auth = store, network, auth
        self.now = int(time.time()) if now is None else now
        self.state = s.read_json(store.root / "state.json", private=True)
        s.require(self.state["schema_version"] == 1, "unsupported_journal")
        self.settings = s.config(self.state["settings"])
        s.require(set(self.state["lanes"]) in ({"child"}, {"child", "gateway"}), "invalid_journal_lanes")
        for lane in self.state["lanes"].values():
            s.config(lane["settings"])
            sentry.endpoint_url(lane["endpoint"])
        if len(self.state["lanes"]) > 1 and auth is not None:
            s.require(type(auth) is dict and set(auth) == set(self.state["lanes"]), "lane_credentials_required")
        self.subscriptions = {item["subscription"]["account"]: item["subscription"]
                              for item in self.state["subscriptions"]}

    def lane(self, operation):
        return self.state["lanes"][self.state["operations"][operation]["lane"]]

    def authorization(self, operation):
        return self.auth[self.state["operations"][operation]["lane"]] if type(self.auth) is dict else self.auth

    def save(self):
        payload = s.canonical(self.state)
        s.require(len(payload) <= s.MAX_FILE, "journal_capacity_reached")
        fd, temporary = tempfile.mkstemp(prefix=".dispatcher-", dir=self.store.root)
        try:
            with os.fdopen(fd, "wb") as output:
                output.write(payload)
                output.flush()
                os.fsync(output.fileno())
            os.replace(temporary, self.store.root / "state.json")
            sync_dir(self.store.root)
        finally:
            Path(temporary).unlink(missing_ok=True)

    def live(self):
        s.require(self.now < self.state["enrollment"]["ends_at"], "dispatch_window_closed")

    def directory(self, operation):
        s.require(operation in self.state["operations"], "unknown_operation")
        return self.store.root / operation

    def request(self, account, expires_at):
        self.live()
        subscription = self.subscriptions.get(account)
        s.require(subscription is not None, "unknown_account")
        s.require(self.now < s.uint(expires_at, 64) <= self.state["enrollment"]["ends_at"],
                  "invalid_request_expiry")
        request = s.typed_request("ServiceWorkRequestV1", REQUEST,
                              {"journalId": self.state["journal_id"], "account": account,
                               "subscriptionHash": s.struct_hash("ProverSubscriptionV1", s.SUBSCRIPTION, subscription),
                               "period": self.state["period"], "nonce": self.state["accounts"][account]["nonce"],
                               "expiresAt": expires_at}, "ZkSysServiceDispatcher",
                              self.settings["registry_chain_id"], self.settings["registry"], subscription["operator"])
        # The signed journal identity commits to these scopes; expose them for operator review.
        request["work_scopes"] = [{key: lane["settings"][key] for key in
                                   ("execution_chain_id", "chain_address", "settlement_chain_id", "vk_hash")}
                                  for lane in self.state["lanes"].values()]
        return request

    def ready(self, request, signature):
        message = request["typed_data"]["message"]
        expected = self.request(message["account"], message["expiresAt"])
        s.require(request == expected, "stale_or_foreign_work_request")
        s.verify_eoa(expected, signature)
        account = message["account"]
        entry = self.state["accounts"][account]
        s.require((entry["ready"] is None or entry["ready"]["request"]["typed_data"]["message"]["expiresAt"] <= self.now)
                  and entry["offered"] < self.settings["duties_per_round"],
                  "request_already_pending_or_quota_offered")
        s.require(not any(op["account"] == account and op["status"] in
                          ("pick_pending", "picked", "offered", "submission_pending")
                          for op in self.state["operations"].values()), "account_has_live_job")
        entry["ready"] = {"request": expected, "signature": signature}
        entry["nonce"] += 1
        self.save()

    def next_account(self):
        accounts = sorted(self.state["accounts"])
        for offset in range(len(accounts)):
            index = (self.state["next_account"] + offset) % len(accounts)
            account, entry = accounts[index], self.state["accounts"][accounts[index]]
            ready = entry["ready"]
            if ready and ready["request"]["typed_data"]["message"]["expiresAt"] > self.now:
                return account, index
        return None

    def pick(self):
        self.live()
        s.require(self.state["pending_pick"] is None, "recover_pending_pick_first")
        selected = self.next_account()
        if selected is None:
            return "no_authenticated_demand"
        s.require(len(self.state["operations"]) < MAX_OPERATIONS, "journal_capacity_reached")
        account, index = selected
        operation = f"job-{self.state['next_operation']:06d}"
        directory = self.store.root / operation
        directory.mkdir(mode=0o700, exist_ok=True)
        Store(directory)
        s.require(not any(directory.iterdir()), "unexpected_operation_files")
        sync_dir(self.store.root)
        ready = self.state["accounts"][account]["ready"]
        lanes = list(self.state["lanes"])
        lane_name = lanes[self.state["next_lane"]]
        lane = self.state["lanes"][lane_name]
        self.state["next_lane"] = (self.state["next_lane"] + 1) % len(lanes)
        self.state["operations"][operation] = {"status": "pick_pending", "account": account,
                                                "lane": lane_name,
                                                "account_index": index, "request": ready,
                                                "assignment": None, "submit_attempts": 0}
        self.state["pending_pick"] = operation
        self.state["next_operation"] += 1
        self.save()
        query = urllib.parse.urlencode({"id": "service-dispatcher", "supported_vk_hashes": lane["settings"]["vk_hash"],
                                        "nonempty_only": "true", "max_fri_pick_response_bytes": job.MAX_PICK["FRI"]})
        status, headers, raw = self.network.request(lane["endpoint"] + "prover-jobs/v1/FRI/pick?" + query,
                                                   "POST", authorization=self.authorization(operation), maximum=job.MAX_PICK["FRI"])
        unleased = {k.lower(): v for k, v in headers.items()}.get("x-syscoin-prover-pick-outcome")
        if status == 204 or unleased == "unleased" and status in (429, 500):
            del self.state["operations"][operation]
            self.state["pending_pick"] = None
            self.save()
            return "no_job"
        s.require(status == 200, "pick_uncertain_keep_journal")
        job.write_new(directory / "picked-wire.json", raw)
        return self.recover_pick()

    def recover_pick(self):
        operation = self.state["pending_pick"]
        s.require(operation is not None, "no_pending_pick")
        lane = self.lane(operation)
        settings = lane["settings"]
        directory = self.directory(operation)
        wire = decode(s.read(directory / "picked-wire.json", private=True, maximum=job.MAX_PICK["FRI"]))
        token = s.nonzero(wire.pop("lease_token", None))
        payload = job.validate_payload(wire, "FRI", settings["vk_hash"])
        number = payload["batch_number"]
        status, _, raw = self.network.request(lane["endpoint"] + f"prover-jobs/v1/FRI/{number}/evidence",
                                              authorization=self.authorization(operation), maximum=s.MAX_FILE)
        s.require(status == 200, "pending_evidence_unavailable_preserve_pick")
        evidence = decode(raw)
        statements = s.validate_evidence(settings, evidence)
        s.require(set(statements) == {number} and statements[number]["count"] > 0, "nonempty_exact_statement_required")
        prior = [(key, op) for key, op in self.state["operations"].items()
                 if op["assignment"] and op["assignment"]["batch_number"] == number
                 and op["lane"] == self.state["operations"][operation]["lane"]]
        op = self.state["operations"][operation]
        account = op["account"]
        previous = prior[-1] if prior else None
        s.require(previous is None or previous[1]["status"] in ("rejected", "expired"),
                  "batch_has_live_or_accepted_assignment")
        if previous:
            s.require(previous[1]["assignment"]["statement_hash"] == statements[number]["statement"],
                      "retry_statement_changed")
        subscription = self.subscriptions[account]
        assignment = {"account": account,
                      "subscription_hash": s.struct_hash("ProverSubscriptionV1", s.SUBSCRIPTION, subscription),
                      "batch_number": number, "statement_hash": statements[number]["statement"],
                      "lease_commitment": s.keccak(s.raw_hex(token)),
                      "attempt": previous[1]["assignment"]["attempt"] + 1 if previous else 1,
                      "slot": self.state["accounts"][account]["offered"], "period": self.state["period"]}
        assignment["assignment_id"] = s.keccak(s.canonical(
            {"journal_id": self.state["journal_id"], "operation": operation, **assignment}))
        if previous:
            s.require(previous[1]["assignment"]["lease_commitment"] != assignment["lease_commitment"],
                      "retry_requires_new_native_lease")
        authority = {"schema_version": 1, "stage": "FRI", "status": "picked", "vk_hash": settings["vk_hash"],
                     "bounds": {"batch_number": number}, "lease_token": token, "submission_sha256": None}
        for name, value in (("payload.json", payload), ("evidence.json", evidence), ("authority.json", authority)):
            path = directory / name
            if path.exists():
                s.require(s.read_json(path, private=True, maximum=job.MAX_PICK["FRI"]) == value,
                          "retained_pick_changed")
            else:
                job.write_new(path, s.canonical(value))
        op.update(status="picked", assignment=assignment, previous=previous[0] if previous else None,
                  previous_cursor=self.state["cursor"])
        # The live operation reserves its slot before export; only its signed offer consumes an
        # opportunity. Expiring an unsigned reservation never turns sparse demand into a failure.
        self.state["accounts"][account]["ready"] = None
        self.state["next_account"] = (op["account_index"] + 1) % len(self.subscriptions)
        self.state["cursor"] = s.keccak(s.canonical({"previous": self.state["cursor"], "assignment": assignment}))
        self.state["pending_pick"] = None
        self.save()
        return operation

    def abandon_unexported_pick(self):
        operation = self.state["pending_pick"]
        s.require(operation is not None, "no_pending_pick")
        op = self.state["operations"][operation]
        s.require(op["status"] == "pick_pending" and op["assignment"] is None
                  and not any(self.directory(operation).iterdir()), "retained_pick_cannot_be_abandoned")
        # Nothing was exported, and any unknown native capability is inaccessible. Leave that
        # lease to expire in the node; this record never becomes an opportunity or accepted duty.
        op["status"] = "unexported_pick_abandoned"
        self.state["pending_pick"] = None
        self.save()

    def manifest_payload(self, operations):
        s.require(operations and all(key in self.state["operations"] for key in operations), "unknown_operation")
        lane_name = self.state["operations"][operations[0]]["lane"]
        s.require(all(self.state["operations"][key]["lane"] == lane_name for key in operations),
                  "one_execution_lane_per_manifest")
        settings = self.state["lanes"][lane_name]["settings"]
        assignments, retries, included = [], [], set()
        def include(key):
            if key in included:
                return
            op = self.state["operations"][key]
            s.require(op["assignment"] is not None, "unassigned_operation")
            previous = op.get("previous")
            if previous:
                include(previous)
                old = self.state["operations"][previous]
                retries.append({"assignment_id": old["assignment"]["assignment_id"],
                                "next_assignment_id": op["assignment"]["assignment_id"],
                                "reason": "invalid" if old["status"] == "rejected" else "expired"})
            included.add(key)
            assignments.append(op["assignment"])
        for operation in operations:
            include(operation)
        return {"schema_version": 1, "chain_id": settings["execution_chain_id"],
                "chain_address": settings["chain_address"], "sequencer": settings["sequencer"],
                "period": self.state["period"], "previous_cursor": self.state["operations"][operations[0]]["previous_cursor"],
                "subscription_snapshot_hash": s.keccak(s.canonical(self.state["subscriptions"])),
                "assignments": assignments, "retries": retries}

    def offer_request(self, operation):
        s.require(self.state["operations"][operation]["status"] in ("picked", "offered"), "job_not_offerable")
        return s.manifest_request(self.lane(operation)["settings"], self.manifest_payload([operation]))

    def authorize(self, operation, signature):
        self.live()
        op = self.state["operations"][operation]
        s.require(op["request"]["request"]["typed_data"]["message"]["expiresAt"] > self.now, "request_expired")
        s.verify_eoa(self.offer_request(operation), signature)
        manifest = {"payload": self.manifest_payload([operation]), "sequencer_signature": signature}
        if op["status"] == "picked":
            self.state["accounts"][op["account"]]["offered"] += 1
        op["status"] = "offered"
        self.save()
        atomic_json(self.directory(operation) / "manifest.json", manifest)

    def proof_request(self, operation, proof):
        op = self.state["operations"][operation]
        s.require(op["status"] in ("offered", "submission_pending", "accepted"), "signed_offer_required")
        settings = self.lane(operation)["settings"]
        proof = job.validate_payload(proof, "FRI", settings["vk_hash"], proof=True)
        assignment = op["assignment"]
        s.require(proof["batch_number"] == assignment["batch_number"], "proof_batch_mismatch")
        evidence = s.read_json(self.directory(operation) / "evidence.json", private=True)
        statement = s.validate_evidence(settings, evidence)[proof["batch_number"]]
        duty = {"account": op["account"], "subscriptionHash": assignment["subscription_hash"],
                "batchNumber": proof["batch_number"], "statementHash": statement["statement"],
                "friProofHash": s.keccak(s.decode_proof(proof["proof"])), "transactionCount": statement["count"],
                "period": assignment["period"], "slot": assignment["slot"], "attempt": assignment["attempt"],
                "assignmentId": assignment["assignment_id"]}
        return s.duty_request(settings, duty, self.subscriptions[op["account"]])

    def accepted(self, operation, authority, proof, signature):
        directory = self.directory(operation)
        authority["status"] = "accepted"
        atomic_json(directory / "authority.json", authority)
        request = s.prepare_duty(self.lane(operation)["settings"], s.read_json(directory / "evidence.json", private=True),
                                 s.read_json(directory / "manifest.json", private=True), self.state["subscriptions"],
                                 authority, proof)
        s.verify_eoa(request, signature)
        atomic_json(directory / "duty.json", {**request["typed_data"]["message"], "operatorSignature": signature})
        self.state["operations"][operation]["status"] = "accepted"
        self.save()
        return "accepted"

    def recover_acceptance(self, operation, proof):
        number = proof["batch_number"]
        lane = self.lane(operation)
        status, _, raw = self.network.request(lane["endpoint"] + f"prover-jobs/v1/SNARK/{number}/{number}/peek",
                                              authorization=self.authorization(operation), maximum=job.MAX_PICK["SNARK"])
        if status != 200:
            return False
        retained = decode(raw)
        s.exact(retained, ("from_batch_number", "to_batch_number", "vk_hash", "fri_proofs"))
        s.require(retained == {"from_batch_number": number, "to_batch_number": number,
                              "vk_hash": lane["settings"]["vk_hash"], "fri_proofs": [proof["proof"]]},
                  "different_proof_retained_no_service_credit")
        status, _, raw = self.network.request(lane["endpoint"] + f"prover-jobs/v1/SNARK/{number}/{number}/evidence",
                                              authorization=self.authorization(operation), maximum=s.MAX_FILE)
        s.require(status == 200, "accepted_metadata_unavailable_keep_pending")
        expected = s.read_json(self.directory(operation) / "evidence.json", private=True)
        s.require(decode(raw) == expected, "accepted_metadata_changed")
        return True

    def submit(self, operation, proof, signature):
        op = self.state["operations"][operation]
        request = self.proof_request(operation, proof)
        s.verify_eoa(request, signature)
        directory = self.directory(operation)
        authority = s.read_json(directory / "authority.json", private=True)
        wire = s.canonical({**proof, "lease_token": authority["lease_token"]})
        digest = hashlib.sha256(wire).hexdigest()
        s.require(authority["submission_sha256"] in (None, digest), "submission_bytes_changed")
        path = directory / "submission.json"
        if path.exists():
            s.require(s.read(path, private=True) == wire, "submission_bytes_changed")
        else:
            job.write_new(path, wire)
        signed = {"proof": proof, "signature": signature}
        signed_path = directory / "signed-proof.json"
        if signed_path.exists():
            s.require(s.read_json(signed_path, private=True) == signed, "signed_submission_changed")
        else:
            s.write_new(signed_path, signed)
        authority["submission_sha256"] = digest
        if op["status"] == "accepted" or authority["status"] == "accepted":
            return self.accepted(operation, authority, proof, signature)
        if op["status"] == "submission_pending" and self.recover_acceptance(operation, proof):
            return self.accepted(operation, authority, proof, signature)
        if op["status"] != "submission_pending":
            self.live()
            s.require(op["request"]["request"]["typed_data"]["message"]["expiresAt"] > self.now, "offer_expired")
        op["status"] = "submission_pending"
        op["submit_attempts"] += 1
        authority["status"] = "submission_pending"
        atomic_json(directory / "authority.json", authority)
        self.save()
        status, headers, _ = self.network.request(self.lane(operation)["endpoint"] + "prover-jobs/v1/FRI/submit?id=service-dispatcher",
                                                   "POST", wire, self.authorization(operation), maximum=job.MAX_MANIFEST)
        disposition = {k.lower(): v for k, v in headers.items()}.get("x-syscoin-prover-disposition")
        if status == 204 and disposition == "accepted":
            return self.accepted(operation, authority, proof, signature)
        if status in (400, 413, 422) and disposition == "rejected" or (
                status == 409 and disposition == "rejected" and op["submit_attempts"] == 1):
            op["status"] = authority["status"] = "rejected"
            atomic_json(directory / "authority.json", authority)
            self.save()
            return "rejected"
        raise s.Error("submission_uncertain_retry_identical_bytes")

    def expire(self, operation):
        op = self.state["operations"][operation]
        s.require(op["status"] in ("picked", "offered") and self.now >=
                  op["request"]["request"]["typed_data"]["message"]["expiresAt"], "live_or_ambiguous_job_cannot_expire")
        op["status"] = "expired"
        self.save()

    def export(self, operation, destination):
        op = self.state["operations"][operation]
        s.require(op["status"] in ("offered", "submission_pending", "accepted"), "signed_offer_required")
        source = self.directory(operation)
        manifest = s.read_json(source / "manifest.json", private=True)
        settings = self.lane(operation)["settings"]
        s.verify_eoa(s.manifest_request(settings, manifest["payload"]), manifest["sequencer_signature"])
        s.require(manifest["payload"] == self.manifest_payload([operation]), "offer_manifest_changed")
        destination = Path(destination)
        destination.mkdir(mode=0o700)
        sync_dir(destination.parent)
        for name in ("payload.json", "evidence.json", "manifest.json"):
            job.write_new(destination / name, s.read(source / name, private=True, maximum=job.MAX_PICK["FRI"]))
        s.write_new(destination / "subscriptions.json", self.state["subscriptions"])
        s.write_new(destination / "config.json", settings)
        if op["status"] == "accepted":
            duty = s.read_json(source / "duty.json", private=True)
            s.verify_eoa(s.duty_request(settings, {key: duty[key] for key, _ in s.DUTY},
                                       self.subscriptions[op["account"]]), duty["operatorSignature"])
            s.write_new(destination / "duty.json", duty)

    def report(self):
        return {"journal_id": self.state["journal_id"], "cursor": self.state["cursor"],
                "phase": self.state["enrollment"]["phase"], "period": self.state["period"],
                "accounts": {account: {"offered_opportunities": data["offered"],
                                       "quota": self.settings["duties_per_round"],
                                       "quota_offered": data["offered"] == self.settings["duties_per_round"],
                                       "native_accepted": sum(op["account"] == account and op["status"] == "accepted"
                                                       for op in self.state["operations"].values())}
                             for account, data in self.state["accounts"].items()}}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--state", required=True)
    parser.add_argument("--execute", action="store_true")
    parser.add_argument("--auth-file")
    parser.add_argument("--gateway-auth-file")
    commands = parser.add_subparsers(dest="command", required=True)
    init = commands.add_parser("init")
    for flag in ("config", "subscriptions", "rpc", "endpoint"):
        init.add_argument("--" + flag, required=True)
    init.add_argument("--gateway-config")
    init.add_argument("--gateway-endpoint")
    init.add_argument("--period", required=True, type=int)
    request = commands.add_parser("work-request")
    request.add_argument("--account", required=True)
    request.add_argument("--expires-at", required=True, type=int)
    request.add_argument("--output", required=True)
    ready = commands.add_parser("ready")
    ready.add_argument("--request", required=True)
    ready.add_argument("--signature", required=True)
    commands.add_parser("pick")
    commands.add_parser("recover-pick")
    commands.add_parser("abandon-unexported-pick")
    for name in ("offer-request", "authorize", "proof-request", "submit", "expire"):
        command = commands.add_parser(name)
        command.add_argument("--operation", required=True)
        if name in ("authorize", "submit"):
            command.add_argument("--signature", required=True)
        if name in ("proof-request", "submit"):
            command.add_argument("--proof", required=True)
        if name.endswith("request"):
            command.add_argument("--output", required=True)
    manifest = commands.add_parser("manifest-request")
    manifest.add_argument("--operations", required=True, nargs="+")
    manifest.add_argument("--output", required=True)
    export = commands.add_parser("export")
    export.add_argument("--operation", required=True)
    export.add_argument("--output-directory", required=True)
    commands.add_parser("report")
    args = parser.parse_args()
    if not args.execute:
        print("dry-run: no RPC, lease, journal mutation, or submission")
        return
    network = job.NativeNetwork()
    if args.command == "init":
        s.require(bool(args.gateway_config) == bool(args.gateway_endpoint), "complete_gateway_lane_required")
        gateway = ({"settings": s.read_json(args.gateway_config), "endpoint": args.gateway_endpoint}
                   if args.gateway_config else None)
        initialize(args.state, s.read_json(args.config), s.read_json(args.subscriptions), args.period,
                   args.endpoint, Rpc(args.rpc, network), gateway)
        print("initialized")
        return
    store = Store(args.state)
    with store.lock("dispatcher.lock"):
        auth = None
        if args.command in ("pick", "recover-pick", "submit"):
            state = s.read_json(store.root / "state.json", private=True)
            auth = {"child": sentry.authorization(args.auth_file)}
            if "gateway" in state["lanes"]:
                auth["gateway"] = sentry.authorization(args.gateway_auth_file)
        dispatcher = Dispatcher(store, network, auth)
        if args.command == "work-request":
            s.write_new(args.output, dispatcher.request(args.account, args.expires_at))
        elif args.command == "ready":
            dispatcher.ready(s.read_json(args.request), s.read_json(args.signature)["signature"])
        elif args.command == "pick":
            print(dispatcher.pick())
        elif args.command == "recover-pick":
            print(dispatcher.recover_pick())
        elif args.command == "abandon-unexported-pick":
            dispatcher.abandon_unexported_pick()
        elif args.command == "offer-request":
            s.write_new(args.output, dispatcher.offer_request(args.operation))
        elif args.command == "authorize":
            dispatcher.authorize(args.operation, s.read_json(args.signature)["signature"])
        elif args.command == "proof-request":
            s.write_new(args.output, dispatcher.proof_request(args.operation, s.read_json(args.proof)))
        elif args.command == "submit":
            print(dispatcher.submit(args.operation, s.read_json(args.proof), s.read_json(args.signature)["signature"]))
        elif args.command == "expire":
            dispatcher.expire(args.operation)
        elif args.command == "manifest-request":
            payload = dispatcher.manifest_payload(args.operations)
            s.write_new(args.output, {"payload": payload, "request": s.manifest_request(dispatcher.settings, payload)})
        elif args.command == "export":
            dispatcher.export(args.operation, args.output_directory)
        else:
            print(json.dumps(dispatcher.report(), sort_keys=True))


if __name__ == "__main__":
    os.umask(0o077)
    try:
        main()
    except (s.Error, TransportError, OSError, ValueError, TypeError, KeyError) as error:
        print(str(error) if isinstance(error, (s.Error, TransportError)) else "dispatcher_failed", file=sys.stderr)
        sys.exit(1)
