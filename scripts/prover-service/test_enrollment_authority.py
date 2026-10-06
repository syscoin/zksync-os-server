import copy
from dataclasses import FrozenInstanceError
from pathlib import Path
import unittest
from unittest.mock import patch

import audit
import dispatcher as d
import keeper as k
import test_keeper as tk
import service as s
import test_audit as ta
import test_dispatcher as td
from test_service import fixture, a, h, sign


def contract_account(f):
    f = copy.deepcopy(f)
    sub = f["subscription"]
    sub["account"] = a(169)
    # Contract signature bytes are opaque historical metadata, not an EOA signature.
    f["subscriptions"][0]["signature"] = "0x1234"
    hashed = s.subscription_request(f["settings"], sub)["struct_hash"]
    payload = f["manifest"]["payload"]
    payload["subscription_snapshot_hash"] = s.keccak(s.canonical(f["subscriptions"]))
    for assignment in payload["assignments"]:
        assignment.update(account=sub["account"], subscription_hash=hashed)
    f["manifest"]["sequencer_signature"] = sign(s.manifest_request(f["settings"], payload), "sequencer")
    for duty in f["duties"]:
        duty.update(account=sub["account"], subscriptionHash=hashed)
        duty["operatorSignature"] = sign(s.duty_request(f["settings"], {key: duty[key] for key, _ in s.DUTY}, sub), "operator")
    return f


class AuthorityTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.base = fixture()

    def setUp(self):
        self.f = contract_account(self.base)
        self.rpc = td.Rpc(self.f["settings"], self.f["subscriptions"])

    def authority(self):
        return s.EnrollmentAuthority(self.f["settings"], self.f["subscriptions"], 5, self.rpc, h(100))

    def validate(self, enrollment=None):
        return s.validate_manifest(self.f["settings"], self.f["manifest"], self.f["subscriptions"],
                                   s.validate_evidence(self.f["settings"], self.f["evidence"]), enrollment=enrollment)

    def test_canonical_enrollment_authenticates_contract_account_but_offline_stays_eoa(self):
        with self.assertRaisesRegex(s.Error, "eoa_signature_required"):
            self.validate()
        authority = self.authority()
        self.assertEqual(len(self.validate(authority)[0]), 2)
        self.assertTrue(self.rpc.anchors and all(anchor == h(100) for anchor in self.rpc.anchors))
        with self.assertRaises(FrozenInstanceError):
            authority.period = 6

    def test_complete_mixed_roster_keeps_the_eoa_account(self):
        eoa = copy.deepcopy(self.base["subscriptions"][0])
        eoa["subscription"]["operator"] = eoa["subscription"]["account"]
        eoa["signature"] = sign(s.subscription_request(self.f["settings"], eoa["subscription"]), "account")
        subscriptions = [eoa, *self.f["subscriptions"]]
        rpc = td.Rpc(self.f["settings"], subscriptions)
        normalized, _ = d.enrollment_snapshot(self.f["settings"], subscriptions, 5, rpc)
        self.assertEqual({entry["subscription"]["account"] for entry in normalized},
                         {eoa["subscription"]["account"], self.f["subscription"]["account"]})
        with self.assertRaisesRegex(s.Error, "omits_or_adds"):
            d.enrollment_snapshot(self.f["settings"], [eoa], 5, rpc)

    def test_context_is_not_a_json_flag_and_cannot_authorize_changed_snapshot(self):
        with self.assertRaisesRegex(s.Error, "authenticated_enrollment_authority_required"):
            self.validate({"authenticated": True})
        authority = self.authority()
        for field, value in (("signature", "0xabcd"), ("account", a(170)), ("beneficiary", a(171))):
            with self.subTest(field=field):
                changed = copy.deepcopy(self.f["subscriptions"])
                if field == "signature":
                    changed[0][field] = value
                else:
                    changed[0]["subscription"][field] = value
                with self.assertRaisesRegex(s.Error, "scope_mismatch"):
                    authority.check(self.f["settings"], changed, 5)
        for field, value in (("registry", a(172)), ("registry_chain_id", "0x23b"),
                             ("policy_hash", h(173)), ("sequencer", a(174)), ("duties_per_round", 5)):
            with self.subTest(field=field), self.assertRaisesRegex(s.Error, "scope_mismatch"):
                authority.check({**self.f["settings"], field: value}, self.f["subscriptions"], 5)
        with self.assertRaisesRegex(s.Error, "scope_mismatch"):
            authority.check(self.f["settings"], self.f["subscriptions"], 6)

    def test_authority_requires_canonical_complete_stored_registry_state(self):
        for field, value in (("account", a(177)), ("operator", a(178)), ("beneficiary", a(179)), ("nonce", 8)):
            forged = copy.deepcopy(self.f["subscriptions"])
            forged[0]["subscription"][field] = value
            with self.subTest(field=field), self.assertRaisesRegex(s.Error, "not_enrolled"):
                s.EnrollmentAuthority(self.f["settings"], forged, 5, self.rpc, h(100))
        for failure in ("subscriptionAt(address,address,uint64)", "operatorAccountAt(address,uint64)",
                        "subscription(bytes32)", "seniorBonus(address)"):
            with self.subTest(failure=failure), self.assertRaises(s.Error):
                self.rpc.fail = failure
                self.authority()
        self.rpc.fail = None
        self.rpc.extra_enrolled = [a(175)]
        with self.assertRaisesRegex(s.Error, "omits_or_adds"):
            self.authority()
        self.rpc.ineligible.add(a(175))
        self.authority()
        with patch.object(self.rpc, "call", return_value="0x23b"), self.assertRaisesRegex(s.Error, "wrong_rpc_chain"):
            self.authority()
        original = self.rpc.call
        def changed_block(method, params):
            return {"hash": h(176), "timestamp": "0x226"} if method == "eth_getBlockByHash" else original(method, params)
        with patch.object(self.rpc, "call", side_effect=changed_block), self.assertRaisesRegex(s.Error, "enrollment_block_changed"):
            self.authority()
        with self.assertRaisesRegex(s.Error, "current_service_period_required"):
            s.EnrollmentAuthority(self.f["settings"], self.f["subscriptions"], 6, self.rpc, h(100))

    def test_keeper_reconstructs_authority_from_trusted_configuration_for_permits(self):
        authority = self.authority()
        def prepare(f):
            return s.prepare_package(f["settings"], f["evidence"], f["manifest"], f["subscriptions"], f["duties"],
                                     f["proposal"], f["snark"], f["fri_payload"], enrollment=authority)
        with patch.object(tk, "prepare", side_effect=prepare):
            config, request, item = tk.setup(self.f)
        config["enrollment"] = {"registry_rpc_file": "/trusted/unit-registry.json", "block_hash": h(100)}
        rpc = tk.NativeRpc(self.f, config, item)
        cached = k.permit(config, rpc, request, self.f["evidence"], self.f["fri_payload"], 1000, 50, enrollment=authority)
        self.assertEqual(k.validate_permit(config, rpc, cached, self.f["evidence"], self.f["fri_payload"], 1001, 50,
                                          enrollment=authority), cached)
        wrong_pin = {**config, "enrollment": {**config["enrollment"], "block_hash": h(180)}}
        with self.assertRaisesRegex(s.Error, "enrollment_authority_anchor_mismatch"):
            k.permit(wrong_pin, rpc, request, self.f["evidence"], self.f["fri_payload"], 1000, 50, enrollment=authority)
        with self.assertRaisesRegex(s.Error, "configured_enrollment_authority_required"):
            k.permit(config, rpc, request, self.f["evidence"], self.f["fri_payload"], 1000, 50, enrollment={"authenticated": True})
        with patch.object(k, "registry_rpc_for", return_value=self.rpc) as connection:
            permit = k.permit(config, rpc, request, self.f["evidence"], self.f["fri_payload"], 1000, 50)
            self.assertEqual(k.validate_permit(config, rpc, permit, self.f["evidence"], self.f["fri_payload"], 1001, 50), permit)
            self.assertEqual(k.package_call(config, rpc, request, self.f["evidence"], self.f["fri_payload"], 1000, "open")["action"],
                             "openPackage")
            self.assertEqual(connection.call_count, 3)
        offline = {key: value for key, value in config.items() if key != "enrollment"}
        with self.assertRaisesRegex(s.Error, "configured_enrollment_authority_required"):
            k.permit(offline, rpc, request, self.f["evidence"], self.f["fri_payload"], 1000, 50, enrollment=authority)
        with self.assertRaisesRegex(s.Error, "eoa_signature_required"):
            k.permit(offline, rpc, request, self.f["evidence"], self.f["fri_payload"], 1000, 50)

    def test_fresh_sequencer_operator_and_wrapper_signatures_remain_required(self):
        authority = self.authority()
        request = {name: self.f[name] for name in ("proposal", "manifest", "subscriptions", "duties")}
        k.prepare(self.f["settings"], request, self.f["evidence"], self.f["fri_payload"], enrollment=authority)
        prepared = s.prepare_package(self.f["settings"], self.f["evidence"], self.f["manifest"], self.f["subscriptions"],
            self.f["duties"], self.f["proposal"], self.f["snark"], self.f["fri_payload"], enrollment=authority)
        with self.assertRaises(s.Error):
            s.complete_package(self.f["settings"], prepared, sign(prepared["sequencer_request"], "operator"),
                               sign(prepared["wrapper_request"], "wrapper"))
        with self.assertRaises(s.Error):
            s.complete_package(self.f["settings"], prepared, sign(prepared["sequencer_request"], "sequencer"),
                               sign(prepared["wrapper_request"], "operator"))
        s.complete_package(self.f["settings"], prepared, sign(prepared["sequencer_request"], "sequencer"),
                           sign(prepared["wrapper_request"], "wrapper"))
        self.f["duties"][0]["operatorSignature"] = sign(s.duty_request(self.f["settings"],
            {key: self.f["duties"][0][key] for key, _ in s.DUTY}, self.f["subscription"]), "account")
        with self.assertRaises(s.Error):
            k.prepare(self.f["settings"], request, self.f["evidence"], self.f["fri_payload"], enrollment=authority)
        with self.assertRaises(s.Error):
            s.prepare_package(self.f["settings"], self.f["evidence"], self.f["manifest"], self.f["subscriptions"],
                self.f["duties"], self.f["proposal"], self.f["snark"], self.f["fri_payload"], enrollment=authority)
        self.f["manifest"]["sequencer_signature"] = sign(s.manifest_request(self.f["settings"],
            self.f["manifest"]["payload"]), "account")
        with self.assertRaises(s.Error):
            self.validate(authority)


class ContractAccountFlowTests(unittest.TestCase):
    setUpClass = classmethod(td.DispatcherTests.setUpClass.__func__)
    ready = td.DispatcherTests.ready
    evidence = td.DispatcherTests.evidence
    pick = td.DispatcherTests.pick
    offered = td.DispatcherTests.offered
    signed_proof = td.DispatcherTests.signed_proof
    accepted = ta.AuditTests.accepted
    package = ta.AuditTests.package

    def setUp(self):
        td.DispatcherTests.setUp(self)
        self.f = contract_account(self.f)
        self.rpc = td.Rpc(self.f["settings"], self.f["subscriptions"])
        self.store = d.initialize(Path(self.tmp.name) / "contract-account", self.f["settings"], self.f["subscriptions"],
                                  5, "https://trusted.example/", self.rpc)
        self.dispatcher = d.Dispatcher(self.store, self.network, "Basic host-only", now=550, registry_rpc=self.rpc)
        self.account = self.f["subscription"]["account"]

    def test_contract_account_with_eoa_operator_finishes_dispatch_audit_and_package(self):
        with self.assertRaises(s.Error):
            self.dispatcher.ready(self.dispatcher.request(self.account, 590),
                                  sign(self.dispatcher.request(self.account, 590), "account"))
        self.assertEqual(self.network.calls, [])
        self.accepted(1)
        self.accepted(2)
        package = self.package()
        summary = audit.verify_package(**package)
        self.assertEqual(summary["accounts"][self.account]["offered_opportunities"], 2)
        authority = self.dispatcher.enrollment
        request = {"proposal": self.f["proposal"], **{name: package[name] for name in ("manifest", "subscriptions", "duties")}}
        k.prepare(self.f["settings"], request, package["evidence"], package["fri_payload"], enrollment=authority)
        prepared = s.prepare_package(self.f["settings"], package["evidence"], package["manifest"], package["subscriptions"],
            package["duties"], self.f["proposal"], self.f["snark"], package["fri_payload"], enrollment=authority)
        self.assertEqual(len(prepared["sidecar"]["duties"]), 2)
