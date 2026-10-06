import copy
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import threading
import unittest

import service as s
from test_service import fixture, a, h, sign


class ServiceOnlyEnrollmentTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.f = fixture()
        sub = cls.f["subscription"]
        sub["account"] = a(169)
        cls.f["subscriptions"][0]["signature"] = "0x1234"
        hashed = s.subscription_request(cls.f["settings"], sub)["struct_hash"]
        payload = cls.f["manifest"]["payload"]
        payload["subscription_snapshot_hash"] = s.keccak(s.canonical(cls.f["subscriptions"]))
        for assignment in payload["assignments"]:
            assignment.update(account=sub["account"], subscription_hash=hashed)
        cls.f["manifest"]["sequencer_signature"] = sign(s.manifest_request(cls.f["settings"], payload), "sequencer")
        for duty in cls.f["duties"]:
            duty.update(account=sub["account"], subscriptionHash=hashed)
            duty["operatorSignature"] = sign(s.duty_request(cls.f["settings"],
                {key: duty[key] for key, _ in s.DUTY}, sub), "operator")
        settings = cls.f["settings"]
        views = [("policyHash()", (), s.raw_hex(settings["policy_hash"])),
                 ("supportedLane(uint256)", (s.uint(settings["execution_chain_id"]),),
                  bytes(12) + s.raw_hex(settings["chain_address"]) + s.raw_hex(settings["vk_hash"])),
                 ("dutiesPerRound()", (), s.word(settings["duties_per_round"])),
                 ("startTime()", (), s.word(0)), ("periodSeconds()", (), s.word(100)),
                 ("firstServicePeriod()", (), s.word(5)), ("serviceActive()", (), s.word(1)),
                 ("friSubscriberCount(address,uint64)", (settings["sequencer"], 5), s.word(1)),
                 ("friSubscriberAt(address,uint64,uint256)", (settings["sequencer"], 5, 0),
                  bytes(12) + s.raw_hex(sub["account"])),
                 ("isEligibleFriSubscriber(address,address,uint64)", (sub["account"], settings["sequencer"], 5), s.word(1)),
                 ("subscriptionAt(address,address,uint64)", (sub["account"], settings["sequencer"], 5), s.raw_hex(hashed)),
                 ("operatorAccountAt(address,uint64)", (sub["operator"], 5), bytes(12) + s.raw_hex(sub["account"])),
                 ("subscription(bytes32)", (hashed,), s.encode_fields(s.SUBSCRIPTION, sub)),
                 ("seniorBonus(address)", (sub["account"],), s.word(35000))]
        cls.views = {s.cast("calldata", signature, *map(str, values)): "0x" + raw.hex()
                     for signature, values, raw in views}

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        # Only the standalone service modules are installed: no native/rental shim.
        self.source = self.root / "service-only"
        self.source.mkdir()
        for name in ("service.py", "enrollment.py", "relay.py", "workflow_io.py"):
            shutil.copy2(Path(s.__file__).parent / name, self.source / name)
        self.f = copy.deepcopy(self.f)
        self.calls = []
        self.chain_id = self.f["settings"]["registry_chain_id"]
        self.block_hash = h(100)
        owner = self

        class Handler(BaseHTTPRequestHandler):
            def do_POST(self):
                request = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
                method, params = request["method"], request["params"]
                owner.calls.append((method, params))
                if method == "eth_chainId":
                    result = owner.chain_id
                elif method == "eth_getBlockByHash" and params == [h(100), False]:
                    result = {"hash": owner.block_hash, "timestamp": "0x226"}
                elif (method == "eth_call" and len(params) == 2
                      and params[1] == {"blockHash": h(100), "requireCanonical": True}
                      and params[0]["to"] == owner.f["settings"]["registry"]):
                    result = owner.views.get(params[0]["data"])
                else:
                    result = None
                data = s.canonical({"jsonrpc": "2.0", "id": request["id"], "result": result})
                self.send_response(200)
                self.send_header("Content-Length", str(len(data)))
                self.end_headers()
                self.wfile.write(data)

            def log_message(self, *args):
                pass

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.addCleanup(self.server.server_close)
        self.addCleanup(self.server.shutdown)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()
        self.connection = self.write("connection", {"url": f"http://127.0.0.1:{self.server.server_port}",
                                                    "authorization": None})

    def write(self, name, value):
        path = self.root / (name + ".json")
        path.write_bytes(s.canonical(value))
        path.chmod(0o600)
        return path

    def run_cli(self, command, context=True):
        output = self.root / (command + ".output.json")
        args = [sys.executable, str(self.source / "service.py"), "--config", str(self.write("settings", self.f["settings"])),
                "--output", str(output)]
        if context:
            args += ["--registry-rpc-file", str(self.connection), "--enrollment-block-hash", h(100)]
        args += [command]
        for name in ("evidence", "manifest", "subscriptions"):
            args += ["--" + name, str(self.write(name, self.f[name]))]
        args += ["--proof", str(self.write("proof", self.f["snark"] if command == "package" else self.f["proofs"][0]))]
        if command == "duty":
            args += ["--authority", str(self.write("authority", self.f["authorities"][0]))]
        elif command == "package":
            for name in ("duties", "proposal", "fri_payload"):
                args += ["--" + name.replace("_", "-"), str(self.write(name, self.f[name]))]
        env = {**os.environ, "ZKSYNC_AIRBENDER_PROVER_DIR": str(self.root / "missing-rental"),
               "PYTHONPATH": "", "PYTHONDONTWRITEBYTECODE": "1"}
        result = subprocess.run(args, env=env, cwd=self.source, text=True, capture_output=True, timeout=30)
        return result, output

    def test_all_enrollment_cli_paths_work_without_rental_checkout(self):
        for command in ("offered-duty", "duty", "package"):
            with self.subTest(command=command):
                result, output = self.run_cli(command)
                self.assertEqual(result.returncode, 0, result.stderr)
                value = json.loads(output.read_bytes())
                self.assertEqual(value["sidecar"]["duties"][0]["account"] if command == "package"
                                 else value["typed_data"]["message"]["account"], a(169))
        self.assertTrue(any(method == "eth_call" for method, _ in self.calls))

    def test_offline_contract_account_still_requires_canonical_consent(self):
        result, output = self.run_cli("offered-duty", context=False)
        self.assertEqual(result.returncode, 1)
        self.assertIn("eoa_signature_required", result.stderr)
        self.assertFalse(output.exists())
        self.assertEqual(self.calls, [])

    def test_wrong_chain_or_anchor_rejects_before_output(self):
        for field, value, error in (("chain_id", "0x1", "wrong_rpc_chain"),
                                    ("block_hash", h(101), "enrollment_block_changed")):
            with self.subTest(field=field):
                original = getattr(self, field)
                setattr(self, field, value)
                result, output = self.run_cli("offered-duty")
                setattr(self, field, original)
                self.assertEqual(result.returncode, 1)
                self.assertIn(error, result.stderr)
                self.assertFalse(output.exists())


if __name__ == "__main__":
    unittest.main()
