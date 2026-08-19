from __future__ import annotations

import os
import subprocess
import tempfile
import unittest
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[2]
OTHER_TARGET = "0x1111111111111111111111111111111111111111"
PUBLISHED_PATCH_TARGET = "0x64ef2f0c4168eb76fe95993f2a7c7b35dcf3fe19"
PUBLISHED_GAS_TANK = "0xb9feff70ec42b6b5af5a690b4dbc332a2d1f3beb"
PUBLISHED_EDGE_SOURCE_SHA256 = (
    "1eb8dc0da30570626a860968140c41663b9a40077f2c420665196b7506d7a7cb"
)


def rust_address_bytes(address: str) -> str:
    value = address.removeprefix("0x")
    return ", ".join(f"0x{value[index:index + 2]}" for index in range(0, 40, 2))


class LauncherStaticTests(unittest.TestCase):
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

    def test_run_local_builds_one_patched_prebuilt_and_executes_it(self) -> None:
        script = (REPO_ROOT / "run_local.sh").read_text(encoding="utf-8")
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
        self.assertIn("differs from the published V7 app value", wrapper)

    def test_os_applicator_does_not_regenerate_consensus_constants(self) -> None:
        applicator = (
            REPO_ROOT / "scripts" / "apply-zksync-os-syscoin-patch.sh"
        ).read_text(encoding="utf-8")
        self.assertNotIn("write_syscoin_edge_da_commit_target", applicator)
        self.assertNotIn("SYSCOIN_EDGE_DA_COMMIT_TARGET", applicator)
        self.assertNotIn("SYSCOIN_GAS_TANK_ADDRESS", applicator)
        self.assertIn("apply --reverse --check --recount", applicator)

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
            REPO_ROOT / "scripts" / "patches" / "zksync-os-syscoin.patch"
        ).read_text(encoding="utf-8")
        self.assertIn(rust_address_bytes(PUBLISHED_PATCH_TARGET), patch)
        self.assertIn(rust_address_bytes(PUBLISHED_GAS_TANK), patch)

        deploy_en_text = deploy_en.read_text(encoding="utf-8")
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
        self.assertLess(
            deploy_gas_tank.index("cast compute-address --nonce"),
            deploy_gas_tank.index("forge create src/zksys/ZkSysGasTank.sol"),
        )
        self.assertIn("cast call --rpc-url", deploy_gas_tank)
        self.assertIn("--create \"${gas_tank_creation_code}\"", deploy_gas_tank)
        self.assertGreaterEqual(deploy_gas_tank.count("--no-metadata"), 2)
        self.assertIn("existing_gas_tank_runtime", deploy_gas_tank)
        self.assertIn("expected_gas_tank_runtime", deploy_gas_tank)

    def test_multivm_build_fails_closed_on_unpatched_execution_source(self) -> None:
        build_rs = (REPO_ROOT / "lib" / "multivm" / "build.rs").read_text(
            encoding="utf-8"
        )
        self.assertIn("verify_syscoin_execution_source", build_rs)
        self.assertIn("system_hooks/slh_dsa_precompile", build_rs)
        self.assertIn("SLH_DSA_SHA2_128_24_VERIFY_HOOK_ADDRESS_LOW", build_rs)
        self.assertIn("require_patched_source_sha256", build_rs)
        self.assertIn(PUBLISHED_EDGE_SOURCE_SHA256, build_rs)
        self.assertIn("Use run_local.sh", build_rs)


class EraAttestationStaticTests(unittest.TestCase):
    def test_era_helper_requires_reverse_applicability_and_exact_artifacts(self) -> None:
        helper = (
            REPO_ROOT / "scripts" / "apply-era-contracts-syscoin-patch.sh"
        ).read_text(encoding="utf-8")
        self.assertIn("apply --reverse --check --recount", helper)
        self.assertNotIn("base_patch_core_applied", helper)
        for size, digest in (
            (
                "95217",
                "6302e7132a53c1895bf6ee9ede83a2c4e7bdddc5eedbffaabbe69fb043ee7e2f",
            ),
            (
                "8082",
                "f2805b9ef334f61c874e152b183035cb1d31172d48c6b125f0e6047c9aaa5168",
            ),
            (
                "77746",
                "9308b1850d4197bd7b6a59cc35029f51b94ffce76f5951848669fd9424a07d48",
            ),
            (
                "1920",
                "a1d093cf2bb0f5331c4a6bbf0e40d5f4888cc850324e8b9e406bde6686f07f77",
            ),
        ):
            self.assertIn(size, helper)
            self.assertIn(digest, helper)


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
                self.assertIn("differs from the published V7 app value", result.stderr)

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


if __name__ == "__main__":
    unittest.main()
