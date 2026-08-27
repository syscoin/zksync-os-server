from __future__ import annotations

import hashlib
import importlib.util
import json
import os
import subprocess
import tempfile
import unittest
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[2]
OTHER_TARGET = "0x1111111111111111111111111111111111111111"
PUBLISHED_PATCH_TARGET = "0xd0ec30807902886b61a86d9bd209fe353c1d912b"
PUBLISHED_PATCH_TARGET_RUNTIME_SIZE = 2840
PUBLISHED_PATCH_TARGET_RUNTIME_HASH = (
    "0xed00d115b16594117ebb53b6d0322ada70270ee75e2b7e8eed5e33967c3fb777"
)
PUBLISHED_EDGE_RELAY = "0x758b06cda80bdd016f79afd0df1a984039067a21"
PUBLISHED_EDGE_RELAY_RUNTIME_HASH = (
    "0x4c86ffe57098cb09a48ee6dfa4f21b2cce8e327409e1da1dc6be4545220b89e0"
)
PUBLISHED_GAS_TANK = "0xb49943ea232624dd4aa63e18186076c6c99a68ef"
PUBLISHED_GAS_TANK_INIT_CODE_HASH = (
    "0x1fce42acba699bc198d2e146b0284e3bdd821d1634cd809f1c0a12e961dac561"
)
PUBLISHED_GAS_TANK_RUNTIME_HASH = (
    "0x041faf31b2f3576502f25fd5d106eaf411611e42dc996c28872abe487cb6e269"
)
PUBLISHED_EDGE_SOURCE_SHA256 = (
    "99a0ae0dfc013ce7beacc60df0a487b35fd2af1fdcb04103ba438353cbd2a3bd"
)
PUBLISHED_GAS_TANK_SOURCE_SHA256 = (
    "7ba8d21c59b244c090be3cda6e01581d652a79c930ff0a488172e1212b74f188"
)
PUBLISHED_ZKSYNC_OS_PATCHED_TREE = "9fb99cf591c553447cd3839489cc4d327eb424b4"
PUBLISHED_ERA_PATCHED_TREE = "ea1c0600ebbcafbada4e0080aa0178311084f86a"
PENDING_V8_MOCK_ZKSTACK_SHA = "d1f681c395a5b40fd4cfa591dea8ac3d3f80ebdc"
PENDING_V8_MOCK_CONTRACTS_SHA = "8fb7c29a4e3174335c6480b23f57822e054f9d5f"
PUBLISHED_ERA_GENESIS_ROOT = (
    "0xec4a6d11ed43e56364b38684633718eea0c3c270849ccef03dfcf2721a2b77fb"
)
PUBLISHED_ERA_GENESIS_SHA256 = (
    "5adf0dd1b618911d51c335e983c0c71cc1c74fc7db37161bf76a4b51e5055a95"
)
OFFICIAL_OS_URL = "https://github.com/matter-labs/zksync-os"
FINAL_OS_TAG = "v0.4.0"
OTHER_OS_TAG = "v0.2.10-interface-v0.1.3-2026-02-10"
FINAL_LOCKED_REV = "3" * 40
FINAL_PATCHED_REV = "4" * 40


def rust_address_bytes(address: str) -> str:
    value = address.removeprefix("0x")
    return ", ".join(f"0x{value[index:index + 2]}" for index in range(0, 40, 2))


class LauncherStaticTests(unittest.TestCase):
    def test_zkstack_generated_signers_never_enter_forge_argv(self) -> None:
        patch_path = (
            REPO_ROOT / "scripts" / "patches" / "zksync-era-syscoin.patch"
        )
        patch = patch_path.read_text(encoding="utf-8")
        applicator = (
            REPO_ROOT / "scripts" / "apply-zksync-era-syscoin-patch.sh"
        ).read_text(encoding="utf-8")
        added = "\n".join(
            line[1:]
            for line in patch.splitlines()
            if line.startswith("+") and not line.startswith("+++")
        )

        # SYSCOIN: Keep the upstream delta surgical while testing its complete
        # signer boundary here in the server repository.
        for expected in (
            "// SYSCOIN: Generated zkstack signers must never enter Forge argv",
            "struct EphemeralForgeSigner",
            "private_key: Option<H256>",
            "LocalWallet::encrypt_keystore(",
            'cmd.env("ETH_KEYSTORE", &self.keystore_path)',
            '.env("ETH_PASSWORD", &self.password_path)',
            "fs::Permissions::from_mode(0o700)",
            "fs::Permissions::from_mode(0o600)",
            "self.private_key = Some(private_key);",
            "self.args.reject_raw_secret_args()?;",
            "const RAW_SECRET_ARGS: [&str; 6]",
            "fn argument_uses_flag(argument: &str, flag: &str) -> bool",
            '"--mnemonic-passphrases"',
            '"--mnenomic-passphrases"',
            "// SYSCOIN: Forge resume is deliberately disabled for governance acceptance",
            "SyscoinOwnable2StepQuery",
            "fn owner_acceptance_required(",
            "async fn current_admin(",
            '"target exposes neither getAdmin() nor admin()',
            "# SYSCOIN: Materialize generated Forge signers as private ephemeral keystores.",
            "# SYSCOIN: Keep generated signer material out of Forge argv and command logs.",
            'tempfile = "3.14.0"',
            "tempfile.workspace = true",
            "L1Network::Tanenbaum | L1Network::Mainnet => {",
            "let min_validator_balance = match chain_config.l1_network",
            "_ => U256::from(10).pow(19.into()),",
        ):
            self.assertIn(expected, added)
        self.assertEqual(added.count("cmd = signer.apply(cmd);"), 2)
        self.assertNotIn("PrivateKey {", added)
        self.assertNotIn('to_string = "private-key=', added)
        signer_creation = patch.index("+        let ephemeral_signer = self")
        self.assertLess(
            signer_creation,
            patch.index("         if self.args.resume", signer_creation),
        )

        self.assertEqual(
            hashlib.sha256(patch_path.read_bytes()).hexdigest(),
            "b3cdaa066399085369141a01cfd0345fb4aa8e17c055aac7ae0a76b127eec718",
        )
        for expected in (
            'EXPECTED_PATCH_SHA256="b3cdaa066399085369141a01cfd0345fb4aa8e17c055aac7ae0a76b127eec718"',
            'EXPECTED_PATCH_PATH_COUNT="14"',
            'EXPECTED_PATCH_PATHS_SHA256="3e068f5438c569c17e98ccbce686b72f8fd90737c8d01b37e456ce8df3d6f170"',
            'EXPECTED_PATCHED_TREE="4f72053786df7257568e70d889bcc915356e2dc2"',
        ):
            self.assertIn(expected, applicator)

    def test_gateway_common_binds_default_foundry_profile_for_children(self) -> None:
        common = REPO_ROOT / "scripts" / "gateway-launch" / "_common.sh"
        result = subprocess.run(
            [
                "bash",
                "-c",
                'source "$COMMON"; printf "%s|" "$FOUNDRY_PROFILE"; '
                "bash -c 'printf %s \"$FOUNDRY_PROFILE\"'",
            ],
            check=False,
            capture_output=True,
            text=True,
            env={
                **os.environ,
                "COMMON": str(common),
                "FOUNDRY_PROFILE": "anvil-interop",
            },
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, "default|default")

    def test_l1_probe_accepts_zkstack_huge_decimal_bytecode_scalars(self) -> None:
        if importlib.util.find_spec("yaml") is None:
            self.skipTest("PyYAML is not installed in this test environment")
        common = REPO_ROOT / "scripts" / "gateway-launch" / "_common.sh"
        addresses = {
            "bridgehub": "0x" + "11" * 20,
            "ctm": "0x" + "22" * 20,
            "supplier": "0x" + "33" * 20,
            "genesis": "0x" + "44" * 20,
            "verifier": "0x" + "55" * 20,
            "router": "0x" + "66" * 20,
            "handler": "0x" + "77" * 20,
            "tracker": "0x" + "88" * 20,
        }
        with tempfile.TemporaryDirectory() as temporary_dir:
            root = Path(temporary_dir)
            contracts = root / "configs" / "contracts.yaml"
            contracts.parent.mkdir(parents=True)
            contracts.write_text(
                "core_ecosystem_contracts:\n"
                f"  bridgehub_proxy_addr: {int(addresses['bridgehub'], 16)}\n"
                "zksync_os_ctm:\n"
                f"  state_transition_proxy_addr: {int(addresses['ctm'], 16)}\n"
                f"  l1_bytecodes_supplier_addr: {int(addresses['supplier'], 16)}\n"
                f"  genesis_upgrade_addr: {int(addresses['genesis'], 16)}\n"
                f"  verifier_addr: {int(addresses['verifier'], 16)}\n"
                f"  diamond_cut_data: {'9' * 11562}\n"
                f"  force_deployments_data: {'8' * 10097}\n",
                encoding="utf-8",
            )
            bin_dir = root / "bin"
            bin_dir.mkdir()
            fake_cast = bin_dir / "cast"
            fake_cast.write_text(
                "#!/usr/bin/env bash\n"
                "set -euo pipefail\n"
                "if [ \"${1:-}\" = code ]; then printf '%s\\n' 0x6000; exit 0; fi\n"
                "[ \"${1:-}\" = call ] || exit 2\n"
                "case \"${3:-}\" in\n"
                "  'chainTypeManagerIsRegistered(address)(bool)'|'isZKsyncOS()(bool)') printf '%s\\n' true ;;\n"
                "  'BRIDGE_HUB()(address)') printf '%s\\n' \"${TEST_BRIDGEHUB:?}\" ;;\n"
                "  'getSemverProtocolVersion()(uint32,uint32,uint32)') printf '%s\\n' '0 32 0' ;;\n"
                "  'l1GenesisUpgrade()(address)') printf '%s\\n' \"${TEST_GENESIS:?}\" ;;\n"
                "  'L1_BYTECODES_SUPPLIER()(address)') printf '%s\\n' \"${TEST_SUPPLIER:?}\" ;;\n"
                "  'protocolVersionVerifier(uint256)(address)') printf '%s\\n' \"${TEST_VERIFIER:?}\" ;;\n"
                "  'storedBatchZero()(bytes32)'|'initialCutHash()(bytes32)') printf '0x%064d\\n' 1 ;;\n"
                "  'ctmAssetIdFromAddress(address)(bytes32)') printf '0x%064d\\n' 2 ;;\n"
                "  'ctmAssetIdToAddress(bytes32)(address)') printf '%s\\n' \"${TEST_CTM:?}\" ;;\n"
                "  'assetRouter()(address)') printf '%s\\n' \"${TEST_ROUTER:?}\" ;;\n"
                "  'chainAssetHandler()(address)') printf '%s\\n' \"${TEST_HANDLER:?}\" ;;\n"
                "  'l1CtmDeployer()(address)') printf '%s\\n' \"${TEST_TRACKER:?}\" ;;\n"
                "  'assetHandlerAddress(bytes32)(address)') printf '%s\\n' \"${TEST_HANDLER:?}\" ;;\n"
                "  'assetDeploymentTracker(bytes32)(address)') printf '%s\\n' \"${TEST_TRACKER:?}\" ;;\n"
                "  *) exit 3 ;;\n"
                "esac\n",
                encoding="utf-8",
            )
            fake_cast.chmod(0o755)
            result = subprocess.run(
                [
                    "bash",
                    "-c",
                    'source "$COMMON"; gl_probe_l1_ecosystem_deployed_ready',
                ],
                check=False,
                capture_output=True,
                text=True,
                env={
                    **os.environ,
                    "COMMON": str(common),
                    "GATEWAY_DIR": str(root),
                    "L1_RPC_URL": "http://127.0.0.1:1",
                    "PATH": f"{bin_dir}:{os.environ['PATH']}",
                    **{f"TEST_{key.upper()}": value for key, value in addresses.items()},
                },
            )
            self.assertEqual(result.returncode, 0, result.stderr)

    def test_zkstack_nightly_detection_works_with_mawk(self) -> None:
        common = REPO_ROOT / "scripts" / "gateway-launch" / "_common.sh"
        with tempfile.TemporaryDirectory() as temporary_dir:
            bin_dir = Path(temporary_dir)
            rustup = bin_dir / "rustup"
            rustup.write_text(
                "#!/usr/bin/env bash\n"
                "printf '%s\\n' \\\n"
                "  nightly-2026-01-22-x86_64-unknown-linux-gnu \\\n"
                "  'nightly-2026-02-10-x86_64-unknown-linux-gnu (active, default)' \\\n"
                "  stable-x86_64-unknown-linux-gnu\n",
                encoding="utf-8",
            )
            rustup.chmod(0o755)
            result = subprocess.run(
                ["bash", "-c", 'source "$COMMON"; gl_detect_gateway_zkstack_nightly'],
                check=False,
                capture_output=True,
                text=True,
                env={
                    **os.environ,
                    "COMMON": str(common),
                    "PATH": f"{bin_dir}:{os.environ['PATH']}",
                },
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(
                result.stdout.strip(),
                "nightly-2026-02-10-x86_64-unknown-linux-gnu",
            )

    def test_private_zkstack_wrapper_protects_failure_artifacts_and_restores_umask(
        self,
    ) -> None:
        common = REPO_ROOT / "scripts" / "gateway-launch" / "_common.sh"
        with tempfile.TemporaryDirectory() as temporary_dir:
            output = Path(temporary_dir) / "wallets.yaml"
            result = subprocess.run(
                [
                    "bash",
                    "-c",
                    r'''
source "$COMMON"
umask 022
before="$(umask)"
gl_zkstack_pty() {
  printf '%s\n' secret >"$OUTPUT"
  return 73
}
set +e
gl_zkstack_private_pty zkstack ignored
rc=$?
set -e
after="$(umask)"
printf '%s|%s|%s\n' "$rc" "$before" "$after"
''',
                ],
                check=False,
                capture_output=True,
                text=True,
                env={
                    **os.environ,
                    "COMMON": str(common),
                    "OUTPUT": str(output),
                },
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            status, before, after = result.stdout.strip().split("|")
            self.assertEqual(status, "73")
            self.assertEqual(after, before)
            self.assertEqual(output.stat().st_mode & 0o777, 0o600)

    def test_ecosystem_path_is_resolved_before_wallet_hardening(self) -> None:
        helper = (
            REPO_ROOT
            / "scripts"
            / "gateway-launch"
            / "gateway-ecosystem-create.sh"
        ).read_text(encoding="utf-8")
        planned_resolution = helper.index("gl_resolve_gateway_dir planned")
        creation = helper.index("gl_zkstack_private_pty env")
        resolution = helper.index("gl_resolve_gateway_dir\n", creation)
        hardening = helper.index("gl_secure_generated_wallet_file", resolution)
        binding = helper.index("gl_bind_gateway_launch_context", hardening)
        persistence = helper.index("gl_persist_wallet_file", hardening)
        self.assertLess(planned_resolution, creation)
        self.assertLess(creation, resolution)
        self.assertLess(resolution, hardening)
        self.assertLess(hardening, binding)
        self.assertLess(hardening, persistence)
        self.assertIn("GIT_CONFIG_COUNT=1", helper)
        self.assertIn("GIT_CONFIG_KEY_0=submodule.contracts.update", helper)
        self.assertIn("GIT_CONFIG_VALUE_0=none", helper)
        self.assertNotIn("--update-submodules", helper)
        self.assertNotIn("gl_checkout_contracts_sha", helper)
        self.assertGreater(
            helper.index("gl_ensure_era_contracts_syscoin_postimage", creation), creation
        )

        edge_create = (
            REPO_ROOT / "scripts" / "gateway-launch" / "edge-chain-create-init.sh"
        ).read_text(encoding="utf-8")
        gateway_init = (
            REPO_ROOT / "scripts" / "gateway-launch" / "gateway-chain-init.sh"
        ).read_text(encoding="utf-8")
        self.assertIn("gl_zkstack_private_pty zkstack chain create", edge_create)
        self.assertIn("gl_zkstack_private_pty zkstack chain init", edge_create)
        self.assertIn("gl_zkstack_private_pty zkstack chain init", gateway_init)
        self.assertNotIn("--update-submodules", edge_create)
        self.assertNotIn("--update-submodules", gateway_init)

        for launcher_name in (
            "run-gateway-launch.sh",
            "gateway-launch-repair.sh",
        ):
            launcher = (
                REPO_ROOT / "scripts" / "gateway-launch" / launcher_name
            ).read_text(encoding="utf-8")
            self.assertIn(
                '"${SCRIPT_DIR}/gateway-ecosystem-create.sh" || return $?',
                launcher,
            )
            self.assertIn(
                'export FOUNDRY_OFFLINE="${FOUNDRY_OFFLINE:-true}"', launcher
            )
            self.assertLess(
                launcher.index("gl_resolve_gateway_dir planned"),
                launcher.index("gl_checkpoint_state_init"),
            )

        common = REPO_ROOT / "scripts" / "gateway-launch" / "_common.sh"
        with tempfile.TemporaryDirectory() as temporary_dir:
            env = os.environ.copy()
            env.pop("ZKSYNC_OS_SERVER_PATH", None)
            env.pop("GATEWAY_ECOSYSTEM_NAME", None)
            env.pop("GATEWAY_ECOSYSTEM_PARENT_DIR", None)
            env.update(
                {
                    "COMMON": str(common),
                    "ROOT": temporary_dir,
                }
            )
            result = subprocess.run(
                [
                    "bash",
                    "-c",
                    r'''
source "$COMMON"
export GATEWAY_DIR="$ROOT/gateway-v32-test"
gl_resolve_gateway_dir planned >/dev/null
printf "%s\n" "$GATEWAY_DIR"
printf "%s\n" "$GATEWAY_ECOSYSTEM_NAME"
''',
                ],
                check=False,
                capture_output=True,
                text=True,
                env=env,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(
                result.stdout.splitlines(),
                [
                    str(Path(temporary_dir) / "gateway_v32_test"),
                    "gateway-v32-test",
                ],
            )

    def test_checkpoint_fingerprint_binds_deployment_identity(self) -> None:
        common = REPO_ROOT / "scripts" / "gateway-launch" / "_common.sh"
        with tempfile.TemporaryDirectory() as temporary_dir:
            base_env = {
                "PATH": os.environ["PATH"],
                "COMMON": str(common),
                "REQUIRED_ZKSTACK_CLI_SHA": "1" * 40,
                "REQUIRED_CONTRACTS_SHA": "2" * 40,
                "L1_CHAIN_ID": "5700",
                "L1_NETWORK": "tanenbaum",
                "L1_RPC_URL": "http://127.0.0.1:8545",
                "GATEWAY_DIR": str(Path(temporary_dir) / "gateway_v32_test"),
                "GATEWAY_ECOSYSTEM_NAME": "gateway-v32-test",
                "GATEWAY_CHAIN_NAME": "gateway",
                "EDGE_CHAIN_NAME": "zksys",
                "PROVER_MODE": "no-proofs",
                "GATEWAY_PROVER_MODE": "no-proofs",
                "SYSCOIN_ZKSYNC_OS_MOCK_VERIFIER": "true",
                "FOUNDRY_EVM_VERSION": "cancun",
                "GATEWAY_CREATE2_FACTORY_SALT": "0x" + "99" * 32,
                "ZKSYS_L2_TOKEN_ADMIN_ADDRESS": "0x" + "11" * 20,
            }

            def fingerprint(**overrides: str) -> dict:
                result = subprocess.run(
                    ["bash", "-c", 'source "$COMMON"; gl_checkpoint_fingerprint_json'],
                    check=False,
                    capture_output=True,
                    text=True,
                    env={**base_env, **overrides},
                )
                self.assertEqual(result.returncode, 0, result.stderr)
                return json.loads(result.stdout)

            baseline = fingerprint()
            self.assertEqual(baseline, fingerprint(PROTOCOL_VERSION="v32.0"))
            self.assertEqual(baseline["gateway_chain_id"], "57001")
            self.assertEqual(baseline["edge_chain_id"], "57057")
            self.assertEqual(baseline["edge_prover_mode"], "no-proofs")
            self.assertEqual(baseline["gateway_commit_mode"], "rollup")
            self.assertEqual(baseline["zksync_os_mock_verifier"], "true")
            self.assertEqual(
                baseline["gateway_settlement_fee"], "15000000000000000000"
            )
            self.assertEqual(baseline["edge_reuse_gateway_governor"], "true")
            self.assertEqual(
                baseline["gateway_l2_da_commitment_scheme_value"], "4"
            )
            self.assertEqual(
                baseline["edge_gateway_committer_wallet_name"], "blob_operator"
            )
            self.assertEqual(
                baseline,
                fingerprint(
                    GATEWAY_CHAIN_ID="057001",
                    EDGE_CHAIN_ID="057057",
                    EDGE_PROVER_MODE="no-proofs",
                    GATEWAY_COMMIT_MODE="rollup",
                    SYSCOIN_ZKSYNC_OS_MOCK_VERIFIER="TRUE",
                    ZKSYS_DEPLOY_L1_REGISTRY_BRIDGE="TRUE",
                    L1_WETH_TOKEN_ADDRESS="0xa66b2E50c2b805F31712beA422D0D9e7D0Fd0F35",
                    ZKSYS_L1_REGISTRY_BRIDGE_SENIORITY_HEIGHT1="0210240",
                    EDGE_REUSE_GATEWAY_GOVERNOR="TRUE",
                    GATEWAY_L2_DA_COMMITMENT_SCHEME_VALUE="04",
                    EDGE_GATEWAY_COMMITTER_WALLET_NAME="blob_operator",
                    GATEWAY_SETTLEMENT_FEE=hex(15 * 10**18),
                ),
            )
            self.assertEqual(
                baseline,
                fingerprint(
                    GATEWAY_INTEROP_FEE_USD="0.30",
                    NATIVE_TOKEN_PRICE_USD="0.02",
                ),
            )
            self.assertEqual(
                baseline,
                fingerprint(GATEWAY_ECOSYSTEM_NAME="gateway_v32_test"),
            )

            variations = (
                ("GATEWAY_CHAIN_ID", "57002", "gateway_chain_id"),
                ("EDGE_CHAIN_ID", "57900001", "edge_chain_id"),
                ("GATEWAY_COMMIT_MODE", "validium", "gateway_commit_mode"),
                (
                    "GATEWAY_SETTLEMENT_FEE",
                    str(15 * 10**18 + 1),
                    "gateway_settlement_fee",
                ),
                (
                    "EDGE_REUSE_GATEWAY_GOVERNOR",
                    "false",
                    "edge_reuse_gateway_governor",
                ),
            )
            for variable, value, field in variations:
                with self.subTest(variable=variable):
                    changed = fingerprint(**{variable: value})
                    self.assertEqual(changed[field], value)
                    self.assertNotEqual(changed, baseline)

            bridge_drift = fingerprint(
                ZKSYS_L1_REGISTRY_BRIDGE_SENIORITY_HEIGHT1="210241"
            )
            self.assertEqual(
                bridge_drift["zksys_l1_registry_bridge"]["seniority_height1"],
                "210241",
            )
            self.assertNotEqual(bridge_drift, baseline)

            short_salt = fingerprint(ZKSYS_L2_REGISTRY_IMPL_SALT="1")
            padded_salt = fingerprint(
                ZKSYS_L2_REGISTRY_IMPL_SALT="0x" + "0" * 63 + "1"
            )
            self.assertEqual(short_salt, padded_salt)
            self.assertEqual(
                fingerprint(GATEWAY_CREATE2_FACTORY_SALT="1"),
                fingerprint(
                    GATEWAY_CREATE2_FACTORY_SALT="0x" + "0" * 63 + "1"
                ),
            )

            bridge_disabled = fingerprint(ZKSYS_DEPLOY_L1_REGISTRY_BRIDGE="false")
            self.assertIn("zksys_l2_deployment", bridge_disabled)
            self.assertNotIn(
                "registry_impl_salt", bridge_disabled["zksys_l2_deployment"]
            )

            mainnet = fingerprint(
                L1_CHAIN_ID="57",
                L1_NETWORK="mainnet",
                PROVER_MODE="gpu",
                GATEWAY_PROVER_MODE="gpu",
                EDGE_PROVER_MODE="gpu",
                SYSCOIN_ZKSYNC_OS_MOCK_VERIFIER="false",
            )
            spaced_name = fingerprint(
                L1_CHAIN_ID="57",
                L1_NETWORK="mainnet",
                PROVER_MODE="gpu",
                GATEWAY_PROVER_MODE="gpu",
                EDGE_PROVER_MODE="gpu",
                SYSCOIN_ZKSYNC_OS_MOCK_VERIFIER="false",
                ZKSYS_L2_TOKEN_NAME=" ZKSYS ",
            )
            self.assertEqual(
                spaced_name["zksys_l2_deployment"]["token_name"], " ZKSYS "
            )
            self.assertNotEqual(spaced_name, mainnet)

            for invalid in (
                {"EDGE_REUSE_GATEWAY_GOVERNOR": "truthy"},
                {"GATEWAY_CHAIN_ID": "0"},
                {"GATEWAY_CHAIN_ID": str(1 << 32)},
                {"EDGE_CHAIN_ID": "0"},
                {"EDGE_CHAIN_ID": str(1 << 32)},
                {"GATEWAY_L2_DA_COMMITMENT_SCHEME_VALUE": "256"},
                {"GATEWAY_L2_DA_COMMITMENT_SCHEME_VALUE": "0"},
                {"GATEWAY_L2_DA_COMMITMENT_SCHEME_VALUE": "5"},
                {"GATEWAY_L2_DA_COMMITMENT_SCHEME": "Calldata"},
                {"EDGE_GATEWAY_COMMITTER_WALLET_NAME": "execute_operator"},
                {"GATEWAY_PROVER_MODE": "typo"},
                {"EDGE_PROVER_MODE": "typo"},
                {"GATEWAY_COMMIT_MODE": "typo"},
                {"SYSCOIN_ZKSYNC_OS_MOCK_VERIFIER": "false"},
                {"PROVER_MODE": "gpu"},
                {"GATEWAY_PROVER_MODE": "gpu"},
                {"EDGE_PROVER_MODE": "gpu"},
                {"L1_CHAIN_ID": "57", "L1_NETWORK": "mainnet"},
                {
                    "PROVER_MODE": "gpu",
                    "GATEWAY_PROVER_MODE": "gpu",
                    "EDGE_PROVER_MODE": "gpu",
                },
                {"GATEWAY_SETTLEMENT_FEE": "-1"},
                {"GATEWAY_SETTLEMENT_FEE": str(1 << 256)},
                {
                    "GATEWAY_INTEROP_FEE_USD": str(1 << 256),
                    "NATIVE_TOKEN_PRICE_USD": "1",
                },
                {"NATIVE_TOKEN_PRICE_USD": "0"},
                {"GATEWAY_INTEROP_FEE_TOKEN_DECIMALS": "-1"},
                {"GATEWAY_INTEROP_FEE_TOKEN_DECIMALS": "256"},
                {"L1_WETH_TOKEN_ADDRESS": "0x" + "00" * 20},
                {"GATEWAY_CREATE2_FACTORY_SALT": ""},
                {"GATEWAY_CREATE2_FACTORY_SALT": "   "},
                {"ZKSYS_L2_TOKEN_ADMIN_ADDRESS": ""},
                {"ZKSYS_L2_TOKEN_ADMIN_ADDRESS": "0x" + "00" * 20},
                {"ZKSYS_L2_CREATE2_DEPLOYER": "0x" + "00" * 20},
                {"ZKSYS_L2_CREATE2_DEPLOYER": "0x" + "12" * 20},
                {"ZKSYS_L2_TOKEN_DECIMALS": "60"},
                {
                    "ZKSYS_L1_REGISTRY_BRIDGE_PROXY_ADMIN_OWNER_ADDRESS": "0x"
                    + "00" * 20
                },
                {"ZKSYS_L1_REGISTRY_BRIDGE_SENIORITY_HEIGHT1": "0"},
                {"ZKSYS_L1_REGISTRY_BRIDGE_NEVM_START_BLOCK": "0"},
                {"ZKSYS_L1_REGISTRY_BRIDGE_SENIORITY_HEIGHT2": "210240"},
                {"ZKSYS_L1_REGISTRY_BRIDGE_SENIORITY_LEVEL1_BPS": "10001"},
                {"ZKSYS_L1_REGISTRY_BRIDGE_SENIORITY_LEVEL2_BPS": "10001"},
                {
                    "ZKSYS_L1_REGISTRY_BRIDGE_SENIORITY_LEVEL1_BPS": "1",
                    "ZKSYS_L1_REGISTRY_BRIDGE_SENIORITY_LEVEL2_BPS": "0",
                },
                {"USE_DUMMY_MESSAGE_ROOT": "true"},
                {"ZKSYS_ZK_TOKEN_ASSET_ID": "0x" + "77" * 32},
                {"ZK_TOKEN_ASSET_ID": "0x" + "88" * 32},
            ):
                with self.subTest(invalid=invalid):
                    result = subprocess.run(
                        [
                            "bash",
                            "-c",
                            'source "$COMMON"; gl_checkpoint_fingerprint_json',
                        ],
                        check=False,
                        capture_output=True,
                        text=True,
                        env={**base_env, **invalid},
                    )
                    self.assertNotEqual(result.returncode, 0)

            mismatch = subprocess.run(
                [
                    "bash",
                    "-c",
                    r'''
source "$COMMON"
gl_checkpoint_state_init
gl_checkpoint_set_fingerprint_if_empty
export EDGE_CHAIN_ID=57900001
gl_checkpoint_assert_fingerprint_matches
''',
                ],
                check=False,
                capture_output=True,
                text=True,
                env=base_env,
            )
            self.assertNotEqual(mismatch.returncode, 0)
            self.assertIn("edge_chain_id", mismatch.stderr)

            salt_mismatch = subprocess.run(
                [
                    "bash",
                    "-c",
                    'source "$COMMON"; export GATEWAY_CREATE2_FACTORY_SALT=1; '
                    "gl_checkpoint_assert_fingerprint_matches",
                ],
                check=False,
                capture_output=True,
                text=True,
                env=base_env,
            )
            self.assertNotEqual(salt_mismatch.returncode, 0)
            self.assertIn("gateway_create2_factory_salt", salt_mismatch.stderr)
            gateway_dir = Path(base_env["GATEWAY_DIR"])
            state_key = hashlib.sha256(
                os.path.realpath(gateway_dir).encode("utf-8")
            ).hexdigest()
            state = json.loads(
                (
                    gateway_dir.parent
                    / ".gateway-launch-state"
                    / state_key
                    / "state.json"
                ).read_text(encoding="utf-8")
            )
            self.assertEqual(
                state["fingerprint"]["gateway_create2_factory_salt"],
                "0x" + "99" * 32,
            )

    def test_launch_lock_ignores_unmarked_wrapper_fd8_then_reuses_its_own(self) -> None:
        common = REPO_ROOT / "scripts" / "gateway-launch" / "_common.sh"
        with tempfile.TemporaryDirectory() as temporary_dir:
            result = subprocess.run(
                [
                    "bash",
                    "-c",
                    'exec 8</dev/null; source "$COMMON"; '
                    "gl_acquire_gateway_launch_lock; "
                    "gl_acquire_gateway_launch_lock; "
                    'test -n "$GATEWAY_LAUNCH_LOCK_FD8_KEY"',
                ],
                check=False,
                capture_output=True,
                text=True,
                env={
                    **os.environ,
                    "COMMON": str(common),
                    "GATEWAY_DIR": str(Path(temporary_dir) / "gateway"),
                },
            )
            self.assertEqual(result.returncode, 0, result.stderr)

    def test_zksys_asset_id_uses_the_edge_origin_chain(self) -> None:
        deploy = (
            REPO_ROOT / "scripts" / "gateway-launch" / "gateway-deploy-l1.sh"
        ).read_text(encoding="utf-8")
        start = deploy.index("derive_and_export_zksys_zk_token_asset_id()")
        end = deploy.index("\nderive_zksys_l2_registry_address()", start)
        derivation = deploy[start:end]
        self.assertIn("normalize_zksys_uint_var EDGE_CHAIN_ID", derivation)
        self.assertIn('"${zksys_origin_chain_id}"', derivation)
        self.assertNotIn('"${GATEWAY_CHAIN_ID}"', derivation)

    def test_reused_gateway_governor_is_authenticated_on_l1(self) -> None:
        common = REPO_ROOT / "scripts" / "gateway-launch" / "_common.sh"
        governor = "0x7e5f4552091a69125d5dfcb7b8c2659029395bdf"
        other = "0x" + "22" * 20
        bridgehub = "0x" + "33" * 20
        diamond = "0x" + "44" * 20
        gateway_diamond = "0x" + "77" * 20
        zero = "0x" + "00" * 20
        chain_admin = "0x" + "33" * 20
        gateway_chain_admin = "0x" + "88" * 20
        with tempfile.TemporaryDirectory() as temporary_dir:
            root = Path(temporary_dir)
            gateway_wallet = root / "chains" / "gateway" / "configs" / "wallets.yaml"
            edge_wallet = root / "chains" / "zksys" / "configs" / "wallets.yaml"
            gateway_zkstack = root / "chains" / "gateway" / "ZkStack.yaml"
            edge_zkstack = root / "chains" / "zksys" / "ZkStack.yaml"
            gateway_contracts = root / "chains" / "gateway" / "configs" / "contracts.yaml"
            edge_contracts = root / "chains" / "zksys" / "configs" / "contracts.yaml"
            ecosystem_contracts = root / "configs" / "contracts.yaml"
            gateway_wallet.parent.mkdir(parents=True)
            edge_wallet.parent.mkdir(parents=True)
            ecosystem_contracts.parent.mkdir(parents=True)
            gateway_wallet.write_text(
                json.dumps(
                    {
                        "governor": {
                            "address": governor,
                            "private_key": "0x" + "00" * 31 + "01",
                        }
                    }
                ),
                encoding="utf-8",
            )
            ecosystem_contracts.write_text(
                json.dumps(
                    {
                        "core_ecosystem_contracts": {
                            "bridgehub_proxy_addr": bridgehub
                        }
                    }
                ),
                encoding="utf-8",
            )

            bin_dir = root / "bin"
            bin_dir.mkdir()
            fake_cast = bin_dir / "cast"
            fake_cast.write_text(
                "#!/usr/bin/env bash\n"
                "set -euo pipefail\n"
                "case \"${1:-}\" in\n"
                "  keccak)\n"
                "    if [ \"${2:-}\" = '0x79be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798483ada7726a3c4655da4fbfc0e1108a8fd17b448a68554199c47d08ffb10d4b8' ]; then suffix=\"${TEST_GATEWAY_GOVERNOR#0x}\"; else suffix=\"${TEST_OTHER_ADDRESS#0x}\"; fi\n"
                "    printf '%s%s\\n' '0x000000000000000000000000' \"${suffix}\" ;;\n"
                "  code)\n"
                "    if [ \"${2:-}\" = \"${TEST_GATEWAY_CHAIN_ADMIN:?}\" ]; then printf '%s\\n' \"${TEST_GATEWAY_CHAIN_ADMIN_CODE:?}\"; elif [ \"${2:-}\" = \"${TEST_CHAIN_ADMIN:?}\" ]; then printf '%s\\n' \"${TEST_CHAIN_ADMIN_CODE:?}\"; else exit 3; fi ;;\n"
                "  call)\n"
                "    if [ \"${3:-}\" = 'getZKChain(uint256)(address)' ] && [ \"${2:-}\" != \"${TEST_BRIDGEHUB:?}\" ]; then exit 3; fi\n"
                "    case \"${3:-}\" in\n"
                "      'getZKChain(uint256)(address)')\n"
                "        if [ \"${4:-}\" = 57001 ]; then [ \"${TEST_GATEWAY_QUERY_FAIL:-false}\" != true ] || exit 9; printf '%s\\n' \"${TEST_GATEWAY_DIAMOND:?}\"; elif [ \"${4:-}\" = 57057 ]; then printf '%s\\n' \"${TEST_EDGE_DIAMOND:?}\"; else exit 3; fi ;;\n"
                "      'getAdmin()(address)')\n"
                "        if [ \"${2:-}\" = \"${TEST_GATEWAY_DIAMOND:?}\" ]; then printf '%s\\n' \"${TEST_GATEWAY_CHAIN_ADMIN:?}\"; elif [ \"${2:-}\" = \"${TEST_EDGE_DIAMOND:?}\" ]; then printf '%s\\n' \"${TEST_CHAIN_ADMIN:?}\"; else exit 3; fi ;;\n"
                "      'owner()(address)')\n"
                "        if [ \"${2:-}\" = \"${TEST_GATEWAY_CHAIN_ADMIN:?}\" ]; then printf '%s\\n' \"${TEST_GATEWAY_OWNER:?}\"; elif [ \"${2:-}\" = \"${TEST_CHAIN_ADMIN:?}\" ]; then printf '%s\\n' \"${TEST_CHAIN_ADMIN_OWNER:?}\"; else exit 3; fi ;;\n"
                "      *) exit 3 ;;\n"
                "    esac ;;\n"
                "  *) exit 2 ;;\n"
                "esac\n",
                encoding="utf-8",
            )
            fake_cast.chmod(0o755)
            (bin_dir / "yaml.py").write_text(
                "from json import loads\n"
                "safe_load = loads\n"
                "BaseLoader = object\n"
                "def load(value, Loader=None):\n"
                "    return loads(value)\n",
                encoding="utf-8",
            )

            def run_check(
                *,
                edge_governor: str,
                registered_diamond: str,
                owner: str = governor,
                gateway_owner: str = governor,
                gateway_private_key: str = "0x" + "00" * 31 + "01",
                registered_gateway_diamond: str = gateway_diamond,
                code: str = "0x6000",
                gateway_code: str = "0x6000",
                gateway_query_failure: bool = False,
                persisted_gateway_diamond: str = gateway_diamond,
                edge_created: bool = False,
                post_init: bool = False,
                gateway_chain_id: int = 57001,
                edge_chain_id: int = 57057,
            ):
                gateway_wallet.write_text(
                    json.dumps(
                        {
                            "governor": {
                                "address": governor,
                                "private_key": gateway_private_key,
                            }
                        }
                    ),
                    encoding="utf-8",
                )
                def chain_config(name: str, chain_id: int) -> dict:
                    return {
                        "name": name,
                        "chain_id": chain_id,
                        "prover_version": "Gpu",
                        "l1_batch_commit_data_generator_mode": "Rollup",
                        "vm_option": "ZKSyncOsVM",
                        "evm_emulator": False,
                        "base_token": {
                            "address": "0x" + "00" * 19 + "01",
                            "nominator": 1,
                            "denominator": 1,
                        },
                    }

                gateway_zkstack.write_text(
                    json.dumps(chain_config("gateway", gateway_chain_id)),
                    encoding="utf-8",
                )
                edge_zkstack.write_text(
                    json.dumps(chain_config("zksys", edge_chain_id)),
                    encoding="utf-8",
                )
                gateway_contracts.write_text(
                    json.dumps(
                        {
                            "ecosystem_contracts": {
                                "bridgehub_proxy_addr": bridgehub
                            },
                            "l1": {"diamond_proxy_addr": persisted_gateway_diamond},
                        }
                    ),
                    encoding="utf-8",
                )
                edge_contracts.write_text(
                    json.dumps(
                        {
                            "ecosystem_contracts": {
                                "bridgehub_proxy_addr": bridgehub
                            },
                            "l1": {"diamond_proxy_addr": registered_diamond},
                        }
                    ),
                    encoding="utf-8",
                )
                edge_wallet.write_text(
                    json.dumps(
                        {
                            "governor": {
                                "address": edge_governor,
                                "private_key": (
                                    "0x" + "00" * 31 + "01"
                                    if edge_governor == governor
                                    else "0x" + "66" * 32
                                ),
                            }
                        }
                    ),
                    encoding="utf-8",
                )
                wallet_before = edge_wallet.read_bytes()
                env = os.environ.copy()
                env.pop("ZKSYNC_OS_SERVER_PATH", None)
                env.update(
                    {
                        "COMMON": str(common),
                        "GATEWAY_DIR": str(root),
                        "L1_RPC_URL": "http://l1.invalid",
                        "PATH": f"{bin_dir}{os.pathsep}{env['PATH']}",
                        "PYTHONPATH": str(bin_dir),
                        "TEST_FAKE_CAST": str(fake_cast),
                        "TEST_CHAIN_ADMIN_CODE": code,
                        "TEST_GATEWAY_CHAIN_ADMIN_CODE": gateway_code,
                        "TEST_CHAIN_ADMIN_OWNER": owner,
                        "TEST_EDGE_DIAMOND": registered_diamond,
                        "TEST_CHAIN_ADMIN": chain_admin,
                        "TEST_GATEWAY_DIAMOND": registered_gateway_diamond,
                        "TEST_GATEWAY_CHAIN_ADMIN": gateway_chain_admin,
                        "TEST_GATEWAY_OWNER": gateway_owner,
                        "TEST_GATEWAY_GOVERNOR": governor,
                        "TEST_OTHER_ADDRESS": other,
                        "TEST_GATEWAY_QUERY_FAIL": str(gateway_query_failure).lower(),
                        "TEST_BRIDGEHUB": bridgehub,
                    }
                )
                function = (
                    "gl_assert_edge_chain_admin_owned_by_gateway_governor"
                    if post_init
                    else "gl_assert_existing_edge_chain_admin_safe_for_governor_reuse"
                )
                args = "" if post_init else str(edge_created).lower()
                result = subprocess.run(
                    [
                        "bash",
                        "-c",
                        f'source "$COMMON"; cast() {{ "$TEST_FAKE_CAST" "$@"; }}; {function} {args}',
                    ],
                    check=False,
                    capture_output=True,
                    text=True,
                    env=env,
                )
                self.assertEqual(edge_wallet.read_bytes(), wallet_before)
                return result

            accepted = run_check(
                edge_governor=other,
                registered_diamond=diamond,
                owner=governor.upper().replace("0X", "0x"),
            )
            self.assertEqual(accepted.returncode, 0, accepted.stderr)
            mismatch = run_check(
                edge_governor=other,
                registered_diamond=diamond,
                owner=other,
            )
            self.assertNotEqual(mismatch.returncode, 0)
            self.assertIn("edge ChainAdmin owner mismatch", mismatch.stderr)
            gateway_mismatch = run_check(
                edge_governor=other,
                registered_diamond=diamond,
                gateway_owner=other,
            )
            self.assertNotEqual(gateway_mismatch.returncode, 0)
            self.assertIn("Gateway ChainAdmin owner mismatch", gateway_mismatch.stderr)

            gateway_unregistered = run_check(
                edge_governor=governor,
                registered_diamond=zero,
                registered_gateway_diamond=zero,
            )
            self.assertNotEqual(gateway_unregistered.returncode, 0)
            self.assertIn("persisted Gateway diamond", gateway_unregistered.stderr)

            gateway_missing_runtime = run_check(
                edge_governor=governor,
                registered_diamond=zero,
                gateway_code="0x",
            )
            self.assertNotEqual(gateway_missing_runtime.returncode, 0)
            self.assertIn("missing Gateway ChainAdmin runtime", gateway_missing_runtime.stderr)

            gateway_query_failed = run_check(
                edge_governor=governor,
                registered_diamond=zero,
                gateway_query_failure=True,
            )
            self.assertNotEqual(gateway_query_failed.returncode, 0)
            self.assertIn("failed to query L1 BridgeHub registration for Gateway", gateway_query_failed.stderr)

            key_mismatch = run_check(
                edge_governor=governor,
                registered_diamond=zero,
                gateway_private_key="0x" + "00" * 31 + "02",
            )
            self.assertNotEqual(key_mismatch.returncode, 0)
            self.assertIn("address/private-key mismatch", key_mismatch.stderr)

            stale_gateway = run_check(
                edge_governor=governor,
                registered_diamond=zero,
                gateway_chain_id=57002,
            )
            self.assertNotEqual(stale_gateway.returncode, 0)
            self.assertIn("Gateway chain_id mismatch", stale_gateway.stderr)
            stale_edge = run_check(
                edge_governor=governor,
                registered_diamond=zero,
                edge_chain_id=57058,
            )
            self.assertNotEqual(stale_edge.returncode, 0)
            self.assertIn("edge chain_id mismatch", stale_edge.stderr)

            # A separately governed edge is still bound to its configured ID.
            edge_zkstack.write_text(
                json.dumps({"chain_id": 57058}), encoding="utf-8"
            )
            reuse_disabled_env = os.environ.copy()
            reuse_disabled_env.pop("ZKSYNC_OS_SERVER_PATH", None)
            reuse_disabled_env.update(
                {
                    "COMMON": str(common),
                    "GATEWAY_DIR": str(root),
                    "EDGE_CHAIN_ID": "57057",
                    "EDGE_REUSE_GATEWAY_GOVERNOR": "false",
                    "PYTHONPATH": str(bin_dir),
                }
            )
            reuse_disabled = subprocess.run(
                [
                    "bash",
                    "-c",
                    'source "$COMMON"; gl_probe_edge_chain_inited_ready() { return 0; }; '
                    "gl_probe_edge_chain_inited_and_governor_ready",
                ],
                check=False,
                capture_output=True,
                text=True,
                env=reuse_disabled_env,
            )
            self.assertNotEqual(reuse_disabled.returncode, 0)
            self.assertIn("edge chain_id mismatch", reuse_disabled.stderr)

            already_rewritten = run_check(
                edge_governor=governor,
                registered_diamond=zero,
            )
            self.assertEqual(already_rewritten.returncode, 0, already_rewritten.stderr)
            ambiguous = run_check(
                edge_governor=other,
                registered_diamond=zero,
            )
            self.assertNotEqual(ambiguous.returncode, 0)
            self.assertIn("refusing to overwrite", ambiguous.stderr)
            just_created = run_check(
                edge_governor=other,
                registered_diamond=zero,
                edge_created=True,
            )
            self.assertEqual(just_created.returncode, 0, just_created.stderr)

            missing_post_init = run_check(
                edge_governor=governor,
                registered_diamond=zero,
                post_init=True,
            )
            self.assertNotEqual(missing_post_init.returncode, 0)
            self.assertIn("still unregistered on L1 after init", missing_post_init.stderr)
            missing_runtime = run_check(
                edge_governor=governor,
                registered_diamond=diamond,
                code="0x",
                post_init=True,
            )
            self.assertNotEqual(missing_runtime.returncode, 0)
            self.assertIn("missing edge ChainAdmin runtime", missing_runtime.stderr)

    def test_gateway_launch_requires_generated_genesis_byte_identity(self) -> None:
        launcher = (
            REPO_ROOT / "scripts" / "gateway-launch" / "gateway-deploy-l1.sh"
        ).read_text(encoding="utf-8")

        self.assertIn(
            f'SYSCOIN_CANONICAL_GENESIS_SHA256="{PUBLISHED_ERA_GENESIS_SHA256}"',
            launcher,
        )
        self.assertIn(
            "canonical Syscoin V32 genesis digest mismatch before generation",
            launcher,
        )
        self.assertIn(
            "generated Syscoin V32 genesis is not byte-identical to the reviewed snapshot",
            launcher,
        )
        self.assertIn("if canonical != reviewed:", launcher)
        self.assertIn("if generated != reviewed:", launcher)
        self.assertIn(
            'FOUNDRY_PROFILE=default FOUNDRY_EVM_VERSION="${FOUNDRY_EVM_VERSION}"',
            launcher,
        )
        self.assertEqual(launcher.count("FOUNDRY_EVM_VERSION=prague"), 1)
        self.assertIn(
            '--out "${SYSCOIN_GENESIS_WORK_DIR}/contracts/l1-contracts/out"',
            launcher,
        )
        self.assertIn(
            '--cache-path "${SYSCOIN_GENESIS_WORK_DIR}/forge-cache"',
            launcher,
        )
        self.assertIn(
            'cd "${SYSCOIN_GENESIS_WORK_DIR}/contracts/tools/zksync-os-genesis-gen"',
            launcher,
        )
        self.assertIn("cargo +nightly-2026-01-22 run", launcher)
        self.assertIn(
            '--manifest-path "${ZKSYNC_ERA_PATH}/contracts/tools/zksync-os-genesis-gen/Cargo.toml"',
            launcher,
        )
        self.assertIn(
            "export FOUNDRY_PROFILE=default",
            launcher,
        )
        self.assertIn("os.replace(generated_path, destination_path)", launcher)
        self.assertIn(
            "installed Syscoin V32 genesis differs from the reviewed snapshot",
            launcher,
        )
        self.assertLess(
            launcher.index("trap cleanup_genesis_work_dir EXIT"),
            launcher.index('SYSCOIN_GENESIS_WORK_DIR="$(mktemp'),
        )
        self.assertNotIn(
            '--output-file "${SYSCOIN_GENERATED_GENESIS}"',
            launcher,
        )
        self.assertIn("--bin zksync-os-genesis-gen", launcher)
        self.assertIn("--locked", launcher)
        self.assertLess(
            launcher.index('FOUNDRY_EVM_VERSION="${FOUNDRY_EVM_VERSION}"'),
            launcher.index("FOUNDRY_EVM_VERSION=prague"),
        )
        self.assertLess(
            launcher.index("os.replace(generated_path, destination_path)"),
            launcher.index(
                "gl_zkstack_pty env FOUNDRY_PROFILE=default zkstack dev contracts"
            ),
        )

    def test_server_verifier_uses_the_final_v8_airbender_graph(self) -> None:
        manifest = (REPO_ROOT / "Cargo.toml").read_text(encoding="utf-8")
        lock = (REPO_ROOT / "Cargo.lock").read_text(encoding="utf-8")
        self.assertIn('tag = "v0.6.0-rc.2"', manifest)
        self.assertNotIn('tag = "v0.6.0-rc.1"', manifest)
        self.assertIn(
            "?tag=v0.6.0-rc.2#03454c7a41053a4b88bb421e97fb9efe893a92f5",
            lock,
        )
        self.assertNotIn("?tag=v0.6.0-rc.1", lock)

    def test_pre_keygen_app_identity_is_explicitly_fail_closed(self) -> None:
        workflow = (
            REPO_ROOT / ".github" / "workflows" / "syscoin-v32-v8-keygen.yml"
        ).read_text(encoding="utf-8")
        verifier = (
            REPO_ROOT
            / "node"
            / "bin"
            / "src"
            / "prover_api"
            / "fri_proof_verifier.rs"
        ).read_text(encoding="utf-8")

        for source in (workflow, verifier):
            self.assertIn(PUBLISHED_ZKSYNC_OS_PATCHED_TREE, source)
            self.assertNotIn("5117d5dac6dbd34b93fef54e04d0b41c", source)
            self.assertNotIn("1279059325", source)
            self.assertNotIn("220972078", source)

        self.assertIn("APP_IDENTITY_STATUS: regeneration-required", workflow)
        self.assertIn('APP_BIN_SIZE: "0"', workflow)
        self.assertIn('APP_TEXT_SIZE: "0"', workflow)
        self.assertIn('APP_END_PARAMS: "[0, 0, 0, 0, 0, 0, 0, 0]"', workflow)
        self.assertIn('SECURITY100_WORDS: "[0, 0, 0, 0, 0, 0, 0, 0]"', workflow)
        status_gate = workflow.index(
            'if [[ "${APP_IDENTITY_STATUS}" != "attested" ]]; then'
        )
        self.assertLess(status_gate, workflow.index("  syscoin-keygen:"))
        self.assertIn(
            '[[ "${APP_IDENTITY_SOURCE_TREE}" == "${ZKSYNC_OS_PATCHED_TREE}" ]]',
            workflow,
        )
        self.assertIn(
            'require_words("V8_APP_END_PARAMS", os.environ["APP_END_PARAMS"])',
            workflow,
        )
        self.assertIn(
            'require_words("V8_SECURITY100_EXPECTED_CHAIN", os.environ["SECURITY100_WORDS"])',
            workflow,
        )

        self.assertIn(
            "const V8_APP_IDENTITY_REGENERATION_REQUIRED: bool = true;", verifier
        )
        self.assertIn("const V8_APP_END_PARAMS: [u32; 8] = [0; 8];", verifier)
        self.assertIn(
            "const V8_SECURITY100_EXPECTED_CHAIN: [u32; 8] = [0; 8];", verifier
        )
        self.assertLess(
            verifier.index("if v8_verifier::V8_APP_IDENTITY_REGENERATION_REQUIRED"),
            verifier.index("validate_v8_proof_shape(proof)?;"),
        )

    def test_gateway_launcher_sources_shared_workspace_helper(self) -> None:
        launcher = (
            REPO_ROOT
            / "scripts"
            / "gateway-launch"
            / "run-os-server-with-patched-zksync-os.sh"
        ).read_text(encoding="utf-8")
        helper = (REPO_ROOT / "scripts" / "_patched-zksync-os-workspace.sh").read_text(
            encoding="utf-8"
        )
        self.assertIn('source "${SCRIPT_DIR}/../_patched-zksync-os-workspace.sh"', launcher)
        self.assertNotIn("extract_zksync_os_tag()", launcher)
        self.assertIn("extract_zksync_os_tag()", helper)
        self.assertIn("prepare_run_workspace()", helper)
        self.assertIn("ZKSYNC_OS_ALIAS=zk_os_forward_system", launcher)
        self.assertIn("apply-zksync-os-syscoin-v0.4.0-patch.sh", launcher)
        self.assertNotIn("ZKSYNC_OS_V8_DEV_PATH", launcher)
        self.assertNotIn("V7_ZKSYNC_OS", launcher)
        self.assertNotIn("V8_ZKSYNC_OS", launcher)
        self.assertNotIn("zk_os_forward_system_prev", helper)

    def test_run_local_builds_one_patched_prebuilt_and_executes_it(self) -> None:
        script = (REPO_ROOT / "run_local.sh").read_text(encoding="utf-8")
        runner = (
            REPO_ROOT
            / "scripts"
            / "gateway-launch"
            / "run-os-server-with-patched-zksync-os.sh"
        ).read_text(encoding="utf-8")
        self.assertEqual(script.count("-- build-prebuilt"), 1)
        self.assertEqual(script.count("-- exec-prebuilt --"), 1)
        self.assertEqual(script.count('bash "$PATCHED_OS_RUNNER"'), 2)
        self.assertIn("ZKSYNC_OS_FORCE_PATCHED_WORKSPACE=true", script)
        self.assertIn(PUBLISHED_PATCH_TARGET, script)
        self.assertIn(PUBLISHED_GAS_TANK, script)
        self.assertNotIn("resolve-local-zksync-os-context.py", script)
        self.assertNotIn("cargo run --release --manifest-path", script)
        self.assertIn('exit "$exit_status"', script)
        self.assertIn("trap cleanup EXIT", script)
        self.assertIn("runner_sha256_file()", runner)
        self.assertIn("runner_sha256_stdin()", runner)
        self.assertEqual(runner.count("shasum -a 256"), 2)

    def test_generated_real_prover_storage_covers_the_full_queue_window(self) -> None:
        generator = (
            REPO_ROOT
            / "scripts"
            / "gateway-launch"
            / "generate-os-server-configs.sh"
        ).read_text(encoding="utf-8")
        self.assertIn('PROVER_BATCH_WITH_PROOF_CAPACITY_BYTES:=8589934592', generator)
        self.assertIn('prover_batch_with_proof_capacity_bytes < 8 * 1024**3', generator)
        self.assertIn(
            'batch_with_proof_capacity: {prover_batch_with_proof_capacity_bytes} B',
            generator,
        )

    def test_generated_prover_proxy_bounds_request_and_response_buffering(self) -> None:
        generator = (
            REPO_ROOT
            / "scripts"
            / "gateway-launch"
            / "generate-os-server-configs.sh"
        ).read_text(encoding="utf-8")

        # SYSCOIN: The public proxy must reject oversized bodies before an unauthenticated
        # client can fill nginx temporary storage; the node enforces the same 10 MiB ceiling.
        self.assertIn("client_max_body_size 10m;", generator)
        self.assertNotIn("client_max_body_size 0;", generator)
        self.assertIn("proxy_connect_timeout 5s;", generator)
        self.assertIn("proxy_send_timeout 650s;", generator)
        self.assertIn("proxy_read_timeout 650s;", generator)
        # SYSCOIN: The complete bounded response must drain from the node into nginx even if an
        # authenticated remote prover stops reading, releasing the node's scarce pick permit.
        self.assertIn("proxy_buffering on;", generator)
        self.assertIn("proxy_max_temp_file_size 384m;", generator)
        self.assertIn("proxy_ignore_headers X-Accel-Buffering;", generator)
        self.assertIn(
            "location ~ ^/prover-jobs/v1/(?:FRI/[^/]+/(?:peek|failed)|SNARK/[^/]+/[^/]+/peek)/?$",
            generator,
        )
        self.assertNotIn("ALLOW_INSECURE_PROVER_HTTP", generator)
        self.assertNotIn("allow_insecure_public_bind", generator)
        self.assertIn('if prover_api_bind_host != "127.0.0.1":', generator)
        self.assertIn("len(prover_api_auth_password) < 32", generator)
        self.assertIn("openssl rand -hex 32", generator)

    def test_generated_and_local_configs_allow_the_cpu_snark_lease(self) -> None:
        generator = (
            REPO_ROOT
            / "scripts"
            / "gateway-launch"
            / "generate-os-server-configs.sh"
        ).read_text(encoding="utf-8")
        local_config = (REPO_ROOT / "local-chains" / "local_dev.yaml").read_text(
            encoding="utf-8"
        )

        # SYSCOIN: CPU combine/wrap can exceed the former ten-minute lease; both generated
        # deployments and the local overlay must exercise the conservative production default.
        self.assertIn('"  snark_job_timeout: 2h"', generator)
        self.assertIn("  snark_job_timeout: 2h", local_config)

    def test_generic_cargo_wrapper_uses_official_source_and_static_app_inputs(self) -> None:
        wrapper = (
            REPO_ROOT / "scripts" / "cargo-with-patched-zksync-os.sh"
        ).read_text(encoding="utf-8")
        self.assertIn("https://github.com/matter-labs/zksync-os.git", wrapper)
        self.assertNotIn("https://github.com/syscoin/zksync-os", wrapper)
        self.assertIn(PUBLISHED_PATCH_TARGET, wrapper)
        self.assertIn(PUBLISHED_GAS_TANK, wrapper)
        self.assertIn("ZKSYNC_OS_FORCE_PATCHED_WORKSPACE=true", wrapper)
        self.assertIn("ZKSYNC_OS_STATIC_BUILD_CONTEXT=true", wrapper)
        self.assertIn("differs from the published zksync-os app value", wrapper)
        self.assertIn("${CARGO_TARGET_DIR}/syscoin-zksync-os-server-build", wrapper)
        self.assertNotIn("${TMPDIR:-/tmp}/syscoin-zksync-os-server-build", wrapper)

    def test_os_applicator_does_not_regenerate_consensus_constants(self) -> None:
        applicator = (
            REPO_ROOT / "scripts" / "apply-zksync-os-syscoin-v0.4.0-patch.sh"
        ).read_text(encoding="utf-8")
        self.assertIn("apply --reverse --check --recount", applicator)
        self.assertIn("--unidiff-zero", applicator)
        self.assertNotIn("EXPECTED_BASE_TAG", applicator)
        for expected in (
            'EXPECTED_BASE_COMMIT="69bc430549e88f9264066d14f2001707572c5d33"',
            'EXPECTED_BASE_TREE="233b36e77843e460ee9da3e344ee227fa8cce04a"',
            'EXPECTED_PATCHED_TREE="9fb99cf591c553447cd3839489cc4d327eb424b4"',
            'EXPECTED_PATCH_SIZE="275841"',
            'EXPECTED_PATCH_SHA256="556a223a7c095e30030a869c4d08d102c24ac00c8623f50649d17a07a9193965"',
            'EXPECTED_PATCH_PATH_COUNT="64"',
            'EXPECTED_PATCH_PATHS_SHA256="33a2714fec3c4c61e754ed699f94c1529fbddc549bd033ced143162deb4bcf7a"',
        ):
            self.assertIn(expected, applicator)
        workspace_helper = (
            REPO_ROOT / "scripts" / "_patched-zksync-os-workspace.sh"
        ).read_text(encoding="utf-8")
        self.assertIn(
            'SYSCOIN_EXPECTED_ZKSYNC_OS_PATCHED_TREE="9fb99cf591c553447cd3839489cc4d327eb424b4"',
            workspace_helper,
        )
        self.assertIn('require_text "${tagged_path}" "SYSCOIN:"', applicator)
        self.assertIn('*.rs | *.toml | *.sh)', applicator)

        patch = (
            REPO_ROOT
            / "scripts"
            / "patches"
            / "zksync-os-syscoin-v0.4.0.patch"
        ).read_text(encoding="utf-8")
        self.assertNotIn("canonical_upgrade_tx_hash", patch)
        self.assertNotIn("canonical upgrade tx hash", patch)
        self.assertNotIn("blob_data_id_advice", patch)
        self.assertNotIn("callable_oracles/src/blob_data_id", patch)
        self.assertIn("host advice is neither", patch)

    def test_os_keygen_workflow_attests_current_source_inputs(self) -> None:
        helper_path = (
            REPO_ROOT / "scripts" / "apply-zksync-os-syscoin-v0.4.0-patch.sh"
        )
        patch_path = (
            REPO_ROOT
            / "scripts"
            / "patches"
            / "zksync-os-syscoin-v0.4.0.patch"
        )
        workflow = (
            REPO_ROOT / ".github" / "workflows" / "syscoin-v32-v8-keygen.yml"
        ).read_text(encoding="utf-8")
        helper = helper_path.read_bytes()
        patch = patch_path.read_bytes()
        gateway_identity_path = (
            REPO_ROOT / "local-chains" / "v32.0" / "gateway-identity.v1.json"
        )
        gateway_identity = gateway_identity_path.read_bytes()
        gateway_identity_data = json.loads(gateway_identity)

        for expected in (
            f'ZKSYNC_OS_PATCHED_TREE: {PUBLISHED_ZKSYNC_OS_PATCHED_TREE}',
            f'ZKSYNC_OS_PATCH_SIZE: "{len(patch)}"',
            f"ZKSYNC_OS_PATCH_SHA256: {hashlib.sha256(patch).hexdigest()}",
            f'ZKSYNC_OS_HELPER_SIZE: "{len(helper)}"',
            f"ZKSYNC_OS_HELPER_SHA256: {hashlib.sha256(helper).hexdigest()}",
            f'APP_IDENTITY_SOURCE_TREE: {PUBLISHED_ZKSYNC_OS_PATCHED_TREE}',
            f'SYSCOIN_EDGE_DA_COMMIT_TARGET: "{PUBLISHED_PATCH_TARGET}"',
            f'SYSCOIN_EDGE_DA_COMMIT_TARGET_RUNTIME_SIZE: "{PUBLISHED_PATCH_TARGET_RUNTIME_SIZE}"',
            f'SYSCOIN_EDGE_DA_COMMIT_TARGET_RUNTIME_HASH: "{PUBLISHED_PATCH_TARGET_RUNTIME_HASH}"',
            f'SYSCOIN_EDGE_DA_RELAY_EMITTER: "{PUBLISHED_EDGE_RELAY}"',
            f'SYSCOIN_EDGE_DA_RELAY_RUNTIME_HASH: "{PUBLISHED_EDGE_RELAY_RUNTIME_HASH}"',
            "GATEWAY_TARGET_IDENTITY_STATUS: regeneration-required",
            "GATEWAY_TARGET_IDENTITY_PATH: local-chains/v32.0/gateway-identity.v1.json",
            f'GATEWAY_TARGET_IDENTITY_SIZE: "{len(gateway_identity)}"',
            f"GATEWAY_TARGET_IDENTITY_SHA256: {hashlib.sha256(gateway_identity).hexdigest()}",
            "Gateway target derivation must be frozen before app/VK generation",
            "Gateway identity artifact has incomplete production derivation inputs",
            "native server does not contain the approved Gateway target identity",
            'gateway_target: {',
            'validator_timelock: $gateway_target',
            'relay_emitter: $gateway_relay',
            'relay_runtime_keccak256: $gateway_relay_runtime_hash',
            'derivation_sha256: $gateway_target_identity_sha256',
        ):
            self.assertIn(expected, workflow)
        self.assertEqual(gateway_identity_data["status"], "integration-candidate")
        self.assertFalse(gateway_identity_data["production_attested"])
        self.assertEqual(
            gateway_identity_data["outputs"]["validator_timelock"],
            PUBLISHED_PATCH_TARGET,
        )
        self.assertEqual(
            gateway_identity_data["outputs"]["validator_timelock_runtime_size"],
            PUBLISHED_PATCH_TARGET_RUNTIME_SIZE,
        )
        self.assertEqual(
            gateway_identity_data["outputs"]["validator_timelock_runtime_keccak256"],
            PUBLISHED_PATCH_TARGET_RUNTIME_HASH,
        )
        self.assertEqual(
            gateway_identity_data["outputs"]["relay_emitter"],
            PUBLISHED_EDGE_RELAY,
        )
        self.assertEqual(
            gateway_identity_data["outputs"]["relay_runtime_keccak256"],
            PUBLISHED_EDGE_RELAY_RUNTIME_HASH,
        )

    def test_published_consensus_constants_are_consistent(self) -> None:
        deploy_en = (
            REPO_ROOT
            / "scripts"
            / "explorer"
            / "blockscout"
            / "deploy-zksys-en-rpc.sh"
        )
        paths_with_target = (
            REPO_ROOT / "run_local.sh",
            REPO_ROOT / "scripts" / "cargo-with-patched-zksync-os.sh",
            REPO_ROOT
            / "scripts"
            / "gateway-launch"
            / "run-os-server-with-patched-zksync-os.sh",
            deploy_en,
        )
        paths_with_gas_tank = paths_with_target + (
            REPO_ROOT / "scripts" / "gateway-launch" / "zksys-l2-bootstrap.sh",
            REPO_ROOT
            / "scripts"
            / "explorer"
            / "blockscout"
            / "deploy-pali-entrypoint-gastank-zktanenbaum.sh",
        )
        for path in paths_with_target:
            with self.subTest(path=path, constant="edge target"):
                self.assertIn(PUBLISHED_PATCH_TARGET, path.read_text(encoding="utf-8"))
        for path in paths_with_gas_tank:
            with self.subTest(path=path, constant="gas tank"):
                self.assertIn(PUBLISHED_GAS_TANK, path.read_text(encoding="utf-8"))

        patch = (
            REPO_ROOT
            / "scripts"
            / "patches"
            / "zksync-os-syscoin-v0.4.0.patch"
        ).read_text(encoding="utf-8")
        patch_postimage = "\n".join(
            line[1:] if line.startswith("+") and not line.startswith("+++") else line
            for line in patch.splitlines()
        )
        normalized_patch = " ".join(patch_postimage.split())
        self.assertIn(rust_address_bytes(PUBLISHED_PATCH_TARGET), normalized_patch)
        self.assertNotIn("64ef2f0c4168eb76fe95993f2a7c7b35dcf3fe19", normalized_patch)
        self.assertIn(rust_address_bytes(PUBLISHED_EDGE_RELAY), normalized_patch)
        self.assertIn(rust_address_bytes(PUBLISHED_GAS_TANK), normalized_patch)

        types = (REPO_ROOT / "lib" / "types" / "src" / "lib.rs").read_text(
            encoding="utf-8"
        ).lower()
        era_patch = (
            REPO_ROOT / "scripts" / "patches" / "era-contracts-syscoin.patch"
        ).read_text(encoding="utf-8").lower()
        self.assertIn(PUBLISHED_EDGE_RELAY, types)
        self.assertIn(PUBLISHED_PATCH_TARGET_RUNTIME_HASH.removeprefix("0x"), types)
        self.assertIn(
            "syscoin_compact_edge_da_commit_target_runtime_size: u32 = 2_840",
            types,
        )
        self.assertIn(PUBLISHED_EDGE_RELAY_RUNTIME_HASH.removeprefix("0x"), types)
        self.assertIn(PUBLISHED_EDGE_RELAY, era_patch)
        self.assertIn(PUBLISHED_EDGE_RELAY_RUNTIME_HASH, era_patch)

        deploy_en_text = deploy_en.read_text(encoding="utf-8")
        migration = (
            REPO_ROOT
            / "scripts"
            / "gateway-launch"
            / "edge-chain-migrate-to-gateway.sh"
        ).read_text(encoding="utf-8")
        self.assertIn(
            f'readonly SYSCOIN_COMPACT_EDGE_DA_RELAY="{PUBLISHED_EDGE_RELAY}"',
            migration,
        )
        self.assertIn(
            'readonly SYSCOIN_COMPACT_EDGE_DA_RELAY_RUNTIME_HASH="'
            f'{PUBLISHED_EDGE_RELAY_RUNTIME_HASH}"',
            migration,
        )
        self.assertIn("gateway_address_has_exact_runtime", migration)
        self.assertIn("does not match the guest-bound relay", migration)
        relay_preflight = (
            'l1_da_validator_addr="$(get_l1_da_validator_for_edge '
            '"${EDGE_CHAIN_NAME}" "${GATEWAY_CHAIN_NAME}" "${GATEWAY_RPC_URL}")"'
        )
        self.assertEqual(migration.count(relay_preflight), 1)
        self.assertLess(
            migration.index(relay_preflight),
            migration.index("zkstack chain pause-deposits \\"),
        )
        self.assertLess(
            migration.index(relay_preflight),
            migration.index("zkstack chain gateway migrate-to-gateway"),
        )
        self.assertLess(
            migration.index(relay_preflight),
            migration.index("finalize-chain-migration-to-gateway"),
        )
        self.assertLess(
            deploy_en_text.index("PUBLISHED_EDGE_DA_COMMIT_TARGET="),
            deploy_en_text.index("remote_prebuilt_stamps=("),
        )
        deploy_gas_tank = (
            REPO_ROOT
            / "scripts"
            / "explorer"
            / "blockscout"
            / "deploy-pali-entrypoint-gastank-zktanenbaum.sh"
        ).read_text(encoding="utf-8")
        self.assertNotIn("cast compute-address --nonce", deploy_gas_tank)
        self.assertNotIn("forge create src/zksys/ZkSysGasTank.sol", deploy_gas_tank)
        self.assertIn("require_canonical_create2_deployer", deploy_gas_tank)
        self.assertIn(
            f'GAS_TANK_INIT_CODE_HASH="{PUBLISHED_GAS_TANK_INIT_CODE_HASH}"',
            deploy_gas_tank,
        )
        self.assertIn(
            f'GAS_TANK_RUNTIME_HASH="{PUBLISHED_GAS_TANK_RUNTIME_HASH}"',
            deploy_gas_tank,
        )
        self.assertIn(
            "cast create2 \\\n"
            '    --deployer "${CREATE2_DEPLOYER_ADDRESS}"',
            deploy_gas_tank,
        )
        self.assertIn('"${GAS_TANK_SALT}${gas_tank_init_code#0x}"', deploy_gas_tank)
        self.assertIn("cast call --rpc-url", deploy_gas_tank)
        self.assertIn("--create \"${gas_tank_creation_code}\"", deploy_gas_tank)
        self.assertIn("FOUNDRY_EVM_VERSION=cancun forge inspect --no-metadata", deploy_gas_tank)
        self.assertIn("existing_gas_tank_runtime", deploy_gas_tank)
        self.assertIn("expected_gas_tank_runtime", deploy_gas_tank)

    def test_zksys_bootstrap_attests_factory_and_tank_before_burn_role(self) -> None:
        bootstrap = (
            REPO_ROOT / "scripts" / "gateway-launch" / "zksys-l2-bootstrap.sh"
        ).read_text(encoding="utf-8")

        # SYSCOIN: a nonempty-code check is insufficient on custom genesis.
        # Pin both Arachnid runtime bytes and their independently fixed hash.
        self.assertIn(
            "ARACHNID_CREATE2_RUNTIME=0x7fffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffe03601600081602082378035828234f58015156039578182fd5b8082525050506014600cf3",
            bootstrap,
        )
        self.assertIn(
            "ARACHNID_CREATE2_RUNTIME_HASH=0x2fa86add0aed31f33a762c9d88e807c475bd51d0f52bd0955754b2608f7e4989",
            bootstrap,
        )
        self.assertIn('actual_runtime="$(rpc_code "${address}")"', bootstrap)
        self.assertIn('actual_runtime_hash="$(cast keccak "${actual_runtime}")"', bootstrap)
        self.assertIn("does not match the exact canonical bytecode", bootstrap)
        self.assertLess(
            bootstrap.index("require_create2_deployer\n"),
            bootstrap.index('deploy_create2 "zkSYS proxy admin"'),
        )
        manifest_bind = bootstrap.rindex("\nbind_zksys_l2_bootstrap_manifest\n")
        self.assertLess(
            bootstrap.index('ZKSYS_L2_GAS_TANK_ADDRESS="$('), manifest_bind
        )
        self.assertLess(manifest_bind, bootstrap.index("require_create2_deployer\n"))
        self.assertIn('"schema_version": 2', bootstrap)
        self.assertIn('"derived_addresses":', bootstrap)
        self.assertIn('"init_code_hashes":', bootstrap)
        for identity in (
            "ZKSYS_L2_PROXY_ADMIN_ADDRESS",
            "ZKSYS_L2_TOKEN_ADDRESS",
            "ZKSYS_L2_REGISTRY_ADDRESS",
            "ZKSYS_L2_WEIGHT_REGISTRY_ADDRESS",
            "ZKSYS_L2_ISSUER_ADDRESS",
            "ZKSYS_L2_STAKING_VAULT_ADDRESS",
            "ZKSYS_L2_GAS_TANK_ADDRESS",
        ):
            self.assertIn(f'    "{identity}",', bootstrap)

        # SYSCOIN: pin the constructor-specific compiler output independently
        # of its CREATE2 address as part of the app/VK release surface.
        self.assertIn(
            f"PUBLISHED_GAS_TANK_INIT_CODE_HASH={PUBLISHED_GAS_TANK_INIT_CODE_HASH}",
            bootstrap,
        )
        self.assertIn(
            f"PUBLISHED_GAS_TANK_RUNTIME_HASH={PUBLISHED_GAS_TANK_RUNTIME_HASH}",
            bootstrap,
        )
        self.assertIn(f"PUBLISHED_GAS_TANK_ADDRESS={PUBLISHED_GAS_TANK}", bootstrap)
        init_hash_check = bootstrap.index(
            'gl_die "derived gas tank init-code hash ${gas_tank_init_code_hash}'
        )
        address_check = bootstrap.index(
            'gl_die "derived gas tank ${ZKSYS_L2_GAS_TANK_ADDRESS}'
        )
        self.assertLess(init_hash_check, address_check)

        # Constructor execution specializes immutable token references. Its
        # byte-for-byte runtime attestation must precede the first burn grant.
        runtime_build = bootstrap.index('expected_gas_tank_runtime="$(')
        canonical_runtime_check = bootstrap.index(
            'gl_die "derived gas tank runtime hash ${expected_gas_tank_runtime_hash}'
        )
        tank_deploy = bootstrap.index('deploy_create2 "zkSYS gas tank"')
        tank_attestation = bootstrap.index('assert_exact_runtime \\\n  "zkSYS gas tank"')
        burner_grant = bootstrap.index(
            'send_l2 "${ZKSYS_L2_TOKEN_ADDRESS}" "grantRole(bytes32,address)" "${BURNER_ROLE}"'
        )
        self.assertLess(runtime_build, tank_deploy)
        self.assertLess(runtime_build, canonical_runtime_check)
        self.assertLess(canonical_runtime_check, tank_deploy)
        self.assertLess(tank_deploy, tank_attestation)
        self.assertLess(tank_attestation, burner_grant)

        # Execute the production assertion helper with mocked RPC/hash output:
        # wrong bytecode and wrong hashes must both fail closed.
        function_start = bootstrap.index("assert_exact_runtime() {")
        function_end = bootstrap.index(
            "\n}\n\nrequire_create2_deployer()", function_start
        ) + len("\n}")
        assertion_function = bootstrap[function_start:function_end]
        probe = f"""
set -euo pipefail
gl_to_lower() {{ printf '%s' "${{1:-}}" | tr '[:upper:]' '[:lower:]'; }}
gl_die() {{ echo "$*" >&2; exit 1; }}
rpc_code() {{ printf '%s\n' "$MOCK_RUNTIME"; }}
cast() {{
  [ "$1" = "keccak" ] || exit 90
  printf '%s\n' "$MOCK_HASH"
}}
{assertion_function}
assert_exact_runtime "test tank" 0x1234 0xaaaa 0xhash
"""

        def run_probe(runtime: str, runtime_hash: str) -> subprocess.CompletedProcess[str]:
            return subprocess.run(
                ["bash", "-c", probe],
                text=True,
                capture_output=True,
                env={
                    **os.environ,
                    "MOCK_RUNTIME": runtime,
                    "MOCK_HASH": runtime_hash,
                },
                check=False,
            )

        self.assertEqual(run_probe("0xaaaa", "0xhash").returncode, 0)
        wrong_runtime = run_probe("0xbbbb", "0xhash")
        self.assertNotEqual(wrong_runtime.returncode, 0)
        self.assertIn("does not match the exact canonical bytecode", wrong_runtime.stderr)
        wrong_hash = run_probe("0xaaaa", "0xwrong")
        self.assertNotEqual(wrong_hash.returncode, 0)
        self.assertIn("runtime hash", wrong_hash.stderr)

    def test_direct_mutators_bind_identity_and_bridge_check_cannot_be_disabled(
        self,
    ) -> None:
        launch_dir = REPO_ROOT / "scripts" / "gateway-launch"
        funder = (launch_dir / "fund-wallets.sh").read_text(encoding="utf-8")
        generator = (launch_dir / "generate-os-server-configs.sh").read_text(
            encoding="utf-8"
        )
        common = (launch_dir / "_common.sh").read_text(encoding="utf-8")
        deploy = (launch_dir / "gateway-deploy-l1.sh").read_text(encoding="utf-8")

        for script in (funder, generator):
            self.assertIn("gl_resolve_required_source_pins", script)
            self.assertIn("gl_checkpoint_assert_fingerprint_matches", script)
            self.assertIn("gl_bind_gateway_launch_context", script)
        self.assertIn("address_for_private_key", common)
        self.assertIn("missing private key for required server signer", common)
        self.assertLess(
            common.index("server_signer_roles ="),
            common.index("if check_only:", common.index("server_signer_roles =")),
        )

        function = deploy[deploy.index("deploy_zksys_l1_registry_bridge() {") :]
        disabled = function.index(
            'if [ "${ZKSYS_L1_REGISTRY_BRIDGE_CHECK_ONLY}" = true ]'
        )
        attestation = function.index("actual_proxy_admin_owner=", disabled)
        self.assertIn("verifying the persisted bridge", function[disabled:attestation])
        self.assertIn("return 0", function[disabled:attestation])
        self.assertLess(disabled, attestation)

    def test_l1_ecosystem_recovery_stays_inside_zkstack_resume(self) -> None:
        deploy = (
            REPO_ROOT / "scripts" / "gateway-launch" / "gateway-deploy-l1.sh"
        ).read_text(encoding="utf-8")
        resumable = deploy[
            deploy.index("run_ecosystem_init() {") : deploy.index(
                "\necosystem_contracts_ready() {"
            )
        ]
        retry_start = deploy.index(
            'if [ "${ecosystem_already_ready}" != true ]; then'
        )
        retry_loop = deploy[
            retry_start : deploy.index(
                "\ndeploy_zksys_l1_registry_bridge", retry_start
            )
        ]

        self.assertIn("zkstack ecosystem init", resumable)
        self.assertIn("resume_args+=(--resume)", resumable)
        self.assertEqual(retry_loop.count('run_ecosystem_init "${resume_attempt}"'), 1)
        self.assertIn('GATEWAY_ECOSYSTEM_RESUME_FIRST:=false', deploy)
        self.assertIn('if [ "${attempt}" -gt 1 ]', retry_loop)
        self.assertNotIn("wait_for_deployer_nonce_sync", retry_loop)
        self.assertNotIn("run_ecosystem_init_resume", deploy)
        self.assertNotIn("extract_l1_contracts_dir_from_log", deploy)
        self.assertNotIn("LAST_L1_CONTRACTS_DIR", deploy)
        self.assertNotIn(
            "forge script deploy-scripts/ecosystem/DeployL1CoreContracts.s.sol",
            deploy,
        )
        # The external-signer nonce drain remains scoped to its direct
        # DeployErc20 retry path.
        self.assertEqual(deploy.count("      wait_for_deployer_nonce_sync\n"), 1)

        normal = (
            REPO_ROOT / "scripts" / "gateway-launch" / "run-gateway-launch.sh"
        ).read_text(encoding="utf-8")
        repair = (
            REPO_ROOT / "scripts" / "gateway-launch" / "gateway-launch-repair.sh"
        ).read_text(encoding="utf-8")
        self.assertIn("GATEWAY_ECOSYSTEM_RESUME_FIRST=false", normal)
        self.assertIn("REPAIR_PRIOR_STATUS=", repair)
        self.assertIn("blocked | in_progress | passed)", repair)
        self.assertIn("GATEWAY_ECOSYSTEM_RESUME_FIRST=true", repair)
        self.assertIn("pending)", repair)

    def test_launcher_never_deletes_runtime_databases_implicitly(self) -> None:
        launch_dir = REPO_ROOT / "scripts" / "gateway-launch"
        production = "\n".join(
            path.read_text(encoding="utf-8")
            for path in sorted(launch_dir.glob("*.sh"))
        )
        self.assertNotIn("gl_clear_os_server_chain_db", production)
        for line in production.splitlines():
            if "rm -rf" in line:
                self.assertNotIn("/db", line)
                self.assertNotIn("os-server-configs", line)

        normal = (launch_dir / "run-gateway-launch.sh").read_text(encoding="utf-8")
        repair = (launch_dir / "gateway-launch-repair.sh").read_text(
            encoding="utf-8"
        )
        self.assertIn(
            'step_l1_ecosystem_deployed() {\n'
            '  # SYSCOIN: Never mutate runtime DB state',
            normal,
        )
        self.assertIn('gl.l1_ecosystem_deployed)', repair)
        self.assertIn('"${SCRIPT_DIR}/gateway-deploy-l1.sh"', repair)

    def test_multivm_build_fails_closed_on_unpatched_execution_source(self) -> None:
        build_rs = (REPO_ROOT / "lib" / "multivm" / "build.rs").read_text(
            encoding="utf-8"
        )
        self.assertIn("verify_syscoin_source", build_rs)
        self.assertIn("require_source_sha256", build_rs)
        self.assertIn("expected one canonical forward_system source", build_rs)
        self.assertIn(PUBLISHED_EDGE_SOURCE_SHA256, build_rs)
        self.assertIn(PUBLISHED_GAS_TANK_SOURCE_SHA256, build_rs)
        self.assertIn("syscoin_gas_tank.rs", build_rs)
        self.assertIn("scripts/cargo-with-patched-zksync-os.sh", build_rs)


class EraAttestationStaticTests(unittest.TestCase):
    def test_era_helper_attests_canonical_source_patch_and_excludes_verifier_artifacts(
        self,
    ) -> None:
        helper = (
            REPO_ROOT / "scripts" / "apply-era-contracts-syscoin-patch.sh"
        ).read_text(encoding="utf-8")
        patch = (
            REPO_ROOT / "scripts" / "patches" / "era-contracts-syscoin.patch"
        ).read_text(encoding="utf-8")

        self.assertIn("apply --reverse --check --recount", helper)
        self.assertNotIn("base_patch_core_applied", helper)
        for expected in (
            'EXPECTED_BASE_COMMIT="8fb7c29a4e3174335c6480b23f57822e054f9d5f"',
            'EXPECTED_BASE_TREE="acdd11e5bb7787d9df2306f6a1dc96bf92e67f53"',
            'EXPECTED_NESTED_SHA="e554ae64ec150c47d6f17786e7f4aacebc7bf945"',
            'EXPECTED_PATCH_SIZE="1419459"',
            'EXPECTED_PATCH_SHA256="b1a2d9705d0ba03f3a91ddf48d0160a1d7258dfa9ac6d6e5c1ab8854426b88b9"',
            'EXPECTED_PATCH_PATH_COUNT="59"',
            'EXPECTED_PATCH_PATHS_SHA256="d520d73b6b6b1001f4e8a845e2aa6e1fa04256c38d16cdb223b0643868fee5ff"',
            f'EXPECTED_PATCHED_TREE="{PUBLISHED_ERA_PATCHED_TREE}"',
            'STOCK_APP_VK_HASH="0x9f7576b911e7d3f528d49f894208682c81800814db9e3beac7fc3b1c4d626e7a"',
            "uint32 internal constant CANONICAL_ZKSYNC_OS_VERIFIER_VERSION = 8;",
            "if (version != CANONICAL_ZKSYNC_OS_VERIFIER_VERSION) {",
            "_verifySyscoinEdgeDARefs(_newBatch.edgeDARefsInput, _newBatch.edgeDARefsRoot);",
            "uint256 totalRefs;",
            "if (totalRefs > SYSCOIN_DA_MAX_REFS_PER_BATCH) {",
            "_l1ChainId != SYSCOIN_MAINNET_CHAIN_ID &&",
            "constructor(GatewayVerifiersDeployerConfig memory _config, uint256 _l1ChainId)",
            "return abi.encode(_fflonk, _plonk, _owner, _l1ChainId);",
            "? abi.encode(verifiersConfig, config.l1ChainId)",
            "testGatewayVerifierDeployerZKsyncOSRejectsSyscoinMainnetRootForTestnetRoute",
            "testGatewayVerifierDeployerZKsyncOSRejectsEthereumMainnetRootForTestnetRoute",
            '*.sol | *.rs | *.toml | *.gitignore)',
            'done <<< "${PATCH_PATHS}"',
            "postimage manifest does not exactly match the canonical patch path set",
            "canonical source patch unexpectedly deletes an upstream path",
            "fflonkVerifiers[CANONICAL_ZKSYNC_OS_VERIFIER_VERSION] = _fflonkVerifier;",
            "function replaceVerifier(uint32 version, IVerifier newPlonkVerifier) external override onlyOwner",
            "function addVerifier(",
            "function removeVerifier(",
            "stock verifier artifact rejected",
            "canonical V8 VK regeneration required",
            "no app-bound security100 verifier hashes are approved",
            "SYSCOIN_EDGE_DA_RELAY_ADDRESS = 0x758b06cDA80BDD016F79AFd0df1A984039067A21",
            "actualRelayCodeHash != SYSCOIN_EDGE_DA_RELAY_RUNTIME_HASH",
            "_validateSyscoinEdgeDARelayArtifact();",
            "actualInitCodeHash != SYSCOIN_EDGE_DA_RELAY_INIT_CODE_HASH",
            "actualRuntimeHash != SYSCOIN_EDGE_DA_RELAY_RUNTIME_HASH",
            "pub const SYSCOIN_EDGE_DA_RELAY_ADDRESS: Address",
            "pub const INITIAL_CONTRACTS: [(Address, ContractDeployment); 23] = [",
            '"SyscoinRelayedSLDAValidator",',
            "canonical zkOS genesis must contain only the pinned 41 contracts",
            f'"genesis_root": "{PUBLISHED_ERA_GENESIS_ROOT}"',
            f'"{PUBLISHED_ERA_GENESIS_ROOT.removeprefix("0x")}"',
            "0xf537449b2ae8774f0073e37e622c7b69744cfc985baf8236be2c82411a161191;",
        ):
            self.assertIn(expected, helper)

        patch_paths = sorted(
            line.split(" b/", 1)[1]
            for line in patch.splitlines()
            if line.startswith("diff --git a/")
        )
        self.assertEqual(len(patch_paths), 59)
        self.assertEqual(
            hashlib.sha256(
                "".join(f"{path}\n" for path in patch_paths).encode("utf-8")
            ).hexdigest(),
            "d520d73b6b6b1001f4e8a845e2aa6e1fa04256c38d16cdb223b0643868fee5ff",
        )
        manifest_body = helper.split(
            "done <<'SYSCOIN_POSTIMAGE_MANIFEST'\n", 1
        )[1].split("\nSYSCOIN_POSTIMAGE_MANIFEST\n", 1)[0]
        manifest_entries = [line.split(maxsplit=2) for line in manifest_body.splitlines()]
        self.assertEqual(len(manifest_entries), 59)
        self.assertEqual([entry[2] for entry in manifest_entries], patch_paths)
        for size, digest, path in manifest_entries:
            self.assertGreater(int(size), 0, path)
            self.assertEqual(len(digest), 64, path)

        for forbidden_envelope in (
            "deleted file mode ",
            "GIT binary patch",
            "Binary files ",
        ):
            self.assertNotIn(forbidden_envelope, patch)

        for index, path in enumerate(patch_paths):
            if not path.endswith((".sol", ".toml", ".gitignore")):
                continue
            start = patch.index(f"diff --git a/{path} b/{path}")
            end = (
                patch.index("\ndiff --git a/", start + 1)
                if index + 1 < len(patch_paths)
                else len(patch)
            )
            self.assertIn("SYSCOIN:", patch[start:end], path)

        pending_gate = helper.rindex("\n  verify_verifier_artifacts_pending\n")
        self.assertLess(pending_gate, helper.index("submodule sync --recursive\n"))
        self.assertIn("submodule update --init --recursive", helper)
        self.assertLess(
            pending_gate,
            helper.index('apply --recount --unidiff-zero --whitespace'),
        )
        exact_tree_gate = helper.rindex("\nverify_worktree_postimage_tree\n")
        self.assertGreater(
            exact_tree_gate,
            helper.index('apply --recount --unidiff-zero --whitespace'),
        )
        self.assertLess(
            exact_tree_gate,
            helper.rindex("\necho \"Canonical Syscoin Era source patch is exact:"),
        )

        for path, digest in (
            (
                "configs/genesis/zksync-os/latest.json",
                PUBLISHED_ERA_GENESIS_SHA256,
            ),
            (
                "da-contracts/contracts/SyscoinL1DAValidatorZKsyncOS.sol",
                "24fcd082bee0ef29de5b4bd09b8e493a1bb1ef6759235ec71120668a19c417f4",
            ),
            (
                "l1-contracts/contracts/state-transition/data-availability/SyscoinRelayedSLDAValidator.sol",
                "c7f49220b06784bd67d73166fd9fb4e2329d7699d493b4342dfbcabfde683a10",
            ),
            (
                "l1-contracts/contracts/state-transition/data-availability/SyscoinRollupDAManager.sol",
                "a7a77cf790b20e91573ab5d5c30458b4aa1dc06f550e211d74e7f4afb448c04f",
            ),
            (
                "l1-contracts/contracts/state-transition/verifiers/ZKsyncOSDualVerifier.sol",
                "c9b04c90afedd8503fa3a27944b8b3446cd445213d0beac37410e857d8b63d77",
            ),
            (
                "l1-contracts/contracts/state-transition/verifiers/ZKsyncOSTestnetVerifier.sol",
                "99b2f630ccb303dc130e6010ae91ab2c462218a4cc813602ed307b9f05b95fe3",
            ),
            (
                "l1-contracts/contracts/state-transition/chain-deps/gateway-ctm-deployer/GatewayCTMDeployerVerifiersZKsyncOS.sol",
                "e5d289cbcb0bbd9f77b7a89f01fba3cc6bcb07f847754517c56d60b2f6c194bd",
            ),
            (
                "l1-contracts/deploy-scripts/gateway/GatewayCTMDeployerHelper.sol",
                "22d5c9ed58e078d7984757c6c4d023bc0afb64f4f8aff240a576783f92b96fb1",
            ),
            (
                "tools/zksync-os-genesis-gen/src/consts.rs",
                "2d470cd020bad4178adc1cd12889693e235df86103be24bf25549ac411613b6d",
            ),
            (
                "l1-contracts/contracts/common/SyscoinEdgeDARelayDeployment.sol",
                "c2138ea375da32973ecf228abd97adcf0c7099b48a38162bf9a223d81a7361b7",
            ),
            (
                "l1-contracts/contracts/state-transition/chain-deps/facets/Admin.sol",
                "b8afdf177f76cb229a5a98c3367775d3def34e7d7868b567bd08efb742d0698e",
            ),
        ):
            self.assertIn(path, helper)
            self.assertIn(digest, helper)

        for pending_verifier_artifact in (
            "l1-contracts/contracts/state-transition/verifiers/ZKsyncOSVerifierPlonk.sol",
            "tools/verifier-gen/data/ZKsyncOS_plonk_scheduler_key.json",
        ):
            self.assertIn(pending_verifier_artifact, helper)
            self.assertNotIn(f"diff --git a/{pending_verifier_artifact}", patch)

        # SYSCOIN: preserve exact pinned-upstream build/test, FFLONK, generator,
        # deployment-CI, and review-tool bytes outside the downstream patch.
        for size, digest, retained_upstream_path in (
            (
                4895,
                "cfa792fc502364d12c855c02724ceef0843aa193b711630fb87326e16197e4bd",
                "l1-contracts/foundry.toml",
            ),
            (
                58881,
                "4e272ef47b1ba6fbbdd546e8da4b97a130463b54ad48eb03431dfb59e6e44b2e",
                "l1-contracts/test/foundry/l1/unit/concrete/BatchProcessing/Committing.t.sol",
            ),
            (
                77746,
                "9308b1850d4197bd7b6a59cc35029f51b94ffce76f5951848669fd9424a07d48",
                "l1-contracts/contracts/state-transition/verifiers/ZKsyncOSVerifierFflonk.sol",
            ),
            (
                1920,
                "a1d093cf2bb0f5331c4a6bbf0e40d5f4888cc850324e8b9e406bde6686f07f77",
                "tools/verifier-gen/data/ZKsyncOS_fflonk_scheduler_key.json",
            ),
            (
                75842,
                "b2b292b85a7f676d18bee0a0e98af3dbbd4bc05bcaccef1b8260e195652db647",
                "tools/verifier-gen/data/fflonk_verifier_contract_template.txt",
            ),
            (
                5122,
                "7f015b5fbaebf4e21357c56db3282507256b0eb0bb44ed33f49b3d4be0c4c098",
                "tools/verifier-gen/src/fflonk.rs",
            ),
            (
                5962,
                "f63ab6897dd986a6f5f36e1759d1d032f573c4aea9fcba42bc45a33c85df0e65",
                "tools/verifier-gen/src/main.rs",
            ),
            (
                1812,
                "29736c7e0ad4a2e8e5b67e4f2de3064a05aef12db19783bb08635fbb2ed43cdc",
                "tools/verifier-gen/README.md",
            ),
            (
                18272,
                "315661d42cad03e6dbddf995f5f7f9fd3a5716518274d3c620e37922cae3490c",
                ".github/workflows/l1-contracts-ci.yaml",
            ),
            (
                2656,
                "e4c067ed467721e54b967fc57d1342f523c8690f7a5bb5726b348a449529c444",
                ".github/workflows/slither.yaml",
            ),
            (
                1510,
                "18c4ad86772fc5e41d8241d1a7b2dc5f510610d99af44325ddb0ce98977b39bf",
                ".prettierignore",
            ),
        ):
            self.assertIn(
                f"{size} {digest} {retained_upstream_path}", helper
            )
            self.assertNotIn(f"diff --git a/{retained_upstream_path}", patch)

        self.assertIn(
            '"contractName": "l1-contracts/ZKsyncOSVerifierFflonk"', helper
        )
        self.assertIn("ZKSYNC_OS_FFLONK_VERIFICATION_TYPE", helper)
        self.assertIn("fflonkVerifiers[verifierVersion].verify", helper)

        self.assertIn(
            "diff --git a/da-contracts/contracts/SyscoinL1DAValidatorZKsyncOS.sol",
            patch,
        )
        self.assertIn(
            "diff --git a/l1-contracts/contracts/state-transition/data-availability/SyscoinRelayedSLDAValidator.sol",
            patch,
        )
        self.assertIn(
            "diff --git a/l1-contracts/contracts/common/SyscoinEdgeDARelayDeployment.sol",
            patch,
        )
        self.assertIn(
            "diff --git a/l1-contracts/script-config/syscoin-edge-da-relay-v1.json",
            patch,
        )
        self.assertIn(
            "diff --git a/tools/zksync-os-genesis-gen/src/consts.rs",
            patch,
        )
        self.assertIn(
            "diff --git a/configs/genesis/zksync-os/latest.json",
            patch,
        )
        self.assertIn(
            "pub const SYSCOIN_EDGE_DA_RELAY_ADDRESS: Address",
            patch,
        )
        self.assertIn(
            "pub const INITIAL_CONTRACTS: [(Address, ContractDeployment); 23] = [",
            patch,
        )
        self.assertIn("_validateSyscoinEdgeDARelayArtifact();", patch)
        self.assertIn(
            f'"genesis_root": "{PUBLISHED_ERA_GENESIS_ROOT}"',
            patch,
        )
        self.assertIn(
            "canonical zkOS genesis must contain only the pinned 41 contracts",
            patch,
        )
        self.assertIn(
            "0xf537449b2ae8774f0073e37e622c7b69744cfc985baf8236be2c82411a161191;",
            patch,
        )
        self.assertIn(
            "vm.etch(SYSCOIN_EDGE_DA_RELAY_ADDRESS, type(SyscoinRelayedSLDAValidator).runtimeCode);",
            patch,
        )
        self.assertNotIn("syscoinEdgeDARelayCalldata", patch)
        self.assertNotIn(
            "diff --git a/l1-contracts/test/foundry/l1/unit/concrete/BatchProcessing/Committing.t.sol",
            patch,
        )

    # SYSCOIN: Pending VKs may materialize Era sources only for the explicit
    # fake-prover route, never through a chain-specific GPU override.
    def test_pending_v8_source_materialization_requires_exact_mock_modes(self) -> None:
        helper = (
            REPO_ROOT / "scripts" / "apply-era-contracts-syscoin-patch.sh"
        ).read_text(encoding="utf-8")
        function_start = helper.index("pending_v8_mock_source_mode_enabled() {")
        function_end = helper.index("\n}\n", function_start) + len("\n}\n")
        function_source = helper[function_start:function_end]
        probe = (
            function_source
            + "\nif pending_v8_mock_source_mode_enabled; then printf enabled; "
            "else printf blocked; fi\n"
        )

        def run_gate(overrides: dict[str, str]) -> str:
            env = os.environ.copy()
            for name in (
                "PROVER_MODE",
                "SYSCOIN_ZKSYNC_OS_MOCK_VERIFIER",
                "GATEWAY_PROVER_MODE",
                "EDGE_PROVER_MODE",
                "L1_NETWORK",
                "L1_CHAIN_ID",
            ):
                env.pop(name, None)
            env.update(overrides)
            result = subprocess.run(
                ["bash", "-c", probe],
                check=False,
                capture_output=True,
                text=True,
                env=env,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            return result.stdout

        mock_modes = {
            "PROVER_MODE": "no-proofs",
            "SYSCOIN_ZKSYNC_OS_MOCK_VERIFIER": "true",
        }
        self.assertEqual(run_gate({}), "blocked")
        self.assertEqual(run_gate({"PROVER_MODE": "no-proofs"}), "blocked")
        self.assertEqual(
            run_gate({"SYSCOIN_ZKSYNC_OS_MOCK_VERIFIER": "true"}), "blocked"
        )
        self.assertEqual(run_gate(mock_modes), "blocked")
        self.assertEqual(
            run_gate(
                {
                    **mock_modes,
                    "L1_NETWORK": "localhost",
                    "L1_CHAIN_ID": "31337",
                }
            ),
            "enabled",
        )
        self.assertEqual(
            run_gate({**mock_modes, "GATEWAY_PROVER_MODE": "gpu"}), "blocked"
        )
        self.assertEqual(
            run_gate({**mock_modes, "EDGE_PROVER_MODE": "gpu"}), "blocked"
        )
        self.assertEqual(
            run_gate({**mock_modes, "L1_NETWORK": "mainnet"}), "blocked"
        )
        self.assertEqual(run_gate({**mock_modes, "L1_CHAIN_ID": "1"}), "blocked")
        self.assertEqual(run_gate({**mock_modes, "L1_CHAIN_ID": "57"}), "blocked")
        self.assertEqual(
            run_gate(
                {
                    **mock_modes,
                    "L1_NETWORK": "devnet",
                    "L1_CHAIN_ID": "31337",
                }
            ),
            "blocked",
        )
        self.assertEqual(
            run_gate(
                {
                    **mock_modes,
                    "L1_NETWORK": "localhost",
                    "L1_CHAIN_ID": "5700",
                }
            ),
            "blocked",
        )
        self.assertEqual(
            run_gate(
                {
                    **mock_modes,
                    "L1_NETWORK": "tanenbaum",
                    "L1_CHAIN_ID": "5700",
                }
            ),
            "enabled",
        )

    def test_era_keygen_workflow_attests_current_source_inputs(self) -> None:
        helper_path = REPO_ROOT / "scripts" / "apply-era-contracts-syscoin-patch.sh"
        patch_path = REPO_ROOT / "scripts" / "patches" / "era-contracts-syscoin.patch"
        workflow = (
            REPO_ROOT / ".github" / "workflows" / "syscoin-v32-v8-keygen.yml"
        ).read_text(encoding="utf-8")
        helper = helper_path.read_bytes()
        patch = patch_path.read_bytes()

        for expected in (
            f'ERA_PATCH_SIZE: "{len(patch)}"',
            f"ERA_PATCH_SHA256: {hashlib.sha256(patch).hexdigest()}",
            'ERA_PATCH_PATH_COUNT: "59"',
            "ERA_PATCH_PATHS_SHA256: "
            "d520d73b6b6b1001f4e8a845e2aa6e1fa04256c38d16cdb223b0643868fee5ff",
            f"ERA_SOURCE_PATCHED_TREE: {PUBLISHED_ERA_PATCHED_TREE}",
            "ERA_GENESIS_TOOLCHAIN: nightly-2026-01-22",
            'ERA_GENESIS_SIZE: "557518"',
            f"ERA_GENESIS_SHA256: {PUBLISHED_ERA_GENESIS_SHA256}",
            f'ERA_GENESIS_ROOT: "{PUBLISHED_ERA_GENESIS_ROOT}"',
            'ERA_GENESIS_CONTRACT_COUNT: "41"',
            'ERA_CONTRACT_HASH_ENTRY_COUNT: "278"',
            f'ERA_HELPER_SIZE: "{len(helper)}"',
            f"ERA_HELPER_SHA256: {hashlib.sha256(helper).hexdigest()}",
            "zksync_os_fflonk_artifact_or_deployer_present: true",
            "zksync_os_fflonk_proof_route_present: false",
            'canonical_genesis_snapshot="${WORK_DIR}/canonical-zksync-os-genesis.json"',
            'generated_genesis="${WORK_DIR}/generated-zksync-os-genesis.json"',
            'cp "${canonical_genesis}" "${canonical_genesis_snapshot}"',
            'cmp --silent "${canonical_genesis_snapshot}" "${canonical_genesis}"',
            'cmp --silent "${canonical_genesis_snapshot}" "${generated_genesis}"',
            'cargo "+${ERA_GENESIS_TOOLCHAIN}" run',
            "--bin zksync-os-genesis-gen",
        ):
            self.assertIn(expected, workflow)


class CanonicalFixtureGateStaticTests(unittest.TestCase):
    def test_canonical_v32_fixture_is_blocked_until_atomic_v8_regeneration(
        self,
    ) -> None:
        marker_name = "CANONICAL_V8_REGENERATION_REQUIRED"
        marker = REPO_ROOT / "local-chains" / "v32.0" / marker_name
        self.assertTrue(marker.is_file())
        marker_text = marker.read_text(encoding="utf-8")
        self.assertIn("DO NOT LAUNCH THIS FIXTURE", marker_text)
        self.assertIn("Execution V7, Proving V8", marker_text)
        self.assertIn(PUBLISHED_ZKSYNC_OS_PATCHED_TREE, marker_text)
        self.assertIn("zero values in the server and keygen workflow", marker_text)

        guarded_paths = (
            REPO_ROOT / "run_local.sh",
            REPO_ROOT / "integration-tests" / "src" / "config.rs",
            REPO_ROOT / "integration-tests" / "build.rs",
            REPO_ROOT / "scripts" / "gateway-launch" / "_common.sh",
            REPO_ROOT / ".github" / "scripts" / "test-configs.sh",
            REPO_ROOT / ".github" / "workflows" / "spec-tests.yaml",
        )
        for path in guarded_paths:
            with self.subTest(path=path):
                self.assertIn(marker_name, path.read_text(encoding="utf-8"))

        build_script = (REPO_ROOT / "integration-tests" / "build.rs").read_text(
            encoding="utf-8"
        )
        self.assertIn('join("versions.yaml").is_file()', build_script)
        self.assertIn("Ignore local materializations left behind", build_script)

        self.assertFalse(
            (REPO_ROOT / "local-chains" / "v31.0" / "versions.yaml").exists()
        )
        self.assertFalse(
            (REPO_ROOT / "local-chains" / "v32.0" / "versions.yaml").exists()
        )

    def test_pending_v8_mock_source_pins_require_the_full_testnet_gate(self) -> None:
        common = REPO_ROOT / "scripts" / "gateway-launch" / "_common.sh"
        command = r'''
source "$COMMON"
gl_resolve_required_source_pins
printf '%s|%s\n' "$REQUIRED_ZKSTACK_CLI_SHA" "$REQUIRED_CONTRACTS_SHA"
'''

        def run_gate(overrides: dict[str, str]) -> subprocess.CompletedProcess[str]:
            env = os.environ.copy()
            for name in (
                "PROTOCOL_VERSION",
                "PROVER_MODE",
                "GATEWAY_PROVER_MODE",
                "EDGE_PROVER_MODE",
                "SYSCOIN_ZKSYNC_OS_MOCK_VERIFIER",
                "L1_NETWORK",
                "L1_CHAIN_ID",
                "REQUIRED_ZKSTACK_CLI_SHA",
                "REQUIRED_CONTRACTS_SHA",
            ):
                env.pop(name, None)
            env.update({"COMMON": str(common), **overrides})
            return subprocess.run(
                ["bash", "-c", command],
                check=False,
                capture_output=True,
                text=True,
                env=env,
            )

        exact = {
            "PROTOCOL_VERSION": "v32.0",
            "PROVER_MODE": "no-proofs",
            "GATEWAY_PROVER_MODE": "no-proofs",
            "EDGE_PROVER_MODE": "no-proofs",
            "SYSCOIN_ZKSYNC_OS_MOCK_VERIFIER": "true",
            "L1_NETWORK": "tanenbaum",
            "L1_CHAIN_ID": "5700",
        }
        expected = f"{PENDING_V8_MOCK_ZKSTACK_SHA}|{PENDING_V8_MOCK_CONTRACTS_SHA}"
        accepted = run_gate(exact)
        self.assertEqual(accepted.returncode, 0, accepted.stderr)
        self.assertEqual(accepted.stdout.strip(), expected)

        localhost = run_gate(
            {**exact, "L1_NETWORK": "localhost", "L1_CHAIN_ID": "31337"}
        )
        self.assertEqual(localhost.returncode, 0, localhost.stderr)
        self.assertEqual(localhost.stdout.strip(), expected)

        rejected = (
            {k: v for k, v in exact.items() if k != "SYSCOIN_ZKSYNC_OS_MOCK_VERIFIER"},
            {**exact, "PROVER_MODE": "gpu"},
            {**exact, "GATEWAY_PROVER_MODE": "gpu"},
            {**exact, "EDGE_PROVER_MODE": "gpu"},
            {**exact, "L1_NETWORK": "mainnet", "L1_CHAIN_ID": "57"},
            {**exact, "L1_NETWORK": "localhost", "L1_CHAIN_ID": "5700"},
            {
                **exact,
                "REQUIRED_ZKSTACK_CLI_SHA": "1" * 40,
                "REQUIRED_CONTRACTS_SHA": PENDING_V8_MOCK_CONTRACTS_SHA,
            },
            {
                **exact,
                "REQUIRED_ZKSTACK_CLI_SHA": PENDING_V8_MOCK_ZKSTACK_SHA,
                "REQUIRED_CONTRACTS_SHA": "2" * 40,
            },
            {
                **exact,
                "PROVER_MODE": "gpu",
                "REQUIRED_ZKSTACK_CLI_SHA": PENDING_V8_MOCK_ZKSTACK_SHA,
                "REQUIRED_CONTRACTS_SHA": PENDING_V8_MOCK_CONTRACTS_SHA,
            },
        )
        for candidate in rejected:
            with self.subTest(candidate=candidate):
                result = run_gate(candidate)
                self.assertNotEqual(result.returncode, 0)

    def test_gateway_identity_is_authenticated_before_edge_creation(self) -> None:
        launcher = (
            REPO_ROOT / "scripts" / "gateway-launch" / "run-gateway-launch.sh"
        ).read_text(encoding="utf-8")
        common = (
            REPO_ROOT / "scripts" / "gateway-launch" / "_common.sh"
        ).read_text(encoding="utf-8")
        lifecycle = (
            REPO_ROOT
            / "scripts"
            / "gateway-launch"
            / "_gateway_node_lifecycle.sh"
        ).read_text(encoding="utf-8")
        edge_create_helper = (
            REPO_ROOT
            / "scripts"
            / "gateway-launch"
            / "edge-chain-create-init.sh"
        ).read_text(encoding="utf-8")
        edge_migrate_helper = (
            REPO_ROOT
            / "scripts"
            / "gateway-launch"
            / "edge-chain-migrate-to-gateway.sh"
        ).read_text(encoding="utf-8")
        repair = (
            REPO_ROOT
            / "scripts"
            / "gateway-launch"
            / "gateway-launch-repair.sh"
        ).read_text(encoding="utf-8")

        settlement = launcher.index('"gl.gateway_settlement"')
        config_identity = launcher.index("gl_assert_gateway_config_identity", settlement)
        gateway_config = launcher.index('"gl.os_configs_gateway"', config_identity)
        gateway_start = launcher.index(
            "\nstart_gateway_for_migration || exit $?\n", gateway_config
        )
        edge_create = launcher.index('"gl.edge_chain_inited"', gateway_start)
        self.assertLess(settlement, config_identity)
        self.assertLess(config_identity, gateway_config)
        self.assertLess(gateway_config, gateway_start)
        self.assertLess(gateway_start, edge_create)

        start_function = lifecycle.index("start_gateway_for_migration()")
        owned_pid = lifecycle.index("GATEWAY_NODE_PID=$!", start_function)
        build_prebuilt = lifecycle.index('"${chain_name}" -- build-prebuilt', start_function)
        post_build_port_check = lifecycle.index(
            "Gateway RPC became reachable while preparing this launch",
            build_prebuilt,
        )
        background_start = lifecycle.index(
            'nohup bash "${start_script}"', post_build_port_check
        )
        first_attestation = lifecycle.index(
            'gl_assert_gateway_runtime_identity "${GATEWAY_NODE_PID}" true "${owned_gateway_rpc}"',
            owned_pid,
        )
        first_listener_check = lifecycle.index(
            'gl_assert_gateway_listener_owned_by_pid "${GATEWAY_NODE_PID}" "${owned_gateway_rpc}"',
            owned_pid,
        )
        first_listener_recheck = lifecycle.index(
            'gl_assert_gateway_listener_owned_by_pid "${GATEWAY_NODE_PID}" "${owned_gateway_rpc}"',
            first_attestation,
        )
        self.assertLess(build_prebuilt, post_build_port_check)
        self.assertLess(post_build_port_check, background_start)
        self.assertLess(background_start, owned_pid)
        self.assertLess(owned_pid, first_attestation)
        self.assertLess(first_listener_check, first_attestation)
        self.assertLess(first_attestation, first_listener_recheck)
        self.assertIn(
            'kill -0 "${GATEWAY_NODE_PID}"',
            lifecycle[start_function:first_attestation],
        )
        self.assertIn(
            'gl_assert_gateway_runtime_identity "${GATEWAY_NODE_PID}" false "${owned_gateway_rpc}"',
            lifecycle[start_function:owned_pid],
        )
        reuse_attestation = lifecycle.index(
            'gl_assert_gateway_runtime_identity "${GATEWAY_NODE_PID}" false "${owned_gateway_rpc}"',
            start_function,
        )
        reuse_listener_check = lifecycle.rindex(
            'gl_assert_gateway_listener_owned_by_pid "${GATEWAY_NODE_PID}" "${owned_gateway_rpc}"',
            start_function,
            reuse_attestation,
        )
        reuse_listener_recheck = lifecycle.index(
            'gl_assert_gateway_listener_owned_by_pid "${GATEWAY_NODE_PID}" "${owned_gateway_rpc}"',
            reuse_attestation,
        )
        self.assertLess(reuse_listener_check, reuse_attestation)
        self.assertLess(reuse_attestation, reuse_listener_recheck)
        self.assertIn(
            'export GATEWAY_RPC_URL="${owned_gateway_rpc}"', lifecycle
        )
        # SYSCOIN: the submitter RPC may verify an independently supplied pin,
        # but it must never manufacture the native-value recipient pin itself.
        self.assertNotIn("gl_export_gateway_wrapped_base_token_from_owned_rpc", common)
        self.assertNotIn("gl_export_gateway_wrapped_base_token_from_owned_rpc", lifecycle)
        self.assertIn(
            '["pgrep", "-u", str(os.geteuid()), "-f", "zksync-os-server"]',
            lifecycle,
        )
        self.assertIn(
            '["ps", "-ww", "-p", str(pid), "-o", "command="]', lifecycle
        )
        self.assertIn("if result.returncode == 1:", lifecycle)
        self.assertIn("if result.returncode != 0:", lifecycle)
        self.assertEqual(
            lifecycle.count("except (ProcessLookupError, PermissionError):"),
            4,
        )
        self.assertIn("PID exited during re-attestation", lifecycle)
        self.assertIn("PID exited during first attestation", lifecycle)
        self.assertIn(
            'export GATEWAY_RUNTIME_OWNER_PID="${GATEWAY_NODE_PID}"', lifecycle
        )
        self.assertIn(
            'local expected_owner_pid="${1:-${GATEWAY_RUNTIME_OWNER_PID:-}}"',
            common,
        )
        self.assertLess(
            edge_create_helper.index("gl_assert_gateway_runtime_identity"),
            edge_create_helper.index("zkstack chain create"),
        )
        pre_rewrite_owner_check = edge_create_helper.index(
            "gl_assert_existing_edge_chain_admin_safe_for_governor_reuse"
        )
        wallet_rewrite = edge_create_helper.index("python3 - \\", pre_rewrite_owner_check)
        rewrite_invocation_end = edge_create_helper.index("<<'PY'", wallet_rewrite)
        edge_init = edge_create_helper.index("zkstack chain init", wallet_rewrite)
        post_init_owner_check = edge_create_helper.index(
            "gl_assert_edge_chain_admin_owned_by_configured_governor", edge_init
        )
        self.assertLess(pre_rewrite_owner_check, wallet_rewrite)
        self.assertLess(wallet_rewrite, edge_init)
        self.assertLess(edge_init, post_init_owner_check)
        self.assertIn(
            "EDGE_WALLET_PATH",
            edge_create_helper[wallet_rewrite:rewrite_invocation_end],
        )
        self.assertGreater(
            edge_create_helper.index("gl_persist_wallet_file", wallet_rewrite),
            wallet_rewrite,
        )
        self.assertIn("edge_chain_created=false", edge_create_helper)
        self.assertIn(
            'gl_assert_existing_edge_chain_admin_safe_for_governor_reuse "${edge_chain_created}"',
            edge_create_helper,
        )
        self.assertIn(
            "validate_edge_chain_inited() { gl_probe_edge_chain_inited_and_governor_ready; }",
            launcher,
        )
        self.assertIn("gl_probe_edge_chain_inited_and_governor_ready", repair)
        self.assertIn(
            "EDGE_REUSE_GATEWAY_GOVERNOR must be true or false",
            edge_create_helper,
        )
        migration_identity = edge_migrate_helper.index(
            "gl_assert_gateway_runtime_identity"
        )
        self.assertLess(
            migration_identity,
            edge_migrate_helper.index("gateway_acquire_execute_operator_lock"),
        )
        self.assertLess(
            migration_identity, edge_migrate_helper.index("gl_l1_broadcast_preflight")
        )
        for direct_helper in (edge_create_helper, edge_migrate_helper):
            self.assertLess(
                direct_helper.index("gl_ensure_zkstack_cli_release_current"),
                direct_helper.index("gl_path_for_zkstack"),
            )
        self.assertIn("gl_ensure_zkstack_cli_release_current", repair)
        self.assertNotIn(
            'if [ ! -x "${ZKSYNC_ERA_PATH}/zkstack_cli/target/release/zkstack" ]',
            repair,
        )

        for expected in (
            PUBLISHED_PATCH_TARGET,
            PUBLISHED_PATCH_TARGET_RUNTIME_HASH,
            PUBLISHED_EDGE_RELAY,
            PUBLISHED_EDGE_RELAY_RUNTIME_HASH,
            "0x2fa86add0aed31f33a762c9d88e807c475bd51d0f52bd0955754b2608f7e4989",
            "stopped before edge creation for identity repinning/review",
            "Gateway RPC chain ID mismatch",
            "cast block 0 --field hash",
            "runtime-genesis.v1",
            "creating the Gateway genesis stamp requires a live launcher-owned PID",
        ):
            self.assertIn(expected, common)
        self.assertNotIn("runtime-launch.json", common)
        self.assertIn('fd_dir = Path(f"/proc/{pid}/fd")', common)
        self.assertNotIn("/proc/{pid}/cmdline", common)
        self.assertIn(
            "Gateway RPC is already reachable before this launcher started it",
            lifecycle,
        )
        generator = (
            REPO_ROOT
            / "scripts"
            / "gateway-launch"
            / "generate-os-server-configs.sh"
        ).read_text(encoding="utf-8")
        self.assertIn('if chain_name == os.environ["GATEWAY_CHAIN_NAME"]', generator)
        self.assertIn('"exec-prebuilt --"', generator)
        self.assertIn('else "run --release --"', generator)
        self.assertIn('-- {runner_mode} {start_config_args}', generator)
        runtime_identity = common[
            common.index("gl_assert_gateway_runtime_identity()") : common.index(
                "\ngl_zksys_gas_tank_from_edge_config()"
            )
        ]
        final_listener_check = runtime_identity.rindex(
            "gl_assert_gateway_listener_owned_by_pid"
        )
        genesis_stamp = runtime_identity.index("gl_assert_gateway_genesis_stamp")
        self.assertLess(final_listener_check, genesis_stamp)
        genesis_identity = common[
            common.index("gl_assert_gateway_genesis_stamp()") : common.index(
                "\ngl_gateway_relay_from_gateway_config()"
            )
        ]
        block_hash_read = genesis_identity.index("cast block 0 --field hash")
        self.assertLess(
            genesis_identity.index("gl_assert_gateway_listener_owned_by_pid"),
            block_hash_read,
        )
        self.assertGreater(
            genesis_identity.rindex("gl_assert_gateway_listener_owned_by_pid"),
            block_hash_read,
        )
        self.assertLess(
            genesis_identity.rindex("gl_assert_gateway_listener_owned_by_pid"),
            genesis_identity.index("GATEWAY_GENESIS_STAMP="),
        )

    def test_gateway_listener_ownership_rejects_an_unrelated_live_pid(self) -> None:
        common = REPO_ROOT / "scripts" / "gateway-launch" / "_common.sh"
        lifecycle = (
            REPO_ROOT
            / "scripts"
            / "gateway-launch"
            / "_gateway_node_lifecycle.sh"
        )
        listener = subprocess.Popen(
            [
                "python3",
                "-c",
                "import socket,time; "
                "s=socket.socket(); s.bind(('127.0.0.1',0)); s.listen(); "
                "print(s.getsockname()[1],flush=True); time.sleep(30)",
            ],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        sleeper = subprocess.Popen(["sleep", "30"])
        try:
            assert listener.stdout is not None
            port = listener.stdout.readline().strip()
            if not port:
                assert listener.stderr is not None
                listener_error = listener.stderr.read()
                if "PermissionError" in listener_error and "Operation not permitted" in listener_error:
                    self.skipTest("test sandbox forbids loopback listener creation")
                self.fail(f"listener failed to start: {listener_error}")

            command = (
                'source "$COMMON"; source "$LIFECYCLE"; '
                'gl_assert_gateway_listener_owned_by_pid "$EXPECTED_PID" "$RPC_URL"'
            )
            base_env = {
                **os.environ,
                "COMMON": str(common),
                "LIFECYCLE": str(lifecycle),
                "RPC_URL": f"http://127.0.0.1:{port}",
            }
            base_env.pop("ZKSYNC_OS_SERVER_PATH", None)

            owned = subprocess.run(
                ["bash", "-c", command],
                check=False,
                capture_output=True,
                text=True,
                env={**base_env, "EXPECTED_PID": str(listener.pid)},
            )
            self.assertEqual(owned.returncode, 0, owned.stderr)

            unrelated = subprocess.run(
                ["bash", "-c", command],
                check=False,
                capture_output=True,
                text=True,
                env={**base_env, "EXPECTED_PID": str(sleeper.pid)},
            )
            self.assertNotEqual(unrelated.returncode, 0)
            self.assertIn("is not exclusively owned", unrelated.stderr)

            inherited = subprocess.run(
                [
                    "bash",
                    "-c",
                    'source "$COMMON"; '
                    'export GATEWAY_RUNTIME_OWNER_PID="$EXPECTED_PID"; '
                    'gl_assert_gateway_runtime_identity "" false "$RPC_URL"',
                ],
                check=False,
                capture_output=True,
                text=True,
                env={**base_env, "EXPECTED_PID": str(sleeper.pid)},
            )
            self.assertNotEqual(inherited.returncode, 0)
            self.assertIn("is not exclusively owned", inherited.stderr)
        finally:
            for process in (listener, sleeper):
                process.terminate()
            for process in (listener, sleeper):
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=5)
            for stream in (listener.stdout, listener.stderr):
                if stream is not None:
                    stream.close()

    def test_gateway_cleanup_parses_portable_pid_output_and_fails_closed(self) -> None:
        common = REPO_ROOT / "scripts" / "gateway-launch" / "_common.sh"
        lifecycle = (
            REPO_ROOT
            / "scripts"
            / "gateway-launch"
            / "_gateway_node_lifecycle.sh"
        )
        command = r'''
source "$COMMON"
source "$LIFECYCLE"
GATEWAY_STARTED_FOR_MIGRATION=true
GATEWAY_NODE_PID=""
stop_gateway_for_migration
'''

        with tempfile.TemporaryDirectory() as temporary_dir:
            root = Path(temporary_dir)
            bin_dir = root / "bin"
            bin_dir.mkdir()
            pgrep = bin_dir / "pgrep"
            ps = bin_dir / "ps"
            pgrep.write_text(
                "#!/usr/bin/env bash\nprintf '%s\\n' 99999999\n",
                encoding="utf-8",
            )
            ps.write_text(
                "#!/usr/bin/env bash\n"
                "printf '%s\\n' \"zksync-os-server --config "
                "${GATEWAY_DIR}/os-server-configs/gateway/config.yaml\"\n",
                encoding="utf-8",
            )
            pgrep.chmod(0o755)
            ps.chmod(0o755)
            env = {
                **os.environ,
                "COMMON": str(common),
                "LIFECYCLE": str(lifecycle),
                "GATEWAY_DIR": str(root / "gateway"),
                "PATH": f"{bin_dir}:{os.environ['PATH']}",
            }

            found = subprocess.run(
                ["bash", "-c", command],
                check=False,
                capture_output=True,
                text=True,
                env=env,
            )
            self.assertEqual(found.returncode, 0, found.stderr)
            self.assertIn(
                "stopping Gateway node child processes [99999999]", found.stdout
            )

            pgrep.write_text(
                "#!/usr/bin/env bash\n"
                "printf '%s\\n' 'synthetic pgrep failure' >&2\n"
                "exit 2\n",
                encoding="utf-8",
            )
            failed = subprocess.run(
                ["bash", "-c", command],
                check=False,
                capture_output=True,
                text=True,
                env=env,
            )
            self.assertNotEqual(failed.returncode, 0)
            self.assertIn("pgrep failed while locating Gateway children", failed.stderr)

    def test_gateway_genesis_stamp_rejects_missing_or_different_deployment(self) -> None:
        common = REPO_ROOT / "scripts" / "gateway-launch" / "_common.sh"
        block_a = "0x" + "aa" * 32
        block_b = "0x" + "bb" * 32
        command = r'''
source "$COMMON"
gl_assert_gateway_genesis_stamp "$GATEWAY_RPC_URL" 57057 "$ALLOW_CREATE"
'''

        with tempfile.TemporaryDirectory() as temporary_dir:
            root = Path(temporary_dir)
            bin_dir = root / "bin"
            bin_dir.mkdir()
            fake_cast = bin_dir / "cast"
            fake_cast.write_text(
                "#!/usr/bin/env bash\n"
                "set -euo pipefail\n"
                "[ \"${1:-}\" = block ] || exit 2\n"
                "printf '%s\\n' \"${TEST_BLOCK_HASH:?}\"\n",
                encoding="utf-8",
            )
            fake_cast.chmod(0o755)

            def run_stamp(
                block_hash: str, allow_create: bool
            ) -> subprocess.CompletedProcess[str]:
                env = os.environ.copy()
                env.pop("ZKSYNC_OS_SERVER_PATH", None)
                env.update(
                    {
                        "ALLOW_CREATE": str(allow_create).lower(),
                        "COMMON": str(common),
                        "GATEWAY_DIR": str(root / "gateway"),
                        "GATEWAY_RPC_URL": "https://authenticated-gateway.invalid",
                        "PATH": f"{bin_dir}{os.pathsep}{env['PATH']}",
                        "TEST_BLOCK_HASH": block_hash,
                    }
                )
                return subprocess.run(
                    ["bash", "-c", command],
                    check=False,
                    capture_output=True,
                    text=True,
                    env=env,
                )

            missing = run_stamp(block_a, False)
            self.assertNotEqual(missing.returncode, 0)
            self.assertIn("missing Gateway genesis stamp", missing.stderr)

            created = run_stamp(block_a, True)
            self.assertEqual(created.returncode, 0, created.stderr)
            stamp = root / "gateway" / ".gateway-launch" / "gateway-runtime-genesis.v1"
            self.assertEqual(stamp.read_text(encoding="utf-8"), f"57057 {block_a}\n")
            self.assertEqual(stamp.stat().st_mode & 0o777, 0o600)

            reused = run_stamp(block_a, False)
            self.assertEqual(reused.returncode, 0, reused.stderr)

            mismatched = run_stamp(block_b, False)
            self.assertNotEqual(mismatched.returncode, 0)
            self.assertIn("Gateway deployment genesis mismatch", mismatched.stderr)

    def test_gateway_common_rejects_an_alternate_repository_trust_root(self) -> None:
        common = REPO_ROOT / "scripts" / "gateway-launch" / "_common.sh"
        with tempfile.TemporaryDirectory() as alternate_root:
            env = os.environ.copy()
            env["ZKSYNC_OS_SERVER_PATH"] = alternate_root
            result = subprocess.run(
                ["bash", "-c", 'source "$COMMON"'],
                check=False,
                capture_output=True,
                text=True,
                env={**env, "COMMON": str(common)},
            )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("ZKSYNC_OS_SERVER_PATH must resolve", result.stderr)

        with tempfile.TemporaryDirectory() as temporary_dir:
            root = Path(temporary_dir)
            gateway_link = root / "gateway-launch-link"
            repo_link = root / "server-link"
            gateway_link.symlink_to(common.parent, target_is_directory=True)
            repo_link.symlink_to(REPO_ROOT, target_is_directory=True)
            accepted = subprocess.run(
                [
                    "bash",
                    "-c",
                    'source "$COMMON"; cd /; printf "%s|%s\\n" "$GL_DIR" "$ZKSYNC_OS_SERVER_PATH"',
                ],
                check=False,
                capture_output=True,
                text=True,
                env={
                    **os.environ,
                    "COMMON": str(common),
                    "GL_DIR": str(gateway_link),
                    "ZKSYNC_OS_SERVER_PATH": str(repo_link),
                },
            )
        self.assertEqual(accepted.returncode, 0, accepted.stderr)
        self.assertEqual(
            accepted.stdout.strip(),
            f"{common.parent.resolve()}|{REPO_ROOT.resolve()}",
        )

    def test_gateway_fee_payer_is_provisioned_before_deposits_reopen(self) -> None:
        migration = (
            REPO_ROOT
            / "scripts"
            / "gateway-launch"
            / "edge-chain-migrate-to-gateway.sh"
        ).read_text(encoding="utf-8")

        sequence = (
            'ensure_gateway_commit_sender_balance "${EDGE_CHAIN_NAME}"\n'
            '  provision_gateway_settlement_fee_payer "${EDGE_CHAIN_NAME}"\n'
            '  ensure_deposits_unpaused "${EDGE_CHAIN_NAME}"'
        )
        final_sequence = (
            'ensure_gateway_commit_sender_balance "${EDGE_CHAIN_NAME}"\n'
            'provision_gateway_settlement_fee_payer "${EDGE_CHAIN_NAME}"\n'
            'ensure_deposits_unpaused "${EDGE_CHAIN_NAME}"'
        )
        self.assertIn(sequence, migration)
        self.assertIn(final_sequence, migration)
        self.assertEqual(
            migration.count(
                'provision_gateway_settlement_fee_payer "${EDGE_CHAIN_NAME}"'
            ),
            2,
        )
        pin_assertion = migration.index(
            'gl_assert_gateway_wrapped_base_token_pin "${GATEWAY_RPC_URL}"'
        )
        self.assertLess(pin_assertion, migration.index("gl_l1_broadcast_preflight"))
        self.assertEqual(
            migration.count(
                'GATEWAY_WRAPPED_BASE_TOKEN_ADDRESS="${GATEWAY_WRAPPED_BASE_TOKEN_ADDRESS}"'
            ),
            2,
        )
        fee_helper = (
            REPO_ROOT
            / "scripts"
            / "gateway-launch"
            / "provision-edge-settlement-fee-payer.sh"
        ).read_text(encoding="utf-8")
        self.assertLess(
            fee_helper.index("gl_assert_gateway_runtime_identity"),
            fee_helper.index("gl_assert_gateway_wrapped_base_token_pin"),
        )
        self.assertLess(
            fee_helper.index("gl_assert_gateway_wrapped_base_token_pin"),
            fee_helper.index('provision_edge_fee_payer "${edge_name}"'),
        )
        self.assertIn(
            'wrapped_token="${GATEWAY_WRAPPED_BASE_TOKEN_ADDRESS}"', fee_helper
        )
        generator = (
            REPO_ROOT
            / "scripts"
            / "gateway-launch"
            / "generate-os-server-configs.sh"
        ).read_text(encoding="utf-8")
        lock_helper = (
            REPO_ROOT
            / "scripts"
            / "gateway-launch"
            / "_execute_operator_lock.sh"
        ).read_text(encoding="utf-8")
        self.assertIn("_execute_operator_lock.sh", generator)
        self.assertIn("gateway_acquire_execute_operator_lock", generator)
        self.assertIn("fcntl.LOCK_EX | fcntl.LOCK_NB", lock_helper)

    def test_gateway_config_identity_mismatch_fails_closed(self) -> None:
        common = REPO_ROOT / "scripts" / "gateway-launch" / "_common.sh"
        command = r'''
source "$COMMON"
gl_assert_gateway_chain_config_matches_expected() { :; }
gl_syscoin_edge_da_commit_target_from_gateway_config() { printf '%s\n' "$TEST_TARGET"; }
gl_gateway_relay_from_gateway_config() { printf '%s\n' "$TEST_RELAY"; }
gl_assert_gateway_config_identity
'''

        def run_identity(target: str, relay: str) -> subprocess.CompletedProcess[str]:
            env = os.environ.copy()
            env.update(
                {
                    "COMMON": str(common),
                    "TEST_TARGET": target,
                    "TEST_RELAY": relay,
                }
            )
            return subprocess.run(
                ["bash", "-c", command],
                check=False,
                capture_output=True,
                text=True,
                env=env,
            )

        exact = run_identity(PUBLISHED_PATCH_TARGET, PUBLISHED_EDGE_RELAY)
        self.assertEqual(exact.returncode, 0, exact.stderr)

        wrong_target = run_identity(OTHER_TARGET, PUBLISHED_EDGE_RELAY)
        self.assertNotEqual(wrong_target.returncode, 0)
        self.assertIn("stopped before edge creation", wrong_target.stderr)

        wrong_relay = run_identity(PUBLISHED_PATCH_TARGET, OTHER_TARGET)
        self.assertNotEqual(wrong_relay.returncode, 0)
        self.assertIn("stopped before edge creation", wrong_relay.stderr)


class RunLocalBehaviorTests(unittest.TestCase):
    def test_first_boot_keeps_published_gas_tank_and_mismatch_fails(self) -> None:
        common = REPO_ROOT / "scripts" / "gateway-launch" / "_common.sh"
        command = r'''
source "$COMMON"
gl_zksys_gas_tank_from_edge_config() { printf '%s\n' "$CONFIGURED_TANK"; }
gl_export_syscoin_gas_tank_address_from_edge_config
printf '%s\n' "$SYSCOIN_GAS_TANK_ADDRESS"
'''
        env = os.environ.copy()
        env.update(
            {
                "COMMON": str(common),
                "SYSCOIN_GAS_TANK_ADDRESS": PUBLISHED_GAS_TANK,
                "CONFIGURED_TANK": "0x" + "0" * 40,
            }
        )
        first_boot = subprocess.run(
            ["bash", "-c", command],
            check=False,
            capture_output=True,
            text=True,
            env=env,
        )
        self.assertEqual(first_boot.returncode, 0, first_boot.stderr)
        self.assertEqual(first_boot.stdout.strip(), PUBLISHED_GAS_TANK)
        self.assertIn("before its first-boot deployment", first_boot.stderr)

        env["CONFIGURED_TANK"] = OTHER_TARGET
        mismatch = subprocess.run(
            ["bash", "-c", command],
            check=False,
            capture_output=True,
            text=True,
            env=env,
        )
        self.assertNotEqual(mismatch.returncode, 0)
        self.assertIn("does not match l2.zksys_gas_tank_addr", mismatch.stderr)

    def test_gas_tank_requirement_promotion_is_main_node_only(self) -> None:
        common = REPO_ROOT / "scripts" / "gateway-launch" / "_common.sh"
        launcher = (
            REPO_ROOT
            / "scripts"
            / "gateway-launch"
            / "run-os-server-with-patched-zksync-os.sh"
        ).read_text(encoding="utf-8")

        main_case = launcher.index('"${EDGE_CHAIN_NAME:-zksys}")')
        main_promotion = launcher.index(
            "gl_export_syscoin_gas_tank_address_from_edge_config true",
            main_case,
        )
        external_case = launcher.index('"${EDGE_CHAIN_NAME:-zksys}"-*)', main_promotion)
        external_policy = launcher.index(
            "gl_export_syscoin_gas_tank_address_from_edge_config false",
            external_case,
        )
        self.assertLess(main_case, main_promotion)
        self.assertLess(main_promotion, external_case)
        self.assertLess(external_case, external_policy)
        self.assertEqual(
            launcher.count(
                "gl_export_syscoin_gas_tank_address_from_edge_config true"
            ),
            1,
        )
        self.assertEqual(
            launcher.count(
                "gl_export_syscoin_gas_tank_address_from_edge_config false"
            ),
            1,
        )

        command = r'''
source "$COMMON"
gl_zksys_gas_tank_from_edge_config() { printf '%s\n' "$CONFIGURED_TANK"; }
gl_export_syscoin_gas_tank_address_from_edge_config "$AUTO_REQUIRE"
printf '%s|%s\n' "$SYSCOIN_GAS_TANK_ADDRESS" "${SYSCOIN_REQUIRE_GAS_TANK:-unset}"
'''

        def run_policy(
            auto_require: str, require_tank: str
        ) -> subprocess.CompletedProcess[str]:
            env = os.environ.copy()
            env.update(
                {
                    "AUTO_REQUIRE": auto_require,
                    "COMMON": str(common),
                    "CONFIGURED_TANK": PUBLISHED_GAS_TANK,
                    "SYSCOIN_GAS_TANK_ADDRESS": PUBLISHED_GAS_TANK,
                    "SYSCOIN_REQUIRE_GAS_TANK": require_tank,
                }
            )
            return subprocess.run(
                ["bash", "-c", command],
                check=False,
                capture_output=True,
                text=True,
                env=env,
            )

        # Once the canonical main node sees the nonzero, bootstrap-attested
        # address in persisted config, even an explicit first-boot override is
        # retired. This prevents production from silently retaining fallback.
        canonical_main = run_policy("true", "0")
        self.assertEqual(canonical_main.returncode, 0, canonical_main.stderr)
        self.assertEqual(canonical_main.stdout.strip(), f"{PUBLISHED_GAS_TANK}|1")
        self.assertIn(
            "ignoring SYSCOIN_REQUIRE_GAS_TANK=0", canonical_main.stderr
        )

        # An external node may have the nonzero address in config before local
        # state has caught up to deployment, so its explicit catch-up policy is
        # preserved. Runtime validation can be required after catch-up.
        external = run_policy("false", "0")
        self.assertEqual(external.returncode, 0, external.stderr)
        self.assertEqual(external.stdout.strip(), f"{PUBLISHED_GAS_TANK}|0")
        self.assertNotIn("ignoring SYSCOIN_REQUIRE_GAS_TANK=0", external.stderr)

        malformed_requirement = run_policy("false", "true")
        self.assertNotEqual(malformed_requirement.returncode, 0)
        self.assertIn(
            "SYSCOIN_REQUIRE_GAS_TANK must be exactly 0 or 1",
            malformed_requirement.stderr,
        )

        malformed_role_policy = run_policy("sometimes", "0")
        self.assertNotEqual(malformed_role_policy.returncode, 0)
        self.assertIn("invalid gas-tank auto-require policy", malformed_role_policy.stderr)

    def test_cargo_wrapper_rejects_nonpublished_consensus_inputs(self) -> None:
        wrapper = REPO_ROOT / "scripts" / "cargo-with-patched-zksync-os.sh"
        for name in (
            "SYSCOIN_EDGE_DA_COMMIT_TARGET",
            "SYSCOIN_GAS_TANK_ADDRESS",
        ):
            with self.subTest(name=name), tempfile.TemporaryDirectory() as temp:
                env = os.environ.copy()
                env["GATEWAY_DIR"] = temp
                env[name] = OTHER_TARGET
                result = subprocess.run(
                    ["bash", str(wrapper), "reject-mismatch", "--", "metadata"],
                    check=False,
                    capture_output=True,
                    text=True,
                    env=env,
                )
                self.assertNotEqual(result.returncode, 0)
                self.assertIn(
                    "differs from the published zksync-os app value", result.stderr
                )

    def test_setup_failure_is_not_hidden_by_cleanup(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            temp_path = Path(temp)
            env = os.environ.copy()
            env["TMPDIR"] = str(temp_path)
            result = subprocess.run(
                ["bash", str(REPO_ROOT / "run_local.sh"), str(temp_path / "missing")],
                check=False,
                capture_output=True,
                text=True,
                env=env,
            )
            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(list(temp_path.iterdir()), [])


class PatchedWorkspaceRewriteTests(unittest.TestCase):
    @staticmethod
    def fixture_cargo_toml(*, canonical_tag: str = FINAL_OS_TAG) -> str:
        return f'''\
[workspace]
members = []

[workspace.dependencies]
zk_os_forward_system = {{ package = "forward_system", git = "{OFFICIAL_OS_URL}.git", tag = "{canonical_tag}", features = [
    "production",
    "no_print",
], default-features = false }}
zk_ee = {{ git = "{OFFICIAL_OS_URL}.git", tag = "{FINAL_OS_TAG}" }}
zk_os_basic_system = {{ package = "basic_system", git = "{OFFICIAL_OS_URL}.git", tag = "{FINAL_OS_TAG}" }}
zk_os_api = {{ package = "zksync_os_api", git = "{OFFICIAL_OS_URL}.git", tag = "{FINAL_OS_TAG}" }}
zk_os_evm_interpreter = {{ package = "evm_interpreter", git = "{OFFICIAL_OS_URL}.git", tag = "{FINAL_OS_TAG}" }}
'''

    @staticmethod
    def fixture_cargo_lock(*, locked_rev: str = FINAL_LOCKED_REV) -> str:
        return f'''\
version = 3

[[package]]
name = "basic_system"
version = "0.1.0"
source = "git+{OFFICIAL_OS_URL}.git?tag={FINAL_OS_TAG}#{locked_rev}"
dependencies = [
 "canonical-helper 0.1.0 (git+{OFFICIAL_OS_URL}.git?tag={FINAL_OS_TAG})",
 "serde",
 "storage_models",
]

[[package]]
name = "callable_oracles"
version = "0.1.0"
source = "git+{OFFICIAL_OS_URL}.git?tag={FINAL_OS_TAG}#{locked_rev}"
dependencies = [
 "basic_system",
 "c-kzg",
]

[[package]]
name = "c-kzg"
version = "2.1.8"
source = "registry+https://github.com/rust-lang/crates.io-index"

[[package]]
name = "sha2"
version = "0.10.9"
source = "registry+https://github.com/rust-lang/crates.io-index"
'''

    def run_rewrite(
        self,
        temp_path: Path,
        *,
        cargo_toml: str | None = None,
        cargo_lock: str | None = None,
        git_url: str = f"{OFFICIAL_OS_URL}.git",
        copied_marker_mtime_ns: int | None = None,
    ) -> subprocess.CompletedProcess[str]:
        source = temp_path / "source"
        source.mkdir()
        (source / "Cargo.toml").write_text(
            cargo_toml or self.fixture_cargo_toml(), encoding="utf-8"
        )
        (source / "Cargo.lock").write_text(
            cargo_lock or self.fixture_cargo_lock(), encoding="utf-8"
        )
        copied_marker = source / "copied-marker"
        copied_marker.write_text("yes\n", encoding="utf-8")
        if copied_marker_mtime_ns is not None:
            os.utime(
                copied_marker,
                ns=(copied_marker_mtime_ns, copied_marker_mtime_ns),
            )
        patched_path = temp_path / "patched-final-os"
        patched_path.mkdir()

        env = os.environ.copy()
        env.update(
            {
                "HELPER": str(
                    REPO_ROOT / "scripts" / "_patched-zksync-os-workspace.sh"
                ),
                "SERVER": str(source),
                "RUN": str(temp_path / "run"),
                "PATCHED_PATH": str(patched_path),
                "SOURCE_URL": git_url,
            }
        )
        command = f'''\
set -euo pipefail
gl_die() {{ printf 'error: %s\\n' "$*" >&2; exit 1; }}
export ZKSYNC_OS_SERVER_PATH="$SERVER"
source "$HELPER"
prepare_run_workspace \\
  "$RUN" "$PATCHED_PATH" "{FINAL_OS_TAG}" "$SOURCE_URL" \\
  "{FINAL_LOCKED_REV}" "{FINAL_PATCHED_REV}"
'''
        return subprocess.run(
            ["bash", "-c", command],
            check=False,
            capture_output=True,
            text=True,
            env=env,
        )

    def test_rewrites_only_canonical_final_source(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            temp_path = Path(temp)
            result = self.run_rewrite(temp_path)
            self.assertEqual(result.returncode, 0, result.stderr)

            run = temp_path / "run"
            rewritten_toml = (run / "Cargo.toml").read_text(encoding="utf-8")
            rewritten_lock = (run / "Cargo.lock").read_text(encoding="utf-8")
            local_uri = (temp_path / "patched-final-os").resolve().as_uri()

            self.assertEqual(rewritten_toml.count(f'git = "{local_uri}"'), 5)
            self.assertEqual(
                rewritten_lock.count(
                    f"git+{local_uri}?tag={FINAL_OS_TAG}#{FINAL_PATCHED_REV}"
                ),
                2,
            )
            self.assertIn(
                f"git+{local_uri}?tag={FINAL_OS_TAG})", rewritten_lock
            )
            self.assertIn(' "sha2 0.10.9",\n "storage_models",', rewritten_lock)
            callable_block = rewritten_lock.split(
                'name = "callable_oracles"', 1
            )[1].split("[[package]]", 1)[0]
            self.assertIn(' "c-kzg",', callable_block)
            self.assertIn('name = "c-kzg"\nversion = "2.1.8"', rewritten_lock)
            self.assertNotIn(
                f"git+{OFFICIAL_OS_URL}.git?tag={FINAL_OS_TAG}#{FINAL_LOCKED_REV}",
                rewritten_lock,
            )
            self.assertEqual((run / "copied-marker").read_text(), "yes\n")

    def test_recreated_workspace_does_not_preserve_stale_source_mtime(self) -> None:
        # Cargo's freshness logic can otherwise reuse build-script output that
        # names deleted generated files when the source snapshot looks older
        # than a cached artifact. The production helper must perform a content
        # copy with a fresh destination mtime, not metadata-preserving copy2.
        stale_mtime_ns = 946_684_800_000_000_000  # 2000-01-01T00:00:00Z
        with tempfile.TemporaryDirectory() as temp:
            temp_path = Path(temp)
            result = self.run_rewrite(
                temp_path,
                copied_marker_mtime_ns=stale_mtime_ns,
            )
            self.assertEqual(result.returncode, 0, result.stderr)

            source_marker = temp_path / "source" / "copied-marker"
            recreated_marker = temp_path / "run" / "copied-marker"
            self.assertEqual(source_marker.stat().st_mtime_ns, stale_mtime_ns)
            self.assertNotEqual(recreated_marker.stat().st_mtime_ns, stale_mtime_ns)
            self.assertGreater(recreated_marker.stat().st_mtime_ns, stale_mtime_ns)

    def test_rejects_nonofficial_source(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            result = self.run_rewrite(
                Path(temp), git_url="https://github.com/syscoin/zksync-os"
            )
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("not official matter-labs/zksync-os", result.stderr)

    def test_rejects_canonical_alias_tag_drift(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            result = self.run_rewrite(
                Path(temp),
                cargo_toml=self.fixture_cargo_toml(canonical_tag="v0.4.0-rc.2"),
            )
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("noncanonical zksync-os dependency remains", result.stderr)

    def test_rejects_locked_revision_drift(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            result = self.run_rewrite(
                Path(temp), cargo_lock=self.fixture_cargo_lock(locked_rev="6" * 40)
            )
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("does not match canonical locked revision", result.stderr)

    def test_rejects_noncanonical_direct_dependency(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            cargo_toml = self.fixture_cargo_toml() + f'''\
zk_os_forward_system_old = {{ package = "forward_system", git = "{OFFICIAL_OS_URL}", tag = "{OTHER_OS_TAG}" }}
'''
            result = self.run_rewrite(Path(temp), cargo_toml=cargo_toml)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("noncanonical zksync-os dependency remains", result.stderr)

    def test_rejects_noncanonical_lock_source(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            cargo_lock = self.fixture_cargo_lock() + f'''\
[[package]]
name = "old-os"
version = "0.1.0"
source = "git+{OFFICIAL_OS_URL}?tag={OTHER_OS_TAG}#{'5' * 40}"
'''
            result = self.run_rewrite(Path(temp), cargo_lock=cargo_lock)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("noncanonical zksync-os tag remains", result.stderr)

    def test_rejects_missing_patched_lock_dependency_package(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            cargo_lock = self.fixture_cargo_lock().replace(
                '''\
[[package]]
name = "sha2"
version = "0.10.9"
source = "registry+https://github.com/rust-lang/crates.io-index"
''',
                "",
            )
            result = self.run_rewrite(Path(temp), cargo_lock=cargo_lock)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("required sha2 0.10.9 package", result.stderr)

if __name__ == "__main__":
    unittest.main()
