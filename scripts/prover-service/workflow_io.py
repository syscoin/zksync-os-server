"""Private, immutable handoff files and signing-only wallet requests."""

import hashlib
import fcntl
import os
from pathlib import Path
import stat
import tempfile

import relay as r
import service as s


MAX_PAYLOAD = 256 * 1024 * 1024
WORK_FIELDS = [("workHash", "bytes32")]
RESULT_FIELDS = [("workHash", "bytes32"), ("resultHash", "bytes32")]


def digest(value):
    return hashlib.sha256(s.canonical(value)).hexdigest()


def directory(value, create=False):
    path = Path(value)
    s.require(path.is_absolute(), "absolute_private_directory_required")
    for component in (path, *path.parents):
        s.require(not component.is_symlink(), "directory_symlink_forbidden")
    if create and not path.exists():
        try:
            path.mkdir(mode=0o700)
        except FileExistsError:
            pass
        r.sync_dir(path.parent)
    info = path.lstat()
    s.require(stat.S_ISDIR(info.st_mode) and info.st_uid == os.getuid() and not info.st_mode & 0o077,
              "private_directory_required")
    return path


def immutable_bytes(path, data, maximum=s.MAX_FILE):
    path = Path(path)
    s.require(len(data) <= maximum, "handoff_file_too_large")
    directory(path.parent)
    lock = os.open(path.parent / ".handoff.lock", os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW, 0o600)
    try:
        info = os.fstat(lock)
        s.require(stat.S_ISREG(info.st_mode) and info.st_uid == os.getuid() and not info.st_mode & 0o077
                  and info.st_nlink == 1, "unsafe_handoff_lock")
        fcntl.flock(lock, fcntl.LOCK_EX)
        if path.exists() or path.is_symlink():
            s.require(path.lstat().st_nlink == 1 and s.read(path, private=True, maximum=maximum) == data,
                      "immutable_handoff_changed")
            return
        descriptor, name = tempfile.mkstemp(prefix=".handoff-", dir=path.parent)
        temporary = Path(name)
        try:
            with os.fdopen(descriptor, "wb") as output:
                output.write(data)
                output.flush()
                os.fsync(output.fileno())
            # Cooperative writers hold this directory lock through existence checking and
            # rename; a crash never leaves a published file linked to a temporary inode.
            os.replace(temporary, path)
            r.sync_dir(path.parent)
        finally:
            temporary.unlink(missing_ok=True)
    finally:
        os.close(lock)


def immutable_json(path, value, maximum=s.MAX_FILE):
    immutable_bytes(path, s.canonical(value), maximum)


def private_json(path, maximum=s.MAX_FILE):
    path = Path(path)
    s.require(path.lstat().st_nlink == 1, "linked_handoff_file_forbidden")
    return s.read_json(path, private=True, maximum=maximum)


def wallet_connection(path, timeout):
    value = private_json(path)
    s.exact(value, ("url", "authorization"))
    return r.Rpc(value["url"], value["authorization"], timeout)


def typed_signature(root, request, wallet, execute):
    """A lost signing response can only cause the identical digest to be requested again."""
    root = directory(root)
    s.nonzero(request["digest"])
    s.nonzero(request["signer"], 20)
    key = digest(request)
    request_path, signature_path = root / (key + ".request.json"), root / (key + ".signature.json")
    if signature_path.exists():
        s.require(private_json(request_path) == request, "signing_request_changed")
        value = private_json(signature_path)
        s.exact(value, ("request_sha256", "signature"))
        s.require(value["request_sha256"] == key, "signature_request_changed")
        s.verify_eoa(request, value["signature"])
        return value["signature"]
    if not execute:
        return None
    immutable_json(request_path, request)
    s.require(wallet is not None, "signing_wallet_required")
    signature = wallet.call("eth_signTypedData_v4", [request["signer"], s.canonical(request["typed_data"]).decode()])
    s.verify_eoa(request, signature)
    immutable_json(signature_path, {"request_sha256": key, "signature": signature})
    return signature


def work_request(settings, body):
    return s.typed_request("WrapperWorkV1", WORK_FIELDS, {"workHash": s.keccak(s.canonical(body))},
                           "ZkSysWrapperWork", settings["settlement_chain_id"], settings["coordinator"],
                           settings["sequencer"])


def result_request(settings, work_hash, result, operator):
    return s.typed_request("WrapperResultV1", RESULT_FIELDS,
                           {"workHash": work_hash, "resultHash": s.keccak(s.canonical(result))},
                           "ZkSysWrapperWork", settings["settlement_chain_id"], settings["coordinator"], operator)


def validate_work(settings, envelope, payload):
    s.exact(envelope, ("body", "sequencer_signature"))
    body = envelope["body"]
    s.exact(body, ("schema_version", "lane", "release_sha256", "payload_sha256", "request", "evidence",
                   "audit", "native_lease_deadline", "proof"))
    s.require(body["schema_version"] == 1 and body["lane"] in ("child", "gateway"), "invalid_wrapper_work")
    for key in ("release_sha256", "payload_sha256"):
        s.require(isinstance(body[key], str) and len(body[key]) == 64
                  and all(c in "0123456789abcdef" for c in body[key]), "invalid_work_hash")
    s.require(body["payload_sha256"] == digest(payload), "work_payload_changed")
    s.uint(body["native_lease_deadline"], 64)
    request = work_request(settings, body)
    s.verify_eoa(request, envelope["sequencer_signature"])
    return request["typed_data"]["message"]["workHash"]


def publish_work(inbox, envelope, payload):
    inbox = directory(inbox)
    work_hash = s.keccak(s.canonical(envelope["body"]))
    root = directory(inbox / work_hash[2:], create=True)
    immutable_json(root / "payload.json", payload, MAX_PAYLOAD)
    # Readers discover only the final envelope, after the bounded payload is durable.
    immutable_json(root / "work.json", envelope)
    return root


def validate_result(settings, work_hash, envelope, operator):
    s.exact(envelope, ("result", "operator_signature"))
    result = envelope["result"]
    s.exact(result, ("schema_version", "work_hash", "proof", "prepared", "wrapper_signature"))
    s.require(result["schema_version"] == 1 and result["work_hash"] == work_hash, "wrong_wrapper_result")
    s.verify_eoa(result_request(settings, work_hash, result, operator), envelope["operator_signature"])
    return result
