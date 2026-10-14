import copy
import importlib.util
import io
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import Mock


ROOT = Path(__file__).resolve().parents[2]
SOURCE = ROOT / "scripts/explorer/blockscout/publish-token-metadata.py"
SPEC = importlib.util.spec_from_file_location("blockscout_metadata", SOURCE)
metadata = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(metadata)


class FakeAPI:
    def __init__(self):
        self.tokens = metadata.load_manifest()["tokens"]
        self.state = {
            t["address"]: {"address_hash": t["address"], "name": "Existing " + t["symbol"],
                           "symbol": t["symbol"], "decimals": str(t["decimals"]), "icon_url": None}
            for t in self.tokens
        }
        self.calls = []
        self.version = "v9.0.2"
        self.chain = "0xdee1"
        self.fail_import = False
        self.corrupt_after = False

    def request(self, method, path, payload=None):
        self.calls.append((method, path, payload))
        if path.endswith("backend-version"):
            return {"backend_version": self.version}
        if path == "/api/eth-rpc":
            return {"jsonrpc": "2.0", "id": 1, "result": self.chain}
        if path.startswith("/api/v2/tokens/"):
            return copy.deepcopy(self.state[path.split("/")[-1]])
        if path == "/api/v2/import/token-info":
            if self.fail_import:
                raise metadata.MetadataError("simulated failure")
            token = self.state[payload["token_address"]]
            token["icon_url"] = payload["icon_url"]
            if self.corrupt_after:
                token["name"] = "Unexpected replacement"
            return {"message": "Success"}
        raise AssertionError("Unexpected request")

    def imports(self):
        return [call for call in self.calls if call[1] == "/api/v2/import/token-info"]


class MetadataTests(unittest.TestCase):
    def setUp(self):
        self.manifest = metadata.load_manifest()
        self.api = FakeAPI()

    def test_check_is_read_only_and_exactly_four_calls(self):
        result = metadata.publish(self.api, self.manifest)
        self.assertEqual(result["mode"], "check")
        self.assertEqual(len(self.api.calls), 4)
        self.assertEqual(self.api.imports(), [])
        self.assertEqual([t["status"] for t in result["tokens"]], ["needs-import"] * 2)

    def test_apply_uses_supported_body_preserves_identity_and_verifies_both(self):
        result = metadata.publish(self.api, self.manifest, "test-not-a-secret")
        self.assertEqual(len(self.api.calls), 8)
        self.assertEqual(len(self.api.imports()), 2)
        for _, _, body in self.api.imports():
            self.assertEqual(body["api_key"], "test-not-a-secret")
            self.assertIsNone(body["token_name"])
            self.assertIsNone(body["token_symbol"])
        self.assertNotIn("test-not-a-secret", json.dumps(result))
        self.assertEqual([t["status"] for t in result["tokens"]], ["imported-and-verified"] * 2)

    def test_already_current_does_not_import_even_when_apply(self):
        metadata.publish(self.api, self.manifest, "test")
        self.api.calls.clear()
        result = metadata.publish(self.api, self.manifest, "test")
        self.assertEqual(self.api.imports(), [])
        self.assertEqual([t["status"] for t in result["tokens"]], ["already-current"] * 2)

    def test_second_token_preflight_failure_prevents_all_imports(self):
        self.api.state[self.api.tokens[1]["address"]]["decimals"] = "6"
        with self.assertRaises(metadata.MetadataError):
            metadata.publish(self.api, self.manifest, "test")
        self.assertEqual(self.api.imports(), [])

    def test_wrong_version_or_chain_prevents_import(self):
        for attribute, value in [("version", "v9.0.1"), ("chain", "0x1")]:
            with self.subTest(attribute=attribute):
                api = FakeAPI()
                setattr(api, attribute, value)
                with self.assertRaises(metadata.MetadataError):
                    metadata.publish(api, self.manifest, "test")
                self.assertEqual(api.imports(), [])

    def test_wrong_address_or_symbol_prevents_import(self):
        for field, value in [("address_hash", "0x" + "0" * 40), ("symbol", "WRONG")]:
            with self.subTest(field=field):
                api = FakeAPI()
                api.state[api.tokens[0]["address"]][field] = value
                with self.assertRaises(metadata.MetadataError):
                    metadata.publish(api, self.manifest, "test")
                self.assertEqual(api.imports(), [])

    def test_ambiguous_import_is_never_retried(self):
        self.api.fail_import = True
        with self.assertRaises(metadata.MetadataError):
            metadata.publish(self.api, self.manifest, "test")
        self.assertEqual(len(self.api.imports()), 1)

    def test_changed_name_post_import_stops_before_second_import(self):
        self.api.corrupt_after = True
        with self.assertRaises(metadata.MetadataError):
            metadata.publish(self.api, self.manifest, "test")
        self.assertEqual(len(self.api.imports()), 1)

    def test_partial_previous_completion_only_imports_remaining_token(self):
        token = self.api.tokens[0]
        self.api.state[token["address"]]["icon_url"] = token["icon_url"]
        metadata.publish(self.api, self.manifest, "test")
        self.assertEqual(len(self.api.imports()), 1)
        self.assertEqual(self.api.imports()[0][2]["token_address"], self.api.tokens[1]["address"])

    def test_manifest_rejects_extra_duplicate_or_changed_token(self):
        changes = [lambda m: m["tokens"].append(m["tokens"][0]),
                   lambda m: m["tokens"].__setitem__(1, m["tokens"][0]),
                   lambda m: m["tokens"][0].__setitem__("icon_url", "https://attacker.invalid/a"),
                   lambda m: m.__setitem__("chain_id", 1)]
        for change in changes:
            with tempfile.TemporaryDirectory() as directory:
                value = copy.deepcopy(self.manifest)
                change(value)
                path = Path(directory) / "fixture.json"
                path.write_text(json.dumps(value))
                with self.assertRaises(metadata.MetadataError):
                    metadata.load_manifest(path)

    def test_key_input_bound_and_never_in_error(self):
        self.assertEqual(metadata.read_key(io.StringIO("abcDEF123\n")), "abcDEF123")
        for value in ["", "x" * 4097, "private key", "key\nmore", "nonascii☃"]:
            with self.assertRaises(metadata.MetadataError) as caught:
                metadata.read_key(io.StringIO(value))
            self.assertNotIn(value or "None", str(caught.exception))

    def test_only_fixed_base_urls_are_permitted(self):
        for url in ["http://attacker.invalid", "https://explorer-zk.tanenbaum.io@attacker.invalid", "http://127.0.0.1:5200/redirect"]:
            with self.assertRaises(metadata.MetadataError):
                metadata.Transport(url)

    def test_redirect_refused(self):
        with self.assertRaises(metadata.MetadataError):
            metadata.NoRedirect().redirect_request(None, None, 302, "", {}, "https://attacker.invalid")

    def test_response_size_and_call_budget(self):
        transport = metadata.Transport(metadata.BASE_URLS[0])
        response = Mock()
        response.__enter__ = Mock(return_value=response)
        response.__exit__ = Mock(return_value=False)
        raw = io.BytesIO(b"x" * (transport.MAX_RESPONSE + 1))
        response.read1 = raw.read1
        response.status = 200
        transport.opener = Mock(open=Mock(return_value=response))
        with self.assertRaises(metadata.MetadataError):
            transport.request("GET", "/api/v2/config/backend-version")
        transport.requests = transport.MAX_REQUESTS
        with self.assertRaises(metadata.MetadataError):
            transport.request("GET", "/api/v2/config/backend-version")


if __name__ == "__main__":
    unittest.main()
