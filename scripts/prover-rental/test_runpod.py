#!/usr/bin/env python3
"""Run with python3 -m unittest discover -s scripts/prover-rental -p 'test_*.py'."""

from contextlib import redirect_stdout
import copy
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch


SPEC = importlib.util.spec_from_file_location("rental_runpod", Path(__file__).with_name("runpod.py"))
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


def policy():
    return {
        "schema_version": 1, "image": "example/prover@sha256:" + "a" * 64,
        "gpu": {"id": "TEST GPU", "count": 1, "minRamPerGpu": 96,
                "minVcpuCountPerGpu": 8, "minCudaVersion": "12.9"},
        "min_vram_gb": 32, "disk_gb": 50, "cloud": "SECURE", "data_center_ids": ["TEST-1"],
        "additional_hourly_usd": "0.10",
        "limits": {"max_concurrent_pods": 1, "max_runtime_seconds": 3600,
                   "max_hourly_usd": "2", "max_operation_usd": "2", "lifetime_budget_usd": "4",
                   "max_artifact_bytes": 1024, "watchdog_interval_seconds": 10,
                   "watchdog_stale_seconds": 60},
    }


def job(job_id="test-1"):
    return {"schema_version": 1, "job_id": job_id,
            "manifest_url": "https://inputs.example/job?one-job-secret=PRIVATE",
            "manifest_sha256": "b" * 64,
            "result_manifest_url": "https://results.example/manifest?read=SECRET",
            "result_artifact_url": "https://results.example/proof?read=SECRET"}


class FakeApi:
    def __init__(self):
        self.pods = {}
        self.calls = []
        self.create_error = None
        self.delete_error = None
        self.hide_all = False
        self.hide_get = False
        self.quote = "0.50"

    def catalog(self, gpu_id):
        self.calls.append(("catalog", gpu_id))
        return {"id": gpu_id, "manufacturer": "NVIDIA", "memory": 32,
                "price": {"secure": self.quote}}

    def create(self, request):
        self.calls.append(("create", copy.deepcopy(request)))
        pod = copy.deepcopy(request)
        pod.update({"id": "pod_1", "status": "RUNNING", "cost": 0.5, "cudaVersion": "12.9",
                    "dataCenterId": "TEST-1"})
        pod["gpu"].update({"vcpuCount": 8, "memory": 96})
        self.pods[pod["id"]] = pod
        if self.create_error:
            raise self.create_error
        return copy.deepcopy(pod)

    def get(self, pod_id):
        self.calls.append(("get", pod_id))
        return None if self.hide_get else copy.deepcopy(self.pods.get(pod_id))

    def all_pods(self):
        self.calls.append(("list",))
        return [] if self.hide_all else copy.deepcopy(list(self.pods.values()))

    def delete(self, pod_id):
        self.calls.append(("delete", pod_id))
        del self.pods[pod_id]
        if self.delete_error:
            raise self.delete_error


class FakeHttp:
    def __init__(self):
        self.manifest = None
        self.artifact = b"exact-proof-bytes"
        self.partial_error = False
        self.calls = []

    def json(self, url, **kwargs):
        self.calls.append(("manifest", url))
        if self.manifest is None:
            raise MODULE.HttpError(404)
        return copy.deepcopy(self.manifest)

    def transfer(self, url, output, limit):
        self.calls.append(("artifact", url))
        output.write(self.artifact)
        if self.partial_error:
            raise MODULE.Error("interrupted_transfer")

    def complete(self, operation_id, selected_job):
        self.manifest = {"schema_version": 1, "operation_id": operation_id,
                         "job_id": selected_job["job_id"], "manifest_sha256": selected_job["manifest_sha256"],
                         "artifact_sha256": hashlib.sha256(self.artifact).hexdigest(),
                         "artifact_bytes": len(self.artifact)}


class ControllerTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name) / "controller"
        self.store = MODULE.Store.initialize(self.root, policy())
        self.api, self.http = FakeApi(), FakeHttp()
        self.now = 1000
        self.watchdog_lock = self.store.lock("watchdog.lock")
        self.watchdog_lock.__enter__()
        self.addCleanup(self.watchdog_lock.__exit__, None, None, None)
        self.store.heartbeat(self.now)

    def controller(self):
        return MODULE.Controller(self.store, self.api, lambda: self.now, self.http)

    def launch(self, selected_job=None):
        return self.controller().launch(selected_job or job())

    def receipt(self, operation_id):
        self.http.complete(operation_id, job())
        self.assertTrue(self.controller().collect(operation_id))

    def count(self, call):
        return len([v for v in self.api.calls if v[0] == call])

    def test_dry_run_never_constructs_provider_client_or_mutates_state(self):
        job_file = Path(self.temp.name) / "job.json"
        MODULE.atomic_json(job_file, job())
        before = (self.root / "state.json").read_bytes()
        with patch.object(MODULE, "Runpod", side_effect=AssertionError("no network")), redirect_stdout(io.StringIO()) as output:
            MODULE.main(["--state-dir", str(self.root), "launch", "--job", str(job_file)])
        self.assertNotIn("PRIVATE", output.getvalue())
        self.assertNotIn("SECRET", output.getvalue())
        self.assertEqual(before, (self.root / "state.json").read_bytes())

    def test_create_is_once_per_job_even_after_restart(self):
        operation_id = self.launch()
        self.assertEqual(operation_id, self.launch())
        self.assertEqual(self.count("create"), 1)
        request = self.api.calls[1][1]
        self.assertTrue(request["image"].endswith("a" * 64))
        self.assertEqual(set(request["env"]), {"ZKSYS_RENTAL_CONTROLLER_ID", "ZKSYS_RENTAL_OPERATION_ID"})
        self.assertNotIn("mounts", request)
        self.assertNotIn("templateId", request)

    def test_intent_is_durable_before_provider_call_and_interrupt(self):
        def interrupt(_):
            self.assertEqual(next(iter(self.store.load()["operations"].values()))["status"], "create_uncertain")
            raise KeyboardInterrupt
        with patch.object(self.api, "create", side_effect=interrupt), self.assertRaises(KeyboardInterrupt):
            self.launch()
        self.launch()
        self.assertEqual(self.count("create"), 0)
        self.assertEqual(len(self.store.load()["operations"]), 1)

    def test_ambiguous_create_adopts_exact_markers_without_reposting(self):
        self.api.create_error = MODULE.Error("http_transport_failure")
        operation_id = self.launch()
        self.assertEqual(self.store.load()["operations"][operation_id]["status"], "create_uncertain")
        self.controller().reconcile(operation_id)
        self.assertEqual(self.store.load()["operations"][operation_id]["pod_id"], "pod_1")
        self.launch()
        self.assertEqual(self.count("create"), 1)

    def test_empty_reconciliation_keeps_reservation_and_never_reposts(self):
        self.api.create_error = MODULE.HttpError(503)
        operation_id = self.launch()
        self.api.hide_all = True
        self.controller().reconcile(operation_id)
        self.assertEqual(self.store.load()["operations"][operation_id]["status"], "create_uncertain")
        self.launch()
        with self.assertRaisesRegex(MODULE.Error, "concurrency_limit"):
            self.launch(job("another"))
        self.assertEqual(self.count("create"), 1)

    def test_transient_get_absence_does_not_free_concurrency(self):
        operation_id = self.launch()
        self.api.hide_get = True
        self.controller().reconcile(operation_id)
        self.assertEqual(self.store.load()["operations"][operation_id]["status"], "active")
        with self.assertRaisesRegex(MODULE.Error, "concurrency_limit"):
            self.launch(job("another"))

    def test_label_collision_is_not_adopted_or_deleted(self):
        self.api.create_error = MODULE.Error("http_transport_failure")
        operation_id = self.launch()
        self.api.pods["pod_1"]["env"]["ZKSYS_RENTAL_CONTROLLER_ID"] = "other"
        with self.assertRaisesRegex(MODULE.Error, "ownership_mismatch"):
            self.controller().terminate(operation_id, failure=True)
        self.assertEqual(self.count("delete"), 0)

    def test_mismatched_known_pod_is_not_deleted(self):
        operation_id = self.launch()
        self.api.pods["pod_1"]["image"] = "attacker/other"
        with self.assertRaisesRegex(MODULE.Error, "ownership_mismatch"):
            self.controller().terminate(operation_id, failure=True)
        self.assertEqual(self.count("delete"), 0)

    def test_duplicate_matching_allocations_fail_closed(self):
        self.api.create_error = MODULE.Error("http_transport_failure")
        operation_id = self.launch()
        other = copy.deepcopy(self.api.pods["pod_1"])
        other["id"] = "pod_2"
        self.api.pods["pod_2"] = other
        with self.assertRaisesRegex(MODULE.Error, "multiple_matching"):
            self.controller().reconcile(operation_id)
        self.assertEqual(self.count("delete"), 0)

    def test_delete_requires_durable_receipt_or_explicit_failure(self):
        operation_id = self.launch()
        with self.assertRaisesRegex(MODULE.Error, "cleanup_requires"):
            self.controller().terminate(operation_id)
        self.controller().terminate(operation_id, failure=True)
        self.assertEqual(self.store.load()["operations"][operation_id]["status"], "terminated")
        self.assertEqual(self.count("delete"), 1)

    def test_ambiguous_delete_reconciles_before_any_retry(self):
        operation_id = self.launch()
        self.receipt(operation_id)
        self.api.delete_error = MODULE.Error("http_transport_failure")
        with self.assertRaises(MODULE.Error):
            self.controller().terminate(operation_id)
        self.assertEqual(self.store.load()["operations"][operation_id]["status"], "delete_uncertain")
        self.controller().terminate(operation_id)
        self.assertEqual(self.count("delete"), 1)
        self.assertEqual(self.store.load()["operations"][operation_id]["status"], "terminated")

    def test_failed_delete_retry_checks_ownership_again(self):
        operation_id = self.launch()
        with patch.object(self.api, "delete", side_effect=MODULE.Error("http_transport_failure")):
            with self.assertRaises(MODULE.Error):
                self.controller().terminate(operation_id, failure=True)
        self.api.pods["pod_1"]["env"]["ZKSYS_RENTAL_OPERATION_ID"] = "another-operation"
        with self.assertRaisesRegex(MODULE.Error, "ownership_mismatch"):
            self.controller().terminate(operation_id)
        self.assertEqual(self.count("delete"), 0)

    def test_receipt_binds_job_and_exact_bytes_but_not_proof_validity(self):
        operation_id = self.launch()
        self.receipt(operation_id)
        op = self.store.load()["operations"][operation_id]
        self.assertEqual((self.root / (operation_id + ".proof")).read_bytes(), self.http.artifact)
        self.assertFalse(op["receipt"]["proof_verified"])
        self.controller().terminate(operation_id)
        self.assertEqual(self.count("delete"), 1)

    def test_wrong_job_manifest_cannot_authorize_cleanup(self):
        operation_id = self.launch()
        self.http.complete(operation_id, job("wrong"))
        with self.assertRaisesRegex(MODULE.Error, "result_job_mismatch"):
            self.controller().collect(operation_id)
        self.assertIsNone(self.store.load()["operations"][operation_id]["receipt"])
        self.assertEqual(self.count("delete"), 0)

    def test_hash_mismatch_or_interrupted_copy_never_receipts(self):
        operation_id = self.launch()
        self.http.complete(operation_id, job())
        self.http.artifact = b"malicious-or-corrupt"
        with self.assertRaisesRegex(MODULE.Error, "artifact_"):
            self.controller().collect(operation_id)
        self.assertIsNone(self.store.load()["operations"][operation_id]["receipt"])
        self.assertFalse((self.root / (operation_id + ".proof")).exists())
        self.http.complete(operation_id, job())
        self.http.partial_error = True
        with self.assertRaisesRegex(MODULE.Error, "interrupted_transfer"):
            self.controller().collect(operation_id)
        self.assertFalse((self.root / (operation_id + ".proof")).exists())

    def test_receipt_restart_recovers_fsynced_artifact_without_redownload(self):
        operation_id = self.launch()
        self.http.complete(operation_id, job())
        with patch.object(self.store, "save", side_effect=OSError("fsync failed")):
            with self.assertRaises(OSError):
                self.controller().collect(operation_id)
        self.assertTrue((self.root / (operation_id + ".proof")).exists())
        self.assertIsNone(self.store.load()["operations"][operation_id]["receipt"])
        self.controller().collect(operation_id)
        self.assertEqual(len([call for call in self.http.calls if call[0] == "artifact"]), 1)

    def test_local_artifact_corruption_blocks_normal_termination(self):
        operation_id = self.launch()
        self.receipt(operation_id)
        (self.root / (operation_id + ".proof")).write_bytes(b"corrupt")
        with self.assertRaisesRegex(MODULE.Error, "artifact_hash"):
            self.controller().terminate(operation_id)
        self.assertEqual(self.count("delete"), 0)

    def test_watchdog_cleans_deadline_failure_without_proof(self):
        operation_id = self.launch()
        self.now += 3600
        self.controller().tick(operation_id)
        op = self.store.load()["operations"][operation_id]
        self.assertEqual(op["status"], "terminated")
        self.assertEqual(op["cleanup_reason"], "runtime_deadline_or_clock_rollback")

    def test_unknown_provider_status_cleans_owned_pod_before_deadline(self):
        operation_id = self.launch()
        self.api.pods["pod_1"]["status"] = "PAUSED"
        self.controller().tick(operation_id)
        op = self.store.load()["operations"][operation_id]
        self.assertEqual(op["status"], "terminated")
        self.assertEqual(op["cleanup_reason"], "unknown_provider_status")
        self.assertEqual(self.count("delete"), 1)

    def test_missing_provider_status_cleans_owned_pod(self):
        operation_id = self.launch()
        del self.api.pods["pod_1"]["status"]
        self.controller().tick(operation_id)
        self.assertEqual(self.count("delete"), 1)
        self.assertEqual(self.store.load()["operations"][operation_id]["cleanup_reason"],
                         "unknown_provider_status")

    def test_unhashable_provider_status_allows_explicit_failure_cleanup(self):
        operation_id = self.launch()
        self.api.pods["pod_1"]["status"] = []
        self.controller().terminate(operation_id, failure=True)
        self.assertEqual(self.count("delete"), 1)
        self.assertEqual(self.store.load()["operations"][operation_id]["status"], "terminated")

    def test_unknown_status_cannot_bypass_owned_pod_check(self):
        operation_id = self.launch()
        self.api.pods["pod_1"]["status"] = "PAUSED"
        self.api.pods["pod_1"]["env"]["ZKSYS_RENTAL_OPERATION_ID"] = "another-operation"
        self.now += 3600
        with self.assertRaisesRegex(MODULE.Error, "pod_ownership_mismatch"):
            self.controller().tick(operation_id)
        with self.assertRaisesRegex(MODULE.Error, "pod_ownership_mismatch"):
            self.controller().terminate(operation_id, failure=True)
        self.assertEqual(self.count("delete"), 0)

    def test_watchdog_collects_before_stopping_successful_compute(self):
        operation_id = self.launch()
        self.http.complete(operation_id, job())
        self.controller().tick(operation_id)
        self.assertIsNotNone(self.store.load()["operations"][operation_id]["receipt"])
        self.assertEqual(self.count("delete"), 1)

    def test_running_work_with_unavailable_result_is_retained(self):
        operation_id = self.launch()
        self.controller().tick(operation_id)
        self.assertEqual(self.count("delete"), 0)

    def test_exited_worker_result_is_collected_before_cleanup(self):
        operation_id = self.launch()
        self.http.complete(operation_id, job())
        self.api.pods["pod_1"].update(status="EXITED", cost=0)
        self.controller().tick(operation_id)
        self.assertIsNotNone(self.store.load()["operations"][operation_id]["receipt"])
        self.assertEqual(self.count("delete"), 1)

    def test_stale_watchdog_and_high_quote_prevent_rental(self):
        self.now += 61
        with self.assertRaisesRegex(MODULE.Error, "watchdog_stale"):
            self.launch()
        self.store.heartbeat(self.now)
        self.api.quote = "100"
        with self.assertRaisesRegex(MODULE.Error, "catalog_price_above_limit"):
            self.launch()
        self.assertEqual(self.count("create"), 0)

    def test_missing_price_and_mismatched_hardware_trigger_failure_cleanup(self):
        operation_id = self.launch()
        self.api.pods["pod_1"]["cost"] = None
        self.controller().tick(operation_id)
        self.assertEqual(self.store.load()["operations"][operation_id]["cleanup_reason"], "unknown_provider_price")
        self.assertEqual(self.count("delete"), 1)

    def test_unqualified_or_malformed_hardware_is_cleaned_up(self):
        operation_id = self.launch()
        self.api.pods["pod_1"]["gpu"]["memory"] = "malformed"
        self.controller().tick(operation_id)
        self.assertEqual(self.store.load()["operations"][operation_id]["cleanup_reason"], "allocated_hardware_mismatch")
        self.assertEqual(self.count("delete"), 1)

    def test_higher_actual_price_triggers_cleanup(self):
        operation_id = self.launch()
        self.api.pods["pod_1"]["cost"] = 10
        self.controller().tick(operation_id)
        self.assertEqual(self.store.load()["operations"][operation_id]["cleanup_reason"], "provider_price_above_limit")
        self.assertEqual(self.count("delete"), 1)

    def test_missing_watchdog_lock_prevents_launch(self):
        self.watchdog_lock.__exit__(None, None, None)
        with self.assertRaisesRegex(MODULE.Error, "watchdog_not_running"):
            self.launch()
        self.assertEqual(self.count("create"), 0)

    def test_budget_reservations_are_not_reset_by_termination(self):
        first = self.launch()
        self.controller().terminate(first, failure=True)
        second = self.launch(job("test-2"))
        self.controller().terminate(second, failure=True)
        with self.assertRaisesRegex(MODULE.Error, "lifetime_budget_limit"):
            self.launch(job("test-3"))
        self.assertEqual(self.count("create"), 2)

    def test_state_permissions_and_symlinks_fail_closed(self):
        state = self.root / "state.json"
        state.chmod(0o644)
        with self.assertRaisesRegex(MODULE.Error, "0600"):
            self.store.load()
        state.chmod(0o600)
        link = self.root / "symlink.json"
        link.symlink_to(state)
        with self.assertRaises(OSError):
            MODULE.read_private_json(link)

    def test_interrupted_atomic_replace_preserves_prior_state(self):
        previous = (self.root / "state.json").read_bytes()
        changed = self.store.load()
        changed["operations"]["unfinished"] = {"status": "create_uncertain"}
        with patch.object(MODULE.os, "replace", side_effect=OSError("interrupted rename")):
            with self.assertRaises(OSError):
                self.store.save(changed)
        self.assertEqual((self.root / "state.json").read_bytes(), previous)
        self.assertEqual(list(self.root.glob(".state.json.*.tmp")), [])

    def test_public_status_never_contains_urls_or_capabilities(self):
        self.launch()
        encoded = json.dumps(MODULE.public_status(self.store.load()))
        self.assertNotIn("PRIVATE", encoded)
        self.assertNotIn("SECRET", encoded)
        self.assertNotIn("https://", encoded)


class ApiTests(unittest.TestCase):
    def test_v2_paths_and_pagination_preserve_opaque_cursor(self):
        class Http:
            def __init__(self):
                self.calls = []
            def json(self, url, **kwargs):
                self.calls.append((url, kwargs))
                if len(self.calls) == 1:
                    return {"pods": [{"id": "first"}], "pagination": {"hasNextPage": True, "nextCursor": "opaque&cursor"}}
                return {"pods": [{"id": "second"}], "pagination": {"hasNextPage": False, "nextCursor": None}}
        http = Http()
        pods = MODULE.Runpod("secret", http).all_pods()
        self.assertEqual(pods, [{"id": "first"}, {"id": "second"}])
        self.assertEqual(http.calls[1][0], "https://api.runpod.io/v2/pods?limit=1000&cursor=opaque%26cursor")
        self.assertEqual(http.calls[0][1]["key"], "secret")

    def test_invalid_hardware_digest_cuda_and_secret_fields_rejected(self):
        for change in (lambda p: p.update(image="image:latest"),
                       lambda p: p["gpu"].update(minCudaVersion="12"),
                       lambda p: p.update(env={"RUNPOD_API_KEY": "secret"}),
                       lambda p: p["limits"].update(max_hourly_usd="NaN")):
            config = policy()
            change(config)
            with self.assertRaises(MODULE.Error):
                MODULE.validate_policy(config)

    def test_redirect_never_forwards_key(self):
        with self.assertRaisesRegex(MODULE.Error, "redirect_refused"):
            MODULE.NoRedirect().redirect_request(None, None, None, None, None, None)


if __name__ == "__main__":
    unittest.main()
