#!/usr/bin/env python3
"""Bounded Blockscout v9.0.2 token-icon import; read-only unless --apply is set.

The supported upstream admin API takes api_key in the JSON body (not a header).
Do not log request bodies. This helper never connects to a database or sends a
chain transaction. Read metadata publication semantics in TOKEN-METADATA.md.
"""

import argparse
import json
from pathlib import Path
import re
import sys
import time
import urllib.error
import urllib.request


BASE_URLS = ("https://explorer-zk.tanenbaum.io", "http://127.0.0.1:5200")
REVISION = "ed93c12dc1835ec0e7fb3e4be3504ceb7061610e"
EXPECTED = {
    "0xd501e86b1fc5823403b2ba995e2b8b78179236a6": (
        "USDC", 6, "https://bridge-zk.tanenbaum.io/usdc-logo.png"
    ),
    "0x6ebb170f69d886916d9ee9e585ce39e626cbc35d": (
        "ZKSYS", 18, "https://explorer-zk.tanenbaum.io/branding-assets/zksys-token-icon.svg"
    ),
}


class MetadataError(Exception):
    """Safe diagnostic text only: never include remote bodies or credentials."""


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        raise MetadataError("HTTP redirect refused")


class Transport:
    MAX_REQUESTS = 8
    MAX_RESPONSE = 256 * 1024
    MAX_TOTAL = 2 * 1024 * 1024

    def __init__(self, base_url):
        if base_url not in BASE_URLS:
            raise MetadataError("Unapproved explorer base URL")
        self.base_url = base_url
        # Do not forward the admin credential through environment-configured proxies.
        self.opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), NoRedirect())
        self.requests = 0
        self.total = 0
        self.deadline = time.monotonic() + 80

    def request(self, method, path, payload=None):
        if method not in ("GET", "POST") or not path.startswith("/api/"):
            raise MetadataError("Unsupported request")
        self.requests += 1
        remaining = self.deadline - time.monotonic()
        if self.requests > self.MAX_REQUESTS or remaining <= 0:
            raise MetadataError("Request budget exhausted")
        body = None if payload is None else json.dumps(payload).encode("utf-8")
        req = urllib.request.Request(
            self.base_url + path, data=body, method=method,
            headers={"Accept": "application/json", "Content-Type": "application/json"},
        )
        try:
            with self.opener.open(req, timeout=min(10, remaining)) as response:
                chunks = []
                size = 0
                while True:
                    if time.monotonic() >= self.deadline:
                        raise MetadataError("Request deadline exceeded")
                    chunk = response.read1(min(16384, self.MAX_RESPONSE + 1 - size))
                    if not chunk:
                        break
                    chunks.append(chunk)
                    size += len(chunk)
                    if size > self.MAX_RESPONSE:
                        raise MetadataError("Response budget exceeded")
                raw = b"".join(chunks)
                self.total += len(raw)
                if len(raw) > self.MAX_RESPONSE or self.total > self.MAX_TOTAL:
                    raise MetadataError("Response budget exceeded")
                if response.status != 200:
                    raise MetadataError("Unexpected HTTP status")
                result = json.loads(raw)
                if not isinstance(result, dict):
                    raise MetadataError("Invalid JSON object response")
                return result
        except MetadataError:
            raise
        except (OSError, ValueError, urllib.error.URLError):
            # HTTP errors may contain reflected credentials; never print the exception.
            raise MetadataError("HTTP/JSON request failed; no automatic retry") from None


def load_manifest(path=None):
    path = path or Path(__file__).with_name("token-metadata.zksys.json")
    try:
        manifest = json.loads(Path(path).read_text())
        if manifest["chain_id"] != 57057 or manifest["blockscout_revision"] != REVISION:
            raise MetadataError("Unapproved chain or Blockscout revision")
        tokens = manifest["tokens"]
        if not isinstance(tokens, list) or len(tokens) != 2:
            raise MetadataError("Exactly two reviewed tokens are required")
        seen = set()
        for token in tokens:
            address = token["address"].lower()
            if address in seen or address not in EXPECTED:
                raise MetadataError("Duplicate or unapproved token")
            seen.add(address)
            expected = EXPECTED[address]
            if (token["symbol"], token["decimals"], token["icon_url"]) != expected:
                raise MetadataError("Token metadata differs from reviewed values")
        return manifest
    except MetadataError:
        raise
    except (OSError, ValueError, KeyError, TypeError, AttributeError):
        raise MetadataError("Invalid token metadata manifest") from None


def token_identity(raw, token):
    address = raw.get("address_hash")
    if not isinstance(address, str) or address.lower() != token["address"].lower():
        raise MetadataError("Token API address mismatch")
    decimals = raw.get("decimals")
    if str(decimals) != str(token["decimals"]) or raw.get("symbol") != token["symbol"]:
        raise MetadataError("Token API symbol/decimals mismatch")
    if not isinstance(raw.get("name"), str) or not raw["name"]:
        raise MetadataError("Token API name missing")
    return (address.lower(), raw["name"], raw["symbol"], str(decimals))


def publish(transport, manifest, api_key=None):
    """Preflight both tokens before any write; do not retry an ambiguous POST."""
    version = transport.request("GET", "/api/v2/config/backend-version")
    if version.get("backend_version") not in ("9.0.2", "v9.0.2"):
        raise MetadataError("Blockscout version is not the reviewed v9.0.2")
    chain = transport.request("POST", "/api/eth-rpc", {
        "jsonrpc": "2.0", "id": 1, "method": "eth_chainId", "params": [],
    })
    if (chain.get("jsonrpc"), chain.get("id"), chain.get("result")) != ("2.0", 1, "0xdee1") or "error" in chain:
        raise MetadataError("Explorer RPC chain identity mismatch")
    before = []
    for token in manifest["tokens"]:
        raw = transport.request("GET", "/api/v2/tokens/" + token["address"])
        before.append((token, token_identity(raw, token), raw.get("icon_url")))
    report = []
    for token, identity, old_icon in before:
        if old_icon == token["icon_url"]:
            state = "already-current"
        elif api_key is None:
            state = "needs-import"
        else:
            # Null fields leave name/symbol unchanged in the pinned upstream API.
            result = transport.request("POST", "/api/v2/import/token-info", {
                "api_key": api_key, "token_address": token["address"],
                "icon_url": token["icon_url"], "token_symbol": None, "token_name": None,
            })
            if result.get("message") != "Success":
                raise MetadataError("Admin import did not report success; inspect read-only before retry")
            after = transport.request("GET", "/api/v2/tokens/" + token["address"])
            if token_identity(after, token) != identity or after.get("icon_url") != token["icon_url"]:
                raise MetadataError("Post-import icon/identity mismatch; inspect read-only before retry")
            state = "imported-and-verified"
        report.append({"address": token["address"], "symbol": token["symbol"], "status": state})
    return {"chain_id": 57057, "mode": "check" if api_key is None else "apply", "tokens": report}


def read_key(stream):
    # Private pipe/stdin only: no key in argv, environment, logs or output files.
    value = stream.read(4097)
    if len(value) > 4096:
        raise MetadataError("API key input too large")
    value = value.rstrip("\r\n")
    if not value or not re.fullmatch(r"[\x21-\x7e]+", value):
        raise MetadataError("API key input must be nonempty printable ASCII without spaces")
    return value


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base-url", default=BASE_URLS[0], choices=BASE_URLS)
    parser.add_argument("--apply", action="store_true", help="Read existing admin key from private stdin and import")
    args = parser.parse_args(argv)
    try:
        manifest = load_manifest()
        key = read_key(sys.stdin) if args.apply else None
        print(json.dumps(publish(Transport(args.base_url), manifest, key), indent=2))
        return 0
    except MetadataError as error:
        print("Token metadata check/import refused: " + str(error), file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
