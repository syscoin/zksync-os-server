#!/usr/bin/env python3
"""Drive a selected wrapper turn from the private native lease to node settlement."""

import argparse
from contextlib import nullcontext
import os
from pathlib import Path
import signal
import sys
import time
import uuid

import audit
import dispatcher
import keeper as k
import native_handoff
import proof_check
import relay as r
import roster as rosters
import service as s
import transactions
import workflow_io as io

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "prover-rental"))
import job
import sentry
from runpod import Store, atomic_json, read_private_json


def configuration(value):
    s.exact(value, ("schema_version", "keeper", "endpoint", "native_auth_file", "release_file",
                    "native_lease_seconds", "dispatcher_dir", "roster_dir", "operator_inboxes",
                    "priority_witness_dir", "publication_dir", "sequencer_wallet_file", "relay_wallet_file",
                    "transaction_policy", "relay_policy", "sequencer_beneficiary", "poll_interval_seconds"))
    s.require(value["schema_version"] == 1, "unsupported_coordinator_configuration")
    k.configuration(value["keeper"])
    settings, pins = value["keeper"]["settings"], value["keeper"]["policy"]
    s.require(settings["coordinator"] != s.ZERO_ADDRESS and pins["expected_operator"] == settings["sequencer"],
              "coordinator_requires_sequencer_keeper")
    sentry.endpoint_url(value["endpoint"])
    for name in ("dispatcher_dir", "roster_dir", "priority_witness_dir", "publication_dir"):
        io.directory(value[name])
    for name in ("native_auth_file", "release_file", "sequencer_wallet_file", "relay_wallet_file"):
        s.require(Path(value[name]).is_absolute(), "absolute_coordinator_file_required")
    release = job.release_identity(job.read_file(value["release_file"], job.MAX_MANIFEST, private=True))
    s.require(release["stage"] == "SNARK" and release["vk_hash"] == settings["vk_hash"], "wrong_snark_release")
    s.require(0 < s.uint(value["native_lease_seconds"], 32) <= 86400, "invalid_native_lease_seconds")
    s.nonzero(value["sequencer_beneficiary"], 20)
    s.require(type(value["operator_inboxes"]) is dict and 0 < len(value["operator_inboxes"]) <= rosters.MAX_CANDIDATES,
              "invalid_operator_inboxes")
    for operator, path in value["operator_inboxes"].items():
        s.nonzero(operator, 20)
        io.directory(path)
    for name in ("transaction_policy", "relay_policy"):
        r.policy(value[name])
        for pin in ("gate_code_hash", "coordinator_code_hash", "priority_guard_code_hash"):
            s.require(value[name][pin] == pins[pin], "transaction_code_pin_mismatch")
    s.require(value["transaction_policy"]["account"] == settings["sequencer"]
              and value["relay_policy"]["account"] != settings["sequencer"], "separate_relay_nonce_account_required")
    s.require(0 < s.uint(value["poll_interval_seconds"], 32) <= 60, "invalid_coordinator_poll_interval")
    return value


def initialize(root, config):
    configuration(config)
    root = io.directory(root, create=True)
    s.require(not any(root.iterdir()), "coordinator_state_must_be_empty")
    for name in ("jobs", "signatures"):
        io.directory(root / name, create=True)
    raw = job.read_file(config["release_file"], job.MAX_MANIFEST, private=True)
    io.immutable_bytes(root / "release.json", raw)
    io.immutable_json(root / "configuration.json", config)
    transactions.initialize(root / "transactions", config["keeper"]["settings"], config["transaction_policy"])
    r.Store.initialize(root / "relay", config["keeper"]["settings"], config["relay_policy"])
    atomic_json(root / "coordinator.json", {"schema_version": 1, "configuration_sha256": io.digest(config),
                "release_sha256": job.hash_bytes(raw), "active": None, "operations": {}})
    return Store(root)


class Coordinator:
    def __init__(self, store, rpc=None, wallet=None, relay_wallet=None, native=None, clock=time.time):
        self.store, self.clock = store, clock
        self.config = configuration(io.private_json(store.root / "configuration.json"))
        self.state = io.private_json(store.root / "coordinator.json")
        s.require(self.state["schema_version"] == 1 and self.state["configuration_sha256"] == io.digest(self.config),
                  "coordinator_configuration_changed")
        self.keeper, self.settings = self.config["keeper"], self.config["keeper"]["settings"]
        self.rpc = rpc or k.rpc_for(self.keeper)
        self.wallet, self.relay_wallet = wallet, relay_wallet
        self.native = native or job.NativeNetwork()
        self.release = job.read_file(store.root / "release.json", job.MAX_MANIFEST, private=True)
        s.require(job.hash_bytes(self.release) == self.state["release_sha256"], "coordinator_release_changed")

    def save(self, execute=True):
        if execute:
            atomic_json(self.store.root / "coordinator.json", self.state)

    def signing_wallet(self):
        return self.wallet or io.wallet_connection(self.config["sequencer_wallet_file"], self.keeper["policy"]["rpc_timeout_seconds"])

    def sign(self, request, execute):
        return io.typed_signature(self.store.root / "signatures", request,
                                  self.signing_wallet() if execute else None, execute)

    def status(self):
        state = k.status(self.keeper, self.rpc, int(self.clock()))
        if state["roster"] is None:
            return state, None
        roster = io.private_json(Path(self.config["roster_dir"]) / (str(state["roster"]["period"]) + ".json"))
        state = k.status(self.keeper, self.rpc, int(self.clock()), roster)
        return state, roster

    def prepare(self, request, evidence, payload):
        return k.prepare(self.settings, request, evidence, payload, enrollment=k.enrollment_for(self.keeper, request))

    def directory(self, identifier):
        return self.store.root / "jobs" / identifier

    def reserve(self):
        s.require(len(self.state["operations"]) < 1000, "coordinator_history_capacity")
        identifier = uuid.uuid4().hex
        io.directory(self.directory(identifier), create=True)
        self.state["operations"][identifier] = {"status": "picking", "leases": [], "lease": None,
                    "bundle": None, "proof": None, "works": {}, "relay": None, "maintenance": None,
                    "publication": None, "last_error": None}
        self.state["active"] = identifier
        self.save()
        return identifier

    def acquire(self, identifier, execute):
        op, directory = self.state["operations"][identifier], self.directory(identifier)
        if not execute:
            return "would_pick_native_snark"
        if op["lease"] is None:
            s.require(len(op["leases"]) < 256, "native_lease_attempt_limit")
            name = "lease-" + str(len(op["leases"]))
            op["leases"].append({"name": name, "started_at": int(self.clock()),
                                 "deadline": int(self.clock()) + self.config["native_lease_seconds"]})
            op["lease"] = name
            self.save()
        lease = directory / op["lease"]
        if not os.path.lexists(lease / "authority.json"):
            bounds = {}
            if (directory / "payload.json").exists():
                frozen = io.private_json(directory / "payload.json", io.MAX_PAYLOAD)
                bounds["expected_range"] = (frozen["from_batch_number"], frozen["to_batch_number"])
            sentry.reset_unstarted_pick(lease, self.config["endpoint"], self.release,
                                        identifier + ":" + op["lease"], **bounds)
            # Only a proven pre-request restart may move the lease window forward.
            lease_info = next(item for item in op["leases"] if item["name"] == op["lease"])
            now = int(self.clock())
            lease_info.update(started_at=now, deadline=now + self.config["native_lease_seconds"])
            self.save()
            sentry.pick(lease, self.config["endpoint"], self.release, identifier + ":" + op["lease"],
                        sentry.authorization(self.config["native_auth_file"]), self.native, **bounds)
        authority = read_private_json(lease / "authority.json")
        if authority["status"] == "no_job":
            if op["bundle"] is None:
                op["status"], self.state["active"] = "no_job", None
                io.immutable_json(directory / "completed.json", op)
                del self.state["operations"][identifier]
            else:
                op["lease"] = None
            self.save()
            return "native_queue_empty"
        if authority["status"] == "pick_uncertain":
            s.require((lease / "picked-wire.json").exists(), "native_pick_uncertain_reconcile_before_new_pick")
            sentry.recover_pick(lease)
            authority = read_private_json(lease / "authority.json")
        s.require(authority["status"] in ("picked", "submission_pending", "accepted"), "native_lease_requires_reconciliation")
        s.require(authority["endpoint"] == sentry.endpoint_url(self.config["endpoint"])
                  and authority["release_sha256"] == self.state["release_sha256"]
                  and authority["job_id"] == identifier + ":" + op["lease"], "native_origin_changed")
        payload = io.private_json(lease / "payload.json", io.MAX_PAYLOAD)
        job.validate_payload(payload, "SNARK", self.settings["vk_hash"])
        io.immutable_json(directory / "payload.json", payload, io.MAX_PAYLOAD)
        if not (directory / "evidence.json").exists():
            start, end = payload["from_batch_number"], payload["to_batch_number"]
            status, _, raw = self.native.request(sentry.endpoint_url(self.config["endpoint"]) +
                f"prover-jobs/v1/SNARK/{start}/{end}/evidence", authorization=sentry.authorization(self.config["native_auth_file"]),
                maximum=sentry.MAX_EVIDENCE)
            s.require(status == 200, "native_evidence_unavailable_preserve_lease")
            evidence = job.decode(raw)
            statements = s.validate_evidence(self.settings, evidence)
            s.require(list(statements) == list(range(start, end + 1)), "native_evidence_range_changed")
            k.base_context(self.keeper, self.rpc, evidence, int(self.clock()))
            io.immutable_json(directory / "evidence.json", evidence)
        op["status"] = "working"
        self.save()
        return None

    def snapshot(self, identifier, chain, roster, execute):
        op, directory = self.state["operations"][identifier], self.directory(identifier)
        if op["bundle"] is not None:
            return io.private_json(directory / "bundle.json")
        if not execute:
            return None
        payload = io.private_json(directory / "payload.json", io.MAX_PAYLOAD)
        evidence = io.private_json(directory / "evidence.json")
        snapshot_path = directory / "snapshot.json"
        if snapshot_path.exists():
            snapshot = io.private_json(snapshot_path)
        else:
            journal_root = Path(self.config["dispatcher_dir"])
            if not (journal_root / "state.json").exists():
                journal_root = journal_root / str(chain["roster"]["period"])
            store = Store(io.directory(journal_root))
            with store.lock("dispatcher.lock"):
                dispatch = dispatcher.Dispatcher(store, now=int(self.clock()), registry_rpc=k.registry_rpc_for(self.keeper))
                lane = self.keeper["lane"]
                s.require(dispatch.state["lanes"][lane]["settings"] == self.settings
                          and dispatch.state["lanes"][lane]["endpoint"] == sentry.endpoint_url(self.config["endpoint"]),
                          "dispatcher_lane_changed")
                start, end = payload["from_batch_number"], payload["to_batch_number"]
                dispatch.network = self.native
                authorization = sentry.authorization(self.config["native_auth_file"])
                dispatch.auth = {lane: authorization} if len(dispatch.state["lanes"]) > 1 else authorization
                for key, operation in list(dispatch.state["operations"].items()):
                    assignment = operation["assignment"]
                    if operation["lane"] != lane or assignment is not None and not start <= assignment["batch_number"] <= end:
                        continue
                    if operation["status"] == "submission_pending":
                        source = dispatch.directory(key)
                        signed = io.private_json(source / "signed-proof.json")
                        if dispatch.recover_acceptance(key, signed["proof"]):
                            dispatch.accepted(key, io.private_json(source / "authority.json"), signed["proof"], signed["signature"])
                    s.require(operation["status"] not in audit.LIVE, "wait_for_dispatcher_range_reconciliation")
                manifest = dispatch.range_manifest_payload(lane, start, end)
                bundle = dispatch.audit_payload(lane, start, end)
                duties = []
                if not chain["control_work"]:
                    for key, operation in dispatch.state["operations"].items():
                        if operation["lane"] == lane and operation["status"] == "accepted" and operation["assignment"] is not None \
                                and start <= operation["assignment"]["batch_number"] <= end:
                            duties.append(io.private_json(dispatch.directory(key) / "duty.json"))
                statements = s.validate_evidence(self.settings, evidence)
                credited = {duty["batchNumber"] for duty in duties}
                exhausted = all(account["offered"] == self.settings["duties_per_round"]
                                for account in dispatch.state["accounts"].values())
                s.require(chain["control_work"] or exhausted or all(statement["count"] == 0 or number in credited
                          for number, statement in statements.items()), "wait_for_nonempty_service_duties")
                hashes = {number: s.keccak(s.decode_proof(proof))
                          for number, proof in zip(statements, payload["fri_proofs"])}
                s.require(all(duty["friProofHash"] == hashes[duty["batchNumber"]] for duty in duties),
                          "accepted_duty_native_payload_changed")
                for event in bundle["events"]:
                    if event["kind"] == "rejected":
                        operation = dispatch.state["operations"][event["payload"]["operation"]]
                        assignment = operation["assignment"]
                        if operation["lane"] == lane and start <= assignment["batch_number"] <= end:
                            rejected = io.private_json(dispatch.directory(event["payload"]["operation"]) / "signed-proof.json")
                            s.require(s.keccak(s.decode_proof(rejected["proof"]["proof"])) != hashes[assignment["batch_number"]],
                                      "rejected_operator_proof_reused")
                snapshot = {"manifest": manifest, "audit": bundle, "subscriptions": dispatch.state["subscriptions"],
                            "duties": sorted(duties, key=lambda duty: duty["batchNumber"])}
            io.immutable_json(snapshot_path, snapshot)
        manifest_signature = self.sign(s.manifest_request(self.settings, snapshot["manifest"]), True)
        audit_signature = self.sign(audit.checkpoint_request(snapshot["audit"]), True)
        candidate = next((entry for entry in roster["candidates"] if entry["operator"] != self.settings["sequencer"]), None)
        s.require(candidate is not None, "no_independent_wrapper_in_roster")
        accepted = {"domainVersion": 1, "protocolVersion": 32, "chainId": self.settings["execution_chain_id"],
            "chainAddress": self.settings["chain_address"], "parent": chain["parent"],
            "batchFrom": payload["from_batch_number"], "batchTo": payload["to_batch_number"],
            "period": snapshot["manifest"]["period"], "rosterRoot": chain["roster"]["root"],
            "policyHash": self.settings["policy_hash"], "vkHash": self.settings["vk_hash"], "turn": 0,
            "sequencer": self.settings["sequencer"], "sequencerBeneficiary": self.config["sequencer_beneficiary"],
            "wrapper": candidate["operator"], "wrapperBeneficiary": candidate["beneficiary"]}
        request = {"proposal": {"mode": "service", "accepted_package": accepted, "candidate": candidate,
                    "candidate_proof": roster["proofs"][candidate["index"]]},
                   "manifest": {"payload": snapshot["manifest"], "sequencer_signature": manifest_signature},
                   "subscriptions": snapshot["subscriptions"], "duties": snapshot["duties"]}
        s.require(accepted["period"] == chain["roster"]["period"], "dispatcher_period_does_not_match_opening_roster")
        self.prepare(request, evidence, payload)
        bundle = {"request": request, "audit": {**snapshot["audit"], "sequencer_signature": audit_signature}}
        io.immutable_json(directory / "bundle.json", bundle)
        op["bundle"] = io.digest(bundle)
        self.save()
        return bundle

    def maintenance(self, identifier, call, producer, execute):
        op = self.state["operations"][identifier]
        if not execute:
            return "would_" + call["action"]
        manager = transactions.Transactions(self.settings, self.config["transaction_policy"], self.rpc,
            self.signing_wallet(), transactions.Store(self.store.root / "transactions"), self.clock)
        if op["maintenance"] is None:
            op["maintenance"] = manager.plan(call)
            self.save()
        operation_id = self.stage_maintenance(op["maintenance"], manager, True)
        def preflight(original, head, anchor):
            if original["action"] in transactions.REPEATABLE and manager.state["operations"][operation_id].get("invocation") is None:
                return False
            try:
                current = producer()
            except s.Error:
                return False
            return current["transaction"] == original["transaction"] and current["anchor"]["hash"] == head["hash"]
        result = manager.step(operation_id, True, preflight)
        if result["status"] in transactions.TERMINAL:
            op["maintenance"] = None
            self.save()
        return result

    def stage_maintenance(self, pending, manager, execute):
        if isinstance(pending, str):
            return pending
        s.exact(pending, ("operation_id", "call", "invocation"))
        expected = r.sha256(transactions.intent(self.settings, self.config["transaction_policy"],
                                               pending["call"], pending["invocation"]))
        s.require(pending["operation_id"] == expected, "maintenance_descriptor_changed")
        if execute:
            s.require(manager.stage(pending["call"], pending["invocation"]) == expected, "maintenance_descriptor_changed")
        return expected

    def recover_maintenance(self, identifier, execute, bundle, evidence, payload):
        op = self.state["operations"][identifier]
        if op["maintenance"] is None:
            return None
        manager = transactions.Transactions(self.settings, self.config["transaction_policy"], self.rpc,
            self.signing_wallet() if execute else None, transactions.Store(self.store.root / "transactions"), self.clock)
        operation_id = self.stage_maintenance(op["maintenance"], manager, execute)
        if operation_id not in manager.state["operations"]:
            return {"next_action": "would_stage_maintenance", "operation_id": operation_id}
        def preflight(original, head, anchor):
            try:
                action = original["action"]
                # Legacy rows did not bind a checkpoint. A reserved nonce remains receipt-only recovery.
                if action in transactions.REPEATABLE and manager.state["operations"][operation_id].get("invocation") is None:
                    return False
                if action == "openPackage":
                    call = k.package_call(self.keeper, self.rpc, bundle["request"], evidence, payload, int(self.clock()), "open")
                elif action == "refreshPriorityCheckpoint":
                    call = k.maintenance(self.keeper, self.rpc, "refresh-priority", {}, int(self.clock()))
                elif action == "publishPrefixWitness":
                    work_id = r.call_word(self.rpc, self.settings["proof_gate"], "priorityWorkId()", anchor, kind="bytes32")
                    witness = io.private_json(Path(self.config["priority_witness_dir"]) / (work_id[2:] + ".json"))
                    call = k.prefix_call(self.keeper, self.rpc, evidence, witness, int(self.clock()))
                else:
                    return False
                return call["transaction"] == original["transaction"] and call["anchor"]["hash"] == head["hash"]
            except (s.Error, OSError):
                return False
        result = manager.step(operation_id, execute, preflight)
        # A staged call can be reconstructed below. Reserved nonces must first resolve.
        entry = manager.state["operations"][operation_id]
        if result["status"] in transactions.TERMINAL or entry["unsigned_transaction"] is None:
            if execute:
                op["maintenance"] = None
                self.save()
            return None
        return result

    def result(self, identifier, execute):
        op, directory = self.state["operations"][identifier], self.directory(identifier)
        evidence, payload = io.private_json(directory / "evidence.json"), io.private_json(directory / "payload.json", io.MAX_PAYLOAD)
        for work_hash, meta in reversed(list(op["works"].items())):
            path = Path(meta["directory"]) / "result.json"
            if not path.exists() or meta["checked"]:
                continue
            try:
                result = io.validate_result(self.settings, work_hash, io.private_json(path), meta["operator"])
                job.validate_payload(result["proof"], "SNARK", self.settings["vk_hash"], proof=True)
                proof_check.inputs(self.settings, evidence, payload, result["proof"])
            except (s.Error, job.Error, OSError, ValueError, TypeError, KeyError):
                if execute:
                    meta.update(checked=True, ignored=True, error="invalid_authenticated_result")
                    self.save()
                continue
            try:
                proof_check.verify_snark(self.keeper, self.rpc, evidence, payload, result["proof"], int(self.clock()))
            except s.Error as error:
                if str(error) != "native_snark_rejected":
                    raise
                if execute:
                    meta.update(checked=True, ignored=True, error=str(error))
                    self.save()
                continue
            if op["proof"] is not None and io.private_json(directory / "proof.json") != result["proof"]:
                # The native submission freezes one exact proof; later valid proofs cannot replace it.
                if execute:
                    meta["checked"] = True
                    meta["ignored"] = True
                    self.save()
                continue
            if not execute:
                return "would_retain_verified_wrapper_result"
            io.immutable_json(directory / "proof.json", result["proof"])
            io.immutable_json(directory / (work_hash[2:] + ".result.json"), result)
            op["proof"], meta["checked"] = io.digest(result["proof"]), True
            self.save()
        return None

    def relay(self, identifier, execute):
        op = self.state["operations"][identifier]
        if op["relay"] is None:
            return None
        store = r.Store(self.store.root / "relay")
        wallet = (self.relay_wallet or io.wallet_connection(self.config["relay_wallet_file"],
                   self.keeper["policy"]["rpc_timeout_seconds"])) if execute else None
        with store.lock() if execute else nullcontext():
            relay = r.Relay(self.settings, self.config["relay_policy"], self.rpc, wallet, store, self.clock)
            result = relay.step(op["relay"], execute)
            if result["status"] in ("accepted", "accepted_elsewhere"):
                s.require(op["publication"] is not None, "native_publication_missing")
                handoff = relay.export_handoff(op["relay"], op["publication"], execute)
                if execute:
                    op["status"], self.state["active"] = "settled", None
                    self.save()
                return handoff
            entry = relay.state["operations"][op["relay"]]
            if result["status"] in ("stale_unsigned", "reverted") and (entry["unsigned_transaction"] is None
                    or result["status"] == "reverted"):
                if execute:
                    op["relay"] = None
                    self.save()
                return None
            return result

    def step(self, execute=False):
        s.require(type(execute) is bool, "invalid_execute_flag")
        identifier = self.state["active"]
        if identifier is not None:
            result = self.relay(identifier, execute)
            if result is not None:
                return result
        chain, roster = self.status()
        s.require(chain["mode"] == "service" and chain["service_active"], "service_activation_required")
        if identifier is None:
            if not execute:
                return {"next_action": "would_pick_native_snark", "chain": chain}
            identifier = self.reserve()
        op, directory = self.state["operations"][identifier], self.directory(identifier)
        if op["lease"] is None or op["status"] == "picking":
            result = self.acquire(identifier, execute)
            if result:
                return {"next_action": result}
        payload, evidence = io.private_json(directory / "payload.json", io.MAX_PAYLOAD), io.private_json(directory / "evidence.json")
        if chain["next_native_batch"] > payload["to_batch_number"]:
            if execute:
                op["status"], self.state["active"] = "superseded", None
                self.save()
            return {"next_action": "range_already_proven"}
        s.require(chain["next_native_batch"] == payload["from_batch_number"], "native_frontier_does_not_match_retained_lease")
        bundle = self.snapshot(identifier, chain, roster, execute)
        if bundle is None:
            return {"next_action": "would_sign_dispatch_snapshot"}
        s.require(io.digest(bundle) == op["bundle"], "frozen_bundle_changed")
        pending = self.recover_maintenance(identifier, execute, bundle, evidence, payload)
        if pending is not None:
            return pending
        if not chain["package_open"]:
            produce = lambda: k.package_call(self.keeper, self.rpc, bundle["request"], evidence, payload, int(self.clock()), "open")
            return self.maintenance(identifier, produce(), produce, execute)
        request = k.rebind_request(self.settings, bundle["request"], chain, roster)
        prepared = self.prepare(request, evidence, payload)
        priority = r.priority_context(self.rpc, self.settings, {**self.keeper["policy"], "min_turn_seconds": 1},
                     {"sidecar": prepared}, chain["head"], {"blockHash": chain["head"]["hash"], "requireCanonical": True})
        r.check_anchor(self.rpc, self.settings, chain["head"])
        if priority["reason"] == "priority_checkpoint_needs_refresh":
            produce = lambda: k.maintenance(self.keeper, self.rpc, "refresh-priority", {}, int(self.clock()))
            return self.maintenance(identifier, produce(), produce, execute)
        if priority["reason"] == "priority_prefix_witness_missing":
            witness_path = Path(self.config["priority_witness_dir"]) / (priority["work_id"][2:] + ".json")
            if not witness_path.exists():
                return {"next_action": "await_authenticated_priority_witness", "work_id": priority["work_id"]}
            witness = io.private_json(witness_path)
            produce = lambda: k.prefix_call(self.keeper, self.rpc, evidence, witness, int(self.clock()))
            return self.maintenance(identifier, produce(), produce, execute)
        s.require(priority["reason"] == "ready", priority["reason"])
        result = self.result(identifier, execute)
        if result:
            return {"next_action": result}
        proof = io.private_json(directory / "proof.json") if op["proof"] is not None else None
        if proof is not None:
            s.require(io.digest(proof) == op["proof"], "retained_proof_changed")
            publication = native_handoff.publication_work(self.config["publication_dir"], self.settings,
                            self.config["relay_policy"], evidence, proof)
            if publication is not None:
                op["publication"] = str(publication)
                self.save(execute)
            else:
                if not execute:
                    return {"next_action": "would_submit_verified_native_snark"}
                lease_info = next(item for item in op["leases"] if item["name"] == op["lease"])
                authority = read_private_json(directory / op["lease"] / "authority.json")
                if int(self.clock()) >= lease_info["deadline"] and authority["status"] in ("picked", "rejected"):
                    op["lease"], op["status"] = None, "picking"
                    self.save()
                    return {"next_action": "renew_expired_native_lease_same_frozen_payload"}
                outcome = native_handoff.submit(directory / op["lease"], proof,
                            sentry.authorization(self.config["native_auth_file"]), self.native)
                s.require(outcome != "rejected", "native_submission_rejected_reconcile_lease")
                return {"next_action": "await_node_publication", "native_status": outcome}
            for work_hash, meta in reversed(list(op["works"].items())):
                if not meta["checked"] or meta.get("ignored"):
                    continue
                result = io.private_json(directory / (work_hash[2:] + ".result.json"))
                if result["prepared"] is None:
                    continue
                expected = s.prepare_package(self.settings, evidence, request["manifest"], request["subscriptions"],
                              request["duties"], request["proposal"], proof, payload,
                              enrollment=k.enrollment_for(self.keeper, request))
                if result["prepared"] != expected:
                    continue
                s.verify_eoa(expected["wrapper_request"], result["wrapper_signature"])
                signature = self.sign(expected["sequencer_request"], execute)
                if not execute:
                    return {"next_action": "would_endorse_package"}
                sidecar = s.complete_package(self.settings, expected, signature, result["wrapper_signature"])
                item = r.artifact(self.settings, sidecar, expected["proof_data"])
                relay_store = r.Store(self.store.root / "relay")
                with relay_store.lock():
                    relay = r.Relay(self.settings, self.config["relay_policy"], self.rpc, store=relay_store, clock=self.clock)
                    op["relay"] = relay.stage(item)
                self.save()
                return {"next_action": "relay_dual_endorsed_package", "operation_id": op["relay"]}
        lease_info = next(item for item in op["leases"] if item["name"] == op["lease"])
        if proof is None and int(self.clock()) >= lease_info["deadline"]:
            authority = read_private_json(directory / op["lease"] / "authority.json")
            s.require(authority["status"] == "picked", "uncertain_submission_blocks_lease_renewal")
            if execute:
                op["lease"], op["status"] = None, "picking"
                self.save()
            return {"next_action": "renew_expired_native_lease_same_frozen_payload"}
        operator = request["proposal"]["candidate"]["operator"]
        inbox = self.config["operator_inboxes"].get(operator)
        if inbox is None:
            return {"next_action": "await_selected_operator_transport", "operator": operator,
                    "turn": chain["current_turn"], "deadline": chain["turn_deadline"]}
        body = {"schema_version": 1, "lane": self.keeper["lane"], "release_sha256": self.state["release_sha256"],
                "payload_sha256": io.digest(payload), "request": request, "evidence": evidence, "audit": bundle["audit"],
                "native_lease_deadline": lease_info["deadline"], "proof": proof}
        signing_request = io.work_request(self.settings, body)
        work_hash = signing_request["typed_data"]["message"]["workHash"]
        signature = self.sign(signing_request, execute)
        if not execute:
            return {"next_action": "would_deliver_selected_wrapper", "operator": operator, "turn": chain["current_turn"]}
        source = io.publish_work(inbox, {"body": body, "sequencer_signature": signature}, payload)
        if work_hash not in op["works"]:
            s.require(len(op["works"]) < 256, "wrapper_turn_attempt_limit")
            op["works"][work_hash] = {"directory": str(source), "operator": operator, "checked": False}
            self.save()
        return {"next_action": "await_selected_wrapper", "operator": operator, "turn": chain["current_turn"],
                "deadline": chain["turn_deadline"], "work_hash": work_hash}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--state-dir", required=True)
    parser.add_argument("--execute", action="store_true")
    commands = parser.add_subparsers(dest="command", required=True)
    init = commands.add_parser("init")
    init.add_argument("--config", required=True)
    commands.add_parser("status")
    run = commands.add_parser("run")
    run.add_argument("--once", action="store_true")
    args = parser.parse_args()
    if args.command == "init":
        config = io.private_json(args.config)
        configuration(config)
        if args.execute:
            initialize(args.state_dir, config)
        print("initialized" if args.execute else "dry-run: configuration valid")
        return
    stopped = False
    def stop(*_):
        nonlocal stopped
        stopped = True
    signal.signal(signal.SIGINT, stop)
    signal.signal(signal.SIGTERM, stop)
    store = Store(args.state_dir)
    while not stopped:
        try:
            with store.lock("coordinator.lock") if args.execute else nullcontext():
                coordinator = Coordinator(store)
                result = coordinator.status()[0] if args.command == "status" else coordinator.step(args.execute)
            print(s.canonical(result).decode(), flush=True)
        except (s.Error, job.Error, OSError, ValueError, TypeError, KeyError) as error:
            print(str(error) if isinstance(error, (s.Error, job.Error)) else "coordinator_step_failed", file=sys.stderr, flush=True)
            if args.command == "status" or args.once:
                raise
        if args.command == "status" or args.once:
            return
        time.sleep(coordinator.config["poll_interval_seconds"] if "coordinator" in locals() else 5)


if __name__ == "__main__":
    os.umask(0o077)
    try:
        main()
    except (s.Error, job.Error, OSError, ValueError, TypeError, KeyError) as error:
        print(str(error) if isinstance(error, (s.Error, job.Error)) else "coordinator_failed", file=sys.stderr)
        sys.exit(1)
