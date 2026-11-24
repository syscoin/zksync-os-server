#!/usr/bin/env python3
"""Drive a selected wrapper turn from the private native lease to node settlement."""

import argparse
import copy
from contextlib import contextmanager, ExitStack, nullcontext
import os
from pathlib import Path
import re
import signal
import sys
import time
import uuid

import audit
import dispatcher
import journal
import keeper as k
import native_handoff
import period_routing
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


MAX_RETAINED_NATIVE_LEASES = 256
WRAPPER_WORK_PAGE_SIZE = 256


def configuration(value):
    s.exact(value, ("schema_version", "keeper", "endpoint", "native_auth_file", "release_file",
                    "native_lease_seconds", "dispatcher_dir", "roster_dir", "operator_inboxes",
                    "publication_dir", "sequencer_wallet_file", "relay_wallet_file",
                    "transaction_policy", "relay_policy", "sequencer_beneficiary", "poll_interval_seconds"))
    s.require(value["schema_version"] == 1, "unsupported_coordinator_configuration")
    k.configuration(value["keeper"])
    settings, pins = value["keeper"]["settings"], value["keeper"]["policy"]
    s.require(settings["coordinator"] != s.ZERO_ADDRESS and pins["expected_operator"] == settings["sequencer"],
              "coordinator_requires_sequencer_keeper")
    sentry.endpoint_url(value["endpoint"])
    for name in ("dispatcher_dir", "roster_dir", "publication_dir"):
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
        for pin in ("gate_code_hash", "coordinator_code_hash"):
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
    journal.save(root / "coordinator.json", {"schema_version": 1, "configuration_sha256": io.digest(config),
                "release_sha256": job.hash_bytes(raw), "active": None, "operations": {}})
    return Store(root)


class Coordinator:
    def __init__(self, store, rpc=None, wallet=None, relay_wallet=None, native=None, clock=time.time):
        self.store, self.clock = store, clock
        self.config = configuration(io.private_json(store.root / "configuration.json"))
        self.state = journal.load(store.root / "coordinator.json")
        s.require(self.state["schema_version"] == 1 and self.state["configuration_sha256"] == io.digest(self.config),
                  "coordinator_configuration_changed")
        self.keeper, self.settings = self.config["keeper"], self.config["keeper"]["settings"]
        self.rpc = rpc or k.rpc_for(self.keeper)
        self.wallet, self.relay_wallet = wallet, relay_wallet
        self.native = native or job.NativeNetwork()
        self.registry_rpc, self.enrollment = None, None
        self.release = job.read_file(store.root / "release.json", job.MAX_MANIFEST, private=True)
        s.require(job.hash_bytes(self.release) == self.state["release_sha256"], "coordinator_release_changed")

    def save(self, execute=True):
        if execute:
            journal.save(self.store.root / "coordinator.json", self.state)

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
        return k.prepare(self.settings, request, evidence, payload, enrollment=self.enrollment_for(request))

    def enrollment_for(self, request):
        if request["manifest"]["payload"]["schema_version"] == 2:
            payload = request["manifest"]["payload"]
            chain = {"roster": {"period": payload["period"]}, "control_work": False}
            current = self.status()[0]
            if current["roster"] is not None and current["roster"]["period"] == payload["period"]:
                chain["control_work"] = current["control_work"]
                chain["head"] = current["head"]
            with self.dispatchers_for(chain,
                                      request["proposal"]["accepted_package"]["batchFrom"],
                                      request["proposal"]["accepted_package"]["batchTo"]) as (current, dispatches):
                authorities = {item.state["journal_id"]: item.enrollment for item in dispatches}
                self.enrollment = s.EnrollmentGroup(self.settings, authorities, current.state["journal_id"], self.registry_rpc)
        elif type(self.enrollment) is not s.EnrollmentAuthority or self.enrollment.period != request["manifest"]["payload"]["period"]:
            self.enrollment = None
        self.enrollment = k.enrollment_for(self.keeper, request, self.registry_rpc, enrollment=self.enrollment)
        return self.enrollment

    @contextmanager
    def dispatchers_for(self, chain, start, end):
        with self.dispatcher_for(chain) as current, ExitStack() as locks:
            base = period_routing.root_store(current.store)
            candidates = [base]
            parent = base.root / "periods"
            if parent.exists():
                s.require(not parent.is_symlink(), "period_directory_symlink")
                for path in sorted(parent.iterdir()):
                    if re.fullmatch(r"[0-9]+-(bootstrap|service)", path.name):
                        s.require(not path.is_symlink(), "period_directory_symlink")
                        candidates.append(Store(path))
            dispatches = [current]
            for store in candidates:
                if store.root == current.store.root:
                    continue
                # The base lock serializes routing; child locks serialize reconciliation.
                if store.root != base.root:
                    locks.enter_context(store.lock("dispatcher.lock"))
                state = journal.load(store.root / "state.json", list_fields=("audit.events",))
                try:
                    relevant = state["period"] <= chain["roster"]["period"] and any(
                        True for _ in journal.range_items(state["operations"], self.keeper["lane"], start, end))
                finally:
                    if state.database is not None:
                        state.database.close()
                if not relevant:
                    continue
                item = dispatcher.Dispatcher(store, now=int(self.clock()), registry_rpc=self.registry_rpc,
                                             reuse_local_enrollment=False)
                lane = item.state["lanes"].get(self.keeper["lane"])
                s.require(lane is not None and lane["settings"] == self.settings
                          and lane["endpoint"] == sentry.endpoint_url(self.config["endpoint"]), "dispatcher_lane_changed")
                dispatches.append(item)
            try:
                yield current, sorted(dispatches, key=lambda item: item.state["journal_id"])
            finally:
                for item in dispatches:
                    if item.state.database is not None:
                        item.state.database.close()

    def context_snapshot(self, dispatch, payload, evidence):
        lane = self.keeper["lane"]
        start, end = payload["from_batch_number"], payload["to_batch_number"]
        dispatch.network = self.native
        authorization = sentry.authorization(self.config["native_auth_file"])
        dispatch.auth = {lane: authorization} if len(dispatch.state["lanes"]) > 1 else authorization
        operations = dict(journal.range_items(dispatch.state["operations"], lane, start, end))
        pending = dispatch.state.get("pending_pick")
        if pending is not None:
            operation = dispatch.state["operations"][pending]
            if operation["lane"] == lane and operation["assignment"] is None:
                operations[pending] = operation
        for key in operations:
            operation = dispatch.state["operations"][key]
            if operation["status"] == "submission_pending":
                source = dispatch.directory(key)
                signed = io.private_json(source / "signed-proof.json")
                if dispatch.recover_acceptance(key, signed["proof"]):
                    dispatch.accepted(key, io.private_json(source / "authority.json"), signed["proof"], signed["signature"])
            s.require(operation["status"] not in audit.LIVE, "wait_for_dispatcher_range_reconciliation")
        manifest = dispatch.range_manifest_payload(lane, start, end)
        bundle = dispatch.audit_payload(lane, start, end, export_events=False)
        receipts = [io.private_json(dispatch.directory(key) / "duty.json") for key, operation in
                    journal.range_items(dispatch.state["operations"], lane, start, end) if operation["status"] == "accepted"]
        statements = s.validate_evidence(self.settings, evidence)
        hashes = {number: s.keccak(s.decode_proof(proof)) for number, proof in zip(statements, payload["fri_proofs"])}
        s.require(all(duty["friProofHash"] == hashes[duty["batchNumber"]] for duty in receipts),
                  "accepted_duty_native_payload_changed")
        for event in dispatch.state["audit"]["events"]:
            if event["kind"] == "rejected" and event["payload"]["operation"] in operations:
                operation = dispatch.state["operations"][event["payload"]["operation"]]
                rejected = io.private_json(dispatch.directory(event["payload"]["operation"]) / "signed-proof.json")
                s.require(s.keccak(s.decode_proof(rejected["proof"]["proof"])) != hashes[operation["assignment"]["batch_number"]],
                          "rejected_operator_proof_reused")
        return {"manifest": manifest, "audit": bundle, "subscriptions": dispatch.state["subscriptions"], "receipts": receipts}

    @contextmanager
    def dispatcher_for(self, chain):
        period = chain["roster"]["period"]
        pin = k.enrollment_pin(self.keeper, period)
        store = period_routing.root_store(Store(io.directory(self.config["dispatcher_dir"])))
        with ExitStack() as locks:
            locks.enter_context(store.lock("dispatcher.lock"))
            state = journal.load(store.root / "state.json", list_fields=("audit.events",))
            # A gate can activate before its peer receives the activation message. Its
            # prestart control proof uses the authenticated one-time bootstrap journal.
            phase = "bootstrap" if chain.get("control_work", False) and state["enrollment"]["phase"] == "bootstrap" \
                and int(self.clock()) < state["enrollment"]["ends_at"] and type(chain.get("head")) is dict \
                and s.uint(chain["head"]["timestamp"]) < state["enrollment"]["ends_at"] else "service"
            if state["period"] != period or state["enrollment"]["phase"] != phase \
                    or state["enrollment"]["block_hash"] != pin:
                try:
                    store = period_routing.current_store(store, None, int(self.clock()), requested_period=period,
                                  existing_only=True, requested_phase=phase,
                                  enrollment_block_hash=pin)
                except s.Error as error:
                    if state["period"] == period and str(error) == "unknown_period_journal":
                        raise s.Error("enrollment_authority_anchor_mismatch") from None
                    raise
                locks.enter_context(store.lock("dispatcher.lock"))
                state = journal.load(store.root / "state.json", list_fields=("audit.events",))
            s.require(state["period"] == period, "dispatcher_period_does_not_match_opening_roster")
            s.require(state["enrollment"]["block_hash"] == pin,
                      "enrollment_authority_anchor_mismatch")
            if self.registry_rpc is None:
                self.registry_rpc = k.registry_rpc_for(self.keeper)
            dispatch = dispatcher.Dispatcher(store, now=int(self.clock()), registry_rpc=self.registry_rpc,
                                               reuse_local_enrollment=False,
                                               enrollment=self.enrollment if type(self.enrollment) is s.EnrollmentAuthority
                                               and self.enrollment.block_hash == pin else None)
            s.require(phase != "bootstrap" or dispatch.enrollment.anchor == s.canonical(state["enrollment"]),
                      "bootstrap_enrollment_authority_changed")
            lane = dispatch.state["lanes"].get(self.keeper["lane"])
            s.require(lane is not None and lane["settings"] == self.settings
                      and lane["endpoint"] == sentry.endpoint_url(self.config["endpoint"]), "dispatcher_lane_changed")
            self.enrollment = dispatch.enrollment
            yield dispatch

    def directory(self, identifier):
        return self.store.root / "jobs" / identifier

    def reserve(self):
        identifier = uuid.uuid4().hex
        io.directory(self.directory(identifier), create=True)
        self.state["operations"][identifier] = {"status": "picking", "leases": [], "lease": None,
                    "bundle": None, "proof": None, "works": {}, "relay": None, "maintenance": None,
                    "publication": None, "last_error": None}
        self.state["active"] = identifier
        self.save()
        return identifier

    def archive_finished_attempt(self, identifier, attempt, expected_range=None):
        lease = self.directory(identifier) / attempt["name"]
        authority = lease / "authority.json"
        if not authority.exists():
            return False
        record = read_private_json(authority)
        status = record["status"]
        if status not in ("no_job", "picked", "rejected"):
            return False
        if status != "no_job" and (expected_range is None or int(self.clock()) < attempt["deadline"]):
            return False
        s.require(record["job_id"] == identifier + ":" + attempt["name"]
                  and record["endpoint"] == sentry.endpoint_url(self.config["endpoint"])
                  and record["release_sha256"] == self.state["release_sha256"]
                  and record["stage"] == "SNARK" and record["vk_hash"] == self.settings["vk_hash"]
                  and record["operation_id"] is None and record["manifest_sha256"] is None,
                  "native_origin_changed")
        if status == "no_job":
            s.require(record["lease_token"] is None and record["submission_sha256"] is None,
                      "native_unleased_authority_changed")
        else:
            bounds = sentry.validate_expected_range("SNARK", expected_range)
            s.require(record["bounds"] == bounds and record.get("expected_bounds", bounds) == bounds,
                      "archived_lease_range_changed")
            job.b256(record["lease_token"])
            if status == "rejected":
                submitted = job.read_file(lease / "submission.json", job.MAX_SUBMIT, private=True)
                s.require(record["submission_sha256"] == job.hash_bytes(submitted), "native_terminal_submission_changed")
            else:
                s.require(record["submission_sha256"] is None, "uncertain_submission_blocks_lease_renewal")
        io.immutable_json(lease / "attempt.json", attempt)
        return True

    def acquire(self, identifier, execute, *, next_native_batch=None):
        op, directory = self.state["operations"][identifier], self.directory(identifier)
        if not execute:
            return "would_pick_native_snark"
        lease = directory / op["lease"] if op["lease"] is not None else None
        expected_range = None
        if lease is None or not os.path.lexists(lease / "authority.json"):
            if lease is not None and os.path.lexists(lease):
                sentry.require(all(entry.name == "release.json" or re.fullmatch(r"\.authority\.json\.[0-9a-f]{32}\.tmp", entry.name)
                                   for entry in io.directory(lease).iterdir()),
                               "pick_initialization_contains_unknown_artifacts")
            # An existing uncertain pick keeps its reconciliation path. Every new native
            # request first authenticates the configured journal and immutable enrollment.
            chain, _ = self.status()
            s.require(chain["mode"] == "service" and chain["service_active"], "service_activation_required")
            with self.dispatcher_for(chain):
                pass
            if (directory / "payload.json").exists():
                frozen = io.private_json(directory / "payload.json", io.MAX_PAYLOAD)
                expected_range = (frozen["from_batch_number"], frozen["to_batch_number"])
        if op["lease"] is None:
            if len(op["leases"]) >= MAX_RETAINED_NATIVE_LEASES:
                # Keep uncertain picks and submissions in the active index. Finished
                # attempts retain their private lease evidence on disk.
                op["leases"] = [item for item in op["leases"]
                                if not self.archive_finished_attempt(identifier, item, expected_range)]
                s.require(len(op["leases"]) < MAX_RETAINED_NATIVE_LEASES, "native_lease_attempt_limit")
            name = "lease-" + uuid.uuid4().hex
            s.require(not os.path.lexists(directory / name), "native_lease_directory_already_exists")
            op["leases"].append({"name": name, "started_at": int(self.clock()),
                                 "deadline": int(self.clock()) + self.config["native_lease_seconds"]})
            op["lease"] = name
            self.save()
        lease = directory / op["lease"]
        if not os.path.lexists(lease / "authority.json"):
            sentry.reset_unstarted_pick(lease, self.config["endpoint"], self.release,
                                        identifier + ":" + op["lease"], expected_range=expected_range)
            # Only a proven pre-request restart may move the lease window forward.
            lease_info = next(item for item in op["leases"] if item["name"] == op["lease"])
            now = int(self.clock())
            lease_info.update(started_at=now, deadline=now + self.config["native_lease_seconds"])
            self.save()
            sentry.pick(lease, self.config["endpoint"], self.release, identifier + ":" + op["lease"],
                        sentry.authorization(self.config["native_auth_file"]), self.native, expected_range=expected_range)
        authority = read_private_json(lease / "authority.json")
        if authority["status"] == "no_job":
            if op["bundle"] is None:
                op["status"], self.state["active"] = "no_job", None
                io.immutable_json(directory / "completed.json", op)
                del self.state["operations"][identifier]
            else:
                attempt = next(item for item in op["leases"] if item["name"] == op["lease"])
                s.require(self.archive_finished_attempt(identifier, attempt), "native_unleased_authority_changed")
                op["leases"].remove(attempt)
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
        if next_native_batch is not None and payload["from_batch_number"] > next_native_batch:
            return "await_native_frontier_for_picked_lease"
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

    @staticmethod
    def retained_bundle(job_root, op):
        bundle = io.private_json(job_root / op.get("bundle_file", "bundle.json"))
        s.require(io.digest(bundle) == op["bundle"], "frozen_bundle_changed")
        return bundle

    @staticmethod
    def check_rollover_history(dispatches, receipts, manifest, audit_bundle, duties):
        if audit_bundle["schema_version"] == 3:
            sources = [(ctx["request"]["manifest"]["payload"], ctx["bundle"]) for ctx in audit_bundle["contexts"]]
        else:
            sources = [(manifest, audit_bundle)]
        ledgers = {item.state["journal_id"]: item.state["audit"]["events"] for item in dispatches}
        for source, original in sources:
            if not (source["assignments"] or source["retries"]):
                continue
            events = ledgers.get(original["journal_id"])
            count = audit.event_count(original)
            s.require(events is not None and 0 < count <= len(events)
                      and events[count - 1]["hash"] == original["checkpoint"]["eventHead"], "rollover_audit_prefix_changed")
        observed = {s.canonical(duty) for duty in receipts}
        s.require(all(s.canonical(duty) in observed for duty in duties), "rollover_duty_receipt_changed")

    def snapshot(self, identifier, chain, roster, execute):
        op, job_root = self.state["operations"][identifier], self.directory(identifier)
        directory = job_root / op.get("snapshot_directory", ".")
        period, root = chain["roster"]["period"], chain["roster"]["root"]
        rollover = False
        old_ref = None
        retained_manifest = None
        prior_history = None
        if op["bundle"] is not None:
            bundle = self.retained_bundle(job_root, op)
            accepted = bundle["request"]["proposal"]["accepted_package"]
            if op["maintenance"] is not None or (accepted["period"], accepted["rosterRoot"]) == (period, root):
                return bundle
            old_key = f"{accepted['period']}-{accepted['rosterRoot']}"
            old_ref = {"bundle": op["bundle"], "bundle_file": op.get("bundle_file", "bundle.json"),
                       "snapshot_directory": op.get("snapshot_directory", ".")}
            if chain["package_open"]:
                # A reorg can restore a previous opening. Resume its authenticated bundle,
                # never generate a replacement for a package the contract already froze.
                previous = op.get("opening_history", {}).get(f"{period}-{root}")
                s.require(previous is not None, "frozen_roster_mismatch")
                restored = self.retained_bundle(job_root, previous)
                saved = restored["request"]["proposal"]["accepted_package"]
                s.require((saved["period"], saved["rosterRoot"]) == (period, root), "frozen_roster_mismatch")
                if execute:
                    op["opening_history"][old_key] = old_ref
                    op.update(previous)
                    self.save()
                return restored
            rollover = True
            retained_manifest = bundle["request"]["manifest"]["payload"]
            prior_history = (bundle["audit"], bundle["request"]["duties"])
        if not execute:
            return None
        payload = io.private_json(job_root / "payload.json", io.MAX_PAYLOAD)
        evidence = io.private_json(job_root / "evidence.json")
        if op["bundle"] is None and not chain["package_open"]:
            for name in ("snapshot.json", "snapshot-intent.json"):
                if (directory / name).exists():
                    saved = io.private_json(directory / name)
                    snapshot = saved if name == "snapshot.json" else saved["snapshot"]
                    retained_manifest = snapshot["manifest"]
                    rollover = snapshot["manifest"]["period"] != period
                    prior_history = (snapshot["audit"], snapshot["duties"])
                    break
        if rollover:
            # An unopened proposal may outlive its roster. Preserve each signed prefix and
            # its immutable event archive while rebuilding this range from the dispatcher journals.
            directory = io.directory(job_root / f"opening-{period}-{root[2:]}-{io.digest(prior_history)}", create=True)
        snapshot_path = directory / "snapshot.json"
        intent_path = directory / "snapshot-intent.json"
        if snapshot_path.exists():
            snapshot = io.private_json(snapshot_path)
        elif intent_path.exists():
            intent = io.private_json(intent_path)
            s.exact(intent, ("schema_version", "payload_sha256", "evidence_sha256", "snapshot"))
            s.require(intent["schema_version"] == 1 and intent["payload_sha256"] == io.digest(payload)
                      and intent["evidence_sha256"] == io.digest(evidence), "frozen_snapshot_inputs_changed")
            snapshot = intent["snapshot"]
            with self.dispatchers_for(chain, payload["from_batch_number"], payload["to_batch_number"]) as (_, dispatches):
                self.export_context_events(dispatches, snapshot, payload, directory)
            io.immutable_json(snapshot_path, snapshot)
        else:
            with self.dispatchers_for(chain, payload["from_batch_number"], payload["to_batch_number"]) as (current, dispatches):
                contexts = [(item, self.context_snapshot(item, payload, evidence)) for item in dispatches]
                statements = s.validate_evidence(self.settings, evidence)
                receipts = [duty for _, context in contexts for duty in context["receipts"]]
                if rollover:
                    self.check_rollover_history(dispatches, receipts, retained_manifest, *prior_history)
                covered = {duty["batchNumber"] for duty in receipts}
                s.require(len(covered) == len(receipts), "duplicate_cross_period_accepted_batch")
                exhausted = all(account["offered"] >= self.settings["duties_per_round"]
                                for account in current.state["accounts"].values())
                current_closed = s.round_closed(self.settings, current.enrollment.rpc, current.state["period"])
                s.require(chain["control_work"] or exhausted or current_closed or all(statement["count"] == 0 or number in covered
                          for number, statement in statements.items()), "wait_for_nonempty_service_duties")
                if len(contexts) == 1:
                    dispatch, context = contexts[0]
                    snapshot = {"manifest": context["manifest"], "audit": context["audit"],
                                "subscriptions": context["subscriptions"], "duties": [] if chain["control_work"] or current_closed else
                                [duty for duty in context["receipts"] if duty["slot"] < self.settings["duties_per_round"]]}
                else:
                    group = s.EnrollmentGroup(self.settings, {item.state["journal_id"]: item.enrollment for item in dispatches},
                                              current.state["journal_id"], self.registry_rpc)
                    manifest_contexts, audit_contexts, duties = [], [], []
                    for dispatch, context in contexts:
                        journal_id = dispatch.state["journal_id"]
                        signed = {"payload": context["manifest"],
                                  "sequencer_signature": self.sign(s.manifest_request(self.settings, context["manifest"]), True)}
                        manifest_contexts.append({"journal_id": journal_id, "manifest": signed, "subscriptions": context["subscriptions"]})
                        reward = [] if chain["control_work"] or not group.rewardable(journal_id) else [
                            duty for duty in context["receipts"] if duty["slot"] < self.settings["duties_per_round"]]
                        duties.extend(reward)
                        audit_contexts.append({"journal_id": journal_id,
                            "request": {"manifest": signed, "subscriptions": context["subscriptions"], "duties": reward},
                            "bundle": context["audit"]})
                    subscriptions = s.aggregate_subscriptions(self.settings, manifest_contexts)
                    manifest = {"schema_version": 2, "chain_id": self.settings["execution_chain_id"],
                        "chain_address": self.settings["chain_address"], "sequencer": self.settings["sequencer"],
                        "period": current.state["period"], "current_journal_id": current.state["journal_id"],
                        "subscription_snapshot_hash": s.keccak(s.canonical(subscriptions)), "contexts": manifest_contexts}
                    snapshot = {"manifest": manifest, "subscriptions": subscriptions,
                        "duties": sorted(duties, key=lambda duty: duty["batchNumber"]),
                        "audit": {"schema_version": 3, "period": current.state["period"],
                                  "current_journal_id": current.state["journal_id"], "contexts": audit_contexts}}
                io.immutable_json(intent_path, {"schema_version": 1, "payload_sha256": io.digest(payload),
                                  "evidence_sha256": io.digest(evidence), "snapshot": snapshot})
                self.export_context_events(dispatches, snapshot, payload, directory)
            io.immutable_json(snapshot_path, snapshot)
        manifest_signature = self.sign(s.manifest_request(self.settings, snapshot["manifest"]), True)
        audit_signature = None if snapshot["audit"]["schema_version"] == 3 else self.sign(audit.checkpoint_request(snapshot["audit"]), True)
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
        if snapshot["audit"]["schema_version"] == 3:
            signed_audit = copy.deepcopy(snapshot["audit"])
            for context in signed_audit["contexts"]:
                context["request"]["proposal"] = request["proposal"]
                context["bundle"]["sequencer_signature"] = self.sign(audit.checkpoint_request(context["bundle"]), True)
        else:
            signed_audit = {**snapshot["audit"], "sequencer_signature": audit_signature}
        # Snapshot persistence can precede a restart beyond a receipt's reward grace.
        # Retain that immutable history and prepare only its current reward projection.
        bundle = self.refresh_report(identifier, {"request": request, "audit": signed_audit}, False)
        self.prepare(bundle["request"], evidence, payload)
        name = "bundle.json"
        if (job_root / name).exists() and io.private_json(job_root / name) != bundle:
            name = "bundle-" + io.digest(bundle) + ".json"
        io.immutable_json(job_root / name, bundle)
        if old_ref is not None:
            op.setdefault("opening_history", {})[old_key] = old_ref
        op["bundle"] = io.digest(bundle)
        if name != "bundle.json":
            op["bundle_file"] = name
        else:
            op.pop("bundle_file", None)
        if directory != job_root:
            op["snapshot_directory"] = directory.name
        self.save()
        return bundle

    def refresh_report(self, identifier, bundle, execute):
        authority = self.enrollment_for(bundle["request"])
        replacement = copy.deepcopy(bundle)
        duties = []
        if bundle["audit"]["schema_version"] == 3:
            for context in replacement["audit"]["contexts"]:
                if not authority.rewardable(context["journal_id"]):
                    context["request"]["duties"] = []
                duties.extend(context["request"]["duties"])
        else:
            closed = {period for period in {duty["period"] for duty in replacement["request"]["duties"]}
                      if s.round_closed(self.settings, authority.rpc, period)}
            duties = [duty for duty in replacement["request"]["duties"] if duty["period"] not in closed]
        replacement["request"]["duties"] = sorted(duties, key=lambda duty: duty["batchNumber"])
        if replacement == bundle or not execute:
            return replacement
        digest = io.digest(replacement)
        name = "bundle-" + digest + ".json"
        io.immutable_json(self.directory(identifier) / name, replacement)
        op = self.state["operations"][identifier]
        # Receipts and signed audit prefixes remain immutable; only expired reward
        # projection changes, through the contract's existing report repair path.
        op["bundle"], op["bundle_file"], op["report_repair"] = digest, name, True
        self.save()
        return replacement

    def export_context_events(self, dispatches, snapshot, payload, directory):
        if snapshot["audit"]["schema_version"] != 3:
            if snapshot["audit"]["schema_version"] == 2:
                self.export_snapshot_events(dispatches[0], snapshot, payload, directory)
            return
        if any(context["bundle"]["schema_version"] == 2 for context in snapshot["audit"]["contexts"]):
            io.directory(directory / "audit-events", create=True)
        by_id = {item.state["journal_id"]: item for item in dispatches}
        for context in snapshot["audit"]["contexts"]:
            bundle = context["bundle"]
            if bundle["schema_version"] == 2:
                dispatch = by_id.get(context["journal_id"])
                s.require(dispatch is not None, "frozen_snapshot_journal_missing")
                self.export_snapshot_events(dispatch, {"audit": bundle, "subscriptions": context["request"]["subscriptions"]},
                                            payload, directory / "audit-events" / context["journal_id"][2:], aggregate=True)

    def export_snapshot_events(self, dispatch, snapshot, payload, directory, *, aggregate=False):
        bundle, checkpoint = snapshot["audit"], snapshot["audit"]["checkpoint"]
        count = audit.event_count(bundle)
        ledger = dispatch.state["audit"]
        s.require(bundle["schema_version"] == 2 and bundle["journal_id"] == dispatch.state["journal_id"]
                  and bundle["identity"] == ledger["identity"]
                  and snapshot["subscriptions"] == dispatch.state["subscriptions"]
                  and checkpoint["eventCount"] == count <= len(ledger["events"])
                  and checkpoint["batchFrom"] == payload["from_batch_number"]
                  and checkpoint["batchTo"] == payload["to_batch_number"]
                  and checkpoint["chainId"] == self.settings["execution_chain_id"]
                  and checkpoint["chainAddress"] == self.settings["chain_address"]
                  and checkpoint["period"] == dispatch.state["period"], "frozen_snapshot_scope_changed")
        head = ledger["events"][count - 1]["hash"] if count else dispatch.state["journal_id"]
        s.require(head == checkpoint["eventHead"], "frozen_snapshot_audit_prefix_changed")
        descriptor = audit.export_events(ledger["events"], directory if aggregate else directory / "audit-events", count=count)
        s.require(descriptor == bundle["events"], "frozen_snapshot_archive_descriptor_changed")

    def maintenance(self, identifier, call, producer, execute):
        op = self.state["operations"][identifier]
        if not execute:
            return "would_" + call["action"]
        if op["maintenance"] is None and self.state.get("maintenance_receipt"):
            return {"next_action": "wait_for_existing_wallet_reservation"}
        manager = transactions.Transactions(self.settings, self.config["transaction_policy"], self.rpc,
            self.signing_wallet(), transactions.Store(self.store.root / "transactions"), self.clock)
        if call["action"] == "observeNativeSettlement":
            producer = self.native_observation_call
        if op["maintenance"] is None:
            pending = manager.plan(producer if call["action"] in transactions.REPEATABLE else call)
            if (pending["call"]["action"] != call["action"] or
                    call["action"] not in transactions.REPEATABLE and pending["call"]["transaction"] != call["transaction"]):
                return self.track_reserved_receipt(manager, pending["operation_id"], True)
            op["maintenance"] = pending
            self.save()
        operation_id = self.stage_maintenance(op["maintenance"], manager, True)
        def preflight(original, head, anchor):
            try:
                current = producer(head)
            except s.Error:
                return False
            return current["transaction"] == original["transaction"] and current["anchor"]["hash"] == head["hash"]
        result = manager.step(operation_id, True, preflight)
        if result["status"] in transactions.TERMINAL:
            op["maintenance"] = None
            self.save()
        return result

    def stage_maintenance(self, pending, manager, execute):
        s.exact(pending, ("operation_id", "call", "invocation"))
        expected = r.sha256(transactions.intent(self.settings, self.config["transaction_policy"],
                                               pending["call"], pending["invocation"]))
        s.require(pending["operation_id"] == expected, "maintenance_descriptor_changed")
        if execute:
            s.require(manager.stage(pending["call"], pending["invocation"]) == expected, "maintenance_descriptor_changed")
        return expected

    def track_reserved_receipt(self, manager, operation_id, execute):
        result = manager.step(operation_id, execute, receipt_only=True)
        if execute:
            entry = manager.state["operations"][operation_id]
            if entry["unsigned_transaction"] is not None and result["status"] not in transactions.TERMINAL:
                self.state["maintenance_receipt"] = operation_id
            self.save()
        return result

    def recover_maintenance(self, identifier, execute, bundle, evidence, payload, *, receipt_only=False):
        op = self.state["operations"][identifier]
        if op["maintenance"] is None:
            return None
        manager = transactions.Transactions(self.settings, self.config["transaction_policy"], self.rpc,
            self.signing_wallet() if execute and not receipt_only else None, transactions.Store(self.store.root / "transactions"), self.clock)
        operation_id = self.stage_maintenance(op["maintenance"], manager, execute)
        if operation_id not in manager.state["operations"]:
            return {"next_action": "would_stage_maintenance", "operation_id": operation_id}
        if not receipt_only and operation_id == self.state.get("maintenance_receipt"):
            if execute:
                op["maintenance"] = None
            self.track_reserved_receipt(manager, operation_id, execute)
            return None
        def preflight(original, head, anchor):
            if receipt_only:
                return False
            try:
                action = original["action"]
                if action in ("openPackage", "repairPackage"):
                    call = k.package_call(self.keeper, self.rpc, bundle["request"], evidence, payload, int(self.clock()),
                                          "open" if action == "openPackage" else "repair",
                                          enrollment=self.enrollment_for(bundle["request"]), at_head=head)
                elif action == "observeNativeSettlement":
                    call = self.native_observation_call(head)
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

    def record_wrapper_work(self, identifier, work_hash, source, operator):
        op = self.state["operations"][identifier]
        if work_hash in op["works"]:
            meta = op["works"][work_hash]
            s.require(meta["directory"] == str(source) and meta["operator"] == operator, "wrapper_work_destination_changed")
            return False
        if len(op["works"]) >= WRAPPER_WORK_PAGE_SIZE:
            page = op.get("work_pages", 0)
            # A crash before the journal save leaves this page unreferenced. Rebuild
            # it from the authoritative inline records, including any later results.
            r.atomic_json(self.directory(identifier) / f"wrapper-work-{page}.json", op["works"])
            op["works"] = {}
            op["work_pages"] = page + 1
        op["works"][work_hash] = {"directory": str(source), "operator": operator, "checked": False}
        return True

    def wrapper_work_items(self, identifier, page):
        op = self.state["operations"][identifier]
        for work_hash, meta in reversed(list(op["works"].items())):
            yield work_hash, meta, None
        if op.get("work_pages", 0):
            path = self.directory(identifier) / f"wrapper-work-{page}.json"
            history = io.private_json(path)
            for work_hash, meta in history.items():
                if work_hash not in op["works"]:
                    yield work_hash, meta, (path, history)

    def verify_wrapper_result(self, work_hash, meta, path, evidence, payload, selected):
        try:
            result = io.validate_result(self.settings, work_hash, io.private_json(path), meta["operator"])
            job.validate_payload(result["proof"], "SNARK", self.settings["vk_hash"], proof=True)
            proof_check.inputs(self.settings, evidence, payload, result["proof"])
        except (s.Error, job.Error, OSError, ValueError, TypeError, KeyError):
            return None, {"ignored": True, "error": "invalid_authenticated_result"}
        # An endorsement for different proof bytes cannot authorize the selected
        # native submission. Its verifier call could delay an already verified proof.
        if selected is not None and selected != result["proof"]:
            return None, {"ignored": True}
        try:
            proof_check.verify_snark(self.keeper, self.rpc, evidence, payload, result["proof"], int(self.clock()))
        except s.Error as error:
            if str(error) != "native_snark_rejected":
                raise
            return None, {"ignored": True, "error": str(error)}
        return result, {}

    def retained_proof(self, identifier, execute):
        op, directory = self.state["operations"][identifier], self.directory(identifier)
        selection = op["proof"]
        if selection is None:
            return None
        path = directory / "proof.json"
        if path.exists():
            proof = io.private_json(path)
        else:
            # Selection is durable before proof.json, so a restart can finish the
            # interrupted write even if the worker has since removed its response.
            work_hash = s.nonzero(selection["work_hash"])
            result = io.private_json(directory / (work_hash[2:] + ".result.json"))
            s.require(result["work_hash"] == work_hash and io.digest(result) == selection["result_sha256"],
                      "selected_wrapper_result_changed")
            proof = result["proof"]
        s.require(io.digest(proof) == selection["proof_sha256"], "retained_proof_changed")
        if execute and not path.exists():
            io.immutable_json(path, proof)
        return proof

    def result(self, identifier, execute):
        op, directory = self.state["operations"][identifier], self.directory(identifier)
        evidence, payload = io.private_json(directory / "evidence.json"), io.private_json(directory / "payload.json", io.MAX_PAYLOAD)
        proof_path = directory / "proof.json"
        selected = self.retained_proof(identifier, execute)
        page = op.get("work_cursor", 0)
        for work_hash, meta, archive in self.wrapper_work_items(identifier, page):
            path = Path(meta["directory"]) / "result.json"
            if meta["checked"]:
                continue
            if selected is not None and work_hash == op["proof"]["work_hash"]:
                # An archived page is saved after the selection journal. Its checked flag
                # may still need replay even after the worker has archived its own result.
                if execute and archive is not None:
                    meta["checked"] = True
                    r.atomic_json(*archive)
                continue
            if not path.exists():
                continue
            result, disposition = self.verify_wrapper_result(work_hash, meta, path, evidence, payload, selected)
            if not disposition:
                if not execute:
                    return "would_retain_verified_wrapper_result"
                io.immutable_json(directory / (work_hash[2:] + ".result.json"), result)
                if op["proof"] is None:
                    selected = result["proof"]
                    op["proof"] = {"work_hash": work_hash, "result_sha256": io.digest(result),
                                   "proof_sha256": io.digest(selected)}
            if execute:
                meta.update(checked=True, **disposition)
                self.save()
                if not disposition and not proof_path.exists():
                    io.immutable_json(proof_path, selected)
                if archive is not None:
                    r.atomic_json(*archive)
        if execute and op.get("work_pages", 0) > 1:
            # Keep polling bounded while revisiting every old inbox for late proofs.
            op["work_cursor"] = (page + 1) % op["work_pages"]
            self.save()
        return None

    def relay(self, identifier, execute, *, receipt_only=False):
        op = self.state["operations"][identifier]
        if op["relay"] is None:
            return None
        store = r.Store(self.store.root / "relay")
        wallet = (self.relay_wallet or io.wallet_connection(self.config["relay_wallet_file"],
                   self.keeper["policy"]["rpc_timeout_seconds"])) if execute and not receipt_only else None
        with store.lock() if execute else nullcontext():
            relay = r.Relay(self.settings, self.config["relay_policy"], self.rpc, wallet, store, self.clock)
            entry = relay.state["operations"][op["relay"]]
            if entry["status"] == "stale_unsigned" and entry["unsigned_transaction"] is None and entry["raw_transaction"] is None:
                if execute:
                    op["relay"] = None
                    self.save()
                return None
            if (not receipt_only and entry["unsigned_transaction"] is None and entry["raw_transaction"] is None
                    and entry["status"] not in r.TERMINAL):
                bundle = io.private_json(self.directory(identifier) / op.get("bundle_file", "bundle.json"))
                s.require(io.digest(bundle) == op["bundle"], "frozen_bundle_changed")
                preview = relay.step(op["relay"], False)
                if (entry["acceptance"] is None and entry["receipt"] is None
                        and preview["context"]["reason"] != "range_already_proven"
                        and self.refresh_report(identifier, bundle, False) != bundle):
                    if not execute:
                        return {"next_action": "would_retire_expired_unsigned_report", "operation_id": op["relay"]}
                    entry["status"] = "stale_unsigned"
                    relay.save(True)
                    op["relay"] = None
                    self.save()
                    return None
            result = relay.step(op["relay"], execute, receipt_only=receipt_only)
            entry = relay.state["operations"][op["relay"]]
            pending_nonce = relay.external_acceptance_with_pending_nonce(entry)
            if result["status"] in ("accepted", "accepted_elsewhere") or pending_nonce:
                accepted_tx = result["acceptance"].get("transaction_hash")
                if not accepted_tx:
                    return {**result, "next_action": "wait_for_acceptance_transaction"}
                s.require(op["publication"] is not None, "native_publication_missing")
                try:
                    handoff = relay.export_handoff(op["relay"], op["publication"], execute)
                except FileNotFoundError as error:
                    if not receipt_only or error.filename != op["publication"]:
                        raise
                    # A delivered hint is bound to this frozen relay and its acceptance transaction.
                    # An archived first or replacement handoff still needs its original publication.
                    if op.get("handed_off_tx") != accepted_tx:
                        if execute and op.get("handoff_restore_required") != accepted_tx:
                            op["handoff_restore_required"] = accepted_tx
                            self.save()
                        return {**result, "next_action": "restore_publication_for_handoff"}
                    handoff = result
                status = "settled_with_pending_nonce" if pending_nonce else "settled"
                if execute and (op["status"] != status or self.state["active"] == identifier
                                or op.get("handed_off_tx") != accepted_tx or op.get("handoff_restore_required")):
                    op["handed_off_tx"] = accepted_tx
                    op.pop("handoff_restore_required", None)
                    op["status"] = status
                    if pending_nonce and identifier not in self.state.setdefault("regrouped_pending", []):
                        self.state["regrouped_pending"].append(identifier)
                    if self.state["active"] == identifier:
                        self.state["active"] = None
                    self.save()
                return handoff
            if result["status"] in ("stale_unsigned", "reverted") and (entry["unsigned_transaction"] is None
                    or result["status"] == "reverted"):
                if execute:
                    op["relay"] = None
                    op.pop("handoff_restore_required", None)
                    self.save()
                return None
            return result

    def retire_regrouped(self, identifier, payload, execute):
        op = self.state["operations"][identifier]
        directory = self.directory(identifier)
        evidence_file = directory / "evidence.json"
        evidence = io.private_json(evidence_file) if evidence_file.exists() else None
        proof = self.retained_proof(identifier, execute)
        s.require(proof is None or evidence is not None, "retained_proof_changed")
        source = native_handoff.regrouped_work(self.config["publication_dir"], op["publication"], self.settings,
            self.config["relay_policy"], self.rpc, int(self.clock()), payload, evidence, proof)
        if source is None:
            return None
        if execute:
            op["publication"] = str(source)
            self.detach_native_work(identifier, "regrouped")
        return {"next_action": "pick_regrouped_native_range" if execute else "would_retire_regrouped_native_range"}

    def detach_native_work(self, identifier, status):
        op = self.state["operations"][identifier]
        op["status"], self.state["active"] = status, None
        if op["relay"] or op["maintenance"]:
            # Releasing local proof ownership does not release either wallet's nonce.
            self.state.setdefault("regrouped_pending", []).append(identifier)
        self.save()

    def retire_superseded(self, identifier, start, end, head, execute):
        op, directory = self.state["operations"][identifier], self.directory(identifier)
        evidence = io.private_json(directory / "evidence.json")
        proof = self.retained_proof(identifier, execute)
        disposition = native_handoff.superseded_work(self.config["publication_dir"], op["publication"],
            self.settings, self.config["relay_policy"], self.rpc, head, start, end, evidence, proof)
        if disposition is None:
            return None
        verified, status = disposition
        if execute:
            self.detach_native_work(identifier, status)
        action = "range_already_proven" if verified >= end else "native_advance_requires_new_range"
        return {"next_action": action if execute else "would_retire_superseded_native_range"}

    def reconcile_regrouped(self, execute):
        for identifier in list(self.state.get("regrouped_pending", [])):
            op = self.state["operations"][identifier]
            self.recover_maintenance(identifier, execute, None, None, None, receipt_only=True)
            if op["status"] != "settled":
                self.relay(identifier, execute, receipt_only=True)
            if execute and not op["maintenance"] and (not op["relay"] or op["status"] == "settled"):
                self.state["regrouped_pending"].remove(identifier)
                self.save()

    def native_observation_call(self, head):
        return k.maintenance(self.keeper, self.rpc, "observe-native", {}, int(self.clock()), at_head=head)

    def observe_native(self, execute):
        pending = self.state.get("native_observation")
        if not pending and self.state.get("maintenance_receipt"):
            return {"next_action": "wait_for_existing_wallet_reservation"}
        if not pending and not execute:
            return {"next_action": "would_observe_native_settlement"}
        manager = transactions.Transactions(self.settings, self.config["transaction_policy"], self.rpc,
            self.signing_wallet() if execute else None, transactions.Store(self.store.root / "transactions"), self.clock)
        if not pending:
            pending = manager.plan(self.native_observation_call)
            if pending["call"]["action"] == "observeNativeSettlement":
                self.state["native_observation"] = pending
                self.save()
        operation_id = self.stage_maintenance(pending, manager, execute)
        if operation_id not in manager.state["operations"]:
            return {"next_action": "would_stage_maintenance", "operation_id": operation_id}
        entry = manager.state["operations"][operation_id]
        if entry["call"]["action"] != "observeNativeSettlement":
            # The maintenance wallet can already own a nonce for another package.
            return self.track_reserved_receipt(manager, operation_id, execute)

        def preflight(original, head, anchor):
            try:
                current = self.native_observation_call(head)
            except s.Error:
                return False
            return current["transaction"] == original["transaction"] and current["anchor"]["hash"] == head["hash"]

        result = manager.step(operation_id, execute, preflight)
        entry = manager.state["operations"][operation_id]
        if execute and (result["status"] in transactions.TERMINAL
                        or result["status"] == "stale_unsigned" and entry["unsigned_transaction"] is None):
            self.state.pop("native_observation", None)
            self.save()
        elif (result["status"] in ("authorization_stale_reserved", "nonce_conflict")
              and native_handoff.confirmed_observation(self.rpc, self.settings, self.config["transaction_policy"],
                                                       entry["invocation"]["scope"], int(self.clock()))):
            if execute:
                # Move ownership in one journal write. The maintenance wallet still owns the nonce.
                self.state.pop("native_observation")
                self.state["maintenance_receipt"] = operation_id
                self.save()
            return {**result, "next_action": "resume_after_confirmed_native_observation" if execute
                    else "would_resume_after_confirmed_native_observation"}
        return result

    def step(self, execute=False):
        s.require(type(execute) is bool, "invalid_execute_flag")
        reserved = self.state.get("maintenance_receipt")
        if reserved:
            manager = transactions.Transactions(self.settings, self.config["transaction_policy"], self.rpc,
                store=transactions.Store(self.store.root / "transactions"), clock=self.clock)
            receipt = manager.step(reserved, execute, receipt_only=True)
            if execute and receipt["status"] in transactions.TERMINAL:
                self.state.pop("maintenance_receipt")
                self.save()
        self.reconcile_regrouped(execute)
        identifier = self.state["active"]
        if self.state.get("native_observation"):
            return self.observe_native(execute)
        if identifier is not None:
            op = self.state["operations"][identifier]
            if op["publication"] and (Path(op["publication"]) / "regrouped.json").exists():
                payload = io.private_json(self.directory(identifier) / "payload.json", io.MAX_PAYLOAD)
                retired = self.retire_regrouped(identifier, payload, execute)
                if retired:
                    return retired
            result = self.relay(identifier, execute)
            if result is not None:
                context = result.get("context")
                acceptance = result.get("acceptance") or {}
                if (context and context["reason"] in ("range_already_proven", "accepted_parent_changed", "package_already_accepted")
                        and not acceptance.get("transaction_hash")):
                    head = context["head"]
                    anchor = {"blockHash": head["hash"], "requireCanonical": True}
                    if (context["reason"] == "range_already_proven" or
                            r.call_word(self.rpc, self.settings["chain_address"], "getTotalBatchesVerified()", anchor) >= context["batch_from"]):
                        if transactions.observation_scope(self.rpc, self.settings, head) is not None:
                            return self.observe_native(execute)
                        retired = self.retire_superseded(identifier, context["batch_from"], context["batch_to"], head, execute)
                        if retired:
                            return retired
                return result
        chain, roster = self.status()
        if chain["observed_native_batch"] + 1 < chain["next_native_batch"]:
            return self.observe_native(execute)
        s.require(chain["mode"] == "service" and chain["service_active"], "service_activation_required")
        if identifier is None:
            if not execute:
                return {"next_action": "would_pick_native_snark", "chain": chain}
            identifier = self.reserve()
        op, directory = self.state["operations"][identifier], self.directory(identifier)
        payload_path = directory / "payload.json"
        if payload_path.exists():
            # A restart may have persisted the pick just before its old evidence became unavailable.
            payload = io.private_json(payload_path, io.MAX_PAYLOAD)
            if chain["next_native_batch"] < payload["from_batch_number"] and op["publication"] is None:
                retired = self.retire_regrouped(identifier, payload, execute)
                if retired:
                    return retired
                # A node restart can replace an in-memory lease before a wrapper exists
                # to journal. Only an expired pick with no submission or endorsements can
                # be released on this evidence alone. Keep its original authority on disk.
                if op["lease"] and not any(op[k] for k in ("bundle", "proof", "works", "maintenance", "relay")):
                    authority = read_private_json(directory / op["lease"] / "authority.json")
                    if authority["status"] == "picked" and authority["submission_sha256"] is None:
                        deadline = next(item["deadline"] for item in op["leases"] if item["name"] == op["lease"])
                        if int(self.clock()) < deadline:
                            return {"next_action": "await_native_frontier_for_picked_lease"}
                        if execute:
                            op["status"], self.state["active"] = "expired", None
                            self.save()
                        return {"next_action": "pick_after_expired_future_lease" if execute else "would_release_expired_future_lease"}
            if chain["next_native_batch"] > payload["from_batch_number"]:
                if op["maintenance"] or op["relay"] or op["publication"]:
                    head, _ = r.anchored_head(self.rpc, self.settings, self.config["relay_policy"], int(self.clock()))
                    retired = self.retire_superseded(identifier, payload["from_batch_number"], payload["to_batch_number"],
                                                     head, execute)
                    return retired or {"next_action": "await_confirmed_native_disposition"}
                if execute:
                    op["status"], self.state["active"] = "superseded", None
                    self.save()
                return {"next_action": "range_already_proven" if chain["next_native_batch"] > payload["to_batch_number"]
                        else "native_advance_requires_new_range"}
        if op["lease"] is None or op["status"] == "picking":
            result = self.acquire(identifier, execute, next_native_batch=chain["next_native_batch"])
            if result:
                return {"next_action": result}
        payload = io.private_json(payload_path, io.MAX_PAYLOAD)
        evidence = io.private_json(directory / "evidence.json")
        s.require(chain["next_native_batch"] == payload["from_batch_number"], "native_frontier_does_not_match_retained_lease")
        if op["maintenance"] is not None:
            retained = self.retained_bundle(directory, op)
            pending = self.recover_maintenance(identifier, execute, retained, evidence, payload)
            if pending is not None:
                return pending
        bundle = self.snapshot(identifier, chain, roster, execute)
        if bundle is None:
            return {"next_action": "would_sign_dispatch_snapshot"}
        bundle = self.refresh_report(identifier, bundle, execute)
        if chain["package_open"] and (bundle["audit"]["schema_version"] == 3 or op.get("report_repair")
                                      or io.digest(bundle) != op["bundle"]):
            projected = self.prepare(bundle["request"], evidence, payload)["accepted_package"]
            frozen = k.normalized(projected)
            if frozen != chain["frozen_package"]:
                expected = dict(chain["frozen_package"])
                expected["reportHash"] = frozen["reportHash"]
                s.require(frozen == expected, "report_repair_changed_frozen_work")
                produce = lambda head=None: k.package_call(self.keeper, self.rpc, bundle["request"], evidence, payload,
                                 int(self.clock()), "repair", enrollment=self.enrollment_for(bundle["request"]), at_head=head)
                return self.maintenance(identifier, produce(), produce, execute)
        if not chain["package_open"]:
            produce = lambda head=None: k.package_call(self.keeper, self.rpc, bundle["request"], evidence, payload,
                                 int(self.clock()), "open", enrollment=self.enrollment_for(bundle["request"]), at_head=head)
            return self.maintenance(identifier, produce(), produce, execute)
        request = k.rebind_request(self.settings, bundle["request"], chain, roster)
        self.prepare(request, evidence, payload)
        operator = request["proposal"]["candidate"]["operator"]
        work_page = op.get("work_cursor", 0)
        result = self.result(identifier, execute)
        if result:
            return {"next_action": result}
        proof = self.retained_proof(identifier, execute)
        if proof is not None:
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
            expected = None
            for work_hash, meta, _ in self.wrapper_work_items(identifier, work_page):
                if not meta["checked"] or meta.get("ignored") or meta["operator"] != operator:
                    continue
                result = io.private_json(directory / (work_hash[2:] + ".result.json"))
                if result["prepared"] is None:
                    continue
                if expected is None:
                    expected = s.prepare_package(self.settings, evidence, request["manifest"], request["subscriptions"],
                                  request["duties"], request["proposal"], proof, payload,
                                  enrollment=self.enrollment_for(request))
                if result["prepared"] != expected:
                    continue
                try:
                    s.verify_eoa(expected["wrapper_request"], result["wrapper_signature"])
                except s.Error:
                    # A signed result envelope can carry an unusable package endorsement.
                    # Keep both the selected proof and the candidate for the next poll
                    # because a local signature verifier failure can be transient.
                    continue
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
        if self.record_wrapper_work(identifier, work_hash, Path(inbox) / work_hash[2:], operator):
            self.save()
        io.publish_work(inbox, {"body": body, "sequencer_signature": signature}, payload,
                        event_root=directory / op.get("snapshot_directory", ".") / "audit-events")
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
