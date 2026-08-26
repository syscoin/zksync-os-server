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
        (gateway_chain / "ZkStack.yaml").write_text(
            "chain_id: 57001\n", encoding="utf-8"
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
                "wrapped_balance": 10 * 10**18,
                "native_balance": 100 * 10**18,
                "allowance": 0,
                "agreement": False,
                "fee_reads": 0,
                "fee_after_first_read": None,
                "send_gas_cost": 10**18,
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
                    if signature == "wrappedZKToken()(address)":
                        output = "{WRAPPED_TOKEN}"
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
                    state["native_balance"] -= state["send_gas_cost"]
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
                    for line in lines:
                        if line.startswith("chain_id:"):
                            return {"chain_id": int(line.split(":", 1)[1].strip(), 0)}
                    result = {}
                    for line in lines:
                        key, _, value = line.partition(":")
                        if key in {"validator_timelock_addr", "relayed_sl_da_validator"}:
                            result[key] = value.strip().strip("'\\\"")
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

    def test_provisions_live_fee_target_then_is_idempotent(self) -> None:
        first = self._run()
        self.assertEqual(first.returncode, 0, first.stderr)
        first_state = self._state()
        sends = [call for call in first_state["calls"] if call[0] == "send"]
        self.assertEqual(len(sends), 3, sends)

        deposit = next(call for call in sends if call[2] == "deposit()")
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

    def test_fails_if_transactions_consume_the_native_reserve(self) -> None:
        state = self._state()
        state["native_balance"] = 77 * 10**18
        self._write_state(state)
        result = self._run()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("native gas reserve verification failed", result.stderr)

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
        self.assertLess(migration.index(acquire), migration.index("pause-deposits --chain"))
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
        self.assertNotIn("PRIVATE_KEY=", source)


if __name__ == "__main__":
    unittest.main()
