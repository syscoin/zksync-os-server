import copy
import base64
import hashlib
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

import keeper as k
import native_handoff
import proof_check
import relay as r
import roster
import service as s
import workflow_io as io
import wrapper as w
from test_keeper import NativeRpc, setup
from test_dispatcher import Rpc as RegistryRpc
from test_service import fixture, sign, a, h


REAL_AUDIT_VERIFY = w.audit.verify_package


class Chain(NativeRpc):
    def __init__(self, f, config, item):
        super().__init__(f, config, item)
        self.valid_proof, self.verifications = True, []
        self.expected_calldata = proof_check.verifier_calldata(list(s.validate_evidence(f["settings"], f["evidence"])
                                        [number]["statement"] for number in (1, 2)), s.decode_proof(f["snark"]["proof"]))

    def call(self, method, params):
        if method == "eth_call":
            tx, anchor = params
            data = s.raw_hex(tx["data"])
            if tx["to"] == a(130) and data[:4] == r.selector(proof_check.VERIFY_SIGNATURE):
                self.verifications.append(copy.deepcopy(params))
                assert anchor == {"blockHash": self.head["hash"], "requireCanonical": True}
                return "0x" + s.word(int(self.valid_proof and tx["data"] == self.expected_calldata)).hex()
            if data == r.selector("nextBatch()"):
                return "0x" + s.word(self.native_proved + 1).hex()
        return super().call(method, params)


class TypedWallet:
    def __init__(self, root):
        self.root, self.calls, self.role = root, [], "wrapper"

    def call(self, method, params):
        self.calls.append((method, copy.deepcopy(params)))
        assert method == "eth_signTypedData_v4"
        typed = json.loads(params[1])
        primary, domain = typed["primaryType"], typed["domain"]
        request = s.typed_request(primary, [(field["name"], field["type"]) for field in typed["types"][primary]],
                                 typed["message"], domain["name"], domain["chainId"], domain["verifyingContract"], params[0])
        assert io.private_json(self.root / (io.digest(request) + ".request.json")) == request
        return sign(request, self.role)


class Pool:
    def __init__(self, root, keeper, release_raw):
        self.store = w.Store(root)
        self.settings = {"lanes": {"child": {"stages": {"SNARK": {"acquisition": "external", "service": keeper,
            "release_sha256": w.job.hash_bytes(release_raw), "rental_policy": {"limits": {"max_runtime_seconds": 50}}}}}}}
        self.operations, self.enqueues, self.interrupt = {}, [], False
        io.directory(root / "jobs", create=True)
        io.directory(root / "warm-controller", create=True)

    def enqueue(self, lane, stage, job_id, payload, evidence, deadline, compute_permit=None):
        identity = (lane, stage, job_id, payload, evidence, deadline, compute_permit)
        self.enqueues.append(copy.deepcopy(identity))
        if job_id in self.operations:
            assert self.operations[job_id]["identity"] == identity
        else:
            io.directory(self.store.root / "jobs" / job_id, create=True)
            self.operations[job_id] = {"identity": copy.deepcopy(identity), "status": "ready", "warm_owner": "warm-session",
                "warm_controller_dir": str(self.store.root / "warm-controller"), "rental_operation": "rental-1"}
        if self.interrupt:
            self.interrupt = False
            raise KeyboardInterrupt
        return job_id

    def operation(self, identifier):
        return self.operations[identifier]

    def directory(self, identifier):
        return self.store.root / "jobs" / identifier


class WrapperTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.base = fixture()

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        self.f = copy.deepcopy(self.base)
        self.roster = roster.construct(5, [self.f["proposal"]["candidate"]])
        self.f["proposal"]["accepted_package"]["rosterRoot"] = self.roster["root"]
        self.f["proposal"]["candidate_proof"] = self.roster["proofs"][0]
        self.keeper, self.request, item = setup(self.f)
        self.rpc = Chain(self.f, self.keeper, item)
        self.registry_rpc = RegistryRpc(self.f["settings"], self.f["subscriptions"])
        for name in ("pool", "inbox", "rosters", "trust"):
            io.directory(self.root / name, create=True)
        for name in ("wallet", "registry-rpc"):
            s.write_new(self.root / (name + ".json"), {"url": "http://127.0.0.1:8545", "authorization": None})
        self.verifier = self.root / "verifier"
        self.verifier.write_bytes(b"pinned-test-verifier")
        self.verifier.chmod(0o700)
        self.release = {"schema_version": 1, "stage": "SNARK", "protocol_version": 32, "execution_version": 7,
                        "proving_version": 8, "security_level": 100, "vk_hash": self.f["settings"]["vk_hash"],
                        "program_commitment": h(200), "app_bin_sha256": "1" * 64, "app_text_sha256": "2" * 64,
                        "worker_sha256": "3" * 64, "crs_sha256": "4" * 64}
        self.release_raw = w.job.encode(self.release)
        release_directory = io.directory(self.root / "pool" / "child", create=True)
        release_directory = io.directory(release_directory / "SNARK", create=True)
        io.immutable_bytes(release_directory / "release.json", self.release_raw)
        self.p = Pool(self.root / "pool", self.keeper, self.release_raw)
        self.config = {"schema_version": 1, "keeper": self.keeper, "pool_dir": str(self.root / "pool"),
            "inbox_dir": str(self.root / "inbox"), "roster_dir": str(self.root / "rosters"),
            "audit_trust_dir": str(self.root / "trust"), "wallet_file": str(self.root / "wallet.json"),
            "registry_rpc_file": str(self.root / "registry-rpc.json"), "fri_verifier": {"executable": str(self.verifier),
                "sha256": hashlib.sha256(self.verifier.read_bytes()).hexdigest(), "timeout_seconds": 5},
            "poll_interval_seconds": 5}
        self.store = w.initialize(self.root / "wrapper", self.config)
        self.wallet = TypedWallet(self.store.root / "signatures")
        io.immutable_json(self.root / "rosters" / "5.json", self.roster)
        self.trust = {"minimum_checkpoint": {"event_count": 0, "event_head": h(180), "assignment_cursor": h(180)}}
        io.immutable_json(self.root / "trust" / (h(180)[2:] + ".json"), self.trust)
        self.fri_calls, self.audit_calls, self.result_checks = [], [], []
        for target, value in (("wrapper.audit.verify_package", self.audit), ("wrapper.Wrapper.pool", lambda _: self.p),
                               ("wrapper.sentry.verify_input_result", self.result_check)):
            patched = patch(target, value)
            patched.start()
            self.addCleanup(patched.stop)
        self.work_hash, self.source = self.publish()

    def audit(self, settings, evidence, manifest, subscriptions, duties, bundle, trust, rpc, *, fri_payload, allow_control=False):
        self.assertEqual(fri_payload, self.f["fri_payload"])
        self.assertEqual(evidence, self.f["evidence"])
        self.audit_calls.append(copy.deepcopy((bundle, trust, allow_control)))
        return {"event_count": 2, "event_head": h(181), "assignment_cursor": h(182)}

    def test_missing_keeper_enrollment_fails_before_wrapper_state_creation(self):
        config = copy.deepcopy(self.config)
        del config["keeper"]["enrollment"]
        root = self.root / "unconfigured-wrapper"
        with self.assertRaisesRegex(s.Error, "configured_enrollment_authority_required"):
            w.initialize(root, config)
        self.assertFalse(root.exists())
        self.assertEqual(self.p.enqueues, [])

    def fri_check(self, root, executable, release, settings, evidence, payload):
        self.assertEqual(payload, self.f["fri_payload"])
        self.assertEqual(evidence, self.f["evidence"])
        self.assertEqual(release, self.release)
        self.fri_calls.append(copy.deepcopy(payload))

    def result_check(self, directory, controller, operation, result_file):
        self.assertEqual(controller.root, self.root / "pool" / "warm-controller")
        self.assertEqual(operation, "rental-1")
        self.assertFalse((directory / "authority.json").exists())
        self.assertEqual(io.private_json(result_file), self.f["snark"])
        self.result_checks.append(result_file)

    def runner(self):
        return w.Wrapper(self.store, self.rpc, self.registry_rpc, self.wallet,
                         lambda: self.rpc.now, fri_checker=self.fri_check)

    def publish(self, request=None, proof=None, mutate=None, event_root=None):
        body = {"schema_version": 1, "lane": "child", "release_sha256": w.job.hash_bytes(self.release_raw),
                "payload_sha256": io.digest(self.f["fri_payload"]), "request": copy.deepcopy(request or self.request),
                "evidence": self.f["evidence"], "audit": {"journal_id": h(180)}, "native_lease_deadline": 1500,
                "proof": copy.deepcopy(proof)}
        if mutate:
            mutate(body)
        request = io.work_request(self.f["settings"], body)
        envelope = {"body": body, "sequencer_signature": sign(request, "sequencer")}
        source = io.publish_work(self.root / "inbox", envelope, self.f["fri_payload"], event_root=event_root)
        return request["typed_data"]["message"]["workHash"], source

    def queued(self):
        result = self.runner().step(True)
        self.assertEqual(result["action"], "queued_selected_snark")
        return result["pool_operation"]

    def unstarted(self):
        runner = self.runner()
        def interrupted(*_):
            raise KeyboardInterrupt
        runner.fri_checker = interrupted
        with self.assertRaises(KeyboardInterrupt):
            runner.step(True)
        entry = self.runner().state["jobs"][self.work_hash]
        self.assertEqual((entry["status"], entry["pool_operation"]), ("received", None))

    def changed_payload_work(self, payload, turn=3):
        envelope = io.private_json(self.source / "work.json")
        envelope["body"]["payload_sha256"] = io.digest(payload)
        envelope["body"]["request"]["proposal"]["accepted_package"]["turn"] = turn
        request = io.work_request(self.f["settings"], envelope["body"])
        envelope["sequencer_signature"] = sign(request, "sequencer")
        source = io.publish_work(self.root / "inbox", envelope, payload)
        return request["typed_data"]["message"]["workHash"], source

    def returned(self, identifier):
        self.p.operation(identifier)["status"] = "returned"
        io.immutable_json(self.p.directory(identifier) / "returned-proof.json", self.f["snark"])

    def snapshot(self):
        return {str(path.relative_to(self.root)): path.read_bytes() for path in self.root.rglob("*") if path.is_file()}

    def test_selected_turn_warm_result_verifies_native_snark_then_signs_and_returns(self):
        identifier = self.queued()
        self.assertEqual(len(self.fri_calls), 1)
        self.assertEqual(self.wallet.calls, [])
        self.returned(identifier)
        result = self.runner().step(True)
        self.assertEqual(result["action"], "returned_endorsement")
        answer = io.private_json(self.source / "result.json")
        accepted = io.validate_result(self.f["settings"], self.work_hash, answer, self.keeper["policy"]["expected_operator"])
        self.assertEqual(accepted["proof"], self.f["snark"])
        s.verify_eoa(accepted["prepared"]["wrapper_request"], accepted["wrapper_signature"])
        self.assertEqual(len(self.rpc.verifications), 1)
        self.assertEqual(len(self.result_checks), 1)
        self.assertEqual(len(self.wallet.calls), 2)
        self.assertEqual({json.loads(call[1][1])["primaryType"] for call in self.wallet.calls}, {"AcceptedPackageV1", "WrapperResultV1"})

    def test_complete_flow_replays_real_signed_dispatch_audit(self):
        from test_audit import AuditTests
        case = AuditTests("test_full_history_replays_and_contains_no_host_capabilities")
        case.setUpClass()
        case.setUp()
        self.addCleanup(case.doCleanups)
        case.accepted(1)
        case.accepted(2)
        package = case.package(1, 2)
        for field in ("manifest", "subscriptions", "duties"):
            self.f[field] = package[field]
        self.keeper, self.request, item = setup(self.f)
        self.rpc, self.registry_rpc = Chain(self.f, self.keeper, item), case.rpc
        journal = package["bundle"]["journal_id"]
        io.immutable_json(self.root / "trust" / (journal[2:] + ".json"), package["trust"])
        work_hash, source = self.publish(mutate=lambda body: body.update(audit=package["bundle"]))
        with patch("wrapper.audit.verify_package", REAL_AUDIT_VERIFY):
            identifier = self.queued()
            self.returned(identifier)
            self.assertEqual(self.runner().step(True)["action"], "returned_endorsement")
        answer = io.validate_result(self.f["settings"], work_hash, io.private_json(source / "result.json"),
                                    self.keeper["policy"]["expected_operator"])
        self.assertEqual(answer["prepared"]["sidecar"]["duties"], package["duties"])
        self.assertEqual(io.private_json(self.store.root / "wrapper.json")["checkpoints"][journal]["event_count"],
                         package["bundle"]["checkpoint"]["eventCount"])

    def test_streamed_handoff_retains_archive_for_offline_proof_completion(self):
        from test_audit import AuditTests
        case = AuditTests("test_full_history_replays_and_contains_no_host_capabilities")
        case.setUpClass()
        case.setUp()
        self.addCleanup(case.doCleanups)
        case.accepted(1)
        case.accepted(2)
        package = case.package(1, 2)
        events = package["bundle"]["events"]
        archive = self.root / "signed-archive"
        with patch.object(w.audit, "SEGMENT_EVENTS", 2):
            descriptor = w.audit.export_events(events, archive)
        bundle = {**package["bundle"], "schema_version": 2, "events": descriptor}
        for field in ("manifest", "subscriptions", "duties"):
            self.f[field] = package[field]
        self.keeper, self.request, item = setup(self.f)
        self.rpc, self.registry_rpc = Chain(self.f, self.keeper, item), case.rpc
        journal_id = bundle["journal_id"]
        io.immutable_json(self.root / "trust" / (journal_id[2:] + ".json"), package["trust"])
        work_hash, source = self.publish(mutate=lambda body: body.update(audit=bundle), event_root=archive)
        with patch("wrapper.audit.verify_package", REAL_AUDIT_VERIFY):
            identifier = self.queued()
            retained = self.store.root / "jobs" / work_hash[2:] / "audit-events"
            self.assertEqual(list(w.audit.iter_events(bundle, retained)), events)
            for path in (source / "audit-events").iterdir():
                path.unlink()
            self.returned(identifier)
            self.assertEqual(self.runner().step(True)["action"], "returned_endorsement")
        answer = io.validate_result(self.f["settings"], work_hash, io.private_json(source / "result.json"),
                                   self.keeper["policy"]["expected_operator"])
        self.assertEqual(answer["prepared"]["sidecar"]["duties"], package["duties"])

    def test_completed_history_does_not_block_new_work_or_count_toward_inbox_capacity(self):
        runner = self.runner()
        for index in range(1025):
            work_hash = "0x" + (index + 1000).to_bytes(32, "big").hex()
            runner.state["jobs"][work_hash] = {"status": "returned", "pool_operation": None,
                                             "source": "unused", "last_error": None}
            source = io.directory(self.root / "inbox" / work_hash[2:], create=True)
            io.immutable_json(source / "work.json", {})
        runner.save()
        self.assertTrue((self.store.root / "wrapper.json.sqlite3").is_file())
        resumed = self.runner()
        self.assertEqual(resumed.step(True)["action"], "queued_selected_snark")
        self.assertLessEqual(len(resumed.state["jobs"].cache), 1)
        recovered = self.runner()
        self.assertEqual(len(recovered.state["jobs"]), 1026)
        self.assertEqual(recovered.state["jobs"][self.work_hash]["status"], "computing")

    def test_live_capacity_remains_bounded_when_completed_history_is_unlimited(self):
        runner = self.runner()
        for index in range(1024):
            work_hash = "0x" + (index + 1000).to_bytes(32, "big").hex()
            runner.state["jobs"][work_hash] = {"status": "received", "pool_operation": None,
                                             "source": "unused", "last_error": None}
        runner.save()
        state, artifact = runner.status()
        with self.assertRaisesRegex(s.Error, "wrapper_live_capacity"):
            runner.receive(self.source, state, artifact, True)
        self.assertEqual(self.p.enqueues, [])
        self.assertNotIn(self.work_hash, runner.state["jobs"])

    def test_more_than_inbox_capacity_signed_missed_turns_do_not_block_current_work(self):
        for index in range(1025):
            def missed(body, index=index):
                body["request"]["proposal"]["accepted_package"]["turn"] = 2
                body["native_lease_deadline"] += index
            self.publish(mutate=missed)
        result = self.runner().step(True)
        self.assertEqual(result["action"], "queued_selected_snark")
        self.assertEqual(result["work_hash"], self.work_hash)
        self.assertEqual(len(self.p.enqueues), 1)
        self.assertEqual(len(self.fri_calls), 1)
        self.assertEqual(set(self.runner().state["jobs"]), {self.work_hash})

    def test_future_turn_handoff_is_skipped_then_becomes_eligible(self):
        self.source.joinpath("work.json").unlink()
        work_hash, _ = self.publish(mutate=lambda body:
                body["request"]["proposal"]["accepted_package"].update(turn=4))
        self.assertEqual(self.runner().step(True)["action"], "selected_waiting_for_authenticated_handoff")
        self.assertEqual(self.p.enqueues, [])
        self.assertNotIn(work_hash, self.runner().state["jobs"])
        self.rpc.turn, self.rpc.deadline = 4, 1400
        result = self.runner().step(True)
        self.assertEqual((result["action"], result["work_hash"]), ("queued_selected_snark", work_hash))

    def test_received_missed_turn_releases_live_capacity_before_new_work(self):
        self.unstarted()
        self.rpc.turn, self.rpc.deadline = 4, 1400
        state = k.status(self.keeper, self.rpc, self.rpc.now, self.roster)
        rebound = k.rebind_request(self.f["settings"], self.request, state, self.roster)
        work_hash, _ = self.publish(rebound)
        result = self.runner().step(True)
        self.assertEqual((result["action"], result["work_hash"]), ("queued_selected_snark", work_hash))
        jobs = self.runner().state["jobs"]
        self.assertEqual(jobs[self.work_hash]["status"], "obsolete")
        self.assertEqual(jobs[work_hash]["status"], "computing")
        self.assertEqual(len(self.p.enqueues), 1)

    def test_received_future_turn_survives_earlier_canonical_view_and_recovers(self):
        self.unstarted()
        self.rpc.turn = 2
        self.assertEqual(self.runner().step(True)["action"], "selected_waiting_for_authenticated_handoff")
        self.assertEqual(self.runner().state["jobs"][self.work_hash]["status"], "received")
        self.assertEqual(self.p.enqueues, [])
        self.rpc.turn = 3
        self.assertEqual(self.runner().step(True)["action"], "queued_selected_snark")
        self.assertEqual(self.runner().state["jobs"][self.work_hash]["status"], "computing")

    def test_different_frozen_package_cannot_retire_received_work_by_turn_number(self):
        self.unstarted()
        runner = self.runner()
        state, artifact = runner.status()
        state["current_turn"] = 4
        state["frozen_package"]["reportHash"] = h(222)
        state["frozen_package_hash"] = s.struct_hash("AcceptedPackageV1", s.PACKAGE, state["frozen_package"])
        entry = runner.state["jobs"][self.work_hash]
        self.assertFalse(runner.retire_unstarted(self.work_hash, entry, state, artifact, True))
        self.assertEqual(entry["status"], "received")
        self.assertEqual(self.p.enqueues, [])

    def test_signed_proof_substitution_neither_computes_nor_retires_received_work(self):
        payload = copy.deepcopy(self.f["fri_payload"])
        payload["fri_proofs"][0] = base64.b64encode(b"substituted-native-proof").decode()
        work_hash, source = self.changed_payload_work(payload, turn=2)
        runner = self.runner()
        directory = io.directory(self.store.root / "jobs" / work_hash[2:], create=True)
        io.immutable_json(directory / "work.json", io.private_json(source / "work.json"))
        io.immutable_json(directory / "payload.json", payload, io.MAX_PAYLOAD)
        runner.state["jobs"][work_hash] = {"status": "received", "pool_operation": None,
                                         "source": str(source), "last_error": None}
        runner.save()
        state, artifact = runner.status()
        with self.assertRaisesRegex(s.Error, "duty_native_artifact_mismatch"):
            runner.retire_unstarted(work_hash, runner.state["jobs"][work_hash], state, artifact, True)
        self.assertEqual(runner.state["jobs"][work_hash]["status"], "received")
        self.assertEqual(self.p.enqueues, [])
        self.changed_payload_work(payload)
        self.source.joinpath("work.json").unlink()
        self.assertEqual(self.runner().step(True)["action"], "selected_waiting_for_authenticated_handoff")
        self.assertEqual(self.p.enqueues, [])

    def test_foreign_signed_package_is_not_an_active_candidate(self):
        self.source.joinpath("work.json").unlink()
        self.publish(mutate=lambda body:
                body["request"]["proposal"]["accepted_package"].update(chainAddress=a(230)))
        runner = self.runner()
        self.assertEqual(runner.step(True)["action"], "selected_waiting_for_authenticated_handoff")
        self.assertEqual(runner.state["last_status"]["error"], "work_package_scope_changed")
        self.assertEqual((self.p.enqueues, self.fri_calls, self.wallet.calls), ([], [], []))

    def test_missed_turn_claim_still_requires_signature_directory_and_payload_authentication(self):
        envelope = io.private_json(self.source / "work.json")
        envelope["body"]["request"]["proposal"]["accepted_package"]["turn"] = 2
        envelope["sequencer_signature"] = sign(io.work_request(self.f["settings"], envelope["body"]), "account")
        bad_signature = io.publish_work(self.root / "inbox", envelope, self.f["fri_payload"])
        _, bad_directory = self.publish(mutate=lambda body:
                (body["request"]["proposal"]["accepted_package"].update(turn=2), body.update(native_lease_deadline=1501)))
        renamed = self.root / "inbox" / ("00" * 31 + "01")
        bad_directory.rename(renamed)
        _, bad_payload = self.publish(mutate=lambda body:
                (body["request"]["proposal"]["accepted_package"].update(turn=2), body.update(native_lease_deadline=1502)))
        (bad_payload / "payload.json").unlink()
        io.immutable_json(bad_payload / "payload.json", {"changed": True})
        runner = self.runner()
        for source, reason in ((bad_signature, "invalid_signature|cast_failed"), (renamed, "work_directory_hash_mismatch"),
                               (bad_payload, "work_payload_changed")):
            with self.subTest(reason=reason), self.assertRaisesRegex(s.Error, reason):
                runner.authenticated_work(source)
        result = runner.step(True)
        self.assertEqual((result["action"], result["work_hash"]), ("queued_selected_snark", self.work_hash))
        self.assertEqual(len(self.p.enqueues), 1)

    def test_turn_rollover_returns_bare_proof_and_next_turn_reuses_it_without_new_compute(self):
        identifier = self.queued()
        self.returned(identifier)
        self.rpc.turn, self.rpc.deadline = 4, 1400
        result = self.runner().step(True)
        self.assertEqual(result["action"], "returned_proof_after_turn_change")
        answer = io.private_json(self.source / "result.json")["result"]
        self.assertIsNone(answer["prepared"])
        self.assertEqual(answer["wrapper_signature"], "0x")
        rebound = k.rebind_request(self.f["settings"], self.request, k.status(self.keeper, self.rpc, self.rpc.now, self.roster), self.roster)
        new_hash, new_source = self.publish(rebound, self.f["snark"])
        self.assertNotEqual(new_hash, self.work_hash)
        self.assertEqual(self.runner().step(True)["action"], "returned_endorsement")
        self.assertEqual(len(self.p.operations), 1)
        self.assertIsNotNone(io.private_json(new_source / "result.json")["result"]["prepared"])

    def test_single_period_grace_closure_after_compute_returns_only_verified_bare_proof(self):
        class ClosingRegistry(RegistryRpc):
            closed = False
            def contract(self, address, signature, values, anchor):
                if signature == "isRoundFinalized(uint256)":
                    return s.word(int(self.closed))
                return super().contract(address, signature, values, anchor)
        self.registry_rpc = ClosingRegistry(self.f["settings"], self.f["subscriptions"])
        identifier = self.queued()
        self.returned(identifier)
        self.registry_rpc.closed = True
        self.assertEqual(self.runner().step(True)["action"], "returned_proof_after_turn_change")
        answer = io.private_json(self.source / "result.json")["result"]
        self.assertEqual(answer["proof"], self.f["snark"])
        self.assertIsNone(answer["prepared"])
        self.assertEqual(answer["wrapper_signature"], "0x")
        self.assertEqual([json.loads(call[1][1])["primaryType"] for call in self.wallet.calls], ["WrapperResultV1"])
        self.assertEqual(len(self.rpc.verifications), 1)
        self.assertEqual(len(self.p.enqueues), 1)

    def test_dry_run_has_no_file_pool_verifier_or_wallet_side_effects(self):
        before = self.snapshot()
        self.assertEqual(self.runner().step(False)["action"], "would_verify_and_handle_selected_turn")
        self.assertEqual(self.snapshot(), before)
        self.assertEqual((self.fri_calls, self.wallet.calls, self.p.enqueues, self.rpc.verifications), ([], [], [], []))

    def test_cli_init_dry_run_does_not_create_state(self):
        before = self.snapshot()
        target = self.root / "new-wrapper"
        with patch("sys.argv", ["wrapper.py", "--state-dir", str(target), "init", "--config",
                                 str(self.store.root / "configuration.json")]), patch("builtins.print"):
            w.main()
        self.assertFalse(target.exists())
        self.assertEqual(self.snapshot(), before)

    def test_dry_run_returned_warm_job_does_not_create_locks_or_collect(self):
        identifier = self.queued()
        self.returned(identifier)
        (self.p.store.root / "pool.lock").unlink()
        before = self.snapshot()
        self.assertEqual(self.runner().step(False)["action"], "would_collect_returned_proof")
        self.assertEqual(self.snapshot(), before)
        self.assertEqual((self.result_checks, self.wallet.calls), ([], []))

    def test_enqueue_crash_reuses_frozen_permit_and_idempotent_job(self):
        self.p.interrupt = True
        with self.assertRaises(KeyboardInterrupt):
            self.runner().step(True)
        self.rpc.advance(1)
        self.assertEqual(self.runner().step(True)["action"], "queued_selected_snark")
        self.assertEqual(len(self.p.operations), 1)
        self.assertEqual(len(self.p.enqueues), 2)
        self.assertEqual(self.p.enqueues[0], self.p.enqueues[1])

    def test_cached_signed_result_is_republished_exactly_after_turn_changes(self):
        identifier = self.queued()
        self.returned(identifier)
        original = io.immutable_json
        def interrupt_source(path, value, *args):
            if Path(path) == self.source / "result.json":
                raise KeyboardInterrupt
            return original(path, value, *args)
        with patch("wrapper.io.immutable_json", interrupt_source), self.assertRaises(KeyboardInterrupt):
            self.runner().step(True)
        cached = (self.store.root / "jobs" / self.work_hash[2:] / "result.json").read_bytes()
        calls = len(self.wallet.calls)
        self.rpc.turn = 4
        self.assertEqual(self.runner().step(True)["action"], "recovered_cached_result")
        self.assertEqual((self.source / "result.json").read_bytes(), cached)
        self.assertEqual(len(self.wallet.calls), calls)

    def test_invalid_snark_is_never_signed_or_used_to_mark_pending_range_obsolete(self):
        identifier = self.queued()
        self.returned(identifier)
        self.rpc.valid_proof = False
        self.runner().step(True)
        state = io.private_json(self.store.root / "wrapper.json")
        self.assertEqual(state["jobs"][self.work_hash]["status"], "rejected")
        self.assertEqual(state["last_status"]["error"], "native_snark_rejected")
        self.assertEqual(self.wallet.calls, [])
        self.assertFalse((self.source / "result.json").exists())

    def test_returned_range_metadata_cannot_mark_real_pending_range_obsolete(self):
        self.queued()
        bad = {**self.f["snark"], "to_batch_number": 0}
        with self.assertRaisesRegex(s.Error, "proof_range_or_vk_mismatch"):
            self.runner().finish(self.work_hash, bad, True)
        state = io.private_json(self.store.root / "wrapper.json")
        self.assertEqual(state["jobs"][self.work_hash]["status"], "computing")
        self.assertEqual(self.wallet.calls, [])

    def test_wrong_operator_waits_without_compute_or_signing(self):
        config = copy.deepcopy(self.keeper)
        config["policy"]["expected_operator"] = a(230)
        runner = self.runner()
        runner.keeper = config
        self.assertEqual(runner.step(True)["action"], "continue_fri_until_selected")
        self.assertEqual((self.p.enqueues, self.wallet.calls, self.fri_calls), ([], [], []))

    def test_wrong_roster_and_frozen_report_refuse_compute(self):
        bad = copy.deepcopy(self.roster)
        bad["period"] = 6
        r.atomic_json(self.root / "rosters" / "5.json", bad)
        with self.assertRaisesRegex(s.Error, "roster_context_mismatch"):
            self.runner().step(True)
        r.atomic_json(self.root / "rosters" / "5.json", self.roster)
        self.source.joinpath("work.json").unlink()
        _, source = self.publish(mutate=lambda body: body["request"].update(duties=[]))
        state = k.status(self.keeper, self.rpc, self.rpc.now, self.roster)
        with self.assertRaisesRegex(s.Error, "request_frozen_commitments_changed"):
            self.runner().receive(source, state, self.roster, True)
        self.assertEqual((self.p.enqueues, self.wallet.calls, self.fri_calls), ([], [], []))

    def test_invalid_sequencer_signature_cannot_starve_a_valid_handoff(self):
        bad = io.directory(self.root / "inbox" / ("00" * 31 + "01"), create=True)
        envelope = io.private_json(self.source / "work.json")
        envelope["sequencer_signature"] = sign(io.work_request(self.f["settings"], envelope["body"]), "account")
        io.immutable_json(bad / "work.json", envelope)
        io.immutable_json(bad / "payload.json", self.f["fri_payload"])
        self.assertEqual(self.runner().step(True)["action"], "queued_selected_snark")
        self.assertEqual(len(self.p.operations), 1)
        self.assertEqual(self.wallet.calls, [])

    def test_wallet_wrong_signer_is_rejected_and_no_result_is_published(self):
        self.wallet.role = "account"
        identifier = self.queued()
        self.returned(identifier)
        self.runner().step(True)
        self.assertFalse((self.source / "result.json").exists())
        self.assertEqual(len(self.wallet.calls), 1)

    def test_already_proved_range_becomes_obsolete_without_wallet(self):
        identifier = self.queued()
        self.returned(identifier)
        self.rpc.native_proved, self.rpc.opened = 2, False
        self.assertEqual(self.runner().step(True)["action"], "range_already_proved")
        self.assertEqual(io.private_json(self.store.root / "wrapper.json")["jobs"][self.work_hash]["status"], "obsolete")
        self.assertEqual((self.wallet.calls, self.rpc.verifications), ([], []))

    def test_native_fri_cache_publishes_only_valid_complete_records(self):
        calls = []
        original_run = subprocess.run
        def verify(command, **kwargs):
            if command[0] != str(self.verifier):
                return original_run(command, **kwargs)
            expected = io.private_json(Path(command[command.index("--expected") + 1]))
            output = Path(command[command.index("--output") + 1])
            result = {"schema_version": 1, "verification": "native_v32_fri_payload", "payload_sha256": expected["payload_sha256"],
                      "expected_sha256": hashlib.sha256(s.canonical(expected)).hexdigest(), "vk_hash": expected["vk_hash"],
                      "program_commitment": expected["program_commitment"], "from_batch_number": expected["from_batch_number"],
                      "to_batch_number": expected["to_batch_number"], "proof_count": 2}
            s.write_new(output, result)
            calls.append(command)
            return subprocess.CompletedProcess(command, 0)
        args = (self.store.root / "fri-checks", self.config["fri_verifier"], self.release,
                self.f["settings"], self.f["evidence"], self.f["fri_payload"])
        with patch("wrapper.subprocess.run", verify):
            first = w.verify_fris(*args)
            self.assertEqual(w.verify_fris(*args), first)
        self.assertEqual(len(calls), 1)
        self.assertIn(".verify-", calls[0][-1])
        self.verifier.write_bytes(b"changed")
        with self.assertRaisesRegex(s.Error, "fri_verifier_binary_changed"):
            w.verify_fris(*args)


class CrossEpochWrapperTests(unittest.TestCase):
    setUpClass = classmethod(WrapperTests.setUpClass.__func__)
    setUp = WrapperTests.setUp
    runner = WrapperTests.runner
    fri_check = WrapperTests.fri_check
    result_check = WrapperTests.result_check
    returned = WrapperTests.returned
    audit = WrapperTests.audit
    publish = WrapperTests.publish

    def cross_epoch(self, streamed=False):
        import test_coordinator_cross_epoch as cross
        case = cross.CrossEpochCoordinatorTests("test_old_accepted_range_settles_with_current_roster_and_original_credit")
        case.setUpClass()
        case.setUp()
        self.addCleanup(case.doCleanups)
        if streamed:
            with patch.object(w.journal, "MAX_INLINE_EVENTS", 1):
                case.dispatcher.save()
        case.handoff()
        identifier, bundle = case.frozen()
        work = case.controller.step(True)
        original = Path(case.controller.state["operations"][identifier]["works"][work["work_hash"]]["directory"])
        body = io.private_json(original / "work.json")["body"]
        self.cross, self.cross_identifier, self.cross_bundle = case, identifier, bundle
        self.f = copy.deepcopy(case.f)
        self.f.update(copy.deepcopy(bundle["request"]))
        self.keeper = copy.deepcopy(case.config["keeper"])
        self.keeper["policy"]["expected_operator"] = self.f["proposal"]["candidate"]["operator"]
        self.keeper["enrollment"]["audit_trust_dir"] = str(self.root / "trust")
        self.config = {**self.config, "keeper": self.keeper}
        self.store = w.initialize(self.root / "cross-wrapper", self.config)
        self.wallet = TypedWallet(self.store.root / "signatures")
        self.p.settings["lanes"]["child"]["stages"]["SNARK"]["service"] = self.keeper
        self.rpc, self.registry_rpc, self.roster = case.rpc, case.registry_rpc, case.roster
        io.immutable_json(self.root / "rosters" / "6.json", self.roster)
        for context in bundle["audit"]["contexts"]:
            key, period = context["journal_id"], context["bundle"]["identity"]["period"]
            trust = {"schema_version": 1, "journal_id": key, "enrollment_block_hash": {5: h(100), 6: h(101)}[period],
                     "minimum_checkpoint": {"event_count": 0, "event_head": key, "assignment_cursor": key},
                     "required_readiness": [], "required_duties": []}
            io.immutable_json(self.root / "trust" / (key[2:] + ".json"), trust)
        body["release_sha256"] = w.job.hash_bytes(self.release_raw)
        return self.deliver(body, case.controller.directory(identifier) / "audit-events")

    def deliver(self, body, event_root=None):
        self.cross_body = copy.deepcopy(body)
        signing = io.work_request(self.f["settings"], body)
        envelope = {"body": body, "sequencer_signature": sign(signing, "sequencer")}
        source = io.publish_work(self.root / "inbox", envelope, self.f["fri_payload"], event_root=event_root)
        self.work_hash, self.source = signing["typed_data"]["message"]["workHash"], source
        return self.work_hash, source

    def queue(self):
        with patch("wrapper.audit.verify_package", REAL_AUDIT_VERIFY):
            result = self.runner().step(True)
        self.assertEqual(result["action"], "queued_selected_snark")
        self.assertEqual(result["work_hash"], self.work_hash)
        return result["pool_operation"]

    def test_streamed_cross_epoch_proof_return_preserves_original_periods_and_each_checkpoint(self):
        self.cross_epoch(streamed=True)
        identifier = self.queue()
        retained = self.store.root / "jobs" / self.work_hash[2:] / "audit-events"
        old = next(context for context in self.cross_body["audit"]["contexts"]
                   if context["bundle"]["identity"]["period"] == 5)
        self.assertTrue((retained / old["journal_id"][2:] / "0000000000000000.jsonl").is_file())
        self.returned(identifier)
        with patch("wrapper.audit.verify_package", REAL_AUDIT_VERIFY):
            self.assertEqual(self.runner().step(True)["action"], "returned_endorsement")
        answer = io.validate_result(self.f["settings"], self.work_hash, io.private_json(self.source / "result.json"),
                                    self.keeper["policy"]["expected_operator"])
        self.assertEqual([duty["period"] for duty in answer["prepared"]["sidecar"]["duties"]], [5, 5])
        self.assertEqual(answer["prepared"]["sidecar"]["accepted_package"]["period"], 6)
        state = self.runner().state
        self.assertEqual(len(state["known_receipts"]), 2)
        for context in self.cross_body["audit"]["contexts"]:
            self.assertEqual(state["checkpoints"][context["journal_id"]]["event_count"],
                             context["bundle"]["checkpoint"]["eventCount"])

    def test_historical_consent_requires_its_own_independent_pin(self):
        self.cross_epoch()
        old = next(context for context in self.cross_body["audit"]["contexts"]
                   if context["bundle"]["identity"]["period"] == 5)
        path = self.root / "trust" / (old["journal_id"][2:] + ".json")
        r.atomic_json(path, {**io.private_json(path), "enrollment_block_hash": h(101)})
        with self.assertRaisesRegex(s.Error, "current_service_period_required"):
            self.runner().receive(self.source, *self.runner().status(), True)
        self.assertEqual((self.p.enqueues, self.wallet.calls), ([], []))

    def test_pool_revalidates_cross_epoch_permit_from_independent_configured_pins(self):
        self.cross_epoch()
        self.queue()
        permit = io.private_json(self.store.root / "jobs" / self.work_hash[2:] / "permit.json")
        with patch.object(k, "registry_rpc_for", return_value=self.registry_rpc):
            # The rental pool reloads this permit without the wrapper's in-memory authority.
            checked = k.validate_permit(self.keeper, self.rpc, permit, self.f["evidence"],
                                       self.f["fri_payload"], self.rpc.now, 50)
        self.assertEqual(checked, permit)
        old = next(context for context in self.cross_body["audit"]["contexts"]
                   if context["bundle"]["identity"]["period"] == 5)
        old_path = self.root / "trust" / (old["journal_id"][2:] + ".json")
        old_trust = io.private_json(old_path)
        old_path.unlink()
        with patch.object(k, "registry_rpc_for", return_value=self.registry_rpc), \
                patch.object(self.rpc, "call") as native:
            with self.assertRaises(FileNotFoundError):
                k.validate_permit(self.keeper, self.rpc, permit, self.f["evidence"],
                                  self.f["fri_payload"], self.rpc.now, 50)
            native.assert_not_called()
        io.immutable_json(old_path, old_trust)
        current = self.cross_body["audit"]["current_journal_id"]
        current_path = self.root / "trust" / (current[2:] + ".json")
        r.atomic_json(current_path, {**io.private_json(current_path), "enrollment_block_hash": h(100)})
        with patch.object(k, "registry_rpc_for", return_value=self.registry_rpc), \
                patch.object(self.rpc, "call") as native:
            with self.assertRaisesRegex(s.Error, "enrollment_authority_anchor_mismatch"):
                k.validate_permit(self.keeper, self.rpc, permit, self.f["evidence"],
                                  self.f["fri_payload"], self.rpc.now, 50)
            native.assert_not_called()
        self.assertEqual(len(self.p.enqueues), 1)

    def test_cross_epoch_pool_trust_directory_must_match_before_compute(self):
        self.cross_epoch()
        original = copy.deepcopy(self.keeper)
        other = io.directory(self.root / "other-trust", create=True)
        for value in (None, str(other)):
            with self.subTest(audit_trust_dir=value):
                runner = self.runner()
                runner.keeper = copy.deepcopy(original)
                if value is None:
                    del runner.keeper["enrollment"]["audit_trust_dir"]
                else:
                    runner.keeper["enrollment"]["audit_trust_dir"] = value
                self.p.settings["lanes"]["child"]["stages"]["SNARK"]["service"] = runner.keeper
                with patch("wrapper.audit.verify_package", REAL_AUDIT_VERIFY):
                    with self.assertRaisesRegex(s.Error, "wrapper_pool_audit_trust_directory_mismatch"):
                        runner.receive(self.source, *runner.status(), True)
                self.assertEqual((self.p.enqueues, self.fri_calls, self.wallet.calls), ([], [], []))

    def test_known_receipts_force_historical_context_even_without_external_required_duties(self):
        self.cross_epoch()
        with patch.object(w.journal, "MAX_ROWS", 1):
            self.queue()
        runner = self.runner()
        self.assertTrue((self.store.root / "wrapper.json.sqlite3").is_file())
        self.assertEqual(len(runner.state["known_receipts"]), 2)
        current = next(context for context in self.cross_body["audit"]["contexts"]
                       if context["journal_id"] == self.cross_body["audit"]["current_journal_id"])
        body = copy.deepcopy(self.cross_body)
        body.update(audit=current["bundle"], request=current["request"])
        state, _ = runner.status()
        with self.assertRaisesRegex(s.Error, "aggregate_known_duty_context_omitted"):
            runner.audited(body, state, True, self.f["fri_payload"])
        body = copy.deepcopy(self.cross_body)
        body["audit"]["contexts"] = [current]
        with self.assertRaisesRegex(s.Error, "aggregate_known_duty_context_omitted"):
            runner.audited(body, state, True, self.f["fri_payload"])
        self.assertEqual(len(self.p.enqueues), 1)

    def test_external_historical_receipt_blocks_standalone_context_omission(self):
        self.cross_epoch()
        old, current = (next(context for context in self.cross_body["audit"]["contexts"]
                            if context["bundle"]["identity"]["period"] == period) for period in (5, 6))
        path = self.root / "trust" / (old["journal_id"][2:] + ".json")
        r.atomic_json(path, {**io.private_json(path), "required_duties": old["request"]["duties"]})
        body = copy.deepcopy(self.cross_body)
        body.update(audit=current["bundle"], request=current["request"])
        runner = self.runner()
        with self.assertRaisesRegex(s.Error, "aggregate_known_duty_context_omitted"):
            runner.audited(body, runner.status()[0], True, self.f["fri_payload"])
        self.assertEqual(self.p.enqueues, [])

    def test_grace_closure_after_compute_returns_bare_proof_and_repaired_report_reuses_it(self):
        self.cross_epoch()
        identifier = self.queue()
        self.returned(identifier)
        self.registry_rpc.finalize(660, 102)
        self.rpc.now = self.cross.now = 665
        self.rpc.head["timestamp"] = hex(665)
        self.rpc.blocks[self.rpc.head["number"]] = copy.deepcopy(self.rpc.head)
        original_source = self.source
        with patch("wrapper.audit.verify_package", REAL_AUDIT_VERIFY):
            self.assertEqual(self.runner().step(True)["action"], "returned_proof_after_turn_change")
        answer = io.private_json(original_source / "result.json")["result"]
        self.assertIsNone(answer["prepared"])
        self.assertEqual(answer["wrapper_signature"], "0x")
        self.assertEqual(answer["proof"], self.f["snark"])
        revised = self.cross.controller.refresh_report(self.cross_identifier, self.cross_bundle, True)
        self.assertEqual(revised["request"]["duties"], [])
        self.rpc.item["sidecar"] = self.cross.controller.prepare(revised["request"], self.f["evidence"], self.f["fri_payload"])
        body = copy.deepcopy(self.cross_body)
        current, artifact = self.runner().status()
        rebound = k.rebind_request(self.f["settings"], revised["request"], current, artifact)
        body.update(request=rebound, audit=revised["audit"], proof=self.f["snark"])
        self.deliver(body, self.cross.controller.directory(self.cross_identifier) / "audit-events")
        with patch("wrapper.audit.verify_package", REAL_AUDIT_VERIFY):
            self.assertEqual(self.runner().step(True)["action"], "returned_endorsement")
        result = io.private_json(self.source / "result.json")["result"]
        self.assertEqual(result["prepared"]["sidecar"]["duties"], [])
        self.assertEqual(len(self.p.enqueues), 1)


class HandoffTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.f = fixture()

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()

    def test_atomic_immutable_publication_survives_crash_without_hardlinks(self):
        path = self.root / "value.json"
        with patch("workflow_io.r.sync_dir", side_effect=KeyboardInterrupt), self.assertRaises(KeyboardInterrupt):
            io.immutable_json(path, {"value": 1})
        self.assertEqual(path.stat().st_nlink, 1)
        io.immutable_json(path, {"value": 1})
        self.assertEqual(io.private_json(path), {"value": 1})
        with self.assertRaisesRegex(s.Error, "immutable_handoff_changed"):
            io.immutable_json(path, {"value": 2})

    def test_ambiguous_native_acceptance_preserves_pending_on_stale_retry(self):
        authority = {"schema_version": 1, "stage": "SNARK", "status": "picked", "vk_hash": self.f["settings"]["vk_hash"],
                     "bounds": {"from_batch_number": 1, "to_batch_number": 2}, "lease_token": h(150),
                     "endpoint": "http://127.0.0.1:3125/", "submission_sha256": None}
        s.write_new(self.root / "authority.json", authority)
        calls = []
        class Network:
            def request(inner, url, method, data, auth):
                calls.append((url, method, data, auth))
                current = io.private_json(self.root / "authority.json")
                self.assertEqual(current["status"], "submission_pending")
                self.assertEqual(current["submission_sha256"], w.job.hash_bytes(data))
                return ((503, {}, b"") if len(calls) == 1 else
                        (409, {"x-syscoin-prover-disposition": "rejected"}, b""))
        network = Network()
        for _ in range(2):
            with self.assertRaisesRegex(s.Error, "native_submission_uncertain"):
                native_handoff.submit(self.root, self.f["snark"], "private-auth", network)
        self.assertEqual(calls[0], calls[1])
        self.assertEqual(io.private_json(self.root / "authority.json")["status"], "submission_pending")
        raw = s.decode_proof(self.f["snark"]["proof"])
        changed = {**self.f["snark"], "proof": base64.b64encode(b"\xff" + raw[1:]).decode()}
        with self.assertRaisesRegex(s.Error, "native_submission_already_frozen"):
            native_handoff.submit(self.root, changed, "private-auth", network)


if __name__ == "__main__":
    unittest.main()
