#!/usr/bin/env python3
"""Schedule bounded child/Gateway FRI and SNARK rentals from a trusted host."""

import argparse
from decimal import Decimal
import os
from pathlib import Path
import re
import sys
import time
import uuid

import job
import sentry
from runpod import (Controller, Error, Runpod, Store, TERMINAL, atomic_json, exact_fields,
                    money, positive_int, read_private_json, require, sync_dir, validate_policy)


ORDER = (("child", "FRI"), ("gateway", "FRI"), ("child", "SNARK"), ("gateway", "SNARK"))
DONE = {"no_job", "complete", "rejected", "lease_expired", "returned"}
MAX_EVIDENCE = 1024 * 1024


def service_keeper():
    directory = str(Path(__file__).resolve().parent.parent / "prover-service")
    if directory not in sys.path:
        sys.path.insert(0, directory)
    import keeper
    return keeper


def validate_identity(identity):
    exact_fields(identity, ("chain_id", "chain_address", "settlement_chain_id", "protocol_version", "vk_hash"))
    job.validate_chain_binding({**identity, "lane": "child", "evidence_sha256": "1" * 64,
                                "payload_sha256": "2" * 64, "origin_endpoint_sha256": "3" * 64})


def validate_limits(limits):
    exact_fields(limits, ("max_inflight_jobs", "lifetime_budget_usd"))
    require(positive_int(limits["max_inflight_jobs"]) <= 32, "too_many_pool_jobs")
    money(limits["lifetime_budget_usd"])


def load_config(config):
    exact_fields(config, ("schema_version", "limits", "lanes"))
    require(config["schema_version"] == 1, "unsupported_pool_schema")
    validate_limits(config["limits"])
    exact_fields(config["lanes"], ("child", "gateway"))
    frozen, inputs = {}, {}
    for name in ("child", "gateway"):
        lane = config["lanes"][name]
        exact_fields(lane, ("endpoint", "auth_file", "identity", "limits", "native_lease_seconds", "stages"))
        validate_identity(lane["identity"])
        validate_limits(lane["limits"])
        require(60 <= positive_int(lane["native_lease_seconds"]) <= 86400, "invalid_native_lease_duration")
        endpoint = sentry.endpoint_url(lane["endpoint"])
        exact_fields(lane["stages"], ("FRI", "SNARK"))
        needs_native = any(entry["acquisition"] == "native" for entry in lane["stages"].values())
        if needs_native:
            require(isinstance(lane["auth_file"], str), "native_lane_requires_auth_file")
            secret = job.read_file(lane["auth_file"], 4096, private=True)
            sentry.authorization(lane["auth_file"])
        else:
            require(lane["auth_file"] is None, "external_lane_must_not_copy_native_credentials")
            secret = None
        stages = {}
        inputs[name] = {"auth": secret, "releases": {}}
        for stage in ("FRI", "SNARK"):
            entry = lane["stages"][stage]
            fields = ("release_file", "rental_policy_file", "acquisition")
            if "service" in entry or stage == "SNARK" and entry.get("acquisition") == "external":
                fields += ("service",)
            exact_fields(entry, fields)
            require(entry["acquisition"] in ("native", "external"), "invalid_pool_acquisition")
            service = entry.get("service")
            require(not (stage == "SNARK" and entry["acquisition"] == "external") or service is not None,
                    "external_snark_requires_service_configuration")
            if service is not None:
                require(stage == "SNARK" and entry["acquisition"] == "external", "service_requires_external_snark")
                exact_fields(service, ("keeper_config_file",))
                keeper = service_keeper()
                try:
                    service = keeper.configuration(read_private_json(service["keeper_config_file"]))
                except keeper.s.Error as error:
                    raise Error(str(error)) from None
                require(service["lane"] == name and all(service["settings"][key] == lane["identity"][other]
                    for key, other in (("execution_chain_id", "chain_id"), ("chain_address", "chain_address"),
                                       ("settlement_chain_id", "settlement_chain_id"), ("vk_hash", "vk_hash"))),
                    "service_lane_identity_mismatch")
            raw = job.read_file(entry["release_file"], job.MAX_MANIFEST)
            release = job.release_identity(raw)
            require(release["stage"] == stage and release["vk_hash"] == lane["identity"]["vk_hash"],
                    "lane_release_identity_mismatch")
            policy = validate_policy(read_private_json(entry["rental_policy_file"]))
            require(policy["limits"]["max_runtime_seconds"] < lane["native_lease_seconds"],
                    "rental_runtime_must_fit_native_lease")
            reserve = money(policy["limits"]["max_operation_usd"])
            require(reserve <= money(lane["limits"]["lifetime_budget_usd"])
                    and reserve <= money(config["limits"]["lifetime_budget_usd"]), "pool_budget_too_small")
            stages[stage] = {"release_sha256": job.hash_bytes(raw), "rental_policy": policy,
                             "acquisition": entry["acquisition"], "service": service}
            inputs[name]["releases"][stage] = raw
        frozen[name] = {"endpoint": endpoint, "auth_sha256": job.hash_bytes(secret) if secret is not None else None,
                        "identity": lane["identity"], "limits": lane["limits"],
                        "native_lease_seconds": lane["native_lease_seconds"], "stages": stages}
    require(frozen["child"]["endpoint"] != frozen["gateway"]["endpoint"], "lanes_require_distinct_endpoints")
    require(frozen["child"]["identity"]["chain_id"] != frozen["gateway"]["identity"]["chain_id"],
            "lanes_require_distinct_identities")
    return {"schema_version": 1, "limits": config["limits"], "lanes": frozen}, inputs


def initialize(root, config):
    settings, inputs = load_config(config)
    root = sentry.private_directory(root, create=True)
    for name, lane in settings["lanes"].items():
        directory = sentry.private_directory(root / name, create=True)
        sentry.private_directory(directory / "jobs", create=True)
        if inputs[name]["auth"] is not None:
            job.write_new(directory / "auth.txt", inputs[name]["auth"])
        for stage, entry in lane["stages"].items():
            directory_stage = sentry.private_directory(directory / stage, create=True)
            job.write_new(directory_stage / "release.json", inputs[name]["releases"][stage])
            Store.initialize(directory_stage / "controller", entry["rental_policy"])
    atomic_json(root / "pool.json", {"schema_version": 1, "pool_id": uuid.uuid4().hex,
                                     "settings": settings, "cursor": 0, "operations": {}})
    return Store(root)


class Pool:
    # The pool lock is always acquired before a lane/controller lock. Watchdogs only
    # take their controller lock, so recovery cannot invert this lock order.
    def __init__(self, store, native=None, storage=None, api=None, clock=time.time, controller_http=None, service_rpc=None):
        self.store, self.native, self.storage = store, native or job.NativeNetwork(), storage or job.Network()
        self.api, self.clock, self.controller_http = api, clock, controller_http
        self.service_rpc = service_rpc
        self.state = read_private_json(store.root / "pool.json")
        exact_fields(self.state, ("schema_version", "pool_id", "settings", "cursor", "operations"))
        require(self.state["schema_version"] == 1 and re.fullmatch(r"[0-9a-f]{32}", self.state["pool_id"]),
                "invalid_pool_state")
        self.settings = self.state["settings"]

    def save(self):
        atomic_json(self.store.root / "pool.json", self.state)

    def operation(self, operation):
        require(operation in self.state["operations"], "unknown_pool_operation")
        return self.state["operations"][operation]

    def directory(self, operation):
        op = self.operation(operation)
        return self.store.root / op["lane"] / "jobs" / operation

    def controller_store(self, op):
        return Store(self.store.root / op["lane"] / op["stage"] / "controller")

    def auth(self, name):
        require(self.settings["lanes"][name]["auth_sha256"] is not None, "external_lane_has_no_native_credentials")
        path = self.store.root / name / "auth.txt"
        require(job.hash_bytes(job.read_file(path, 4096, private=True))
                == self.settings["lanes"][name]["auth_sha256"], "lane_credentials_changed")
        return sentry.authorization(path)

    def eligible(self, name, stage):
        ops = list(self.state["operations"].values())
        lane = self.settings["lanes"][name]
        selected = [op for op in ops if op["lane"] == name]
        policy = lane["stages"][stage]["rental_policy"]
        reserve = money(policy["limits"]["max_operation_usd"])
        for entries, limits in ((ops, self.settings["limits"]), (selected, lane["limits"])):
            if sum(op["status"] not in DONE for op in entries) >= limits["max_inflight_jobs"]:
                return False
            if sum((money(op["reserved_usd"], allow_zero=True) for op in entries), Decimal(0)) + reserve > money(limits["lifetime_budget_usd"]):
                return False
        # An unresolved native pick can already own work even without a response body.
        if any(op["stage"] == stage and op["status"] in ("pick_uncertain", "awaiting_evidence") for op in selected):
            return False
        stage_ops = [op for op in selected if op["stage"] == stage]
        limits = policy["limits"]
        if sum(op["status"] not in DONE for op in stage_ops) >= limits["max_concurrent_pods"]:
            return False
        return sum((money(op["reserved_usd"], allow_zero=True) for op in stage_ops), Decimal(0)) + reserve <= money(limits["lifetime_budget_usd"])

    def pick_next(self):
        require(len(self.state["operations"]) < 1000, "pool_operation_count_limit")
        for offset in range(len(ORDER)):
            index = (self.state["cursor"] + offset) % len(ORDER)
            name, stage = ORDER[index]
            if self.settings["lanes"][name]["stages"][stage]["acquisition"] != "native" or not self.eligible(name, stage):
                continue
            lane = self.settings["lanes"][name]
            raw = job.read_file(self.store.root / name / stage / "release.json", job.MAX_MANIFEST, private=True)
            require(job.hash_bytes(raw) == lane["stages"][stage]["release_sha256"], "lane_release_changed")
            auth = self.auth(name)
            operation = uuid.uuid4().hex
            self.state["operations"][operation] = {"lane": name, "stage": stage, "mode": "native", "status": "pick_uncertain",
                "picked_at": self.clock(), "job_id": self.state["pool_id"] + ":" + name + ":" + stage + ":" + operation,
                "deadline": self.clock() + lane["native_lease_seconds"],
                "expiry_not_before": None,
                "lease_sha256": None,
                "reserved_usd": str(lane["stages"][stage]["rental_policy"]["limits"]["max_operation_usd"]),
                "rental_operation": None, "chain_binding": None, "native_status": None}
            self.state["cursor"] = (index + 1) % len(ORDER)
            self.save()
            sentry.pick(self.directory(operation), lane["endpoint"], raw,
                        self.operation(operation)["job_id"], auth, self.native)
            self.recover_pick(operation)
            return operation
        raise Error("no_pool_capacity_or_budget")

    def recover_pick(self, operation):
        op = self.operation(operation)
        require(op["mode"] == "native", "external_job_has_no_native_pick")
        require(op["status"] in ("pick_uncertain", "awaiting_evidence", "ready"), "pool_pick_not_recoverable")
        directory = self.directory(operation)
        authority = read_private_json(directory / "authority.json")
        lane = self.settings["lanes"][op["lane"]]
        require(authority["endpoint"] == lane["endpoint"] and authority["job_id"] == op["job_id"]
                and authority["stage"] == op["stage"] and authority["release_sha256"]
                == lane["stages"][op["stage"]]["release_sha256"], "native_origin_changed")
        if authority["status"] == "no_job":
            op.update(status="no_job", reserved_usd="0", native_status="no_job")
            self.save()
            return
        if authority["status"] == "pick_uncertain":
            sentry.recover_pick(directory)
            authority = read_private_json(directory / "authority.json")
        require(authority["status"] == "picked", "invalid_native_pick_state")
        lease_hash = job.hash_bytes(job.b256(authority["lease_token"]).encode())
        require(op["lease_sha256"] in (None, lease_hash), "native_lease_changed")
        op["lease_sha256"] = lease_hash
        op["status"] = "awaiting_evidence"
        if op["expiry_not_before"] is None:
            op["expiry_not_before"] = self.clock() + lane["native_lease_seconds"]
        self.save()
        payload_raw = job.read_file(directory / "payload.json", job.MAX_PICK[op["stage"]], private=True)
        payload = job.validate_payload(job.decode(payload_raw), op["stage"], lane["identity"]["vk_hash"])
        start = payload.get("batch_number", payload.get("from_batch_number"))
        end = payload.get("batch_number", payload.get("to_batch_number"))
        bounds = str(start) if op["stage"] == "FRI" else f"{start}/{end}"
        status, _, raw = self.native.request(lane["endpoint"] + f"prover-jobs/v1/{op['stage']}/{bounds}/evidence",
                                             authorization=self.auth(op["lane"]), maximum=MAX_EVIDENCE)
        require(status == 200, "lane_evidence_unavailable_preserve_pick")
        evidence = job.decode(raw)
        binding, evidence_raw = self.bind_evidence(op["lane"], payload_raw, evidence, start, end)
        require(op["chain_binding"] in (None, binding) and authority.get("chain_binding") in (None, binding),
                "frozen_lane_evidence_changed")
        if (directory / "evidence.json").exists():
            require(job.read_file(directory / "evidence.json", MAX_EVIDENCE, private=True) == evidence_raw,
                    "frozen_lane_evidence_changed")
        else:
            job.write_new(directory / "evidence.json", evidence_raw)
        authority["chain_binding"] = binding
        atomic_json(directory / "authority.json", authority)
        op.update(status="ready", chain_binding=binding)
        self.save()

    def bind_evidence(self, name, payload_raw, evidence, start, end):
        lane = self.settings["lanes"][name]
        exact_fields(evidence, ("schema_version", "chain_id", "chain_address", "settlement_chain_id",
                                "protocol_version", "vk_hash", "previous_batch", "batches"))
        require(evidence["schema_version"] == 1 and all(evidence[key] == value for key, value in lane["identity"].items()),
                "lane_evidence_identity_mismatch")
        require(type(start) is int and start > 0 and isinstance(evidence["batches"], list)
                and [entry["stored"]["batchNumber"] for entry in evidence["batches"]] == list(range(start, end + 1))
                and evidence["previous_batch"]["batchNumber"] == start - 1, "lane_evidence_range_mismatch")
        evidence_raw = job.encode(evidence)
        binding = {**lane["identity"], "lane": name, "evidence_sha256": job.hash_bytes(evidence_raw),
                   "payload_sha256": job.hash_bytes(payload_raw), "origin_endpoint_sha256": job.hash_bytes(lane["endpoint"].encode())}
        job.validate_chain_binding(binding)
        return binding, evidence_raw

    def enqueue(self, name, stage, job_id, payload_raw, evidence, deadline, compute_permit=None):
        require(name in ("child", "gateway") and stage in ("FRI", "SNARK"), "invalid_pool_lane_stage")
        lane = self.settings["lanes"][name]
        require(lane["stages"][stage]["acquisition"] == "external", "lane_requires_native_acquisition")
        service = lane["stages"][stage].get("service")
        require((service is None) == (compute_permit is None), "service_compute_permit_required")
        permit_hash = job.hash_bytes(job.encode(compute_permit)) if compute_permit is not None else None
        require(isinstance(job_id, str) and re.fullmatch(r"[A-Za-z0-9_.:-]{1,128}", job_id), "invalid_job_id")
        payload = job.validate_payload(job.decode(payload_raw), stage, lane["identity"]["vk_hash"])
        payload_raw = job.encode(payload)
        binding, evidence_raw = self.bind_evidence(name, payload_raw, evidence,
            payload.get("batch_number", payload.get("from_batch_number")),
            payload.get("batch_number", payload.get("to_batch_number")))
        require(type(deadline) is int and self.clock() < deadline <= self.clock() + 86400, "invalid_external_deadline")
        operation = None
        for existing, op in self.state["operations"].items():
            if op["job_id"] == job_id:
                require(op["mode"] == "external" and op["lane"] == name and op["stage"] == stage
                        and op["chain_binding"] == binding and op["deadline"] == deadline
                        and op.get("compute_permit_sha256") == permit_hash, "external_job_identity_changed")
                operation = existing
                break
        if operation is None:
            require(len(self.state["operations"]) < 1000 and self.eligible(name, stage), "no_pool_capacity_or_budget")
            operation = uuid.uuid4().hex
            self.state["operations"][operation] = {"lane": name, "stage": stage, "mode": "external", "status": "import_pending",
                "picked_at": self.clock(), "deadline": deadline, "job_id": job_id, "chain_binding": binding,
                "reserved_usd": str(lane["stages"][stage]["rental_policy"]["limits"]["max_operation_usd"]),
                "rental_operation": None, "native_status": "not_owned"}
            self.state["operations"][operation]["compute_permit_sha256"] = permit_hash
            self.save()
        op = self.operation(operation)
        directory = self.directory(operation)
        if not directory.exists():
            sentry.private_directory(directory, create=True)
        for filename, raw in (("payload.json", payload_raw), ("evidence.json", evidence_raw)):
            if (directory / filename).exists():
                require(job.read_file(directory / filename, max(job.MAX_PICK.values()), private=True) == raw,
                        "external_import_file_changed")
            else:
                job.write_new(directory / filename, raw)
        if compute_permit is not None:
            path, raw = directory / "compute-permit.json", job.encode(compute_permit)
            if path.exists():
                require(job.read_file(path, 16 * 1024 * 1024, private=True) == raw, "compute_permit_changed")
            else:
                job.write_new(path, raw)
        if op["status"] == "import_pending":
            op["status"] = "ready"
        self.save()
        return operation

    def bound_authority(self, operation):
        op = self.operation(operation)
        lane = self.settings["lanes"][op["lane"]]
        require(op["chain_binding"] is not None, "chain_binding_missing")
        authority = None
        if op["mode"] == "native":
            authority = read_private_json(self.directory(operation) / "authority.json")
            require(authority.get("chain_binding") == op["chain_binding"]
                    and authority["endpoint"] == lane["endpoint"] and authority["job_id"] == op["job_id"]
                    and authority["stage"] == op["stage"] and authority["vk_hash"] == lane["identity"]["vk_hash"]
                    and authority["release_sha256"] == lane["stages"][op["stage"]]["release_sha256"], "native_origin_changed")
            require(job.hash_bytes(job.b256(authority["lease_token"]).encode()) == op["lease_sha256"], "native_lease_changed")
        else:
            require(not (self.directory(operation) / "authority.json").exists(), "external_job_must_not_own_lease")
        for filename, digest, maximum in (("payload.json", op["chain_binding"]["payload_sha256"], job.MAX_PICK[op["stage"]]),
                                         ("evidence.json", op["chain_binding"]["evidence_sha256"], MAX_EVIDENCE)):
            require(job.hash_bytes(job.read_file(self.directory(operation) / filename, maximum, private=True)) == digest,
                    "chain_bound_file_changed")
        if authority is not None:
            payload = job.decode(job.read_file(self.directory(operation) / "payload.json", job.MAX_PICK[op["stage"]], private=True))
            require(authority["bounds"] == {key: value for key, value in payload.items()
                    if key in ("batch_number", "from_batch_number", "to_batch_number")}, "native_bounds_changed")
        return authority

    def export(self, operation, plan):
        op = self.operation(operation)
        require(op["status"] in ("ready", "exported"), "pool_job_not_exportable")
        self.bound_authority(operation)
        if op["mode"] == "native":
            sentry.export(self.directory(operation), plan, self.storage)
        else:
            raw = job.read_file(self.store.root / op["lane"] / op["stage"] / "release.json", job.MAX_MANIFEST, private=True)
            require(job.hash_bytes(raw) == self.settings["lanes"][op["lane"]]["stages"][op["stage"]]["release_sha256"],
                    "lane_release_changed")
            sentry.export_input(self.directory(operation), job.read_file(self.directory(operation) / "payload.json",
                job.MAX_PICK[op["stage"]], private=True), raw, op["job_id"], plan, self.storage, op["chain_binding"])
        op["status"] = "exported"
        self.save()

    def launch(self, operation):
        op = self.operation(operation)
        require(op["status"] in ("exported", "rented"), "pool_job_not_launchable")
        self.bound_authority(operation)
        lane = self.settings["lanes"][op["lane"]]
        policy = lane["stages"][op["stage"]]["rental_policy"]
        selected = read_private_json(self.directory(operation) / "controller-job.json")
        controller_store = self.controller_store(op)
        with controller_store.lock():
            controller = Controller(controller_store, self.api, clock=self.clock, http=self.controller_http)
            require(controller.policy == policy, "lane_rental_policy_changed")
            existing = any(entry["job"]["job_id"] == op["job_id"] for entry in controller.state["operations"].values())
            require(existing or self.clock() + policy["limits"]["max_runtime_seconds"] < op["deadline"],
                    "insufficient_native_lease_remaining")
            service = lane["stages"][op["stage"]].get("service")
            latest_create_at = None
            if service is not None and not existing:
                require(op["mode"] == "external" and op["stage"] == "SNARK", "service_requires_external_snark")
                keeper = service_keeper()
                raw = job.read_file(self.directory(operation) / "compute-permit.json", 16 * 1024 * 1024, private=True)
                require(job.hash_bytes(raw) == op.get("compute_permit_sha256"), "compute_permit_changed")
                try:
                    checked = keeper.validate_permit(service, self.service_rpc or keeper.rpc_for(service), job.decode(raw),
                        job.decode(job.read_file(self.directory(operation) / "evidence.json", MAX_EVIDENCE, private=True)),
                        job.decode(job.read_file(self.directory(operation) / "payload.json", job.MAX_PICK["SNARK"], private=True)),
                        self.clock(), policy["limits"]["max_runtime_seconds"])
                except keeper.s.Error as error:
                    raise Error(str(error)) from None
                require(op["deadline"] <= checked["state"]["deadline"], "external_deadline_exceeds_compute_window")
                require(self.clock() + policy["limits"]["max_runtime_seconds"] + service["policy"]["reserve_seconds"]
                        < min(op["deadline"], checked["state"]["deadline"]), "compute_window_elapsed_during_validation")
                latest_create_at = min(op["deadline"], checked["state"]["deadline"]) - policy["limits"]["max_runtime_seconds"] \
                    - service["policy"]["reserve_seconds"]
            rental = controller.launch(selected, latest_create_at=latest_create_at)
        require(op["rental_operation"] in (None, rental), "rental_operation_changed")
        op.update(status="rented", rental_operation=rental)
        self.save()
        return rental

    def complete(self, operation):
        op = self.operation(operation)
        require(op["status"] in ("rented", "submission_pending", "complete", "rejected", "returned"), "pool_job_not_completable")
        self.bound_authority(operation)
        controller_store = self.controller_store(op)
        with controller_store.lock():
            controller = Controller(controller_store, self.api, clock=self.clock, http=self.controller_http)
            require(controller.collect(op["rental_operation"]), "rental_result_not_ready")
            controller.terminate(op["rental_operation"])
            stopped = controller.operation(op["rental_operation"])["status"] in TERMINAL
        if op["mode"] == "external":
            sentry.verify_input_result(self.directory(operation), controller_store, op["rental_operation"],
                                       self.directory(operation) / "returned-proof.json")
            if stopped:
                op["status"] = "returned"
            self.save()
            return op["status"]
        op["status"] = "submission_pending"
        self.save()
        native_status = sentry.submit(self.directory(operation), controller_store, op["rental_operation"],
                                      self.auth(op["lane"]), self.native)
        op["native_status"] = native_status
        if stopped:
            op["status"] = "complete" if native_status == "accepted" else "rejected"
        self.save()
        return op["status"]

    def expire(self, operation):
        op = self.operation(operation)
        require(op["status"] not in DONE, "pool_job_already_terminal")
        require(op["status"] not in ("pick_uncertain", "awaiting_evidence"),
                "uncertain_native_pick_requires_origin_reconciliation")
        require(self.clock() >= (op["expiry_not_before"] if op["mode"] == "native" else op["deadline"]),
                "native_lease_not_expired")
        controller_store = self.controller_store(op)
        with controller_store.lock():
            controller = Controller(controller_store, self.api, clock=self.clock)
            matches = [entry for entry in controller.state["operations"].values() if entry["job"]["job_id"] == op["job_id"]]
            require(all(entry["status"] in TERMINAL for entry in matches), "rental_must_be_reconciled_and_stopped")
        require(op["status"] != "submission_pending", "native_submission_requires_reconciliation")
        op["status"] = "lease_expired"
        self.save()

    def report(self):
        return {operation: {key: op[key] for key in ("lane", "stage", "mode", "status", "reserved_usd", "rental_operation", "native_status")}
                for operation, op in self.state["operations"].items()}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--state-dir", required=True)
    parser.add_argument("--execute", action="store_true")
    parser.add_argument("--api-key-file")
    commands = parser.add_subparsers(dest="command", required=True)
    setup = commands.add_parser("init")
    setup.add_argument("--config", required=True)
    for name in ("pick-next", "status"):
        commands.add_parser(name)
    enqueue = commands.add_parser("enqueue")
    enqueue.add_argument("--lane", required=True, choices=("child", "gateway"))
    enqueue.add_argument("--stage", required=True, choices=("FRI", "SNARK"))
    enqueue.add_argument("--job-id", required=True)
    enqueue.add_argument("--payload", required=True)
    enqueue.add_argument("--evidence", required=True)
    enqueue.add_argument("--deadline", required=True, type=int)
    enqueue.add_argument("--compute-permit")
    for name in ("recover-pick", "export", "launch", "complete", "expire"):
        command = commands.add_parser(name)
        command.add_argument("--operation-id", required=True)
        if name == "export":
            command.add_argument("--storage-plan", required=True)
    args = parser.parse_args()
    if args.command == "init":
        config = read_private_json(args.config)
        load_config(config)
        if args.execute:
            initialize(args.state_dir, config)
        print(job.encode({"action": "init", "execute": args.execute}).decode())
        return
    store = Store(args.state_dir)
    if args.command != "status" and not args.execute:
        print("dry-run: no native picks, rentals, transfers, or submissions")
        return
    api = None
    if args.command in ("launch", "complete"):
        key = job.read_file(args.api_key_file, 4096, private=True).decode().strip() if args.api_key_file else os.environ.get("RUNPOD_API_KEY", "")
        api = Runpod(key)
    with store.lock("pool.lock"):
        pool = Pool(store, api=api)
        if args.command == "pick-next":
            pool.pick_next()
        elif args.command == "enqueue":
            pool.enqueue(args.lane, args.stage, args.job_id, job.read_file(args.payload, job.MAX_PICK[args.stage]),
                         job.decode(job.read_file(args.evidence, MAX_EVIDENCE)), args.deadline,
                         job.decode(job.read_file(args.compute_permit, 16 * 1024 * 1024)) if args.compute_permit else None)
        elif args.command == "export":
            pool.export(args.operation_id, read_private_json(args.storage_plan))
        elif args.command != "status":
            getattr(pool, args.command.replace("-", "_"))(args.operation_id)
        print(job.encode(pool.report()).decode())


if __name__ == "__main__":
    os.umask(0o077)
    try:
        main()
    except (Error, OSError, ValueError, TypeError, KeyError) as error:
        print(str(error) if isinstance(error, Error) else "pool_operation_failed", file=sys.stderr)
        sys.exit(1)
