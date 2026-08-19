from __future__ import annotations

import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from typing import Dict, Optional


REPO_ROOT = Path(__file__).resolve().parents[2]
RESOLVER = REPO_ROOT / "scripts" / "resolve-local-zksync-os-context.py"
EXPECTED_TARGET = "0x9ba6e5da3d3b75043b5ed73f6442f504e8745c61"
OTHER_TARGET = "0x1111111111111111111111111111111111111111"
PUBLISHED_PATCH_TARGET = "0x64ef2f0c4168eb76fe95993f2a7c7b35dcf3fe19"


def write(path: Path, text: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text, encoding="utf-8")


def versions(protocol: str) -> str:
    return f'general:\n  protocol_version: "{protocol}"\n'


def chain_config(chain_id: int) -> str:
    return f"genesis:\n  chain_id: {chain_id}\n"


def contracts(target: str, l1_target: Optional[str] = None) -> str:
    if l1_target is None:
        l1_target = target
    return (
        "ecosystem_contracts:\n"
        f"  validator_timelock_addr: {target}\n"
        "l1:\n"
        f"  validator_timelock_addr: {l1_target}\n"
    )


class ResolverTests(unittest.TestCase):
    def run_resolver(
        self, config_dir: Path, extra_env: Optional[Dict[str, str]] = None
    ) -> subprocess.CompletedProcess[str]:
        env = os.environ.copy()
        for name in (
            "SYSCOIN_EDGE_DA_COMMIT_TARGET",
            "ZKSYNC_OS_SYSCOIN_EDGE_DA_COMMIT_TARGET",
            "ZKSYNC_OS_LOCAL_PROTOCOL_VERSION",
        ):
            env.pop(name, None)
        if extra_env:
            env.update(extra_env)
        return subprocess.run(
            [sys.executable, str(RESOLVER), str(config_dir)],
            check=False,
            capture_output=True,
            text=True,
            env=env,
        )

    def test_real_v31_single_fixture_uses_matching_contracts_file(self) -> None:
        result = self.run_resolver(REPO_ROOT / "local-chains" / "v31.0" / "default")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), f"v31.0\t{EXPECTED_TARGET}")

    def test_real_v31_multichain_fixtures_agree(self) -> None:
        result = self.run_resolver(
            REPO_ROOT / "local-chains" / "v31.0" / "multi_chain"
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), f"v31.0\t{EXPECTED_TARGET}")

    def test_explicit_target_is_required_when_contracts_are_absent(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            version_dir = Path(temp) / "v31.0"
            config_dir = version_dir / "custom"
            write(version_dir / "versions.yaml", versions("v31.0"))
            write(config_dir / "config.yaml", chain_config(7))

            missing = self.run_resolver(config_dir)
            self.assertNotEqual(missing.returncode, 0)
            self.assertIn("SYSCOIN_EDGE_DA_COMMIT_TARGET", missing.stderr)

            explicit = self.run_resolver(
                config_dir, {"SYSCOIN_EDGE_DA_COMMIT_TARGET": EXPECTED_TARGET}
            )
            self.assertEqual(explicit.returncode, 0, explicit.stderr)
            self.assertEqual(explicit.stdout.strip(), f"v31.0\t{EXPECTED_TARGET}")

    def test_multichain_target_mismatch_fails_closed(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            version_dir = Path(temp) / "v31.0"
            config_dir = version_dir / "multi_chain"
            write(version_dir / "versions.yaml", versions("v31.0"))
            write(config_dir / "chain_7.yaml", chain_config(7))
            write(config_dir / "chain_8.yaml", chain_config(8))
            write(config_dir / "contracts_7.yaml", contracts(EXPECTED_TARGET))
            write(config_dir / "contracts_8.yaml", contracts(OTHER_TARGET))

            result = self.run_resolver(config_dir)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("different validator timelocks", result.stderr)

    def test_explicit_target_must_match_fixture(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            version_dir = Path(temp) / "v31.0"
            config_dir = version_dir / "default"
            write(version_dir / "versions.yaml", versions("v31.0"))
            write(config_dir / "config.yaml", chain_config(7))
            write(config_dir / "contracts_7.yaml", contracts(EXPECTED_TARGET))

            result = self.run_resolver(
                config_dir, {"SYSCOIN_EDGE_DA_COMMIT_TARGET": OTHER_TARGET}
            )
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("does not match", result.stderr)

    def test_duplicate_contract_fields_must_agree(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            version_dir = Path(temp) / "v31.0"
            config_dir = version_dir / "default"
            write(version_dir / "versions.yaml", versions("v31.0"))
            write(config_dir / "config.yaml", chain_config(7))
            write(
                config_dir / "contracts_7.yaml",
                contracts(EXPECTED_TARGET, OTHER_TARGET),
            )

            result = self.run_resolver(config_dir)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("addresses disagree", result.stderr)

    def test_non_v31_does_not_require_contracts(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            version_dir = Path(temp) / "v30.2"
            config_dir = version_dir / "default"
            write(version_dir / "versions.yaml", versions("v30.2"))
            config_dir.mkdir(parents=True)

            result = self.run_resolver(config_dir)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(result.stdout, f"v30.2\t{PUBLISHED_PATCH_TARGET}\n")

    def test_non_v31_allows_an_explicit_source_target(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            version_dir = Path(temp) / "v30.2"
            config_dir = version_dir / "default"
            write(version_dir / "versions.yaml", versions("v30.2"))
            config_dir.mkdir(parents=True)

            result = self.run_resolver(
                config_dir, {"SYSCOIN_EDGE_DA_COMMIT_TARGET": OTHER_TARGET}
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(result.stdout, f"v30.2\t{OTHER_TARGET}\n")

    def test_quoted_protocol_with_inline_comment_stays_v31(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            version_dir = Path(temp) / "v31.0"
            config_dir = version_dir / "default"
            write(
                version_dir / "versions.yaml",
                'general:\n  protocol_version: "v31.0" # fixture note\n',
            )
            write(config_dir / "config.yaml", chain_config(7))
            write(config_dir / "contracts_7.yaml", contracts(EXPECTED_TARGET))

            result = self.run_resolver(config_dir)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(result.stdout, f"v31.0\t{EXPECTED_TARGET}\n")

    def test_invalid_protocol_fails_instead_of_using_static_context(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            version_dir = Path(temp) / "custom"
            config_dir = version_dir / "default"
            write(version_dir / "versions.yaml", versions("v31-invalid"))
            config_dir.mkdir(parents=True)

            result = self.run_resolver(config_dir)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("invalid general.protocol_version", result.stderr)


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
        self.assertIn("ZKSYNC_OS_FORCE_PATCHED_WORKSPACE=true", script)
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
        self.assertIn(
            "0xb9feff70ec42b6b5af5a690b4dbc332a2d1f3beb", wrapper
        )
        self.assertIn("ZKSYNC_OS_FORCE_PATCHED_WORKSPACE=true", wrapper)
        self.assertIn("ZKSYNC_OS_STATIC_BUILD_CONTEXT=true", wrapper)
        self.assertIn('[ -z "${SYSCOIN_GAS_TANK_ADDRESS+x}" ]', wrapper)

    def test_multivm_build_fails_closed_on_unpatched_execution_source(self) -> None:
        build_rs = (REPO_ROOT / "lib" / "multivm" / "build.rs").read_text(
            encoding="utf-8"
        )
        self.assertIn("verify_syscoin_execution_source", build_rs)
        self.assertIn("system_hooks/slh_dsa_precompile", build_rs)
        self.assertIn("SLH_DSA_SHA2_128_24_VERIFY_HOOK_ADDRESS_LOW", build_rs)
        self.assertIn("Use run_local.sh", build_rs)


class RunLocalBehaviorTests(unittest.TestCase):
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
