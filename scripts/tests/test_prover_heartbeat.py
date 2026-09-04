"""SYSCOIN: No-network regressions for the explicitly funded companion policy."""

import copy
import importlib.util
from pathlib import Path
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, patch


SCRIPT = Path(__file__).resolve().parents[1] / "prover-heartbeat.py"
SPEC = importlib.util.spec_from_file_location("prover_heartbeat", SCRIPT)
HEARTBEAT = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(HEARTBEAT)
ADDRESS = "0x" + "11" * 20
TX_HASH = "0x" + "22" * 32
JOB = {"fri_job": {"batch_number": 7, "vk_hash": "0x" + "ab" * 32},
       "assigned_to_prover_id": None, "added_seconds_ago": 3600}


class ProverHeartbeatTests(unittest.TestCase):
    def setUp(self):
        self.args = SimpleNamespace(
            rpc_url="http://127.0.0.1:3050", prover_url="http://127.0.0.1:3320/prover-jobs/v1",
            chain_id=57001, min_age=300, send=True, address=ADDRESS,
            keystore="/not-a-real-wallet", password_file="/not-a-real-password", cast="cast",
            gas_limit=100_000, max_fee_per_gas=1000,
        )
        self.heartbeat = HEARTBEAT.Heartbeat(self.args)
        self.heartbeat.status = Mock(side_effect=lambda stage: [copy.deepcopy(JOB)] if stage == "SNARK" else [])
        self.responses = {
            "eth_chainId": hex(57001),
            "eth_getBlockByNumber": {"timestamp": "0x1", "baseFeePerGas": "0x64"},
            "eth_getCode": "0x", "eth_getTransactionCount": "0x5",
            "eth_getBalance": hex(100_000 * 1000),
        }
        self.heartbeat.rpc = Mock(side_effect=lambda method, params: self.responses[method])
        self.process = patch.object(HEARTBEAT.subprocess, "run")
        self.run = self.process.start()
        self.addCleanup(self.process.stop)
        self.run.side_effect = lambda argv, **kwargs: SimpleNamespace(stdout=ADDRESS if argv[1] == "wallet" else TX_HASH)

    def test_only_unleased_aged_real_singleton_needs_traffic(self):
        self.assertEqual(HEARTBEAT.singleton_batch([JOB], [], 300), 7)
        for jobs, fris in [([], []), ([JOB, JOB], []), ([JOB], [JOB])]:
            self.assertIsNone(HEARTBEAT.singleton_batch(jobs, fris, 300))
        for field, value in [("assigned_to_prover_id", "worker"), ("added_seconds_ago", 299)]:
            job = copy.deepcopy(JOB)
            job[field] = value
            self.assertIsNone(HEARTBEAT.singleton_batch([job], [], 300))
        for vk in ["0x" + "00" * 32, "malformed"]:
            job = copy.deepcopy(JOB)
            job["fri_job"]["vk_hash"] = vk
            self.assertIsNone(HEARTBEAT.singleton_batch([job], [], 300))

    def test_dry_run_never_opens_wallet(self):
        self.args.send = False
        self.assertIn("dry-run", self.heartbeat.tick())
        self.run.assert_not_called()

    def test_status_uses_server_lowercase_stage_paths(self):
        heartbeat = HEARTBEAT.Heartbeat(self.args)
        with patch.object(HEARTBEAT, "read_json", return_value=[]) as read_json:
            for stage in ["SNARK", "FRI"]:
                self.assertEqual(heartbeat.status(stage), [])
                self.assertEqual(
                    read_json.call_args.args[0],
                    self.args.prover_url + "/status/" + stage.lower(),
                )

    def test_send_is_zero_value_self_transfer_with_chain_nonce_and_fee_caps(self):
        self.assertIn(TX_HASH, self.heartbeat.tick())
        argv = self.run.call_args.args[0]
        self.assertEqual(argv[:3], ["cast", "send", ADDRESS])
        for flag, value in {"--from": ADDRESS, "--value": "0", "--data": "0x", "--chain": "57001",
                            "--nonce": "5", "--gas-limit": "100000", "--gas-price": "1000",
                            "--priority-gas-price": "0", "--keystore": self.args.keystore}.items():
            self.assertEqual(argv[argv.index(flag) + 1], value)
        self.heartbeat.tick()
        self.assertEqual(self.run.call_count, 2)  # wallet address + one broadcast only

    def test_identity_fee_code_and_balance_fail_closed(self):
        for method, value in [("eth_chainId", "0x1"), ("eth_getCode", "0xef0100"),
                              ("eth_getBalance", "0x0"),
                              ("eth_getBlockByNumber", {"timestamp": "0x1", "baseFeePerGas": "0xffff"})]:
            with self.subTest(method=method):
                original = self.responses[method]
                self.responses[method] = value
                with self.assertRaises(RuntimeError):
                    self.heartbeat.tick()
                self.assertFalse(any(call.args[0][1] == "send" for call in self.run.call_args_list))
                self.responses[method] = original
                self.run.reset_mock()

    def test_pending_nonce_and_new_traffic_suppress_broadcast(self):
        self.heartbeat.rpc.side_effect = lambda method, params: (
            "0x6" if method == "eth_getTransactionCount" and params[-1] == "pending"
            else self.responses[method]
        )
        self.assertIn("pending work", self.heartbeat.tick())
        self.heartbeat.rpc.side_effect = lambda method, params: self.responses[method]
        self.heartbeat.status.side_effect = [[JOB], [], [], []]
        self.assertIn("queue progressed", self.heartbeat.tick())
        self.assertFalse(any(call.args[0][1] == "send" for call in self.run.call_args_list))

    def test_ambiguous_send_is_not_automatically_repeated(self):
        self.run.side_effect = [SimpleNamespace(stdout=ADDRESS), TimeoutError("ambiguous send")]
        with self.assertRaises(TimeoutError):
            self.heartbeat.tick()
        self.assertEqual(self.heartbeat.last_attempted_batch, 7)
        self.heartbeat.tick()
        self.assertEqual(self.run.call_count, 2)

    def test_remote_http_and_url_credentials_are_rejected_before_network(self):
        for url in ["http://prover.example/status", "https://user:secret@prover.example/status"]:
            with self.assertRaises(RuntimeError):
                HEARTBEAT.read_json(url)


if __name__ == "__main__":
    unittest.main()
