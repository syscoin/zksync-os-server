"""Reattach a remotely checked SNARK to its original, private native lease."""

from pathlib import Path
import sys

import relay as r
import service as s
import workflow_io as io

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "prover-rental"))
import job
import sentry
from runpod import atomic_json, read_private_json


def submit(directory, proof, auth, network):
    directory = sentry.private_directory(directory)
    authority = read_private_json(directory / "authority.json")
    s.require(authority["stage"] == "SNARK" and authority["status"] in
              ("picked", "submission_pending", "accepted", "rejected"), "native_lease_not_submittable")
    proof = job.validate_payload(proof, "SNARK", authority["vk_hash"], proof=True)
    s.require({key: proof[key] for key in ("from_batch_number", "to_batch_number")} == authority["bounds"],
              "returned_snark_range_changed")
    wire = job.encode({**proof, "lease_token": authority["lease_token"]})
    digest = job.hash_bytes(wire)
    s.require(authority["submission_sha256"] in (None, digest), "native_submission_already_frozen")
    io.immutable_bytes(directory / "submission.json", wire, job.MAX_SUBMIT)
    if authority["status"] in ("accepted", "rejected"):
        s.require(authority["submission_sha256"] == digest, "native_terminal_submission_changed")
        return authority["status"]
    previously_pending = authority["status"] == "submission_pending"
    authority.update(status="submission_pending", submission_sha256=digest)
    atomic_json(directory / "authority.json", authority)
    status, headers, _ = network.request(authority["endpoint"] + "prover-jobs/v1/SNARK/submit?id=selected-wrapper",
                                          "POST", wire, auth)
    disposition = {key.lower(): value for key, value in headers.items()}.get("x-syscoin-prover-disposition")
    if status == 204 and disposition == "accepted":
        authority["status"] = "accepted"
    elif status in (400, 413, 422) and disposition == "rejected" or (
            status == 409 and disposition == "rejected" and not previously_pending):
        authority["status"] = "rejected"
    else:
        raise s.Error("native_submission_uncertain_retry_identical_bytes")
    atomic_json(directory / "authority.json", authority)
    return authority["status"]


def publication_directories(root):
    root = io.directory(root)
    entries = sorted(root.iterdir())
    s.require(len(entries) <= 1024, "publication_scan_capacity")
    return [child for child in entries if not child.name.startswith(".") and child.is_dir() and not child.is_symlink()]


def publication_pins(settings, limits):
    return {"schema_version": 1, "execution_chain_id": settings["execution_chain_id"],
            "settlement_chain_id": settings["settlement_chain_id"], "chain_address": settings["chain_address"],
            "gate": settings["proof_gate"], "gate_code_hash": limits["gate_code_hash"],
            "coordinator": settings["coordinator"], "coordinator_code_hash": limits["coordinator_code_hash"],
            "policy_hash": settings["policy_hash"], "production_vk_hash": settings["vk_hash"],
            "sequencer": settings["sequencer"]}


def regrouped_work(root, retained, settings, limits, rpc, now, payload, evidence=None, proof=None):
    """A node disposition retires local work. Only the pinned native chain supplies its frontier."""
    root = io.directory(root)
    if retained is not None:
        pinned_dir = io.directory(retained)
        s.require(pinned_dir.parent == root, "publication_outside_configured_directory")
        candidates = [pinned_dir]
    else:
        candidates = publication_directories(root)
    start, end = payload["from_batch_number"], payload["to_batch_number"]
    pins = publication_pins(settings, limits)
    for child in candidates:
        marker_file = child / "regrouped.json"
        if not marker_file.exists():
            continue
        io.directory(child)
        work_file = child / "work.json"
        s.require(work_file.lstat().st_nlink == 1, "linked_handoff_file_forbidden")
        data = s.read(work_file, private=True, maximum=2 * 1024 * 1024)
        work = s.decode_json(data)
        if (work.get("batch_from"), work.get("batch_to")) != (start, end):
            continue
        s.require(all(work.get(key) == value for key, value in pins.items()), "node_publication_work_mismatch")
        if evidence is not None and work["batch_outputs"] != [batch["output"] for batch in evidence["batches"]]:
            continue
        if proof is not None and work["proof_data"] != "0x" + s.proof_data(evidence, s.decode_proof(proof["proof"])).hex():
            continue
        disposition = io.private_json(marker_file)
        s.exact(disposition, ("schema_version", "work_hash", "native_proved", "native_batch_hash"))
        s.require(disposition["schema_version"] == 1 and disposition["work_hash"] == s.keccak(data), "regrouped_work_binding_changed")
        frontier = s.uint(disposition["native_proved"], 64)
        s.require(frontier + 1 < start, "invalid_regrouped_frontier")
        head, anchor = r.anchored_head(rpc, settings, limits, now)
        chain = settings["chain_address"]
        gate = settings["proof_gate"]
        s.require(s.keccak(s.raw_hex(rpc.call("eth_getCode", [gate, anchor]))) == limits["gate_code_hash"]
                  and r.call_word(rpc, gate, "chain()", anchor, kind="address") == chain, "gate_configuration_changed")
        tip = r.call_word(rpc, chain, "getTotalBatchesVerified()", anchor)
        # A later advance can overtake the marker. Once this exact range is next,
        # the native planner may legitimately issue a fresh lease with the same bounds.
        retired = frontier <= tip and (retained is not None or tip + 1 < start)
        if retired:
            s.require(r.call_word(rpc, chain, "storedBatchHash(uint256)", anchor, s.word(frontier), "bytes32")
                      == s.nonzero(disposition["native_batch_hash"]), "regrouped_native_hash_changed")
        r.check_anchor(rpc, settings, head)
        if retired:
            return child
    return None


def publication_work(root, settings, limits, evidence, proof):
    """A work file is only a handoff candidate; the node later authenticates settlement."""
    expected_data = "0x" + s.proof_data(evidence, s.decode_proof(proof["proof"])).hex()
    matches = []
    for child in publication_directories(root):
        path = child / "work.json"
        if not path.exists() or (child / "superseded.json").exists() or (child / "regrouped.json").exists():
            continue
        io.directory(child)
        work = io.private_json(path, 2 * 1024 * 1024)
        if work.get("batch_from") != proof["from_batch_number"] or work.get("batch_to") != proof["to_batch_number"]:
            continue
        expected = {**publication_pins(settings, limits), "proof_data": expected_data,
                    "batch_outputs": [batch["output"] for batch in evidence["batches"]]}
        s.require(all(work.get(key) == value for key, value in expected.items()), "node_publication_work_mismatch")
        s.require(s.uint(work["required_confirmations"], 64) <= limits["confirmations"],
                  "relay_confirmations_below_node_policy")
        s.nonzero(work["timelock"], 20)
        matches.append(child)
    s.require(len(matches) <= 1, "ambiguous_node_publication_work")
    return matches[0] if matches else None
