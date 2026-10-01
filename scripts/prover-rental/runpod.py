#!/usr/bin/env python3
"""Bounded Runpod v2 rental lifecycle; no proof or settlement authority."""

import argparse
from contextlib import contextmanager
from decimal import Decimal, InvalidOperation
import fcntl
import hashlib
import http.client
import json
import os
from pathlib import Path
import re
import signal
import stat
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid


API = "https://api.runpod.io/v2"
JSON_LIMIT = 2 * 1024 * 1024
TERMINAL = {"terminated", "refused"}
PROVIDER_STATES = {"PROVISIONING", "STARTING", "RUNNING", "EXITED", "ERROR", "TERMINATED"}


class Error(Exception):
    """Only fixed, credential-free diagnostic codes cross the CLI boundary."""


class HttpError(Error):
    def __init__(self, status):
        self.status = status
        super().__init__(f"http_{status}")


def require(condition, message):
    if not condition:
        raise Error(message)


def exact_fields(value, fields):
    require(isinstance(value, dict) and set(value) == set(fields), "invalid_fields")


def positive_int(value):
    require(type(value) is int and value > 0, "invalid_positive_integer")
    return value


def money(value, allow_zero=False):
    require(isinstance(value, (str, int, float)) and not isinstance(value, bool), "invalid_money")
    try:
        result = Decimal(str(value))
    except InvalidOperation:
        raise Error("invalid_money") from None
    require(result.is_finite() and (result >= 0 if allow_zero else result > 0), "invalid_money")
    return result


def sha256(value):
    require(isinstance(value, str) and re.fullmatch(r"[0-9a-f]{64}", value), "invalid_sha256")
    return value


def https_url(value):
    require(isinstance(value, str) and len(value) <= 8192, "invalid_https_url")
    try:
        parsed = urllib.parse.urlsplit(value)
        valid = (parsed.scheme == "https" and parsed.hostname and not parsed.username
                 and not parsed.password and not parsed.fragment and parsed.port in (None, 443))
    except ValueError:
        valid = False
    require(valid and not any(ord(c) < 33 for c in value), "invalid_https_url")
    return value


def validate_policy(p):
    exact_fields(p, ("schema_version", "image", "gpu", "min_vram_gb", "disk_gb", "cloud",
                     "data_center_ids", "additional_hourly_usd", "limits"))
    require(p["schema_version"] == 1, "unsupported_schema")
    require(isinstance(p["image"], str) and re.fullmatch(
        r"[A-Za-z0-9][A-Za-z0-9./:_-]*@sha256:[0-9a-f]{64}", p["image"]), "image_requires_digest")
    exact_fields(p["gpu"], ("id", "count", "minRamPerGpu", "minVcpuCountPerGpu", "minCudaVersion"))
    require(isinstance(p["gpu"]["id"], str) and 0 < len(p["gpu"]["id"]) <= 128, "invalid_gpu_id")
    for field in ("count", "minRamPerGpu", "minVcpuCountPerGpu"):
        positive_int(p["gpu"][field])
    require(isinstance(p["gpu"]["minCudaVersion"], str) and re.fullmatch(
        r"\d+\.\d+", p["gpu"]["minCudaVersion"]), "invalid_cuda_version")
    positive_int(p["min_vram_gb"])
    positive_int(p["disk_gb"])
    require(p["cloud"] in ("SECURE", "COMMUNITY"), "invalid_cloud")
    require(isinstance(p["data_center_ids"], list) and 0 < len(p["data_center_ids"]) <= 32,
            "data_centers_required")
    require(all(isinstance(v, str) and re.fullmatch(r"[A-Z0-9-]{1,64}", v)
                for v in p["data_center_ids"]), "invalid_data_center")
    money(p["additional_hourly_usd"], allow_zero=True)
    limits = p["limits"]
    exact_fields(limits, ("max_concurrent_pods", "max_runtime_seconds", "max_hourly_usd",
                          "max_operation_usd", "lifetime_budget_usd", "max_artifact_bytes",
                          "watchdog_interval_seconds", "watchdog_stale_seconds"))
    for field in ("max_concurrent_pods", "max_runtime_seconds", "max_artifact_bytes",
                  "watchdog_interval_seconds", "watchdog_stale_seconds"):
        positive_int(limits[field])
    require(limits["max_concurrent_pods"] <= 32, "too_many_concurrent_pods")
    require(1 <= limits["watchdog_interval_seconds"] <= 60, "invalid_watchdog_interval")
    require(limits["watchdog_stale_seconds"] >= 2 * limits["watchdog_interval_seconds"],
            "watchdog_stale_window_too_short")
    require(money(limits["max_operation_usd"]) >= money(limits["max_hourly_usd"])
            * Decimal(limits["max_runtime_seconds"]) / 3600, "operation_budget_too_small")
    require(money(limits["lifetime_budget_usd"]) >= money(limits["max_operation_usd"]),
            "lifetime_budget_too_small")
    return p


def validate_job(job):
    exact_fields(job, ("schema_version", "job_id", "manifest_url", "manifest_sha256",
                       "result_manifest_url", "result_artifact_url"))
    require(job["schema_version"] == 1, "unsupported_schema")
    require(isinstance(job["job_id"], str) and re.fullmatch(r"[A-Za-z0-9_.:-]{1,128}", job["job_id"]),
            "invalid_job_id")
    sha256(job["manifest_sha256"])
    for field in ("manifest_url", "result_manifest_url", "result_artifact_url"):
        https_url(job[field])
    return job


def private_open(path, flags=os.O_RDONLY):
    fd = os.open(path, flags | os.O_NOFOLLOW, 0o600)
    info = os.fstat(fd)
    if not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid() or info.st_mode & 0o077:
        os.close(fd)
        raise Error("file_must_be_owned_regular_mode_0600")
    return fd


def read_private_json(path):
    with os.fdopen(private_open(path), "rb") as source:
        data = source.read(JSON_LIMIT + 1)
    require(len(data) <= JSON_LIMIT, "json_too_large")
    try:
        return json.loads(data)
    except (ValueError, UnicodeDecodeError):
        raise Error("invalid_json") from None


def sync_dir(path):
    fd = os.open(path, os.O_RDONLY | os.O_DIRECTORY)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def atomic_json(path, value):
    payload = json.dumps(value, sort_keys=True, separators=(",", ":"), allow_nan=False).encode()
    require(len(payload) <= JSON_LIMIT, "state_capacity_reached")
    temporary = path.with_name(f".{path.name}.{uuid.uuid4().hex}.tmp")
    try:
        with os.fdopen(private_open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL), "wb") as output:
            output.write(payload)
            output.flush()
            os.fsync(output.fileno())
        os.replace(temporary, path)
        sync_dir(path.parent)
    finally:
        temporary.unlink(missing_ok=True)


class Store:
    def __init__(self, root):
        self.root = Path(root)
        require(self.root.is_absolute(), "state_directory_must_be_absolute")
        info = self.root.lstat()
        require(stat.S_ISDIR(info.st_mode) and info.st_uid == os.getuid() and not info.st_mode & 0o077,
                "state_directory_must_be_owned_mode_0700")

    @classmethod
    def initialize(cls, root, policy):
        validate_policy(policy)
        root = Path(root)
        require(root.is_absolute(), "state_directory_must_be_absolute")
        root.mkdir(mode=0o700)
        sync_dir(root.parent)
        store = cls(root)
        atomic_json(root / "state.json", {"schema_version": 1, "controller_id": uuid.uuid4().hex,
                                          "policy": policy, "operations": {}})
        return store

    @contextmanager
    def lock(self, name="controller.lock", blocking=True):
        fd = private_open(self.root / name, os.O_RDWR | os.O_CREAT)
        try:
            fcntl.flock(fd, fcntl.LOCK_EX | (0 if blocking else fcntl.LOCK_NB))
            yield
        finally:
            os.close(fd)

    def load(self):
        data = read_private_json(self.root / "state.json")
        exact_fields(data, ("schema_version", "controller_id", "policy", "operations"))
        require(data["schema_version"] == 1 and re.fullmatch(r"[0-9a-f]{32}", data["controller_id"]),
                "invalid_state")
        validate_policy(data["policy"])
        require(isinstance(data["operations"], dict), "invalid_state")
        return data

    def save(self, data):
        atomic_json(self.root / "state.json", data)

    def heartbeat(self, now):
        atomic_json(self.root / "watchdog.json", {"at": now})

    def require_watchdog(self, now, max_age):
        try:
            with self.lock("watchdog.lock", blocking=False):
                raise Error("independent_watchdog_not_running")
        except BlockingIOError:
            pass
        at = read_private_json(self.root / "watchdog.json")["at"]
        require(0 <= now - at <= max_age, "independent_watchdog_stale")


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *args, **kwargs):
        raise Error("redirect_refused")


class Http:
    def __init__(self):
        self.opener = urllib.request.build_opener(NoRedirect())

    def transfer(self, url, output, limit, method="GET", payload=None, key=None):
        https_url(url)
        headers = {"Accept-Encoding": "identity"}
        if key:
            headers["Authorization"] = "Bearer " + key
        if payload is not None:
            headers["Content-Type"] = "application/json"
        request = urllib.request.Request(url, data=None if payload is None else json.dumps(payload).encode(),
                                         headers=headers, method=method)
        started = time.monotonic()
        try:
            with self.opener.open(request, timeout=15) as response:
                require(response.status in (200, 201, 204), "unexpected_http_success")
                total = 0
                while True:
                    require(time.monotonic() - started <= 30, "http_total_deadline")
                    part = response.read1(min(64 * 1024, limit - total + 1))
                    if not part:
                        return response.status
                    total += len(part)
                    require(total <= limit, "http_body_too_large")
                    output.write(part)
        except urllib.error.HTTPError as error:
            raise HttpError(error.code) from None
        except (urllib.error.URLError, TimeoutError, OSError, http.client.HTTPException):
            raise Error("http_transport_failure") from None

    def json(self, url, limit=JSON_LIMIT, **kwargs):
        import io
        output = io.BytesIO()
        status = self.transfer(url, output, limit, **kwargs)
        if status == 204:
            return None
        try:
            return json.loads(output.getvalue())
        except (ValueError, UnicodeDecodeError):
            raise Error("invalid_http_json") from None


class Runpod:
    def __init__(self, key, http=None):
        require(isinstance(key, str) and key and not any(c.isspace() for c in key), "invalid_api_key")
        self.key = key
        self.http = http or Http()

    def call(self, path, method="GET", payload=None):
        return self.http.json(API + path, method=method, payload=payload, key=self.key)

    def catalog(self, gpu_id):
        return self.call("/catalog/gpus/" + urllib.parse.quote(gpu_id, safe=""))

    def get(self, pod_id):
        require(isinstance(pod_id, str) and re.fullmatch(r"[A-Za-z0-9_-]{1,128}", pod_id), "invalid_pod_id")
        try:
            return self.call("/pods/" + pod_id)
        except HttpError as error:
            if error.status == 404:
                return None
            raise

    def all_pods(self):
        pods, seen, cursor = [], set(), None
        for _ in range(100):
            path = "/pods?limit=1000"
            if cursor:
                path += "&cursor=" + urllib.parse.quote(cursor, safe="")
            page = self.call(path)
            require(isinstance(page, dict) and isinstance(page.get("pods"), list), "invalid_pod_list")
            pods.extend(page["pods"])
            pagination = page.get("pagination", {})
            if pagination.get("hasNextPage") is False:
                return pods
            cursor = pagination.get("nextCursor")
            require(pagination.get("hasNextPage") is True and isinstance(cursor, str)
                    and cursor and cursor not in seen, "invalid_pagination")
            seen.add(cursor)
        raise Error("pod_list_page_limit")

    def create(self, request):
        return self.call("/pods", "POST", request)

    def delete(self, pod_id):
        require(re.fullmatch(r"[A-Za-z0-9_-]{1,128}", pod_id), "invalid_pod_id")
        self.call("/pods/" + pod_id, "DELETE")


def request_for(state, operation_id, job):
    p = state["policy"]
    return {"name": "zksys-" + operation_id, "image": p["image"], "gpu": p["gpu"],
            "disk": p["disk_gb"], "cloud": p["cloud"], "dataCenterIds": p["data_center_ids"],
            "ports": [], "startSsh": False, "startJupyter": False,
            "env": {"ZKSYS_RENTAL_CONTROLLER_ID": state["controller_id"],
                    "ZKSYS_RENTAL_OPERATION_ID": operation_id},
            "cmd": ["--operation-id", operation_id, "--job-id", job["job_id"],
                    "--manifest-url", job["manifest_url"], "--manifest-sha256", job["manifest_sha256"],
                    "--runtime-limit-seconds", str(p["limits"]["max_runtime_seconds"])]}


def owned(operation, pod):
    request = operation["request"]
    require(isinstance(pod, dict), "invalid_pod")
    require(pod.get("name") == request["name"] and pod.get("image") == request["image"]
            and isinstance(pod.get("env"), dict)
            and all(pod["env"].get(k) == v for k, v in request["env"].items()),
            "pod_ownership_mismatch")
    pod_id = pod.get("id")
    require(isinstance(pod_id, str) and re.fullmatch(r"[A-Za-z0-9_-]{1,128}", pod_id), "invalid_pod_id")
    require(not operation["pod_id"] or operation["pod_id"] == pod_id, "pod_id_changed")
    return pod_id


class Controller:
    # All methods run under Store.lock, shared with the independently supervised watchdog.
    def __init__(self, store, api, clock=time.time, http=None):
        self.store, self.api, self.clock = store, api, clock
        self.http = http or Http()
        self.state = store.load()
        self.policy = self.state["policy"]
        self.limits = self.policy["limits"]

    def save(self):
        self.store.save(self.state)

    def operation(self, operation_id):
        require(operation_id in self.state["operations"], "unknown_operation")
        return self.state["operations"][operation_id]

    def launch(self, job, latest_create_at=None):
        validate_job(job)
        for operation_id, op in self.state["operations"].items():
            if op["job"]["job_id"] == job["job_id"]:
                require(op["job"] == job, "job_id_reused_with_different_manifest")
                return operation_id
        self.store.require_watchdog(self.clock(), self.limits["watchdog_stale_seconds"])
        ops = list(self.state["operations"].values())
        require(sum(op["status"] not in TERMINAL for op in ops) < self.limits["max_concurrent_pods"],
                "concurrency_limit")
        reserved = sum((money(op["reserved_usd"]) for op in ops), Decimal(0))
        require(reserved + money(self.limits["max_operation_usd"]) <= money(self.limits["lifetime_budget_usd"]),
                "lifetime_budget_limit")
        require(len(ops) < 1000, "operation_count_limit")
        catalog = self.api.catalog(self.policy["gpu"]["id"])
        require(isinstance(catalog, dict) and isinstance(catalog.get("price"), dict), "invalid_catalog")
        require(catalog.get("id") == self.policy["gpu"]["id"] and catalog.get("manufacturer") == "NVIDIA",
                "catalog_identity_mismatch")
        require(positive_int(catalog.get("memory")) >= self.policy["min_vram_gb"], "insufficient_vram")
        quote = money(catalog.get("price", {}).get(self.policy["cloud"].lower())) * self.policy["gpu"]["count"]
        quote += money(self.policy["additional_hourly_usd"], allow_zero=True)
        require(quote <= money(self.limits["max_hourly_usd"]), "catalog_price_above_limit")
        require(latest_create_at is None or self.clock() < latest_create_at, "compute_window_elapsed_before_create")
        operation_id = uuid.uuid4().hex
        op = {"status": "create_uncertain", "pod_id": None, "job": job,
              "request": request_for(self.state, operation_id, job), "started_at": self.clock(),
              "reserved_usd": str(self.limits["max_operation_usd"]), "receipt": None,
              "cleanup_reason": None, "last_error": None, "provider_status": None}
        self.state["operations"][operation_id] = op
        # No documented provider idempotency key exists. Persist intent before the sole POST;
        # a crash anywhere after this boundary may only discover the allocation by reconciliation.
        self.save()
        if latest_create_at is not None and self.clock() >= latest_create_at:
            op["status"], op["last_error"] = "refused", "compute_window_elapsed_before_create"
            self.save()
            raise Error("compute_window_elapsed_before_create")
        try:
            pod = self.api.create(op["request"])
            op["pod_id"] = owned(op, pod)
            op["status"] = "active"
            self.observe(op, pod)
        except HttpError as error:
            if error.status in (400, 401, 402, 403, 404, 413, 422):
                op["status"] = "refused"
            op["last_error"] = str(error)
        except Error as error:
            op["last_error"] = str(error)
        self.save()
        return operation_id

    def observe(self, op, pod):
        owned(op, pod)
        status = pod.get("status")
        if not isinstance(status, str) or status not in PROVIDER_STATES:
            # Once ownership is established, unobservable state must stop billing rather
            # than make both the watchdog and explicit failure cleanup unable to delete.
            op["provider_status"] = "UNKNOWN"
            op["cleanup_reason"] = op["cleanup_reason"] or "unknown_provider_status"
            op["last_error"] = "unknown_provider_status"
            return
        op["provider_status"] = status
        if status == "TERMINATED":
            op["status"] = "terminated"
            return
        try:
            rate = money(pod.get("cost"), allow_zero=status in ("EXITED", "ERROR"))
            rate += money(self.policy["additional_hourly_usd"], allow_zero=True)
            if rate > money(self.limits["max_hourly_usd"]):
                op["cleanup_reason"] = "provider_price_above_limit"
        except Error:
            op["cleanup_reason"] = "unknown_provider_price"
        if status in ("EXITED", "ERROR"):
            op["cleanup_reason"] = op["cleanup_reason"] or "provider_job_stopped"
        if status == "RUNNING":
            gpu = pod.get("gpu", {})
            version = pod.get("cudaVersion", "")
            valid_version = isinstance(version, str) and re.fullmatch(r"\d+\.\d+", version)
            enough_cuda = valid_version and tuple(map(int, version.split("."))) >= tuple(
                map(int, self.policy["gpu"]["minCudaVersion"].split(".")))
            if not (isinstance(gpu, dict) and gpu.get("id") == self.policy["gpu"]["id"]
                    and gpu.get("count") == self.policy["gpu"]["count"] and enough_cuda
                    and type(gpu.get("memory")) is int and type(gpu.get("vcpuCount")) is int
                    and gpu.get("memory", 0) >= self.policy["gpu"]["minRamPerGpu"] * self.policy["gpu"]["count"]
                    and gpu.get("vcpuCount", 0) >= self.policy["gpu"]["minVcpuCountPerGpu"] * self.policy["gpu"]["count"]
                    and pod.get("dataCenterId") in self.policy["data_center_ids"]):
                op["cleanup_reason"] = "allocated_hardware_mismatch"

    def reconcile(self, operation_id):
        op = self.operation(operation_id)
        if op["status"] in TERMINAL:
            return
        if not op["pod_id"]:
            matches = [pod for pod in self.api.all_pods() if pod.get("name") == op["request"]["name"]]
            require(len(matches) <= 1, "multiple_matching_allocations_require_operator")
            if not matches:
                op["last_error"] = "allocation_still_unknown_do_not_recreate"
                self.save()
                return
            op["pod_id"] = owned(op, matches[0])
            op["status"] = "active"
            self.save()
        pod = self.api.get(op["pod_id"])
        if pod is None:
            # An allocation may not yet be visible through GET. Only our durable delete
            # intent makes absence sufficient to release the concurrency reservation.
            if op["status"] == "delete_uncertain":
                op["status"] = "terminated"
                op["provider_status"] = "ABSENT"
            else:
                op["last_error"] = "allocated_pod_temporarily_absent"
        else:
            self.observe(op, pod)
        self.save()

    def collect(self, operation_id):
        op = self.operation(operation_id)
        require(op["pod_id"] is not None, "allocation_not_identified")
        if op["receipt"]:
            self.verify_receipt(operation_id)
            return True
        try:
            manifest = self.http.json(op["job"]["result_manifest_url"], limit=64 * 1024)
        except HttpError as error:
            if error.status == 404:
                return False
            raise
        exact_fields(manifest, ("schema_version", "operation_id", "job_id", "manifest_sha256",
                                "artifact_sha256", "artifact_bytes"))
        require(manifest["schema_version"] == 1 and manifest["operation_id"] == operation_id
                and manifest["job_id"] == op["job"]["job_id"]
                and manifest["manifest_sha256"] == op["job"]["manifest_sha256"], "result_job_mismatch")
        expected = sha256(manifest["artifact_sha256"])
        size = positive_int(manifest["artifact_bytes"])
        require(size <= self.limits["max_artifact_bytes"], "artifact_too_large")
        destination = self.store.root / (operation_id + ".proof")
        temporary = self.store.root / ("." + operation_id + ".partial")
        if not destination.exists():
            # A partial transfer has no receipt authority and may be replaced on retry.
            temporary.unlink(missing_ok=True)
            try:
                with os.fdopen(private_open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL), "wb") as output:
                    self.http.transfer(op["job"]["result_artifact_url"], output, size)
                    output.flush()
                    os.fsync(output.fileno())
                check_artifact(temporary, expected, size)
                os.replace(temporary, destination)
                sync_dir(destination.parent)
            finally:
                temporary.unlink(missing_ok=True)
        check_artifact(destination, expected, size)
        # This receipt confirms transport integrity/durability, never proof validity or credit.
        op["receipt"] = {"sha256": expected, "bytes": size, "proof_verified": False}
        self.save()
        return True

    def verify_receipt(self, operation_id):
        receipt = self.operation(operation_id)["receipt"]
        require(receipt is not None, "durable_artifact_required")
        check_artifact(self.store.root / (operation_id + ".proof"), receipt["sha256"], receipt["bytes"])

    def terminate(self, operation_id, failure=False):
        op = self.operation(operation_id)
        if op["status"] in TERMINAL:
            return
        if failure:
            op["cleanup_reason"] = op["cleanup_reason"] or "explicit_failure_cleanup"
            self.save()
        require(op["receipt"] is not None or op["cleanup_reason"], "cleanup_requires_receipt_or_failure")
        if not op["cleanup_reason"]:
            self.verify_receipt(operation_id)
        self.reconcile(operation_id)
        if op["status"] in TERMINAL:
            return
        require(op["pod_id"] is not None, "allocation_unknown_cleanup_pending")
        # Re-read exact ownership before each DELETE, including after an ambiguous prior DELETE.
        pod = self.api.get(op["pod_id"])
        if pod is None:
            op["last_error"] = "allocated_pod_temporarily_absent"
            self.save()
            return
        owned(op, pod)
        op["status"] = "delete_uncertain"
        self.save()
        self.api.delete(op["pod_id"])
        # A success response is not used as a substitute for observing resource disappearance.
        self.reconcile(operation_id)

    def tick(self, operation_id):
        op = self.operation(operation_id)
        if op["status"] in TERMINAL:
            return
        elapsed = self.clock() - op["started_at"]
        if elapsed < 0 or elapsed >= self.limits["max_runtime_seconds"]:
            op["cleanup_reason"] = "runtime_deadline_or_clock_rollback"
            self.save()
        self.reconcile(operation_id)
        if op["status"] in TERMINAL or not op["pod_id"]:
            return
        if op["cleanup_reason"]:
            if op["cleanup_reason"] == "provider_job_stopped" and not op["receipt"]:
                # A normal one-shot worker may exit immediately after publishing its result.
                try:
                    self.collect(operation_id)
                except Error as error:
                    op["last_error"] = str(error)
                    self.save()
            self.terminate(operation_id)
        elif self.collect(operation_id):
            self.terminate(operation_id)


def check_artifact(path, expected, size):
    digest, actual = hashlib.sha256(), 0
    with os.fdopen(private_open(path), "rb") as source:
        while part := source.read(min(64 * 1024, size - actual + 1)):
            actual += len(part)
            require(actual <= size, "artifact_size_mismatch")
            digest.update(part)
    require(actual == size and digest.hexdigest() == expected, "artifact_hash_or_size_mismatch")


def public_status(state):
    return {operation_id: {key: op[key] for key in (
        "status", "pod_id", "provider_status", "receipt", "cleanup_reason", "last_error")}
        for operation_id, op in state["operations"].items()}


def watchdog(store, api):
    stopped = False

    def stop(*_):
        nonlocal stopped
        stopped = True

    signal.signal(signal.SIGINT, stop)
    signal.signal(signal.SIGTERM, stop)
    with store.lock("watchdog.lock", blocking=False):
        while not stopped:
            store.heartbeat(time.time())
            with store.lock():
                snapshot = store.load()
            for operation_id in snapshot["operations"]:
                if stopped:
                    break
                try:
                    with store.lock():
                        controller = Controller(store, api)
                        controller.tick(operation_id)
                except Error as error:
                    print(json.dumps({"operation_id": operation_id, "error": str(error)}), flush=True)
                except (OSError, ValueError, KeyError, TypeError):
                    print(json.dumps({"operation_id": operation_id,
                                      "error": "local_or_response_validation_failure"}), flush=True)
            interval = snapshot["policy"]["limits"]["watchdog_interval_seconds"]
            for _ in range(interval):
                if stopped:
                    break
                time.sleep(1)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--state-dir", required=True)
    parser.add_argument("--execute", action="store_true", help="permit local writes and provider actions")
    parser.add_argument("--api-key-file", help="owner-only file; otherwise RUNPOD_API_KEY")
    commands = parser.add_subparsers(dest="command", required=True)
    init = commands.add_parser("init")
    init.add_argument("--config", required=True)
    launch = commands.add_parser("launch")
    launch.add_argument("--job", required=True)
    for command in ("reconcile", "collect", "terminate"):
        sub = commands.add_parser(command)
        sub.add_argument("--operation-id", required=True)
        if command == "terminate":
            sub.add_argument("--failure-cleanup", action="store_true")
    commands.add_parser("watchdog")
    commands.add_parser("status")
    args = parser.parse_args(argv)
    if args.command == "init":
        policy = validate_policy(read_private_json(args.config))
        if args.execute:
            Store.initialize(args.state_dir, policy)
        print(json.dumps({"action": "init", "execute": args.execute}))
        return
    store = Store(args.state_dir)
    if args.command == "status":
        with store.lock():
            print(json.dumps(public_status(store.load()), indent=2))
        return
    job = validate_job(read_private_json(args.job)) if args.command == "launch" else None
    if not args.execute:
        print(json.dumps({"action": args.command, "execute": False, "network_requests": 0}))
        return
    if args.command == "collect":
        api = None
    else:
        if args.api_key_file:
            with os.fdopen(private_open(args.api_key_file), "r") as key_file:
                key = key_file.read(4097).strip()
            require(len(key) <= 4096, "invalid_api_key")
        else:
            key = os.environ.get("RUNPOD_API_KEY", "")
        api = Runpod(key)
    if args.command == "watchdog":
        watchdog(store, api)
        return
    with store.lock():
        controller = Controller(store, api)
        if args.command == "launch":
            operation_id = controller.launch(job)
        else:
            operation_id = args.operation_id
            if args.command == "terminate":
                controller.terminate(operation_id, failure=args.failure_cleanup)
            else:
                getattr(controller, args.command)(operation_id)
        print(json.dumps({operation_id: public_status(controller.state)[operation_id]}, indent=2))


if __name__ == "__main__":
    try:
        main()
    except (Error, OSError, ValueError, KeyError, TypeError) as error:
        # Provider responses, presigned URLs, filesystem exceptions and JSON data may hold secrets.
        print(str(error) if isinstance(error, Error) else "local_or_response_validation_failure", file=sys.stderr)
        sys.exit(1)
