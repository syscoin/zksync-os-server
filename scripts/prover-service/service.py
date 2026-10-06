#!/usr/bin/env python3
"""Trusted-host, unsigned service packages; no signing keys or transaction submission."""

import argparse
from dataclasses import dataclass
import base64
import functools
import json
import os
from pathlib import Path
import re
import stat
import subprocess
import sys


ZERO = "0x" + "00" * 32
ZERO_ADDRESS = "0x" + "00" * 20
MAX_FILE = 16 * 1024 * 1024
SUBSCRIPTION = [("account", "address"), ("operator", "address"), ("beneficiary", "address"),
                ("sequencer", "address"), ("firstPeriod", "uint64"), ("lastPeriod", "uint64"),
                ("nonce", "uint64"), ("services", "uint8")]
DUTY = [("account", "address"), ("subscriptionHash", "bytes32"), ("batchNumber", "uint64"),
        ("statementHash", "bytes32"), ("friProofHash", "bytes32"), ("transactionCount", "uint64"),
        ("period", "uint64"), ("slot", "uint16"), ("attempt", "uint32"), ("assignmentId", "bytes32")]
PACKAGE = [("domainVersion", "uint32"), ("policyHash", "bytes32"), ("chainId", "uint256"),
           ("chainAddress", "address"), ("parent", "bytes32"), ("batchFrom", "uint64"),
           ("batchTo", "uint64"), ("protocolVersion", "uint32"), ("vkHash", "bytes32"),
           ("period", "uint64"), ("rosterRoot", "bytes32"), ("turn", "uint32"),
           ("manifestHash", "bytes32"), ("reportHash", "bytes32"), ("proofHash", "bytes32"),
           ("sequencer", "address"), ("sequencerBeneficiary", "address"), ("wrapper", "address"),
           ("wrapperBeneficiary", "address")]
CANDIDATE = [("index", "uint32"), ("account", "address"), ("operator", "address"), ("beneficiary", "address")]
STORED = [("batchNumber", "uint64"), ("batchHash", "bytes32"), ("indexRepeatedStorageChanges", "uint64"),
          ("numberOfLayer1Txs", "uint256"), ("priorityOperationsHash", "bytes32"),
          ("dependencyRootsRollingHash", "bytes32"), ("l2LogsTreeRoot", "bytes32"),
          ("timestamp", "uint256"), ("commitment", "bytes32")]
OUTPUT = [("firstBlockTimestamp", "uint64"), ("lastBlockTimestamp", "uint64"), ("daScheme", "uint256"),
          ("daCommitment", "bytes32"), ("l1TxCount", "uint256"), ("l2TxCount", "uint256"),
          ("priorityOperationsHash", "bytes32"), ("l2LogsRoot", "bytes32"), ("upgradeTxHash", "bytes32"),
          ("dependencyRootsRollingHash", "bytes32"), ("settlementChainId", "uint256"), ("edgeDARefsRoot", "bytes32")]
DOMAIN = [("name", "string"), ("version", "string"), ("chainId", "uint256"), ("verifyingContract", "address")]


class Error(Exception):
    pass


def require(condition, reason):
    if not condition:
        raise Error(reason)


def exact(value, fields):
    require(type(value) is dict and set(value) == set(fields), "unexpected_or_missing_fields")


def uint(value, bits=256):
    if bits == 256 and isinstance(value, str):
        require(re.fullmatch(r"0x(?:0|[1-9a-f][0-9a-f]*)", value), "noncanonical_uint256")
        value = int(value, 16)
    require(type(value) is int and 0 <= value < 2**bits, "invalid_unsigned_integer")
    return value


def raw_hex(value, size=None):
    require(isinstance(value, str) and re.fullmatch(r"0x(?:[0-9a-f]{2})*", value), "invalid_lowercase_hex")
    data = bytes.fromhex(value[2:])
    require(size is None or len(data) == size, "incorrect_hex_size")
    return data


def nonzero(value, size=32):
    require(any(raw_hex(value, size)), "zero_identity")
    return value


def word(value):
    return uint(value).to_bytes(32, "big")


def canonical(value):
    return json.dumps(value, sort_keys=True, separators=(",", ":"), allow_nan=False).encode()


def _unique(pairs):
    result = {}
    for key, value in pairs:
        require(key not in result, "duplicate_json_field")
        result[key] = value
    return result


def read(path, private=False, maximum=MAX_FILE):
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW)
    with os.fdopen(fd, "rb") as source:
        info = os.fstat(source.fileno())
        require(stat.S_ISREG(info.st_mode) and not info.st_mode & 0o022, "unsafe_input_file")
        require(not private or info.st_uid == os.getuid() and not info.st_mode & 0o077, "private_file_required")
        require(info.st_size <= maximum, "file_too_large")
        data = source.read(maximum + 1)
    require(len(data) <= maximum, "file_too_large")
    return data


def read_json(path, private=False, maximum=MAX_FILE):
    try:
        return json.loads(read(path, private, maximum), object_pairs_hook=_unique,
                          parse_constant=lambda _: require(False, "nonfinite_json_value"))
    except (ValueError, UnicodeError):
        raise Error("invalid_json") from None


def write_new(path, value):
    data = canonical(value) + b"\n"
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    with os.fdopen(fd, "wb") as output:
        output.write(data)
        output.flush()
        os.fsync(output.fileno())
    fd = os.open(Path(path).parent, os.O_RDONLY)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def cast(*args, stdin=None):
    try:
        result = subprocess.run(["cast", *args], input=stdin, stdout=subprocess.PIPE,
                                stderr=subprocess.PIPE, check=True, timeout=30)
    except (OSError, subprocess.SubprocessError):
        raise Error("cast_failed") from None
    return result.stdout.decode().strip()


def keccak(data):
    # Stdin avoids OS argv limits for native FRI artifacts and does not expose their bytes in ps.
    digest = cast("keccak", stdin=("0x" + data.hex()).encode())
    raw_hex(digest, 32)
    return digest


def schema_type(name, fields):
    return name + "(" + ",".join(kind + " " + key for key, kind in fields) + ")"


@functools.lru_cache(maxsize=32)
def type_hash(text):
    return keccak(text.encode())


def encode_fields(fields, value, packed=False):
    exact(value, [key for key, _ in fields])
    encoded = b""
    for key, kind in fields:
        item = value[key]
        if kind.startswith("uint"):
            bits = int(kind[4:])
            require(bits != 256 or isinstance(item, str), "uint256_hex_string_required")
            encoded += uint(item, bits).to_bytes(bits // 8 if packed else 32, "big")
        elif kind == "address":
            encoded += (b"" if packed else b"\0" * 12) + raw_hex(item, 20)
        elif kind == "bytes32":
            encoded += raw_hex(item, 32)
        elif kind == "string":
            require(type(item) is str, "invalid_string")
            encoded += raw_hex(keccak(item.encode()), 32)
        else:
            raise Error("unsupported_abi_type")
    return encoded


def struct_hash(name, fields, value):
    return keccak(raw_hex(type_hash(schema_type(name, fields))) + encode_fields(fields, value))


def typed_request(name, fields, value, domain_name, chain_id, contract, signer):
    domain = {"name": domain_name, "version": "1", "chainId": hex(uint(chain_id)), "verifyingContract": contract}
    nonzero(contract, 20)
    nonzero(signer, 20)
    domain_hash = struct_hash("EIP712Domain", DOMAIN, domain)
    hashed = struct_hash(name, fields, value)
    return {"schema_version": 1, "signer": signer,
            "typed_data": {"types": {"EIP712Domain": [{"name": k, "type": t} for k, t in DOMAIN],
                                      name: [{"name": k, "type": t} for k, t in fields]},
                           "primaryType": name, "domain": domain, "message": value},
            "struct_hash": hashed, "digest": keccak(b"\x19\x01" + raw_hex(domain_hash) + raw_hex(hashed))}


def config(value):
    exact(value, ("schema_version", "execution_chain_id", "registry_chain_id", "chain_address", "settlement_chain_id", "registry",
                  "coordinator", "proof_gate", "policy_hash", "vk_hash", "sequencer", "duties_per_round"))
    require(value["schema_version"] == 1, "unsupported_schema")
    for field in ("execution_chain_id", "registry_chain_id", "settlement_chain_id"):
        require(isinstance(value[field], str) and uint(value[field]) > 0, "invalid_chain_id")
    for field in ("chain_address", "registry", "proof_gate", "sequencer"):
        nonzero(value[field], 20)
    raw_hex(value["coordinator"], 20)
    for field in ("policy_hash", "vk_hash"):
        nonzero(value[field])
    require(0 < uint(value["duties_per_round"], 16) <= 64, "invalid_duty_count")
    return value


@dataclass(frozen=True, init=False)
class EnrollmentAuthority:
    """Canonical registry consent for one exact subscription snapshot and period."""

    identity: tuple
    snapshot: bytes
    period: int
    block_hash: str

    def __init__(self, settings, subscriptions, period, rpc, block_hash):
        import enrollment
        nonzero(block_hash)
        _, anchor = enrollment.enrollment_snapshot(settings, subscriptions, period, rpc, block_hash=block_hash)
        object.__setattr__(self, "identity", self.scope(settings))
        object.__setattr__(self, "snapshot", canonical(subscriptions))
        object.__setattr__(self, "period", period)
        object.__setattr__(self, "block_hash", anchor["block_hash"])

    @staticmethod
    def scope(settings):
        config(settings)
        return tuple(settings[key] for key in
                     ("registry_chain_id", "registry", "policy_hash", "sequencer", "duties_per_round"))

    def check(self, settings, subscriptions, period):
        require(self.identity == self.scope(settings) and self.period == period
                and self.snapshot == canonical(subscriptions), "enrollment_authority_scope_mismatch")


def subscription_request(settings, subscription):
    encode_fields(SUBSCRIPTION, subscription)
    for field in ("account", "operator", "beneficiary", "sequencer"):
        nonzero(subscription[field], 20)
    require(subscription["sequencer"] == settings["sequencer"] != subscription["operator"], "wrong_sequencer_or_operator")
    require(0 <= subscription["lastPeriod"] - subscription["firstPeriod"] < 64, "invalid_subscription_period")
    require(subscription["services"] == 3, "combined_fri_and_wrapper_subscription_required")
    return typed_request("ProverSubscriptionV1", SUBSCRIPTION, subscription, "ZkSysProverService",
                         settings["registry_chain_id"], settings["registry"], subscription["account"])


def operator_subscription_request(settings, signed_subscription):
    exact(signed_subscription, ("subscription", "signature"))
    request = subscription_request(settings, signed_subscription["subscription"])
    verify_eoa(request, signed_subscription["signature"])
    # Both parties consent to the same tuple; changing the expected signer must not change its digest.
    request["signer"] = signed_subscription["subscription"]["operator"]
    return request


def prepare_enrollment(settings, signed_subscription, operator_signature):
    request = operator_subscription_request(settings, signed_subscription)
    verify_eoa(request, operator_signature)
    subscription = signed_subscription["subscription"]
    signature = "subscribe((address,address,address,address,uint64,uint64,uint64,uint8),bytes,bytes)"
    calldata = raw_hex(type_hash(signature))[:4] + encode_dynamic_tuple([
        (encode_fields(SUBSCRIPTION, subscription), False),
        (encode_bytes(raw_hex(signed_subscription["signature"])), True),
        (encode_bytes(raw_hex(operator_signature)), True),
    ])
    return {"schema_version": 1, "subscription_hash": request["struct_hash"],
            "transaction": {"chainId": settings["registry_chain_id"], "from": subscription["account"],
                            "to": settings["registry"], "value": "0x0", "data": "0x" + calldata.hex()}}


def verify_eoa(request, signature):
    signature_bytes = raw_hex(signature)
    require(len(signature_bytes) == 65 and signature_bytes[64] in (27, 28), "eoa_signature_required")
    require(0 < int.from_bytes(signature_bytes[32:64], "big") <=
            0x7FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF5D576E7357A4501DDFE92F46681B20A0,
            "noncanonical_signature")
    output = cast("wallet", "verify", "--no-hash", "--address", request["signer"], request["digest"], signature)
    require("validation succeeded" in output.lower(), "invalid_signature")


def prepare_renewal(settings, signed_subscription, snapshot):
    exact(signed_subscription, ("subscription", "signature"))
    subscription = signed_subscription["subscription"]
    request = subscription_request(settings, subscription)
    verify_eoa(request, signed_subscription["signature"])
    exact(snapshot, ("schema_version", "chain_id", "registry", "block_number", "block_hash", "block_timestamp",
                     "start_time", "period_seconds", "first_service_period", "roster_publication_lead_seconds"))
    require(snapshot["schema_version"] == 1 and snapshot["chain_id"] == settings["registry_chain_id"]
            and snapshot["registry"] == settings["registry"], "registry_snapshot_identity_mismatch")
    nonzero(snapshot["block_hash"])
    for field in ("block_number", "block_timestamp", "start_time", "period_seconds"):
        require(isinstance(snapshot[field], str), "snapshot_uint256_hex_required")
        uint(snapshot[field])
    now, start, seconds = uint(snapshot["block_timestamp"]), uint(snapshot["start_time"]), uint(snapshot["period_seconds"])
    first = uint(snapshot["first_service_period"], 64)
    lead = uint(snapshot["roster_publication_lead_seconds"], 64)
    require(0 < lead <= seconds, "invalid_registry_clock")
    next_period = 0 if now < start else (now - start) // seconds + 1
    next_period = max(next_period, first)
    cutoff = max(0, uint(start + next_period * seconds) - lead)
    if now >= cutoff:
        next_period += 1
        cutoff = max(0, uint(start + next_period * seconds) - lead)
    uint(next_period, 64)
    require(subscription["services"] == 3 and subscription["firstPeriod"] <= next_period <= subscription["lastPeriod"],
            "no_active_wrapper_subscription_for_next_period")
    signature = "renewWrapper(bytes32,uint64)"
    calldata = raw_hex(type_hash(signature))[:4] + raw_hex(request["struct_hash"]) + word(next_period)
    return {"schema_version": 1, "subscription_hash": request["struct_hash"], "period": next_period,
            "source_block": {"number": snapshot["block_number"], "hash": snapshot["block_hash"],
                             "timestamp": snapshot["block_timestamp"]},
            "submit_before_timestamp": hex(cutoff),
            "transaction": {"chainId": settings["registry_chain_id"], "to": settings["registry"], "value": "0x0",
                            "data": "0x" + calldata.hex()}}


def encode_dynamic_tuple(parts):
    head_size = sum(32 if dynamic else len(data) for data, dynamic in parts)
    heads, tails = b"", b""
    for data, dynamic in parts:
        if dynamic:
            heads += word(head_size + len(tails))
            tails += data
        else:
            heads += data
    return heads + tails


def encode_bytes(data):
    return word(len(data)) + data + b"\0" * (-len(data) % 32)


def encode_duty(duty):
    exact(duty, [k for k, _ in DUTY] + ["operatorSignature"])
    signature = raw_hex(duty["operatorSignature"])
    require(0 < len(signature) <= 4096, "invalid_signature_size")
    body = {k: duty[k] for k, _ in DUTY}
    return encode_dynamic_tuple([(encode_fields(DUTY, body), False), (encode_bytes(signature), True)])


def report_hash(duties):
    require(type(duties) is list and len(duties) <= 100, "too_many_duties")
    encoded = encode_dynamic_tuple([(encode_duty(d), True) for d in duties])
    return keccak(word(32) + word(len(duties)) + encoded)


def wrapper_leaf(candidate):
    return keccak(raw_hex(keccak(encode_fields(CANDIDATE, candidate))))


def check_candidate(candidate, proof, root):
    require(type(proof) is list and len(proof) <= 32, "invalid_candidate_proof")
    for field in ("account", "operator", "beneficiary"):
        nonzero(candidate[field], 20)
    current = raw_hex(wrapper_leaf(candidate))
    for sibling in proof:
        sibling = raw_hex(sibling, 32)
        current = raw_hex(keccak(min(current, sibling) + max(current, sibling)))
    require(current == raw_hex(root, 32), "candidate_not_in_roster")


def output_hash(output):
    return keccak(encode_fields(OUTPUT, output, packed=True))


def chain_config_hash(chain_id):
    return keccak(word(uint(chain_id)) + word(0) + word(1 << 24))


def statement(previous, stored, execution_chain_id):
    return keccak(raw_hex(previous["batchHash"]) + raw_hex(stored["batchHash"]) +
                  raw_hex(chain_config_hash(execution_chain_id)) + raw_hex(stored["commitment"]))


def validate_evidence(settings, evidence):
    exact(evidence, ("schema_version", "chain_id", "chain_address", "settlement_chain_id", "protocol_version",
                     "vk_hash", "previous_batch", "batches"))
    require(evidence["schema_version"] == 1 and evidence["protocol_version"] == 32, "unsupported_evidence_protocol")
    for source, expected in (("chain_id", "execution_chain_id"), ("chain_address", "chain_address"),
                             ("settlement_chain_id", "settlement_chain_id"), ("vk_hash", "vk_hash")):
        require(evidence[source] == settings[expected], "evidence_identity_mismatch")
    require(type(evidence["batches"]) is list and 1 <= len(evidence["batches"]) <= 100, "invalid_evidence_range")
    previous = evidence["previous_batch"]
    encode_fields(STORED, previous)
    result = {}
    for item in evidence["batches"]:
        exact(item, ("stored", "output"))
        stored, output = item["stored"], item["output"]
        encode_fields(STORED, stored)
        require(stored["batchNumber"] == previous["batchNumber"] + 1, "noncontiguous_evidence")
        require(output_hash(output) == stored["commitment"], "output_commitment_mismatch")
        require(output["settlementChainId"] == settings["settlement_chain_id"], "wrong_output_settlement_chain")
        require(output["firstBlockTimestamp"] <= output["lastBlockTimestamp"], "invalid_output_timestamps")
        for field, other in (("numberOfLayer1Txs", "l1TxCount"), ("priorityOperationsHash", "priorityOperationsHash"),
                             ("dependencyRootsRollingHash", "dependencyRootsRollingHash"), ("l2LogsTreeRoot", "l2LogsRoot")):
            require(stored[field] == output[other], "stored_output_mismatch")
        count = uint(output["l1TxCount"]) + uint(output["l2TxCount"])
        require(count < 2**64, "transaction_count_overflow")
        result[stored["batchNumber"]] = {"statement": statement(previous, stored, settings["execution_chain_id"]), "count": count}
        previous = stored
    return result


def manifest_request(settings, payload):
    message = {"manifestHash": keccak(canonical(payload))}
    return typed_request("WorkManifestV1", [("manifestHash", "bytes32")], message, "ZkSysServiceManifest",
                         settings["registry_chain_id"], settings["registry"], settings["sequencer"])


def validate_manifest(settings, manifest, subscriptions, evidence, allow_empty=False, *, enrollment=None):
    exact(manifest, ("payload", "sequencer_signature"))
    payload = manifest["payload"]
    exact(payload, ("schema_version", "chain_id", "chain_address", "sequencer", "period", "previous_cursor",
                    "subscription_snapshot_hash", "assignments", "retries"))
    require(payload["schema_version"] == 1 and payload["chain_id"] == settings["execution_chain_id"]
            and payload["chain_address"] == settings["chain_address"] and payload["sequencer"] == settings["sequencer"],
            "manifest_identity_mismatch")
    uint(payload["period"], 64)
    nonzero(payload["previous_cursor"])
    minimum = 0 if allow_empty else 1
    require(type(subscriptions) is list and minimum <= len(subscriptions) <= 1000, "invalid_subscription_snapshot")
    require(payload["subscription_snapshot_hash"] == keccak(canonical(subscriptions)), "subscription_snapshot_mismatch")
    if enrollment is not None:
        require(type(enrollment) is EnrollmentAuthority, "authenticated_enrollment_authority_required")
        enrollment.check(settings, subscriptions, payload["period"])
    by_hash = {}
    accounts, operators = set(), set()
    for signed in subscriptions:
        exact(signed, ("subscription", "signature"))
        subscription = signed["subscription"]
        request = subscription_request(settings, subscription)
        if enrollment is None:
            verify_eoa(request, signed["signature"])
        require(subscription["account"] not in accounts and subscription["operator"] not in operators,
                "duplicate_subscription_identity")
        require(subscription["firstPeriod"] <= payload["period"] <= subscription["lastPeriod"]
                and subscription["services"] == 3, "inactive_fri_subscription")
        accounts.add(subscription["account"])
        operators.add(subscription["operator"])
        by_hash[request["struct_hash"]] = subscription
    verify_eoa(manifest_request(settings, payload), manifest["sequencer_signature"])
    require(type(payload["assignments"]) is list and minimum <= len(payload["assignments"]) <= 2000,
            "invalid_assignments")
    assignments = {}
    for item in payload["assignments"]:
        exact(item, ("assignment_id", "account", "subscription_hash", "batch_number", "statement_hash",
                     "lease_commitment", "attempt", "slot", "period"))
        for field in ("assignment_id", "subscription_hash", "statement_hash", "lease_commitment"):
            nonzero(item[field])
        require(item["assignment_id"] not in assignments, "duplicate_assignment")
        subscription = by_hash.get(item["subscription_hash"])
        require(subscription and subscription["account"] == item["account"], "assignment_subscription_mismatch")
        require(item["period"] == payload["period"] and 0 < uint(item["attempt"], 32)
                and uint(item["slot"], 16) < settings["duties_per_round"], "invalid_assignment_duty")
        batch = evidence.get(uint(item["batch_number"], 64))
        require(batch and batch["statement"] == item["statement_hash"], "assignment_statement_mismatch")
        assignments[item["assignment_id"]] = item
    require(type(payload["retries"]) is list and len(payload["retries"]) <= 2000, "invalid_retries")
    retired, incoming = set(), set()
    for retry in payload["retries"]:
        exact(retry, ("assignment_id", "next_assignment_id", "reason"))
        previous, following = assignments.get(retry["assignment_id"]), assignments.get(retry["next_assignment_id"])
        require(previous and following and retry["reason"] in ("expired", "invalid", "unavailable"), "invalid_retry")
        require(previous["assignment_id"] not in retired and following["assignment_id"] not in incoming,
                "forked_retry_history")
        require(previous["batch_number"] == following["batch_number"] and following["attempt"] == previous["attempt"] + 1
                and previous["lease_commitment"] != following["lease_commitment"], "retry_not_fresh_attempt")
        retired.add(previous["assignment_id"])
        incoming.add(following["assignment_id"])
    current = {}
    for key, item in assignments.items():
        require(item["attempt"] == 1 or key in incoming, "missing_retry_history")
        if key not in retired:
            require(item["batch_number"] not in current, "multiple_current_assignments")
            current[item["batch_number"]] = item
    return current, by_hash


def duty_request(settings, duty, subscription):
    encode_fields(DUTY, duty)
    return typed_request("DutySuccessV1", DUTY, duty, "ZkSysProverService", settings["registry_chain_id"],
                         settings["registry"], subscription["operator"])


def offered_duty(settings, evidence, manifest, subscriptions, proof, *, enrollment=None):
    statements = validate_evidence(settings, evidence)
    assignments, by_hash = validate_manifest(settings, manifest, subscriptions, statements, enrollment=enrollment)
    exact(proof, ("batch_number", "vk_hash", "proof"))
    number = uint(proof["batch_number"], 32)
    require(proof["vk_hash"] == settings["vk_hash"], "proof_vk_mismatch")
    assignment = assignments.get(number)
    require(assignment, "proof_has_no_current_assignment")
    proof_bytes = decode_proof(proof["proof"])
    require(statements[number]["count"] > 0, "empty_batch_not_reportable")
    duty = {"account": assignment["account"], "subscriptionHash": assignment["subscription_hash"],
            "batchNumber": number, "statementHash": statements[number]["statement"], "friProofHash": keccak(proof_bytes),
            "transactionCount": statements[number]["count"], "period": assignment["period"],
            "slot": assignment["slot"], "attempt": assignment["attempt"], "assignmentId": assignment["assignment_id"]}
    return duty_request(settings, duty, by_hash[duty["subscriptionHash"]]), assignment


def prepare_offered_duty(settings, evidence, manifest, subscriptions, proof, *, enrollment=None):
    request, _ = offered_duty(settings, evidence, manifest, subscriptions, proof, enrollment=enrollment)
    return {**request, "native_acceptance": "pending"}


def prepare_duty(settings, evidence, manifest, subscriptions, authority, proof, *, enrollment=None):
    request, assignment = offered_duty(settings, evidence, manifest, subscriptions, proof, enrollment=enrollment)
    require(authority.get("schema_version") == 1 and authority.get("stage") == "FRI"
            and authority.get("status") == "accepted" and authority.get("vk_hash") == settings["vk_hash"],
            "native_fri_acceptance_required")
    # The protected node's accepted disposition is local input, never a renter's verified flag.
    number = uint(proof["batch_number"], 32)
    require(authority.get("bounds") == {"batch_number": number} and proof["vk_hash"] == settings["vk_hash"],
            "proof_authority_mismatch")
    require(assignment and assignment["lease_commitment"] == keccak(raw_hex(authority.get("lease_token"), 32)),
            "current_lease_binding_required")
    original_wire = {**proof, "lease_token": authority["lease_token"]}
    import hashlib
    require(authority.get("submission_sha256") == hashlib.sha256(canonical(original_wire)).hexdigest(),
            "native_accepted_proof_bytes_mismatch")
    return request


def decode_proof(value):
    require(isinstance(value, str) and len(value) <= MAX_FILE, "invalid_proof_size")
    try:
        result = base64.b64decode(value, validate=True)
    except ValueError:
        raise Error("invalid_base64_proof") from None
    require(result and base64.b64encode(result).decode() == value, "noncanonical_proof")
    return result


def proof_data(evidence, proof_bytes):
    require(len(proof_bytes) == 44 * 32, "invalid_v8_snark_length")
    previous = encode_fields(STORED, evidence["previous_batch"])
    batches = word(len(evidence["batches"])) + b"".join(encode_fields(STORED, b["stored"]) for b in evidence["batches"])
    words = word(46) + word(0x802) + word(0) + proof_bytes
    return b"\x01" + encode_dynamic_tuple([(previous, False), (batches, True), (words, True)])


def decode_proof_data(data):
    require(data[:1] == b"\x01" and len(data) <= 64 * 1024 and (len(data) - 1) % 32 == 0,
            "invalid_proof_data_encoding")
    words = [data[i:i + 32] for i in range(1, len(data), 32)]
    require(len(words) >= 12 and int.from_bytes(words[9], "big") == 11 * 32, "invalid_proof_data_offsets")
    count = int.from_bytes(words[11], "big")
    proof_start = 12 + 9 * count
    require(2 <= count <= 100 and len(words) == proof_start + 47
            and int.from_bytes(words[10], "big") == proof_start * 32
            and int.from_bytes(words[proof_start], "big") == 46
            and int.from_bytes(words[proof_start + 1], "big") == 0x802
            and int.from_bytes(words[proof_start + 2], "big") == 0, "invalid_native_v8_proof_data")

    def stored(at):
        result = {}
        for offset, (field, kind) in enumerate(STORED):
            value = words[at + offset]
            if kind == "bytes32":
                result[field] = "0x" + value.hex()
            elif kind == "uint256":
                result[field] = hex(int.from_bytes(value, "big"))
            else:
                result[field] = uint(int.from_bytes(value, "big"), int(kind[4:]))
        return result

    return stored(0), [stored(12 + 9 * index) for index in range(count)]


def prepare_package(settings, evidence, manifest, subscriptions, duties, proposal, snark, fri_payload, *, enrollment=None):
    statements = validate_evidence(settings, evidence)
    require(len(statements) >= 2, "wrapper_requires_two_batches")
    require(type(duties) is list and len(duties) <= len(statements), "invalid_duty_count")
    # Idle native proofs still require wrapper endorsement, without inventing a rewarded FRI duty.
    assignments, by_hash = validate_manifest(settings, manifest, subscriptions, statements, allow_empty=not duties,
                                            enrollment=enrollment)
    exact(fri_payload, ("from_batch_number", "to_batch_number", "vk_hash", "fri_proofs"))
    numbers = list(statements)
    require(fri_payload["from_batch_number"] == numbers[0] and fri_payload["to_batch_number"] == numbers[-1]
            and fri_payload["vk_hash"] == settings["vk_hash"] and type(fri_payload["fri_proofs"]) is list
            and len(fri_payload["fri_proofs"]) == len(numbers), "fri_payload_range_mismatch")
    fri_hashes = {number: keccak(decode_proof(value)) for number, value in zip(numbers, fri_payload["fri_proofs"])}
    seen, slots = set(), set()
    for duty in duties:
        encode_duty(duty)
        number = duty["batchNumber"]
        assignment = assignments.get(number)
        require(assignment and number not in seen, "duplicate_or_unassigned_duty")
        for duty_key, assignment_key in (("account", "account"), ("subscriptionHash", "subscription_hash"),
                                         ("period", "period"), ("slot", "slot"), ("attempt", "attempt"),
                                         ("assignmentId", "assignment_id"), ("statementHash", "statement_hash")):
            require(duty[duty_key] == assignment[assignment_key], "duty_assignment_mismatch")
        require(0 < duty["transactionCount"] == statements[number]["count"], "duty_transaction_count_mismatch")
        require(duty["friProofHash"] == fri_hashes[number], "duty_fri_artifact_mismatch")
        slot = (duty["account"], duty["period"], duty["slot"])
        require(slot not in slots, "duplicate_duty_slot")
        unsigned = {k: duty[k] for k, _ in DUTY}
        verify_eoa(duty_request(settings, unsigned, by_hash[duty["subscriptionHash"]]), duty["operatorSignature"])
        seen.add(number)
        slots.add(slot)
    exact(proposal, ("mode", "accepted_package", "candidate", "candidate_proof"))
    require(proposal["mode"] in ("bootstrap", "service"), "invalid_package_mode")
    accepted = dict(proposal["accepted_package"])
    generated = ("manifestHash", "reportHash", "proofHash")
    exact(accepted, [k for k, _ in PACKAGE if k not in generated])
    exact(snark, ("from_batch_number", "to_batch_number", "vk_hash", "proof"))
    require(snark["from_batch_number"] == numbers[0] and snark["to_batch_number"] == numbers[-1]
            and snark["vk_hash"] == settings["vk_hash"], "snark_range_or_vk_mismatch")
    data = proof_data(evidence, decode_proof(snark["proof"]))
    accepted.update(manifestHash=keccak(canonical(manifest)), reportHash=report_hash(duties), proofHash=keccak(data))
    encode_fields(PACKAGE, accepted)
    for key, expected in (("domainVersion", 1), ("protocolVersion", 32), ("policyHash", settings["policy_hash"]),
                           ("chainId", settings["execution_chain_id"]), ("chainAddress", settings["chain_address"]),
                           ("vkHash", settings["vk_hash"]), ("sequencer", settings["sequencer"]),
                           ("period", manifest["payload"]["period"]), ("batchFrom", numbers[0]), ("batchTo", numbers[-1])):
        require(accepted[key] == expected, "package_identity_mismatch")
    nonzero(accepted["parent"])
    nonzero(accepted["sequencerBeneficiary"], 20)
    require(accepted["batchTo"] < 2**64 - 1, "terminal_batch_number")
    candidate, candidate_proof = proposal["candidate"], proposal["candidate_proof"]
    if proposal["mode"] == "bootstrap":
        require(candidate is None and candidate_proof == [] and accepted["wrapper"] == ZERO_ADDRESS
                and accepted["wrapperBeneficiary"] == ZERO_ADDRESS and accepted["rosterRoot"] == ZERO
                and accepted["turn"] == 0, "invalid_bootstrap_package")
        domain_name, contract = "ZkSysProofGate", settings["proof_gate"]
    else:
        nonzero(settings["coordinator"], 20)
        require(type(candidate) is dict, "wrapper_candidate_required")
        check_candidate(candidate, candidate_proof, accepted["rosterRoot"])
        require(candidate["operator"] == accepted["wrapper"] != accepted["sequencer"]
                and candidate["beneficiary"] == accepted["wrapperBeneficiary"], "wrapper_identity_mismatch")
        domain_name, contract = "ZkSysWrapperCoordinator", settings["coordinator"]
    request = typed_request("AcceptedPackageV1", PACKAGE, accepted, domain_name, settings["settlement_chain_id"],
                            contract, settings["sequencer"])
    sidecar = {"accepted_package": accepted, "duties": duties, "batch_outputs": [b["output"] for b in evidence["batches"]],
               "candidate": candidate, "candidate_proof": candidate_proof, "sequencer_signature": "0x", "wrapper_signature": "0x"}
    return {"schema_version": 1, "mode": proposal["mode"], "sidecar": sidecar,
            "sequencer_request": request,
            "wrapper_request": None if proposal["mode"] == "bootstrap" else {**request, "signer": accepted["wrapper"]},
            "proof_data": "0x" + data.hex()}


def complete_package(settings, prepared, sequencer_signature, wrapper_signature):
    exact(prepared, ("schema_version", "mode", "sidecar", "sequencer_request", "wrapper_request", "proof_data"))
    require(prepared["schema_version"] == 1 and prepared["mode"] in ("service", "bootstrap"), "unsupported_package")
    sidecar = prepared["sidecar"]
    exact(sidecar, ("accepted_package", "duties", "batch_outputs", "candidate", "candidate_proof",
                    "sequencer_signature", "wrapper_signature"))
    require(sidecar["sequencer_signature"] == "0x" and sidecar["wrapper_signature"] == "0x", "package_already_signed")
    accepted = sidecar["accepted_package"]
    bootstrap = prepared["mode"] == "bootstrap"
    if not bootstrap:
        nonzero(settings["coordinator"], 20)
    request = typed_request("AcceptedPackageV1", PACKAGE, accepted,
                            "ZkSysProofGate" if bootstrap else "ZkSysWrapperCoordinator",
                            settings["settlement_chain_id"], settings["proof_gate"] if bootstrap else settings["coordinator"],
                            settings["sequencer"])
    require(request == prepared["sequencer_request"] and accepted["sequencer"] == settings["sequencer"],
            "signing_request_changed")
    require(keccak(raw_hex(prepared["proof_data"])) == accepted["proofHash"]
            and report_hash(sidecar["duties"]) == accepted["reportHash"], "signed_artifact_mismatch")
    previous, batches = decode_proof_data(raw_hex(prepared["proof_data"]))
    require(type(sidecar["batch_outputs"]) is list and len(sidecar["batch_outputs"]) == len(batches),
            "output_count_changed")
    evidence = {"schema_version": 1, "chain_id": accepted["chainId"], "chain_address": accepted["chainAddress"],
                "settlement_chain_id": settings["settlement_chain_id"], "protocol_version": accepted["protocolVersion"],
                "vk_hash": accepted["vkHash"], "previous_batch": previous,
                "batches": [{"stored": batch, "output": output} for batch, output in zip(batches, sidecar["batch_outputs"])]}
    statements = validate_evidence(settings, evidence)
    require(accepted["domainVersion"] == 1 and accepted["policyHash"] == settings["policy_hash"]
            and accepted["batchFrom"] == batches[0]["batchNumber"] and accepted["batchTo"] == batches[-1]["batchNumber"],
            "package_identity_mismatch")
    seen = set()
    for duty in sidecar["duties"]:
        number = duty["batchNumber"]
        require(number in statements and number not in seen and duty["period"] == accepted["period"]
                and duty["statementHash"] == statements[number]["statement"]
                and 0 < duty["transactionCount"] == statements[number]["count"], "signed_duty_evidence_mismatch")
        seen.add(number)
    verify_eoa(request, sequencer_signature)
    if bootstrap:
        require(prepared["wrapper_request"] is None and wrapper_signature == "0x" and sidecar["candidate"] is None
                and sidecar["candidate_proof"] == [] and accepted["wrapper"] == ZERO_ADDRESS
                and accepted["wrapperBeneficiary"] == ZERO_ADDRESS, "invalid_bootstrap_wrapper")
    else:
        wrapper = {**request, "signer": accepted["wrapper"]}
        require(wrapper == prepared["wrapper_request"], "wrapper_request_changed")
        check_candidate(sidecar["candidate"], sidecar["candidate_proof"], accepted["rosterRoot"])
        require(sidecar["candidate"]["operator"] == accepted["wrapper"]
                and sidecar["candidate"]["beneficiary"] == accepted["wrapperBeneficiary"], "wrapper_candidate_changed")
        verify_eoa(wrapper, wrapper_signature)
    return {**sidecar, "sequencer_signature": sequencer_signature, "wrapper_signature": wrapper_signature}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--config", required=True)
    parser.add_argument("--output", required=True)
    parser.add_argument("--registry-rpc-file", help="trusted child RPC connection for canonical enrollment consent")
    parser.add_argument("--enrollment-block-hash", help="independently pinned canonical journal enrollment block")
    commands = parser.add_subparsers(dest="command", required=True)
    subscription = commands.add_parser("subscription")
    subscription.add_argument("--subscription", required=True)
    operator_subscription = commands.add_parser("operator-subscription")
    operator_subscription.add_argument("--signed-subscription", required=True)
    enrollment = commands.add_parser("enroll")
    enrollment.add_argument("--signed-subscription", required=True)
    enrollment.add_argument("--operator-signature", required=True)
    manifest = commands.add_parser("manifest-request")
    manifest.add_argument("--payload", required=True)
    complete = commands.add_parser("complete")
    complete.add_argument("--prepared", required=True)
    complete.add_argument("--sequencer-signature", required=True)
    complete.add_argument("--wrapper-signature")
    renewal = commands.add_parser("renew-wrapper")
    renewal.add_argument("--signed-subscription", required=True)
    renewal.add_argument("--registry-snapshot", required=True)
    for name in ("duty", "offered-duty", "package"):
        command = commands.add_parser(name)
        for flag in ("evidence", "manifest", "subscriptions", "proof"):
            command.add_argument("--" + flag, required=True)
        if name == "duty":
            command.add_argument("--authority", required=True)
        elif name == "package":
            command.add_argument("--duties", required=True)
            command.add_argument("--proposal", required=True)
            command.add_argument("--fri-payload", required=True)
    args = parser.parse_args()
    try:
        settings = config(read_json(args.config))
        require(bool(args.registry_rpc_file) == bool(args.enrollment_block_hash), "complete_enrollment_context_required")
        enrollment = None
        if args.registry_rpc_file:
            require(args.command in ("duty", "offered-duty", "package"), "enrollment_context_not_used_by_command")
            import workflow_io
            enrollment = EnrollmentAuthority(settings, read_json(args.subscriptions),
                read_json(args.manifest)["payload"]["period"], workflow_io.wallet_connection(args.registry_rpc_file, 30),
                args.enrollment_block_hash)
        if args.command == "subscription":
            result = subscription_request(settings, read_json(args.subscription))
        elif args.command == "operator-subscription":
            result = operator_subscription_request(settings, read_json(args.signed_subscription))
        elif args.command == "enroll":
            result = prepare_enrollment(settings, read_json(args.signed_subscription),
                                        read(args.operator_signature).decode().strip())
        elif args.command == "manifest-request":
            payload = read_json(args.payload)
            result = manifest_request(settings, payload)
            result["manifest_payload"] = payload
        elif args.command == "complete":
            result = complete_package(settings, read_json(args.prepared), read(args.sequencer_signature).decode().strip(),
                                      read(args.wrapper_signature).decode().strip() if args.wrapper_signature else "0x")
        elif args.command == "renew-wrapper":
            result = prepare_renewal(settings, read_json(args.signed_subscription), read_json(args.registry_snapshot))
        elif args.command == "duty":
            result = prepare_duty(settings, read_json(args.evidence), read_json(args.manifest), read_json(args.subscriptions),
                                  read_json(args.authority, private=True), read_json(args.proof), enrollment=enrollment)
        elif args.command == "offered-duty":
            result = prepare_offered_duty(settings, read_json(args.evidence), read_json(args.manifest),
                                          read_json(args.subscriptions), read_json(args.proof), enrollment=enrollment)
        else:
            result = prepare_package(settings, read_json(args.evidence), read_json(args.manifest), read_json(args.subscriptions),
                                     read_json(args.duties), read_json(args.proposal), read_json(args.proof),
                                     read_json(args.fri_payload, maximum=256 * 1024 * 1024), enrollment=enrollment)
        write_new(args.output, result)
        print(json.dumps({"output": str(Path(args.output).absolute()), "command": args.command}))
    except (Error, OSError) as error:
        print(json.dumps({"error": str(error)}), file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
