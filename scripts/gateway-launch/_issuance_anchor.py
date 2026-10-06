#!/usr/bin/env python3
"""SYSCOIN: Bind issuance to a canonical token-proxy deployment receipt."""
from __future__ import annotations

import json
import os
import re
import stat
import sys
import urllib.request
from pathlib import Path

from _checkpoint_state_io import atomic_write_json

DELAY_SECONDS = 24 * 60 * 60


def specialize_token_runtime(artifact: dict, token: str) -> str:
    """SYSCOIN: Patch only the compiler-recorded token immutable, before deployment."""
    token = hex_value(token, 20, "token address")
    deployed = artifact["deployedBytecode"]
    raw = deployed["object"]
    if not isinstance(raw, str) or not re.fullmatch(r"(?:0x)?[0-9a-fA-F]+", raw):
        raise ValueError("invalid gas tank compiler runtime")
    code = bytearray.fromhex(raw.removeprefix("0x"))
    references = deployed.get("immutableReferences")
    if not isinstance(references, dict) or len(references) != 1:
        raise ValueError("gas tank must have exactly one token immutable")
    slots = next(iter(references.values()))
    if not isinstance(slots, list) or not slots:
        raise ValueError("missing gas tank token immutable references")
    seen = set()
    for slot in slots:
        start, length = uint(slot["start"], "immutable offset"), uint(slot["length"], "immutable length")
        if length != 32 or start + length > len(code) or start in seen:
            raise ValueError("invalid gas tank token immutable reference")
        seen.add(start)
        if code[start:start + length] != bytes(32):
            raise ValueError("gas tank immutable slot must be unbound")
        code[start:start + length] = bytes(12) + bytes.fromhex(token[2:])
    return "0x" + code.hex()


def uint(value: object, label: str) -> int:
    if isinstance(value, bool):
        raise ValueError(f"invalid {label}")
    try:
        result = value if isinstance(value, int) else int(
            value, 16 if isinstance(value, str) and value.startswith("0x") else 10)
    except (TypeError, ValueError):
        raise ValueError(f"invalid {label}") from None
    if not 0 <= result < 2**256:
        raise ValueError(f"invalid {label}")
    return result


def hex_value(value: object, size: int, label: str) -> str:
    if not isinstance(value, str) or not re.fullmatch(rf"0x[0-9a-fA-F]{{{size * 2}}}", value):
        raise ValueError(f"invalid {label}")
    return value.lower()


def read_private_json(path: Path) -> dict:
    info = os.lstat(path)
    if (not stat.S_ISREG(info.st_mode) or info.st_uid != os.geteuid()
            or stat.S_IMODE(info.st_mode) & 0o077 or info.st_nlink != 1):
        raise ValueError("unsafe issuance anchor file")
    # SYSCOIN: Reject symlink replacement between metadata validation and read.
    descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW)
    with os.fdopen(descriptor, "r", encoding="utf-8") as handle:
        opened = os.fstat(handle.fileno())
        if (opened.st_dev, opened.st_ino) != (info.st_dev, info.st_ino):
            raise ValueError("issuance anchor file changed during read")
        value = json.load(handle)
    if not isinstance(value, dict):
        raise ValueError("invalid issuance anchor object")
    return value


def bind_private_json(path: Path, value: dict) -> None:
    validate_parent(path)
    if path.exists() or path.is_symlink():
        if read_private_json(path) != value:
            raise ValueError("issuance prelude identity differs from its first run")
    else:
        atomic_write_json(path, value)


def validate_parent(path: Path) -> None:
    info = os.lstat(path.parent)
    if (not stat.S_ISDIR(info.st_mode) or info.st_uid != os.geteuid()
            or stat.S_IMODE(info.st_mode) & 0o022):
        raise ValueError("unsafe issuance anchor parent directory")


def make_anchor(expected: dict, transaction: dict, receipt: dict, block: dict) -> dict:
    """Validate a successful, exact CREATE2 call and its canonical receipt."""
    tx_hash = hex_value(receipt.get("transactionHash"), 32, "receipt transaction hash")
    block_hash = hex_value(receipt.get("blockHash"), 32, "receipt block hash")
    if uint(receipt.get("status"), "receipt status") != 1:
        raise ValueError("token deployment transaction failed")
    if hex_value(transaction.get("hash"), 32, "transaction hash") != tx_hash:
        raise ValueError("token receipt transaction mismatch")
    if (hex_value(transaction.get("from"), 20, "transaction sender") != expected["signer"]
            or hex_value(transaction.get("to"), 20, "transaction recipient") != expected["create2"]
            or hex_value(receipt.get("from"), 20, "receipt sender") != expected["signer"]
            or hex_value(receipt.get("to"), 20, "receipt recipient") != expected["create2"]):
        raise ValueError("token deployment signer or factory mismatch")
    if transaction.get("input", "").lower() != expected["token_calldata"]:
        raise ValueError("token deployment calldata mismatch")
    if uint(transaction.get("value"), "transaction value") != 0:
        raise ValueError("token deployment value must be zero")
    if uint(transaction.get("chainId"), "transaction chain ID") != int(expected["edge_chain_id"]):
        raise ValueError("token deployment transaction chain mismatch")
    number = uint(receipt.get("blockNumber"), "receipt block number")
    if (uint(block.get("number"), "canonical block number") != number
            or hex_value(block.get("hash"), 32, "canonical block hash") != block_hash
            or hex_value(transaction.get("blockHash"), 32, "transaction block hash") != block_hash
            or uint(transaction.get("blockNumber"), "transaction block number") != number):
        raise ValueError("token deployment receipt is not canonical")
    timestamp = uint(block.get("timestamp"), "token deployment timestamp")
    start = timestamp + DELAY_SECONDS
    if start >= 2**256:
        raise ValueError("issuance start timestamp overflows")
    return {"schema": "syscoin-token-receipt-issuance-v1", "identity": expected,
            "transaction_hash": tx_hash, "block_hash": block_hash, "block_number": number,
            "token_deployment_timestamp": timestamp, "delay_seconds": DELAY_SECONDS,
            "issuer_start_time": start}


def rpc(method: str, params: list) -> object:
    # SYSCOIN: RPC URLs may carry credentials. Neither errors nor argv expose them.
    request = urllib.request.Request(os.environ["ZKSYS_L2_RPC_URL"],
        data=json.dumps({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}).encode(),
        headers={"Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(request, timeout=120) as response:
            result = json.load(response)
    except Exception:
        raise ValueError(f"issuance anchor RPC {method} failed") from None
    if not isinstance(result, dict) or result.get("error") or result.get("result") is None:
        raise ValueError(f"issuance anchor RPC {method} failed")
    return result["result"]


def main() -> None:
    if len(sys.argv) not in (3, 4) or sys.argv[1] not in ("bind", "resolve"):
        raise ValueError("usage: _issuance_anchor.py bind|resolve PATH [DEPLOYMENT_TX_HASH]")
    mode, path = sys.argv[1], Path(sys.argv[2])
    validate_parent(path)
    expected = json.load(sys.stdin)
    if not isinstance(expected, dict):
        raise ValueError("invalid issuance prelude identity")
    if mode == "bind":
        bind_private_json(path, expected)
        return
    saved = read_private_json(path) if path.exists() or path.is_symlink() else None
    supplied = sys.argv[3] if len(sys.argv) == 4 else ""
    if saved and saved.get("identity") != expected:
        raise ValueError("token receipt identity differs from the current prelude")
    tx_hash = supplied or (saved or {}).get("transaction_hash")
    if not tx_hash:
        raise ValueError("token already deployed without an anchor; supply its exact deployment transaction hash")
    tx_hash = hex_value(tx_hash, 32, "token deployment transaction hash")
    if uint(rpc("eth_chainId", []), "RPC chain ID") != int(expected["edge_chain_id"]):
        raise ValueError("issuance anchor RPC chain mismatch")
    receipt = rpc("eth_getTransactionReceipt", [tx_hash])
    transaction = rpc("eth_getTransactionByHash", [tx_hash])
    block = rpc("eth_getBlockByNumber", [receipt["blockNumber"], False])
    anchor = make_anchor(expected, transaction, receipt, block)
    if saved and saved != anchor:
        raise ValueError("recorded token deployment anchor no longer matches canonical history")
    if rpc("eth_getCode", [expected["token"], "latest"]) == "0x":
        raise ValueError("anchored token proxy is missing")
    if not saved:
        atomic_write_json(path, anchor)
    print(anchor["issuer_start_time"])


if __name__ == "__main__":
    try:
        main()
    except (KeyError, OSError, TypeError, ValueError, json.JSONDecodeError) as error:
        # SYSCOIN: These errors contain public identity/path labels, never RPC credentials.
        raise SystemExit(f"issuance anchor: {error}") from None
