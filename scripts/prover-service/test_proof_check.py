import base64
import copy
import unittest
from unittest.mock import patch

import proof_check as p
import relay as r
import service as s
from test_keeper import NativeRpc, setup
from test_service import a, cast_value, fixture, h


class ProofRpc(NativeRpc):
    def __init__(self, f, config, item):
        super().__init__(f, config, item)
        self.observed, self.verifications = [], []
        self.verification_result = "0x" + s.word(1).hex()
        self.after_verify = None
        self.verification_error = None

    def call(self, method, params):
        self.observed.append((method, copy.deepcopy(params)))
        if method == "eth_call" and params[0]["data"][:10] == "0x" + r.selector(p.VERIFY_SIGNATURE).hex():
            self.verifications.append(copy.deepcopy(params))
            if self.verification_error:
                if self.after_verify:
                    self.after_verify()
                raise self.verification_error
            if self.after_verify:
                self.after_verify()
            return self.verification_result
        return super().call(method, params)


class ProofCheckTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.base = fixture()
        cls.base_config, _, cls.base_item = setup(cls.base)

    def setUp(self):
        self.f = copy.deepcopy(self.base)
        self.config = copy.deepcopy(self.base_config)
        self.rpc = ProofRpc(self.f, self.config, copy.deepcopy(self.base_item))

    def verify(self, now=1000):
        return p.verify_snark(self.config, self.rpc, self.f["evidence"], self.f["fri_payload"], self.f["snark"], now)

    def test_canonical_verification_matches_independent_native_abi_and_binds_artifacts(self):
        result = self.verify()
        statements = [entry["statement"] for entry in s.validate_evidence(self.f["settings"], self.f["evidence"]).values()]
        raw = s.decode_proof(self.f["snark"]["proof"])
        proof_words = [0x802, 0] + [int.from_bytes(raw[i:i + 32], "big") for i in range(0, len(raw), 32)]
        expected = s.cast("calldata", p.VERIFY_SIGNATURE,
                          cast_value([int(value, 16) for value in statements]), cast_value(proof_words))
        self.assertEqual(self.rpc.verifications, [[{"to": a(130), "data": expected},
                         {"blockHash": self.rpc.head["hash"], "requireCanonical": True}]])
        self.assertEqual(result["statements"], statements)
        self.assertEqual(result["proof_data_hash"], s.keccak(s.proof_data(self.f["evidence"], raw)))
        self.assertEqual(result["fri_artifact_hashes"], [duty["friProofHash"] for duty in self.f["duties"]])
        self.assertEqual(result["anchor"], self.rpc.head)
        self.assertEqual(result["configuration_sha256"], p.sha(self.config))
        self.assertEqual(result["proof_sha256"], p.sha(self.f["snark"]))
        self.assertEqual(result["payload_sha256"], p.sha(self.f["fri_payload"]))
        unsigned = {key: value for key, value in result.items() if key != "attestation_sha256"}
        self.assertEqual(result["attestation_sha256"], p.sha(unsigned))
        self.assertEqual(self.rpc.observed[-2:], [("eth_getBlockByNumber", [self.rpc.head["number"], False]),
                                                ("eth_chainId", [])])
        for method, params in self.rpc.observed:
            self.assertIn(method, ("eth_chainId", "eth_getBlockByNumber", "eth_getCode", "eth_call"))
            if method in ("eth_call", "eth_getCode"):
                self.assertEqual(params[1], {"blockHash": self.rpc.head["hash"], "requireCanonical": True})

    def test_invalid_or_noncanonical_verifier_results_do_not_attest(self):
        for result, reason in (("0x" + s.word(0).hex(), "native_snark_rejected"),
                               ("0x" + s.word(2).hex(), "invalid_verifier_boolean"),
                               ("0x01", "incorrect_hex_size"),
                               ("0x" + (s.word(1) + s.word(0)).hex(), "incorrect_hex_size")):
            with self.subTest(result=result):
                self.rpc.verification_result = result
                with self.assertRaisesRegex(s.Error, reason):
                    self.verify()

    def test_reorg_after_verifier_response_is_not_classified_as_bad_proof(self):
        for word in (0, 1):
            with self.subTest(word=word):
                self.rpc.reorg_anchor = False
                self.rpc.verification_result = "0x" + s.word(word).hex()
                self.rpc.after_verify = lambda: setattr(self.rpc, "reorg_anchor", True)
                with self.assertRaisesRegex(s.Error, "rpc_reorg_or_chain_change"):
                    self.verify()

    def test_chain_identity_change_after_verification_does_not_attest(self):
        self.rpc.after_verify = lambda: setattr(self.rpc, "chain_override", "0x1")
        with self.assertRaisesRegex(s.Error, "rpc_reorg_or_chain_change"):
            self.verify()

    def test_rpc_failure_or_revert_never_creates_an_attestation(self):
        for error in (s.Error("rpc_transport_failure"), r.RpcError(3)):
            with self.subTest(error=error):
                self.rpc.verification_error = error
                with self.assertRaises(s.Error):
                    self.verify()

    def test_exact_native_rejections_are_canonical_negatives(self):
        reasons = ("loadProof: Proof is invalid", "invalid quotient evaluation",
                   "invalid vanishing polynomial", "finalPairing: pairing failure",
                   "pointNegate: invalid point")
        encodings = {s.cast("calldata", "Error(string)", reason) for reason in reasons}
        self.assertEqual(encodings, p.NATIVE_PROOF_REJECTIONS)
        for data in encodings:
            with self.subTest(data=data):
                self.rpc.verification_error = r.RpcError(3, data)
                with self.assertRaisesRegex(s.Error, "^native_snark_rejected$"):
                    self.verify()
                self.assertEqual(self.rpc.observed[-2:], [
                    ("eth_getBlockByNumber", [self.rpc.head["number"], False]), ("eth_chainId", [])])

    def test_unknown_or_infrastructure_reverts_remain_uncertain(self):
        rejection = next(iter(p.NATIVE_PROOF_REJECTIONS))
        errors = [r.RpcError(3), r.RpcError(3.0, rejection), r.RpcError(-32000, rejection), r.RpcError(3, "0x"),
                  r.RpcError(3, rejection[:-2]), r.RpcError(3, rejection + "00"),
                  r.RpcError(3, {"data": rejection}), r.RpcError(3, "not hex")]
        for reason in ("modexp precompile failed", "pointMulIntoDest: ecMul failed",
                       "pointAddIntoDest: ecAdd failed", "pointSubAssign: ecAdd failed",
                       "pointAddAssign: ecAdd failed", "pointMulAndAddIntoDest",
                       "finalPairing: precompile failure", "unknown verifier error"):
            errors.append(r.RpcError(3, s.cast("calldata", "Error(string)", reason)))
        for error in errors:
            with self.subTest(data=error.data):
                self.rpc.verification_error = error
                with self.assertRaises(r.RpcError) as caught:
                    self.verify()
                self.assertIs(caught.exception, error)

    def test_rejected_proof_still_requires_canonical_chain_and_anchor(self):
        self.rpc.verification_error = r.RpcError(3, next(iter(p.NATIVE_PROOF_REJECTIONS)))
        for field, value in (("reorg_anchor", True), ("chain_override", "0x1")):
            with self.subTest(field=field):
                self.rpc.after_verify = lambda: setattr(self.rpc, field, value)
                with self.assertRaisesRegex(s.Error, "rpc_reorg_or_chain_change"):
                    self.verify()
                setattr(self.rpc, field, False if field == "reorg_anchor" else None)

    def test_rejection_outside_final_verifier_call_is_not_terminal(self):
        error = r.RpcError(3, next(iter(p.NATIVE_PROOF_REJECTIONS)))
        with patch.object(p.k, "base_context", side_effect=error), self.assertRaises(r.RpcError) as caught:
            self.verify()
        self.assertIs(caught.exception, error)
        self.assertEqual(self.rpc.verifications, [])

    def test_rejected_proof_still_requires_deadline_and_freshness(self):
        self.rpc.verification_error = r.RpcError(3, next(iter(p.NATIVE_PROOF_REJECTIONS)))
        for elapsed, age, reason in ((16, 120, "proof_verification_timeout"),
                                     (2, 1, "stale_verification_anchor")):
            with self.subTest(reason=reason), patch.object(p.time, "monotonic", return_value=0) as clock:
                self.config["policy"]["max_head_age_seconds"] = age
                self.rpc.after_verify = lambda: setattr(clock, "return_value", elapsed)
                with self.assertRaisesRegex(s.Error, reason):
                    self.verify()

    def test_native_binding_failures_precede_verifier_call(self):
        for field, value, reason in (("bad_stored", True, "native_stored_batch_changed"),
                                    ("bad_role", True, "native_prover_authority_changed"),
                                    ("bad_code", True, "gate_code_changed"),
                                    ("native_proved", 1, "native_proved_frontier_changed")):
            with self.subTest(field=field):
                before = getattr(self.rpc, field)
                setattr(self.rpc, field, value)
                with self.assertRaisesRegex(s.Error, reason):
                    self.verify()
                self.assertEqual(self.rpc.verifications, [])
                setattr(self.rpc, field, before)

    def test_testnet_wrong_vk_wrong_program_and_wrong_verifier_refuse(self):
        for address, signature, value, reason in (
                (a(130), "IS_TESTNET_VERIFIER()", True, "native_verifier_changed"),
                (a(130), "verificationKeyHash(uint256)", h(250), "native_verifier_changed"),
                (self.f["settings"]["chain_address"], "getVerifier()", a(250), "native_verifier_changed"),
                (self.f["settings"]["chain_address"], "getChainId()", 999, "native_protocol_changed")):
            with self.subTest(signature=signature):
                self.rpc.status_overrides = {(address, signature): value}
                with self.assertRaisesRegex(s.Error, reason):
                    self.verify()
                self.assertEqual(self.rpc.verifications, [])

    def test_output_preimage_tampering_refuses_before_rpc(self):
        self.f["evidence"]["batches"][0]["output"]["l2TxCount"] = "0x123"
        with self.assertRaisesRegex(s.Error, "output_commitment_mismatch"):
            self.verify()
        self.assertEqual(self.rpc.observed, [])

    def test_one_batch_and_more_than_one_hundred_refuse_before_rpc(self):
        for batches in (self.f["evidence"]["batches"][:1], self.f["evidence"]["batches"] * 51):
            with self.subTest(count=len(batches)):
                self.f["evidence"]["batches"] = batches
                with self.assertRaisesRegex(s.Error, "invalid_(snark_verification|evidence)_range"):
                    self.verify()
                self.assertEqual(self.rpc.observed, [])

    def test_stripped_payloads_require_exact_integer_ranges_vk_and_no_private_lease(self):
        for field in ("fri_payload", "snark"):
            original = copy.deepcopy(self.f[field])
            for changes in ({"from_batch_number": 2}, {"to_batch_number": 3}, {"vk_hash": h(251)},
                            {"from_batch_number": True}, {"lease_token": h(252)}):
                with self.subTest(field=field, changes=changes):
                    self.f[field] = {**original, **changes}
                    with self.assertRaises(s.Error):
                        self.verify()
                    self.assertEqual(self.rpc.observed, [])
            self.f[field] = original

    def test_snark_proof_requires_exact_canonical_native_44_words(self):
        raw = s.decode_proof(self.f["snark"]["proof"])
        for proof in (base64.b64encode(raw[:-1]).decode(), base64.b64encode(raw + b"\0").decode(),
                      base64.b64encode(raw).decode() + "\n", "!" * len(self.f["snark"]["proof"])):
            with self.subTest(length=len(proof)):
                self.f["snark"]["proof"] = proof
                with self.assertRaises(s.Error):
                    self.verify()
                self.assertEqual(self.rpc.observed, [])

    def test_fri_bounds_count_and_canonical_base64_are_checked_without_claiming_crypto_verification(self):
        original = copy.deepcopy(self.f["fri_payload"])
        for proofs in ([], original["fri_proofs"][:1], ["", original["fri_proofs"][1]],
                       [original["fri_proofs"][0] + "\n", original["fri_proofs"][1]]):
            self.f["fri_payload"]["fri_proofs"] = proofs
            with self.assertRaises(s.Error):
                self.verify()
            self.assertEqual(self.rpc.observed, [])
        self.f["fri_payload"] = original
        with patch.object(p, "MAX_PAYLOAD", 4096):
            with self.assertRaisesRegex(s.Error, "fri_payload_too_large"):
                self.verify()
        with patch.object(p, "MAX_FRI_ENCODING", 1):
            with self.assertRaisesRegex(s.Error, "fri_payload_too_large"):
                self.verify()
        self.assertEqual(self.rpc.observed, [])

    def test_changed_inputs_during_rpc_cannot_receive_original_verification(self):
        self.rpc.after_verify = lambda: self.f["snark"].update(proof=base64.b64encode(b"\x11" * p.SNARK_BYTES).decode())
        with self.assertRaisesRegex(s.Error, "proof_verification_inputs_changed"):
            self.verify()

    def test_one_deadline_bounds_the_entire_rpc_sequence_and_late_response(self):
        class ImmediateRpc:
            def call(self, method, params):
                return True
        with patch.object(p.time, "monotonic", side_effect=[10, 14, 15]):
            bounded = p.DeadlineRpc(ImmediateRpc(), 5)
            with self.assertRaisesRegex(s.Error, "proof_verification_timeout"):
                bounded.call("eth_chainId", [])
        with patch.object(p.time, "monotonic", side_effect=[10, 15]):
            bounded = p.DeadlineRpc(ImmediateRpc(), 5)
            with self.assertRaisesRegex(s.Error, "proof_verification_timeout"):
                bounded.call("eth_chainId", [])

    def test_stale_or_future_head_and_nonfinite_time_refuse(self):
        for now, reason in ((999, "stale_or_future_rpc_head"), (1121, "stale_or_future_rpc_head"),
                            (float("nan"), "invalid_verification_time"), (True, "invalid_verification_time")):
            with self.subTest(now=now):
                with self.assertRaisesRegex(s.Error, reason):
                    self.verify(now)


if __name__ == "__main__":
    unittest.main()
