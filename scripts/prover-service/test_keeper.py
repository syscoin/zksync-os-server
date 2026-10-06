import copy
import unittest
from unittest.mock import patch

import keeper as k
import relay as r
import roster
import service as s
from test_relay import FakeRpc, relay_policy
from test_dispatcher import Rpc as RegistryRpc
from test_service import fixture, prepare, h, a, abi_type, cast_value, struct_values


def setup(f):
    settings = f["settings"]
    prepared = prepare(f)
    item = {"mode": prepared["mode"], "sidecar": prepared["sidecar"],
            "package_hash": prepared["sequencer_request"]["struct_hash"], "digest": prepared["sequencer_request"]["digest"],
            "calldata": "0xabcdef01"}
    config = {"schema_version": 1, "lane": "child", "rpc_url": "http://127.0.0.1:8545", "settings": settings,
              "enrollment": {"registry_rpc_file": "/trusted/unit-registry.json", "block_hash": h(100)},
              "policy": {"expected_operator": f["proposal"]["candidate"]["operator"] if prepared["mode"] == "service" else settings["sequencer"],
                         "gate_code_hash": s.keccak(b"gate-code"), "coordinator_code_hash": s.keccak(b"coordinator-code"),
                         "priority_guard_code_hash": s.keccak(b"priority-code"), "reserve_seconds": 30,
                         "max_head_age_seconds": 120, "rpc_timeout_seconds": 15}}
    request = {name: f[name] for name in ("proposal", "manifest", "subscriptions", "duties")}
    return config, request, item


class NativeRpc(FakeRpc):
    def __init__(self, f, config, item):
        super().__init__(f["settings"], relay_policy(f["settings"]), item)
        self.evidence = f["evidence"]
        self.control, self.bad_stored, self.bad_role, self.bad_code = False, False, False, False
        self.native_proved, self.opening_period = self.evidence["previous_batch"]["batchNumber"], 5
        self.frozen_bootstrap = k.normalized(item["sidecar"]["accepted_package"])
        self.roster_count, self.status_overrides = 1, {}
        self.simulated = []

    def call(self, method, params):
        if method == "eth_getCode":
            if self.bad_code and params[0] == self.settings["proof_gate"]:
                return "0x00"
            codes = {a(130): b"verifier", a(131): b"plonk"}
            if params[0] in codes:
                return "0x" + codes[params[0]].hex()
        if method == "eth_call":
            tx, anchor = params
            assert anchor.get("requireCanonical") is True
            data, target = s.raw_hex(tx["data"]), tx["to"]
            selector = data[:4]
            for (address, signature), value in self.status_overrides.items():
                if target == address and selector == r.selector(signature):
                    return "0x" + (s.word(int(value)) if type(value) in (bool, int) else s.raw_hex(value).rjust(32, b"\0")).hex()
            if selector == r.selector("getSemverProtocolVersion()"):
                return "0x" + (s.word(0) + s.word(32) + s.word(0)).hex()
            if selector == r.selector("bootstrapPackage()"):
                return "0x" + s.encode_fields(s.PACKAGE, self.frozen_bootstrap).hex()
            if selector == r.selector("frozenPackage()"):
                return "0x" + s.encode_fields(s.PACKAGE, k.normalized(self.item["sidecar"]["accepted_package"])).hex()
            if selector == r.selector("openingRoster()"):
                return "0x" + (s.word(self.opening_period) + s.raw_hex(self.item["sidecar"]["accepted_package"]["rosterRoot"])
                                 + s.word(1) + s.word(int(self.control))).hex()
            if selector == r.selector("storedBatchHash(uint256)"):
                number = int.from_bytes(data[4:], "big")
                stored = next(item for item in [self.evidence["previous_batch"]] + [b["stored"] for b in self.evidence["batches"]]
                              if item["batchNumber"] == number)
                return h(99) if self.bad_stored else s.keccak(s.encode_fields(s.STORED, stored))
            values = {"getChainId()": s.uint(self.settings["execution_chain_id"]),
                      "childChainAddress()": self.settings["chain_address"], "rosterCount()": self.roster_count,
                      "productionVerifier()": a(130),
                      "plonkVerifier()": a(131), "getVerifier()": a(130), "plonkVerifiers(uint32)": a(131),
                      "IS_TESTNET_VERIFIER()": False, "verificationKeyHash(uint256)": self.settings["vk_hash"],
                      "verifierCodeHash()": s.keccak(b"verifier"), "plonkVerifierCodeHash()": s.keccak(b"plonk"),
                      "timelock()": a(132), "isValidator(address)": True,
                      "getRoleMemberCount(address,bytes32)": 2 if self.bad_role else 1,
                      "getRoleMember(address,bytes32,uint256)": self.settings["proof_gate"],
                      "getTotalBatchesVerified()": self.native_proved, "transitionWork()": self.control}
            for signature, value in values.items():
                if selector == r.selector(signature):
                    if type(value) is bool:
                        return "0x" + s.word(int(value)).hex()
                    if type(value) is int:
                        return "0x" + s.word(value).hex()
                    return "0x" + s.raw_hex(value).rjust(32, b"\0").hex()
            actions = [name + "(" + r.abi_tuple(s.PACKAGE) + ")" for name in
                       ("openPackage", "repairPackage", "openBootstrapPackage", "repairBootstrapPackage")]
            actions += ["prepareRosterDraw(uint64)", "recordRosterDraw(uint64)", "captureDraw(bytes32)",
                        "refreshPriorityCheckpoint()", "publishPrefixWitness(bytes32,uint256[],bytes32[],bytes32[],bytes32[])",
                        "requestDraw(bytes32,uint256,uint256,uint16,bytes32[])", "relayDraw(bytes32,uint256,uint256,address)",
                        "relayCheckpoint(uint256,uint256,address)"]
            if selector in [r.selector(signature) for signature in actions]:
                self.simulated.append(copy.deepcopy(tx))
                return "0x"
        return super().call(method, params)


class KeeperTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.base = fixture()

    def setUp(self):
        self.f = copy.deepcopy(self.base)
        self.config, self.request, item = setup(self.f)
        self.rpc = NativeRpc(self.f, self.config, item)
        self.registry_rpc = RegistryRpc(self.f["settings"], self.f["subscriptions"])
        connection = patch.object(k, "registry_rpc_for", return_value=self.registry_rpc)
        connection.start()
        self.addCleanup(connection.stop)

    def permit(self, runtime=50, now=1000):
        return k.permit(self.config, self.rpc, self.request, self.f["evidence"], self.f["fri_payload"], now, runtime)

    def test_exact_native_payload_and_canonical_range_create_revalidatable_permit(self):
        value = self.permit()
        self.assertEqual(value["state"]["deadline"], 1200)
        self.assertEqual(value["payload_sha256"], k.sha(self.f["fri_payload"]))
        self.assertEqual(k.validate_permit(self.config, self.rpc, value, self.f["evidence"], self.f["fri_payload"], 1001, 50), value)

    def test_automated_calls_require_enrollment_before_native_rpc(self):
        config = {key: value for key, value in self.config.items() if key != "enrollment"}
        with patch.object(self.rpc, "call") as native:
            for action in ("permit", "open", "repair"):
                with self.subTest(action=action), self.assertRaisesRegex(s.Error, "configured_enrollment_authority_required"):
                    if action == "permit":
                        k.permit(config, self.rpc, self.request, self.f["evidence"], self.f["fri_payload"], 1000, 50)
                    else:
                        k.package_call(config, self.rpc, self.request, self.f["evidence"], self.f["fri_payload"], 1000, action)
            native.assert_not_called()
        k.prepare(self.f["settings"], self.request, self.f["evidence"], self.f["fri_payload"])

    def test_wrong_registry_chain_fails_before_native_rpc(self):
        with patch.object(self.registry_rpc, "call", return_value="0x1"), patch.object(self.rpc, "call") as native:
            with self.assertRaisesRegex(s.Error, "wrong_rpc_chain"):
                self.permit()
            native.assert_not_called()

    def test_wrong_turn_repair_parent_frontier_or_role_refuses_compute(self):
        for field, changed, reason in (("turn", 4, "stale_wrapper_turn"), ("selected", 1, "stale_wrapper_turn"),
            ("frozen_override", h(222), "frozen_package_repaired"), ("parent", h(221), "accepted_parent_changed"),
            ("native_proved", 1, "native_proved_frontier_changed"), ("bad_role", True, "native_prover_authority_changed"),
            ("bad_stored", True, "native_stored_batch_changed"), ("bad_code", True, "gate_code_changed"),
            ("reorg_anchor", True, "rpc_reorg_or_chain_change"), ("chain_override", "0x1", "wrong_rpc_chain")):
            with self.subTest(field=field):
                original = getattr(self.rpc, field)
                setattr(self.rpc, field, changed)
                with self.assertRaisesRegex(s.Error, reason):
                    self.permit()
                setattr(self.rpc, field, original)

    def test_window_and_witness_checks_precede_rental(self):
        for field, changed, reason in (("witness", False, "priority_prefix_witness_missing"),
                                     ("guard_expired", True, "priority_checkpoint_needs_refresh"),
                                     ("deadline", 1080, "insufficient_compute_window")):
            original = getattr(self.rpc, field)
            setattr(self.rpc, field, changed)
            with self.assertRaisesRegex(s.Error, reason):
                self.permit()
            setattr(self.rpc, field, original)
        with self.assertRaisesRegex(s.Error, "stale_or_future_rpc_head"):
            self.permit(now=1201)
        with self.assertRaisesRegex(s.Error, "insufficient_compute_window"):
            self.permit(runtime=170)

    def test_operator_payload_lane_and_configuration_are_bound(self):
        value = self.permit()
        for field, changed in (("lane", "gateway"), ("payload_sha256", "0" * 64), ("runtime_seconds", 51)):
            altered = {**value, field: changed}
            with self.assertRaisesRegex(s.Error, "compute_permit_binding_changed"):
                k.validate_permit(self.config, self.rpc, altered, self.f["evidence"], self.f["fri_payload"], 1000, 50)
        self.config["policy"]["expected_operator"] = a(200)
        with self.assertRaisesRegex(s.Error, "wrong_local_wrapper"):
            self.permit()

    def test_control_cannot_carry_reward_duties(self):
        self.rpc.control = True
        with self.assertRaisesRegex(s.Error, "control_work_must_have_empty_report"):
            self.permit()
        self.request["duties"] = []
        accepted = k.prepare(self.config["settings"], self.request, self.f["evidence"], self.f["fri_payload"])["accepted_package"]
        self.rpc.frozen_override = s.struct_hash("AcceptedPackageV1", s.PACKAGE, k.normalized(accepted))
        self.assertTrue(self.permit()["state"]["control_work"])

    def test_open_and_repair_use_independent_cast_calldata_and_zero_proof_wrapper(self):
        for action in ("open", "repair"):
            result = k.package_call(self.config, self.rpc, self.request, self.f["evidence"], self.f["fri_payload"], 1000, action)
            accepted = k.normalized(k.prepare(self.config["settings"], self.request, self.f["evidence"], self.f["fri_payload"])["accepted_package"])
            expected = s.cast("calldata", action + "Package(" + abi_type(s.PACKAGE) + ")",
                              cast_value(struct_values(s.PACKAGE, accepted)))
            self.assertEqual(result["transaction"]["data"], expected)
            self.assertFalse(result["broadcast"])

    def test_bootstrap_frozen_range_requires_local_sequencer(self):
        proposal = self.request["proposal"]
        proposal.update(mode="bootstrap", candidate=None, candidate_proof=[])
        proposal["accepted_package"].update(rosterRoot=s.ZERO, turn=0, wrapper=s.ZERO_ADDRESS, wrapperBeneficiary=s.ZERO_ADDRESS)
        self.config, self.request, item = setup(self.f)
        self.config["settings"]["coordinator"] = s.ZERO_ADDRESS
        self.config["policy"]["coordinator_code_hash"] = s.ZERO
        self.rpc = NativeRpc(self.f, self.config, item)
        self.assertEqual(self.permit()["state"]["deadline"], 1500)
        self.rpc.frozen_bootstrap["batchTo"] = 3
        with self.assertRaisesRegex(s.Error, "bootstrap_package_repaired"):
            self.permit()

    def test_prefix_witness_binds_native_preimages_and_is_simulated(self):
        witness = {"batch_item_preimages": [["0x0102"], ["0x0304"]], "left_path": [], "right_path": []}
        for entry, batch in zip(witness["batch_item_preimages"], self.f["evidence"]["batches"]):
            digest = s.keccak(s.raw_hex(s.keccak(b"")) + s.raw_hex(s.keccak(s.raw_hex(entry[0]))))
            batch["output"]["priorityOperationsHash"] = batch["stored"]["priorityOperationsHash"] = digest
            batch["stored"]["commitment"] = s.output_hash(batch["output"])
        result = k.prefix_call(self.config, self.rpc, self.f["evidence"], witness, 1000)
        hashes = [s.keccak(s.raw_hex(entry[0])) for entry in witness["batch_item_preimages"]]
        work_id = s.keccak(s.raw_hex(self.request["proposal"]["accepted_package"]["parent"]) + s.word(1))
        expected = s.cast("calldata", "publishPrefixWitness(bytes32,uint256[],bytes32[],bytes32[],bytes32[])",
                          work_id, "[1,1]", cast_value(hashes), "[]", "[]")
        self.assertEqual(result["transaction"]["data"], expected)
        witness["batch_item_preimages"][0][0] = "0xff"
        with self.assertRaisesRegex(s.Error, "priority_preimage_hash_mismatch"):
            k.prefix_call(self.config, self.rpc, self.f["evidence"], witness, 1000)

    def test_fixed_maintenance_calls_never_sign_or_broadcast(self):
        result = k.maintenance(self.config, self.rpc, "prepare-roster", {"period": 5}, 1000)
        self.assertEqual(result["transaction"]["data"], s.cast("calldata", "prepareRosterDraw(uint64)", "5"))
        self.assertFalse(any(method.startswith("eth_send") or "sign" in method for method, _ in self.rpc.calls))

    def test_status_exposes_frozen_control_and_opening_roster_before_dispatch(self):
        self.rpc.control = True
        result = k.status(self.config, self.rpc, 1000)
        self.assertTrue(result["control_work"])
        self.assertEqual(result["frozen_package"]["period"], 5)
        self.assertTrue(result["package_open"])
        self.assertEqual(result["current_turn"], 3)
        self.assertEqual(result["selected_wrapper_index"], 0)
        self.assertEqual(result["turn_deadline"], 1200)
        self.assertEqual(result["roster"]["count"], 1)
        self.assertIsNone(result["local_operator_selected"])
        self.rpc.opened = False
        result = k.status(self.config, self.rpc, 1000)
        self.assertTrue(result["control_work"])
        self.assertEqual(result["opening_roster"]["period"], 5)
        self.assertIsNone(result["frozen_package"])
        self.assertFalse(result["package_open"])
        self.assertIsNone(result["current_turn"])
        self.assertIsNone(result["selected_wrapper_index"])

    def roster_fixture(self):
        first = self.f["proposal"]["candidate"]
        second = {"index": 1, "account": a(92), "operator": a(95), "beneficiary": a(96)}
        artifact = roster.construct(5, [first, second])
        self.f["proposal"]["accepted_package"]["rosterRoot"] = artifact["root"]
        self.f["proposal"]["candidate_proof"] = artifact["proofs"][0]
        self.config, self.request, item = setup(self.f)
        self.rpc = NativeRpc(self.f, self.config, item)
        self.rpc.roster_count = artifact["count"]
        return artifact

    def test_status_authenticates_complete_roster_and_detects_turn_rollover(self):
        artifact = self.roster_fixture()
        state = k.status(self.config, self.rpc, 1000, artifact)
        self.assertTrue(state["local_operator_selected"])
        self.assertEqual(state["selected_candidate"], artifact["candidates"][0])
        self.rpc.turn, self.rpc.selected, self.rpc.deadline = 4, 1, 1400
        state = k.status(self.config, self.rpc, 1000, artifact)
        self.assertFalse(state["local_operator_selected"])
        self.assertEqual(state["selected_candidate"], artifact["candidates"][1])
        self.assertEqual(state["current_turn"], 4)
        self.assertEqual(state["turn_deadline"], 1400)

    def test_status_rejects_stale_wrong_and_incomplete_rosters(self):
        artifact = self.roster_fixture()
        for field, value, reason in (("period", 6, "roster_context_mismatch"),
            ("root", h(201), "roster_context_mismatch"), ("count", 1, "roster_context_mismatch"),
            ("candidates", artifact["candidates"][:1], "incomplete_roster"),
            ("proofs", artifact["proofs"][:1], "incomplete_roster_proofs")):
            with self.subTest(field=field), self.assertRaisesRegex(s.Error, reason):
                k.status(self.config, self.rpc, 1000, {**artifact, field: value})

    def test_status_pins_configuration_and_canonical_head(self):
        gate, coordinator = self.config["settings"]["proof_gate"], self.config["settings"]["coordinator"]
        for target, signature, wrong, reason in (
            (gate, "sequencer()", a(220), "gate_configuration_changed"),
            (coordinator, "acceptanceGate()", a(220), "coordinator_configuration_changed"),
            (coordinator, "childChainId()", 1, "coordinator_configuration_changed"),
            (coordinator, "childChainAddress()", a(220), "coordinator_configuration_changed"),
            (coordinator, "sequencer()", a(220), "coordinator_configuration_changed"),
            (coordinator, "policyHash()", h(220), "coordinator_configuration_changed"),
            (coordinator, "productionVkHash()", h(220), "coordinator_configuration_changed"),
            (coordinator, "acceptedParent()", h(220), "coordinator_configuration_changed"),
            (coordinator, "nextBatch()", 3, "coordinator_configuration_changed"),
            (coordinator, "frozenPackageHash()", h(220), "frozen_package_hash_mismatch"),
            (coordinator, "selectedWrapperIndex()", 1, "selected_wrapper_out_of_range")):
            with self.subTest(signature=signature):
                self.rpc.status_overrides = {(target, signature): wrong}
                with self.assertRaisesRegex(s.Error, reason):
                    k.status(self.config, self.rpc, 1000)
        self.rpc.status_overrides = {}
        self.rpc.reorg_anchor = True
        with self.assertRaisesRegex(s.Error, "rpc_reorg_or_chain_change"):
            k.status(self.config, self.rpc, 1000)

    def test_rebind_changes_only_current_endorsement_fields(self):
        artifact = self.roster_fixture()
        original = copy.deepcopy(self.request)
        self.rpc.turn, self.rpc.selected, self.rpc.deadline = 4, 1, 1400
        state = k.status(self.config, self.rpc, 1000, artifact)
        rebound = k.rebind_request(self.config["settings"], self.request, state, artifact)
        expected = copy.deepcopy(original)
        expected["proposal"].update(candidate=artifact["candidates"][1], candidate_proof=artifact["proofs"][1])
        expected["proposal"]["accepted_package"].update(turn=4, wrapper=a(95), wrapperBeneficiary=a(96))
        self.assertEqual(rebound, expected)
        self.assertEqual(self.request, original)
        self.assertEqual(k.normalized(k.prepare(self.config["settings"], rebound, self.f["evidence"], self.f["fri_payload"])
                                     ["accepted_package"]), state["frozen_package"])

    def test_rebind_requires_updated_artifacts_after_package_repair(self):
        artifact = self.roster_fixture()
        self.rpc.item["sidecar"]["accepted_package"]["batchTo"] = 3
        state = k.status(self.config, self.rpc, 1000, artifact)
        with self.assertRaisesRegex(s.Error, "request_frozen_commitments_changed"):
            k.rebind_request(self.config["settings"], self.request, state, artifact)
        self.request["proposal"]["accepted_package"]["batchTo"] = 3
        self.assertEqual(k.rebind_request(self.config["settings"], self.request, state, artifact), self.request)
        self.request["manifest"]["payload"]["period"] = 6
        with self.assertRaisesRegex(s.Error, "request_frozen_commitments_changed"):
            k.rebind_request(self.config["settings"], self.request, state, artifact)

    def test_rebind_is_not_a_permit_and_cannot_bypass_a_later_turn(self):
        artifact = self.roster_fixture()
        state = k.status(self.config, self.rpc, 1000, artifact)
        with self.assertRaisesRegex(s.Error, "complete_roster_required"):
            k.rebind_request(self.config["settings"], self.request, state)
        rebound = k.rebind_request(self.config["settings"], self.request, state, artifact)
        self.rpc.turn, self.rpc.selected, self.rpc.deadline = 4, 1, 1400
        with self.assertRaisesRegex(s.Error, "stale_wrapper_turn"):
            k.permit(self.config, self.rpc, rebound, self.f["evidence"], self.f["fri_payload"], 1000, 50)

    def test_closed_and_activation_pending_states_cannot_be_rebound(self):
        artifact = self.roster_fixture()
        self.rpc.opened = False
        state = k.status(self.config, self.rpc, 1000)
        with self.assertRaisesRegex(s.Error, "package_not_open"):
            k.rebind_request(self.config["settings"], self.request, state, artifact)
        gate = self.config["settings"]["proof_gate"]
        self.rpc.status_overrides = {(gate, "serviceActive()"): False}
        state = k.status(self.config, self.rpc, 1000)
        self.assertEqual(state["mode"], "activation_pending")
        self.assertIsNone(state["local_operator_selected"])
        with self.assertRaisesRegex(s.Error, "request_phase_mismatch"):
            k.rebind_request(self.config["settings"], self.request, state, artifact)

    def test_compute_permit_rechecks_coordinator_identity(self):
        coordinator = self.config["settings"]["coordinator"]
        for signature, wrong in (("childChainId()", 1), ("childChainAddress()", a(220)), ("sequencer()", a(220))):
            with self.subTest(signature=signature):
                self.rpc.status_overrides = {(coordinator, signature): wrong}
                with self.assertRaisesRegex(s.Error, "coordinator_configuration_changed"):
                    self.permit()

    def test_rebind_control_does_not_invent_reward_duties(self):
        artifact = self.roster_fixture()
        self.rpc.control = True
        state = k.status(self.config, self.rpc, 1000, artifact)
        with self.assertRaisesRegex(s.Error, "control_work_must_have_empty_report"):
            k.rebind_request(self.config["settings"], self.request, state, artifact)
        self.request["duties"] = []
        self.rpc.item["sidecar"]["accepted_package"]["reportHash"] = s.report_hash([])
        state = k.status(self.config, self.rpc, 1000, artifact)
        self.assertEqual(k.rebind_request(self.config["settings"], self.request, state, artifact)["duties"], [])

    def test_bootstrap_status_and_rebind_require_frozen_zero_wrapper(self):
        self.f["proposal"].update(mode="bootstrap", candidate=None, candidate_proof=[])
        self.f["proposal"]["accepted_package"].update(rosterRoot=s.ZERO, turn=0, wrapper=s.ZERO_ADDRESS,
                                                    wrapperBeneficiary=s.ZERO_ADDRESS)
        self.f["settings"]["coordinator"] = s.ZERO_ADDRESS
        self.config, self.request, item = setup(self.f)
        self.config["policy"]["coordinator_code_hash"] = s.ZERO
        self.rpc = NativeRpc(self.f, self.config, item)
        state = k.status(self.config, self.rpc, 1000)
        self.assertTrue(state["package_open"])
        self.assertTrue(state["local_operator_selected"])
        self.assertIsNone(state["roster"])
        self.assertEqual(k.rebind_request(self.config["settings"], self.request, state), self.request)
        self.request["proposal"]["accepted_package"]["turn"] = 1
        with self.assertRaisesRegex(s.Error, "invalid_bootstrap_compute"):
            k.rebind_request(self.config["settings"], self.request, state)
        self.rpc.guard_work_missing = True
        state = k.status(self.config, self.rpc, 1000)
        self.assertFalse(state["package_open"])
        self.assertIsNone(state["frozen_package"])

    def test_root_draw_and_checkpoint_call_encodings_match_cast(self):
        target = self.config["settings"]["coordinator"]
        common = {"target": target, "code_hash": self.config["policy"]["coordinator_code_hash"], "value": "0x0"}
        args = {**common, "commitment": h(88), "batch_number": "0x5", "index": "0x2", "tx_number_in_batch": 3,
                "proof": [h(90), h(91)]}
        result = k.maintenance(self.config, self.rpc, "request-draw", args, 1000)
        self.assertEqual(result["transaction"]["data"], s.cast("calldata", "requestDraw(bytes32,uint256,uint256,uint16,bytes32[])",
                         h(88), "5", "2", "3", cast_value(args["proof"])))
        args = {**common, "value": "0x1", "gas_limit": "0xf4240", "gas_per_pubdata_byte_limit": "0x320", "refund_recipient": a(77)}
        result = k.maintenance(self.config, self.rpc, "relay-checkpoint", args, 1000)
        self.assertEqual(result["transaction"]["data"], s.cast("calldata", "relayCheckpoint(uint256,uint256,address)", "1000000", "800", a(77)))
        self.assertEqual(result["transaction"]["value"], "0x1")


if __name__ == "__main__":
    unittest.main()
