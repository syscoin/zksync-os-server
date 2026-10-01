"""Reattach a remotely checked SNARK to its original, private native lease."""

from pathlib import Path
import sys

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


def publication_work(root, settings, limits, evidence, proof):
    """A work file is only a handoff candidate; the node later authenticates settlement."""
    root = io.directory(root)
    expected_data = "0x" + s.proof_data(evidence, s.decode_proof(proof["proof"])).hex()
    matches = []
    entries = sorted(root.iterdir())
    s.require(len(entries) <= 1024, "publication_scan_capacity")
    for child in entries:
        if child.name.startswith(".") or not child.is_dir() or child.is_symlink():
            continue
        path = child / "work.json"
        if not path.exists():
            continue
        io.directory(child)
        work = io.private_json(path, 2 * 1024 * 1024)
        if work.get("batch_from") != proof["from_batch_number"] or work.get("batch_to") != proof["to_batch_number"]:
            continue
        expected = {"schema_version": 1, "execution_chain_id": settings["execution_chain_id"],
                    "settlement_chain_id": settings["settlement_chain_id"], "chain_address": settings["chain_address"],
                    "gate": settings["proof_gate"], "gate_code_hash": limits["gate_code_hash"],
                    "coordinator": settings["coordinator"], "coordinator_code_hash": limits["coordinator_code_hash"],
                    "policy_hash": settings["policy_hash"], "production_vk_hash": settings["vk_hash"],
                    "sequencer": settings["sequencer"], "proof_data": expected_data,
                    "batch_outputs": [batch["output"] for batch in evidence["batches"]]}
        s.require(all(work.get(key) == value for key, value in expected.items()), "node_publication_work_mismatch")
        s.require(s.uint(work["required_confirmations"], 64) <= limits["confirmations"],
                  "relay_confirmations_below_node_policy")
        s.nonzero(work["timelock"], 20)
        matches.append(child)
    s.require(len(matches) <= 1, "ambiguous_node_publication_work")
    return matches[0] if matches else None
