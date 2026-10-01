#!/usr/bin/env python3
"""Trusted-side lease export and exact proof reimport; keep this off rental hosts."""

import argparse
import base64
import os
from pathlib import Path
import re
import sys
import urllib.parse

import job
from runpod import (Controller, Error, Store, atomic_json, exact_fields, https_url,
                    read_private_json, require, sync_dir)

MAX_EVIDENCE = 2 * 1024 * 1024


def endpoint_url(value):
    job.native_url(value)
    parsed = urllib.parse.urlsplit(value)
    require(not parsed.query, "sequencer_endpoint_must_not_have_query")
    return value.rstrip("/") + "/"


def authorization(secret_file=None):
    if secret_file:
        secret = job.read_file(secret_file, 4096, private=True).decode().strip()
    else:
        secret = os.environ.get("PROVER_RENTAL_BASIC_AUTH", "")
    require(":" in secret and not any(ord(c) < 32 for c in secret), "basic_auth_required")
    return "Basic " + base64.b64encode(secret.encode()).decode()


def private_directory(path, create=False):
    path = Path(path)
    require(path.is_absolute(), "job_directory_must_be_absolute")
    if create:
        path.mkdir(mode=0o700)
        sync_dir(path.parent)
    Store(path)
    return path


def pick(directory, endpoint, release_bytes, job_id, auth, network):
    release = job.release_identity(release_bytes)
    directory = private_directory(directory, create=True)
    require(isinstance(job_id, str) and re.fullmatch(r"[A-Za-z0-9_.:-]{1,128}", job_id),
            "invalid_job_id")
    endpoint = endpoint_url(endpoint)
    authority = {"schema_version": 1, "status": "pick_uncertain", "endpoint": endpoint,
                 "job_id": job_id, "stage": release["stage"], "release_sha256": job.hash_bytes(release_bytes),
                 "vk_hash": release["vk_hash"], "lease_token": None, "manifest_sha256": None,
                 "operation_id": None, "submission_sha256": None}
    atomic_json(directory / "authority.json", authority)
    job.write_new(directory / "release.json", release_bytes)
    query = urllib.parse.urlencode({"id": "rental-sentry", "supported_vk_hashes": release["vk_hash"],
                                    "max_fri_pick_response_bytes": job.MAX_PICK["FRI"]})
    status, _, raw = network.request(endpoint + "prover-jobs/v1/" + release["stage"] + "/pick?" + query,
                                      "POST", authorization=auth, maximum=job.MAX_PICK[release["stage"]])
    if status == 204:
        authority["status"] = "no_job"
        atomic_json(directory / "authority.json", authority)
        return False
    # Any other response leaves a visible reservation: neither proxy failures nor an
    # interrupted response can prove that a real server lease was never acquired.
    require(status == 200, "pick_outcome_uncertain_preserve_directory")
    job.write_new(directory / "picked-wire.json", raw)
    recover_pick(directory)
    return True


def recover_pick(directory):
    directory = private_directory(directory)
    authority = read_private_json(directory / "authority.json")
    require(authority["status"] in ("picked", "pick_uncertain"), "pick_not_recoverable")
    wire = job.decode(job.read_file(directory / "picked-wire.json", job.MAX_PICK[authority["stage"]], private=True))
    require(isinstance(wire, dict), "invalid_pick")
    token = job.b256(wire.pop("lease_token", None))
    payload = job.validate_payload(wire, authority["stage"], authority["vk_hash"])
    # Persist authority before exporting anything. The full server capability never enters
    # payload.json or the manifest that the rental receives.
    require(authority["lease_token"] in (None, token), "pick_authority_changed")
    encoded = job.encode(payload)
    destination = directory / "payload.json"
    if destination.exists():
        require(job.read_file(destination, job.MAX_PICK[authority["stage"]], private=True) == encoded,
                "retained_payload_changed")
    else:
        job.write_new(destination, encoded)
    authority["lease_token"] = token
    authority["status"] = "picked"
    authority["bounds"] = {key: value for key, value in payload.items()
                           if key in ("batch_number", "from_batch_number", "to_batch_number")}
    atomic_json(directory / "authority.json", authority)


def export(directory, plan, network):
    directory = private_directory(directory)
    authority = read_private_json(directory / "authority.json")
    require(authority["status"] == "picked", "job_not_exportable")
    publish_payload(directory, authority, "authority.json", plan, network)


def validate_storage_plan(plan):
    fields = ("payload_put_url", "payload_get_url", "manifest_put_url", "manifest_get_url",
              "artifact_put_url", "artifact_get_url", "result_manifest_put_url", "result_manifest_get_url")
    exact_fields(plan, fields)
    for url in plan.values():
        https_url(url)


def publish_payload(directory, authority, metadata_name, plan, network):
    validate_storage_plan(plan)
    payload = job.read_file(directory / "payload.json", job.MAX_PICK[authority["stage"]], private=True)
    job.validate_payload(job.decode(payload), authority["stage"], authority["vk_hash"])
    manifest = {"schema_version": 1, "job_id": authority["job_id"], "stage": authority["stage"],
                "release_sha256": authority["release_sha256"],
                "payload": {"url": plan["payload_get_url"], "sha256": job.hash_bytes(payload), "bytes": len(payload)},
                "artifact_put_url": plan["artifact_put_url"], "result_manifest_put_url": plan["result_manifest_put_url"],
                "chain_binding": None}
    if "chain_binding" in authority:
        binding = job.validate_chain_binding(authority["chain_binding"])
        require(binding["payload_sha256"] == job.hash_bytes(payload)
                and binding["vk_hash"] == authority["vk_hash"], "chain_binding_payload_changed")
        manifest["chain_binding"] = binding
    raw = job.encode(manifest)
    digest = job.hash_bytes(raw)
    require(authority["manifest_sha256"] in (None, digest), "export_plan_changed")
    authority["manifest_sha256"] = digest
    atomic_json(directory / metadata_name, authority)
    atomic_json(directory / "manifest.json", manifest)
    rental_job = {"schema_version": 1, "job_id": authority["job_id"], "manifest_url": plan["manifest_get_url"],
                  "manifest_sha256": digest, "result_manifest_url": plan["result_manifest_get_url"],
                  "result_artifact_url": plan["artifact_get_url"]}
    existing = directory / "controller-job.json"
    if existing.exists():
        require(read_private_json(existing) == rental_job, "export_result_locations_changed")
    atomic_json(existing, rental_job)
    network.put(plan["payload_put_url"], payload)
    network.put(plan["manifest_put_url"], raw)


def validate_input(payload_bytes, release_bytes, job_id):
    release = job.release_identity(release_bytes)
    payload = job.validate_payload(job.decode(payload_bytes), release["stage"], release["vk_hash"])
    require(isinstance(job_id, str) and re.fullmatch(r"[A-Za-z0-9_.:-]{1,128}", job_id), "invalid_job_id")
    return release, payload


def export_input(directory, payload_bytes, release_bytes, job_id, plan, network, chain_binding=None):
    release, payload = validate_input(payload_bytes, release_bytes, job_id)
    validate_storage_plan(plan)
    payload_bytes = job.encode(payload)
    directory = private_directory(directory, create=not Path(directory).exists())
    with Store(directory).lock("sentry.lock"):
        require(not (directory / "authority.json").exists(), "input_must_not_have_upstream_authority")
        identity = {"schema_version": 1, "kind": "compute_input", "job_id": job_id, "stage": release["stage"],
                    "release_sha256": job.hash_bytes(release_bytes), "payload_sha256": job.hash_bytes(payload_bytes),
                    "vk_hash": release["vk_hash"], "bounds": {key: value for key, value in payload.items()
                    if key in ("batch_number", "from_batch_number", "to_batch_number")}}
        if chain_binding is not None:
            identity["chain_binding"] = job.validate_chain_binding(chain_binding)
            require(chain_binding["payload_sha256"] == job.hash_bytes(payload_bytes)
                    and chain_binding["vk_hash"] == release["vk_hash"], "chain_binding_payload_changed")
        path = directory / "input.json"
        if path.exists():
            metadata = read_private_json(path)
            require(all(metadata.get(key) == value for key, value in identity.items()), "frozen_input_changed")
        else:
            require(not (directory / "manifest.json").exists() and not (directory / "controller-job.json").exists(),
                    "input_export_state_missing")
            metadata = {**identity, "manifest_sha256": None, "operation_id": None, "returned_artifact_sha256": None}
            atomic_json(path, metadata)
        for name, raw in (("release.json", release_bytes), ("payload.json", payload_bytes)):
            target = directory / name
            if target.exists():
                require(job.read_file(target, job.MAX_PICK[release["stage"]], private=True) == raw, "frozen_input_file_changed")
            else:
                job.write_new(target, raw)
        publish_payload(directory, metadata, "input.json", plan, network)


def collected_proof(authority, controller_store, operation_id):
    with controller_store.lock():
        controller = Controller(controller_store, None)
        operation = controller.operation(operation_id)
        require(operation["job"]["job_id"] == authority["job_id"]
                and operation["job"]["manifest_sha256"] == authority["manifest_sha256"], "rental_job_mismatch")
        controller.verify_receipt(operation_id)
        raw = job.read_file(controller_store.root / (operation_id + ".proof"), job.MAX_SUBMIT, private=True)
    proof = job.validate_payload(job.decode(raw), authority["stage"], authority["vk_hash"], proof=True)
    require(all(proof.get(key) == value for key, value in authority["bounds"].items()), "returned_proof_range_mismatch")
    return raw, proof


def verify_input_result(directory, controller_store, operation_id, output):
    directory = private_directory(directory)
    require(not (directory / "authority.json").exists(), "input_must_not_have_upstream_authority")
    metadata = read_private_json(directory / "input.json")
    require(metadata.get("kind") == "compute_input", "not_a_compute_input")
    raw, _ = collected_proof(metadata, controller_store, operation_id)
    digest = job.hash_bytes(raw)
    require(metadata["operation_id"] in (None, operation_id)
            and metadata["returned_artifact_sha256"] in (None, digest), "returned_input_attempt_changed")
    metadata.update(operation_id=operation_id, returned_artifact_sha256=digest)
    atomic_json(directory / "input.json", metadata)
    output = Path(output)
    if output.exists():
        require(job.read_file(output, job.MAX_SUBMIT, private=True) == raw, "returned_output_changed")
    else:
        job.write_new(output, raw)
    return digest


def import_result(directory, input_directory, controller_store, operation_id, evidence, lane):
    """Bind a separately computed SNARK to the sequencer's original private lease."""
    directory = private_directory(directory)
    input_directory = private_directory(input_directory)
    require(directory != input_directory and not (input_directory / "authority.json").exists(),
            "external_input_must_not_own_lease")
    authority = read_private_json(directory / "authority.json")
    require(authority["stage"] == "SNARK" and authority["status"] in
            ("picked", "submission_pending", "accepted", "rejected"), "snark_job_not_importable")
    exact_fields(evidence, ("schema_version", "chain_id", "chain_address", "settlement_chain_id",
                            "protocol_version", "vk_hash", "previous_batch", "batches"))
    require(evidence["schema_version"] == 1 and evidence["vk_hash"] == authority["vk_hash"],
            "retained_evidence_identity_mismatch")
    payload_raw = job.read_file(directory / "payload.json", job.MAX_PICK["SNARK"], private=True)
    payload = job.validate_payload(job.decode(payload_raw), "SNARK", authority["vk_hash"])
    require(payload_raw == job.encode(payload) and authority["bounds"] == {
            key: payload[key] for key in ("from_batch_number", "to_batch_number")}, "retained_payload_changed")
    require(evidence["previous_batch"]["batchNumber"] == payload["from_batch_number"] - 1
            and [entry["stored"]["batchNumber"] for entry in evidence["batches"]]
            == list(range(payload["from_batch_number"], payload["to_batch_number"] + 1)),
            "retained_evidence_range_mismatch")
    binding = job.validate_chain_binding({key: evidence[key] for key in
        ("chain_id", "chain_address", "settlement_chain_id", "protocol_version", "vk_hash")} | {
        "lane": lane, "evidence_sha256": job.hash_bytes(job.encode(evidence)),
        "payload_sha256": job.hash_bytes(payload_raw),
        "origin_endpoint_sha256": job.hash_bytes(endpoint_url(authority["endpoint"]).encode())})
    release_raw = job.read_file(directory / "release.json", job.MAX_MANIFEST, private=True)
    require(job.hash_bytes(release_raw) == authority["release_sha256"], "retained_release_changed")
    job.release_identity(release_raw)
    with Store(input_directory).lock("sentry.lock"):
        metadata = read_private_json(input_directory / "input.json")
        require(metadata.get("kind") == "compute_input" and metadata.get("schema_version") == 1,
                "not_a_compute_input")
        require(all(metadata.get(key) == authority[key] for key in
                    ("job_id", "stage", "release_sha256", "vk_hash", "bounds"))
                and metadata.get("payload_sha256") == binding["payload_sha256"]
                and metadata.get("chain_binding") == binding, "external_input_identity_mismatch")
        for name, expected, maximum in (("payload.json", payload_raw, job.MAX_PICK["SNARK"]),
                                       ("release.json", release_raw, job.MAX_MANIFEST)):
            require(job.read_file(input_directory / name, maximum, private=True) == expected,
                    "external_input_file_changed")
        manifest_raw = job.read_file(input_directory / "manifest.json", job.MAX_MANIFEST, private=True)
        manifest = job.decode(manifest_raw)
        require(job.hash_bytes(manifest_raw) == metadata["manifest_sha256"]
                and manifest["schema_version"] == 1 and manifest["chain_binding"] == binding
                and manifest["job_id"] == authority["job_id"] and manifest["stage"] == "SNARK"
                and manifest["release_sha256"] == authority["release_sha256"]
                and manifest["payload"]["sha256"] == binding["payload_sha256"]
                and manifest["payload"]["bytes"] == len(payload_raw), "external_manifest_changed")
        raw, _ = collected_proof(metadata, controller_store, operation_id)
        digest = job.hash_bytes(raw)
        require(metadata["operation_id"] == operation_id and metadata["returned_artifact_sha256"] == digest,
                "external_result_not_finalized")
    require(authority["manifest_sha256"] in (None, metadata["manifest_sha256"])
            and authority["operation_id"] in (None, operation_id)
            and authority.get("chain_binding") in (None, binding)
            and authority.get("imported_artifact_sha256") in (None, digest), "imported_attempt_changed")
    # The native verifier still decides acceptance. This only fixes which returned
    # artifact may be submitted using the already retained lease.
    authority.update(manifest_sha256=metadata["manifest_sha256"], operation_id=operation_id,
                     chain_binding=binding, imported_artifact_sha256=digest)
    atomic_json(directory / "authority.json", authority)
    return digest


def submit(directory, controller_store, operation_id, auth, network):
    directory = private_directory(directory)
    authority = read_private_json(directory / "authority.json")
    require(authority["status"] in ("picked", "submission_pending", "accepted", "rejected"), "job_not_submittable")
    if authority["status"] in ("accepted", "rejected"):
        return authority["status"]
    raw, proof = collected_proof(authority, controller_store, operation_id)
    require(authority.get("imported_artifact_sha256") in (None, job.hash_bytes(raw)),
            "imported_artifact_changed")
    wire = job.encode({**proof, "lease_token": authority["lease_token"]})
    require(len(wire) <= job.MAX_SUBMIT, "submission_too_large")
    digest = job.hash_bytes(wire)
    require(authority["submission_sha256"] in (None, digest)
            and authority["operation_id"] in (None, operation_id), "submission_attempt_changed")
    submission_path = directory / "submission.json"
    if submission_path.exists():
        require(job.read_file(submission_path, job.MAX_SUBMIT, private=True) == wire, "durable_submission_mismatch")
    else:
        job.write_new(submission_path, wire)
    authority.update(status="submission_pending", submission_sha256=digest, operation_id=operation_id)
    atomic_json(directory / "authority.json", authority)
    status, headers, _ = network.request(authority["endpoint"] + "prover-jobs/v1/" + authority["stage"]
                                          + "/submit?id=rental-sentry", "POST", wire, auth)
    disposition = {k.lower(): v for k, v in headers.items()}.get("x-syscoin-prover-disposition")
    if status == 204 and disposition == "accepted":
        authority["status"] = "accepted"
    elif status in (400, 409, 413, 422) and disposition == "rejected":
        authority["status"] = "rejected"
    else:
        raise Error("submission_retained_retry_same_bytes_after_recovery")
    # The trusted node's existing native FRI verifier or SNARK preflight is the
    # cryptographic gate. No rental assertion can manufacture this disposition.
    atomic_json(directory / "authority.json", authority)
    return authority["status"]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--execute", action="store_true")
    parser.add_argument("--job-dir", required=True)
    parser.add_argument("--auth-file")
    commands = parser.add_subparsers(dest="command", required=True)
    acquire = commands.add_parser("pick")
    acquire.add_argument("--endpoint", required=True)
    acquire.add_argument("--release", required=True)
    acquire.add_argument("--job-id", required=True)
    publish = commands.add_parser("export")
    publish.add_argument("--storage-plan", required=True)
    input_export = commands.add_parser("export-input")
    input_export.add_argument("--payload", required=True)
    input_export.add_argument("--release", required=True)
    input_export.add_argument("--job-id", required=True)
    input_export.add_argument("--storage-plan", required=True)
    input_result = commands.add_parser("verify-result")
    input_result.add_argument("--controller-state", required=True)
    input_result.add_argument("--operation-id", required=True)
    input_result.add_argument("--output", required=True)
    imported = commands.add_parser("import-result")
    imported.add_argument("--input-dir", required=True)
    imported.add_argument("--controller-state", required=True)
    imported.add_argument("--operation-id", required=True)
    imported.add_argument("--evidence", required=True)
    imported.add_argument("--lane", choices=("child", "gateway"), required=True)
    commands.add_parser("recover-pick")
    complete = commands.add_parser("submit")
    complete.add_argument("--controller-state", required=True)
    complete.add_argument("--operation-id", required=True)
    args = parser.parse_args()
    if not args.execute:
        if args.command == "export-input":
            selected_release = job.read_file(args.release, job.MAX_MANIFEST)
            selected_payload = job.read_file(args.payload, max(job.MAX_PICK.values()))
            validate_input(selected_payload, selected_release, args.job_id)
            validate_storage_plan(read_private_json(args.storage_plan))
        print("dry-run: no lease, transfer or submission")
        return
    network = job.NativeNetwork()
    if args.command == "export-input":
        export_input(args.job_dir, job.read_file(args.payload, max(job.MAX_PICK.values())),
                     job.read_file(args.release, job.MAX_MANIFEST), args.job_id,
                     read_private_json(args.storage_plan), network)
        print("compute_input_exported_without_upstream_lease")
    elif args.command == "pick":
        result = pick(args.job_dir, args.endpoint, job.read_file(args.release, job.MAX_MANIFEST),
                      args.job_id, authorization(args.auth_file), network)
        print("picked" if result else "no_job")
    else:
        store = Store(args.job_dir)
        with store.lock("sentry.lock"):
            if args.command == "recover-pick":
                recover_pick(args.job_dir)
                print("pick_recovered")
            elif args.command == "export":
                export(args.job_dir, read_private_json(args.storage_plan), network)
                print("exported")
            elif args.command == "verify-result":
                verify_input_result(args.job_dir, Store(args.controller_state), args.operation_id, args.output)
                print("compute_result_retained_pending_native_verification")
            elif args.command == "import-result":
                import_result(args.job_dir, args.input_dir, Store(args.controller_state), args.operation_id,
                              job.decode(job.read_file(args.evidence, MAX_EVIDENCE)), args.lane)
                print("external_snark_bound_to_retained_lease_pending_native_verification")
            else:
                result = submit(args.job_dir, Store(args.controller_state), args.operation_id,
                                authorization(args.auth_file), network)
                print(result)


if __name__ == "__main__":
    os.umask(0o077)
    try:
        main()
    except (Error, OSError, ValueError, TypeError, KeyError) as error:
        print(str(error) if isinstance(error, Error) else "sentry_handoff_failed", file=sys.stderr)
        sys.exit(1)
