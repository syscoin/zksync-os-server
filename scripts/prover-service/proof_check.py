#!/usr/bin/env python3
"""Verify a native V32 SNARK against canonical settlement state before endorsement."""

import argparse
import hashlib
import math
import os
import sys
import time

import keeper as k
import relay as r
import service as s


MAX_PAYLOAD = 256 * 1024 * 1024
MAX_FRI_ENCODING = 10 * 1024 * 1024
SNARK_BYTES = 44 * 32
VERIFY_SIGNATURE = "verify(uint256[],uint256[])"


def sha(value):
    return hashlib.sha256(s.canonical(value)).hexdigest()


class DeadlineRpc:
    """Keep the complete canonical-state check within one RPC time budget."""

    def __init__(self, rpc, seconds):
        self.started = time.monotonic()
        self.deadline = self.started + seconds
        # A private transport avoids changing a caller's shared timeout setting.
        self.rpc = r.Rpc(rpc.url, rpc.authorization, seconds) if type(rpc) is r.Rpc else rpc

    def call(self, method, params):
        remaining = self.deadline - time.monotonic()
        s.require(remaining > 0, "proof_verification_timeout")
        if type(self.rpc) is r.Rpc:
            self.rpc.timeout = remaining
        result = self.rpc.call(method, params)
        s.require(time.monotonic() < self.deadline, "proof_verification_timeout")
        return result


def inputs(settings, evidence, payload, proof):
    statements = s.validate_evidence(settings, evidence)
    numbers = list(statements)
    s.require(2 <= len(numbers) <= 100, "invalid_snark_verification_range")
    s.exact(payload, ("from_batch_number", "to_batch_number", "vk_hash", "fri_proofs"))
    s.exact(proof, ("from_batch_number", "to_batch_number", "vk_hash", "proof"))
    for value in (payload, proof):
        s.require(s.uint(value["from_batch_number"], 64) == numbers[0]
                  and s.uint(value["to_batch_number"], 64) == numbers[-1]
                  and value["vk_hash"] == settings["vk_hash"], "proof_range_or_vk_mismatch")
    fri = payload["fri_proofs"]
    s.require(type(fri) is list and len(fri) == len(numbers), "fri_payload_range_mismatch")
    s.require(all(type(value) is str and len(value) <= MAX_FRI_ENCODING for value in fri)
              and sum(map(len, fri)) <= MAX_PAYLOAD - 4096, "fri_payload_too_large")
    s.require(type(proof["proof"]) is str and len(proof["proof"]) == 4 * ((SNARK_BYTES + 2) // 3),
              "invalid_v8_snark_length")
    native_proof = s.decode_proof(proof["proof"])
    s.require(len(native_proof) == SNARK_BYTES, "invalid_v8_snark_length")
    # These hashes bind the audited duty artifacts. The native SNARK authenticates their
    # execution statements, not the identity of individual FRI encodings or their authors.
    fri_hashes = [s.keccak(s.decode_proof(value)) for value in fri]
    return [statements[number]["statement"] for number in numbers], native_proof, fri_hashes


def verifier_calldata(statements, proof_bytes):
    s.require(2 <= len(statements) <= 100 and len(proof_bytes) == SNARK_BYTES, "invalid_verifier_input")
    public_inputs = s.word(len(statements)) + b"".join(s.raw_hex(value, 32) for value in statements)
    native_proof = s.word(46) + s.word(0x802) + s.word(0) + proof_bytes
    # Executor passes the entire unshifted statement array. The V8 verifier performs
    # aggregate hashing and the 32-bit shift; folding here would apply them twice.
    return "0x" + (r.selector(VERIFY_SIGNATURE)
                    + s.encode_dynamic_tuple([(public_inputs, True), (native_proof, True)])).hex()


def verify_snark(config, rpc, evidence, payload, proof, now):
    """Return a local check record; callers must recheck before each new endorsement.

    The record does not attest to FRI authorship or replace assignment/report validation.
    A false verifier result is rejected; transport errors and reorgs produce no record.
    """
    k.configuration(config)
    s.require(type(now) in (int, float) and math.isfinite(now) and now >= 0, "invalid_verification_time")
    bounded = DeadlineRpc(rpc, config["policy"]["rpc_timeout_seconds"])
    statements, native_proof, fri_hashes = inputs(config["settings"], evidence, payload, proof)
    bindings = {"configuration_sha256": sha(config), "evidence_sha256": sha(evidence),
                "payload_sha256": sha(payload), "proof_sha256": sha(proof)}
    head, anchor, call = k.base_context(config, bounded, evidence, now)
    verifier = call(config["settings"]["proof_gate"], "productionVerifier()", kind="address")
    data = verifier_calldata(statements, native_proof)
    result = bounded.call("eth_call", [{"to": verifier, "data": data}, anchor])
    # Recheck the anchor before classifying a negative result as a canonical rejection.
    r.check_anchor(bounded, config["settings"], head)
    elapsed = time.monotonic() - bounded.started
    s.require(now + elapsed - s.uint(head["timestamp"]) <= config["policy"]["max_head_age_seconds"],
              "stale_verification_anchor")
    raw = s.raw_hex(result, 32)
    s.require(raw in (s.word(0), s.word(1)), "invalid_verifier_boolean")
    s.require(raw == s.word(1), "native_snark_rejected")
    s.require(bindings == {"configuration_sha256": sha(config), "evidence_sha256": sha(evidence),
                           "payload_sha256": sha(payload), "proof_sha256": sha(proof)},
              "proof_verification_inputs_changed")
    value = {"schema_version": 1, "verification": "canonical_native_v32_snark", **bindings,
             "lane": config["lane"], "chain_id": config["settings"]["execution_chain_id"],
             "chain_address": config["settings"]["chain_address"],
             "settlement_chain_id": config["settings"]["settlement_chain_id"],
             "vk_hash": config["settings"]["vk_hash"], "verifier": verifier,
             "from_batch_number": proof["from_batch_number"], "to_batch_number": proof["to_batch_number"],
             "anchor": {key: head[key] for key in ("number", "hash", "timestamp")},
             "proof_data_hash": s.keccak(s.proof_data(evidence, native_proof)),
             "verifier_calldata_hash": s.keccak(s.raw_hex(data)),
             "statements": statements, "fri_artifact_hashes": fri_hashes}
    return {**value, "attestation_sha256": sha(value)}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("config", "evidence", "payload", "proof", "output"):
        parser.add_argument("--" + name, required=True)
    args = parser.parse_args()
    config = k.configuration(s.read_json(args.config, private=True))
    result = verify_snark(config, k.rpc_for(config), s.read_json(args.evidence),
                          s.read_json(args.payload, maximum=MAX_PAYLOAD), s.read_json(args.proof), time.time())
    s.write_new(args.output, result)


if __name__ == "__main__":
    os.umask(0o077)
    try:
        main()
    except (s.Error, OSError, ValueError, TypeError, KeyError) as error:
        print(str(error) if isinstance(error, s.Error) else "proof_verification_failed", file=sys.stderr)
        raise SystemExit(1)
