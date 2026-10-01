import argparse
import base64
import copy
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
import urllib.parse
import urllib.request

import job
import runpod
import sentry
import worker
from test_runpod import FakeApi, policy


VK = "0x" + "a1" * 32
TOKEN = "0x" + "f1" * 32


def release(stage):
    return {"schema_version": 1, "stage": stage, "protocol_version": 32, "execution_version": 7,
            "proving_version": 8, "security_level": 100, "vk_hash": VK,
            "program_commitment": "0x" + "b1" * 32, "app_bin_sha256": "1" * 64,
            "app_text_sha256": "2" * 64, "worker_sha256": "3" * 64,
            "crs_sha256": "4" * 64 if stage == "SNARK" else None}


def payload(stage):
    if stage == "FRI":
        return {"batch_number": 12, "vk_hash": VK,
                "prover_input": base64.b64encode(bytes(range(16))).decode()}
    return {"from_batch_number": 12, "to_batch_number": 13, "vk_hash": VK,
            "fri_proofs": [base64.b64encode(v).decode() for v in (b"fri-one", b"fri-two")]}


def storage_plan():
    plan = {}
    for key, obj in (("payload", "payload"), ("manifest", "manifest"),
                     ("artifact", "proof"), ("result_manifest", "result")):
        for action in ("get", "put"):
            plan[f"{key}_{action}_url"] = f"https://storage.example/{obj}?{action}=JOB_SCOPED_SECRET"
    return plan


class Storage:
    def __init__(self):
        self.objects, self.puts = {}, []
        self.fail_artifact_upload = False

    def key(self, url):
        return urllib.parse.urlsplit(url).path

    def put(self, url, data, deadline=None):
        if self.key(url) == "/proof" and self.fail_artifact_upload:
            raise runpod.Error("upload_failed")
        self.puts.append(self.key(url))
        self.objects[self.key(url)] = data

    def get(self, url, maximum, deadline=None):
        data = self.objects[self.key(url)]
        if len(data) > maximum:
            raise runpod.Error("transport_body_too_large")
        return data

    def json(self, url, limit=runpod.JSON_LIMIT):
        return job.decode(self.get(url, limit))

    def transfer(self, url, output, limit):
        output.write(self.get(url, limit))


class TrustedNode:
    def __init__(self, stage):
        self.stage = stage
        self.calls = []
        self.submit_response = (204, {"x-syscoin-prover-disposition": "accepted"}, b"")

    def request(self, url, method="GET", data=None, authorization=None, maximum=None):
        self.calls.append((url, method, data, authorization))
        if "/pick?" in url:
            return 200, {}, job.encode({**payload(self.stage), "lease_token": TOKEN})
        if "/submit?" in url:
            wire = job.decode(data)
            assert wire["lease_token"] == TOKEN
            assert wire["vk_hash"] == VK
            return self.submit_response
        raise AssertionError("unexpected trusted endpoint")


def successful_native(stage, seen=None, corrupt_token=False):
    def run(args, directory, timeout):
        assert args[0] == job.BINARIES[stage]
        assert args[args.index("--iterations") + 1] == "1"
        assert "--disable-zk" not in args
        if stage == "SNARK":
            assert args[1] == "run-prover"
            assert args[args.index("--trusted-setup-file") + 1] == job.CRS
        endpoint = args[args.index("--sequencer-urls") + 1]
        query = urllib.parse.urlencode({"supported_vk_hashes": VK, "max_fri_pick_response_bytes": job.MAX_PICK["FRI"]})
        with urllib.request.urlopen(urllib.request.Request(endpoint + f"/prover-jobs/v1/{stage}/pick?{query}", method="POST")) as response:
            picked = json.load(response)
        assert picked["lease_token"] != TOKEN
        proof = {key: value for key, value in picked.items() if key not in ("prover_input", "fri_proofs")}
        proof["proof"] = base64.b64encode(b"f" * (64 if stage == "SNARK" else 41)).decode()
        if corrupt_token:
            proof["lease_token"] = TOKEN
        body = job.encode(proof)
        request = urllib.request.Request(endpoint + f"/prover-jobs/v1/{stage}/submit?id=rental-one-job",
                                         data=body, headers={"Content-Type": "application/json"}, method="POST")
        with urllib.request.urlopen(request) as response:
            assert response.status == 204
            assert response.headers["x-syscoin-prover-disposition"] == "accepted"
        with urllib.request.urlopen(request) as response:
            assert response.status == 204
        if seen is not None:
            seen.append((args, picked, proof))
    return run


class HandoffTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.storage = Storage()

    def prepare(self, stage):
        directory = self.root / stage
        self.node = TrustedNode(stage)
        self.assertTrue(sentry.pick(directory, "https://trusted.node/", job.encode(release(stage)),
                                    "duty-12-attempt-1", "Basic LOCAL_SECRET", self.node))
        sentry.export(directory, storage_plan(), self.storage)
        return directory

    def adapter(self, directory, stage, operation_id="c" * 32, native=None):
        selected = runpod.read_private_json(directory / "controller-job.json")
        args = argparse.Namespace(operation_id=operation_id, job_id=selected["job_id"],
                                  manifest_url=selected["manifest_url"], manifest_sha256=selected["manifest_sha256"],
                                  runtime_limit_seconds=30)
        worker.execute(args, self.storage, native or successful_native(stage),
                       directory / "release.json", verify=lambda _: None)
        return selected

    def test_fri_and_snark_roundtrip_through_real_wire_shapes_and_local_lease(self):
        for stage in ("FRI", "SNARK"):
            with self.subTest(stage=stage):
                directory = self.prepare(stage)
                store = runpod.Store.initialize(self.root / (stage + "-controller"), policy())
                store.heartbeat(1000)
                api = FakeApi()
                selected = runpod.read_private_json(directory / "controller-job.json")
                with store.lock("watchdog.lock"):
                    controller = runpod.Controller(store, api, clock=lambda: 1000, http=self.storage)
                    operation_id = controller.launch(selected)
                self.adapter(directory, stage, operation_id)
                controller.collect(operation_id)
                self.assertEqual(sentry.submit(directory, store, operation_id, "Basic LOCAL_SECRET", self.node), "accepted")
                for body in self.storage.objects.values():
                    self.assertNotIn(TOKEN.encode(), body)
                    self.assertNotIn(b"LOCAL_SECRET", body)
                    self.assertNotIn(b"trusted.node", body)
                self.assertEqual(self.storage.puts[-2:], ["/proof", "/result"])
                self.assertTrue((directory / "submission.json").exists())

    def test_submission_503_retains_same_exact_body_and_lease_for_restart_retry(self):
        directory = self.prepare("FRI")
        store = runpod.Store.initialize(self.root / "controller", policy())
        store.heartbeat(1000)
        with store.lock("watchdog.lock"):
            controller = runpod.Controller(store, FakeApi(), clock=lambda: 1000, http=self.storage)
            operation_id = controller.launch(runpod.read_private_json(directory / "controller-job.json"))
        self.adapter(directory, "FRI", operation_id)
        controller.collect(operation_id)
        self.node.submit_response = (503, {}, b"proxy unavailability")
        with self.assertRaisesRegex(runpod.Error, "submission_retained"):
            sentry.submit(directory, store, operation_id, "Basic LOCAL_SECRET", self.node)
        self.assertEqual(runpod.read_private_json(directory / "authority.json")["status"], "submission_pending")
        first = self.node.calls[-1][2]
        self.node.submit_response = (204, {"X-Syscoin-Prover-Disposition": "accepted"}, b"")
        self.assertEqual(sentry.submit(directory, store, operation_id, "Basic LOCAL_SECRET", self.node), "accepted")
        self.assertEqual(first, self.node.calls[-1][2])

    def test_public_generic_204_without_manager_disposition_is_not_accepted(self):
        directory = self.prepare("FRI")
        store = runpod.Store.initialize(self.root / "controller", policy())
        store.heartbeat(1000)
        with store.lock("watchdog.lock"):
            controller = runpod.Controller(store, FakeApi(), clock=lambda: 1000, http=self.storage)
            operation_id = controller.launch(runpod.read_private_json(directory / "controller-job.json"))
        self.adapter(directory, "FRI", operation_id)
        controller.collect(operation_id)
        self.node.submit_response = (204, {}, b"")
        with self.assertRaisesRegex(runpod.Error, "submission_retained"):
            sentry.submit(directory, store, operation_id, "Basic LOCAL_SECRET", self.node)

    def test_wrong_manifest_hash_fails_before_native_execution(self):
        directory = self.prepare("FRI")
        self.storage.objects["/manifest"] += b" "
        with self.assertRaisesRegex(runpod.Error, "manifest_hash_mismatch"):
            self.adapter(directory, "FRI", native=lambda *_: self.fail("must not run"))

    def test_stage_confusion_and_wrong_vk_are_rejected_before_compute(self):
        selected = release("FRI")
        manifest = {"schema_version": 1, "job_id": "duty", "stage": "SNARK",
                    "release_sha256": "1" * 64, "payload": {"url": "https://storage.example/input",
                    "sha256": "2" * 64, "bytes": 100}, "artifact_put_url": "https://storage.example/proof",
                    "result_manifest_put_url": "https://storage.example/result", "chain_binding": None}
        with self.assertRaisesRegex(runpod.Error, "manifest_release_mismatch"):
            worker.validate_manifest(manifest, selected, "1" * 64, "duty")
        with self.assertRaisesRegex(runpod.Error, "payload_vk_mismatch"):
            job.validate_payload(payload("FRI"), "FRI", "0x" + "c1" * 32)

    def test_real_token_cannot_complete_the_local_adapter(self):
        directory = self.prepare("FRI")
        with self.assertRaises(urllib.error.HTTPError) as caught:
            self.adapter(directory, "FRI", native=successful_native("FRI", corrupt_token=True))
        caught.exception.close()
        self.assertNotIn("/proof", self.storage.objects)
        self.assertNotIn("/result", self.storage.objects)

    def test_changed_input_bytes_fail_before_native_execution(self):
        directory = self.prepare("FRI")
        self.storage.objects["/payload"] = self.storage.objects["/payload"].replace(b"12", b"13", 1)
        with self.assertRaisesRegex(runpod.Error, "input_hash_or_size_mismatch"):
            self.adapter(directory, "FRI", native=lambda *_: self.fail("must not run"))

    def test_failed_proof_upload_does_not_publish_completion(self):
        directory = self.prepare("SNARK")
        self.storage.fail_artifact_upload = True
        with self.assertRaisesRegex(runpod.Error, "upload_failed"):
            self.adapter(directory, "SNARK")
        self.assertNotIn("/result", self.storage.objects)

    def test_pick_transport_failure_leaves_reservation_and_prevents_repick(self):
        directory = self.root / "uncertain"
        node = TrustedNode("FRI")
        with patch.object(node, "request", side_effect=runpod.Error("transport_failure")):
            with self.assertRaises(runpod.Error):
                sentry.pick(directory, "https://trusted.node/", job.encode(release("FRI")), "duty", "secret", node)
        self.assertEqual(runpod.read_private_json(directory / "authority.json")["status"], "pick_uncertain")
        with self.assertRaises(FileExistsError):
            sentry.pick(directory, "https://trusted.node/", job.encode(release("FRI")), "duty", "secret", node)

    def test_pick_recovery_uses_durable_wire_without_new_request(self):
        directory = self.root / "recover"
        node = TrustedNode("FRI")
        with patch.object(sentry, "recover_pick", side_effect=OSError("crash after wire fsync")):
            with self.assertRaises(OSError):
                sentry.pick(directory, "https://trusted.node/", job.encode(release("FRI")), "duty", "secret", node)
        sentry.recover_pick(directory)
        self.assertEqual(len(node.calls), 1)
        self.assertEqual(runpod.read_private_json(directory / "authority.json")["lease_token"], TOKEN)
        self.assertNotIn(TOKEN.encode(), (directory / "payload.json").read_bytes())

    def test_export_plan_cannot_silently_change_after_manifest_binding(self):
        directory = self.prepare("FRI")
        changed = storage_plan()
        changed["artifact_put_url"] += "changed"
        with self.assertRaisesRegex(runpod.Error, "export_plan_changed"):
            sentry.export(directory, changed, self.storage)

    def test_proof_range_tampering_fails_before_real_lease_submission(self):
        directory = self.prepare("FRI")
        store = runpod.Store.initialize(self.root / "controller", policy())
        store.heartbeat(1000)
        with store.lock("watchdog.lock"):
            controller = runpod.Controller(store, FakeApi(), clock=lambda: 1000, http=self.storage)
            operation_id = controller.launch(runpod.read_private_json(directory / "controller-job.json"))
        self.adapter(directory, "FRI", operation_id)
        proof = job.decode(self.storage.objects["/proof"])
        proof["batch_number"] = 99
        self.storage.objects["/proof"] = job.encode(proof)
        manifest = job.decode(self.storage.objects["/result"])
        manifest.update(artifact_sha256=job.hash_bytes(self.storage.objects["/proof"]),
                        artifact_bytes=len(self.storage.objects["/proof"]))
        self.storage.objects["/result"] = job.encode(manifest)
        controller.collect(operation_id)
        count = len(self.node.calls)
        with self.assertRaisesRegex(runpod.Error, "returned_proof_range_mismatch"):
            sentry.submit(directory, store, operation_id, "Basic LOCAL_SECRET", self.node)
        self.assertEqual(len(self.node.calls), count)

    def test_dispatcher_input_roundtrip_never_acquires_or_exports_upstream_authority(self):
        for stage in ("FRI", "SNARK"):
            with self.subTest(stage=stage):
                directory = self.root / ("offered-" + stage)
                sentry.export_input(directory, job.encode(payload(stage)), job.encode(release(stage)),
                                    "signed-offer-12", storage_plan(), self.storage)
                self.assertFalse((directory / "authority.json").exists())
                self.assertFalse((directory / "picked-wire.json").exists())
                store = runpod.Store.initialize(self.root / ("offered-controller-" + stage), policy())
                store.heartbeat(1000)
                with store.lock("watchdog.lock"):
                    controller = runpod.Controller(store, FakeApi(), clock=lambda: 1000, http=self.storage)
                    operation_id = controller.launch(runpod.read_private_json(directory / "controller-job.json"))
                self.adapter(directory, stage, operation_id)
                controller.collect(operation_id)
                output = directory / "returned.json"
                digest = sentry.verify_input_result(directory, store, operation_id, output)
                self.assertEqual(job.hash_bytes(output.read_bytes()), digest)
                metadata = runpod.read_private_json(directory / "input.json")
                self.assertNotIn("lease_token", metadata)
                self.assertNotIn("status", metadata)
                self.assertNotIn("lease_token", job.decode(output.read_bytes()))
                self.assertEqual(sentry.verify_input_result(directory, store, operation_id, output), digest)
                for body in self.storage.objects.values():
                    self.assertNotIn(TOKEN.encode(), body)

    def test_dispatcher_input_rejects_lease_payload_and_changed_export(self):
        directory = self.root / "offered"
        with self.assertRaises(runpod.Error):
            sentry.export_input(directory, job.encode({**payload("FRI"), "lease_token": TOKEN}),
                                job.encode(release("FRI")), "offer", storage_plan(), self.storage)
        self.assertFalse(directory.exists())
        sentry.export_input(directory, job.encode(payload("FRI")), job.encode(release("FRI")),
                            "offer", storage_plan(), self.storage)
        changed = {**payload("FRI"), "batch_number": 13}
        with self.assertRaisesRegex(runpod.Error, "frozen_input_changed"):
            sentry.export_input(directory, job.encode(changed), job.encode(release("FRI")),
                                "offer", storage_plan(), self.storage)
        plan = storage_plan()
        plan["artifact_get_url"] += "different"
        with self.assertRaisesRegex(runpod.Error, "export_result_locations_changed"):
            sentry.export_input(directory, job.encode(payload("FRI")), job.encode(release("FRI")), "offer", plan, self.storage)

    def external_snark(self):
        retained, external = self.root / "retained", self.root / "external"
        self.node = TrustedNode("SNARK")
        sentry.pick(retained, "https://trusted.node/", job.encode(release("SNARK")), "wrapper-12",
                    "Basic LOCAL_SECRET", self.node)
        evidence = {"schema_version": 1, "chain_id": "0x1", "chain_address": "0x" + "12" * 20,
                    "settlement_chain_id": "0x2", "protocol_version": 32, "vk_hash": VK,
                    "previous_batch": {"batchNumber": 11},
                    "batches": [{"stored": {"batchNumber": n}} for n in (12, 13)]}
        binding = {key: evidence[key] for key in
                   ("chain_id", "chain_address", "settlement_chain_id", "protocol_version", "vk_hash")}
        binding.update(lane="child", evidence_sha256=job.hash_bytes(job.encode(evidence)),
                       payload_sha256=job.hash_bytes(job.encode(payload("SNARK"))),
                       origin_endpoint_sha256=job.hash_bytes(b"https://trusted.node/"))
        sentry.export_input(external, job.encode(payload("SNARK")), job.encode(release("SNARK")),
                            "wrapper-12", storage_plan(), self.storage, binding)
        store = runpod.Store.initialize(self.root / "external-controller", policy())
        store.heartbeat(1000)
        with store.lock("watchdog.lock"):
            controller = runpod.Controller(store, FakeApi(), clock=lambda: 1000, http=self.storage)
            operation = controller.launch(runpod.read_private_json(external / "controller-job.json"))
        self.adapter(external, "SNARK", operation)
        controller.collect(operation)
        sentry.verify_input_result(external, store, operation, external / "returned-proof.json")
        return retained, external, store, operation, evidence

    def test_external_snark_import_binds_original_lease_and_retries_exact_submission(self):
        retained, external, store, operation, evidence = self.external_snark()
        with self.assertRaisesRegex(runpod.Error, "rental_job_mismatch"):
            sentry.submit(retained, store, operation, "Basic LOCAL_SECRET", self.node)
        digest = sentry.import_result(retained, external, store, operation, evidence, "child")
        self.assertEqual(sentry.import_result(retained, external, store, operation, evidence, "child"), digest)
        self.assertNotIn(TOKEN.encode(), (external / "input.json").read_bytes())
        self.node.submit_response = (503, {}, b"interrupted")
        with self.assertRaisesRegex(runpod.Error, "submission_retained"):
            sentry.submit(retained, store, operation, "Basic LOCAL_SECRET", self.node)
        original = self.node.calls[-1][2]
        self.node.submit_response = (204, {"x-syscoin-prover-disposition": "accepted"}, b"")
        self.assertEqual(sentry.submit(retained, store, operation, "Basic LOCAL_SECRET", self.node), "accepted")
        self.assertEqual(original, self.node.calls[-1][2])
        self.assertEqual(job.decode(original)["lease_token"], TOKEN)

    def test_external_snark_import_rejects_wrong_origin_chain_job_and_changed_artifact(self):
        retained, external, store, operation, evidence = self.external_snark()
        original = runpod.read_private_json(external / "input.json")
        cases = [("job_id", "other-job"), ("release_sha256", "f" * 64),
                 ("operation_id", "b" * 32), ("returned_artifact_sha256", "e" * 64)]
        for field, value in cases:
            changed = {**original, field: value}
            runpod.atomic_json(external / "input.json", changed)
            with self.subTest(field=field), self.assertRaises(runpod.Error):
                sentry.import_result(retained, external, store, operation, evidence, "child")
        for field, value in (("origin_endpoint_sha256", "a" * 64), ("chain_id", "0x3")):
            changed = copy.deepcopy(original)
            changed["chain_binding"][field] = value
            runpod.atomic_json(external / "input.json", changed)
            with self.subTest(field=field), self.assertRaisesRegex(runpod.Error, "external_input_identity_mismatch"):
                sentry.import_result(retained, external, store, operation, evidence, "child")
        runpod.atomic_json(external / "input.json", original)
        with self.assertRaisesRegex(runpod.Error, "external_input_identity_mismatch"):
            sentry.import_result(retained, external, store, operation, evidence, "gateway")
        self.assertIsNone(runpod.read_private_json(retained / "authority.json")["manifest_sha256"])
        self.assertEqual(len(self.node.calls), 1)


class AdapterValidationTests(unittest.TestCase):
    def test_release_rejects_zero_vk_wrong_security_and_wrong_stage(self):
        for field, value in (("vk_hash", "0x" + "0" * 64), ("security_level", 80), ("stage", "shell")):
            selected = release("FRI")
            selected[field] = value
            with self.assertRaises(runpod.Error):
                job.release_identity(job.encode(selected))

    def test_payload_rejects_real_lease_and_nonconsecutive_count(self):
        with self.assertRaisesRegex(runpod.Error, "invalid_fields"):
            job.validate_payload({**payload("FRI"), "lease_token": TOKEN}, "FRI", VK)
        wrong = payload("SNARK")
        wrong["to_batch_number"] = 15
        with self.assertRaisesRegex(runpod.Error, "fri_count_mismatch"):
            job.validate_payload(wrong, "SNARK", VK)

    def test_image_verification_hashes_exact_guest_and_native_binary(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            binary, guest, text = root / "worker", root / "app.bin", root / "app.text"
            binary.write_bytes(b"worker-code")
            binary.chmod(0o700)
            guest.write_bytes(b"guest")
            text.write_bytes(b"text")
            selected = release("FRI")
            selected.update(worker_sha256=job.hash_bytes(binary.read_bytes()),
                            app_bin_sha256=job.hash_bytes(guest.read_bytes()),
                            app_text_sha256=job.hash_bytes(text.read_bytes()))
            with patch.dict(job.BINARIES, FRI=str(binary)), patch.object(job, "GUEST_BIN", str(guest)), patch.object(job, "GUEST_TEXT", str(text)):
                job.verify_image(selected)
                guest.write_bytes(b"changed")
                with self.assertRaisesRegex(runpod.Error, "image_file_hash_mismatch"):
                    job.verify_image(selected)

    def test_native_environment_does_not_inherit_secrets(self):
        class Process:
            def wait(self, timeout=None):
                return 0
            def poll(self):
                return 0
        with tempfile.TemporaryDirectory() as temporary, patch.dict(os.environ, {
            "RUNPOD_API_KEY": "provider", "PRIVATE_KEY": "staking", "ZKSYNC_SEQUENCER_URLS": "secret",
        }), patch.object(worker.subprocess, "Popen", return_value=Process()) as popen:
            worker.run_native(["fixed-worker"], Path(temporary), 1)
        env = popen.call_args.kwargs["env"]
        self.assertNotIn("RUNPOD_API_KEY", env)
        self.assertNotIn("PRIVATE_KEY", env)
        self.assertNotIn("ZKSYNC_SEQUENCER_URLS", env)


if __name__ == "__main__":
    unittest.main()
