import copy
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
import urllib.parse

import audit
import coordinator as c
import keeper as k
import relay as r
import roster
import service as s
import workflow_io as io
import test_audit as ta
import test_dispatcher as td
import test_transactions as tt
from test_keeper import setup
from test_proof_check import ProofRpc
from test_relay import FakeWallet, relay_policy, signed_raw
from test_service import KEYS, a, h, sign, fixture
from test_adapter import release


class Wallet:
    def __init__(self, settings):
        self.settings, self.calls = settings, []

    def call(self, method, params):
        self.calls.append((method, copy.deepcopy(params)))
        if method == "eth_chainId":
            return self.settings["settlement_chain_id"]
        if method == "eth_signTransaction":
            return {"raw": signed_raw(params[0], KEYS["sequencer"])}
        if method == "eth_signTypedData_v4":
            data = json.loads(params[1])
            return s.cast("wallet", "sign", "--private-key", KEYS["sequencer"], "--data", json.dumps(data))
        raise AssertionError(method)


class CoordinatorTests(unittest.TestCase):
    setUpClass = classmethod(td.DispatcherTests.setUpClass.__func__)
    reload = td.DispatcherTests.reload
    ready = td.DispatcherTests.ready
    evidence = td.DispatcherTests.evidence
    pick = td.DispatcherTests.pick
    offered = td.DispatcherTests.offered
    signed_proof = td.DispatcherTests.signed_proof
    accepted = ta.AuditTests.accepted

    def setUp(self):
        td.DispatcherTests.setUp(self)
        self.accepted(1)
        self.accepted(2)
        self.registry_rpc = self.rpc
        registry_connection = patch.object(k, "registry_rpc_for", side_effect=lambda _: self.registry_rpc)
        registry_connection.start()
        self.addCleanup(registry_connection.stop)
        self.base = Path(self.tmp.name).resolve()
        self.roster = roster.construct(5, [self.f["proposal"]["candidate"]])
        self.f["proposal"]["accepted_package"]["rosterRoot"] = self.roster["root"]
        self.f["proposal"]["candidate_proof"] = self.roster["proofs"][0]
        keeper, _, item = setup(self.f)
        keeper["policy"]["expected_operator"] = self.f["settings"]["sequencer"]
        self.rpc = ProofRpc(self.f, keeper, item)
        self.rpc.opened = False
        self.native = td.Network()
        for name in ("rosters", "inbox", "witnesses", "publications"):
            io.directory(self.base / name, create=True)
        io.immutable_json(self.base / "rosters" / "5.json", self.roster)
        snark_release = release("SNARK")
        snark_release["vk_hash"] = self.f["settings"]["vk_hash"]
        io.immutable_json(self.base / "release.json", snark_release)
        io.immutable_bytes(self.base / "auth", b"user:password")
        for name in ("sequencer-wallet", "relay-wallet"):
            io.immutable_json(self.base / name, {"url": "http://127.0.0.1:8545", "authorization": None})
        policy = relay_policy(self.f["settings"])
        self.config = {"schema_version": 1, "keeper": keeper, "endpoint": "https://trusted.example/",
            "native_auth_file": str(self.base / "auth"), "release_file": str(self.base / "release.json"),
            "native_lease_seconds": 600, "dispatcher_dir": str(self.root.resolve()), "roster_dir": str(self.base / "rosters"),
            "operator_inboxes": {self.f["proposal"]["candidate"]["operator"]: str(self.base / "inbox")},
            "priority_witness_dir": str(self.base / "witnesses"), "publication_dir": str(self.base / "publications"),
            "sequencer_wallet_file": str(self.base / "sequencer-wallet"), "relay_wallet_file": str(self.base / "relay-wallet"),
            "transaction_policy": {**policy, "account": self.f["settings"]["sequencer"]}, "relay_policy": policy,
            "sequencer_beneficiary": self.f["proposal"]["accepted_package"]["sequencerBeneficiary"], "poll_interval_seconds": 1}
        self.control_store = c.initialize(self.base / "control", self.config)
        self.wallet, self.relay_wallet = Wallet(self.f["settings"]), FakeWallet(self.f["settings"])
        self.now = 1000
        self.controller = self.controller_reload()

    def controller_reload(self):
        self.controller = c.Coordinator(self.control_store, self.rpc, self.wallet, self.relay_wallet,
                                        self.native, clock=lambda: self.now)
        return self.controller

    def acquired(self):
        self.native.responses += [(200, {}, {**self.f["fri_payload"], "lease_token": h(180)}),
                                  (200, {}, self.f["evidence"])]
        identifier = self.controller.reserve()
        self.assertIsNone(self.controller.acquire(identifier, True))
        return identifier

    def test_missing_enrollment_rejects_configuration_before_state_creation(self):
        config = copy.deepcopy(self.config)
        del config["keeper"]["enrollment"]
        root = self.base / "unconfigured-control"
        with self.assertRaisesRegex(s.Error, "configured_enrollment_authority_required"):
            c.initialize(root, config)
        self.assertFalse(root.exists())
        self.assertEqual(self.native.calls, [])

    def test_acquire_authenticates_registry_and_pin_before_lease_mutation(self):
        identifier = self.controller.reserve()
        before = copy.deepcopy(self.controller.state)
        files = {path.name: path.read_bytes() for path in self.controller.directory(identifier).iterdir()}
        with patch.object(self.registry_rpc, "call", return_value="0x1"):
            with self.assertRaisesRegex(s.Error, "wrong_rpc_chain"):
                self.controller.acquire(identifier, True)
        self.assertEqual(self.controller.state, before)
        self.assertEqual(self.native.calls, [])
        self.controller.keeper["enrollment"]["block_hash"] = h(199)
        with self.assertRaisesRegex(s.Error, "enrollment_authority_anchor_mismatch"):
            self.controller.acquire(identifier, True)
        self.assertEqual(self.controller.state, before)
        self.assertEqual(self.native.calls, [])
        self.assertEqual({path.name: path.read_bytes() for path in self.controller.directory(identifier).iterdir()}, files)

    def test_acquire_requires_registry_connection_before_native_pick(self):
        identifier = self.controller.reserve()
        before = copy.deepcopy(self.controller.state)
        with patch.object(k, "registry_rpc_for", side_effect=s.Error("registry_rpc_unavailable")):
            with self.assertRaisesRegex(s.Error, "registry_rpc_unavailable"):
                self.controller.acquire(identifier, True)
        self.assertEqual(self.controller.state, before)
        self.assertEqual(self.native.calls, [])

    def test_snapshot_reuses_authenticated_immutable_enrollment(self):
        identifier = self.acquired()
        count = len(self.registry_rpc.anchors)
        chain, artifact = self.controller.status()
        self.controller.snapshot(identifier, chain, artifact, True)
        self.assertEqual(len(self.registry_rpc.anchors), count)

    def frozen(self):
        identifier = self.acquired()
        chain, roster_artifact = self.controller.status()
        bundle = self.controller.snapshot(identifier, chain, roster_artifact, True)
        prepared = k.prepare(self.f["settings"], bundle["request"], self.f["evidence"], self.f["fri_payload"])
        self.rpc.item["sidecar"] = prepared
        self.rpc.opened = True
        return identifier, bundle

    def returned(self, work, bare=False):
        directory = Path(self.controller.state["operations"][self.controller.state["active"]]["works"][work["work_hash"]]["directory"])
        envelope = io.private_json(directory / "work.json")
        request = envelope["body"]["request"]
        prepared = s.prepare_package(self.f["settings"], self.f["evidence"], request["manifest"],
                        request["subscriptions"], request["duties"], request["proposal"], self.f["snark"], self.f["fri_payload"])
        result = {"schema_version": 1, "work_hash": work["work_hash"], "proof": self.f["snark"],
                  "prepared": None if bare else prepared,
                  "wrapper_signature": "0x" if bare else sign(prepared["wrapper_request"], "wrapper")}
        signed = {"result": result, "operator_signature": sign(io.result_request(self.f["settings"], work["work_hash"],
                                        result, self.f["proposal"]["candidate"]["operator"]), "wrapper")}
        io.immutable_json(directory / "result.json", signed)
        return prepared

    def publication(self, prepared):
        root = io.directory(self.base / "publications" / "native-proof", create=True)
        settings, limits = self.f["settings"], self.config["relay_policy"]
        io.immutable_json(root / "work.json", {"schema_version": 1,
            "execution_chain_id": settings["execution_chain_id"], "settlement_chain_id": settings["settlement_chain_id"],
            "chain_address": settings["chain_address"], "gate": settings["proof_gate"], "gate_code_hash": limits["gate_code_hash"],
            "coordinator": settings["coordinator"], "coordinator_code_hash": limits["coordinator_code_hash"],
            "policy_hash": settings["policy_hash"], "production_vk_hash": settings["vk_hash"], "sequencer": settings["sequencer"],
            "timelock": a(132), "required_confirmations": 2, "batch_from": 1, "batch_to": 2,
            "proof_data": prepared["proof_data"], "batch_outputs": prepared["sidecar"]["batch_outputs"]})
        return root

    def test_dry_run_never_picks_signs_or_writes(self):
        before = {str(path): path.read_bytes() for path in self.base.rglob("*") if path.is_file()}
        self.assertEqual(self.controller.step()["next_action"], "would_pick_native_snark")
        self.assertEqual(self.native.calls, [])
        self.assertEqual(self.wallet.calls, [])
        self.assertEqual(before, {str(path): path.read_bytes() for path in self.base.rglob("*") if path.is_file()})

    def test_signed_snapshot_replays_real_dispatch_audit_and_exact_duties(self):
        identifier, bundle = self.frozen()
        trust = {"schema_version": 1, "journal_id": bundle["audit"]["journal_id"], "enrollment_block_hash": h(100),
                 "minimum_checkpoint": {"event_count": 0, "event_head": bundle["audit"]["journal_id"],
                                        "assignment_cursor": bundle["audit"]["journal_id"]},
                 "required_readiness": [], "required_duties": []}
        request = bundle["request"]
        checked = audit.verify_package(self.f["settings"], self.f["evidence"], request["manifest"],
                    request["subscriptions"], request["duties"], bundle["audit"], trust, self.registry_rpc,
                    fri_payload=self.f["fri_payload"])
        self.assertEqual(checked["required_duties"], request["duties"])
        self.assertEqual(len(request["duties"]), 2)
        self.assertEqual(self.controller.state["operations"][identifier]["bundle"], io.digest(bundle))

    def test_automatic_delivery_keeps_native_capabilities_private_and_restart_is_idempotent(self):
        identifier, _ = self.frozen()
        first = self.controller.step(True)
        self.assertEqual(first["next_action"], "await_selected_wrapper")
        calls = len(self.wallet.calls)
        second = self.controller_reload().step(True)
        self.assertEqual(first, second)
        self.assertEqual(calls, len(self.wallet.calls))
        self.assertEqual(len(self.native.calls), 2)
        work_dir = Path(self.controller.state["operations"][identifier]["works"][first["work_hash"]]["directory"])
        encoded = (work_dir / "work.json").read_bytes() + (work_dir / "payload.json").read_bytes()
        for secret in (b"lease_token", h(180).encode(), b"user:password", b"Basic "):
            self.assertNotIn(secret, encoded)
        self.assertEqual(io.validate_work(self.f["settings"], io.private_json(work_dir / "work.json"),
                                          io.private_json(work_dir / "payload.json")), first["work_hash"])

    def test_turn_change_rebinds_same_frozen_report_without_new_native_pick(self):
        identifier, bundle = self.frozen()
        first = self.controller.step(True)
        self.rpc.turn += 1
        second = self.controller_reload().step(True)
        self.assertNotEqual(first["work_hash"], second["work_hash"])
        works = self.controller.state["operations"][identifier]["works"]
        bodies = [io.private_json(Path(meta["directory"]) / "work.json")["body"] for meta in works.values()]
        self.assertEqual([body["request"]["duties"] for body in bodies], [bundle["request"]["duties"]] * 2)
        self.assertEqual([body["audit"] for body in bodies], [bundle["audit"]] * 2)
        self.assertEqual(len(self.native.calls), 2)

    def test_returned_proof_native_submission_relay_and_confirmed_node_handoff(self):
        identifier, _ = self.frozen()
        work = self.controller.step(True)
        prepared = self.returned(work)
        self.native.responses.append((204, {"x-syscoin-prover-disposition": "accepted"}, b""))
        self.assertEqual(self.controller_reload().step(True)["next_action"], "await_node_publication")
        submission = json.loads(self.native.calls[-1][2])
        self.assertEqual(submission["lease_token"], h(180))
        self.assertEqual(submission["proof"], self.f["snark"]["proof"])
        publication = self.publication(prepared)
        self.rpc.item["digest"] = prepared["sequencer_request"]["digest"]
        staged = self.controller_reload().step(True)
        self.assertEqual(staged["next_action"], "relay_dual_endorsed_package")
        relay = r.Relay(self.f["settings"], self.config["relay_policy"], self.rpc,
                        store=r.Store(self.control_store.root / "relay"))
        self.rpc.item = relay.load_artifact(staged["operation_id"])
        self.assertEqual(self.controller_reload().step(True)["status"], "broadcast")
        self.rpc.accept_receipt()
        self.assertEqual(self.controller_reload().step(True)["status"], "mined_unconfirmed")
        self.rpc.advance()
        self.now = self.rpc.now
        result = self.controller_reload().step(True)
        self.assertTrue(result["written"])
        self.assertTrue((publication / "relay.json").is_file())
        self.assertIsNone(self.controller.state["active"])
        self.assertEqual(self.controller.state["operations"][identifier]["status"], "settled")

    def test_late_proof_is_reused_for_new_turn_endorsement(self):
        identifier, _ = self.frozen()
        work = self.controller.step(True)
        prepared = self.returned(work, bare=True)
        self.publication(prepared)
        self.rpc.turn += 1
        result = self.controller_reload().step(True)
        self.assertEqual(result["next_action"], "await_selected_wrapper")
        source = Path(self.controller.state["operations"][identifier]["works"][result["work_hash"]]["directory"])
        body = io.private_json(source / "work.json")["body"]
        self.assertEqual(body["proof"], self.f["snark"])
        self.assertEqual(body["request"]["proposal"]["accepted_package"]["turn"], self.rpc.turn)
        self.assertEqual(len(self.native.calls), 2)

    def test_ambiguous_pick_blocks_second_acquisition_after_restart(self):
        self.native.responses.append(OSError("connection dropped"))
        with self.assertRaises(OSError):
            self.controller.step(True)
        self.assertEqual(len(self.native.calls), 1)
        with self.assertRaisesRegex(s.Error, "native_pick_uncertain"):
            self.controller_reload().step(True)
        self.assertEqual(len(self.native.calls), 1)

    def test_pre_authority_restart_refreshes_lease_and_preserves_exact_renewal_range(self):
        for renewal in (False, True):
            for window in ("empty", "partial", "complete"):
                with self.subTest(renewal=renewal, window=window):
                    self.control_store = c.initialize(self.base / f"restart-{renewal}-{window}", self.config)
                    self.native, self.now = td.Network(), self.rpc.now
                    self.controller_reload()
                    identifier = self.acquired() if renewal else self.controller.reserve()
                    op = self.controller.state["operations"][identifier]
                    if renewal:
                        op["lease"], op["status"] = None, "picking"
                        self.controller.save()
                    prior_leases = copy.deepcopy(op["leases"])
                    def interrupted_authority(path, value):
                        self.assertEqual(path.name, "authority.json")
                        if window != "empty":
                            raw = c.job.encode(value)
                            if window == "partial":
                                raw = raw[:len(raw) // 2]
                            c.job.write_new(path.with_name(".authority.json." + "a" * 32 + ".tmp"), raw)
                        raise KeyboardInterrupt()
                    with patch.object(c.sentry, "atomic_json", side_effect=interrupted_authority), \
                            self.assertRaises(KeyboardInterrupt):
                        self.controller.acquire(identifier, True)
                    original = copy.deepcopy(op)
                    count = len(self.native.calls)
                    self.rpc.advance(self.config["native_lease_seconds"] + 7)
                    self.now = self.rpc.now
                    self.controller_reload()
                    self.native.responses.append((200, {}, {**self.f["fri_payload"], "lease_token": h(181)}))
                    if not renewal:
                        self.native.responses.append((200, {}, self.f["evidence"]))
                    request = self.native.request
                    def checked_request(url, *args, **kwargs):
                        if "/pick?" in url:
                            retained = c.read_private_json(self.control_store.root / "coordinator.json")["operations"][identifier]
                            self.assertEqual(retained["lease"], original["lease"])
                            self.assertEqual(retained["leases"][:-1], prior_leases)
                            self.assertEqual(retained["leases"][-1], {"name": original["lease"], "started_at": self.now,
                                "deadline": self.now + self.config["native_lease_seconds"]})
                            authority = c.read_private_json(self.controller.directory(identifier) / original["lease"] / "authority.json")
                            self.assertEqual(authority["status"], "pick_uncertain")
                            self.assertEqual(authority["job_id"], identifier + ":" + original["lease"])
                            query = urllib.parse.parse_qs(urllib.parse.urlsplit(url).query)
                            if renewal:
                                expected = {key: self.f["fri_payload"][key] for key in ("from_batch_number", "to_batch_number")}
                                self.assertEqual(authority["expected_bounds"], expected)
                                self.assertEqual(query["snark_batch_from"], [str(expected["from_batch_number"])])
                                self.assertEqual(query["snark_batch_to"], [str(expected["to_batch_number"])])
                            else:
                                self.assertNotIn("expected_bounds", authority)
                                self.assertNotIn("snark_batch_from", query)
                                self.assertNotIn("snark_batch_to", query)
                        return request(url, *args, **kwargs)
                    with patch.object(self.native, "request", side_effect=checked_request):
                        self.assertIsNone(self.controller.acquire(identifier, True))
                    self.assertEqual(len(self.native.calls), count + (1 if renewal else 2))
                    self.assertEqual(self.controller.state["operations"][identifier]["status"], "working")
                    lease = self.controller.directory(identifier) / original["lease"]
                    self.assertFalse(any(lease.glob("*.tmp")))
                    self.assertEqual(io.private_json(self.controller.directory(identifier) / "payload.json"), self.f["fri_payload"])

    def test_existing_authority_or_unknown_evidence_blocks_pick_restart_and_deadline_refresh(self):
        for leftover in ("authority", "picked-wire.json", "unknown.json"):
            with self.subTest(leftover=leftover):
                self.control_store = c.initialize(self.base / ("blocked-" + leftover), self.config)
                self.native, self.now = td.Network(), 1000
                self.controller_reload()
                identifier = self.controller.reserve()
                publish_authority = c.sentry.atomic_json
                def interrupted_authority(path, value):
                    if leftover == "authority":
                        self.assertEqual(path.name, "authority.json")
                        publish_authority(path, value)
                    raise KeyboardInterrupt()
                with patch.object(c.sentry, "atomic_json", side_effect=interrupted_authority), \
                        self.assertRaises(KeyboardInterrupt):
                    self.controller.acquire(identifier, True)
                original = copy.deepcopy(self.controller.state)
                lease = self.controller.directory(identifier) / original["operations"][identifier]["lease"]
                if leftover != "authority":
                    c.job.write_new(lease / leftover, b"retained-evidence")
                files = {path.name: path.read_bytes() for path in lease.iterdir()}
                self.now += self.config["native_lease_seconds"] + 7
                expected_error = s.Error if leftover == "authority" else c.sentry.Error
                message = "native_pick_uncertain" if leftover == "authority" else "pick_initialization_contains_unknown_artifacts"
                with self.assertRaisesRegex(expected_error, message):
                    self.controller_reload().acquire(identifier, True)
                self.assertEqual(self.controller.state, original)
                self.assertEqual(c.read_private_json(self.control_store.root / "coordinator.json"), original)
                self.assertEqual({path.name: path.read_bytes() for path in lease.iterdir()}, files)
                self.assertEqual(self.native.calls, [])

    def test_expired_known_lease_renews_without_changing_frozen_payload(self):
        identifier, bundle = self.frozen()
        self.controller.step(True)
        self.rpc.advance(601)
        self.rpc.deadline = 2000
        self.rpc.status_overrides[(a(123), "maxProofWorkSeconds()")] = 2000
        self.now = self.rpc.now
        result = self.controller.step(True)
        self.assertEqual(result["next_action"], "renew_expired_native_lease_same_frozen_payload")
        self.native.responses.append((200, {}, {**self.f["fri_payload"], "lease_token": h(181)}))
        self.assertIsNone(self.controller.acquire(identifier, True))
        op = self.controller.state["operations"][identifier]
        self.assertEqual(len(op["leases"]), 2)
        self.assertEqual(op["bundle"], io.digest(bundle))
        self.assertEqual(io.private_json(self.controller.directory(identifier) / "payload.json"), self.f["fri_payload"])

    def test_cached_work_signature_does_not_allow_dry_run_to_publish(self):
        identifier, _ = self.frozen()
        result = self.controller.step(True)
        source = Path(self.controller.state["operations"][identifier]["works"][result["work_hash"]]["directory"])
        (source / "work.json").unlink()
        self.assertEqual(self.controller_reload().step(False)["next_action"], "would_deliver_selected_wrapper")
        self.assertFalse((source / "work.json").exists())

    def test_open_maintenance_wallet_recovery_reuses_reserved_transaction(self):
        identifier = self.acquired()
        chain, artifact = self.controller.status()
        bundle = self.controller.snapshot(identifier, chain, artifact, True)
        first = self.controller.step(True)
        self.assertEqual(first["status"], "broadcast")
        transaction = self.rpc.sent[-1]
        self.rpc.advance(31)
        self.now = self.rpc.now
        second = self.controller_reload().step(True)
        self.assertEqual(second["status"], "broadcast")
        self.assertEqual(self.rpc.sent, [transaction, transaction])
        self.assertEqual(len([call for call in self.wallet.calls if call[0] == "eth_signTransaction"]), 1)

    def test_unresolved_dispatcher_receipt_does_not_freeze_or_sign_snapshot(self):
        identifier = self.acquired()
        operation = next(iter(self.dispatcher.state["operations"].values()))
        operation["status"] = "submission_pending"
        self.dispatcher.save()
        self.native.responses.append((404, {}, b""))
        chain, artifact = self.controller.status()
        with self.assertRaisesRegex(s.Error, "wait_for_dispatcher_range_reconciliation"):
            self.controller.snapshot(identifier, chain, artifact, True)
        self.assertFalse((self.controller.directory(identifier) / "snapshot.json").exists())
        self.assertEqual(self.wallet.calls, [])

    def test_invalid_old_result_cannot_starve_current_valid_proof(self):
        identifier, _ = self.frozen()
        old = self.controller.step(True)
        old_path = Path(self.controller.state["operations"][identifier]["works"][old["work_hash"]]["directory"])
        io.immutable_json(old_path / "result.json", {"malformed": True})
        self.rpc.turn += 1
        current = self.controller_reload().step(True)
        self.returned(current)
        self.native.responses.append((204, {"x-syscoin-prover-disposition": "accepted"}, b""))
        self.assertEqual(self.controller_reload().step(True)["next_action"], "await_node_publication")
        self.assertTrue(self.controller.state["operations"][identifier]["works"][old["work_hash"]]["ignored"])

    def test_signed_wrong_range_result_is_rejected_without_rpc_verification(self):
        identifier, _ = self.frozen()
        work = self.controller.step(True)
        meta = self.controller.state["operations"][identifier]["works"][work["work_hash"]]
        result = {"schema_version": 1, "work_hash": work["work_hash"],
                  "proof": {**self.f["snark"], "to_batch_number": 3}, "prepared": None, "wrapper_signature": "0x"}
        io.immutable_json(Path(meta["directory"]) / "result.json", {"result": result,
            "operator_signature": sign(io.result_request(self.f["settings"], work["work_hash"], result,
                                                           meta["operator"]), "wrapper")})
        self.controller_reload().step(True)
        self.assertTrue(self.controller.state["operations"][identifier]["works"][work["work_hash"]]["ignored"])
        self.assertEqual(self.rpc.verifications, [])

    def test_known_empty_picks_are_archived_without_consuming_active_journal_capacity(self):
        self.native.responses.append((204, {"x-syscoin-prover-pick-outcome": "unleased"}, b""))
        self.assertEqual(self.controller.step(True)["next_action"], "native_queue_empty")
        self.assertIsNone(self.controller.state["active"])
        self.assertEqual(self.controller.state["operations"], {})
        self.assertEqual(len(list((self.control_store.root / "jobs").glob("*/completed.json"))), 1)

    def test_period_journal_parent_selects_the_onchain_roster_period(self):
        journals = io.directory(self.base / "journals", create=True)
        self.root.resolve().rename(journals / "5")
        self.config["dispatcher_dir"] = str(journals)
        self.control_store = c.initialize(self.base / "period-control", self.config)
        self.controller_reload()
        _, bundle = self.frozen()
        self.assertEqual(bundle["request"]["manifest"]["payload"]["period"], 5)
        self.assertEqual(len(bundle["request"]["duties"]), 2)

    def test_same_nonce_account_is_rejected(self):
        bad = copy.deepcopy(self.config)
        bad["relay_policy"]["account"] = self.f["settings"]["sequencer"]
        with self.assertRaisesRegex(s.Error, "separate_relay_nonce_account"):
            c.configuration(bad)


class CoordinatorMaintenanceTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.base_fixture = fixture()
        cls.base_keeper, _, _ = setup(cls.base_fixture)
        cls.base_keeper["policy"]["expected_operator"] = cls.base_fixture["settings"]["sequencer"]
        cls.base_policy = relay_policy(cls.base_fixture["settings"])

    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.base = Path(temporary.name).resolve()
        self.f = copy.deepcopy(self.base_fixture)
        keeper, policy = copy.deepcopy(self.base_keeper), copy.deepcopy(self.base_policy)
        for name in ("dispatcher", "rosters", "inbox", "witnesses", "publications"):
            io.directory(self.base / name, create=True)
        snark_release = release("SNARK")
        snark_release["vk_hash"] = self.f["settings"]["vk_hash"]
        io.immutable_json(self.base / "release.json", snark_release)
        self.config = {"schema_version": 1, "keeper": keeper, "endpoint": "https://trusted.example/",
            "native_auth_file": str(self.base / "auth"), "release_file": str(self.base / "release.json"),
            "native_lease_seconds": 600, "dispatcher_dir": str(self.base / "dispatcher"), "roster_dir": str(self.base / "rosters"),
            "operator_inboxes": {self.f["proposal"]["candidate"]["operator"]: str(self.base / "inbox")},
            "priority_witness_dir": str(self.base / "witnesses"), "publication_dir": str(self.base / "publications"),
            "sequencer_wallet_file": str(self.base / "sequencer-wallet"), "relay_wallet_file": str(self.base / "relay-wallet"),
            "transaction_policy": {**policy, "account": self.f["settings"]["sequencer"]}, "relay_policy": policy,
            "sequencer_beneficiary": self.f["proposal"]["accepted_package"]["sequencerBeneficiary"], "poll_interval_seconds": 1}
        self.start("control")

    def start(self, name):
        self.store = c.initialize(self.base / name, self.config)
        self.rpc = tt.Rpc(self.f["settings"], self.config["transaction_policy"])
        self.rpc.store = c.transactions.Store(self.store.root / "transactions")
        self.wallet = tt.Wallet(self.f["settings"], self.config["transaction_policy"], self.rpc.store)
        self.reload()
        self.identifier = self.controller.reserve()

    def reload(self):
        self.controller = c.Coordinator(self.store, self.rpc, self.wallet, clock=lambda: self.rpc.now)
        return self.controller

    def refresh(self):
        return k.maintenance(self.config["keeper"], self.rpc, "refresh-priority", {}, self.rpc.now)

    def pending(self):
        return self.controller.state["operations"][self.identifier]["maintenance"]

    def maintain(self, producer=None):
        producer = producer or self.refresh
        original_stage = c.transactions.Transactions.stage
        def stage(manager, *args, **kwargs):
            identifier = original_stage(manager, *args, **kwargs)
            self.rpc.operation_id = identifier
            return identifier
        with patch.object(c.transactions.Transactions, "stage", new=stage):
            return self.controller.maintenance(self.identifier, producer(), producer, True)

    def recover(self, execute=True):
        pending = self.pending()
        if pending is not None:
            self.rpc.operation_id = pending if isinstance(pending, str) else pending["operation_id"]
        return self.controller.recover_maintenance(self.identifier, execute, None, self.f["evidence"], self.f["fri_payload"])

    def confirm(self, status=1):
        self.rpc.mine(status)
        self.rpc.advance()
        self.reload()
        self.assertIsNone(self.recover())
        self.assertIsNone(self.pending())
        self.rpc.receipt = None

    def signatures(self):
        return [args[0] for method, args in self.wallet.calls if method == "eth_signTransaction"]

    def test_successive_refreshes_and_later_package_survive_coordinator_restart(self):
        identifiers = []
        for work, checkpoint in ((h(150), h(151)), (h(150), h(152)), (h(153), h(154))):
            self.rpc.work_id, self.rpc.checkpoint_hash = work, checkpoint
            self.assertEqual(self.maintain()["status"], "broadcast")
            pending = copy.deepcopy(self.pending())
            identifiers.append(pending["operation_id"])
            self.reload()
            self.assertEqual(self.pending(), pending)
            self.confirm()
        self.assertEqual(len(set(identifiers)), 3)
        self.assertEqual([s.uint(tx["nonce"]) for tx in self.signatures()], [0, 1, 2])
        self.assertEqual(len(self.rpc.sent), 3)

    def test_identical_prefix_witness_is_published_again_after_refresh(self):
        witness = {"batch_item_preimages": [["0x0102"], ["0x0304"]], "left_path": [], "right_path": []}
        for entry, batch in zip(witness["batch_item_preimages"], self.f["evidence"]["batches"]):
            digest = s.keccak(s.raw_hex(s.keccak(b"")) + s.raw_hex(s.keccak(s.raw_hex(entry[0]))))
            batch["output"]["priorityOperationsHash"] = batch["stored"]["priorityOperationsHash"] = digest
            batch["stored"]["commitment"] = s.output_hash(batch["output"])
        io.immutable_json(self.base / "witnesses" / (self.rpc.work_id[2:] + ".json"), witness)
        producer = lambda: k.prefix_call(self.config["keeper"], self.rpc, self.f["evidence"], witness, self.rpc.now)
        self.assertEqual(self.maintain(producer)["status"], "broadcast")
        first = copy.deepcopy(self.pending())
        self.confirm()
        self.assertEqual(self.maintain()["status"], "broadcast")
        self.rpc.checkpoint_hash = h(152)
        self.confirm()
        self.assertEqual(self.maintain(producer)["status"], "broadcast")
        second = copy.deepcopy(self.pending())
        self.assertEqual(first["call"]["transaction"], second["call"]["transaction"])
        self.assertNotEqual(first["operation_id"], second["operation_id"])
        self.confirm()
        self.assertEqual(len(self.signatures()), 3)
        self.assertEqual(len(self.rpc.sent), 3)

    def test_descriptor_is_durable_before_stage_and_reconstructs_after_either_crash(self):
        original_stage = c.transactions.Transactions.stage
        for after_stage in (False, True):
            with self.subTest(after_stage=after_stage):
                self.start("crash-" + str(after_stage))
                def interrupted(manager, *args, **kwargs):
                    disk = io.private_json(self.store.root / "coordinator.json")["operations"][self.identifier]["maintenance"]
                    self.assertEqual(disk["call"], args[0])
                    self.assertEqual(disk["invocation"], args[1])
                    if after_stage:
                        original_stage(manager, *args, **kwargs)
                    raise KeyboardInterrupt()
                with patch.object(c.transactions.Transactions, "stage", new=interrupted), self.assertRaises(KeyboardInterrupt):
                    self.controller.maintenance(self.identifier, self.refresh(), self.refresh, True)
                pending = copy.deepcopy(self.pending())
                self.assertEqual(self.signatures(), [])
                self.assertEqual(len(self.rpc.store.load(self.f["settings"], self.config["transaction_policy"])["operations"]), int(after_stage))
                self.reload()
                before = {str(path): path.read_bytes() for path in self.store.root.rglob("*") if path.is_file()}
                self.recover(False)
                self.assertEqual(before, {str(path): path.read_bytes() for path in self.store.root.rglob("*") if path.is_file()})
                self.assertEqual(self.recover()["status"], "broadcast")
                self.assertEqual(self.pending(), pending)
                self.reload()
                self.assertEqual(self.recover()["status"], "broadcast")
                self.assertEqual(len(self.signatures()), 1)
                self.assertEqual(len(self.rpc.sent), 1)
                self.assertEqual(len(self.rpc.store.load(self.f["settings"], self.config["transaction_policy"])["operations"]), 1)

    def test_stale_unsigned_descriptor_keeps_scope_during_restart(self):
        with patch.object(c.transactions.Transactions, "stage", side_effect=KeyboardInterrupt()), self.assertRaises(KeyboardInterrupt):
            self.controller.maintenance(self.identifier, self.refresh(), self.refresh, True)
        old = copy.deepcopy(self.pending())
        self.rpc.checkpoint_hash = h(152)
        self.reload()
        self.assertIsNone(self.recover())
        self.assertEqual(self.signatures(), [])
        self.assertIsNone(self.pending())
        self.assertEqual(self.maintain()["status"], "broadcast")
        self.assertNotEqual(self.pending()["operation_id"], old["operation_id"])
        old_row = self.rpc.store.load(self.f["settings"], self.config["transaction_policy"])["operations"][old["operation_id"]]
        self.assertEqual(old_row["status"], "stale_unsigned")
        self.assertIsNone(old_row["unsigned_transaction"])
        self.assertEqual(len(self.signatures()), 1)

    def test_ambiguous_send_changed_scope_keeps_nonce_until_canonical_receipt(self):
        self.rpc.send_error = s.Error("rpc_transport_failure")
        self.assertEqual(self.maintain()["status"], "send_uncertain")
        pending = copy.deepcopy(self.pending())
        self.rpc.send_error, self.rpc.checkpoint_hash = None, h(152)
        self.rpc.advance()
        self.reload()
        self.assertEqual(self.recover()["status"], "authorization_stale_reserved")
        self.assertEqual(self.pending(), pending)
        self.assertEqual(len(self.signatures()), 1)
        self.assertEqual(len(self.rpc.sent), 1)
        self.confirm()
        self.assertEqual(self.maintain()["status"], "broadcast")
        self.assertEqual([s.uint(tx["nonce"]) for tx in self.signatures()], [0, 1])

    def test_legacy_pointer_and_orphaned_reservation_are_receipt_only(self):
        for pointed in (False, True):
            with self.subTest(pointed=pointed):
                self.start("legacy-" + str(pointed))
                manager = c.transactions.Transactions(self.f["settings"], self.config["transaction_policy"], self.rpc,
                                                       self.wallet, self.rpc.store, lambda: self.rpc.now)
                legacy = manager.stage(self.refresh())
                self.rpc.operation_id = legacy
                self.assertEqual(manager.step(legacy, True, lambda *args: True)["status"], "broadcast")
                self.controller.state["operations"][self.identifier]["maintenance"] = legacy if pointed else None
                self.controller.save()
                self.rpc.checkpoint_hash = h(152)
                self.rpc.advance()
                self.reload()
                result = self.recover() if pointed else self.maintain()
                self.assertEqual(result["status"], "authorization_stale_reserved")
                self.assertEqual(len(self.signatures()), 1)
                self.assertEqual(len(self.rpc.sent), 1)
                self.confirm()
                retained = copy.deepcopy(self.rpc.store.load(self.f["settings"], self.config["transaction_policy"])["operations"][legacy])
                self.assertEqual(self.maintain()["status"], "broadcast")
                self.assertNotEqual(self.pending()["operation_id"], legacy)
                self.assertEqual(self.rpc.store.load(self.f["settings"], self.config["transaction_policy"])["operations"][legacy], retained)
                self.assertEqual(len(self.signatures()), 2)

    def test_revert_retry_descriptor_survives_restart_and_reorg_blocks_new_nonce(self):
        self.maintain()
        first = copy.deepcopy(self.pending())
        self.rpc.mine(0)
        included, block_hash = self.rpc.receipt["blockNumber"], self.rpc.receipt["blockHash"]
        self.rpc.advance()
        self.reload()
        self.assertIsNone(self.recover())
        self.rpc.receipt = None
        with patch.object(c.transactions.Transactions, "stage", side_effect=KeyboardInterrupt()), self.assertRaises(KeyboardInterrupt):
            self.controller.maintenance(self.identifier, self.refresh(), self.refresh, True)
        second = copy.deepcopy(self.pending())
        self.assertEqual(second["invocation"]["generation"], 1)
        self.assertEqual(first["invocation"]["scope"], second["invocation"]["scope"])
        self.rpc.blocks[included]["hash"] = h(99)
        self.reload()
        self.assertIsNone(self.recover())
        self.assertEqual(len(self.signatures()), 1)
        self.assertEqual(self.maintain()["next_action"], "investigate_reserved_nonce_without_receipt")
        self.rpc.blocks[included]["hash"] = block_hash
        self.rpc.receipt = copy.deepcopy(self.rpc.receipts[self.rpc.store.load(self.f["settings"], self.config["transaction_policy"])["operations"][first["operation_id"]]["transaction_hash"]])
        self.reload()
        self.assertIsNone(self.recover())
        self.rpc.receipt = None
        self.assertEqual(self.maintain()["status"], "broadcast")
        self.assertEqual(self.pending(), second)
        self.assertEqual([s.uint(tx["nonce"]) for tx in self.signatures()], [0, 1])


if __name__ == "__main__":
    unittest.main()
