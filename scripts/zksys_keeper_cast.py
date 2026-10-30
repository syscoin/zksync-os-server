"""Foundry keystore adapter for signed keeper transactions."""
import json
import os
import subprocess

from zksys_keeper_transactions import TransactionError


class CastSigner:
    def __init__(self, rpc_url, keystore, password_file):
        if not all(isinstance(x, str) and x for x in (rpc_url, keystore, password_file)):
            raise TransactionError("missing signer configuration")
        self.rpc_url = rpc_url
        self.keystore = keystore
        self.password_file = password_file

    @staticmethod
    def _run(args, *, stdin=None, env=None):
        try:
            result = subprocess.run(["cast", *args], input=stdin, text=True, capture_output=True,
                                    env=env, check=True, timeout=60)
        except (OSError, subprocess.SubprocessError):
            # Do not expose RPC credentials or signer details from stderr.
            raise TransactionError("cast signing or decoding failed") from None
        return result.stdout.strip()

    def sign(self, tx):
        env = dict(os.environ)
        env["ETH_RPC_URL"] = self.rpc_url
        args = ["mktx", tx["to"], tx["data"], "--chain", str(tx["chainId"]),
                "--nonce", str(tx["nonce"]), "--gas-limit", str(tx["gas"]),
                "--gas-price", str(tx["gasPrice"]), "--value", str(tx["value"]),
                "--legacy", "--keystore", self.keystore, "--password-file", self.password_file]
        return self._run(args, env=env)

    def decode(self, raw):
        result = self._run(["decode-transaction", "--json"], stdin=raw + "\n")
        try:
            obj = json.loads(result)
            if isinstance(obj, str):
                obj = json.loads(obj)
            obj["from"] = obj["signer"]
            obj["data"] = obj["input"]
            return obj
        except (ValueError, TypeError, KeyError):
            raise TransactionError("invalid decoded signed transaction") from None

    def hash_raw(self, raw):
        return self._run(["keccak"], stdin=raw + "\n")


def hash_bytes(value):
    return CastSigner._run(["keccak"], stdin="0x" + value.hex() + "\n")
