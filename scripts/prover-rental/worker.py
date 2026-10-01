#!/usr/bin/env python3
"""Run one authenticated FRI/SNARK payload with the existing standalone worker binary."""

import argparse
import hmac
from http.server import BaseHTTPRequestHandler, HTTPServer
import os
from pathlib import Path
import re
import secrets
import signal
import subprocess
import sys
import tempfile
import threading
import time
import urllib.parse

import job
from runpod import Error, exact_fields, https_url, positive_int, require, sha256


def validate_manifest(manifest, release, release_hash, job_id):
    fields = ("schema_version", "job_id", "stage", "release_sha256", "payload",
              "artifact_put_url", "result_manifest_put_url", "chain_binding")
    exact_fields(manifest, fields)
    require(manifest["schema_version"] == 1 and manifest["job_id"] == job_id,
            "manifest_job_mismatch")
    require(manifest["stage"] == release["stage"] and manifest["release_sha256"] == release_hash,
            "manifest_release_mismatch")
    exact_fields(manifest["payload"], ("url", "sha256", "bytes"))
    require(positive_int(manifest["payload"]["bytes"]) <= job.MAX_PICK[release["stage"]], "input_too_large")
    sha256(manifest["payload"]["sha256"])
    if manifest["chain_binding"] is not None:
        binding = job.validate_chain_binding(manifest["chain_binding"])
        require(binding["vk_hash"] == release["vk_hash"]
                and binding["payload_sha256"] == manifest["payload"]["sha256"], "manifest_chain_binding_mismatch")
    for url in (manifest["payload"]["url"], manifest["artifact_put_url"], manifest["result_manifest_put_url"]):
        https_url(url)


class OneJob:
    def __init__(self, payload, release, output):
        self.payload, self.release, self.output = payload, release, output
        self.token = "0x" + secrets.token_hex(32)
        self.picked = False
        self.result = None

    def submit(self, value):
        require(self.picked, "submission_before_pick")
        require(isinstance(value, dict) and isinstance(value.get("lease_token"), str)
                and hmac.compare_digest(value["lease_token"], self.token), "wrong_local_token")
        proof = {key: field for key, field in value.items() if key != "lease_token"}
        job.validate_payload(proof, self.release["stage"], self.release["vk_hash"], proof=True)
        for key in ("batch_number", "from_batch_number", "to_batch_number", "vk_hash"):
            if key in self.payload:
                require(proof.get(key) == self.payload[key], "submission_job_mismatch")
        encoded = job.encode(proof)
        if self.result is not None:
            require(self.result == encoded, "conflicting_local_submission")
        else:
            job.write_new(self.output, encoded)
            self.result = encoded


def serve_once(work):
    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *_):
            pass

        def reply(self, status, body=b"", disposition=None):
            self.send_response(status)
            self.send_header("Content-Length", str(len(body)))
            self.send_header("Content-Type", "application/json")
            self.send_header("Cache-Control", "no-store")
            if disposition:
                self.send_header("x-syscoin-prover-disposition", disposition)
            if status == 204 and not disposition:
                self.send_header("x-syscoin-prover-pick-outcome", "unleased")
            self.end_headers()
            self.wfile.write(body)

        def do_GET(self):
            if self.path in ("/prover-jobs/v1/status/fri", "/prover-jobs/v1/status/snark"):
                self.reply(200, b"[]")
            else:
                self.reply(404)

        def do_POST(self):
            self.connection.settimeout(10)
            route = urllib.parse.urlsplit(self.path)
            prefix = "/prover-jobs/v1/" + work.release["stage"]
            try:
                if route.path == prefix + "/pick":
                    if work.picked:
                        self.reply(204)
                        return
                    query = urllib.parse.parse_qs(route.query)
                    require(work.release["vk_hash"] in query.get("supported_vk_hashes", [""])[0].split(","),
                            "worker_vk_advertisement_mismatch")
                    payload = job.encode({**work.payload, "lease_token": work.token})
                    if work.release["stage"] == "FRI":
                        require(len(payload) <= int(query.get("max_fri_pick_response_bytes", [0])[0]),
                                "worker_response_capacity_too_small")
                    work.picked = True
                    self.reply(200, payload)
                elif route.path == prefix + "/submit":
                    require(self.headers.get("Transfer-Encoding") is None, "chunked_submit_unsupported")
                    length = int(self.headers.get("Content-Length", "0"))
                    require(0 < length <= job.MAX_SUBMIT, "invalid_submit_size")
                    body = self.rfile.read(length)
                    require(len(body) == length, "short_submit_body")
                    work.submit(job.decode(body))
                    # This isolated adapter acknowledges durable compute output only. It has no
                    # genuine server capability and cannot accept a chain proof or award credit.
                    self.reply(204, disposition="accepted")
                else:
                    self.reply(404)
            except (Error, OSError, ValueError):
                self.reply(422, disposition="rejected")

    server = HTTPServer(("127.0.0.1", 0), Handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    return server, thread


def command(release, endpoint, directory):
    args = [job.BINARIES[release["stage"]]]
    if release["stage"] == "SNARK":
        args += ["run-prover", "--trusted-setup-file", job.CRS, "--output-dir", str(directory)]
    args += ["--sequencer-urls", endpoint, "--app-bin-path", job.GUEST_BIN, "--iterations", "1",
             "--submission-dir", str(directory / "local-submissions"), "--prover-name", "rental-one-job",
             "--prometheus-port", "0"]
    return args


def run_native(args, directory, timeout):
    # The child receives runtime settings only, even if an operator accidentally started the
    # adapter in an environment containing provider or wallet credentials.
    env = {name: os.environ[name] for name in (
        "PATH", "LD_LIBRARY_PATH", "CUDA_VISIBLE_DEVICES", "NVIDIA_VISIBLE_DEVICES",
        "NVIDIA_DRIVER_CAPABILITIES") if name in os.environ}
    env["RUST_MIN_STACK"] = "268435456"
    process = subprocess.Popen(args, cwd=directory, env=env, stdout=subprocess.DEVNULL,
                               stderr=subprocess.DEVNULL, start_new_session=True)
    try:
        require(process.wait(timeout=timeout) == 0, "native_worker_failed")
    finally:
        if process.poll() is None:
            os.killpg(process.pid, signal.SIGKILL)
            process.wait()


def execute(args, network=None, native=run_native, release_path=job.RELEASE_PATH, verify=job.verify_image):
    network = network or job.Network()
    deadline = time.monotonic() + positive_int(args.runtime_limit_seconds)
    require(re.fullmatch(r"[0-9a-f]{32}", args.operation_id), "invalid_operation_id")
    require(re.fullmatch(r"[A-Za-z0-9_.:-]{1,128}", args.job_id), "invalid_job_id")
    release_bytes = job.read_file(release_path, job.MAX_MANIFEST)
    release = job.release_identity(release_bytes)
    verify(release)
    raw = network.get(https_url(args.manifest_url), job.MAX_MANIFEST, deadline)
    require(job.hash_bytes(raw) == sha256(args.manifest_sha256), "manifest_hash_mismatch")
    manifest = job.decode(raw)
    validate_manifest(manifest, release, job.hash_bytes(release_bytes), args.job_id)
    payload_bytes = network.get(manifest["payload"]["url"], manifest["payload"]["bytes"], deadline)
    require(len(payload_bytes) == manifest["payload"]["bytes"]
            and job.hash_bytes(payload_bytes) == manifest["payload"]["sha256"], "input_hash_or_size_mismatch")
    payload = job.validate_payload(job.decode(payload_bytes), release["stage"], release["vk_hash"])
    with tempfile.TemporaryDirectory(prefix="zksys-rental-") as temporary:
        directory = Path(temporary)
        work = OneJob(payload, release, directory / "proof.json")
        server, thread = serve_once(work)
        try:
            native(command(release, f"http://127.0.0.1:{server.server_port}", directory), directory,
                   max(0.01, deadline - time.monotonic()))
        finally:
            server.shutdown()
            server.server_close()
            thread.join(timeout=2)
        require(work.result is not None, "native_worker_did_not_return_proof")
        network.put(manifest["artifact_put_url"], work.result, deadline)
        result = {"schema_version": 1, "operation_id": args.operation_id, "job_id": args.job_id,
                  "manifest_sha256": args.manifest_sha256, "artifact_sha256": job.hash_bytes(work.result),
                  "artifact_bytes": len(work.result)}
        network.put(manifest["result_manifest_put_url"], job.encode(result), deadline)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--validate-image", action="store_true")
    parser.add_argument("--operation-id")
    parser.add_argument("--job-id")
    parser.add_argument("--manifest-url")
    parser.add_argument("--manifest-sha256")
    parser.add_argument("--runtime-limit-seconds", type=int)
    args = parser.parse_args()
    if args.validate_image:
        job.verify_image(job.release_identity(job.read_file(job.RELEASE_PATH, job.MAX_MANIFEST)))
    else:
        execute(args)


if __name__ == "__main__":
    os.umask(0o077)
    try:
        main()
    except (Error, OSError, ValueError, TypeError, KeyError, subprocess.TimeoutExpired) as error:
        print(str(error) if isinstance(error, Error) else "rental_worker_failed", file=sys.stderr)
        sys.exit(1)
