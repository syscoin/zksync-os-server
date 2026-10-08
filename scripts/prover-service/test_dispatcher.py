import base64
import copy
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import dispatcher as d
import service as s
from test_service import fixture, sign, h, a, KEYS


class Rpc:
    def __init__(self, settings, subscriptions):
        self.settings = settings
        self.subscriptions = subscriptions
        self.fail = None
        self.active = True
        self.vk = s.raw_hex(settings["vk_hash"])
        self.anchors = []
        self.extra_enrolled = []
        self.ineligible = set()
        self.gateway = None

    def call(self, method, params):
        if method == "eth_chainId":
            return self.settings["registry_chain_id"]
        if method == "eth_getBlockByNumber":
            assert params == ["finalized", False]
            return {"hash": h(100), "timestamp": "0x226" if self.active else "0x1c2"}
        if method == "eth_getBlockByHash":
            assert params == [h(100), False]
            return {"hash": h(100), "timestamp": "0x226" if self.active else "0x1c2"}
        raise AssertionError(method)

    def contract(self, address, signature, values, anchor):
        assert address == self.settings["registry"]
        self.anchors.append(anchor)
        assert anchor == h(100)
        if signature == self.fail:
            return bytes(32)
        enrolled = [entry["subscription"]["account"] for entry in self.subscriptions] + self.extra_enrolled
        if signature == "friSubscriberCount(address,uint64)":
            return s.word(len(enrolled))
        if signature == "friSubscriberAt(address,uint64,uint256)":
            return bytes(12) + s.raw_hex(enrolled[values[2]])
        if signature == "isEligibleFriSubscriber(address,address,uint64)":
            return s.word(int(values[0] not in self.ineligible))
        if signature == "supportedLane(uint256)":
            settings = self.settings if values[0] == s.uint(self.settings["execution_chain_id"]) else self.gateway
            if settings is None:
                return bytes(64)
            vk = self.vk if settings is self.settings else s.raw_hex(settings["vk_hash"])
            return bytes(12) + s.raw_hex(settings["chain_address"]) + vk
        fixed = {"policyHash()": s.raw_hex(self.settings["policy_hash"]),
                 "settlementChainAddress()": bytes(12) + s.raw_hex(self.settings["chain_address"]),
                 "dutiesPerRound()": s.word(self.settings["duties_per_round"]),
                 "startTime()": s.word(0), "periodSeconds()": s.word(100),
                 "firstServicePeriod()": s.word(5), "serviceActive()": s.word(int(self.active)),
                 "productionVkHash()": self.vk, "bootstrapVkHash()": self.vk,
                 "seniorBonus(address)": s.word(35000)}
        if signature in fixed:
            return fixed[signature]
        for signed in self.subscriptions:
            sub = signed["subscription"]
            hashed = s.struct_hash("ProverSubscriptionV1", s.SUBSCRIPTION, sub)
            if signature == "subscriptionAt(address,address,uint64)" and values[0] == sub["account"]:
                return s.raw_hex(hashed)
            if signature == "operatorAccountAt(address,uint64)" and values[0] == sub["operator"]:
                return bytes(12) + s.raw_hex(sub["account"])
            if signature == "subscription(bytes32)" and values[0] == hashed:
                return s.encode_fields(s.SUBSCRIPTION, sub)
        return bytes(32)


class Network:
    def __init__(self):
        self.responses = []
        self.calls = []

    def request(self, url, method="GET", data=None, authorization=None, maximum=None):
        self.calls.append((url, method, data, authorization))
        if not self.responses:
            raise AssertionError("unexpected request " + url)
        response = self.responses.pop(0)
        if isinstance(response, Exception):
            raise response
        status, headers, payload = response
        return status, headers, s.canonical(payload) if not isinstance(payload, bytes) else payload


class DispatcherTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.fixture = fixture()

    def setUp(self):
        self.f = copy.deepcopy(self.fixture)
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name) / "dispatcher"
        self.rpc = Rpc(self.f["settings"], self.f["subscriptions"])
        self.network = Network()
        self.store = d.initialize(self.root, self.f["settings"], self.f["subscriptions"], 5,
                                  "https://trusted.example/", self.rpc)
        self.dispatcher = d.Dispatcher(self.store, self.network, "Basic host-only", now=550, registry_rpc=self.rpc)
        self.account = self.f["subscription"]["account"]

    def reload(self, now=550):
        self.dispatcher = d.Dispatcher(self.store, self.network, "Basic host-only", now=now,
                                       registry_rpc=getattr(self, "registry_rpc", self.rpc))
        return self.dispatcher

    def ready(self, account=None, key="operator", expires=590):
        account = account or self.account
        request = self.dispatcher.request(account, expires)
        self.dispatcher.ready(request, sign(request, key))
        return request

    def evidence(self, number=1):
        evidence = copy.deepcopy(self.f["evidence"])
        if number == 2:
            evidence["previous_batch"] = evidence["batches"][0]["stored"]
        evidence["batches"] = [evidence["batches"][number - 1]]
        return evidence

    def pick(self, number=1, token=None):
        token = token or h(101 + number)
        self.network.responses += [(200, {}, {"batch_number": number, "vk_hash": self.f["settings"]["vk_hash"],
                                              "prover_input": base64.b64encode(b"\1\0\0\0").decode(),
                                              "lease_token": token}), (200, {}, self.evidence(number))]
        return self.dispatcher.pick()

    def offered(self, number=1, token=None):
        self.ready()
        operation = self.pick(number, token)
        self.dispatcher.authorize(operation, sign(self.dispatcher.offer_request(operation), "sequencer"))
        return operation

    def signed_proof(self, operation, number=1):
        proof = copy.deepcopy(self.f["proofs"][number - 1])
        signature = sign(self.dispatcher.proof_request(operation, proof), "operator")
        return proof, signature

    def test_canonical_authority_is_required_before_dispatch_work(self):
        before = (self.root / "state.json").read_bytes()
        with self.assertRaisesRegex(s.Error, "registry_rpc_required"):
            d.Dispatcher(self.store, self.network, "Basic host-only", now=550)
        with patch.object(self.rpc, "call", return_value="0x1"), self.assertRaisesRegex(s.Error, "wrong_rpc_chain"):
            d.Dispatcher(self.store, self.network, "Basic host-only", now=550, registry_rpc=self.rpc)
        with self.assertRaisesRegex(s.Error, "authenticated_enrollment_authority_required"):
            d.Dispatcher(self.store, self.network, "Basic host-only", now=550, registry_rpc=self.rpc,
                         enrollment={"authenticated": True})
        self.assertEqual(self.network.calls, [])
        self.assertEqual((self.root / "state.json").read_bytes(), before)

    def test_eoa_duty_in_complete_mixed_roster_survives_native_acceptance(self):
        contract = copy.deepcopy(self.f["subscriptions"][0])
        contract["subscription"].update(account=a(169), operator=self.account)
        contract["signature"] = "0x1234"
        subscriptions = [*self.f["subscriptions"], contract]
        registry_rpc = Rpc(self.f["settings"], subscriptions)
        store = d.initialize(Path(self.tmp.name) / "mixed", self.f["settings"], subscriptions, 5,
                             "https://trusted.example/", registry_rpc)
        self.dispatcher = d.Dispatcher(store, self.network, "Basic host-only", now=550, registry_rpc=registry_rpc)
        operation = self.offered()
        proof, signature = self.signed_proof(operation)
        self.network.responses.append((204, {"x-syscoin-prover-disposition": "accepted"}, b""))
        self.assertEqual(self.dispatcher.submit(operation, proof, signature), "accepted")
        duty = s.read_json(self.dispatcher.directory(operation) / "duty.json", private=True)
        self.assertEqual(duty["account"], self.account)
        self.assertEqual(self.dispatcher.report()["accounts"][self.account]["native_accepted"], 1)

    def test_finalized_enrollment_checks_signature_membership_and_live_chain(self):
        self.assertTrue(self.rpc.anchors)
        for failure in ("subscriptionAt(address,address,uint64)", "operatorAccountAt(address,uint64)",
                        "subscription(bytes32)", "policyHash()", "seniorBonus(address)", "supportedLane(uint256)"):
            self.rpc.fail = failure
            with self.assertRaises(s.Error):
                d.enrollment_snapshot(self.f["settings"], self.f["subscriptions"], 5, self.rpc)
        self.rpc.fail = None
        bad = copy.deepcopy(self.f["subscriptions"])
        bad[0]["subscription"]["operator"] = a(77)
        with self.assertRaises(s.Error):
            d.enrollment_snapshot(self.f["settings"], bad, 5, self.rpc)
        with self.assertRaisesRegex(s.Error, "current_service_period"):
            d.enrollment_snapshot(self.f["settings"], self.f["subscriptions"], 6, self.rpc)

    def test_initialization_reserves_capacity_for_the_complete_roster_quota(self):
        for count, quota, accepted in ((250, 8, True), (32, 64, False)):
            with self.subTest(count=count, quota=quota):
                settings = {**self.f["settings"], "duties_per_round": quota}
                subscriptions = [copy.deepcopy(self.f["subscriptions"][0]) for _ in range(count)]
                for index, signed in enumerate(subscriptions):
                    signed["subscription"]["account"] = f"0x{index + 1000:040x}"
                root = Path(self.tmp.name) / f"capacity-{count}-{quota}"
                # Enrollment authentication is covered above; this probes initialization after
                # a complete finalized snapshot has been validated.
                with patch.object(d, "enrollment_snapshot", return_value=(subscriptions,
                        self.dispatcher.state["enrollment"])):
                    if accepted:
                        store = d.initialize(root, settings, subscriptions, 5, "https://trusted.example/", self.rpc)
                        self.assertEqual(len(s.read_json(store.root / "state.json", private=True)["accounts"]), count)
                    else:
                        with self.assertRaisesRegex(s.Error, "full_roster_quota_exceeds_dispatcher_capacity"):
                            d.initialize(root, settings, subscriptions, 5, "https://trusted.example/", self.rpc)
                        self.assertFalse(root.exists())

    def test_operator_request_replay_wrong_identity_and_expiry_fail_before_pick(self):
        request = self.dispatcher.request(self.account, 590)
        with self.assertRaises(s.Error):
            self.dispatcher.ready(request, sign(request, "account"))
        self.dispatcher.ready(request, sign(request, "operator"))
        with self.assertRaises(s.Error):
            self.dispatcher.ready(request, sign(request, "operator"))
        self.assertEqual(self.network.calls, [])
        self.reload(591)
        self.assertEqual(self.dispatcher.pick(), "no_authenticated_demand")
        replacement = self.dispatcher.request(self.account, 599)
        self.dispatcher.ready(replacement, sign(replacement, "operator"))
        self.assertEqual(self.dispatcher.state["accounts"][self.account]["nonce"], 2)

    def test_complete_snapshot_rejects_omission_but_removed_subscriber_cannot_block_it(self):
        omitted = a(77)
        self.rpc.extra_enrolled = [omitted]
        with self.assertRaisesRegex(s.Error, "omits_or_adds"):
            d.enrollment_snapshot(self.f["settings"], self.f["subscriptions"], 5, self.rpc)
        self.rpc.ineligible.add(omitted)
        normalized, _ = d.enrollment_snapshot(self.f["settings"], self.f["subscriptions"], 5, self.rpc)
        self.assertEqual(normalized, self.f["subscriptions"])
        self.rpc.extra_enrolled = [omitted] * 256
        with self.assertRaisesRegex(s.Error, "capacity"):
            d.enrollment_snapshot(self.f["settings"], self.f["subscriptions"], 5, self.rpc)

    def test_first_bootstrap_has_no_registry_vk_pin_but_active_lane_must_have_one(self):
        self.rpc.active = False
        self.rpc.vk = bytes(32)
        subscriptions, anchor = d.enrollment_snapshot(self.f["settings"], self.f["subscriptions"], 5, self.rpc)
        self.assertEqual(subscriptions, self.f["subscriptions"])
        self.assertEqual((anchor["phase"], anchor["ends_at"]), ("bootstrap", 500))
        self.rpc.vk = bytes([99]) * 32
        with self.assertRaisesRegex(s.Error, "registry_vk_mismatch"):
            d.enrollment_snapshot(self.f["settings"], self.f["subscriptions"], 5, self.rpc)
        self.rpc.active = True
        self.rpc.vk = bytes(32)
        with self.assertRaisesRegex(s.Error, "registry_vk_mismatch"):
            d.enrollment_snapshot(self.f["settings"], self.f["subscriptions"], 5, self.rpc)

    def test_sparse_demand_and_proven_unleased_responses_do_not_consume_cursor_or_quota(self):
        self.assertEqual(self.dispatcher.pick(), "no_authenticated_demand")
        self.ready()
        cursor = self.dispatcher.state["cursor"]
        for response in [(204, {}, b""), (429, {"x-syscoin-prover-pick-outcome": "unleased"}, b"")]:
            self.network.responses.append(response)
            self.assertEqual(self.dispatcher.pick(), "no_job")
        self.assertEqual(self.dispatcher.state["cursor"], cursor)
        self.assertEqual(self.dispatcher.report()["accounts"][self.account]["offered_opportunities"], 0)
        self.assertIsNotNone(self.dispatcher.state["accounts"][self.account]["ready"])
        self.assertTrue(all("nonempty_only=true" in call[0] for call in self.network.calls))

    def test_uncertain_pick_requires_explicit_unexported_abandonment_before_progress(self):
        self.ready()
        cursor = self.dispatcher.state["cursor"]
        account = copy.deepcopy(self.dispatcher.state["accounts"][self.account])
        self.network.responses.append(d.TransportError("transport_failure"))
        with self.assertRaises(d.TransportError):
            self.dispatcher.pick()
        self.reload()
        with self.assertRaisesRegex(s.Error, "recover_pending_pick"):
            self.dispatcher.pick()
        self.assertEqual(len(self.network.calls), 1)
        self.assertEqual(self.dispatcher.report()["accounts"][self.account]["offered_opportunities"], 0)
        previous = self.dispatcher.state["pending_pick"]
        self.dispatcher.abandon_unexported_pick()
        self.assertEqual(self.dispatcher.state["operations"][previous]["status"], "unexported_pick_abandoned")
        self.assertEqual(self.dispatcher.state["cursor"], cursor)
        self.assertEqual(self.dispatcher.state["accounts"][self.account], account)
        operation = self.pick()
        self.assertNotEqual(operation, previous)
        self.assertEqual(self.dispatcher.state["operations"][operation]["assignment"]["attempt"], 1)
        with self.assertRaises(s.Error):
            self.dispatcher.abandon_unexported_pick()

    def test_empty_evidence_and_replaced_statement_never_create_assignment(self):
        self.ready()
        evidence = self.evidence()
        output = evidence["batches"][0]["output"]
        output["l1TxCount"] = output["l2TxCount"] = "0x0"
        evidence["batches"][0]["stored"]["numberOfLayer1Txs"] = "0x0"
        evidence["batches"][0]["stored"]["commitment"] = s.output_hash(output)
        self.network.responses = [(200, {}, {"batch_number": 1, "vk_hash": self.f["settings"]["vk_hash"],
                                             "prover_input": "AQAAAA==", "lease_token": h(120)}),
                                  (200, {}, evidence)]
        with self.assertRaisesRegex(s.Error, "nonempty_exact"):
            self.dispatcher.pick()
        op = next(iter(self.dispatcher.state["operations"].values()))
        self.assertIsNone(op["assignment"])
        self.assertEqual(self.dispatcher.state["accounts"][self.account]["offered"], 0)
        with self.assertRaisesRegex(s.Error, "retained_pick"):
            self.dispatcher.abandon_unexported_pick()

    def test_crash_after_empty_directory_creation_can_resume_without_duplicate_lease(self):
        self.ready()
        directory = self.root / "job-000000"
        directory.mkdir(mode=0o700)
        self.reload()
        operation = self.pick()
        self.assertEqual(operation, "job-000000")
        self.assertEqual(sum("FRI/pick" in item[0] for item in self.network.calls), 1)

    def test_signed_offer_and_operator_duty_bind_actual_verifier_acceptance(self):
        operation = self.offered()
        proof, signature = self.signed_proof(operation)
        with self.assertRaises(s.Error):
            self.dispatcher.submit(operation, proof, sign(self.dispatcher.proof_request(operation, proof), "account"))
        self.network.responses.append((204, {"x-syscoin-prover-disposition": "accepted"}, b""))
        self.assertEqual(self.dispatcher.submit(operation, proof, signature), "accepted")
        authority = s.read_json(self.root / operation / "authority.json", private=True)
        duty = s.read_json(self.root / operation / "duty.json", private=True)
        self.assertEqual(authority["status"], "accepted")
        self.assertEqual(duty["operatorSignature"], signature)
        self.assertNotIn("lease_token", s.read_json(self.root / operation / "payload.json", private=True))
        self.assertNotIn(authority["lease_token"], json.dumps(s.read_json(self.root / operation / "manifest.json")))
        self.assertEqual(self.dispatcher.report()["accounts"][self.account]["native_accepted"], 1)
        count = len(self.network.calls)
        self.reload()
        self.assertEqual(self.dispatcher.submit(operation, proof, signature), "accepted")
        self.assertEqual(len(self.network.calls), count)
        self.assertEqual(self.dispatcher.report()["accounts"][self.account]["native_accepted"], 1)

    def test_ambiguous_submit_recovers_only_exact_retained_native_proof_and_metadata(self):
        operation = self.offered()
        proof, signature = self.signed_proof(operation)
        self.network.responses.append((503, {}, b""))
        with self.assertRaisesRegex(s.Error, "uncertain"):
            self.dispatcher.submit(operation, proof, signature)
        self.reload()
        self.network.responses += [(200, {}, {"from_batch_number": 1, "to_batch_number": 1,
                                              "vk_hash": proof["vk_hash"], "fri_proofs": [proof["proof"]]}),
                                   (200, {}, self.evidence())]
        self.assertEqual(self.dispatcher.submit(operation, proof, signature), "accepted")
        self.assertEqual(sum("FRI/submit" in item[0] for item in self.network.calls), 1)

    def test_changed_submission_and_proxy_acceptance_cannot_mint_receipt(self):
        operation = self.offered()
        proof, signature = self.signed_proof(operation)
        self.network.responses.append((204, {}, b""))
        with self.assertRaisesRegex(s.Error, "uncertain"):
            self.dispatcher.submit(operation, proof, signature)
        changed = {**proof, "proof": base64.b64encode(b"different").decode()}
        with self.assertRaisesRegex(s.Error, "submission_bytes_changed"):
            self.dispatcher.submit(operation, changed, sign(self.dispatcher.proof_request(operation, changed), "operator"))
        self.assertFalse((self.root / operation / "duty.json").exists())
        self.reload(599)
        with self.assertRaisesRegex(s.Error, "ambiguous"):
            self.dispatcher.expire(operation)
        with self.assertRaises(s.Error):
            self.dispatcher.abandon_unexported_pick()

    def test_retry_uses_new_lease_attempt_and_slot_and_never_double_counts_batch(self):
        first = self.offered()
        proof, signature = self.signed_proof(first)
        self.network.responses.append((422, {"x-syscoin-prover-disposition": "rejected"}, b""))
        self.assertEqual(self.dispatcher.submit(first, proof, signature), "rejected")
        second = self.offered(token=h(123))
        assignment = self.dispatcher.state["operations"][second]["assignment"]
        self.assertEqual((assignment["attempt"], assignment["slot"]), (2, 1))
        manifest = s.read_json(self.root / second / "manifest.json", private=True)
        self.assertEqual(len(manifest["payload"]["retries"]), 1)
        proof, signature = self.signed_proof(second)
        self.network.responses.append((204, {"x-syscoin-prover-disposition": "accepted"}, b""))
        self.dispatcher.submit(second, proof, signature)
        self.assertEqual(self.dispatcher.report()["accounts"][self.account]["native_accepted"], 1)
        self.ready()
        self.network.responses += [(200, {}, {"batch_number": 1, "vk_hash": proof["vk_hash"],
                                              "prover_input": "AQAAAA==", "lease_token": h(124)}),
                                   (200, {}, self.evidence())]
        with self.assertRaisesRegex(s.Error, "live_or_accepted"):
            self.dispatcher.pick()

    def test_unsigned_expired_reservation_does_not_consume_real_opportunity(self):
        self.ready(expires=560)
        operation = self.pick()
        self.assertEqual(self.dispatcher.state["accounts"][self.account]["offered"], 0)
        self.reload(561)
        self.dispatcher.expire(operation)
        self.ready(expires=590)
        second = self.pick(token=h(125))
        self.assertEqual(self.dispatcher.state["operations"][second]["assignment"]["slot"], 0)

    def test_export_allowlist_never_contains_native_lease_or_authentication(self):
        operation = self.offered()
        destination = Path(self.tmp.name) / "public-handoff"
        self.dispatcher.export(operation, destination)
        self.assertEqual({path.name for path in destination.iterdir()},
                         {"payload.json", "evidence.json", "manifest.json", "subscriptions.json", "config.json"})
        authority = s.read_json(self.root / operation / "authority.json", private=True)
        for path in destination.iterdir():
            raw = path.read_text()
            self.assertNotIn(authority["lease_token"], raw)
            self.assertNotIn("Basic host-only", raw)
        proof, signature = self.signed_proof(operation)
        self.network.responses.append((204, {"x-syscoin-prover-disposition": "accepted"}, b""))
        self.dispatcher.submit(operation, proof, signature)
        accepted = Path(self.tmp.name) / "accepted-handoff"
        self.dispatcher.export(operation, accepted)
        self.assertTrue((accepted / "duty.json").exists())
        self.assertFalse((accepted / "authority.json").exists())

    def test_sorted_round_robin_not_request_order_controls_opportunities(self):
        sub = copy.deepcopy(self.f["subscription"])
        sub["account"] = s.cast("wallet", "address", "--private-key", KEYS["wrapper"]).lower()
        sub["operator"] = self.f["subscription"]["account"]
        signed = {"subscription": sub, "signature": sign(s.subscription_request(self.f["settings"], sub), "wrapper")}
        subscriptions = [signed, *self.f["subscriptions"]]
        root = Path(self.tmp.name) / "two-accounts"
        registry_rpc = Rpc(self.f["settings"], subscriptions)
        store = d.initialize(root, self.f["settings"], subscriptions, 5, "https://trusted.example/", registry_rpc)
        self.dispatcher = d.Dispatcher(store, self.network, "Basic host-only", now=550, registry_rpc=registry_rpc)
        for account in sorted(self.dispatcher.subscriptions, reverse=True):
            self.ready(account, "operator" if account == self.account else "account")
        first = self.pick(1)
        second = self.pick(2)
        self.assertEqual([self.dispatcher.state["operations"][key]["account"] for key in (first, second)],
                         sorted(self.dispatcher.subscriptions))


class SharedLaneDispatcherTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.child = fixture()
        cls.gateway = fixture(execution_chain_id="0x1644", settlement_chain_id="0x39", chain_address=a(117))

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name) / "shared"
        self.rpc = Rpc(self.child["settings"], self.child["subscriptions"])
        self.rpc.gateway = self.gateway["settings"]
        self.network = Network()
        self.auth = {"child": "Basic child-only", "gateway": "Basic gateway-only"}
        self.store = d.initialize(self.root, self.child["settings"], self.child["subscriptions"], 5,
                                  "https://child.example/", self.rpc,
                                  {"settings": self.gateway["settings"], "endpoint": "https://gateway.example/"})
        self.dispatcher = d.Dispatcher(self.store, self.network, self.auth, now=550, registry_rpc=self.rpc)
        self.account = self.child["subscription"]["account"]

    def ready(self):
        request = self.dispatcher.request(self.account, 590)
        self.dispatcher.ready(request, sign(request, "operator"))
        return request

    def pick(self, lane):
        item = self.child if lane == "child" else self.gateway
        evidence = copy.deepcopy(item["evidence"])
        evidence["batches"] = evidence["batches"][:1]
        self.network.responses.extend([(200, {}, {"batch_number": 1, "vk_hash": item["settings"]["vk_hash"],
            "prover_input": base64.b64encode(b"\1\0\0\0").decode(), "lease_token": h(201 if lane == "child" else 202)}),
            (200, {}, evidence)])
        return self.dispatcher.pick()

    def complete(self, lane):
        item = self.child if lane == "child" else self.gateway
        self.ready()
        operation = self.pick(lane)
        self.dispatcher.authorize(operation, sign(self.dispatcher.offer_request(operation), "sequencer"))
        proof = item["proofs"][0]
        request = self.dispatcher.proof_request(operation, proof)
        self.assertEqual(request["typed_data"]["domain"]["chainId"], self.child["settings"]["registry_chain_id"])
        self.network.responses.append((204, {"x-syscoin-prover-disposition": "accepted"}, b""))
        self.assertEqual(self.dispatcher.submit(operation, proof, sign(request, "operator")), "accepted")
        return operation

    def test_both_chains_share_slots_but_keep_batch_identity_endpoint_and_credentials(self):
        child = self.complete("child")
        self.dispatcher = d.Dispatcher(self.store, self.network, self.auth, now=551, registry_rpc=self.rpc)
        gateway = self.complete("gateway")
        operations = self.dispatcher.state["operations"]
        self.assertEqual(operations[child]["assignment"]["slot"], 0)
        self.assertEqual(operations[gateway]["assignment"]["slot"], 1)
        self.assertEqual(operations[child]["assignment"]["batch_number"], 1)
        self.assertEqual(operations[gateway]["assignment"]["batch_number"], 1)
        self.assertNotEqual(operations[child]["assignment"]["statement_hash"], operations[gateway]["assignment"]["statement_hash"])
        for url, _, _, authorization in self.network.calls:
            self.assertEqual(authorization, self.auth["child" if url.startswith("https://child.") else "gateway"])
        report = self.dispatcher.report()["accounts"][self.account]
        self.assertEqual((report["offered_opportunities"], report["native_accepted"]), (2, 2))
        with self.assertRaisesRegex(s.Error, "one_execution_lane"):
            self.dispatcher.manifest_payload([child, gateway])
        destination = Path(self.tmp.name) / "gateway-export"
        self.dispatcher.export(gateway, destination)
        self.assertEqual(s.read_json(destination / "config.json"), self.gateway["settings"])
        self.assertNotIn("authority.json", [p.name for p in destination.iterdir()])
        self.assertEqual(s.read_json(destination / "manifest.json")["payload"]["chain_id"], "0x1644")

    def test_no_child_job_rotates_to_gateway_without_spending_an_opportunity(self):
        request = self.ready()
        self.assertEqual(len(request["work_scopes"]), 2)
        self.network.responses.append((204, {}, b""))
        self.assertEqual(self.dispatcher.pick(), "no_job")
        operation = self.pick("gateway")
        self.assertEqual(self.dispatcher.state["operations"][operation]["lane"], "gateway")
        self.assertEqual(self.dispatcher.state["accounts"][self.account]["offered"], 0)

    def test_foreign_lane_evidence_preserves_uncertain_pick_without_credit(self):
        self.ready()
        self.network.responses.append((204, {}, b""))
        self.dispatcher.pick()
        with self.assertRaisesRegex(s.Error, "evidence_identity_mismatch"):
            self.pick("child")
        self.assertIsNotNone(self.dispatcher.state["pending_pick"])
        self.assertEqual(self.dispatcher.state["accounts"][self.account]["offered"], 0)
        self.assertEqual(self.dispatcher.state["operations"][self.dispatcher.state["pending_pick"]]["lane"], "gateway")

    def test_credentials_and_readiness_must_bind_both_lanes(self):
        with self.assertRaisesRegex(s.Error, "lane_credentials_required"):
            d.Dispatcher(self.store, self.network, "Basic one-ambiguous-credential", now=550, registry_rpc=self.rpc)
        request = self.dispatcher.request(self.account, 590)
        signature = sign(request, "operator")
        request["work_scopes"][1]["chain_address"] = a(199)
        with self.assertRaisesRegex(s.Error, "stale_or_foreign"):
            self.dispatcher.ready(request, signature)

    def test_registration_domain_cannot_be_replaced_with_gateway_chain(self):
        bad = copy.deepcopy(self.gateway["settings"])
        bad["registry_chain_id"] = bad["execution_chain_id"]
        with self.assertRaisesRegex(s.Error, "shared_service_identity_required"):
            d.initialize(Path(self.tmp.name) / "bad", self.child["settings"], self.child["subscriptions"], 5,
                         "https://child.example/", self.rpc, {"settings": bad, "endpoint": "https://gateway.example/"})

    def test_gateway_lost_submit_recovers_only_matching_origin_evidence(self):
        self.complete("child")
        self.ready()
        operation = self.pick("gateway")
        self.dispatcher.authorize(operation, sign(self.dispatcher.offer_request(operation), "sequencer"))
        proof = self.gateway["proofs"][0]
        signature = sign(self.dispatcher.proof_request(operation, proof), "operator")
        self.network.responses.append((503, {}, b""))
        with self.assertRaisesRegex(s.Error, "submission_uncertain_retry_identical_bytes"):
            self.dispatcher.submit(operation, proof, signature)
        self.assertEqual(self.dispatcher.state["operations"][operation]["status"], "submission_pending")
        submission = self.network.calls[-1]
        retained = {"from_batch_number": 1, "to_batch_number": 1,
                    "vk_hash": self.gateway["settings"]["vk_hash"], "fri_proofs": [proof["proof"]]}
        foreign = copy.deepcopy(self.child["evidence"])
        foreign["batches"] = foreign["batches"][:1]
        self.network.responses.extend([(200, {}, retained), (200, {}, foreign)])
        with self.assertRaisesRegex(s.Error, "accepted_metadata_changed"):
            self.dispatcher.submit(operation, proof, signature)
        self.assertEqual(self.dispatcher.state["operations"][operation]["status"], "submission_pending")
        correct = copy.deepcopy(self.gateway["evidence"])
        correct["batches"] = correct["batches"][:1]
        self.network.responses.extend([(200, {}, retained), (200, {}, correct)])
        self.assertEqual(self.dispatcher.submit(operation, proof, signature), "accepted")
        for url, method, _, authorization in self.network.calls[-4:]:
            self.assertTrue(url.startswith("https://gateway.example/"))
            self.assertEqual((method, authorization), ("GET", self.auth["gateway"]))
        self.assertTrue(submission[0].startswith("https://gateway.example/"))
        self.assertEqual(submission[3], self.auth["gateway"])
        self.assertEqual(len([call for call in self.network.calls if "/FRI/submit?" in call[0]]), 2)


if __name__ == "__main__":
    unittest.main()
