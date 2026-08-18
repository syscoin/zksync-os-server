#!/usr/bin/env python3
"""Fail-closed corpus checks against the pinned independent C verifier."""

import argparse
import hashlib
import json
import subprocess
import sys
from pathlib import Path


SIGNATURE_BYTES = 3856
EXPECTED_PARAMETERS = {
    "n": 16,
    "h": 22,
    "d": 1,
    "hPrime": 22,
    "a": 24,
    "k": 6,
    "lgW": 2,
    "m": 21,
}


def fail(message):
    raise RuntimeError(message)


def no_duplicate_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            fail(f"duplicate JSON key: {key}")
        result[key] = value
    return result


def decode_hex(value, label, expected_len=None):
    if not isinstance(value, str) or not value.startswith("0x"):
        fail(f"{label}: expected 0x-prefixed hex string")
    try:
        decoded = bytes.fromhex(value[2:])
    except ValueError as err:
        fail(f"{label}: invalid hex: {err}")
    if expected_len is not None and len(decoded) != expected_len:
        fail(f"{label}: expected {expected_len} bytes, got {len(decoded)}")
    return decoded


def load_vector(path):
    with path.open("r", encoding="utf-8") as handle:
        vector = json.load(handle, object_pairs_hook=no_duplicate_object)

    vector_id = vector.get("id")
    if vector.get("schemaVersion") != 1 or vector.get("expected") is not True:
        fail(f"{path}: unsupported schema or non-valid fixture")
    if vector.get("spec", {}).get("status") != "initial-public-draft":
        fail(f"{path}: fixture is not pinned to the SP 800-230 IPD")
    if vector["spec"].get("doi") != "10.6028/NIST.SP.800-230.ipd":
        fail(f"{path}: unexpected specification DOI")
    if vector["spec"].get("fipsApprovedParameterSet") is not False:
        fail(f"{path}: must not claim FIPS approval")
    if vector["spec"].get("nistAcvpVector") is not False:
        fail(f"{path}: must not claim NIST ACVP provenance")
    if vector["spec"].get("parameters") != EXPECTED_PARAMETERS:
        fail(f"{path}: unexpected parameter tuple")

    pk_seed_word = decode_hex(vector.get("pkSeed"), f"{vector_id}.pkSeed", 32)
    pk_root_word = decode_hex(vector.get("pkRoot"), f"{vector_id}.pkRoot", 32)
    message = decode_hex(vector.get("message"), f"{vector_id}.message", 32)
    signature = decode_hex(vector.get("signature"), f"{vector_id}.signature", SIGNATURE_BYTES)
    if any(pk_seed_word[16:]) or any(pk_root_word[16:]):
        fail(f"{path}: public-key words have noncanonical low padding")

    actual_signature_hash = hashlib.sha256(signature).hexdigest()
    recorded_signature_hash = vector.get("signatureSha256", "").removeprefix("0x")
    if actual_signature_hash != recorded_signature_hash:
        fail(f"{path}: signature SHA-256 mismatch")
    precompile_input = pk_seed_word + pk_root_word + message + signature
    actual_input_hash = hashlib.sha256(precompile_input).hexdigest()
    recorded_input_hash = vector.get("precompileInputSha256", "").removeprefix("0x")
    if actual_input_hash != recorded_input_hash:
        fail(f"{path}: 0x101 precompile-input SHA-256 mismatch")

    reproducible = vector.get("provenance", {}).get("reproducible")
    if vector_id == "slh-dsa-sha2-128-24-legacy-regression-v1":
        if reproducible is not False or vector.get("status") != "legacy-unreproducible-regression-only":
            fail(f"{path}: legacy fixture lost its unreproducible-only label")
    elif vector_id == "slh-dsa-sha2-128-24-sp800-230-ipd-counter0-v1":
        if reproducible is not True or vector.get("status") != "canonical-reproducible-conformance":
            fail(f"{path}: canonical fixture lost reproducible provenance")
        inputs = vector.get("inputs", {})
        for field, length in (("masterSecret", 32), ("skSeed", 16), ("skPrf", 16),
                              ("pkSeed", 16), ("optRand", 16), ("message", 32)):
            decode_hex(inputs.get(field), f"{vector_id}.inputs.{field}", length)
        if decode_hex(inputs["pkSeed"], f"{vector_id}.inputs.pkSeed") != pk_seed_word[:16]:
            fail(f"{path}: canonical input PK.seed does not match public key")
        if decode_hex(inputs["message"], f"{vector_id}.inputs.message") != message:
            fail(f"{path}: canonical input message does not match fixture")
        envelope = decode_hex(vector["spec"].get("messageEnvelope"), f"{vector_id}.messageEnvelope")
        if envelope != b"\x00\x00" + message:
            fail(f"{path}: external empty-context envelope mismatch")
        if vector.get("hashes", {}).get("signatureSha256", "").removeprefix("0x") != actual_signature_hash:
            fail(f"{path}: generator signature hash mismatch")
    else:
        fail(f"{path}: unrecognized vector id {vector_id!r}")

    return {
        "id": vector_id,
        "pkSeed": pk_seed_word[:16],
        "pkRoot": pk_root_word[:16],
        "message": message,
        "signature": signature,
    }


def invoke(binary, vector, message, signature):
    command = [
        str(binary),
        vector["pkSeed"].hex(),
        vector["pkRoot"].hex(),
        message.hex(),
        signature.hex(),
    ]
    completed = subprocess.run(command, check=True, capture_output=True, text=True, timeout=30)
    try:
        return json.loads(completed.stdout)
    except json.JSONDecodeError as err:
        fail(f"{vector['id']}: verifier emitted invalid JSON: {err}")


def expect_result(binary, vector, label, message, signature, valid):
    result = invoke(binary, vector, message, signature)
    expected = {"external": int(valid), "internalWrapped": int(valid), "internalRaw": 0}
    if result != expected:
        fail(f"{vector['id']} {label}: got {result}, expected {expected}")
    print(f"PASS {vector['id']} {label}")


def mutated(data, offset):
    result = bytearray(data)
    result[offset] ^= 0x01
    return bytes(result)


def exercise_vector(binary, vector):
    message = vector["message"]
    signature = vector["signature"]
    expect_result(binary, vector, "valid", message, signature, True)

    # SYSCOIN: Mirror the NIST ACVP verification modification classes without
    # claiming this draft parameter set is itself covered by an ACVP vector.
    cases = [
        ("modified-message", mutated(message, 0), signature),
        ("modified-R", message, mutated(signature, 0)),
        ("modified-SIG_FORS", message, mutated(signature, 16)),
        ("modified-SIG_HT-WOTS", message, mutated(signature, 2416)),
        ("modified-SIG_HT-auth", message, mutated(signature, 3504)),
        ("signature-too-short", message, signature[:-1]),
        ("signature-too-long", message, signature + b"\x00"),
    ]
    for label, case_message, case_signature in cases:
        expect_result(binary, vector, label, case_message, case_signature, False)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("binary", type=Path)
    parser.add_argument("vectors", nargs=2, type=Path)
    args = parser.parse_args()

    if not args.binary.is_file():
        fail(f"verifier binary not found: {args.binary}")
    vectors = [load_vector(path) for path in args.vectors]
    if {vector["id"] for vector in vectors} != {
        "slh-dsa-sha2-128-24-legacy-regression-v1",
        "slh-dsa-sha2-128-24-sp800-230-ipd-counter0-v1",
    }:
        fail("the corpus must contain exactly the legacy and canonical fixtures")
    for vector in vectors:
        exercise_vector(args.binary, vector)
    print("PASS SP 800-230 IPD independent conformance corpus")


if __name__ == "__main__":
    try:
        main()
    except (RuntimeError, subprocess.SubprocessError) as err:
        print(f"FAIL: {err}", file=sys.stderr)
        sys.exit(1)
