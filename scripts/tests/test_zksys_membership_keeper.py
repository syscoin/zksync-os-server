from __future__ import annotations

import copy
import contextlib
import fcntl
import importlib.util
import io
import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch


ROOT = Path(__file__).resolve().parents[2]
MODULE = ROOT / "scripts" / "zksys_membership_keeper.py"
spec = importlib.util.spec_from_file_location("zksys_membership_keeper", MODULE)
keeper = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = keeper
spec.loader.exec_module(keeper)
spec = importlib.util.spec_from_file_location("keeper_transactions", ROOT / "scripts" / "zksys_keeper_transactions.py")
transactions = importlib.util.module_from_spec(spec)
spec.loader.exec_module(transactions)
sys.modules["zksys_keeper_transactions"] = transactions
spec = importlib.util.spec_from_file_location("keeper_cast", ROOT / "scripts" / "zksys_keeper_cast.py")
cast_adapter = importlib.util.module_from_spec(spec)
spec.loader.exec_module(cast_adapter)
sys.modules["zksys_keeper_cast"] = cast_adapter
spec = importlib.util.spec_from_file_location("keeper_runner", ROOT / "scripts" / "zksys_keeper_runner.py")
runner_module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(runner_module)
sys.modules["zksys_keeper_runner"] = runner_module
spec = importlib.util.spec_from_file_location("keeper_hooks", ROOT / "scripts" / "zksys_keeper_hooks.py")
hooks_module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(hooks_module)
sys.modules["zksys_keeper_hooks"] = hooks_module

A = "0x" + "11" * 20
B = "0x" + "22" * 20
C = "0x" + "33" * 20


def entry(address, identity="01"):
    return {"proTxHash": identity * 32, "state": {"nevmAddress": address}}


class MembershipDiscoveryTests(unittest.TestCase):
    def test_replacement_removal_and_l2_only_address_survive_restart(self):
        state = {"ever_seen": []}
        keeper.merge_discovery(state, [entry(A), entry(B, "02")], [])
        # A changed to C, B disappeared; another caller placed B on L2.
        persisted = copy.deepcopy(state)
        merged = keeper.merge_discovery(persisted, [entry(C)], [B])
        self.assertEqual(set(merged), {A, B, C})
        self.assertEqual(set(persisted["ever_seen"]), {A, B, C})
        # The absence of A and B on both chains must not erase history.
        after_restart = copy.deepcopy(persisted)
        self.assertEqual(set(keeper.merge_discovery(after_restart, [entry(C)], [])), {A, B, C})

    def test_discovery_deduplicates_and_ignores_empty_node_addresses(self):
        state = {"ever_seen": []}
        merged = keeper.merge_discovery(state, [entry(A), entry("", "02")], [A])
        self.assertEqual([value.lower() for value in merged], [A])

    def test_malformed_node_address_is_not_silently_ignored(self):
        with self.assertRaises(keeper.KeeperError):
            keeper.address_set([entry("0x1234")])

    def test_duplicate_nevm_or_protx_rejected(self):
        for entries in ([entry(A), entry(A, "02")], [entry(A), entry(B)]):
            with self.subTest(entries=entries), self.assertRaises(keeper.KeeperError):
                keeper.address_set(entries)


class ActivationTests(unittest.TestCase):
    def test_pending_sentry_waits_until_effective_period(self):
        self.assertFalse(keeper.mature_pending((0, 0, 100, 3), 2))
        self.assertTrue(keeper.mature_pending((0, 0, 100, 3), 3))
        self.assertTrue(keeper.mature_pending((0, 0, 100, 3), 4))

    def test_period_zero_can_be_activated_before_issuance(self):
        self.assertTrue(keeper.mature_pending((0, 0, 100, 0), 0))

    def test_stake_only_pending_does_not_become_sentry_activation(self):
        self.assertFalse(keeper.mature_pending((100, 0, 0, 0), 1))

    def test_activation_requires_current_matching_l1_and_l2_membership(self):
        l1_head = {"number": 10, "hash": "0x" + "aa" * 32}
        l2_head = {"number": 20, "hash": "0x" + "bb" * 32}
        l1, l2 = object(), RPC(lambda method, params: "0x" + b"".join(
            value.to_bytes(32, "big") for value in (0, 0, 100, 0)).hex())
        facts = {"nevmCollateralHeight(address)": 8, "nevmSentryNodeWeight(address)": 100, "currentPeriod()": 0}
        def read_word(rpc, contract, signature, block_hash, argument=b""):
            return facts[signature].to_bytes(32, "big")
        def read_block(rpc, tag):
            return l1_head if rpc is l1 else l2_head
        with patch.object(keeper, "eth_call_word", side_effect=read_word), \
             patch.object(keeper, "member_at", return_value=(8, 100)), \
             patch.object(keeper, "block", side_effect=read_block):
            self.assertTrue(keeper.activation_ready(l1, l2, A, B, C, A, B, l1_head, l2_head))
            facts["nevmCollateralHeight(address)"] = 0
            facts["nevmSentryNodeWeight(address)"] = 0
            self.assertFalse(keeper.activation_ready(l1, l2, A, B, C, A, B, l1_head, l2_head))
            facts["nevmCollateralHeight(address)"] = 9
            facts["nevmSentryNodeWeight(address)"] = 100
            self.assertFalse(keeper.activation_ready(l1, l2, A, B, C, A, B, l1_head, l2_head))


class BridgeEncodingTests(unittest.TestCase):
    def test_push_matches_solidity_abi(self):
        expected = subprocess.check_output([
            "cast", "calldata", "pushSentryNodeUpdates(address[],uint256,uint256,address)",
            f"[{A},{B}]", "1000000", "800", C,
        ], text=True).strip().lower()
        self.assertEqual(keeper.encode_push([A, B], 1_000_000, 800, C).lower(), expected)

    def test_decode_priority_payload_and_reject_invalid_calldata(self):
        payload = subprocess.check_output([
            "cast", "calldata", "applyL1SentryNodeUpdates((address,uint32,uint128)[],uint64,uint64)",
            f"[({A},42,100),({B},0,0)]", "100", "123456",
        ], text=True).strip()
        self.assertEqual(keeper.decode_updates(payload), ([(A, 42, 100), (B, 0, 0)], 100, 123456))
        for invalid in (payload[:-2], "0xffffffff" + payload[10:], payload + "00"):
            with self.subTest(invalid=invalid), self.assertRaises(keeper.KeeperError):
                keeper.decode_updates(invalid)


class RPC:
    def __init__(self, handle):
        self.handle = handle
        self.calls = []

    def call(self, method, params):
        self.calls.append((method, params))
        return self.handle(method, params)


class SnapshotTests(unittest.TestCase):
    def make_clients(self, reorg=False):
        # 0x61 exposes serialized Core uint256 bytes; Core RPC uses display order.
        serialized_core_hash = bytes(range(32))
        core_hash = serialized_core_hash[::-1].hex()
        l1_hash = "0x" + "bb" * 32
        l2_hash = "0x" + "cc" * 32
        checks = []

        def core(method, params):
            if method == "getblockheader":
                self.assertEqual(params, [core_hash])
                return {"hash": core_hash, "height": 99}
            if method == "getblockhash":
                checks.append(params[0])
                return "dd" * 32 if reorg and len(checks) > 1 else core_hash
            if method == "protx_list":
                self.assertEqual(params, ["registered", True, 99])
                return [entry(A)]
            raise AssertionError(method)

        def l1(method, params):
            if method == "eth_getBlockByNumber":
                return {"hash": l1_hash, "number": "0x64"}
            self.assertEqual(method, "eth_call")
            self.assertEqual(params[0]["data"], "0x" + (99).to_bytes(8, "big").hex())
            self.assertEqual(params[1], {"blockHash": l1_hash, "requireCanonical": True})
            return "0x" + serialized_core_hash.hex()

        def l2(method, params):
            if method == "eth_getBlockByNumber":
                return {"hash": l2_hash, "number": "0x4"}
            self.assertEqual(method, "eth_call")
            self.assertEqual(params[1], {"blockHash": l2_hash, "requireCanonical": True})
            data = params[0]["data"]
            if len(data) == 10:
                return "0x" + (1).to_bytes(32, "big").hex()
            return "0x" + "00" * 12 + B[2:]

        return RPC(core), RPC(l1), RPC(l2)

    def test_snapshot_pins_l2_and_reads_core_at_paired_height(self):
        core, l1, l2 = self.make_clients()
        snapshot = keeper.snapshot(core, l1, l2, C)
        self.assertEqual(snapshot["l2_active"], [B])
        self.assertEqual(snapshot["entries"], [entry(A)])
        self.assertEqual(snapshot["core_height"], 99)

    def test_core_reorg_during_discovery_is_rejected(self):
        core, l1, l2 = self.make_clients(reorg=True)
        with self.assertRaisesRegex(keeper.KeeperError, "core_pair_changed"):
            keeper.snapshot(core, l1, l2, C)

    def test_reconcile_prioritizes_removal_before_addition_and_ignores_unchanged(self):
        l1_hash = "0x" + "aa" * 32
        l2_hash = "0x" + "bb" * 32
        heights = {A: 0, B: 80, C: 80}
        weights = {A: 0, B: 100, C: 100}
        old = {A: (80, 100), B: (0, 0), C: (80, 100)}
        height_selector = keeper.selector("nevmCollateralHeight(address)").hex()
        weight_selector = keeper.selector("nevmSentryNodeWeight(address)").hex()

        def l1(method, params):
            if method == "eth_getBlockByNumber": return {"hash": l1_hash, "number": "0x64"}
            self.assertEqual(params[1], {"blockHash": l1_hash, "requireCanonical": True})
            data = params[0]["data"]
            account = "0x" + data[-40:]
            value = heights[account] if data[2:10] == height_selector else weights[account]
            self.assertIn(data[2:10], (height_selector, weight_selector))
            return "0x" + value.to_bytes(32, "big").hex()

        def l2(method, params):
            if method == "eth_getBlockByNumber": return {"hash": l2_hash, "number": "0x10"}
            self.assertEqual(params[1], {"blockHash": l2_hash, "requireCanonical": True})
            account = "0x" + params[0]["data"][-40:]
            height, weight = old[account]
            return "0x" + height.to_bytes(32, "big").hex() + weight.to_bytes(32, "big").hex()

        changes = keeper.reconcile(RPC(l1), RPC(l2), C, B, [B, C, A],
                                   {"number": 100, "hash": l1_hash}, {"number": 16, "hash": l2_hash})
        self.assertEqual([change["account"] for change in changes], [A, B])
        self.assertEqual(changes[0]["l1"], (0, 0))
        self.assertEqual(changes[1]["l1"], (80, 100))


class TransactionSafetyTests(unittest.TestCase):
    def setUp(self):
        self.hash = "0x" + "44" * 32
        self.blockhash = "0x" + "55" * 32
        self.raw = "0xabcd"
        self.journal = {}
        self.persisted = []
        self.tx = None
        self.sent = []

        def save(journal):
            self.persisted.append(copy.deepcopy(journal))

        def sign(tx):
            self.tx = tx
            return self.raw

        self.save = save
        self.sign = sign
        self.driver = transactions.TransactionDriver(self.journal, save, sign, lambda raw: self.tx, lambda raw: self.hash)

    def rpc(self, method, params):
        if method == "eth_chainId": return "0x39"
        if method == "eth_getTransactionCount": return "0x0"
        if method in ("eth_getTransactionReceipt", "eth_getTransactionByHash"): return None
        if method == "eth_sendRawTransaction":
            self.sent.append(params[0])
            raise TimeoutError("submission uncertain")
        raise AssertionError(method)

    def test_restart_uses_persisted_signed_bytes_and_rejects_new_nonce(self):
        with self.assertRaises(TimeoutError):
            self.driver.begin("l1", self.rpc, 57, A, B, "0x1234", 1, 100000, 10, kind="bridge")
        restored = self.persisted[-1]
        restarted = transactions.TransactionDriver(restored, self.save, self.sign, lambda raw: self.tx, lambda raw: self.hash)
        with self.assertRaises(transactions.TransactionError):
            restarted.begin("l1", self.rpc, 57, A, B, "0x1234", 1, 100000, 10, kind="bridge")
        with self.assertRaises(TimeoutError):
            restarted.broadcast("l1", self.rpc)
        self.assertEqual(self.sent, [self.raw, self.raw])

    def test_finality_check_rejects_reorg_between_reads(self):
        receipt = {"transactionHash": self.hash, "blockHash": self.blockhash,
                   "blockNumber": "0x5", "status": "0x1"}
        canonical_reads = 0
        def rpc(method, params):
            nonlocal canonical_reads
            if method == "eth_getTransactionReceipt": return receipt
            if params[0] == "finalized": return {"number": "0x6"}
            canonical_reads += 1
            return {"hash": self.blockhash if canonical_reads == 1 else "0x" + "77" * 32}
        with self.assertRaisesRegex(transactions.TransactionError, "changed"):
            transactions.finalized_receipt(rpc, self.hash)

    def test_unavailable_finality_is_not_success(self):
        receipt = {"transactionHash": self.hash, "blockHash": self.blockhash,
                   "blockNumber": "0x5", "status": "0x1"}
        def rpc(method, params):
            if method == "eth_getTransactionReceipt": return receipt
            if params[0] == "finalized": return None
            return {"hash": self.blockhash}
        with self.assertRaisesRegex(transactions.TransactionError, "finality unavailable"):
            transactions.finalized_receipt(rpc, self.hash)

    def test_priority_event_tuple_is_decoded_and_hash_authenticated(self):
        tuple_type = "(uint256,uint256,uint256,uint256,uint256,uint256,uint256,uint256,uint256,uint256,uint256[4],bytes,bytes,uint256[],bytes,bytes)"
        tuple_value = f"(127,{int(A,16)},{int(B,16)},1000000,800,2,0,0,5,0,[0,0,0,0],0x11223344,0x,[],0x,0x)"
        encoded = subprocess.check_output(["cast", "abi-encode", f"f({tuple_type})", tuple_value], text=True).strip()
        digest = subprocess.check_output(["cast", "keccak", encoded], text=True).strip().lower()
        event_data = subprocess.check_output([
            "cast", "abi-encode", f"f(uint256,bytes32,uint64,{tuple_type},bytes[])",
            "5", digest, "1000", tuple_value, "[]",
        ], text=True).strip()
        topic = "0x" + "66" * 32
        receipt = {"logs": [{"address": C, "topics": [topic], "data": event_data}]}
        def keccak(raw):
            return subprocess.check_output(["cast", "keccak", "0x" + raw.hex()], text=True).strip()
        payload = transactions.priority_payload(receipt, C, topic, digest, keccak)
        self.assertEqual(payload, {"from": A, "to": B, "input": "0x11223344", "transactionType": 127})
        changed = copy.deepcopy(receipt)
        # The factoryDeps array lies after the canonical tuple and is excluded
        # from the canonical hash; tamper with the tuple payload instead.
        data = bytearray.fromhex(event_data[2:])
        index = data.find(bytes.fromhex("11223344"))
        self.assertGreater(index, 0)
        data[index] ^= 1
        changed["logs"][0]["data"] = "0x" + data.hex()
        with self.assertRaises(transactions.TransactionError):
            transactions.priority_payload(changed, C, topic, digest, keccak)

    def test_cast_signer_signs_and_decodes_real_legacy_transaction(self):
        # An explicitly disposable key; no network submission or user wallet access.
        with tempfile.TemporaryDirectory() as directory:
            secret = "0x" + "00" * 31 + "01"
            password = Path(directory) / "password"
            password.write_text("test-only-password\n")
            subprocess.run(["cast", "wallet", "import", "test-only", "--keystore-dir", directory,
                            "--private-key", secret, "--unsafe-password", "test-only-password"],
                           check=True, capture_output=True, text=True, timeout=60)
            signer = cast_adapter.CastSigner("http://127.0.0.1:1", str(Path(directory) / "test-only"), str(password))
            expected = {"from": "0x7e5f4552091a69125d5dfcb7b8c2659029395bdf", "to": A,
                        "data": "0x12345678", "chainId": 57, "nonce": 3,
                        "value": 100, "gas": 100000, "gasPrice": 10, "type": 0}
            raw = signer.sign(expected)
            decoded = signer.decode(raw)
            self.assertTrue(transactions._same_transaction(expected, decoded))
            self.assertEqual(transactions._quantity(decoded.get("type", 0)), 0)
            self.assertEqual(signer.hash_raw(raw), subprocess.check_output(["cast", "keccak", raw], text=True).strip())


class ReadOnlyCliTests(unittest.TestCase):
    def test_send_cli_signs_and_journals_activation_before_broadcast(self):
        sys.path.insert(0, str(ROOT / "scripts"))
        self.addCleanup(sys.path.remove, str(ROOT / "scripts"))
        cli_spec = importlib.util.spec_from_file_location("keeper_cli_send", ROOT / "scripts" / "zksys-membership-keeper.py")
        cli = importlib.util.module_from_spec(cli_spec)
        cli_spec.loader.exec_module(cli)
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            password = root / "password"
            password.write_text("test-only-password\n")
            subprocess.run(["cast", "wallet", "import", "test-only", "--keystore-dir", directory,
                            "--private-key", "0x" + "00" * 31 + "01", "--unsafe-password", "test-only-password"],
                           check=True, capture_output=True, text=True, timeout=60)
            signer = "0x7e5f4552091a69125d5dfcb7b8c2659029395bdf"
            cfg = {"bridge": A, "membership": B, "l1_chain_id": 57, "l2_chain_id": 58,
                   "l1_keystore": str(root / "test-only"), "l2_keystore": str(root / "test-only"),
                   "l1_password_file": str(password), "l2_password_file": str(password)}
            config_path = root / "config.json"
            config_path.write_text(json.dumps(cfg))
            config_path.chmod(0o600)
            sent = []
            def l2_rpc(method, params):
                if method == "eth_chainId": return "0x3a"
                if method == "eth_getTransactionCount": return "0x0"
                if method == "eth_sendRawTransaction":
                    saved = json.loads((root / "transactions.json").read_text())
                    self.assertEqual(saved["transactions"]["l2"]["state"], "signed")
                    self.assertEqual(saved["transactions"]["l2"]["raw"], params[0])
                    sent.append(params[0])
                    return subprocess.check_output(["cast", "keccak", params[0]], text=True).strip()
                raise AssertionError(method)
            l1 = RPC(lambda method, params: (_ for _ in ()).throw(AssertionError(method)))
            l2 = RPC(l2_rpc)
            class Hooks:
                def __init__(self, *args): pass
                def verify_deployment(self, config): return True
                def discover(self): return {"accounts": [B], "changes": []}
                def activation_ready(self, account): return True
                def activation_target(self, account): return 100
                def activation_request(self, account):
                    return {"chain_id": 58, "sender": signer, "to": C, "data": "0x12345678", "value": 0,
                            "gas": 100000, "gas_price": 10}
            args = ["keeper", "--state-dir", directory, "--bridge", A, "--membership", B,
                    "--l1-chain-id", "57", "--l2-chain-id", "58", "--send", "--config", str(config_path)]
            with patch.object(cli, "env_rpc", side_effect=lambda name: {"ZKSYS_CORE": object(), "ZKSYS_L1": l1, "ZKSYS_L2": l2}[name]), \
                 patch.object(hooks_module, "Hooks", Hooks), \
                 patch.object(hooks_module, "validate_config", side_effect=lambda config: config), \
                 patch.dict(os.environ, {"ZKSYS_L1_RPC_URL": "http://127.0.0.1:1", "ZKSYS_L2_RPC_URL": "http://127.0.0.1:1"}), \
                 patch.object(sys, "argv", args + ["--sync-only"]), contextlib.redirect_stdout(io.StringIO()) as output:
                self.assertEqual(cli.main(), 0)
                self.assertEqual(json.loads(output.getvalue())["result"], "sync_only_idle")
                self.assertEqual(sent, [])
                self.assertEqual(json.loads((root / "transactions.json").read_text())["transactions"], {})
                output.seek(0)
                output.truncate(0)
                with patch.object(sys, "argv", args):
                    self.assertEqual(cli.main(), 0)
            self.assertEqual(json.loads(output.getvalue())["result"], "activation_submitted")
            self.assertEqual(len(sent), 1)
            saved = json.loads((root / "transactions.json").read_text())
            self.assertEqual(saved["transactions"]["l2"]["state"], "submitted")

    def test_sync_only_requires_send(self):
        with tempfile.TemporaryDirectory() as directory:
            result = subprocess.run([
                sys.executable, str(ROOT / "scripts" / "zksys-membership-keeper.py"),
                "--state-dir", directory, "--bridge", A, "--membership", B,
                "--l1-chain-id", "57", "--l2-chain-id", "58", "--sync-only",
            ], capture_output=True, text=True, timeout=10)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("sync_only_requires_send", result.stderr)

    def test_exclusive_lock_rejects_a_second_keeper_before_rpc(self):
        with tempfile.TemporaryDirectory() as directory:
            lock = os.open(Path(directory) / "keeper.lock", os.O_CREAT | os.O_RDWR, 0o600)
            try:
                fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
                result = subprocess.run([
                    sys.executable, str(ROOT / "scripts" / "zksys-membership-keeper.py"),
                    "--state-dir", directory, "--bridge", A, "--membership", B,
                    "--l1-chain-id", "57", "--l2-chain-id", "58",
                ], capture_output=True, text=True, timeout=10)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("keeper_already_running", result.stderr)
                self.assertFalse((Path(directory) / "discovery.json").exists())
            finally:
                os.close(lock)

    def test_scans_persist_removed_and_changed_addresses_across_runs(self):
        sys.path.insert(0, str(ROOT / "scripts"))
        self.addCleanup(sys.path.remove, str(ROOT / "scripts"))
        cli_spec = importlib.util.spec_from_file_location("keeper_cli", ROOT / "scripts" / "zksys-membership-keeper.py")
        cli = importlib.util.module_from_spec(cli_spec)
        cli_spec.loader.exec_module(cli)
        l1 = RPC(lambda method, params: "0x39" if method == "eth_chainId" else "0x1234")
        l2 = RPC(lambda method, params: "0x3a" if method == "eth_chainId" else "0x1234")
        core = object()
        block1 = {"number": 1, "hash": "0x" + "aa" * 32}
        block2 = {"number": 2, "hash": "0x" + "bb" * 32}
        snapshots = [
            {"core_height": 1, "core_hash": "aa" * 32, "l1_block": block1, "l2_block": block2,
             "entries": [entry(A), entry(B, "02")], "l2_active": []},
            {"core_height": 2, "core_hash": "cc" * 32, "l1_block": block1, "l2_block": block2,
             "entries": [entry(C)], "l2_active": [B]},
        ]
        seen = []
        def reconcile(core_l1, core_l2, bridge, membership, accounts, *anchors):
            seen.append(list(accounts))
            return []
        def addr_call(rpc, target, signature, anchor):
            return B if signature == "l2Registry()" else A
        with tempfile.TemporaryDirectory() as directory, \
             patch.object(cli, "env_rpc", side_effect=lambda name: {"ZKSYS_CORE": core, "ZKSYS_L1": l1, "ZKSYS_L2": l2}[name]), \
             patch.object(cli, "addr_call", side_effect=addr_call), \
             patch.object(cli.k, "block", side_effect=lambda rpc, tag: block1 if rpc is l1 else block2), \
             patch.object(cli.k, "eth_call_word", return_value=(58).to_bytes(32, "big")), \
             patch.object(cli.k, "snapshot", side_effect=snapshots), \
             patch.object(cli.k, "reconcile", side_effect=reconcile), \
             patch.object(sys, "argv", ["keeper", "--state-dir", directory, "--bridge", A, "--membership", B,
                                       "--l1-chain-id", "57", "--l2-chain-id", "58"]), \
             contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(cli.main(), 0)
            self.assertEqual(cli.main(), 0)
            persisted = json.loads((Path(directory) / "discovery.json").read_text())
            self.assertEqual(set(persisted["ever_seen"]), {A, B, C})
        self.assertEqual([set(items) for items in seen], [{A, B}, {A, B, C}])
        self.assertTrue(all(method in ("eth_chainId", "eth_getCode") for method, _ in l1.calls + l2.calls))


class RunnerTests(unittest.TestCase):
    def test_sync_only_relays_then_resumes_same_journal_for_activation(self):
        journal = {'config_sha256': 'same-config', 'ever_seen': [], 'transactions': {}}
        changes = [{'account': A}]
        sends = []

        class Hooks:
            def verify_deployment(self, cfg): return True
            def discover(self): return {'accounts': [A], 'changes': list(changes)}
            def bridge_request(self, batch): return {}
            def activation_ready(self, account): return True
            def activation_target(self, account): return 100
            def activation_request(self, account): return {}

        class Driver:
            def begin(self, name, rpc, kind, metadata, **kwargs):
                sends.append((name, kind, metadata))
                journal['transactions'][name] = {'state': 'submitted', 'metadata': metadata}

        hooks, driver = Hooks(), Driver()
        runner = runner_module.KeeperRunner(journal, lambda _: None, driver, driver, None, None,
                                            hooks, {'max_batch_size': 512}, sync_only=True)
        self.assertEqual(runner.run_once(), 'bridge_submitted')
        self.assertEqual(sends, [('l1', 'bridge', {'accounts': [A]})])
        with patch.object(runner, '_resume_bridge', return_value='bridge_l1_pending'):
            self.assertEqual(runner.run_once(), 'bridge_l1_pending')
        del journal['transactions']['l1']
        changes.clear()
        self.assertEqual(runner.run_once(), 'sync_only_idle')
        self.assertEqual(len(sends), 1)
        self.assertEqual(journal['config_sha256'], 'same-config')

        normal_runner = runner_module.KeeperRunner(journal, lambda _: None, driver, driver, None, None,
                                                   hooks, {'max_batch_size': 512})
        self.assertEqual(normal_runner.run_once(), 'activation_submitted')
        self.assertEqual(sends[-1], ('l2', 'activation', {'account': A, 'target': 100}))
        self.assertEqual(journal['config_sha256'], 'same-config')

    def test_sync_only_rejects_journaled_activation_without_rpc_or_broadcast(self):
        class NoHooks:
            def verify_deployment(self, cfg): raise AssertionError('must stop before RPC')
            def discover(self): raise AssertionError('must stop before discovery')

        for state in ('signed', 'submitted', 'finalized', 'failed'):
            with self.subTest(state=state):
                journal = {'transactions': {'l2': {'kind': 'activation', 'state': state, 'raw': '0x1234'}}}
                before = copy.deepcopy(journal)
                runner = runner_module.KeeperRunner(journal, lambda _: self.fail('must not save'),
                                                    None, None, None, None, NoHooks(), {}, sync_only=True)
                with self.assertRaisesRegex(transactions.TransactionError, 'cannot cancel'):
                    runner.run_once()
                with self.assertRaisesRegex(transactions.TransactionError, 'cannot cancel'):
                    runner._resume_activation()
                self.assertEqual(journal, before)

    def test_batch_submits_up_to_512_addresses(self):
        accounts = [f'0x{value:040x}' for value in range(1, 514)]
        batches = []

        class Hooks:
            def verify_deployment(self, cfg): return True
            def discover(self): return {'accounts': accounts, 'changes': [{'account': a} for a in accounts]}
            def bridge_request(self, batch):
                batches.append(batch)
                return {}

        class Driver:
            def begin(self, name, rpc, kind, metadata, **kwargs):
                self.metadata = metadata

        driver = Driver()
        runner = runner_module.KeeperRunner({}, lambda state: None, driver, None, None, None,
                                            Hooks(), {'max_batch_size': 512})
        self.assertEqual(runner.run_once(), 'bridge_submitted')
        self.assertEqual(batches, [accounts[:512]])
        self.assertEqual(driver.metadata['accounts'], accounts[:512])

    def test_sync_finishes_before_permissionless_activation(self):
        journal = {}
        saved = []
        changes = [{"account": A}]
        bridge_receipt = {"ok": True}

        class Hooks:
            def verify_deployment(self, cfg): return True
            def discover(self): return {"accounts": [A], "changes": list(changes)}
            def bridge_request(self, batch): return {}
            def activation_ready(self, account): return True
            def activation_target(self, account): return 100
            def activation_request(self, account): return {}
            def verify_delivery(self, receipt, entry, updates, height, observed):
                return updates == [(A, 8, 100)]
            def verify_activation(self, receipt, entry): return True
            def hash_bytes(self, value): return "0x" + "33" * 32

        class Driver:
            def __init__(self): self.final = False; self.delivered = False; self.sends = []; self.retries = []
            def begin(self, name, rpc, kind, metadata, **kwargs):
                self.sends.append((name, metadata))
                journal["transactions"][name] = {"state": "submitted", "metadata": metadata}
            def receipt(self, name, rpc): return bridge_receipt if self.final else None
            def rebroadcast_if_missing(self, name, rpc): self.retries.append(name); return False
            def confirm_bridge_delivery(self, name, rpc, chain, digest, verify):
                if not self.delivered: return None
                self.assertion = verify({}, journal["transactions"][name])
                if not self.assertion: raise AssertionError("delivery mismatch")
                return {}
            def complete(self, name): del journal["transactions"][name]

        l1, l2 = Driver(), Driver()
        cfg = {"max_batch_size": 20, "bridge": A, "bridge_event_topic": "0x" + "11" * 32,
               "diamond": C, "priority_event_topic": "0x" + "22" * 32, "l1_alias": C,
               "membership": B, "l2_chain_id": 58}
        runner = runner_module.KeeperRunner(journal, lambda state: saved.append(copy.deepcopy(state)),
                                            l1, l2, None, None, Hooks(), cfg)
        self.assertEqual(runner.run_once(), "bridge_submitted")
        self.assertEqual(saved[0]["ever_seen"], [A])
        self.assertEqual(runner.run_once(), "bridge_l1_pending")
        self.assertEqual(l1.retries, ['l1'])
        self.assertEqual(l2.sends, [])
        l1.final = True
        with patch.object(runner_module, "bridge_priority_hash", return_value="0x" + "33" * 32), \
             patch.object(runner_module, "priority_payload", return_value={"from": C, "to": B, "transactionType": 127, "input": "0x01"}), \
             patch.object(runner_module.membership, "decode_updates", return_value=([(A, 8, 100)], 10, 1000)):
            self.assertEqual(runner.run_once(), "bridge_l2_pending")
            self.assertEqual(l2.sends, [])
            l1.delivered = True
            self.assertEqual(runner.run_once(), "bridge_finalized")
        changes.clear()
        self.assertEqual(runner.run_once(), "activation_submitted")
        self.assertEqual(l2.sends, [("l2", {"account": A, "target": 100})])
        self.assertEqual(runner.run_once(), "activation_pending")
        self.assertEqual(l2.retries, ['l2'])
        l2.final = True
        self.assertEqual(runner.run_once(), "activation_finalized")


class HookTests(unittest.TestCase):
    def test_configuration_rejects_unbounded_or_malformed_values(self):
        positive = ("l1_chain_id", "l2_chain_id", "max_batch_size", "l1_gas_price", "l1_gas_limit",
                    "priority_gas_limit", "gas_per_pubdata", "max_priority_fee", "max_l1_transaction_cost",
                    "l2_gas_price", "l2_gas_limit", "max_l2_transaction_cost")
        config = {key: 1 for key in positive}
        config["l2_chain_id"] = 2
        for field in ("bridge", "bridgehub", "diamond", "membership", "weights", "issuer", "l1_alias",
                      "l1_signer", "l2_signer", "refund"):
            config[field] = A
        for field in ("l1_genesis", "l2_genesis"):
            config[field] = "0x" + "12" * 32
        for field in ("l1_keystore", "l2_keystore", "l1_password_file", "l2_password_file"):
            config[field] = "/tmp/test-only"
        config["code_checks"] = []
        signatures = ('facetAddress(bytes4)',
                      'bridgehubRequestL2Transaction((address,address,uint256,uint256,bytes,uint256,uint256,bytes[],address))',
                      'l2TransactionBaseCost(uint256,uint256,uint256)')
        config['diamond_facets'] = [{'selector': '0x' + keeper.selector(sig).hex(),
                                     'address': A, 'code_hash': '0x' + '12' * 32} for sig in signatures]
        self.assertIs(hooks_module.validate_config(config), config)
        without_batch_size = {key: value for key, value in config.items() if key != 'max_batch_size'}
        self.assertEqual(hooks_module.validate_config(without_batch_size)['max_batch_size'], 512)
        for field, invalid in (("max_batch_size", 513), ("max_l1_transaction_cost", 0),
                               ("l1_gas_limit", True), ("l2_chain_id", 1),
                               ("l1_keystore", "relative")):
            candidate = {**config, field: invalid}
            with self.subTest(field=field), self.assertRaises((transactions.TransactionError, keeper.KeeperError)):
                hooks_module.validate_config(candidate)

    def test_discovery_persists_union_even_if_reconciliation_fails(self):
        journal = {"ever_seen": [A]}
        saved = []
        config = {"membership": B, "bridge": C}
        hook = hooks_module.Hooks(None, None, None, journal, config, lambda value: saved.append(copy.deepcopy(value)))
        snap = {"entries": [entry(B)], "l2_active": [], "l2_block": {"number": 1, "hash": "0x" + "aa" * 32}}
        with patch.object(hooks_module.k, "snapshot", return_value=snap), \
             patch.object(hooks_module.k, "block", return_value=snap["l2_block"]), \
             patch.object(hooks_module.k, "reconcile", side_effect=keeper.KeeperError("rpc failed")):
            with self.assertRaises(keeper.KeeperError):
                hook.discover()
        self.assertEqual(set(saved[-1]["ever_seen"]), {A, B})

    def test_bridge_quote_enforces_fee_budget_and_activation_encodes_account(self):
        config = {"l1_gas_price": 10, "l1_gas_limit": 100000, "l2_chain_id": 58,
                  "priority_gas_limit": 1000000, "gas_per_pubdata": 800, "bridgehub": A,
                  "max_priority_fee": 100, "max_l1_transaction_cost": 1000100,
                  "l1_chain_id": 57, "l1_signer": A, "bridge": B, "refund": C,
                  "l2_gas_price": 3, "l2_gas_limit": 100000, "max_l2_transaction_cost": 300000,
                  "l2_signer": A, "weights": C}
        hook = hooks_module.Hooks(None, None, None, {}, config, lambda state: None)
        with patch.object(hooks_module.k, "block", return_value={"hash": "0x" + "aa" * 32}), \
             patch.object(hooks_module, "_call", return_value=(100).to_bytes(32, "big")):
            request = hook.bridge_request([A, B])
            self.assertEqual(request["value"], 100)
            self.assertEqual(request["data"], keeper.encode_push([A, B], 1000000, 800, C))
            config["max_priority_fee"] = 99
            with self.assertRaises(transactions.TransactionError):
                hook.bridge_request([A, B])
        activation = hook.activation_request(B)
        expected = subprocess.check_output(["cast", "calldata", "activatePendingWeightFor(address)", B], text=True).strip()
        self.assertEqual(activation["data"], expected)
        config["max_l2_transaction_cost"] -= 1
        with self.assertRaises(transactions.TransactionError):
            hook.activation_request(B)

    def test_concrete_discovery_and_request_hooks_drive_runner(self):
        config = {"l1_gas_price": 10, "l1_gas_limit": 100000, "l2_chain_id": 58,
                  "priority_gas_limit": 1000000, "gas_per_pubdata": 800, "bridgehub": A,
                  "max_priority_fee": 100, "max_l1_transaction_cost": 1000100,
                  "l1_chain_id": 57, "l1_signer": A, "bridge": B, "refund": C,
                  "l2_gas_price": 3, "l2_gas_limit": 100000, "max_l2_transaction_cost": 300000,
                  "l2_signer": A, "weights": C, "membership": A, "issuer": B, "max_batch_size": 10}
        anchor = {"number": 20, "hash": "0x" + "aa" * 32}
        journal = {"ever_seen": [A], "transactions": {}}
        snapshot = {"entries": [entry(B)], "l2_active": [A], "l2_block": anchor}
        l1_rpc = RPC(lambda method, params: None)
        l2_rpc = RPC(lambda method, params: None)
        hook = hooks_module.Hooks(None, l1_rpc, l2_rpc, journal, config, lambda state: None)
        sends = []
        class Driver:
            def begin(self, name, rpc, kind, metadata, **request):
                sends.append((name, kind, metadata, request))
                journal["transactions"][name] = {"state": "submitted", "metadata": metadata}
        driver = Driver()
        runner = runner_module.KeeperRunner(journal, lambda _: None, driver, driver,
                                            l1_rpc.call, l2_rpc.call, hook, config)
        def reconcile(l1, l2, bridge, membership, accounts, l1_block, l2_block):
            self.assertEqual(set(accounts), {A, B})
            return [{"account": A}, {"account": B}]
        with patch.object(hook, "verify_deployment", return_value=True), \
             patch.object(hooks_module.k, "snapshot", return_value=snapshot), \
             patch.object(hooks_module.k, "block", return_value=anchor), \
             patch.object(hooks_module.k, "reconcile", side_effect=reconcile), \
             patch.object(hooks_module, "_call", return_value=(100).to_bytes(32, "big")):
            self.assertEqual(runner.run_once(), "bridge_submitted")
        self.assertEqual(sends[0][0:3], ("l1", "bridge", {"accounts": [A, B]}))
        self.assertEqual(sends[0][3]["data"], keeper.encode_push([A, B], 1000000, 800, C))


if __name__ == "__main__":
    unittest.main()
