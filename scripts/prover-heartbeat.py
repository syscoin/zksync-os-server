#!/usr/bin/env python3
"""SYSCOIN: Supply ordinary paid traffic for an idle real-SNARK singleton.

Dry-run by default. Use a dedicated, funded EOA and --send --watch to operate.
This does not produce empty protocol blocks, change priority mode, or copy proofs.
"""

import argparse
import base64
import json
import os
import re
import subprocess
import time
import urllib.request
from urllib.parse import urlsplit


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *args, **kwargs):
        raise RuntimeError("endpoint redirected; refusing to forward credentials")


def read_json(url, payload=None, auth=None):
    parsed = urlsplit(url)
    if parsed.scheme not in ("http", "https") or parsed.username or parsed.fragment:
        raise RuntimeError("use an HTTP(S) endpoint without embedded credentials")
    if parsed.scheme == "http" and parsed.hostname not in ("localhost", "127.0.0.1", "::1"):
        raise RuntimeError("remote endpoints require HTTPS; tunnel local HTTP endpoints")
    headers = {"Content-Type": "application/json"}
    if auth:
        headers["Authorization"] = "Basic " + base64.b64encode(auth.encode()).decode()
    request = urllib.request.Request(
        url, data=None if payload is None else json.dumps(payload).encode(), headers=headers
    )
    with urllib.request.build_opener(NoRedirect()).open(request, timeout=15) as response:
        body = response.read(2 * 1024 * 1024 + 1)
    if len(body) > 2 * 1024 * 1024:
        raise RuntimeError("endpoint response exceeds heartbeat status bound")
    return json.loads(body)


def singleton_batch(snark_jobs, fri_jobs, min_age):
    # SYSCOIN: Never add traffic for a busy fleet, leased aggregate, or fake/unfinished VK lane.
    if fri_jobs or len(snark_jobs) != 1:
        return None
    job = snark_jobs[0]
    if job["assigned_to_prover_id"] is not None or job["added_seconds_ago"] < min_age:
        return None
    vk = job["fri_job"]["vk_hash"]
    if not re.fullmatch(r"0x[0-9a-fA-F]{64}", vk) or int(vk, 16) == 0:
        return None
    batch = job["fri_job"]["batch_number"]
    if type(batch) is not int or batch < 1:
        raise RuntimeError("invalid singleton batch number")
    return batch


class Heartbeat:
    def __init__(self, args):
        self.args = args
        self.last_attempted_batch = None

    def rpc(self, method, params):
        response = read_json(
            self.args.rpc_url,
            {"jsonrpc": "2.0", "id": 1, "method": method, "params": params},
        )
        if response.get("error") is not None or "result" not in response:
            raise RuntimeError(f"RPC {method} failed")
        return response["result"]

    def status(self, stage):
        result = read_json(
            self.args.prover_url.rstrip("/") + "/status/" + stage.lower(),
            auth=os.environ.get("PROVER_HEARTBEAT_BASIC_AUTH"),
        )
        if not isinstance(result, list):
            raise RuntimeError("invalid prover queue status")
        return result

    def tick(self):
        args = self.args
        if int(self.rpc("eth_chainId", []), 16) != args.chain_id:
            raise RuntimeError("RPC chain identity mismatch")
        batch = singleton_batch(self.status("SNARK"), self.status("FRI"), args.min_age)
        if batch is None or batch == self.last_attempted_batch:
            return "no unattended singleton"
        head = self.rpc("eth_getBlockByNumber", ["latest", False])
        if time.time() - int(head["timestamp"], 16) < args.min_age:
            return "recent block traffic; waiting for the pipeline"
        if not args.send:
            return f"batch {batch} needs a companion; dry-run, no transaction sent"
        address = args.address.lower()
        wallet = ["--keystore", args.keystore, "--password-file", args.password_file]
        signer = subprocess.run(
            [args.cast, "wallet", "address", *wallet],
            capture_output=True, text=True, timeout=30, check=True,
        ).stdout.strip().lower()
        if signer != address or self.rpc("eth_getCode", [address, "latest"]) != "0x":
            raise RuntimeError("heartbeat requires the configured dedicated EOA")
        nonce = self.rpc("eth_getTransactionCount", [address, "latest"])
        if nonce != self.rpc("eth_getTransactionCount", [address, "pending"]):
            return "heartbeat account has pending work; no new transaction"
        if int(head["baseFeePerGas"], 16) > args.max_fee_per_gas:
            raise RuntimeError("base fee exceeds the heartbeat fee cap")
        if int(self.rpc("eth_getBalance", [address, "latest"]), 16) < args.gas_limit * args.max_fee_per_gas:
            raise RuntimeError("heartbeat account lacks native fee collateral")
        # Recheck queues after wallet/RPC work; ordinary traffic may have supplied the companion.
        if singleton_batch(self.status("SNARK"), self.status("FRI"), args.min_age) != batch:
            return "queue progressed; no transaction needed"
        # SYSCOIN: A new block can precede its FRI job; queues alone cannot establish idleness.
        latest = self.rpc("eth_getBlockByNumber", ["latest", False])
        if latest["hash"] != head["hash"] or time.time() - int(latest["timestamp"], 16) < args.min_age:
            return "block traffic progressed; waiting for the pipeline"
        # SYSCOIN: One explicit-nonce attempt per tail, including ambiguous send failures. Do not
        # keep spending after a stuck pipeline; inspect the nonce/transaction before restarting.
        self.last_attempted_batch = batch
        result = subprocess.run(
            [args.cast, "send", address, "--from", address, "--value", "0", "--data", "0x",
             "--rpc-url", args.rpc_url, "--chain", str(args.chain_id), "--nonce", str(int(nonce, 16)),
             "--gas-limit", str(args.gas_limit), "--gas-price", str(args.max_fee_per_gas),
             "--priority-gas-price", "0", "--async", "--rpc-timeout", "15", *wallet],
            capture_output=True, text=True, timeout=45, check=True,
        )
        tx_hash = result.stdout.strip()
        if not re.fullmatch(r"0x[0-9a-fA-F]{64}", tx_hash):
            raise RuntimeError("ambiguous broadcast result; inspect the heartbeat nonce before retrying")
        return f"batch {batch}: companion transaction {tx_hash}; awaiting sequencing/proving"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--rpc-url", required=True)
    parser.add_argument("--chain-id", type=int, required=True)
    parser.add_argument("--prover-url", required=True, help="URL ending in /prover-jobs/v1")
    parser.add_argument("--min-age", type=int, default=300, help="idle seconds before supplying traffic")
    parser.add_argument("--interval", type=int, default=60)
    parser.add_argument("--watch", action="store_true")
    parser.add_argument("--send", action="store_true", help="authorize paid zero-value self-transactions")
    parser.add_argument("--address")
    parser.add_argument("--keystore")
    parser.add_argument("--password-file")
    parser.add_argument("--max-fee-per-gas", type=int, help="hard EIP-1559 fee cap in wei")
    parser.add_argument("--gas-limit", type=int, default=100_000)
    parser.add_argument("--cast", default="cast")
    args = parser.parse_args()
    if min(args.chain_id, args.min_age, args.interval, args.gas_limit) <= 0:
        parser.error("chain ID, ages, interval and gas limit must be positive")
    if args.send and (not args.keystore or not args.password_file
                      or not re.fullmatch(r"0x[0-9a-fA-F]{40}", args.address or "")
                      or not args.max_fee_per_gas or args.max_fee_per_gas < 0):
        parser.error("--send requires address, keystore, password-file and a positive fee cap")
    heartbeat = Heartbeat(args)
    while True:
        # Do not print subprocess stderr/arguments or endpoint exceptions: they can contain secrets.
        try:
            print(heartbeat.tick(), flush=True)
        except Exception as error:
            print(f"heartbeat stopped ({type(error).__name__}); inspect endpoints, wallet and pending nonce", flush=True)
            return 1
        if not args.watch:
            return 0
        time.sleep(args.interval)


if __name__ == "__main__":
    raise SystemExit(main())
