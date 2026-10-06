#!/usr/bin/env python3
"""Run the selected SNARK turn on the same trusted controller that handles FRI work."""

import argparse
from contextlib import nullcontext
import copy
import hashlib
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time

import audit
import keeper as k
import proof_check
import service as s
import workflow_io as io

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "prover-rental"))
import job
import pool
import sentry
from runpod import Store, atomic_json, read_private_json


def configuration(value):
    s.exact(value, ("schema_version", "keeper", "pool_dir", "inbox_dir", "roster_dir", "audit_trust_dir",
                    "wallet_file", "registry_rpc_file", "fri_verifier", "poll_interval_seconds"))
    s.require(value["schema_version"] == 1, "unsupported_wrapper_configuration")
    k.configuration(value["keeper"])
    s.require(value["keeper"]["settings"]["coordinator"] != s.ZERO_ADDRESS, "wrapper_requires_service_coordinator")
    for name in ("pool_dir", "inbox_dir", "roster_dir", "audit_trust_dir"):
        io.directory(value[name])
    for name in ("wallet_file", "registry_rpc_file"):
        s.require(Path(value[name]).is_absolute(), "absolute_connection_file_required")
        io.wallet_connection(value[name], value["keeper"]["policy"]["rpc_timeout_seconds"])
    verifier = value["fri_verifier"]
    s.exact(verifier, ("executable", "sha256", "timeout_seconds"))
    s.require(Path(verifier["executable"]).is_absolute() and isinstance(verifier["sha256"], str)
              and len(verifier["sha256"]) == 64
              and all(c in "0123456789abcdef" for c in verifier["sha256"]), "invalid_fri_verifier_pin")
    s.require(0 < s.uint(verifier["timeout_seconds"], 32) <= 1800, "invalid_fri_verification_timeout")
    s.require(0 < s.uint(value["poll_interval_seconds"], 32) <= 60, "invalid_wrapper_poll_interval")
    return value


def initialize(root, config):
    config = configuration(config)
    root = io.directory(root, create=True)
    s.require(not any(root.iterdir()), "wrapper_state_must_be_empty")
    for name in ("jobs", "signatures", "fri-checks"):
        io.directory(root / name, create=True)
    io.immutable_json(root / "configuration.json", config)
    atomic_json(root / "wrapper.json", {"schema_version": 1, "configuration_sha256": io.digest(config),
                                        "checkpoints": {}, "jobs": {}, "last_status": None})
    return Store(root)


def verify_fris(root, executable_policy, release, settings, evidence, payload):
    executable = Path(executable_policy["executable"])
    s.require(executable.is_file() and not executable.is_symlink(), "fri_verifier_binary_required")
    binary = hashlib.sha256()
    with executable.open("rb") as source:
        while part := source.read(1024 * 1024):
            binary.update(part)
    s.require(binary.hexdigest() == executable_policy["sha256"], "fri_verifier_binary_changed")
    statements = s.validate_evidence(settings, evidence)
    payload_raw = s.canonical(payload)
    expected = {"schema_version": 1, "protocol_version": 32, "proving_version": 8, "security_level": 100,
                "from_batch_number": payload["from_batch_number"], "to_batch_number": payload["to_batch_number"],
                "vk_hash": settings["vk_hash"], "program_commitment": release["program_commitment"],
                "statements": [entry["statement"] for entry in statements.values()],
                "payload_sha256": hashlib.sha256(payload_raw).hexdigest()}
    key = io.digest({"expected": expected, "binary_sha256": executable_policy["sha256"]})
    directory = io.directory(root / key, create=True)
    io.immutable_bytes(directory / "payload.json", payload_raw, io.MAX_PAYLOAD)
    io.immutable_json(directory / "expected.json", expected)
    output = directory / "verified.json"
    expected_result = {"schema_version": 1, "verification": "native_v32_fri_payload",
                       "payload_sha256": expected["payload_sha256"],
                       "expected_sha256": hashlib.sha256(s.canonical(expected)).hexdigest(),
                       "vk_hash": expected["vk_hash"], "program_commitment": expected["program_commitment"],
                       "from_batch_number": expected["from_batch_number"], "to_batch_number": expected["to_batch_number"],
                       "proof_count": len(statements)}
    if not output.exists():
        # The executable is locally pinned; no command, option, or filesystem path comes
        # from the sequencer's handoff. Verification runs without a GPU or an EN.
        with tempfile.TemporaryDirectory(prefix=".verify-", dir=directory) as temporary, tempfile.TemporaryFile() as errors:
            pending = Path(temporary) / "verified.json"
            try:
                completed = subprocess.run([str(executable), "verify-fri", "--payload", str(directory / "payload.json"),
                    "--expected", str(directory / "expected.json"), "--output", str(pending)],
                    stdin=subprocess.DEVNULL, stdout=errors, stderr=errors,
                    timeout=executable_policy["timeout_seconds"], check=False)
            except (OSError, subprocess.TimeoutExpired):
                raise s.Error("fri_verification_unavailable_or_timed_out") from None
            s.require(completed.returncode == 0, "native_fri_verification_failed")
            result = io.private_json(pending, 64 * 1024)
            s.require(result == expected_result, "fri_verification_record_changed")
            io.immutable_json(output, result)
    result = io.private_json(output, 64 * 1024)
    s.require(result == expected_result, "fri_verification_record_changed")
    return result


class Wrapper:
    def __init__(self, store, rpc=None, registry_rpc=None, wallet=None, clock=time.time,
                 fri_checker=verify_fris, snark_checker=proof_check.verify_snark):
        self.store, self.clock = store, clock
        self.config = configuration(io.private_json(store.root / "configuration.json"))
        self.state = io.private_json(store.root / "wrapper.json")
        s.require(self.state["schema_version"] == 1
                  and self.state["configuration_sha256"] == io.digest(self.config), "wrapper_configuration_changed")
        self.keeper = self.config["keeper"]
        self.settings = self.keeper["settings"]
        self.rpc = rpc or k.rpc_for(self.keeper)
        self.registry_rpc = registry_rpc or io.wallet_connection(self.config["registry_rpc_file"],
                                                                 self.keeper["policy"]["rpc_timeout_seconds"])
        self.enrollment_authority = None
        self.wallet = wallet
        self.fri_checker, self.snark_checker = fri_checker, snark_checker

    def save(self):
        atomic_json(self.store.root / "wrapper.json", self.state)

    def status(self):
        state = k.status(self.keeper, self.rpc, self.clock())
        if not state["package_open"] or state["mode"] != "service":
            return state, None
        roster = io.private_json(Path(self.config["roster_dir"]) / (str(state["roster"]["period"]) + ".json"))
        return k.status(self.keeper, self.rpc, self.clock(), roster), roster

    def audited(self, body, chain_state, execute, payload):
        bundle = body["audit"]
        s.require(type(bundle) is dict, "signed_dispatch_audit_required")
        journal_id = s.nonzero(bundle["journal_id"])
        trust = io.private_json(Path(self.config["audit_trust_dir"]) / (journal_id[2:] + ".json"))
        previous = self.state["checkpoints"].get(journal_id)
        if previous:
            count = trust["minimum_checkpoint"]["event_count"]
            if count == previous["event_count"]:
                s.require(trust["minimum_checkpoint"] == previous, "independent_checkpoint_conflict")
            elif count < previous["event_count"]:
                trust = {**trust, "minimum_checkpoint": previous}
        request = body["request"]
        result = audit.verify_package(self.settings, body["evidence"], request["manifest"], request["subscriptions"],
                    request["duties"], bundle, trust, self.registry_rpc, fri_payload=payload,
                    allow_control=chain_state["control_work"])
        if execute:
            self.state["checkpoints"][journal_id] = {key: result[key] for key in
                                                    ("event_count", "event_head", "assignment_cursor")}
            self.save()
        return result

    def pool(self):
        return pool.Pool(Store(Path(self.config["pool_dir"])), clock=self.clock, service_rpc=self.rpc)

    def enrollment(self, request):
        self.enrollment_authority = k.enrollment_for(self.keeper, request, self.registry_rpc,
                                                    enrollment=self.enrollment_authority)
        return self.enrollment_authority

    def pool_policy(self, p, body):
        lane = p.settings["lanes"][self.keeper["lane"]]
        stage = lane["stages"]["SNARK"]
        s.require(stage["acquisition"] == "external" and stage["service"] == self.keeper,
                  "wrapper_pool_service_configuration_changed")
        s.require(body["lane"] == self.keeper["lane"]
                  and stage["release_sha256"] == body["release_sha256"], "wrapper_release_or_lane_changed")
        raw = job.read_file(p.store.root / self.keeper["lane"] / "SNARK" / "release.json", job.MAX_MANIFEST, private=True)
        s.require(job.hash_bytes(raw) == stage["release_sha256"], "wrapper_release_file_changed")
        release = job.release_identity(raw)
        return stage["rental_policy"]["limits"]["max_runtime_seconds"], release

    def receive(self, source, state, roster, execute):
        envelope = io.private_json(source / "work.json")
        payload = io.private_json(source / "payload.json", io.MAX_PAYLOAD)
        work_hash = io.validate_work(self.settings, envelope, payload)
        s.require(source.name == work_hash[2:], "work_directory_hash_mismatch")
        body = envelope["body"]
        request = k.rebind_request(self.settings, body["request"], state, roster)
        s.require(request == body["request"] and state["local_operator_selected"], "work_not_for_current_local_turn")
        enrollment = self.enrollment(request)
        prepared = k.prepare(self.settings, request, body["evidence"], payload, enrollment=enrollment)
        self.audited(body, state, execute, payload)
        p = self.pool()
        runtime, release = self.pool_policy(p, body)
        runtime = 1 if body["proof"] is not None else runtime
        k.inspect(self.keeper, self.rpc, prepared, body["evidence"], self.clock(), runtime)
        if not execute:
            return {"action": "would_verify_and_handle_selected_turn", "work_hash": work_hash}
        s.require(len(self.state["jobs"]) < 1024 or work_hash in self.state["jobs"], "wrapper_history_capacity")
        directory = io.directory(self.store.root / "jobs" / work_hash[2:], create=True)
        io.immutable_json(directory / "work.json", envelope)
        io.immutable_json(directory / "payload.json", payload, io.MAX_PAYLOAD)
        self.state["jobs"].setdefault(work_hash, {"status": "received", "pool_operation": None,
                                                "source": str(source), "last_error": None})
        self.save()
        self.fri_checker(self.store.root / "fri-checks", self.config["fri_verifier"], release,
                         self.settings, body["evidence"], payload)
        if body["proof"] is not None:
            return self.finish(work_hash, body["proof"], execute)
        permit_path = directory / "permit.json"
        if permit_path.exists():
            permit = io.private_json(permit_path)
            k.validate_permit(self.keeper, self.rpc, permit, body["evidence"], payload, self.clock(), runtime,
                              enrollment=enrollment)
        else:
            permit = k.permit(self.keeper, self.rpc, request, body["evidence"], payload, self.clock(), runtime,
                              enrollment=enrollment)
            io.immutable_json(permit_path, permit)
        deadline = min(body["native_lease_deadline"], permit["state"]["deadline"])
        s.require(self.clock() + runtime + self.keeper["policy"]["reserve_seconds"] < deadline,
                  "native_lease_too_short_for_compute")
        # The pool's own durable job ID makes a crash between enqueue and save idempotent.
        with p.store.lock("pool.lock"):
            p = self.pool()
            operation = p.enqueue(body["lane"], "SNARK", "wrapper-" + work_hash[2:], s.canonical(payload),
                                  body["evidence"], int(deadline), compute_permit=permit)
        self.state["jobs"][work_hash].update(status="computing", pool_operation=operation)
        self.save()
        return {"action": "queued_selected_snark", "work_hash": work_hash, "pool_operation": operation}

    def finish(self, work_hash, proof, execute):
        entry = self.state["jobs"][work_hash]
        directory = self.store.root / "jobs" / work_hash[2:]
        envelope, payload = io.private_json(directory / "work.json"), io.private_json(directory / "payload.json", io.MAX_PAYLOAD)
        body = envelope["body"]
        io.validate_work(self.settings, envelope, payload)
        if not execute:
            return {"action": "would_verify_and_return_proof", "work_hash": work_hash}
        io.immutable_json(directory / "proof.json", proof)
        current, roster = self.status()
        if current["next_native_batch"] > payload["to_batch_number"]:
            entry.update(status="obsolete", last_error=None)
            self.save()
            return {"action": "range_already_proved", "work_hash": work_hash}
        checked = self.snark_checker(self.keeper, self.rpc, body["evidence"], payload, proof, self.clock())
        atomic_json(directory / "proof-check.json", checked)
        current, roster = self.status()
        prepared, signature = None, "0x"
        enrollment = self.enrollment(body["request"])
        original = k.prepare(self.settings, body["request"], body["evidence"], payload, enrollment=enrollment)
        unchanged = current["frozen_package"] == k.normalized(original["accepted_package"])
        if unchanged and current["mode"] == "service" and current["package_open"] and current["local_operator_selected"]:
            rebound = k.rebind_request(self.settings, body["request"], current, roster)
            if rebound == body["request"]:
                self.audited(body, current, execute, payload)
                request = body["request"]
                prepared = s.prepare_package(self.settings, body["evidence"], request["manifest"], request["subscriptions"],
                                             request["duties"], request["proposal"], proof, payload, enrollment=enrollment)
                k.inspect(self.keeper, self.rpc, k.prepare(self.settings, request, body["evidence"], payload,
                                                        enrollment=enrollment),
                          body["evidence"], self.clock(), 1)
                signature = io.typed_signature(self.store.root / "signatures", prepared["wrapper_request"],
                                                self.signing_wallet(), execute)
        result = {"schema_version": 1, "work_hash": work_hash, "proof": proof,
                  "prepared": prepared, "wrapper_signature": signature}
        result_signature = io.typed_signature(self.store.root / "signatures",
            io.result_request(self.settings, work_hash, result, self.keeper["policy"]["expected_operator"]),
            self.signing_wallet(), execute)
        answer = {"result": result, "operator_signature": result_signature}
        io.immutable_json(directory / "result.json", answer)
        io.immutable_json(Path(entry["source"]) / "result.json", answer)
        entry.update(status="returned", last_error=None)
        self.save()
        return {"action": "returned_endorsement" if prepared else "returned_proof_after_turn_change", "work_hash": work_hash}

    def signing_wallet(self):
        return self.wallet or io.wallet_connection(self.config["wallet_file"], self.keeper["policy"]["rpc_timeout_seconds"])

    def failure(self, work_hash, error, execute):
        if execute:
            reason = str(error) if isinstance(error, (s.Error, job.Error)) else "invalid_wrapper_handoff"
            self.state["last_status"] = {"work_hash": work_hash, "error": reason}
            entry = self.state["jobs"].get(work_hash)
            if entry:
                entry["last_error"] = reason
                if reason == "native_snark_rejected":
                    entry["status"] = "rejected"
            self.save()

    def recover(self, execute):
        for work_hash, entry in self.state["jobs"].items():
            if entry["status"] in ("returned", "expired", "obsolete", "rejected"):
                continue
            try:
                result = self.recover_job(work_hash, entry, execute)
                if result is not None:
                    return result
            except (s.Error, job.Error, OSError, ValueError, TypeError, KeyError) as error:
                self.failure(work_hash, error, execute)
        return None

    def recover_job(self, work_hash, entry, execute):
        directory = self.store.root / "jobs" / work_hash[2:]
        result = directory / "result.json"
        if result.exists():
            cached = io.private_json(result)
            io.validate_result(self.settings, work_hash, cached, self.keeper["policy"]["expected_operator"])
            if execute:
                io.immutable_json(Path(entry["source"]) / "result.json", cached)
                entry["status"] = "returned"
                self.save()
            return {"action": "recovered_cached_result", "work_hash": work_hash}
        proof_file = directory / "proof.json"
        if proof_file.exists():
            return self.finish(work_hash, io.private_json(proof_file), execute)
        operation = entry["pool_operation"]
        if operation is None:
            return None
        p = self.pool()
        if not execute:
            return ({"action": "would_collect_returned_proof", "work_hash": work_hash}
                    if p.operation(operation)["status"] == "returned" else None)
        with p.store.lock("pool.lock"):
            p = self.pool()
            op = p.operation(operation)
            if op["status"] == "returned":
                controller = Store(Path(op["warm_controller_dir"])) if op.get("warm_owner") else p.controller_store(op)
                result_file = p.directory(operation) / "returned-proof.json"
                sentry.verify_input_result(p.directory(operation), controller, op["rental_operation"], result_file)
                proof = io.private_json(result_file, job.MAX_SUBMIT)
            elif op["status"] in ("expired", "lease_expired", "authorization_expired", "rejected", "failed"):
                if execute:
                    entry["status"] = "expired"
                    self.save()
                return None
            else:
                return None
        return self.finish(work_hash, proof, execute)

    def step(self, execute=False):
        recovered = self.recover(execute)
        if recovered:
            return recovered
        state, roster = self.status()
        if state["mode"] != "service" or not state["package_open"] or not state["local_operator_selected"]:
            return {"action": "continue_fri_until_selected", "current_turn": state.get("current_turn"),
                    "selected_wrapper_index": state.get("selected_wrapper_index")}
        entries = sorted(io.directory(self.config["inbox_dir"]).iterdir())
        s.require(len(entries) <= 1024, "wrapper_inbox_capacity")
        for directory in entries:
            if not directory.is_dir() or directory.is_symlink() or not (directory / "work.json").exists():
                continue
            work_hash = "0x" + directory.name
            try:
                s.nonzero(work_hash)
                entry = self.state["jobs"].get(work_hash)
                if entry and entry["status"] in ("returned", "expired", "obsolete", "rejected", "computing"):
                    continue
                envelope = io.private_json(directory / "work.json")
                proposal = envelope["body"]["request"]["proposal"]
                accepted = proposal["accepted_package"]
                if accepted["turn"] != state["current_turn"] or accepted["wrapper"] != self.keeper["policy"]["expected_operator"]:
                    continue
                return self.receive(directory, state, roster, execute)
            except (s.Error, job.Error, OSError, ValueError, TypeError, KeyError) as error:
                self.failure(work_hash, error, execute)
        return {"action": "selected_waiting_for_authenticated_handoff", "current_turn": state["current_turn"]}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--state-dir", required=True)
    parser.add_argument("--execute", action="store_true")
    commands = parser.add_subparsers(dest="command", required=True)
    commands.add_parser("init").add_argument("--config", required=True)
    commands.add_parser("status")
    commands.add_parser("run").add_argument("--once", action="store_true")
    args = parser.parse_args()
    if args.command == "init":
        config = configuration(io.private_json(args.config))
        if args.execute:
            initialize(args.state_dir, config)
        print("wrapper_initialized" if args.execute else "dry-run: configuration valid")
        return
    store = Store(io.directory(args.state_dir))
    stopped = False
    def stop(*_):
        nonlocal stopped
        stopped = True
    signal.signal(signal.SIGTERM, stop)
    signal.signal(signal.SIGINT, stop)
    while not stopped:
        with store.lock("wrapper.lock") if args.execute else nullcontext():
            wrapper = Wrapper(store)
            if args.command == "status":
                state, _ = wrapper.status()
                print(s.canonical(state).decode())
                return
            try:
                print(s.canonical(wrapper.step(args.execute)).decode(), flush=True)
            except (s.Error, job.Error, OSError, ValueError, TypeError, KeyError) as error:
                print(s.canonical({"action": "waiting", "error": str(error) if isinstance(error, (s.Error, job.Error))
                                   else "wrapper_step_failed"}).decode(), flush=True)
        if args.once:
            return
        time.sleep(wrapper.config["poll_interval_seconds"])


if __name__ == "__main__":
    os.umask(0o077)
    try:
        main()
    except (s.Error, job.Error, OSError, ValueError, TypeError, KeyError) as error:
        print(str(error) if isinstance(error, (s.Error, job.Error)) else "wrapper_failed", file=sys.stderr)
        raise SystemExit(1)
