#!/usr/bin/env python3
"""Replay authenticated dispatch history against an independently pinned checkpoint."""

import copy

import service as s

MAX_EVENTS = 16000
REQUEST = [("journalId", "bytes32"), ("account", "address"), ("subscriptionHash", "bytes32"),
           ("period", "uint64"), ("nonce", "uint64"), ("expiresAt", "uint64")]
CHECKPOINT = [("journalId", "bytes32"), ("eventCount", "uint64"), ("eventHead", "bytes32"),
              ("assignmentCursor", "bytes32"), ("issuedAt", "uint64"), ("chainId", "uint256"),
              ("chainAddress", "address"), ("batchFrom", "uint64"), ("batchTo", "uint64"), ("period", "uint64")]
ASSIGNMENT_FIELDS = ("assignment_id", "account", "subscription_hash", "batch_number", "statement_hash",
                     "lease_commitment", "attempt", "slot", "period")
LIVE = ("pick_pending", "picked", "offered", "submission_pending")


def checkpoint_request(bundle):
    identity, checkpoint = bundle["identity"], bundle["checkpoint"]
    settings = s.config(identity["settings"])
    s.encode_fields(CHECKPOINT, checkpoint)
    return s.typed_request("DispatchCheckpointV1", CHECKPOINT, checkpoint, "ZkSysServiceDispatcher",
                           settings["registry_chain_id"], settings["registry"], settings["sequencer"])


def work_request(identity, journal_id, subscription, nonce, expires_at):
    settings = identity["settings"]
    request = s.typed_request("ServiceWorkRequestV1", REQUEST,
                {"journalId": journal_id, "account": subscription["account"],
                 "subscriptionHash": s.struct_hash("ProverSubscriptionV1", s.SUBSCRIPTION, subscription),
                 "period": identity["period"], "nonce": nonce, "expiresAt": expires_at},
                "ZkSysServiceDispatcher", settings["registry_chain_id"], settings["registry"], subscription["operator"])
    request["work_scopes"] = [{key: identity["lanes"][name]["settings"][key] for key in
                               ("execution_chain_id", "chain_address", "settlement_chain_id", "vk_hash")}
                              for name in sorted(identity["lanes"])]
    return request


def validate_identity(identity, journal_id, trust, rpc):
    # The sequencer's own snapshot is insufficient: enumerate the complete registry at the
    # independently pinned canonical block, including eligible accounts omitted from the file.
    import dispatcher
    s.exact(identity, ("schema_version", "settings", "subscriptions", "period", "enrollment", "lanes"))
    s.require(identity["schema_version"] == 1 and s.keccak(s.canonical(identity)) == journal_id,
              "audit_journal_identity_mismatch")
    settings = s.config(identity["settings"])
    s.uint(identity["period"], 64)
    enrollment = identity["enrollment"]
    s.exact(enrollment, ("block_hash", "timestamp", "phase", "ends_at"))
    s.require(enrollment["block_hash"] == trust["enrollment_block_hash"], "audit_enrollment_anchor_mismatch")
    lanes = identity["lanes"]
    s.require(type(lanes) is dict and set(lanes) in ({"child"}, {"child", "gateway"}), "invalid_audit_lanes")
    s.require(lanes["child"]["settings"] == settings, "audit_primary_lane_changed")
    for lane in lanes.values():
        s.exact(lane, ("settings", "endpoint_commitment"))
        s.nonzero(lane["endpoint_commitment"])
        other = s.config(lane["settings"])
        for key in ("registry_chain_id", "registry", "policy_hash", "sequencer", "duties_per_round"):
            s.require(other[key] == settings[key], "audit_shared_identity_changed")
        subscriptions, anchor = dispatcher.enrollment_snapshot(other, identity["subscriptions"],
                                          identity["period"], rpc, block_hash=enrollment["block_hash"])
        s.require(subscriptions == identity["subscriptions"] and anchor == enrollment,
                  "audit_enrollment_snapshot_changed")
    if "gateway" in lanes:
        gateway = lanes["gateway"]["settings"]
        s.require(settings["execution_chain_id"] == settings["registry_chain_id"]
                  and settings["settlement_chain_id"] == gateway["execution_chain_id"]
                  and gateway["execution_chain_id"] != settings["execution_chain_id"]
                  and gateway["settlement_chain_id"] not in (settings["execution_chain_id"], gateway["execution_chain_id"])
                  and lanes["child"]["endpoint_commitment"] != lanes["gateway"]["endpoint_commitment"],
                  "audit_invalid_gateway_topology")
    s.require(len(identity["subscriptions"]) * settings["duties_per_round"] <= dispatcher.MAX_OPERATIONS,
              "audit_roster_exceeds_capacity")
    return s.EnrollmentAuthority(settings, identity["subscriptions"], identity["period"], rpc,
                                 trust["enrollment_block_hash"])


class Replay:
    """No dispatcher state, native endpoint, lease capability, or mutable journal is trusted."""

    def __init__(self, identity, journal_id):
        self.identity, self.journal_id = identity, journal_id
        self.settings = identity["settings"]
        self.lanes = {name: identity["lanes"][name] for name in sorted(identity["lanes"])}
        self.subscriptions = {entry["subscription"]["account"]: entry["subscription"]
                              for entry in identity["subscriptions"]}
        self.accounts = {account: {"nonce": 0, "offered": 0, "ready": None}
                         for account in sorted(self.subscriptions)}
        self.operations, self.pending = {}, None
        self.next_account = self.next_lane = self.next_operation = 0
        self.cursor = journal_id
        self.readiness, self.accepted = {}, {}

    def live(self, at):
        s.require(at < self.identity["enrollment"]["ends_at"], "audit_dispatch_window_closed")

    def selected(self, at):
        accounts = list(self.accounts)
        for offset in range(len(accounts)):
            index = (self.next_account + offset) % len(accounts)
            account = accounts[index]
            ready = self.accounts[account]["ready"]
            if ready and ready["request"]["typed_data"]["message"]["expiresAt"] > at:
                return account, index
        return None

    def manifest(self, operations, lane):
        settings = self.lanes[lane]["settings"]
        assignments, retries, included = [], [], set()
        def include(key):
            if key in included:
                return
            op = self.operations[key]
            previous = op.get("previous")
            if previous:
                include(previous)
                old = self.operations[previous]
                retries.append({"assignment_id": old["assignment"]["assignment_id"],
                                "next_assignment_id": op["assignment"]["assignment_id"],
                                "reason": "invalid" if old["status"] == "rejected" else "expired"})
            included.add(key)
            assignments.append(op["assignment"])
        for operation in operations:
            include(operation)
        return {"schema_version": 1, "chain_id": settings["execution_chain_id"],
                "chain_address": settings["chain_address"], "sequencer": settings["sequencer"],
                "period": self.identity["period"],
                "previous_cursor": self.operations[operations[0]]["previous_cursor"] if operations else self.cursor,
                "subscription_snapshot_hash": s.keccak(s.canonical(self.identity["subscriptions"])),
                "assignments": assignments, "retries": retries}

    def duty(self, op, duty):
        s.encode_duty(duty)
        assignment = op["assignment"]
        for source, dest in (("account", "account"), ("subscriptionHash", "subscription_hash"),
                             ("batchNumber", "batch_number"), ("statementHash", "statement_hash"),
                             ("period", "period"), ("slot", "slot"), ("attempt", "attempt"),
                             ("assignmentId", "assignment_id")):
            s.require(duty[source] == assignment[dest], "audit_duty_assignment_mismatch")
        s.require(duty["transactionCount"] == op["transaction_count"] > 0, "audit_duty_count_mismatch")
        s.nonzero(duty["friProofHash"])
        s.verify_eoa(s.duty_request(self.lanes[op["lane"]]["settings"],
                                     {key: duty[key] for key, _ in s.DUTY}, self.subscriptions[op["account"]]),
                     duty["operatorSignature"])

    def apply(self, event):
        kind, payload, at = event["kind"], event["payload"], event["at"]
        if kind == "ready":
            s.exact(payload, ("request", "signature"))
            self.live(at)
            message = payload["request"]["typed_data"]["message"]
            account = message["account"]
            s.require(account in self.accounts, "audit_unknown_ready_account")
            entry = self.accounts[account]
            expiry = s.uint(message["expiresAt"], 64)
            s.require(at < expiry <= self.identity["enrollment"]["ends_at"], "audit_invalid_ready_expiry")
            expected = work_request(self.identity, self.journal_id, self.subscriptions[account], entry["nonce"], expiry)
            s.require(expected == payload["request"], "audit_stale_or_foreign_readiness")
            s.verify_eoa(expected, payload["signature"])
            s.require((entry["ready"] is None or
                       entry["ready"]["request"]["typed_data"]["message"]["expiresAt"] <= at)
                      and entry["offered"] < self.settings["duties_per_round"]
                      and not any(op["account"] == account and op["status"] in LIVE for op in self.operations.values()),
                      "audit_duplicate_or_busy_readiness")
            entry["ready"], entry["nonce"] = payload, entry["nonce"] + 1
            self.readiness[s.keccak(s.canonical(payload))] = event["sequence"]
            return
        if kind == "pick_started":
            s.exact(payload, ("operation", "account", "lane"))
            self.live(at)
            s.require(self.pending is None and len(self.operations) < 2000, "audit_pending_or_full_dispatcher")
            selected = self.selected(at)
            s.require(selected is not None and payload["account"] == selected[0], "audit_biased_ready_selection")
            s.require(payload["operation"] == f"job-{self.next_operation:06d}"
                      and payload["lane"] == list(self.lanes)[self.next_lane], "audit_operation_or_lane_order_changed")
            account, index = selected
            self.operations[payload["operation"]] = {"account": account, "account_index": index,
                        "lane": payload["lane"], "status": "pick_pending", "assignment": None,
                        "request": self.accounts[account]["ready"]}
            self.pending = payload["operation"]
            self.next_operation += 1
            self.next_lane = (self.next_lane + 1) % len(self.lanes)
            return
        s.require(kind in ("pick_empty", "pick_abandoned", "assigned", "offered", "submission_started",
                           "accepted", "rejected", "expired"), "unknown_audit_event")
        operation = payload["operation"]
        s.require(operation in self.operations, "audit_unknown_operation")
        op = self.operations[operation]
        if kind in ("pick_empty", "pick_abandoned"):
            s.exact(payload, ("operation",))
            s.require(self.pending == operation and op["status"] == "pick_pending", "audit_pick_not_pending")
            self.pending = None
            if kind == "pick_empty":
                del self.operations[operation]
            else:
                op["status"] = "unexported_pick_abandoned"
        elif kind == "assigned":
            s.exact(payload, ("operation", "assignment", "transaction_count"))
            s.require(self.pending == operation and op["status"] == "pick_pending", "audit_pick_not_pending")
            item = payload["assignment"]
            s.exact(item, ASSIGNMENT_FIELDS)
            for field in ("assignment_id", "subscription_hash", "statement_hash", "lease_commitment"):
                s.nonzero(item[field])
            s.require(0 < s.uint(item["batch_number"], 64) and 0 < s.uint(payload["transaction_count"], 64),
                      "audit_nonempty_assignment_required")
            prior = [(key, old) for key, old in self.operations.items() if old["assignment"]
                     and old["lane"] == op["lane"] and old["assignment"]["batch_number"] == item["batch_number"]]
            previous = prior[-1] if prior else None
            if previous:
                old = previous[1]
                s.require(old["status"] in ("rejected", "expired")
                          and old["assignment"]["statement_hash"] == item["statement_hash"]
                          and old["transaction_count"] == payload["transaction_count"]
                          and old["assignment"]["lease_commitment"] != item["lease_commitment"],
                          "audit_retry_not_retired_or_changed")
            account = op["account"]
            expected = {"account": account,
                        "subscription_hash": s.struct_hash("ProverSubscriptionV1", s.SUBSCRIPTION, self.subscriptions[account]),
                        "batch_number": item["batch_number"], "statement_hash": item["statement_hash"],
                        "lease_commitment": item["lease_commitment"],
                        "attempt": previous[1]["assignment"]["attempt"] + 1 if previous else 1,
                        "slot": self.accounts[account]["offered"], "period": self.identity["period"]}
            expected["assignment_id"] = s.keccak(s.canonical(
                {"journal_id": self.journal_id, "operation": operation, **expected}))
            s.require(item == expected and item["slot"] < self.settings["duties_per_round"], "audit_assignment_changed")
            op.update(status="picked", assignment=item, previous=previous[0] if previous else None,
                      previous_cursor=self.cursor, transaction_count=payload["transaction_count"])
            self.accounts[account]["ready"] = None
            self.next_account = (op["account_index"] + 1) % len(self.accounts)
            self.cursor = s.keccak(s.canonical({"previous": self.cursor, "assignment": item}))
            self.pending = None
        elif kind == "offered":
            s.exact(payload, ("operation", "sequencer_signature"))
            self.live(at)
            s.require(op["status"] == "picked" and at < op["request"]["request"]["typed_data"]["message"]["expiresAt"],
                      "audit_offer_not_live")
            # Prior assignments and retry reasons are already authenticated in the prefix.
            # Reconstructing them avoids quadratic copies of long retry histories in the log.
            manifest = self.manifest([operation], op["lane"])
            s.verify_eoa(s.manifest_request(self.lanes[op["lane"]]["settings"], manifest),
                         payload["sequencer_signature"])
            self.accounts[op["account"]]["offered"] += 1
            op["status"] = "offered"
        elif kind == "submission_started":
            s.exact(payload, ("operation", "duty"))
            self.live(at)
            s.require(op["status"] == "offered" and at < op["request"]["request"]["typed_data"]["message"]["expiresAt"],
                      "audit_submission_not_live")
            self.duty(op, payload["duty"])
            op.update(status="submission_pending", submitted_duty=payload["duty"])
        elif kind == "accepted":
            s.exact(payload, ("operation", "duty"))
            s.require(op["status"] == "submission_pending" and payload["duty"] == op["submitted_duty"],
                      "audit_acceptance_without_exact_submission")
            op["status"] = "accepted"
            op["duty"] = payload["duty"]
            self.accepted[s.keccak(s.canonical(payload["duty"]))] = payload["duty"]
        elif kind == "rejected":
            s.exact(payload, ("operation",))
            s.require(op["status"] == "submission_pending", "audit_rejection_without_submission")
            op["status"] = "rejected"
        else:
            s.exact(payload, ("operation",))
            s.require(op["status"] in ("picked", "offered")
                      and at >= op["request"]["request"]["typed_data"]["message"]["expiresAt"],
                      "audit_live_or_ambiguous_assignment_expired")
            op["status"] = "expired"


def verify_package(settings, evidence, manifest, subscriptions, duties, bundle, trust, rpc, *, fri_payload, allow_control=False):
    s.config(settings)
    s.exact(bundle, ("schema_version", "identity", "journal_id", "events", "checkpoint", "sequencer_signature"))
    s.exact(trust, ("schema_version", "journal_id", "enrollment_block_hash", "minimum_checkpoint",
                    "required_readiness", "required_duties"))
    s.require(bundle["schema_version"] == trust["schema_version"] == 1, "unsupported_audit_schema")
    s.require(type(allow_control) is bool, "invalid_control_exemption")
    journal_id = s.nonzero(bundle["journal_id"])
    s.require(journal_id == s.nonzero(trust["journal_id"]), "audit_untrusted_journal")
    identity, checkpoint, events = bundle["identity"], bundle["checkpoint"], bundle["events"]
    s.require(type(events) is list and len(events) <= MAX_EVENTS and len(s.canonical(bundle)) <= s.MAX_FILE,
              "audit_capacity_exceeded")
    s.verify_eoa(checkpoint_request(bundle), bundle["sequencer_signature"])
    enrollment = validate_identity(identity, journal_id, trust, rpc)
    s.require(subscriptions == identity["subscriptions"], "audit_package_snapshot_changed")
    lanes = [name for name, lane in identity["lanes"].items() if lane["settings"] == settings]
    s.require(len(lanes) == 1, "audit_package_lane_changed")
    lane = lanes[0]
    statements = s.validate_evidence(settings, evidence)
    numbers = list(statements)
    s.exact(fri_payload, ("from_batch_number", "to_batch_number", "vk_hash", "fri_proofs"))
    s.require(fri_payload["from_batch_number"] == numbers[0] and fri_payload["to_batch_number"] == numbers[-1]
              and fri_payload["vk_hash"] == settings["vk_hash"] and type(fri_payload["fri_proofs"]) is list
              and len(fri_payload["fri_proofs"]) == len(numbers), "audit_native_fri_payload_mismatch")
    proof_hashes = {number: s.keccak(s.decode_proof(proof)) for number, proof in zip(numbers, fri_payload["fri_proofs"])}
    s.require(checkpoint["journalId"] == journal_id and checkpoint["eventCount"] == len(events)
              and checkpoint["chainId"] == settings["execution_chain_id"]
              and checkpoint["chainAddress"] == settings["chain_address"]
              and checkpoint["period"] == identity["period"]
              and checkpoint["batchFrom"] == numbers[0] and checkpoint["batchTo"] == numbers[-1],
              "audit_checkpoint_scope_mismatch")
    minimum = trust["minimum_checkpoint"]
    s.exact(minimum, ("event_count", "event_head", "assignment_cursor"))
    count = s.uint(minimum["event_count"], 64)
    s.require(count <= len(events), "audit_checkpoint_truncated")
    replay, head, at = Replay(identity, journal_id), journal_id, identity["enrollment"]["timestamp"]
    prefix = {"event_count": 0, "event_head": head, "assignment_cursor": journal_id}
    for index, event in enumerate(events, 1):
        s.exact(event, ("sequence", "at", "kind", "payload", "previous", "hash"))
        s.require(s.uint(event["sequence"], 64) == index and event["previous"] == head
                  and s.uint(event["at"], 64) >= at, "audit_event_order_changed")
        s.require(s.keccak(s.canonical({key: value for key, value in event.items() if key != "hash"})) == event["hash"],
                  "audit_event_hash_changed")
        replay.apply(event)
        head, at = event["hash"], event["at"]
        if index == count:
            prefix = {"event_count": index, "event_head": head, "assignment_cursor": replay.cursor}
    s.require(prefix == minimum, "audit_checkpoint_prefix_changed")
    s.require(checkpoint["eventHead"] == head and checkpoint["assignmentCursor"] == replay.cursor
              and checkpoint["issuedAt"] >= at, "audit_checkpoint_head_changed")
    required_readiness, required_duties = trust["required_readiness"], trust["required_duties"]
    s.require(type(required_readiness) is list and len(required_readiness) <= MAX_EVENTS
              and type(required_duties) is list and len(required_duties) <= 2000, "invalid_audit_external_receipts")
    for receipt in required_readiness:
        s.exact(receipt, ("request", "signature", "observed_before_event_count"))
        bound = s.uint(receipt["observed_before_event_count"], 64)
        sequence = replay.readiness.get(s.keccak(s.canonical({key: receipt[key] for key in ("request", "signature")})))
        s.require(sequence is not None and sequence <= bound, "audit_known_readiness_omitted_or_delayed")
    for duty in required_duties:
        s.encode_duty(duty)
        s.require(s.keccak(s.canonical(duty)) in replay.accepted, "audit_known_accepted_duty_omitted")
    relevant = [key for key, op in replay.operations.items() if op["lane"] == lane and op["assignment"] is not None
                and numbers[0] <= op["assignment"]["batch_number"] <= numbers[-1]]
    for key in relevant:
        op = replay.operations[key]
        item, statement = op["assignment"], statements[op["assignment"]["batch_number"]]
        s.require(item["statement_hash"] == statement["statement"] and op["transaction_count"] == statement["count"],
                  "audit_assignment_native_evidence_changed")
        s.require(op["status"] not in LIVE, "audit_range_has_unresolved_assignment")
        # Reusing the very proof that an operator submitted while claiming it was rejected
        # cannot authorize an uncredited package or a replacement recipient.
        s.require(op["status"] != "rejected" or op["submitted_duty"]["friProofHash"] != proof_hashes[item["batch_number"]],
                  "audit_rejected_operator_proof_reused")
    expected_manifest = replay.manifest(relevant, lane)
    s.require(manifest["payload"] == expected_manifest, "audit_package_assignments_or_retries_incomplete")
    s.validate_manifest(settings, manifest, subscriptions, statements, allow_empty=True, enrollment=enrollment)
    expected_duties = [replay.operations[key]["duty"] for key in relevant if replay.operations[key]["status"] == "accepted"]
    s.require(all(duty["friProofHash"] == proof_hashes[duty["batchNumber"]] for duty in expected_duties),
              "audit_accepted_duty_native_proof_changed")
    s.require(type(duties) is list and len(duties) == len(expected_duties)
              and sorted(map(s.canonical, duties)) == sorted(map(s.canonical, expected_duties)),
              "audit_package_accepted_duties_incomplete")
    credited = {duty["batchNumber"] for duty in expected_duties}
    unrewarded = [number for number, statement in statements.items() if statement["count"] > 0 and number not in credited]
    exhausted = all(account["offered"] == settings["duties_per_round"] for account in replay.accounts.values())
    s.require(not unrewarded or allow_control or exhausted, "audit_nonempty_batch_without_service_duty_wait")
    zero_duty_reason = None
    if not expected_duties:
        if allow_control:
            zero_duty_reason = "control"
        elif all(statement["count"] == 0 for statement in statements.values()):
            zero_duty_reason = "empty_native_range"
        elif exhausted:
            zero_duty_reason = "quota_exhausted"
        s.require(zero_duty_reason is not None, "audit_no_accepted_duties_wait")
    return {"journal_id": journal_id, "event_count": len(events), "event_head": head,
            "assignment_cursor": replay.cursor, "lane": lane,
            "zero_duty_reason": zero_duty_reason,
            "unrewarded_batches": unrewarded,
            "unrewarded_reason": ("control" if allow_control else "quota_exhausted") if unrewarded else None,
            "required_duties": copy.deepcopy(expected_duties),
            "accounts": {account: {"offered_opportunities": data["offered"], "quota": settings["duties_per_round"]}
                         for account, data in replay.accounts.items()}}
