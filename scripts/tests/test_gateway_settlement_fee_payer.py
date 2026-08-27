from __future__ import annotations

import fcntl
import json
import os
import subprocess
import tempfile
import textwrap
import unittest
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[2]
HELPER = (
    REPO_ROOT
    / "scripts"
    / "gateway-launch"
    / "provision-edge-settlement-fee-payer.sh"
)
LOCK_HELPER = (
    REPO_ROOT / "scripts" / "gateway-launch" / "_execute_operator_lock.sh"
)
MIGRATION = (
    REPO_ROOT / "scripts" / "gateway-launch" / "edge-chain-migrate-to-gateway.sh"
)
GENERATOR = (
    REPO_ROOT / "scripts" / "gateway-launch" / "generate-os-server-configs.sh"
)
TRACKER = "0x0000000000000000000000000000000000010010"
WRAPPED_TOKEN = "0x0000000000000000000000000000000000002000"
OPERATOR = "0x19E7E376E7C213B7E7e7e46cc70a5dD086DAff2A"
EDGE_PROXY = "0x0000000000000000000000000000000000004000"
GATEWAY_TARGET = "0xd0ec30807902886b61a86d9bd209fe353c1d912b"
GATEWAY_RELAY = "0x758b06cda80bdd016f79afd0df1a984039067a21"
CREATE2_FACTORY = "0x4e59b44847b379578588920ca78fbf26c0b4956c"
GATEWAY_BLOCK_ZERO = "0x" + "dd" * 32
PRIVATE_KEY = "0x" + "11" * 32
PRIVATE_KEY_PUBLIC = (
    "0x4f355bdcb7cc0af728ef3cceb9615d90684bb5b2ca5f859ab0f0b704075871aa"
    "385b6b1b8ead809ca67454d9683fcf2ba03456d6fe2c4abe2b07f0fbdbb2f1c1"
)
OTHER_PRIVATE_KEY_PUBLIC = (
    "0x466d7fcae563e5cb09a0d1870bb580344804617879a14949cf22285f1bae3f27"
    "6728176c3c6431f8eeda4538dc37c865e2784f3a9e77d044f33e407797e1278a"
)
UINT256_MAX = 2**256 - 1


class GatewaySettlementFeePayerTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp_dir = tempfile.TemporaryDirectory()
        root = Path(self.temp_dir.name)
        self.gateway_dir = root / "gateway"
        self.bin_dir = root / "bin"
        self.home_dir = root / "home"
        self.state_path = root / "cast-state.json"
        self.bin_dir.mkdir()
        self.home_dir.mkdir()

        gateway_chain = self.gateway_dir / "chains" / "gateway"
        edge_chain = self.gateway_dir / "chains" / "edge-a"
        (gateway_chain / "configs").mkdir(parents=True)
        (edge_chain / "configs").mkdir(parents=True)
        (self.gateway_dir / "ZkStack.yaml").write_text(
            "prover_version: Gpu\n"
            "l1_network: localhost\n",
            encoding="utf-8",
        )
        (gateway_chain / "ZkStack.yaml").write_text(
            "name: gateway\n"
            "chain_id: 57001\n"
            "prover_version: Gpu\n"
            "l1_batch_commit_data_generator_mode: Rollup\n"
            "vm_option: ZKSyncOsVM\n"
            "evm_emulator: false\n"
            "legacy_bridge: false\n"
            "l1_network: localhost\n"
            "base_token:\n"
            "  address: '0x0000000000000000000000000000000000000001'\n"
            "  nominator: 1\n"
            "  denominator: 1\n",
            encoding="utf-8",
        )
        (gateway_chain / "configs" / "gateway.yaml").write_text(
            f"validator_timelock_addr: '{GATEWAY_TARGET}'\n"
            f"relayed_sl_da_validator: '{GATEWAY_RELAY}'\n",
            encoding="utf-8",
        )
        stamp_dir = self.gateway_dir / ".gateway-launch"
        stamp_dir.mkdir()
        stamp = stamp_dir / "gateway-runtime-genesis.v1"
        stamp.write_text(f"57001 {GATEWAY_BLOCK_ZERO}\n", encoding="utf-8")
        stamp.chmod(0o600)
        (edge_chain / "ZkStack.yaml").write_text(
            "chain_id: 57002\n", encoding="utf-8"
        )
        wallet_path = edge_chain / "configs" / "wallets.yaml"
        wallet_path.write_text(
            "execute_operator:\n"
            f"  address: '{OPERATOR}'\n"
            f"  private_key: '{PRIVATE_KEY}'\n",
            encoding="utf-8",
        )
        wallet_path.chmod(0o600)

        self._write_state(
            {
                "fee": 15 * 10**18,
                "wrapped_token": WRAPPED_TOKEN,
                "wrapped_balance": 10 * 10**18,
                "native_balance": 100 * 10**18,
                "allowance": 0,
                "agreement": False,
                "fee_reads": 0,
                "fee_after_first_read": None,
                "latest_nonce": 0,
                "pending_nonce": 0,
                "gas_price": 1000,
                "gas_estimates": {
                    "deposit()": 100_000,
                    "approve(address,uint256)": 80_000,
                    "setSettlementFeePayerAgreement(uint256,bool)": 90_000,
                },
                "calls": [],
            }
        )
        self._write_fake_cast()
        self._write_fake_expect()
        self._write_fake_yaml()

        self.env = os.environ.copy()
        self.env.pop("GATEWAY_EXECUTE_OPERATOR_LOCK_INHERIT_FD", None)
        self.env.update(
            {
                "PATH": f"{self.bin_dir}:{self.env['PATH']}",
                "HOME": str(self.home_dir),
                "GATEWAY_DIR": str(self.gateway_dir),
                "GATEWAY_RPC_URL": "http://gateway.invalid",
                "GATEWAY_WRAPPED_BASE_TOKEN_ADDRESS": WRAPPED_TOKEN,
                "L1_NETWORK": "localhost",
                "TEST_CAST_STATE": str(self.state_path),
                "TEST_EXPECTED_ADDRESS": OPERATOR,
                "PYTHONPATH": f"{self.bin_dir}:{self.env.get('PYTHONPATH', '')}",
            }
        )

    def tearDown(self) -> None:
        self.temp_dir.cleanup()

    def _write_state(self, state: dict[str, object]) -> None:
        self.state_path.write_text(json.dumps(state), encoding="utf-8")

    def _state(self) -> dict[str, object]:
        return json.loads(self.state_path.read_text(encoding="utf-8"))

    def _write_fake_cast(self) -> None:
        fake = self.bin_dir / "cast"
        fake.write_text(
            textwrap.dedent(
                f"""\
                #!/usr/bin/env python3
                import json
                import os
                import sys

                path = os.environ["TEST_CAST_STATE"]
                with open(path, encoding="utf-8") as handle:
                    state = json.load(handle)
                args = sys.argv[1:]
                state["calls"].append(args)

                if args[0] == "wallet" and args[1] == "address":
                    output = os.environ["TEST_EXPECTED_ADDRESS"]
                elif args[0] == "chain-id":
                    output = "57001"
                elif args[0] == "nonce":
                    block = args[args.index("--block") + 1]
                    output = str(state[f"{{block}}_nonce"])
                elif args[0] == "gas-price":
                    output = str(state["gas_price"])
                elif args[0] == "estimate":
                    output = str(state["gas_estimates"][args[2]])
                elif args[0] == "code":
                    address = args[1].lower()
                    if address == "{GATEWAY_TARGET}":
                        output = "0x" + "aa" * 2840
                    elif address == "{GATEWAY_RELAY}":
                        output = "0xbb"
                    elif address == "{CREATE2_FACTORY}":
                        output = "0xcc"
                    else:
                        output = "0x6000"
                elif args[0] == "keccak":
                    if args[1] == "0x" + "aa" * 2840:
                        output = "0xed00d115b16594117ebb53b6d0322ada70270ee75e2b7e8eed5e33967c3fb777"
                    elif args[1] == "0xbb":
                        output = "0x4c86ffe57098cb09a48ee6dfa4f21b2cce8e327409e1da1dc6be4545220b89e0"
                    elif args[1] == "0xcc":
                        output = "0x2fa86add0aed31f33a762c9d88e807c475bd51d0f52bd0955754b2608f7e4989"
                    elif args[1] == "{PRIVATE_KEY_PUBLIC}":
                        output = "0x" + "00" * 12 + "{OPERATOR[2:].lower()}"
                    elif args[1] == "{OTHER_PRIVATE_KEY_PUBLIC}":
                        output = "0x" + "00" * 12 + "1563915e194d8cfba1943570603f7606a3115508"
                    else:
                        raise SystemExit(f"unexpected cast keccak: {{args}}")
                elif args[0] == "block":
                    output = "{GATEWAY_BLOCK_ZERO}"
                elif args[0] == "balance":
                    output = str(state["native_balance"])
                elif args[0] == "call":
                    signature = args[2]
                    if signature in {{
                        "WETH_TOKEN()(address)",
                        "wrappedZKToken()(address)",
                    }}:
                        output = state["wrapped_token"]
                    elif signature == "getZKChain(uint256)(address)":
                        output = "{EDGE_PROXY}"
                    elif signature == "gatewaySettlementFee()(uint256)":
                        output = str(
                            state["fee_after_first_read"]
                            if state["fee_reads"] and state["fee_after_first_read"] is not None
                            else state["fee"]
                        )
                        state["fee_reads"] += 1
                    elif signature == "balanceOf(address)(uint256)":
                        output = str(state["wrapped_balance"])
                    elif signature == "allowance(address,address)(uint256)":
                        output = str(state["allowance"])
                    elif signature == "settlementFeePayerAgreement(address,uint256)(bool)":
                        output = "true" if state["agreement"] else "false"
                    else:
                        raise SystemExit(f"unexpected cast call: {{args}}")
                elif args[0] == "send":
                    signature = args[2]
                    if signature == "deposit()":
                        value = int(args[args.index("--value") + 1])
                        state["wrapped_balance"] += value
                        state["native_balance"] -= value
                    elif signature == "approve(address,uint256)":
                        if args[3].lower() != "{TRACKER}" or int(args[4]) != {UINT256_MAX}:
                            raise SystemExit(f"bad approval: {{args}}")
                        state["allowance"] = {UINT256_MAX}
                    elif signature == "setSettlementFeePayerAgreement(uint256,bool)":
                        if args[3] != "57002" or args[4] != "true":
                            raise SystemExit(f"bad agreement: {{args}}")
                        state["agreement"] = True
                    else:
                        raise SystemExit(f"unexpected cast send: {{args}}")
                    gas_limit = int(args[args.index("--gas-limit") + 1])
                    gas_price = int(args[args.index("--gas-price") + 1])
                    state["native_balance"] -= gas_limit * gas_price
                    state["latest_nonce"] += 1
                    state["pending_nonce"] = state["latest_nonce"]
                    output = "transactionHash 0x01"
                else:
                    raise SystemExit(f"unexpected cast invocation: {{args}}")

                with open(path, "w", encoding="utf-8") as handle:
                    json.dump(state, handle)
                print(output)
                """
            ),
            encoding="utf-8",
        )
        fake.chmod(0o755)

    def _write_fake_expect(self) -> None:
        fake = self.bin_dir / "expect"
        fake.write_text(
            textwrap.dedent(
                """\
                #!/usr/bin/env bash
                set -euo pipefail
                cat >/dev/null
                : >"${KEYSTORE_DIR}/${ACCOUNT_NAME}"
                chmod 600 "${KEYSTORE_DIR}/${ACCOUNT_NAME}"
                """
            ),
            encoding="utf-8",
        )
        fake.chmod(0o755)

    def _write_fake_yaml(self) -> None:
        # The launch host provides PyYAML. Keep this focused behavior test
        # hermetic on developer machines whose system Python does not.
        (self.bin_dir / "yaml.py").write_text(
            textwrap.dedent(
                """\
                def safe_load(text):
                    lines = [line for line in text.splitlines() if line.strip()]
                    if any(line.startswith("execute_operator:") for line in lines):
                        result = {"execute_operator": {}}
                        for line in lines:
                            stripped = line.strip()
                            if stripped.startswith("address:"):
                                result["execute_operator"]["address"] = stripped.split(":", 1)[1].strip().strip("'\\\"")
                            elif stripped.startswith("private_key:"):
                                result["execute_operator"]["private_key"] = stripped.split(":", 1)[1].strip().strip("'\\\"")
                        return result
                    if any(
                        line.startswith("l1_sender:")
                        or line.startswith("gateway_sender:")
                        for line in lines
                    ):
                        result = {}
                        current = None
                        for line in lines:
                            stripped = line.strip()
                            if line.startswith("l1_sender:"):
                                current = "l1_sender"
                                result[current] = {}
                            elif line.startswith("gateway_sender:"):
                                current = "gateway_sender"
                                result[current] = {}
                            elif current and stripped.startswith("operator_execute_sk:"):
                                result[current]["operator_execute_sk"] = stripped.split(":", 1)[1].strip().strip("'\\\"")
                        return result
                    result = {}
                    current = None
                    for line in lines:
                        stripped = line.strip()
                        key, _, value = line.partition(":")
                        if key in {"validator_timelock_addr", "relayed_sl_da_validator"}:
                            result[key] = value.strip().strip("'\\\"")
                            continue
                        if not line.startswith((" ", "\\t")):
                            current = key
                            raw = value.strip().strip("'\\\"")
                            if key == "base_token":
                                result[key] = {}
                            elif raw.lower() in {"true", "false"}:
                                result[key] = raw.lower() == "true"
                            elif raw.isdecimal():
                                result[key] = int(raw, 10)
                            elif raw:
                                result[key] = raw
                        elif current == "base_token":
                            nested_key, _, nested_value = stripped.partition(":")
                            raw = nested_value.strip().strip("'\\\"")
                            result[current][nested_key] = (
                                int(raw, 10) if raw.isdecimal() else raw
                            )
                    if result:
                        return result
                    return None
                """
            ),
            encoding="utf-8",
        )

    def _lock_path(self) -> Path:
        return (
            self.gateway_dir
            / ".gateway-launch-locks"
            / f"gateway-57001-execute-operator-{OPERATOR[2:].lower()}.lock"
        )

    def _run(
        self, edge_name: str = "edge-a", **env_overrides: str
    ) -> subprocess.CompletedProcess[str]:
        env = self.env.copy()
        env.update(env_overrides)
        return subprocess.run(
            ["bash", str(HELPER), edge_name],
            check=False,
            capture_output=True,
            text=True,
            env=env,
        )

    @staticmethod
    def _bounded_gas_cost(state: dict[str, object], signatures: list[str]) -> int:
        max_fee_per_gas = int(state["gas_price"]) * 2
        estimates = state["gas_estimates"]
        return max_fee_per_gas * sum(
            (int(estimates[signature]) * 5 + 3) // 4 for signature in signatures
        )

    def test_provisions_live_fee_target_then_is_idempotent(self) -> None:
        first = self._run()
        self.assertEqual(first.returncode, 0, first.stderr)
        first_state = self._state()
        sends = [call for call in first_state["calls"] if call[0] == "send"]
        self.assertEqual(len(sends), 3, sends)

        deposit = next(call for call in sends if call[2] == "deposit()")
        self.assertEqual(deposit[1].lower(), WRAPPED_TOKEN.lower())
        self.assertEqual(int(deposit[deposit.index("--value") + 1]), 65 * 10**18)
        self.assertTrue(any(call[2] == "approve(address,uint256)" for call in sends))
        self.assertTrue(
            any(
                call[2] == "setSettlementFeePayerAgreement(uint256,bool)"
                for call in sends
            )
        )
        self.assertEqual(first_state["wrapped_balance"], 75 * 10**18)
        self.assertEqual(first_state["allowance"], UINT256_MAX)
        self.assertTrue(first_state["agreement"])
        estimates = [call for call in first_state["calls"] if call[0] == "estimate"]
        self.assertEqual(
            [call[2] for call in estimates],
            [
                "deposit()",
                "approve(address,uint256)",
                "setSettlementFeePayerAgreement(uint256,bool)",
            ],
        )
        for estimate, send in zip(estimates, sends, strict=True):
            self.assertEqual(
                send[send.index("--gas-limit") + 1],
                str((int(first_state["gas_estimates"][estimate[2]]) * 5 + 3) // 4),
            )
            self.assertEqual(
                estimate[estimate.index("--gas-price") + 1],
                send[send.index("--gas-price") + 1],
            )
            self.assertEqual(
                estimate[estimate.index("--from") + 1].lower(), OPERATOR.lower()
            )
        self.assertTrue(self._lock_path().is_file())
        self.assertFalse(
            (self.gateway_dir / ".gateway-launch-locks" / "edge-a-execute-operator.lock").exists()
        )

        second = self._run()
        self.assertEqual(second.returncode, 0, second.stderr)
        second_sends = [
            call for call in self._state()["calls"] if call[0] == "send"
        ]
        self.assertEqual(second_sends, sends)
        self.assertNotIn(PRIVATE_KEY, first.stdout + first.stderr + second.stdout + second.stderr)

        # The ephemeral keystore paths present in signer argv are gone at exit.
        for call in sends:
            keystore = Path(call[call.index("--keystore") + 1])
            self.assertFalse(keystore.exists())

    def test_rejects_zero_live_fee_without_broadcasting(self) -> None:
        state = self._state()
        state["fee"] = 0
        self._write_state(state)
        result = self._run()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("gatewaySettlementFee must be non-zero", result.stderr)
        self.assertFalse(any(call[0] == "send" for call in self._state()["calls"]))

    def test_requires_an_independently_trusted_wrapped_token_pin(self) -> None:
        self.env.pop("GATEWAY_WRAPPED_BASE_TOKEN_ADDRESS")
        result = self._run()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("GATEWAY_WRAPPED_BASE_TOKEN_ADDRESS", result.stderr)
        self.assertFalse(any(call[0] == "send" for call in self._state()["calls"]))

    def test_rejects_rpc_selected_wrapped_token_before_signing(self) -> None:
        state = self._state()
        state["wrapped_token"] = "0xdead00000000000000000000000000000000beef"
        self._write_state(state)
        result = self._run()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("wrapped base-token pin mismatch", result.stderr)
        self.assertFalse(any(call[0] == "send" for call in self._state()["calls"]))

    def test_rejects_malformed_or_zero_wrapped_token_pins(self) -> None:
        for pin in ("not-an-address", "0x" + "00" * 20):
            with self.subTest(pin=pin):
                state = self._state()
                state["calls"] = []
                self._write_state(state)
                result = self._run(GATEWAY_WRAPPED_BASE_TOKEN_ADDRESS=pin)
                self.assertNotEqual(result.returncode, 0)
                self.assertFalse(
                    any(call[0] == "send" for call in self._state()["calls"])
                )

    def test_rejects_target_above_cap_without_broadcasting(self) -> None:
        result = self._run(
            GATEWAY_INTEROP_SETTLEMENT_MAX_WRAP_WEI=str(74 * 10**18)
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("exceeds configured wrap cap", result.stderr)
        self.assertFalse(any(call[0] == "send" for call in self._state()["calls"]))

    def test_rejects_zero_native_gas_reserve_without_broadcasting(self) -> None:
        result = self._run(GATEWAY_INTEROP_SETTLEMENT_NATIVE_GAS_RESERVE_WEI="0")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("native gas reserve must be non-zero", result.stderr)
        self.assertFalse(any(call[0] == "send" for call in self._state()["calls"]))

    def test_fails_closed_if_live_fee_increases_during_provisioning(self) -> None:
        state = self._state()
        state["fee_after_first_read"] = 20 * 10**18
        self._write_state(state)
        result = self._run()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("wrapped Gateway base-token balance verification failed", result.stderr)

    def test_full_native_requirement_fails_before_any_broadcast(self) -> None:
        state = self._state()
        signatures = list(state["gas_estimates"])
        deficit = 65 * 10**18
        reserve = 10 * 10**18
        state["native_balance"] = (
            deficit + reserve + self._bounded_gas_cost(state, signatures) - 1
        )
        self._write_state(state)
        result = self._run()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("before broadcasting deposit approval agreement", result.stderr)
        final_state = self._state()
        self.assertFalse(any(call[0] == "send" for call in final_state["calls"]))
        self.assertEqual(final_state["wrapped_balance"], 10 * 10**18)
        self.assertEqual(final_state["allowance"], 0)
        self.assertFalse(final_state["agreement"])

    def test_exact_native_requirement_retains_the_reserve(self) -> None:
        state = self._state()
        signatures = list(state["gas_estimates"])
        reserve = 10 * 10**18
        state["native_balance"] = (
            65 * 10**18 + reserve + self._bounded_gas_cost(state, signatures)
        )
        self._write_state(state)
        result = self._run()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self._state()["native_balance"], reserve)

    def test_every_pending_send_combination_is_preflighted(self) -> None:
        signatures = [
            "deposit()",
            "approve(address,uint256)",
            "setSettlementFeePayerAgreement(uint256,bool)",
        ]
        for mask in range(1, 8):
            with self.subTest(mask=mask):
                state = self._state()
                state.update(
                    {
                        "wrapped_balance": 10 * 10**18 if mask & 1 else 75 * 10**18,
                        "allowance": 0 if mask & 2 else UINT256_MAX,
                        "agreement": not bool(mask & 4),
                        "fee_reads": 0,
                        "calls": [],
                    }
                )
                planned = [
                    signature
                    for bit, signature in enumerate(signatures)
                    if mask & (1 << bit)
                ]
                deficit = 65 * 10**18 if mask & 1 else 0
                reserve = 10 * 10**18
                state["native_balance"] = (
                    deficit + reserve + self._bounded_gas_cost(state, planned) - 1
                )
                self._write_state(state)

                result = self._run()
                self.assertNotEqual(result.returncode, 0)
                final_state = self._state()
                self.assertEqual(
                    [call[2] for call in final_state["calls"] if call[0] == "estimate"],
                    planned,
                )
                self.assertFalse(
                    any(call[0] == "send" for call in final_state["calls"])
                )

    def test_invalid_gas_inputs_fail_before_broadcast(self) -> None:
        for field, value, expected in (
            ("gas_price", UINT256_MAX, "scaled transaction gas bound overflows"),
            ("gas_estimates", {"deposit()": "invalid"}, "invalid wrapped-token deposit gas estimate"),
        ):
            with self.subTest(field=field):
                state = self._state()
                state["calls"] = []
                state["gas_price"] = 1000
                state["gas_estimates"] = {
                    "deposit()": 100_000,
                    "approve(address,uint256)": 80_000,
                    "setSettlementFeePayerAgreement(uint256,bool)": 90_000,
                }
                if field == "gas_estimates":
                    state[field] = {**state[field], **value}
                else:
                    state[field] = value
                self._write_state(state)
                result = self._run()
                self.assertNotEqual(result.returncode, 0)
                self.assertIn(expected, result.stderr)
                self.assertFalse(
                    any(call[0] == "send" for call in self._state()["calls"])
                )

    def test_check_only_never_estimates_or_broadcasts(self) -> None:
        result = subprocess.run(
            ["bash", str(HELPER), "--check-only", "edge-a"],
            check=False,
            capture_output=True,
            text=True,
            env=self.env,
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("wrapped balance is below", result.stderr)
        self.assertFalse(
            any(
                call[0] in {"gas-price", "estimate", "send"}
                for call in self._state()["calls"]
            )
        )

    def test_pending_operator_transaction_blocks_all_provisioning(self) -> None:
        for already_ready in (False, True):
            with self.subTest(already_ready=already_ready):
                state = self._state()
                state["calls"] = []
                if already_ready:
                    state.update(
                        {
                            "wrapped_balance": 75 * 10**18,
                            "allowance": UINT256_MAX,
                            "agreement": True,
                        }
                    )
                state["pending_nonce"] = state["latest_nonce"] + 1
                self._write_state(state)
                result = self._run()
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("has pending Gateway transactions", result.stderr)
                self.assertFalse(
                    any(
                        call[0] in {"gas-price", "estimate", "send"}
                        for call in self._state()["calls"]
                    )
                )

    def test_rejects_a_concurrent_execute_operator_user(self) -> None:
        lock_root = self.gateway_dir / ".gateway-launch-locks"
        lock_root.mkdir()
        lock_path = self._lock_path()
        with lock_path.open("w", encoding="utf-8") as lock:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            result = self._run()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("execute_operator is in use", result.stderr)
        self.assertFalse(any(call[0] == "send" for call in self._state()["calls"]))

    def test_same_gateway_operator_serializes_different_edge_names(self) -> None:
        edge_b = self.gateway_dir / "chains" / "edge-b"
        (edge_b / "configs").mkdir(parents=True)
        (edge_b / "ZkStack.yaml").write_text("chain_id: 57003\n", encoding="utf-8")
        wallet_path = edge_b / "configs" / "wallets.yaml"
        wallet_path.write_text(
            "execute_operator:\n"
            f"  address: '{OPERATOR.upper().replace('0X', '0x')}'\n"
            f"  private_key: '{PRIVATE_KEY}'\n",
            encoding="utf-8",
        )
        wallet_path.chmod(0o600)

        lock_root = self.gateway_dir / ".gateway-launch-locks"
        lock_root.mkdir()
        with self._lock_path().open("w", encoding="utf-8") as lock:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            result = self._run("edge-b")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("execute_operator is in use", result.stderr)
        self.assertFalse(any(call[0] == "send" for call in self._state()["calls"]))

    def test_migration_inherited_lock_is_reused_without_deadlock(self) -> None:
        lock_root = self.gateway_dir / ".gateway-launch-locks"
        lock_root.mkdir()
        with self._lock_path().open("w", encoding="utf-8") as lock:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)

            def inherit_as_fd9() -> None:
                os.dup2(lock.fileno(), 9)

            env = self.env.copy()
            env["GATEWAY_EXECUTE_OPERATOR_LOCK_INHERIT_FD"] = "9"
            result = subprocess.run(
                ["bash", str(HELPER), "edge-a"],
                check=False,
                capture_output=True,
                text=True,
                env=env,
                pass_fds=(lock.fileno(),),
                preexec_fn=inherit_as_fd9,
            )
            blocked = self._run()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertNotEqual(blocked.returncode, 0)
        self.assertIn("execute_operator is in use", blocked.stderr)
        released = self._run()
        self.assertEqual(released.returncode, 0, released.stderr)

    def test_node_lifetime_lock_survives_exec(self) -> None:
        env = self.env.copy()
        env["LOCK_HELPER"] = str(LOCK_HELPER)
        env["GATEWAY_CHAIN_NAME"] = "gateway"
        holder = subprocess.Popen(
            [
                "bash",
                "-c",
                'set -euo pipefail; source "$LOCK_HELPER"; '
                'gateway_acquire_execute_operator_lock edge-a; '
                'printf "ready\\n"; exec sleep 30',
            ],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            env=env,
        )
        try:
            self.assertEqual(holder.stdout.readline().strip(), "ready")
            result = self._run()
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("execute_operator is in use", result.stderr)
        finally:
            holder.terminate()
            holder.wait(timeout=5)
            if holder.stdout is not None:
                holder.stdout.close()
            if holder.stderr is not None:
                holder.stderr.close()

    def test_migration_and_generated_node_use_the_shared_identity_lock(self) -> None:
        migration = MIGRATION.read_text(encoding="utf-8")
        generator = GENERATOR.read_text(encoding="utf-8")
        acquire = 'gateway_acquire_execute_operator_lock "${EDGE_CHAIN_NAME}"'
        self.assertIn(acquire, migration)
        self.assertLess(
            migration.index(acquire),
            migration.index('gateway_chain_id="$(get_chain_id_from_zkstack_yaml'),
        )
        self.assertLess(
            migration.index(acquire),
            migration.index("zkstack chain pause-deposits"),
        )
        self.assertIn(
            'GATEWAY_EXECUTE_OPERATOR_LOCK_INHERIT_FD="${GATEWAY_EXECUTE_OPERATOR_LOCK_FD}"',
            migration,
        )
        self.assertIn("_execute_operator_lock.sh", generator)
        self.assertIn(
            'gateway_acquire_execute_operator_lock "{chain_name}" "{config_path}"',
            generator,
        )
        self.assertNotIn("{chain_name}-execute-operator.lock", generator)

    def test_rejects_wallet_or_generated_signer_address_mismatch(self) -> None:
        wallet_path = (
            self.gateway_dir / "chains" / "edge-a" / "configs" / "wallets.yaml"
        )
        original = wallet_path.read_text(encoding="utf-8")
        wallet_path.write_text(
            original.replace(OPERATOR, "0x000000000000000000000000000000000000bEEF"),
            encoding="utf-8",
        )
        mismatch = self._run()
        self.assertNotEqual(mismatch.returncode, 0)
        self.assertIn("address/private-key mismatch", mismatch.stderr)
        self.assertFalse(any(call[0] == "send" for call in self._state()["calls"]))

        wallet_path.write_text(original, encoding="utf-8")
        generated = self.gateway_dir / "generated-edge-config.yaml"
        generated.write_text(
            "l1_sender:\n  operator_execute_sk: '" + PRIVATE_KEY + "'\n"
            "gateway_sender:\n  operator_execute_sk: '0x" + "22" * 32 + "'\n",
            encoding="utf-8",
        )
        command = (
            'source "$LOCK_HELPER"; '
            'gateway_execute_operator_lock_key edge-a "$GENERATED_CONFIG"'
        )
        result = subprocess.run(
            ["bash", "-c", command],
            check=False,
            capture_output=True,
            text=True,
            env={
                **self.env,
                "LOCK_HELPER": str(LOCK_HELPER),
                "GENERATED_CONFIG": str(generated),
                "GATEWAY_CHAIN_NAME": "gateway",
            },
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("generated execute-operator signer mismatch", result.stderr)

    def test_rejects_uint256_multiplication_overflow(self) -> None:
        state = self._state()
        state["fee"] = 2**255
        self._write_state(state)
        result = self._run(
            GATEWAY_INTEROP_SETTLEMENT_OPERATION_BUDGET="3",
            GATEWAY_INTEROP_SETTLEMENT_MAX_WRAP_WEI=str(UINT256_MAX),
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("overflows uint256", result.stderr)
        self.assertFalse(any(call[0] == "send" for call in self._state()["calls"]))

    def test_private_key_never_uses_argv_or_environment_signing(self) -> None:
        source = HELPER.read_text(encoding="utf-8")
        self.assertNotIn("--private-key", source)
        self.assertIn("log_user 0", source)
        self.assertIn("--interactive", source)
        self.assertIn("--keystore", source)
        self.assertIn("--password-file", source)
        self.assertIn("-u CAST_ASYNC", source)
        self.assertNotIn("PRIVATE_KEY=", source)


if __name__ == "__main__":
    unittest.main()
