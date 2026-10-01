"""Fixed V32 proving payloads and bounded transport shared by the two trust domains."""

import base64
import hashlib
import http.client
import io
import json
import os
from pathlib import Path
import re
import time
import urllib.error
import urllib.parse
import urllib.request

from runpod import Error, NoRedirect, exact_fields, https_url, positive_int, require, sha256, sync_dir


MAX_PICK = {"FRI": 384 * 1024 * 1024, "SNARK": 256 * 1024 * 1024}
MAX_SUBMIT = 10 * 1024 * 1024
MAX_MANIFEST = 64 * 1024
BINARIES = {"FRI": "/usr/bin/zksync_os_fri_prover", "SNARK": "/usr/bin/zksync_os_snark_prover"}
GUEST_BIN = "/multiblock_batch.bin"
GUEST_TEXT = "/multiblock_batch.text"
CRS = "/setup_compact.key"
RELEASE_PATH = "/opt/zksys-rental/release.json"


def encode(value):
    return json.dumps(value, sort_keys=True, separators=(",", ":"), allow_nan=False).encode()


def decode(data):
    def unique(pairs):
        output = {}
        for key, value in pairs:
            require(key not in output, "duplicate_json_field")
            output[key] = value
        return output
    try:
        return json.loads(data, object_pairs_hook=unique)
    except (ValueError, UnicodeDecodeError):
        raise Error("invalid_json") from None


def hash_bytes(data):
    return hashlib.sha256(data).hexdigest()


def read_file(path, maximum, private=False):
    import stat
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW)
    with os.fdopen(fd, "rb") as source:
        info = os.fstat(source.fileno())
        require(stat.S_ISREG(info.st_mode) and not info.st_mode & 0o022, "unsafe_input_file")
        if private:
            require(info.st_uid == os.getuid() and not info.st_mode & 0o077, "private_file_required")
        require(info.st_size <= maximum, "file_too_large")
        data = source.read(maximum + 1)
    require(len(data) <= maximum, "file_too_large")
    return data


def write_new(path, data):
    path = Path(path)
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    with os.fdopen(fd, "wb") as output:
        output.write(data)
        output.flush()
        os.fsync(output.fileno())
    sync_dir(path.parent)


def b256(value):
    require(isinstance(value, str) and re.fullmatch(r"0x[0-9a-f]{64}", value), "invalid_b256")
    require(int(value, 16) != 0, "zero_release_identity_or_token")
    return value


def native_url(value):
    """Native API traffic may use a trusted local tunnel; scoped storage remains HTTPS."""
    parsed = urllib.parse.urlsplit(value)
    if parsed.scheme == "http":
        require(parsed.hostname in ("127.0.0.1", "localhost", "::1") and not parsed.username
                and not parsed.password and not parsed.fragment and parsed.port is not None
                and not any(ord(c) < 33 for c in value), "native_api_requires_https_or_loopback")
    else:
        https_url(value)
    return value


def validate_chain_binding(value):
    exact_fields(value, ("lane", "chain_id", "chain_address", "settlement_chain_id", "protocol_version",
                         "vk_hash", "evidence_sha256", "payload_sha256", "origin_endpoint_sha256"))
    require(value["lane"] in ("child", "gateway"), "invalid_settlement_lane")
    for field in ("chain_id", "settlement_chain_id"):
        require(isinstance(value[field], str) and re.fullmatch(r"0x[1-9a-f][0-9a-f]*", value[field])
                and len(value[field]) <= 66, "invalid_chain_identity")
    require(isinstance(value["chain_address"], str)
            and re.fullmatch(r"0x[0-9a-f]{40}", value["chain_address"])
            and int(value["chain_address"], 16) != 0, "invalid_chain_address")
    require(value["protocol_version"] == 32, "unsupported_proving_lane")
    b256(value["vk_hash"])
    for field in ("evidence_sha256", "payload_sha256", "origin_endpoint_sha256"):
        sha256(value[field])
    return value


def release_identity(data):
    release = decode(data)
    exact_fields(release, ("schema_version", "stage", "protocol_version", "execution_version",
                           "proving_version", "security_level", "vk_hash", "program_commitment",
                           "app_bin_sha256", "app_text_sha256", "worker_sha256", "crs_sha256"))
    require(release["schema_version"] == 1 and release["stage"] in BINARIES, "invalid_release_stage")
    require((release["protocol_version"], release["execution_version"], release["proving_version"],
             release["security_level"]) == (32, 7, 8, 100), "unsupported_proving_lane")
    b256(release["vk_hash"])
    b256(release["program_commitment"])
    for field in ("app_bin_sha256", "app_text_sha256", "worker_sha256"):
        sha256(release[field])
    if release["stage"] == "SNARK":
        sha256(release["crs_sha256"])
    else:
        require(release["crs_sha256"] is None, "fri_must_not_use_crs")
    return release


def verify_image(release):
    files = [(GUEST_BIN, "app_bin_sha256"), (GUEST_TEXT, "app_text_sha256"),
             (BINARIES[release["stage"]], "worker_sha256")]
    if release["stage"] == "SNARK":
        files.append((CRS, "crs_sha256"))
    for path, field in files:
        digest = hashlib.sha256()
        fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW)
        with os.fdopen(fd, "rb") as source:
            while part := source.read(1024 * 1024):
                digest.update(part)
        require(digest.hexdigest() == release[field], "image_file_hash_mismatch")
    require(os.access(BINARIES[release["stage"]], os.X_OK), "worker_not_executable")


def decode_base64(value, maximum):
    require(isinstance(value, str) and len(value) <= maximum, "invalid_base64_size")
    try:
        result = base64.b64decode(value, validate=True)
    except (ValueError, base64.binascii.Error):
        raise Error("invalid_base64") from None
    require(result and base64.b64encode(result).decode() == value, "noncanonical_or_empty_base64")
    return result


def validate_payload(payload, stage, vk, proof=False):
    require(stage in BINARIES, "invalid_stage")
    bounds = ["batch_number"] if stage == "FRI" else ["from_batch_number", "to_batch_number"]
    content = "proof" if proof else ("prover_input" if stage == "FRI" else "fri_proofs")
    exact_fields(payload, (*bounds, "vk_hash", content))
    require(payload["vk_hash"] == vk, "payload_vk_mismatch")
    for field in bounds:
        require(type(payload[field]) is int and 0 <= payload[field] < 2**32, "invalid_batch_number")
    if stage == "SNARK":
        count = payload["to_batch_number"] - payload["from_batch_number"] + 1
        require(2 <= count <= 100, "invalid_snark_range")
    if proof:
        data = decode_base64(payload[content], MAX_SUBMIT)
        if stage == "SNARK":
            require(len(data) % 32 == 0, "invalid_snark_word_encoding")
    elif stage == "FRI":
        require(len(decode_base64(payload[content], MAX_PICK[stage])) % 4 == 0, "invalid_fri_words")
    else:
        require(isinstance(payload[content], list) and len(payload[content]) == count, "fri_count_mismatch")
        for value in payload[content]:
            decode_base64(value, MAX_PICK[stage])
    require(len(encode(payload)) <= (MAX_SUBMIT if proof else MAX_PICK[stage]), "payload_too_large")
    return payload


class Network:
    validate_url = staticmethod(https_url)

    def request(self, url, method="GET", data=None, authorization=None, maximum=MAX_MANIFEST, deadline=None):
        self.validate_url(url)
        headers = {"Accept-Encoding": "identity"}
        if data is not None:
            headers["Content-Type"] = "application/json"
        if authorization:
            headers["Authorization"] = authorization
        request = urllib.request.Request(url, data=data, headers=headers, method=method)
        deadline = deadline or time.monotonic() + 600
        require(deadline > time.monotonic(), "transport_deadline")
        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), NoRedirect())
        try:
            try:
                response = opener.open(request, timeout=min(600, max(1, deadline - time.monotonic())))
            except urllib.error.HTTPError as error:
                response = error
            with response:
                output = io.BytesIO()
                while True:
                    require(time.monotonic() < deadline, "transport_deadline")
                    part = response.read1(min(64 * 1024, maximum - output.tell() + 1))
                    if not part:
                        return response.code, dict(response.headers.items()), output.getvalue()
                    require(output.tell() + len(part) <= maximum, "transport_body_too_large")
                    output.write(part)
        except (urllib.error.URLError, OSError, http.client.HTTPException):
            raise Error("transport_failure") from None

    def get(self, url, maximum, deadline=None):
        status, _, body = self.request(url, maximum=maximum, deadline=deadline)
        require(status == 200, "download_failed")
        return body

    def put(self, url, data, deadline=None):
        status, _, _ = self.request(url, "PUT", data, maximum=MAX_MANIFEST, deadline=deadline)
        require(status in (200, 201, 204), "upload_failed")


class NativeNetwork(Network):
    validate_url = staticmethod(native_url)
