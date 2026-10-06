#!/usr/bin/env python3
"""Prepare unsigned service maintenance calls and revalidate SNARK compute permits."""

import argparse
import copy
import hashlib
import os
import sys
import time

import relay as r
import roster as rosters
import service as s


def sha(value):
    return hashlib.sha256(s.canonical(value)).hexdigest()


def configuration(value):
    s.require(type(value) is dict, "unexpected_or_missing_fields")
    s.exact(value, ("schema_version", "lane", "rpc_url", "settings", "policy")
            + (("enrollment",) if "enrollment" in value else ()))
    s.require(value["schema_version"] == 1 and value["lane"] in ("child", "gateway"), "invalid_keeper_configuration")
    s.config(value["settings"])
    p = value["policy"]
    s.exact(p, ("expected_operator", "gate_code_hash", "coordinator_code_hash", "priority_guard_code_hash",
                "reserve_seconds", "max_head_age_seconds", "rpc_timeout_seconds"))
    s.nonzero(p["expected_operator"], 20)
    for field in ("gate_code_hash", "priority_guard_code_hash"):
        s.nonzero(p[field])
    s.raw_hex(p["coordinator_code_hash"], 32)
    s.require((value["settings"]["coordinator"] == s.ZERO_ADDRESS) == (p["coordinator_code_hash"] == s.ZERO),
              "coordinator_address_and_code_hash_must_match_phase")
    for field in ("reserve_seconds", "max_head_age_seconds", "rpc_timeout_seconds"):
        s.require(0 < s.uint(p[field], 32), "positive_keeper_limit_required")
    s.require(p["rpc_timeout_seconds"] <= 30, "rpc_timeout_too_large")
    r.Rpc(value["rpc_url"], timeout=p["rpc_timeout_seconds"])
    if "enrollment" in value:
        enrollment = value["enrollment"]
        s.exact(enrollment, ("registry_rpc_file", "block_hash"))
        s.require(os.path.isabs(enrollment["registry_rpc_file"]), "absolute_registry_rpc_file_required")
        s.nonzero(enrollment["block_hash"])
    return value


def rpc_for(config):
    return r.Rpc(config["rpc_url"], timeout=config["policy"]["rpc_timeout_seconds"])


def registry_rpc_for(config):
    if "enrollment" not in config:
        return None
    import workflow_io
    return workflow_io.wallet_connection(config["enrollment"]["registry_rpc_file"],
                                         config["policy"]["rpc_timeout_seconds"])


def enrollment_for(config, request, registry_rpc=None):
    if "enrollment" not in config:
        return None
    configuration(config)
    return s.EnrollmentAuthority(config["settings"], request["subscriptions"], request["manifest"]["payload"]["period"],
                                 registry_rpc or registry_rpc_for(config), config["enrollment"]["block_hash"])


def normalized(accepted):
    return {**accepted, "turn": 0, "proofHash": s.ZERO, "wrapper": s.ZERO_ADDRESS,
            "wrapperBeneficiary": s.ZERO_ADDRESS}


def prepare(settings, request, evidence, payload, *, enrollment=None):
    """Validate the same native FRI artifacts and endorsements used by package signing."""
    s.exact(request, ("proposal", "manifest", "subscriptions", "duties"))
    proposal, duties = request["proposal"], request["duties"]
    statements = s.validate_evidence(settings, evidence)
    numbers = list(statements)
    s.require(len(numbers) >= 2 and type(duties) is list and len(duties) <= len(numbers), "invalid_compute_range")
    assignments, subscriptions = s.validate_manifest(settings, request["manifest"], request["subscriptions"],
                                                       statements, allow_empty=not duties, enrollment=enrollment)
    s.exact(payload, ("from_batch_number", "to_batch_number", "vk_hash", "fri_proofs"))
    s.require(payload["from_batch_number"] == numbers[0] and payload["to_batch_number"] == numbers[-1]
              and payload["vk_hash"] == settings["vk_hash"] and type(payload["fri_proofs"]) is list
              and len(payload["fri_proofs"]) == len(numbers), "compute_payload_mismatch")
    hashes = {number: s.keccak(s.decode_proof(proof)) for number, proof in zip(numbers, payload["fri_proofs"])}
    seen, slots = set(), set()
    for duty in duties:
        s.encode_duty(duty)
        number = duty["batchNumber"]
        assignment = assignments.get(number)
        s.require(assignment and number not in seen, "duplicate_or_unassigned_duty")
        for source, dest in (("account", "account"), ("subscriptionHash", "subscription_hash"), ("period", "period"),
                             ("slot", "slot"), ("attempt", "attempt"), ("assignmentId", "assignment_id"),
                             ("statementHash", "statement_hash")):
            s.require(duty[source] == assignment[dest], "duty_assignment_mismatch")
        s.require(0 < duty["transactionCount"] == statements[number]["count"]
                  and duty["friProofHash"] == hashes[number], "duty_native_artifact_mismatch")
        slot = (duty["account"], duty["period"], duty["slot"])
        s.require(slot not in slots, "duplicate_duty_slot")
        s.verify_eoa(s.duty_request(settings, {k: duty[k] for k, _ in s.DUTY}, subscriptions[duty["subscriptionHash"]]),
                     duty["operatorSignature"])
        seen.add(number)
        slots.add(slot)
    s.exact(proposal, ("mode", "accepted_package", "candidate", "candidate_proof"))
    s.require(proposal["mode"] in ("service", "bootstrap"), "invalid_compute_mode")
    accepted = dict(proposal["accepted_package"])
    s.exact(accepted, [key for key, _ in s.PACKAGE if key not in ("manifestHash", "reportHash", "proofHash")])
    accepted.update(manifestHash=s.keccak(s.canonical(request["manifest"])), reportHash=s.report_hash(duties), proofHash=s.ZERO)
    s.encode_fields(s.PACKAGE, accepted)
    for key, expected in (("domainVersion", 1), ("protocolVersion", 32), ("chainId", settings["execution_chain_id"]),
                           ("chainAddress", settings["chain_address"]), ("policyHash", settings["policy_hash"]),
                           ("vkHash", settings["vk_hash"]), ("sequencer", settings["sequencer"]),
                           ("period", request["manifest"]["payload"]["period"]), ("batchFrom", numbers[0]), ("batchTo", numbers[-1])):
        s.require(accepted[key] == expected, "compute_package_identity_mismatch")
    s.nonzero(accepted["parent"])
    s.nonzero(accepted["sequencerBeneficiary"], 20)
    s.require(accepted["batchTo"] < 2**64 - 1, "terminal_batch_number")
    candidate = proposal["candidate"]
    if proposal["mode"] == "service":
        s.check_candidate(candidate, proposal["candidate_proof"], accepted["rosterRoot"])
        s.require(candidate["operator"] == accepted["wrapper"] != settings["sequencer"]
                  and candidate["beneficiary"] == accepted["wrapperBeneficiary"], "wrong_compute_operator")
    else:
        s.require(candidate is None and proposal["candidate_proof"] == [] and accepted["rosterRoot"] == s.ZERO
                  and accepted["turn"] == 0 and accepted["wrapper"] == s.ZERO_ADDRESS
                  and accepted["wrapperBeneficiary"] == s.ZERO_ADDRESS, "invalid_bootstrap_compute")
    return {"mode": proposal["mode"], "accepted_package": accepted, "duties": duties,
            "batch_outputs": [batch["output"] for batch in evidence["batches"]],
            "candidate": candidate, "candidate_proof": proposal["candidate_proof"]}


def raw_call(rpc, address, signature, anchor, encoded=b"", size=None):
    return s.raw_hex(rpc.call("eth_call", [{"to": address, "data": "0x" + (r.selector(signature) + encoded).hex()}, anchor]), size)


def check_code(rpc, address, expected, anchor, reason):
    code = s.raw_hex(rpc.call("eth_getCode", [address, anchor]))
    s.require(code and s.keccak(code) == expected, reason)


def base_context(config, rpc, evidence, now):
    settings, limits = config["settings"], config["policy"]
    gate, chain = settings["proof_gate"], settings["chain_address"]
    head, anchor = r.anchored_head(rpc, settings, limits, now)
    check_code(rpc, gate, limits["gate_code_hash"], anchor, "gate_code_changed")
    call = lambda address, signature, encoded=b"", kind="uint": r.call_word(rpc, address, signature, anchor, encoded, kind)
    for signature, kind, value in (("chain()", "address", chain), ("childChainId()", "uint", s.uint(settings["execution_chain_id"])),
                                    ("sequencer()", "address", settings["sequencer"]), ("policyHash()", "bytes32", settings["policy_hash"]),
                                    ("productionVkHash()", "bytes32", settings["vk_hash"])):
        s.require(call(gate, signature, kind=kind) == value, "gate_configuration_changed")
    s.require(call(chain, "getChainId()") == s.uint(settings["execution_chain_id"])
              and raw_call(rpc, chain, "getSemverProtocolVersion()", anchor, size=96) == s.word(0) + s.word(32) + s.word(0),
              "native_protocol_changed")
    verifier = call(gate, "productionVerifier()", kind="address")
    plonk = call(gate, "plonkVerifier()", kind="address")
    s.require(call(chain, "getVerifier()", kind="address") == verifier
              and call(verifier, "plonkVerifiers(uint32)", s.word(8), "address") == plonk
              and not call(verifier, "IS_TESTNET_VERIFIER()", kind="bool")
              and call(verifier, "verificationKeyHash(uint256)", s.word(0x802), "bytes32") == settings["vk_hash"],
              "native_verifier_changed")
    check_code(rpc, verifier, call(gate, "verifierCodeHash()", kind="bytes32"), anchor, "verifier_code_changed")
    check_code(rpc, plonk, call(gate, "plonkVerifierCodeHash()", kind="bytes32"), anchor, "plonk_code_changed")
    timelock = call(gate, "timelock()", kind="address")
    role = s.raw_hex(s.keccak(b"PROVER_ROLE"))
    chain_word = bytes(12) + s.raw_hex(chain, 20)
    s.require(call(chain, "isValidator(address)", bytes(12) + s.raw_hex(timelock, 20), "bool")
              and call(timelock, "getRoleMemberCount(address,bytes32)", chain_word + role) == 1
              and call(timelock, "getRoleMember(address,bytes32,uint256)", chain_word + role + s.word(0), "address") == gate,
              "native_prover_authority_changed")
    s.require(call(chain, "getTotalBatchesVerified()") == evidence["previous_batch"]["batchNumber"], "native_proved_frontier_changed")
    for stored in [evidence["previous_batch"]] + [item["stored"] for item in evidence["batches"]]:
        s.require(call(chain, "storedBatchHash(uint256)", s.word(stored["batchNumber"]), "bytes32")
                  == s.keccak(s.encode_fields(s.STORED, stored)), "native_stored_batch_changed")
    return head, anchor, call


def inspect(config, rpc, prepared, evidence, now, runtime, require_open=True):
    s.require(0 < s.uint(runtime, 32) <= 86400, "invalid_compute_runtime")
    head, anchor, call = base_context(config, rpc, evidence, now)
    settings, limits = config["settings"], config["policy"]
    accepted, mode = prepared["accepted_package"], prepared["mode"]
    gate, coordinator = settings["proof_gate"], settings["coordinator"]
    frozen_hash = s.struct_hash("AcceptedPackageV1", s.PACKAGE, normalized(accepted))
    s.require(call(gate, "lastAcceptedPackage()", kind="bytes32") == accepted["parent"], "accepted_parent_changed")
    active = call(gate, "serviceActive()", kind="bool")
    installed = call(gate, "coordinator()", kind="address")
    deadline, control = None, False
    if mode == "bootstrap":
        s.require(not active and installed == s.ZERO_ADDRESS, "bootstrap_phase_closed")
        s.require(limits["expected_operator"] == settings["sequencer"], "bootstrap_requires_local_sequencer")
        if require_open:
            s.require(raw_call(rpc, gate, "bootstrapPackage()", anchor, size=32 * len(s.PACKAGE))
                      == s.encode_fields(s.PACKAGE, normalized(accepted)), "bootstrap_package_repaired")
    else:
        s.require(active and installed == coordinator, "service_phase_not_active")
        check_code(rpc, coordinator, limits["coordinator_code_hash"], anchor, "coordinator_code_changed")
        for signature, kind, value in (("acceptanceGate()", "address", gate), ("acceptedParent()", "bytes32", accepted["parent"]),
                                        ("childChainId()", "uint", s.uint(settings["execution_chain_id"])),
                                        ("childChainAddress()", "address", settings["chain_address"]),
                                        ("sequencer()", "address", settings["sequencer"]),
                                        ("nextBatch()", "uint", accepted["batchFrom"]), ("policyHash()", "bytes32", settings["policy_hash"]),
                                        ("productionVkHash()", "bytes32", settings["vk_hash"])):
            s.require(call(coordinator, signature, kind=kind) == value, "coordinator_configuration_changed")
        if require_open:
            s.require(call(coordinator, "packageOpen()", kind="bool"), "package_not_open")
            s.require(call(coordinator, "frozenPackageHash()", kind="bytes32") == frozen_hash, "frozen_package_repaired")
            s.require(call(coordinator, "rosterRoot()", kind="bytes32") == accepted["rosterRoot"], "roster_changed")
            s.require(call(coordinator, "currentTurn()") == accepted["turn"]
                      and call(coordinator, "selectedWrapperIndex()") == prepared["candidate"]["index"], "stale_wrapper_turn")
            s.require(limits["expected_operator"] == prepared["candidate"]["operator"], "wrong_local_wrapper")
            deadline = call(coordinator, "turnDeadline()")
            control = call(gate, "transitionWork()", kind="bool")
        else:
            opening = raw_call(rpc, coordinator, "openingRoster()", anchor, size=128)
            period, root, count, transition = [opening[i:i + 32] for i in range(0, 128, 32)]
            s.require(int.from_bytes(period, "big") == accepted["period"] and root == s.raw_hex(accepted["rosterRoot"])
                      and 0 < int.from_bytes(count, "big") < 2**32 and int.from_bytes(transition, "big") in (0, 1),
                      "opening_roster_mismatch")
            control = bool(int.from_bytes(transition, "big"))
    s.require(not control or not prepared["duties"], "control_work_must_have_empty_report")
    priority = None
    if require_open:
        priority = r.priority_context(rpc, settings, {**limits, "min_turn_seconds": runtime + limits["reserve_seconds"]},
                                      {"sidecar": prepared}, head, anchor)
        s.require(priority["reason"] == "ready", priority["reason"])
        deadline = min(deadline, priority["deadline"]) if deadline is not None else priority["deadline"]
        # Host wall time also consumes the work window when an RPC head is delayed.
        s.require(now + runtime + limits["reserve_seconds"] < deadline, "insufficient_compute_window")
    r.check_anchor(rpc, settings, head)
    return {"head": {key: head[key] for key in ("number", "hash", "timestamp")}, "frozen_package_hash": frozen_hash,
            "deadline": deadline, "control_work": control, "priority": priority}


def permit(config, rpc, request, evidence, payload, now, runtime, *, enrollment=None):
    configuration(config)
    if enrollment is not None:
        s.require("enrollment" in config and type(enrollment) is s.EnrollmentAuthority,
                  "configured_enrollment_authority_required")
        s.require(enrollment.block_hash == config["enrollment"]["block_hash"], "enrollment_authority_anchor_mismatch")
    prepared = prepare(config["settings"], request, evidence, payload,
                       enrollment=enrollment if enrollment is not None else enrollment_for(config, request))
    state = inspect(config, rpc, prepared, evidence, now, runtime)
    return {"schema_version": 1, "configuration_sha256": sha(config), "lane": config["lane"],
            "payload_sha256": sha(payload), "evidence_sha256": sha(evidence), "request": request,
            "runtime_seconds": runtime, "state": state}


def validate_permit(config, rpc, value, evidence, payload, now, runtime, *, enrollment=None):
    s.exact(value, ("schema_version", "configuration_sha256", "lane", "payload_sha256", "evidence_sha256", "request",
                    "runtime_seconds", "state"))
    s.require(value["schema_version"] == 1 and value["configuration_sha256"] == sha(config)
              and value["lane"] == config["lane"] and value["payload_sha256"] == sha(payload)
              and value["evidence_sha256"] == sha(evidence) and value["runtime_seconds"] == runtime,
              "compute_permit_binding_changed")
    current = permit(config, rpc, value["request"], evidence, payload, now, runtime, enrollment=enrollment)
    for field in ("frozen_package_hash", "deadline", "control_work"):
        s.require(value["state"][field] == current["state"][field], "compute_permit_state_changed")
    s.require(value["state"]["priority"] == current["state"]["priority"], "compute_permit_checkpoint_changed")
    return current


def unsigned_call(rpc, settings, limits, head, anchor, account, target, signature, encoded=b"", value="0x0"):
    s.nonzero(account, 20)
    s.uint(value)
    tx = {"from": account, "to": target, "data": "0x" + (r.selector(signature) + encoded).hex(), "value": value}
    rpc.call("eth_call", [tx, anchor])
    r.check_anchor(rpc, settings, head)
    return {"schema_version": 1, "chain_id": settings["settlement_chain_id"], "anchor": head,
            "transaction": tx, "action": signature.split("(")[0], "broadcast": False}


def package_call(config, rpc, request, evidence, payload, now, action):
    s.require(action in ("open", "repair"), "invalid_package_action")
    prepared = prepare(config["settings"], request, evidence, payload, enrollment=enrollment_for(config, request))
    # Repairs preserve the old draw even if the clock has rolled to another roster.
    if action == "open":
        inspect(config, rpc, prepared, evidence, now, 1, require_open=False)
    head, anchor, _ = base_context(config, rpc, evidence, now)
    method = action + ("BootstrapPackage" if prepared["mode"] == "bootstrap" else "Package")
    return unsigned_call(rpc, config["settings"], config["policy"], head, anchor, config["settings"]["sequencer"],
                         config["settings"]["proof_gate"], method + "(" + r.abi_tuple(s.PACKAGE) + ")",
                         s.encode_fields(s.PACKAGE, normalized(prepared["accepted_package"])))


def maintenance(config, rpc, action, args, now):
    """Only fixed contract methods are exposed; every returned call is simulated at its anchor."""
    settings, limits = config["settings"], config["policy"]
    head, anchor = r.anchored_head(rpc, settings, limits, now)
    account, target, value = limits["expected_operator"], settings["coordinator"], "0x0"
    if action in ("prepare-roster", "record-roster"):
        s.exact(args, ("period",))
        check_code(rpc, target, limits["coordinator_code_hash"], anchor, "coordinator_code_changed")
        signature = ("prepareRosterDraw" if action == "prepare-roster" else "recordRosterDraw") + "(uint64)"
        encoded = s.word(s.uint(args["period"], 64))
    elif action == "refresh-priority":
        s.exact(args, ())
        target, account = settings["proof_gate"], settings["sequencer"]
        check_code(rpc, target, limits["gate_code_hash"], anchor, "gate_code_changed")
        signature, encoded = "refreshPriorityCheckpoint()", b""
    elif action in ("request-draw", "capture-draw", "relay-draw", "relay-checkpoint"):
        fields = ("target", "code_hash", "value") + (() if action == "relay-checkpoint" else ("commitment",))
        if action == "request-draw":
            fields += ("batch_number", "index", "tx_number_in_batch", "proof")
        if action in ("relay-draw", "relay-checkpoint"):
            fields += ("gas_limit", "gas_per_pubdata_byte_limit", "refund_recipient")
        s.exact(args, fields)
        target, value = s.nonzero(args["target"], 20), args["value"]
        check_code(rpc, target, s.nonzero(args["code_hash"]), anchor, "maintenance_target_code_changed")
        if action != "relay-checkpoint":
            commitment = s.raw_hex(s.nonzero(args["commitment"]))
        if action == "request-draw":
            s.require(type(args["proof"]) is list and len(args["proof"]) <= 64 and s.uint(value) == 0, "invalid_draw_proof")
            signature = "requestDraw(bytes32,uint256,uint256,uint16,bytes32[])"
            encoded = s.encode_dynamic_tuple([(commitment, False), (s.word(args["batch_number"]), False),
                (s.word(args["index"]), False), (s.word(s.uint(args["tx_number_in_batch"], 16)), False),
                (s.word(len(args["proof"])) + b"".join(s.raw_hex(p, 32) for p in args["proof"]), True)])
        elif action == "capture-draw":
            s.require(s.uint(value) == 0, "nonpayable_draw")
            signature, encoded = "captureDraw(bytes32)", commitment
        else:
            s.nonzero(args["refund_recipient"], 20)
            s.require(s.uint(args["gas_limit"]) > 0 and s.uint(args["gas_per_pubdata_byte_limit"]) > 0, "invalid_relay_gas")
            suffix = s.word(args["gas_limit"]) + s.word(args["gas_per_pubdata_byte_limit"]) + bytes(12) + s.raw_hex(args["refund_recipient"], 20)
            signature = "relayDraw(bytes32,uint256,uint256,address)" if action == "relay-draw" else "relayCheckpoint(uint256,uint256,address)"
            encoded = commitment + suffix if action == "relay-draw" else suffix
    else:
        raise s.Error("unknown_maintenance_action")
    return unsigned_call(rpc, settings, limits, head, anchor, account, target, signature, encoded, value)


def prefix_call(config, rpc, evidence, witness, now):
    s.validate_evidence(config["settings"], evidence)
    s.exact(witness, ("batch_item_preimages", "left_path", "right_path"))
    s.require(type(witness["batch_item_preimages"]) is list and len(witness["batch_item_preimages"]) == len(evidence["batches"]),
              "invalid_prefix_batch_count")
    counts, hashes = [], []
    for entries, batch in zip(witness["batch_item_preimages"], evidence["batches"]):
        s.require(type(entries) is list and len(entries) == s.uint(batch["output"]["l1TxCount"]), "invalid_prefix_count")
        rolling = s.keccak(b"")
        for preimage in entries:
            raw = s.raw_hex(preimage)
            s.require(raw and len(raw) <= s.MAX_FILE and len(hashes) < 4096, "invalid_priority_preimage")
            digest = s.keccak(raw)
            hashes.append(digest)
            rolling = s.keccak(s.raw_hex(rolling) + s.raw_hex(digest))
        s.require(rolling == batch["output"]["priorityOperationsHash"], "priority_preimage_hash_mismatch")
        counts.append(len(entries))
    for key in ("left_path", "right_path"):
        s.require(type(witness[key]) is list and len(witness[key]) <= 64, "invalid_priority_path")
    settings, limits = config["settings"], config["policy"]
    head, anchor = r.anchored_head(rpc, settings, limits, now)
    check_code(rpc, settings["proof_gate"], limits["gate_code_hash"], anchor, "gate_code_changed")
    guard = r.call_word(rpc, settings["proof_gate"], "priorityGuard()", anchor, kind="address")
    check_code(rpc, guard, limits["priority_guard_code_hash"], anchor, "priority_guard_code_changed")
    work_id = r.call_word(rpc, settings["proof_gate"], "priorityWorkId()", anchor, kind="bytes32")
    s.nonzero(work_id)
    arrays = [(s.word(len(counts)) + b"".join(s.word(count) for count in counts), True)]
    arrays += [(s.word(len(values)) + b"".join(s.raw_hex(value, 32) for value in values), True)
               for values in (hashes, witness["left_path"], witness["right_path"])]
    return unsigned_call(rpc, settings, limits, head, anchor, limits["expected_operator"], guard,
                         "publishPrefixWitness(bytes32,uint256[],bytes32[],bytes32[],bytes32[])",
                         s.encode_dynamic_tuple([(s.raw_hex(work_id), False)] + arrays))


def decode_package(raw):
    s.require(len(raw) == 32 * len(s.PACKAGE), "invalid_frozen_package_size")
    frozen = {}
    for index, (key, kind) in enumerate(s.PACKAGE):
        word = raw[index * 32:(index + 1) * 32]
        if kind == "address":
            s.require(word[:12] == bytes(12), "invalid_frozen_address")
            frozen[key] = "0x" + word[12:].hex()
        elif kind == "bytes32":
            frozen[key] = "0x" + word.hex()
        else:
            number = s.uint(int.from_bytes(word, "big"), int(kind[4:]))
            frozen[key] = hex(number) if kind == "uint256" else number
    return frozen


def check_frozen(settings, state):
    frozen = state["frozen_package"]
    s.encode_fields(s.PACKAGE, frozen)
    s.require(frozen == normalized(frozen), "invalid_normalized_frozen_package")
    for field, value in (("domainVersion", 1), ("protocolVersion", 32), ("chainId", settings["execution_chain_id"]),
        ("chainAddress", settings["chain_address"]), ("policyHash", settings["policy_hash"]),
        ("vkHash", settings["vk_hash"]), ("sequencer", settings["sequencer"]), ("parent", state["parent"]),
        ("batchFrom", state["next_native_batch"])):
        s.require(frozen[field] == value, "frozen_package_configuration_changed")
    s.nonzero(frozen["parent"])
    s.nonzero(frozen["sequencerBeneficiary"], 20)
    s.nonzero(frozen["manifestHash"])
    s.require(0 < frozen["batchTo"] - frozen["batchFrom"] < 100 and frozen["batchTo"] < 2**64 - 1,
              "invalid_frozen_batch_range")
    digest = s.struct_hash("AcceptedPackageV1", s.PACKAGE, frozen)
    s.require(digest == state["frozen_package_hash"], "frozen_package_hash_mismatch")
    if state["mode"] == "bootstrap":
        s.require(frozen["rosterRoot"] == s.ZERO, "invalid_bootstrap_roster")
    else:
        s.require(state["roster"] is not None and frozen["period"] == state["roster"]["period"]
                  and frozen["rosterRoot"] == state["roster"]["root"], "frozen_roster_mismatch")


def rebind_request(settings, request, state, roster_artifact=None):
    """Bind only endorsement fields to an authenticated turn; repairs require new source artifacts."""
    s.config(settings)
    s.exact(request, ("proposal", "manifest", "subscriptions", "duties"))
    proposal = request["proposal"]
    s.exact(proposal, ("mode", "accepted_package", "candidate", "candidate_proof"))
    s.require(state["mode"] in ("service", "bootstrap") and proposal["mode"] == state["mode"], "request_phase_mismatch")
    s.require(state["package_open"] and state["frozen_package"] is not None, "package_not_open")
    check_frozen(settings, state)
    accepted = dict(proposal["accepted_package"])
    s.exact(accepted, [key for key, _ in s.PACKAGE if key not in ("manifestHash", "reportHash", "proofHash")])
    accepted.update(manifestHash=s.keccak(s.canonical(request["manifest"])), reportHash=s.report_hash(request["duties"]),
                    proofHash=s.ZERO)
    s.encode_fields(s.PACKAGE, accepted)
    s.require(normalized(accepted) == state["frozen_package"], "request_frozen_commitments_changed")
    s.require(not state["control_work"] or not request["duties"], "control_work_must_have_empty_report")
    rebound = copy.deepcopy(request)
    if state["mode"] == "bootstrap":
        s.require(proposal["candidate"] is None and proposal["candidate_proof"] == [] and accepted == normalized(accepted),
                  "invalid_bootstrap_compute")
        return rebound
    s.require(roster_artifact is not None, "complete_roster_required")
    selected = rosters.selected(roster_artifact, state["roster"], state["selected_wrapper_index"])
    candidate = selected["candidate"]
    s.require(candidate["operator"] != settings["sequencer"], "wrapper_must_differ_from_sequencer")
    s.uint(state["current_turn"], 32)
    rebound["proposal"].update(selected)
    rebound["proposal"]["accepted_package"].update(turn=state["current_turn"], wrapper=candidate["operator"],
                                                     wrapperBeneficiary=candidate["beneficiary"])
    return rebound


def status(config, rpc, now, roster_artifact=None):
    """Read the frozen package and schedule from one canonical, configuration-pinned head."""
    configuration(config)
    settings, limits = config["settings"], config["policy"]
    head, anchor = r.anchored_head(rpc, settings, limits, now)
    gate = settings["proof_gate"]
    check_code(rpc, gate, limits["gate_code_hash"], anchor, "gate_code_changed")
    call = lambda target, signature, kind="uint": r.call_word(rpc, target, signature, anchor, kind=kind)
    for signature, kind, expected in (("chain()", "address", settings["chain_address"]),
        ("childChainId()", "uint", s.uint(settings["execution_chain_id"])),
        ("sequencer()", "address", settings["sequencer"]),
        ("policyHash()", "bytes32", settings["policy_hash"]), ("productionVkHash()", "bytes32", settings["vk_hash"])):
        s.require(call(gate, signature, kind) == expected, "gate_configuration_changed")
    coordinator = call(gate, "coordinator()", "address")
    active = call(gate, "serviceActive()", "bool")
    result = {"schema_version": 1, "head": head, "service_active": active,
              "parent": call(gate, "lastAcceptedPackage()", "bytes32"),
              "priority_work_id": call(gate, "priorityWorkId()", "bytes32"),
              "next_native_batch": call(settings["chain_address"], "getTotalBatchesVerified()") + 1,
              "mode": "bootstrap", "control_work": False, "opening_roster": None, "frozen_package": None,
              "package_open": False, "frozen_package_hash": None, "current_turn": None,
              "selected_wrapper_index": None, "turn_deadline": None, "roster": None,
              "selected_candidate": None, "selected_candidate_proof": None, "local_operator_selected": None}
    s.uint(result["next_native_batch"], 64)
    if coordinator != s.ZERO_ADDRESS:
        s.require(coordinator == settings["coordinator"], "coordinator_configuration_changed")
        check_code(rpc, coordinator, limits["coordinator_code_hash"], anchor, "coordinator_code_changed")
        for signature, kind, expected in (("acceptanceGate()", "address", gate),
            ("childChainId()", "uint", s.uint(settings["execution_chain_id"])),
            ("childChainAddress()", "address", settings["chain_address"]), ("sequencer()", "address", settings["sequencer"]),
            ("policyHash()", "bytes32", settings["policy_hash"]), ("productionVkHash()", "bytes32", settings["vk_hash"]),
            ("acceptedParent()", "bytes32", result["parent"]), ("nextBatch()", "uint", result["next_native_batch"])):
            s.require(call(coordinator, signature, kind) == expected, "coordinator_configuration_changed")
        result["mode"] = "service" if active else "activation_pending"
        result["package_open"] = call(coordinator, "packageOpen()", "bool")
        s.require(active or not result["package_open"], "invalid_service_phase")
        if result["package_open"]:
            raw = raw_call(rpc, coordinator, "frozenPackage()", anchor, size=32 * len(s.PACKAGE))
            result["frozen_package"] = decode_package(raw)
            result["frozen_package_hash"] = call(coordinator, "frozenPackageHash()", "bytes32")
            result["current_turn"] = s.uint(call(coordinator, "currentTurn()"), 32)
            result["selected_wrapper_index"] = s.uint(call(coordinator, "selectedWrapperIndex()"), 32)
            result["turn_deadline"] = call(coordinator, "turnDeadline()")
            result["roster"] = {"period": result["frozen_package"]["period"],
                                "root": call(coordinator, "rosterRoot()", "bytes32"),
                                "count": s.uint(call(coordinator, "rosterCount()"), 32)}
            s.require(result["selected_wrapper_index"] < result["roster"]["count"], "selected_wrapper_out_of_range")
            s.require(result["turn_deadline"] > s.uint(head["timestamp"]), "invalid_turn_deadline")
            result["control_work"] = call(gate, "transitionWork()", "bool")
        else:
            raw = raw_call(rpc, coordinator, "openingRoster()", anchor, size=128)
            numbers = [int.from_bytes(raw[i:i + 32], "big") for i in (0, 64, 96)]
            s.require(numbers[2] in (0, 1), "invalid_control_status")
            result["opening_roster"] = {"period": s.uint(numbers[0], 64), "root": "0x" + raw[32:64].hex(),
                                       "count": s.uint(numbers[1], 32)}
            result["control_work"] = bool(numbers[2])
            result["roster"] = result["opening_roster"]
        s.nonzero(result["roster"]["root"])
        s.require(result["roster"]["count"] > 0, "invalid_roster_count")
    else:
        s.require(not active, "invalid_service_phase")
        result["package_open"] = result["priority_work_id"] != s.ZERO
        if result["package_open"]:
            result["frozen_package"] = decode_package(raw_call(rpc, gate, "bootstrapPackage()", anchor,
                                                              size=32 * len(s.PACKAGE)))
            result["frozen_package_hash"] = s.struct_hash("AcceptedPackageV1", s.PACKAGE, result["frozen_package"])
            result["local_operator_selected"] = limits["expected_operator"] == settings["sequencer"]
    if result["package_open"]:
        check_frozen(settings, result)
        if result["mode"] == "service" and roster_artifact is not None:
            selected = rosters.selected(roster_artifact, result["roster"], result["selected_wrapper_index"])
            result["selected_candidate"] = selected["candidate"]
            result["selected_candidate_proof"] = selected["candidate_proof"]
            result["local_operator_selected"] = selected["candidate"]["operator"] == limits["expected_operator"]
    elif result["roster"] is not None and roster_artifact is not None:
        rosters.authenticate(roster_artifact, result["roster"])
    r.check_anchor(rpc, settings, head)
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--config", required=True)
    parser.add_argument("--output", required=True)
    commands = parser.add_subparsers(dest="command", required=True)
    commands.add_parser("status").add_argument("--roster")
    for name in ("permit", "open", "repair"):
        command = commands.add_parser(name)
        for field in ("request", "evidence", "payload"):
            command.add_argument("--" + field, required=True)
        if name == "permit":
            command.add_argument("--runtime-seconds", type=int, required=True)
    command = commands.add_parser("prefix-witness")
    command.add_argument("--evidence", required=True)
    command.add_argument("--witness", required=True)
    for name in ("prepare-roster", "record-roster", "request-draw", "capture-draw", "relay-draw", "relay-checkpoint", "refresh-priority"):
        commands.add_parser(name).add_argument("--arguments", required=True)
    args = parser.parse_args()
    config = configuration(s.read_json(args.config))
    rpc, now = rpc_for(config), time.time()
    if args.command == "status":
        result = status(config, rpc, now, s.read_json(args.roster) if args.roster else None)
    elif args.command in ("permit", "open", "repair"):
        request, evidence = s.read_json(args.request), s.read_json(args.evidence)
        payload = s.read_json(args.payload, maximum=256 * 1024 * 1024)
        result = permit(config, rpc, request, evidence, payload, now, args.runtime_seconds) if args.command == "permit" else package_call(
            config, rpc, request, evidence, payload, now, args.command)
    elif args.command == "prefix-witness":
        result = prefix_call(config, rpc, s.read_json(args.evidence), s.read_json(args.witness), now)
    else:
        result = maintenance(config, rpc, args.command, s.read_json(args.arguments), now)
    s.write_new(args.output, result)


if __name__ == "__main__":
    os.umask(0o077)
    try:
        main()
    except (s.Error, OSError, ValueError, TypeError, KeyError) as error:
        print(str(error) if isinstance(error, s.Error) else "keeper_operation_failed", file=sys.stderr)
        sys.exit(1)
