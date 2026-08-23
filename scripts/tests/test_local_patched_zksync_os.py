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
    "383259d3edeb24c56dfc9d8ee6fb5e814673a712a44cabcbd1c86338b2791899"
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
            'EXPECTED_PATCH_SIZE="1133789"',
            'EXPECTED_PATCH_SHA256="38b06604a483d037542a88f1ab1caf1688d58a0520b3773a74ab6e4b3f64626d"',
            'EXPECTED_PATCH_PATH_COUNT="53"',
            'EXPECTED_PATCH_PATHS_SHA256="dc67052881ca18e7ef03b5142a704a627357e1cb55d21ec2725e06cd343b11ac"',
        ):
            self.assertIn(expected, applicator)
        self.assertIn('require_text "${tagged_path}" "SYSCOIN:"', applicator)
        for tagged_path in (
            "blob_data_id_advice.rs",
            "callable_oracles/src/blob_data_id/mod.rs",
            "forward_system/src/run/mod.rs",
            "zk_ee/src/system/base_system_functions.rs",
        ):
            self.assertIn(tagged_path, applicator)

        patch = (
            REPO_ROOT
            / "scripts"
            / "patches"
            / "zksync-os-syscoin-v0.4.0.patch"
        ).read_text(encoding="utf-8")
        self.assertNotIn("canonical_upgrade_tx_hash", patch)
        self.assertNotIn("canonical upgrade tx hash", patch)

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
        self.assertIn(rust_address_bytes(PUBLISHED_GAS_TANK), normalized_patch)

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
        self.assertIn("verify_syscoin_source", build_rs)
        self.assertIn("require_source_sha256", build_rs)
        self.assertIn("expected one canonical forward_system source", build_rs)
        self.assertIn(PUBLISHED_EDGE_SOURCE_SHA256, build_rs)
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
            'EXPECTED_PATCH_SIZE="619638"',
            'EXPECTED_PATCH_SHA256="9c6cfd173e72ef8f03daa84ebab91301395991fe108d8563629e51d9a268f5e7"',
            'EXPECTED_PATCH_PATH_COUNT="65"',
            'EXPECTED_PATCH_PATHS_SHA256="8649c1aea0b303e6284d9ab26aff4641260aff9f6ce6ce3e2f5556331af3b3b0"',
            'STOCK_APP_VK_HASH="0x9f7576b911e7d3f528d49f894208682c81800814db9e3beac7fc3b1c4d626e7a"',
            "uint32 internal constant CANONICAL_ZKSYNC_OS_VERIFIER_VERSION = 8;",
            "if (version != CANONICAL_ZKSYNC_OS_VERIFIER_VERSION) {",
            "_verifySyscoinEdgeDARefs(_newBatch.edgeDARefsInput, _newBatch.edgeDARefsRoot);",
            "stock verifier artifact rejected",
            "canonical V8 VK regeneration required",
            "no app-bound security100 verifier hashes are approved",
            "disabled legacy path unexpectedly exists",
        ):
            self.assertIn(expected, helper)

        pending_gate = helper.rindex("\nverify_verifier_artifacts_pending\n")
        self.assertLess(pending_gate, helper.index("submodule sync\n"))
        self.assertLess(pending_gate, helper.index('apply --recount --whitespace'))

        for path, digest in (
            (
                "da-contracts/contracts/SyscoinL1DAValidatorZKsyncOS.sol",
                "80ece9ccf2a1193ace6f64148609c6b5d470337674de5d0d0f9ba28a746ea9b1",
            ),
            (
                "l1-contracts/contracts/state-transition/data-availability/SyscoinRelayedSLDAValidator.sol",
                "df0fa15a11933918c3964ddfdab5d1cb68505c2cc84623cb9196a57454e24ace",
            ),
            (
                "l1-contracts/contracts/state-transition/data-availability/SyscoinRollupDAManager.sol",
                "a0f629313ab6ab9eb3f5f015e71aa3c32ca6396677633558f4baec7249e3e504",
            ),
            (
                "l1-contracts/contracts/state-transition/verifiers/ZKsyncOSDualVerifier.sol",
                "1d82642c805eb5bbc70b344e66c534b1838b862834241aeaecb58338ff2d9f48",
            ),
            (
                "l1-contracts/contracts/state-transition/chain-deps/facets/Admin.sol",
                "14497f9b115ef308207a7a8f745694d3a10746d7692abfa0ac8a0fb41d25b155",
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

        # SYSCOIN: zkOS FFLONK is verification-dead and must be deleted by the source patch;
        # generic Era FFLONK artifacts remain outside this zkOS-only denylist.
        for disabled_verifier_artifact in (
            "l1-contracts/contracts/state-transition/verifiers/ZKsyncOSVerifierFflonk.sol",
            "tools/verifier-gen/data/ZKsyncOS_fflonk_scheduler_key.json",
        ):
            self.assertIn(disabled_verifier_artifact, helper)
            self.assertIn(f"diff --git a/{disabled_verifier_artifact}", patch)

        self.assertIn(
            "diff --git a/da-contracts/contracts/SyscoinL1DAValidatorZKsyncOS.sol",
            patch,
        )
        self.assertIn(
            "diff --git a/l1-contracts/contracts/state-transition/data-availability/SyscoinRelayedSLDAValidator.sol",
            patch,
        )
        self.assertIn(
            "diff --git a/l1-contracts/test/foundry/l1/unit/concrete/BatchProcessing/Committing.t.sol",
            patch,
        )
        self.assertIn("deleted file mode 100644", patch)


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
    ) -> subprocess.CompletedProcess[str]:
        source = temp_path / "source"
        source.mkdir()
        (source / "Cargo.toml").write_text(
            cargo_toml or self.fixture_cargo_toml(), encoding="utf-8"
        )
        (source / "Cargo.lock").write_text(
            cargo_lock or self.fixture_cargo_lock(), encoding="utf-8"
        )
        (source / "copied-marker").write_text("yes\n", encoding="utf-8")
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
            self.assertNotIn(' "c-kzg",', callable_block)
            self.assertIn('name = "c-kzg"\nversion = "2.1.8"', rewritten_lock)
            self.assertNotIn(
                f"git+{OFFICIAL_OS_URL}.git?tag={FINAL_OS_TAG}#{FINAL_LOCKED_REV}",
                rewritten_lock,
            )
            self.assertEqual((run / "copied-marker").read_text(), "yes\n")

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

    def test_rejects_unexpected_callable_oracles_lock_shape(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            cargo_lock = self.fixture_cargo_lock().replace(' "c-kzg",\n', "")
            result = self.run_rewrite(Path(temp), cargo_lock=cargo_lock)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("exactly one direct c-kzg edge", result.stderr)


if __name__ == "__main__":
    unittest.main()
