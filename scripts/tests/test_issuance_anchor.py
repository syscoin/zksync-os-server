# SYSCOIN: Receipt-relative issuance regressions; synthetic RPC records, no signing or live RPC.
from __future__ import annotations

import copy
import importlib.util
import io
import json
import os
import stat
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
HELPER = ROOT / "scripts/gateway-launch/_issuance_anchor.py"
sys.path.insert(0, str(HELPER.parent))
spec = importlib.util.spec_from_file_location("issuance_anchor", HELPER)
anchor = importlib.util.module_from_spec(spec)
spec.loader.exec_module(anchor)


class IssuanceAnchorTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.path = Path(self.temporary.name) / "anchor.json"
        self.identity = {"edge_chain_id": "57057", "signer": "0x" + "11" * 20,
                         "create2": "0x" + "22" * 20, "token": "0x" + "33" * 20,
                         "token_calldata": "0x1234", "delay_seconds": 86400}
        self.transaction = {"hash": "0x" + "44" * 32, "from": self.identity["signer"],
                            "to": self.identity["create2"], "input": "0x1234", "value": "0x0",
                            "chainId": "0xdee1", "blockHash": "0x" + "55" * 32, "blockNumber": "0x7"}
        self.receipt = {"transactionHash": self.transaction["hash"], "status": "0x1",
                        "from": self.identity["signer"], "to": self.identity["create2"],
                        "blockHash": self.transaction["blockHash"], "blockNumber": "0x7"}
        self.block = {"hash": self.receipt["blockHash"], "number": "0x7", "timestamp": "0x1000"}

    def make(self):
        return anchor.make_anchor(self.identity, self.transaction, self.receipt, self.block)

    def fake_rpc(self, method, params):
        return {"eth_chainId": "0xdee1", "eth_getTransactionReceipt": self.receipt,
                "eth_getTransactionByHash": self.transaction, "eth_getBlockByNumber": self.block,
                "eth_getCode": "0x6000"}[method]

    def resolve(self, tx_hash="", rpc=None):
        with patch.object(sys, "argv", [str(HELPER), "resolve", str(self.path), tx_hash]), \
                patch.object(sys, "stdin", io.StringIO(json.dumps(self.identity))), \
                patch.object(sys, "stdout", io.StringIO()) as output, \
                patch.object(anchor, "rpc", rpc or self.fake_rpc):
            anchor.main()
            return output.getvalue()

    def test_exact_twenty_four_hours_after_deployment_block(self):
        value = self.make()
        self.assertEqual(value["issuer_start_time"], 4096 + 86400)
        self.assertEqual(value["transaction_hash"], self.transaction["hash"])
        self.assertEqual(value["block_number"], 7)

    def test_specializes_only_the_single_compiler_token_immutable(self):
        artifact = {"deployedBytecode": {"object": "0x6000" + "00" * 32,
                    "immutableReferences": {"71": [{"start": 2, "length": 32}]}}}
        self.assertEqual(anchor.specialize_token_runtime(artifact, self.identity["token"]),
                         "0x6000" + "00" * 12 + "33" * 20)
        for references in ({}, {"71": []}, {"71": [{"start": 2, "length": 20}]},
                           {"71": [{"start": 999, "length": 32}]}, {"71": [], "72": []}):
            artifact["deployedBytecode"]["immutableReferences"] = references
            with self.assertRaises(ValueError):
                anchor.specialize_token_runtime(artifact, self.identity["token"])

    def test_bad_admin_salt_or_profile_fails_before_any_prelude_send(self):
        source = (HELPER.parent / "zksys-l2-bootstrap.sh").read_text()
        guard = source[source.index("# SYSCOIN: Token choices also bind"):source.index("# SYSCOIN: Bind the source, signer")]
        expected_hash = "0x1fce42acba699bc198d2e146b0284e3bdd821d1634cd809f1c0a12e961dac561"
        expected_runtime = "0x041faf31b2f3576502f25fd5d106eaf411611e42dc996c28872abe487cb6e269"
        expected_address = "0xb49943ea232624dd4aa63e18186076c6c99a68ef"
        artifact = Path(self.temporary.name) / "out/ZkSysGasTank.sol/ZkSysGasTank.json"
        artifact.parent.mkdir(parents=True)
        artifact.write_text(json.dumps({"deployedBytecode": {"object": "0x6000" + "00" * 32,
            "immutableReferences": {"71": [{"start": 2, "length": 32}]}}}))
        for scenario in ("admin", "salt", "profile", "valid"):
            sends = Path(self.temporary.name) / (scenario + ".sends")
            setup = '''set -euo pipefail
gl_die() { echo "$*" >&2; exit 1; }
gl_to_lower() { printf '%s' "$1" | tr '[:upper:]' '[:lower:]'; }
forge_inspect_bytecode() { printf '0x6000'; }
cast() {
 case "$1" in
 abi-encode) printf '0x00';;
 create2) if [ "$SCENARIO" = salt ]; then printf '0x0000000000000000000000000000000000000001'; else printf '%s' "$EXPECTED_ADDRESS"; fi;;
 keccak) if [ "$2" = 0x600000 ]; then
   if [ "$SCENARIO" = admin ] || [ "$SCENARIO" = profile ]; then printf '0x00'; else printf '%s' "$EXPECTED_HASH"; fi
  else printf '%s' "$EXPECTED_RUNTIME"; fi;;
 esac
}
'''
            result = subprocess.run(["bash", "-c", setup + guard + '\nprintf send > "$SEND_FILE"'],
                capture_output=True, text=True, env={**os.environ, "SCENARIO": scenario,
                "EXPECTED_HASH": expected_hash, "EXPECTED_RUNTIME": expected_runtime,
                "EXPECTED_ADDRESS": expected_address, "SEND_FILE": str(sends),
                "ZKSYS_L2_TOKEN_ADDRESS": self.identity["token"], "ZKSYS_L2_TOKEN_DECIMALS": "18",
                "ZKSYS_L2_CREATE2_DEPLOYER": self.identity["create2"], "ZKSYS_L2_GAS_TANK_SALT": "0x01",
                "zksys_bootstrap_forge_inspect_dir": self.temporary.name, "SCRIPT_DIR": str(HELPER.parent)})
            with self.subTest(scenario=scenario):
                self.assertEqual(result.returncode == 0, scenario == "valid", result.stderr)
                self.assertEqual(sends.exists(), scenario == "valid")

    def test_integer_rpc_fields_and_boolean_rejection(self):
        self.receipt["status"] = 1
        self.block["timestamp"] = 4096
        self.assertEqual(self.make()["issuer_start_time"], 90496)
        with self.assertRaises(ValueError):
            anchor.uint(True, "timestamp")

    def test_rejects_wrong_receipt_or_transaction_identity(self):
        for obj, field, value in [
            (self.receipt, "status", "0x0"), (self.receipt, "from", "0x" + "77" * 20),
            (self.receipt, "to", "0x" + "77" * 20),
            (self.transaction, "hash", "0x" + "66" * 32),
            (self.transaction, "from", "0x" + "77" * 20),
            (self.transaction, "to", "0x" + "77" * 20),
            (self.transaction, "input", "0x5678"), (self.transaction, "value", "0x1"),
            (self.transaction, "chainId", "0x1644")]:
            with self.subTest(field=field, value=value):
                old = obj[field]
                obj[field] = value
                with self.assertRaises(ValueError): self.make()
                obj[field] = old

    def test_rejects_noncanonical_blocks_and_overflow(self):
        for obj, field, value in [(self.block, "hash", "0x" + "66" * 32),
                                  (self.block, "number", "0x8"),
                                  (self.transaction, "blockNumber", "0x8"),
                                  (self.transaction, "blockHash", "0x" + "66" * 32),
                                  (self.block, "timestamp", str(2**256 - 1))]:
            with self.subTest(field=field):
                old = obj[field]
                obj[field] = value
                with self.assertRaises(ValueError): self.make()
                obj[field] = old

    def test_receipt_is_private_and_retry_reuses_exact_timestamp(self):
        self.assertEqual(self.resolve(self.transaction["hash"]), "90496\n")
        self.assertEqual(stat.S_IMODE(self.path.stat().st_mode), 0o600)
        original = self.path.read_bytes()
        self.assertEqual(self.resolve(), "90496\n")
        self.assertEqual(self.path.read_bytes(), original)

    def test_no_anchor_requires_explicit_deployment_hash(self):
        with self.assertRaisesRegex(ValueError, "exact deployment transaction hash"):
            self.resolve()
        self.assertFalse(self.path.exists())

    def test_retry_rejects_identity_drift_and_reorg(self):
        self.resolve(self.transaction["hash"])
        original = self.path.read_bytes()
        self.identity["token"] = "0x" + "99" * 20
        with self.assertRaisesRegex(ValueError, "identity differs"):
            self.resolve()
        self.identity["token"] = "0x" + "33" * 20
        self.block["hash"] = "0x" + "66" * 32
        with self.assertRaisesRegex(ValueError, "not canonical"):
            self.resolve()
        self.assertEqual(self.path.read_bytes(), original)

    def test_rejects_wrong_rpc_chain_or_missing_token(self):
        for method, value in [("eth_chainId", "0x1644"), ("eth_getCode", "0x")]:
            with self.subTest(method=method):
                def rpc(m, p):
                    return value if m == method else self.fake_rpc(m, p)
                with self.assertRaises(ValueError): self.resolve(self.transaction["hash"], rpc)
                self.assertFalse(self.path.exists())

    def test_immutable_prelude_binding_and_unsafe_files(self):
        anchor.bind_private_json(self.path, self.identity)
        anchor.bind_private_json(self.path, self.identity)
        changed = copy.deepcopy(self.identity)
        changed["token"] = "0x" + "99" * 20
        with self.assertRaisesRegex(ValueError, "differs"):
            anchor.bind_private_json(self.path, changed)
        self.path.chmod(0o644)
        with self.assertRaisesRegex(ValueError, "unsafe"):
            anchor.read_private_json(self.path)
        self.path.chmod(0o600)
        linked = self.path.with_name("linked.json")
        linked.symlink_to(self.path)
        with self.assertRaisesRegex(ValueError, "unsafe"):
            anchor.read_private_json(linked)
        linked.unlink()
        os.link(self.path, linked)
        with self.assertRaisesRegex(ValueError, "unsafe"):
            anchor.read_private_json(self.path)

    def test_rpc_error_does_not_echo_credentials(self):
        with patch.dict(os.environ, {"ZKSYS_L2_RPC_URL": "https://user:secret@invalid/token"}), \
                patch.object(anchor.urllib.request, "urlopen", side_effect=OSError("secret@invalid/token")):
            with self.assertRaises(ValueError) as error: anchor.rpc("eth_chainId", [])
        self.assertNotIn("secret", str(error.exception))
        self.assertNotIn("invalid", str(error.exception))

    def test_shell_stage_binds_before_sends_and_consumes_anchor_before_issuer(self):
        source = (HELPER.parent / "zksys-l2-bootstrap.sh").read_text()
        stage = source[source.index('if [ "${TOKEN_PRELUDE}" = true ]; then'):source.index('\nregistry_impl_init_code=')]
        self.assertLess(stage.index(' bind "${token_prelude_manifest}"'), stage.index('deploy_create2 "zkSYS proxy admin"'))
        self.assertEqual(stage.count('deploy_create2 "'), 3)
        self.assertNotIn('grantRole', stage)
        self.assertNotIn('issuer implementation', stage)
        self.assertIn('conflicts with the canonical token deployment receipt', stage)
        self.assertLess(source.index('anchored_start='), source.index('issuer_init_data='))


if __name__ == "__main__":
    unittest.main()
