import base64
import copy
import hashlib
import json
from pathlib import Path
import subprocess
import unittest

import service as s


def h(byte):
    return "0x" + bytes([byte]).hex() * 32


def a(byte):
    return "0x" + bytes([byte]).hex() * 20


# Public, deliberately trivial test keys. Production code never accepts a signing key.
KEYS = {role: "0x" + (index + 1).to_bytes(32, "big").hex()
        for index, role in enumerate(("account", "operator", "sequencer", "wrapper"))}


def sign(request, role):
    return s.cast("wallet", "sign", "--private-key", KEYS[role], "--no-hash", request["digest"])


def cast_value(value):
    if isinstance(value, list):
        return "[" + ",".join(cast_value(v) for v in value) + "]"
    if isinstance(value, tuple):
        return "(" + ",".join(cast_value(v) for v in value) + ")"
    return str(value)


def abi_type(fields):
    return "(" + ",".join(t for _, t in fields) + ")"


def struct_values(fields, value):
    return tuple(value[k] for k, _ in fields)


def fixture(execution_chain_id="0x23a", registry_chain_id="0x23a", settlement_chain_id="0x1644", chain_address=None):
    addresses = {role: s.cast("wallet", "address", "--private-key", key).lower() for role, key in KEYS.items()}
    settings = {"schema_version": 1, "execution_chain_id": execution_chain_id, "registry_chain_id": registry_chain_id,
                "chain_address": chain_address or a(17), "settlement_chain_id": settlement_chain_id,
                "registry": a(18), "coordinator": a(19),
                "proof_gate": a(20), "policy_hash": h(21), "vk_hash": h(22),
                "sequencer": addresses["sequencer"], "duties_per_round": 4}
    s.config(settings)
    subscription = {"account": addresses["account"], "operator": addresses["operator"],
                    "beneficiary": a(23), "sequencer": addresses["sequencer"],
                    "firstPeriod": 5, "lastPeriod": 8, "nonce": 7, "services": 3}
    request = s.subscription_request(settings, subscription)
    subscriptions = [{"subscription": subscription, "signature": sign(request, "account")}]
    previous = {"batchNumber": 0, "batchHash": h(30), "indexRepeatedStorageChanges": 0,
                "numberOfLayer1Txs": "0x0", "priorityOperationsHash": h(31),
                "dependencyRootsRollingHash": h(32), "l2LogsTreeRoot": h(33), "timestamp": "0x1",
                "commitment": h(34)}
    batches = []
    for number in (1, 2):
        output = {"firstBlockTimestamp": 100 + number, "lastBlockTimestamp": 110 + number, "daScheme": "0x0",
                  "daCommitment": h(40 + number), "l1TxCount": "0x1", "l2TxCount": hex(number + 2),
                  "priorityOperationsHash": h(45), "l2LogsRoot": h(46), "upgradeTxHash": s.ZERO,
                  "dependencyRootsRollingHash": h(47), "settlementChainId": settings["settlement_chain_id"],
                  "edgeDARefsRoot": h(48)}
        stored = {"batchNumber": number, "batchHash": h(50 + number), "indexRepeatedStorageChanges": 9,
                  "numberOfLayer1Txs": output["l1TxCount"], "priorityOperationsHash": output["priorityOperationsHash"],
                  "dependencyRootsRollingHash": output["dependencyRootsRollingHash"], "l2LogsTreeRoot": output["l2LogsRoot"],
                  "timestamp": hex(output["lastBlockTimestamp"]), "commitment": s.output_hash(output)}
        batches.append({"stored": stored, "output": output})
    evidence = {"schema_version": 1, "chain_id": settings["execution_chain_id"], "chain_address": settings["chain_address"],
                "settlement_chain_id": settings["settlement_chain_id"], "protocol_version": 32, "vk_hash": settings["vk_hash"],
                "previous_batch": previous, "batches": batches}
    statements = s.validate_evidence(settings, evidence)
    payload = {"schema_version": 1, "chain_id": settings["execution_chain_id"], "chain_address": settings["chain_address"],
               "sequencer": settings["sequencer"], "period": 5, "previous_cursor": h(60),
               "subscription_snapshot_hash": s.keccak(s.canonical(subscriptions)), "assignments": [], "retries": []}
    proofs, authorities = [], []
    for number in (1, 2):
        token = h(70 + number)
        proof = {"batch_number": number, "vk_hash": settings["vk_hash"],
                 "proof": base64.b64encode(bytes([number]) * 96).decode()}
        authority = {"schema_version": 1, "stage": "FRI", "status": "accepted", "vk_hash": settings["vk_hash"],
                     "bounds": {"batch_number": number}, "lease_token": token,
                     "submission_sha256": hashlib.sha256(s.canonical({**proof, "lease_token": token})).hexdigest()}
        assignment = {"assignment_id": h(80 + number), "account": subscription["account"],
                      "subscription_hash": request["struct_hash"], "batch_number": number,
                      "statement_hash": statements[number]["statement"], "lease_commitment": s.keccak(s.raw_hex(token)),
                      "attempt": 1, "slot": number - 1, "period": 5}
        payload["assignments"].append(assignment)
        proofs.append(proof)
        authorities.append(authority)
    manifest = {"payload": payload, "sequencer_signature": sign(s.manifest_request(settings, payload), "sequencer")}
    duties = []
    for authority, proof in zip(authorities, proofs):
        duty_request = s.prepare_duty(settings, evidence, manifest, subscriptions, authority, proof)
        duties.append({**duty_request["typed_data"]["message"], "operatorSignature": sign(duty_request, "operator")})
    candidate = {"index": 0, "account": a(90), "operator": addresses["wrapper"], "beneficiary": a(91)}
    accepted = {"domainVersion": 1, "policyHash": settings["policy_hash"], "chainId": settings["execution_chain_id"],
                "chainAddress": settings["chain_address"], "parent": h(92), "batchFrom": 1, "batchTo": 2,
                "protocolVersion": 32, "vkHash": settings["vk_hash"], "period": 5,
                "rosterRoot": s.wrapper_leaf(candidate), "turn": 3, "sequencer": settings["sequencer"],
                "sequencerBeneficiary": a(93), "wrapper": candidate["operator"], "wrapperBeneficiary": candidate["beneficiary"]}
    proposal = {"mode": "service", "accepted_package": accepted, "candidate": candidate, "candidate_proof": []}
    snark = {"from_batch_number": 1, "to_batch_number": 2, "vk_hash": settings["vk_hash"],
             "proof": base64.b64encode(bytes(range(32)) * 44).decode()}
    fri_payload = {"from_batch_number": 1, "to_batch_number": 2, "vk_hash": settings["vk_hash"],
                   "fri_proofs": [proof["proof"] for proof in proofs]}
    return {"settings": settings, "subscription": subscription, "subscriptions": subscriptions, "evidence": evidence,
            "manifest": manifest, "proofs": proofs, "authorities": authorities, "duties": duties, "proposal": proposal,
            "snark": snark, "fri_payload": fri_payload}


def prepare(f):
    return s.prepare_package(f["settings"], f["evidence"], f["manifest"], f["subscriptions"], f["duties"],
                             f["proposal"], f["snark"], f["fri_payload"])


class ServiceTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.base = fixture()
        cls.prepared = prepare(cls.base)

    def setUp(self):
        self.f = copy.deepcopy(self.base)

    def test_static_hashes_and_typed_data_match_cast(self):
        cases = [("ProverSubscriptionV1", s.SUBSCRIPTION, self.f["subscription"]),
                 ("DutySuccessV1", s.DUTY, {k: self.f["duties"][0][k] for k, _ in s.DUTY}),
                 ("AcceptedPackageV1", s.PACKAGE, self.prepared["sidecar"]["accepted_package"])]
        for name, fields, value in cases:
            encoded = s.cast("abi-encode", "f(bytes32," + abi_type(fields) + ")",
                             s.type_hash(s.schema_type(name, fields)), cast_value(struct_values(fields, value)))
            self.assertEqual(s.struct_hash(name, fields, value), s.cast("keccak", encoded))
        request = self.prepared["sequencer_request"]
        typed_sig = s.cast("wallet", "sign", "--private-key", KEYS["sequencer"], "--data", json.dumps(request["typed_data"]))
        self.assertEqual(typed_sig, sign(request, "sequencer"))

    def test_dynamic_report_encoding_matches_cast(self):
        fields = s.DUTY + [("operatorSignature", "bytes")]
        encoded = s.cast("abi-encode", "f(" + abi_type(fields) + "[])",
                         cast_value([struct_values(fields, duty) for duty in self.f["duties"]]))
        self.assertEqual(s.report_hash(self.f["duties"]), s.cast("keccak", encoded))
        self.assertEqual(s.report_hash([]), s.cast("keccak", s.cast("abi-encode", "f(" + abi_type(fields) + "[])", "[]")))

    def test_proof_data_native_v8_encoding_matches_cast(self):
        evidence = self.f["evidence"]
        proof = s.decode_proof(self.f["snark"]["proof"])
        words = [0x802, 0] + [int.from_bytes(proof[i:i + 32], "big") for i in range(0, len(proof), 32)]
        encoded = s.cast("abi-encode", "f(" + abi_type(s.STORED) + "," + abi_type(s.STORED) + "[],uint256[])",
                         cast_value(struct_values(s.STORED, evidence["previous_batch"])),
                         cast_value([struct_values(s.STORED, b["stored"]) for b in evidence["batches"]]), cast_value(words))
        self.assertEqual("0x01" + encoded[2:], self.prepared["proof_data"])

    def test_packed_output_hash_and_wrapper_leaf_match_cast(self):
        output = self.f["evidence"]["batches"][0]["output"]
        encoded = s.cast("abi-encode", "--packed", "f(" + ",".join(t for _, t in s.OUTPUT) + ")",
                         *[str(output[k]) for k, _ in s.OUTPUT])
        self.assertEqual(s.output_hash(output), s.cast("keccak", encoded))
        candidate = self.f["proposal"]["candidate"]
        encoded = s.cast("abi-encode", "f(" + abi_type(s.CANDIDATE) + ")", cast_value(struct_values(s.CANDIDATE, candidate)))
        self.assertEqual(s.wrapper_leaf(candidate), s.cast("keccak", s.cast("keccak", encoded)))

    def test_full_signed_sidecar_and_bootstrap_domains(self):
        prepared = copy.deepcopy(self.prepared)
        sidecar = s.complete_package(self.f["settings"], prepared, sign(prepared["sequencer_request"], "sequencer"),
                                     sign(prepared["wrapper_request"], "wrapper"))
        self.assertEqual(set(sidecar), {"accepted_package", "duties", "batch_outputs", "candidate", "candidate_proof",
                                        "sequencer_signature", "wrapper_signature"})
        self.f["proposal"].update(mode="bootstrap", candidate=None, candidate_proof=[])
        self.f["settings"]["coordinator"] = s.ZERO_ADDRESS
        s.config(self.f["settings"])
        self.f["proposal"]["accepted_package"].update(wrapper=s.ZERO_ADDRESS, wrapperBeneficiary=s.ZERO_ADDRESS,
                                                      turn=0, rosterRoot=s.ZERO)
        bootstrap = prepare(self.f)
        self.assertEqual(bootstrap["sequencer_request"]["typed_data"]["domain"]["name"], "ZkSysProofGate")
        self.assertNotEqual(prepared["sequencer_request"]["digest"], bootstrap["sequencer_request"]["digest"])
        s.complete_package(self.f["settings"], bootstrap, sign(bootstrap["sequencer_request"], "sequencer"), "0x")

    def test_empty_native_package_needs_no_fri_assignments_or_subscriptions(self):
        self.f["subscriptions"] = []
        self.f["duties"] = []
        payload = self.f["manifest"]["payload"]
        payload["subscription_snapshot_hash"] = s.keccak(s.canonical([]))
        payload["assignments"] = []
        payload["retries"] = []
        for batch in self.f["evidence"]["batches"]:
            batch["output"]["l1TxCount"] = "0x0"
            batch["output"]["l2TxCount"] = "0x0"
            batch["stored"]["numberOfLayer1Txs"] = "0x0"
            batch["stored"]["commitment"] = s.output_hash(batch["output"])
        self.f["manifest"]["sequencer_signature"] = sign(s.manifest_request(self.f["settings"], payload), "sequencer")
        prepared = prepare(self.f)
        self.assertEqual(prepared["sidecar"]["duties"], [])
        self.assertEqual(prepared["sidecar"]["accepted_package"]["reportHash"], s.report_hash([]))
        self.assertIsNotNone(prepared["wrapper_request"])
        s.complete_package(self.f["settings"], prepared, sign(prepared["sequencer_request"], "sequencer"),
                           sign(prepared["wrapper_request"], "wrapper"))
        with self.assertRaisesRegex(s.Error, "invalid_subscription_snapshot"):
            s.prepare_duty(self.f["settings"], self.f["evidence"], self.f["manifest"], [],
                           self.f["authorities"][0], self.f["proofs"][0])

    def test_rewarded_package_still_requires_subscriptions_and_assignments(self):
        self.f["subscriptions"] = []
        with self.assertRaisesRegex(s.Error, "invalid_subscription_snapshot"):
            prepare(self.f)
        self.f = copy.deepcopy(self.base)
        self.f["manifest"]["payload"]["assignments"] = []
        self.f["manifest"]["sequencer_signature"] = sign(
            s.manifest_request(self.f["settings"], self.f["manifest"]["payload"]), "sequencer")
        with self.assertRaisesRegex(s.Error, "invalid_assignments"):
            prepare(self.f)

    def test_missing_manifest_authentication_is_rejected(self):
        self.f["manifest"]["sequencer_signature"] = "0x"
        with self.assertRaisesRegex(s.Error, "eoa_signature_required"):
            prepare(self.f)

    def test_offered_duty_uses_same_digest_without_exported_lease_or_acceptance_claim(self):
        request = s.prepare_offered_duty(self.f["settings"], self.f["evidence"], self.f["manifest"],
                                         self.f["subscriptions"], self.f["proofs"][0])
        accepted = s.prepare_duty(self.f["settings"], self.f["evidence"], self.f["manifest"],
                                  self.f["subscriptions"], self.f["authorities"][0], self.f["proofs"][0])
        self.assertEqual(request["digest"], accepted["digest"])
        self.assertEqual(request["native_acceptance"], "pending")
        self.assertNotIn(self.f["authorities"][0]["lease_token"], json.dumps(request))
        self.f["manifest"]["sequencer_signature"] = "0x"
        with self.assertRaisesRegex(s.Error, "eoa_signature_required"):
            s.prepare_offered_duty(self.f["settings"], self.f["evidence"], self.f["manifest"],
                                   self.f["subscriptions"], self.f["proofs"][0])

    def test_current_lease_and_exact_native_accepted_bytes_are_required(self):
        for field, value, error in (("lease_token", h(99), "current_lease_binding_required"),
                                     ("status", "picked", "native_fri_acceptance_required"),
                                     ("submission_sha256", "00" * 32, "native_accepted_proof_bytes_mismatch")):
            with self.subTest(field=field):
                authority = {**self.f["authorities"][0], field: value}
                with self.assertRaisesRegex(s.Error, error):
                    s.prepare_duty(self.f["settings"], self.f["evidence"], self.f["manifest"], self.f["subscriptions"],
                                   authority, self.f["proofs"][0])

    def test_independent_en_output_mutation_and_statement_change_rejected(self):
        self.f["evidence"]["batches"][0]["output"]["l2TxCount"] = "0x100"
        with self.assertRaisesRegex(s.Error, "output_commitment_mismatch"):
            prepare(self.f)
        self.f = copy.deepcopy(self.base)
        self.f["evidence"]["previous_batch"]["batchHash"] = h(99)
        with self.assertRaisesRegex(s.Error, "assignment_statement_mismatch"):
            prepare(self.f)

    def test_replaced_fri_bytes_or_false_count_rejected(self):
        self.f["fri_payload"]["fri_proofs"][0] = base64.b64encode(b"replacement").decode()
        with self.assertRaisesRegex(s.Error, "duty_fri_artifact_mismatch"):
            prepare(self.f)
        self.f = copy.deepcopy(self.base)
        self.f["duties"][0]["transactionCount"] += 1
        with self.assertRaisesRegex(s.Error, "duty_transaction_count_mismatch"):
            prepare(self.f)

    def test_missing_retry_history_is_not_current_assignment(self):
        payload = self.f["manifest"]["payload"]
        payload["assignments"][0]["attempt"] = 2
        self.f["manifest"]["sequencer_signature"] = sign(s.manifest_request(self.f["settings"], payload), "sequencer")
        with self.assertRaisesRegex(s.Error, "missing_retry_history"):
            prepare(self.f)

    def test_wrong_signer_and_domain_rejected(self):
        prepared = copy.deepcopy(self.prepared)
        with self.assertRaises(s.Error):
            s.complete_package(self.f["settings"], prepared, sign(prepared["sequencer_request"], "operator"),
                               sign(prepared["wrapper_request"], "wrapper"))
        prepared["sequencer_request"]["typed_data"]["domain"]["chainId"] = "0x1"
        with self.assertRaisesRegex(s.Error, "signing_request_changed"):
            s.complete_package(self.f["settings"], prepared, "0x", "0x")

    def test_unsigned_output_preimages_cannot_change_after_endorsement(self):
        prepared = copy.deepcopy(self.prepared)
        prepared["sidecar"]["batch_outputs"][0]["l2TxCount"] = "0x99"
        with self.assertRaisesRegex(s.Error, "output_commitment_mismatch"):
            s.complete_package(self.f["settings"], prepared, sign(prepared["sequencer_request"], "sequencer"),
                               sign(prepared["wrapper_request"], "wrapper"))

    def test_noncanonical_native_proof_offsets_and_trailing_data_rejected(self):
        data = s.raw_hex(self.prepared["proof_data"])
        previous, batches = s.decode_proof_data(data)
        self.assertEqual(previous, self.f["evidence"]["previous_batch"])
        self.assertEqual(batches, [b["stored"] for b in self.f["evidence"]["batches"]])
        for broken in (data + b"\0" * 32, data[:1 + 9 * 32] + s.word(12 * 32) + data[1 + 10 * 32:]):
            with self.assertRaises(s.Error):
                s.decode_proof_data(broken)

    def test_wrapper_renewal_uses_first_period_and_rolls_at_exact_cutoff(self):
        snapshot = {"schema_version": 1, "chain_id": self.f["settings"]["execution_chain_id"],
                    "registry": self.f["settings"]["registry"], "block_number": "0x42", "block_hash": h(120),
                    "block_timestamp": hex(499), "start_time": "0x0", "period_seconds": hex(100),
                    "first_service_period": 6, "roster_publication_lead_seconds": 20}
        renewal = s.prepare_renewal(self.f["settings"], self.f["subscriptions"][0], snapshot)
        self.assertEqual(renewal["period"], 6)
        self.assertEqual(renewal["submit_before_timestamp"], hex(580))
        expected = s.cast("calldata", "renewWrapper(bytes32,uint64)", renewal["subscription_hash"], "6")
        self.assertEqual(renewal["transaction"]["data"], expected)
        snapshot["block_timestamp"] = hex(580)
        renewal = s.prepare_renewal(self.f["settings"], self.f["subscriptions"][0], snapshot)
        self.assertEqual(renewal["period"], 7)
        self.assertEqual(renewal["submit_before_timestamp"], hex(680))

    def test_renewal_rejects_expired_subscription_wrong_source_and_zero_lead(self):
        snapshot = {"schema_version": 1, "chain_id": self.f["settings"]["execution_chain_id"],
                    "registry": self.f["settings"]["registry"], "block_number": "0x42", "block_hash": h(120),
                    "block_timestamp": hex(800), "start_time": "0x0", "period_seconds": hex(100),
                    "first_service_period": 6, "roster_publication_lead_seconds": 20}
        with self.assertRaisesRegex(s.Error, "no_active_wrapper_subscription"):
            s.prepare_renewal(self.f["settings"], self.f["subscriptions"][0], snapshot)
        snapshot["chain_id"] = "0x1"
        with self.assertRaisesRegex(s.Error, "registry_snapshot_identity_mismatch"):
            s.prepare_renewal(self.f["settings"], self.f["subscriptions"][0], snapshot)
        snapshot["chain_id"] = self.f["settings"]["execution_chain_id"]
        snapshot["roster_publication_lead_seconds"] = 0
        with self.assertRaisesRegex(s.Error, "invalid_registry_clock"):
            s.prepare_renewal(self.f["settings"], self.f["subscriptions"][0], snapshot)


class GatewayServiceDomainTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.fixture = fixture(execution_chain_id="0x1644", settlement_chain_id="0x39", chain_address=a(117))

    def test_gateway_packages_use_root_settlement_and_child_registry_domains(self):
        f = self.fixture
        settings = f["settings"]
        subscription = s.subscription_request(settings, f["subscription"])
        duty = s.prepare_offered_duty(settings, f["evidence"], f["manifest"], f["subscriptions"], f["proofs"][0])
        package = prepare(f)
        self.assertEqual(subscription["typed_data"]["domain"]["chainId"], "0x23a")
        self.assertEqual(duty["typed_data"]["domain"]["chainId"], "0x23a")
        self.assertEqual(package["sequencer_request"]["typed_data"]["domain"]["chainId"], "0x39")
        self.assertEqual(package["wrapper_request"]["typed_data"]["domain"]["chainId"], "0x39")
        self.assertEqual(package["sidecar"]["accepted_package"]["chainId"], "0x1644")
        wrong_domain = copy.deepcopy(settings)
        wrong_domain["registry_chain_id"] = "0x1644"
        with self.assertRaises(s.Error):
            s.prepare_offered_duty(wrong_domain, f["evidence"], f["manifest"], f["subscriptions"], f["proofs"][0])
        wrong_evidence = copy.deepcopy(f["evidence"])
        wrong_evidence["chain_id"] = "0x23a"
        with self.assertRaisesRegex(s.Error, "evidence_identity_mismatch"):
            s.validate_evidence(settings, wrong_evidence)


def write_vectors(path):
    f = fixture()
    prepared = prepare(f)
    package = prepared["sidecar"]["accepted_package"]
    subscription = s.subscription_request(f["settings"], f["subscription"])
    duty = s.duty_request(f["settings"], {k: f["duties"][0][k] for k, _ in s.DUTY}, f["subscription"])
    bootstrap = s.typed_request("AcceptedPackageV1", s.PACKAGE, package, "ZkSysProofGate",
                                f["settings"]["settlement_chain_id"], f["settings"]["proof_gate"], f["settings"]["sequencer"])
    stored_type, duty_type, package_type = abi_type(s.STORED), abi_type(s.DUTY + [("operatorSignature", "bytes")]), abi_type(s.PACKAGE)
    output_type, candidate_type = abi_type(s.OUTPUT), abi_type(s.CANDIDATE)
    selectors = {"submit": s.cast("sig", f"submit({package_type},{duty_type}[],{output_type}[],bytes,{candidate_type},bytes32[],bytes,bytes)"),
                 "submitBootstrap": s.cast("sig", f"submitBootstrap({package_type},{duty_type}[],{output_type}[],bytes,bytes)"),
                 "repairPackage": s.cast("sig", f"repairPackage({package_type})")}
    vector = {"schema_version": 1, "description": "Synthetic ABI vectors only; proof bytes are not valid cryptographic proofs.",
              "config": f["settings"], "subscription": f["subscription"], "duty": f["duties"][0], "duties": f["duties"],
              "accepted_package": package, "candidate": f["proposal"]["candidate"],
              "previous_batch": f["evidence"]["previous_batch"], "batches": f["evidence"]["batches"],
              "proof_data": prepared["proof_data"], "sidecar": s.complete_package(f["settings"], prepared,
                 sign(prepared["sequencer_request"], "sequencer"), sign(prepared["wrapper_request"], "wrapper")),
              "hashes": {"subscription": subscription["struct_hash"], "subscription_digest": subscription["digest"],
                         "duty": duty["struct_hash"], "duty_digest": duty["digest"],
                         "package": prepared["sequencer_request"]["struct_hash"],
                         "coordinator_digest": prepared["sequencer_request"]["digest"], "bootstrap_digest": bootstrap["digest"],
                         "report": s.report_hash(f["duties"]), "wrapper_leaf": s.wrapper_leaf(f["proposal"]["candidate"]),
                         "batch_output": s.output_hash(f["evidence"]["batches"][0]["output"]),
                         "chain_config": s.chain_config_hash(f["settings"]["execution_chain_id"]),
                         "statement": s.validate_evidence(f["settings"], f["evidence"])[1]["statement"]}, "selectors": selectors}
    Path(path).write_text(json.dumps(vector, indent=2) + "\n")


if __name__ == "__main__":
    unittest.main()
