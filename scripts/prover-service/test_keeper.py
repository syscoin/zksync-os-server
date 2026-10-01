import copy
import unittest

import keeper as k
import relay as r
import service as s
from test_relay import FakeRpc, relay_policy
from test_service import fixture, prepare, h, a, abi_type, cast_value, struct_values


def setup(f):
    settings = f["settings"]
    prepared = prepare(f)
    item = {"mode": prepared["mode"], "sidecar": prepared["sidecar"],
            "package_hash": prepared["sequencer_request"]["struct_hash"], "digest": prepared["sequencer_request"]["digest"],
            "calldata": "0xabcdef01"}
    config = {"schema_version": 1, "lane": "child", "rpc_url": "http://127.0.0.1:8545", "settings": settings,
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
            values = {"getChainId()": s.uint(self.settings["execution_chain_id"]), "productionVerifier()": a(130),
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

    def permit(self, runtime=50, now=1000):
        return k.permit(self.config, self.rpc, self.request, self.f["evidence"], self.f["fri_payload"], now, runtime)

    def test_exact_native_payload_and_canonical_range_create_revalidatable_permit(self):
        value = self.permit()
        self.assertEqual(value["state"]["deadline"], 1200)
        self.assertEqual(value["payload_sha256"], k.sha(self.f["fri_payload"]))
        self.assertEqual(k.validate_permit(self.config, self.rpc, value, self.f["evidence"], self.f["fri_payload"], 1001, 50), value)

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
        self.rpc.opened = False
        result = k.status(self.config, self.rpc, 1000)
        self.assertTrue(result["control_work"])
        self.assertEqual(result["opening_roster"]["period"], 5)
        self.assertIsNone(result["frozen_package"])

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
