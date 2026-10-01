"""No-build regressions for the production keygen workflow's PLONK inventory gate."""

import copy
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
import textwrap
import unittest


WORKFLOW = Path(__file__).resolve().parents[2] / ".github/workflows/syscoin-v32-v8-keygen.yml"
PLONK = "l1-contracts/ZKsyncOSVerifierPlonk"
DEPLOYER = "l1-contracts/GatewayCTMDeployerVerifiersZKsyncOS"
HASH_FIELDS = (
    "zkBytecodeHash",
    "evmBytecodeHash",
    "evmDeployedBytecodeHash",
    "evmDeployedBytecodeBlakeHash",
)
ENTRY_COUNT = 278


def inventory_row(name):
    contract = name.split("/")[-1]
    return {
        "contractName": name,
        "zkBytecodePath": f"/l1-contracts/zkout/{contract}.sol/{contract}.json",
        "evmBytecodePath": f"/l1-contracts/out/{contract}.sol/{contract}.json",
        **{field: "0x" + "11" * 32 for field in HASH_FIELDS},
        "evmDeployedBytecodeLength": 256,
    }


class V32KeygenInventoryClosureTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.workflow = WORKFLOW.read_text()
        match = re.search(
            r"(?ms)^          plonk_inventory_closure_snapshot\(\) \{\n.*?^          \}\n",
            cls.workflow,
        )
        if match is None:
            raise AssertionError("missing workflow inventory closure function")
        # Execute the production jq predicate itself, not a second implementation.
        cls.function = textwrap.dedent(match.group())
        normalize_match = re.search(
            r"(?ms)^          normalize_reviewed_hashes_index\(\) \{\n.*?^          \}\n",
            cls.workflow,
        )
        if normalize_match is None:
            raise AssertionError("missing workflow index normalization function")
        cls.normalize_function = textwrap.dedent(normalize_match.group())

    def setUp(self):
        self.reviewed = [inventory_row(PLONK), inventory_row(DEPLOYER)]
        self.reviewed.extend(
            inventory_row(f"l1-contracts/Unchanged{index}")
            for index in range(ENTRY_COUNT - len(self.reviewed))
        )

    def snapshot_result(self, inventory):
        return subprocess.run(
            ["bash", "-euo", "pipefail", "-c",
             self.function + "\nplonk_inventory_closure_snapshot /dev/stdin\n"],
            input=json.dumps(inventory),
            capture_output=True,
            text=True,
            env={**os.environ, "ERA_CONTRACT_HASH_ENTRY_COUNT": str(ENTRY_COUNT)},
            timeout=10,
            check=False,
        )

    def snapshot(self, inventory):
        result = self.snapshot_result(inventory)
        self.assertEqual(result.returncode, 0, result.stderr)
        return json.loads(result.stdout)

    def test_only_two_artifact_hash_and_length_changes_are_permitted(self):
        candidate = copy.deepcopy(self.reviewed)
        for row in candidate[:2]:
            row.update({field: "0x" + "22" * 32 for field in HASH_FIELDS})
            row["evmDeployedBytecodeLength"] += 17
        self.assertEqual(self.snapshot(candidate), self.snapshot(self.reviewed))

    def test_helper_hashes_may_remain_unchanged(self):
        candidate = copy.deepcopy(self.reviewed)
        candidate[0].update({field: "0x" + "33" * 32 for field in HASH_FIELDS})
        self.assertEqual(self.snapshot(candidate), self.snapshot(self.reviewed))
        candidate[1]["evmBytecodeHash"] = "0x" + "44" * 32
        self.assertEqual(self.snapshot(candidate), self.snapshot(self.reviewed))

    def test_reviewed_yul_names_can_repeat_at_distinct_artifact_paths(self):
        reviewed = copy.deepcopy(self.reviewed)
        reviewed[2]["contractName"] = "Bootloader"
        reviewed[3]["contractName"] = "Bootloader"
        self.assertEqual(len(self.snapshot(reviewed)), ENTRY_COUNT)
        reviewed[3]["zkBytecodePath"] = reviewed[2]["zkBytecodePath"]
        reviewed[3]["evmBytecodePath"] = reviewed[2]["evmBytecodePath"]
        self.assertNotEqual(self.snapshot_result(reviewed).returncode, 0)

    def test_every_other_row_and_closure_identity_field_stays_pinned(self):
        reviewed_snapshot = self.snapshot(self.reviewed)
        for index, field, value in (
            (2, "evmBytecodeHash", "0x" + "55" * 32),
            (2, "evmDeployedBytecodeLength", 257),
            (0, "evmBytecodePath", "/unexpected/artifact.json"),
            (1, "zkBytecodePath", "/unexpected/artifact.json"),
            (1, "unreviewedField", "unexpected"),
            (2, "contractName", "l1-contracts/Replacement"),
        ):
            with self.subTest(index=index, field=field):
                candidate = copy.deepcopy(self.reviewed)
                candidate[index][field] = value
                self.assertNotEqual(self.snapshot(candidate), reviewed_snapshot)

    def test_missing_extra_duplicate_and_replaced_contracts_fail_closed(self):
        duplicate = copy.deepcopy(self.reviewed)
        duplicate[2] = copy.deepcopy(duplicate[3])
        missing_helper = copy.deepcopy(self.reviewed)
        missing_helper[1]["contractName"] += "Other"
        duplicate_root = copy.deepcopy(self.reviewed)
        duplicate_root[1] = copy.deepcopy(duplicate_root[0])
        for inventory in (
            self.reviewed[:-1],
            self.reviewed + [inventory_row("l1-contracts/Extra")],
            duplicate,
            duplicate_root,
            missing_helper,
            {},
        ):
            with self.subTest(inventory_type=type(inventory).__name__):
                self.assertNotEqual(self.snapshot_result(inventory).returncode, 0)

    def test_both_rows_require_four_nonzero_canonical_hashes(self):
        for index in range(2):
            for field in HASH_FIELDS:
                for value in (None, "0x", "0x" + "00" * 32, "0x" + "AB" * 32):
                    with self.subTest(index=index, field=field, value=value):
                        candidate = copy.deepcopy(self.reviewed)
                        candidate[index][field] = value
                        self.assertNotEqual(self.snapshot_result(candidate).returncode, 0)

    def test_both_rows_require_positive_integral_artifact_lengths(self):
        for index in range(2):
            for value in (None, 0, -1, 1.5, "256", True):
                with self.subTest(index=index, value=value):
                    candidate = copy.deepcopy(self.reviewed)
                    candidate[index]["evmDeployedBytecodeLength"] = value
                    self.assertNotEqual(self.snapshot_result(candidate).returncode, 0)

    def test_workflow_compares_snapshots_and_keeps_build_and_primary_vk_gates(self):
        self.assertEqual(self.workflow.count(
            'plonk_inventory_closure_snapshot "${era_dir}/AllContractsHashes.json"'
        ), 2)
        self.assertIn(
            '[[ "${candidate_inventory_closure_sha256}" == \\\n'
            '            "${reviewed_inventory_closure_sha256}" ]]',
            self.workflow,
        )
        self.assertIn('ERA_CONTRACT_HASH_ENTRY_COUNT: "278"', self.workflow)
        self.assertIn(
            '[[ "${post_build_selectors_sha256}" == "${reviewed_selectors_sha256}" ]]',
            self.workflow,
        )
        self.assertLess(
            self.workflow.index("yarn calculate-hashes:check"),
            self.workflow.index('candidate_inventory_closure_sha256="$('),
        )
        for suffix in ("zk_hash", "evm_hash", "deployed_hash"):
            self.assertIn(
                f'[[ "${{candidate_plonk_{suffix}}}" != "${{reviewed_plonk_{suffix}}}" ]]',
                self.workflow,
            )
        self.assertIn(
            '[[ "${candidate_plonk_deployed_blake_hash}" != \\\n'
            '            "${reviewed_plonk_deployed_blake_hash}" ]]',
            self.workflow,
        )
        self.assertIn('cmp --silent "${generated_plonk}" "${contract_plonk}"', self.workflow)
        self.assertIn('[[ "${post_build_embedded_hash}" == "${syscoin_vk_hash}" ]]', self.workflow)
        self.assertIn('"${retained_fflonk_sha256}" ]]', self.workflow)

    def test_index_normalization_preserves_candidate_and_rejects_source_drift(self):
        for drift in (None, "source", "inventory_mode"):
            with self.subTest(drift=drift), tempfile.TemporaryDirectory() as directory:
                repo = Path(directory)

                def git(*args):
                    return subprocess.run(
                        ["git", "-C", str(repo), *args], check=True,
                        capture_output=True, text=True,
                    ).stdout.strip()

                git("init", "--quiet")
                git("config", "core.filemode", "true")
                inventory = repo / "AllContractsHashes.json"
                source = repo / "source.sol"
                inventory.write_text("reviewed inventory\n")
                source.write_text("reviewed source\n")
                git("add", "AllContractsHashes.json", "source.sol")
                reviewed_tree = git("write-tree")
                inventory.write_text("candidate inventory\n")
                if drift == "source":
                    source.write_text("unexpected source change\n")
                elif drift == "inventory_mode":
                    inventory.chmod(0o755)
                git("add", "AllContractsHashes.json", "source.sol")
                if drift == "inventory_mode":
                    git("update-index", "--chmod=+x", "AllContractsHashes.json")
                result = subprocess.run(
                    ["bash", "-euo", "pipefail", "-c", self.normalize_function + "\n"
                     'normalize_reviewed_hashes_index\n'
                     '[[ "$(git -C "${era_dir}" write-tree)" == "${ERA_SOURCE_PATCHED_TREE}" ]]\n'],
                    env={**os.environ, "era_dir": str(repo),
                         "ERA_SOURCE_PATCHED_TREE": reviewed_tree},
                    capture_output=True, text=True, timeout=10, check=False,
                )
                self.assertEqual(result.returncode == 0, drift is None, result.stderr)
                self.assertEqual(inventory.read_text(), "candidate inventory\n")
                if drift is None:
                    self.assertEqual(git("show", ":AllContractsHashes.json"), "reviewed inventory")

    def test_both_post_build_tree_attestations_normalize_only_inventory_index(self):
        self.assertEqual(self.workflow.count("          normalize_reviewed_hashes_index\n"), 2)
        early = self.workflow.index("# SYSCOIN: Re-attest bytes, not merely filenames")
        early_attestation = self.workflow.index(
            '[[ "$(git -C "${era_dir}" write-tree)" == "${ERA_SOURCE_PATCHED_TREE}" ]]',
            early,
        )
        self.assertLess(
            self.workflow.index("          normalize_reviewed_hashes_index\n", early),
            early_attestation,
        )
        self.assertIn("--cacheinfo", self.normalize_function)
        self.assertNotIn("checkout", self.normalize_function)
        self.assertNotIn("reset", self.normalize_function)

    def test_ignored_generated_plonk_is_explicitly_staged_before_path_check(self):
        command = 'git -C "${era_dir}" add --force -- "${generated_plonk_rel}"'
        self.assertEqual(self.workflow.count(command), 1)
        self.assertLess(self.workflow.index(command), self.workflow.index("mapfile -t actual_candidate_paths"))
        self.assertGreater(self.workflow.index(command), self.workflow.rindex("normalize_reviewed_hashes_index\n"))
        with tempfile.TemporaryDirectory() as directory:
            repo = Path(directory)
            generated_rel = "tools/verifier-gen/data/ZKsyncOSVerifierPlonk.sol"
            generated = repo / generated_rel
            generated.parent.mkdir(parents=True)
            (repo / ".gitignore").write_text("tools/verifier-gen/data/*.sol\n")
            generated.write_text("verified generated PLONK\n")
            other = generated.with_name("Unreviewed.sol")
            other.write_text("not part of the candidate\n")
            subprocess.run(["git", "-C", str(repo), "init", "--quiet"], check=True)
            subprocess.run(
                ["bash", "-euo", "pipefail", "-c", command],
                env={**os.environ, "era_dir": str(repo), "generated_plonk_rel": generated_rel},
                check=True, capture_output=True, text=True,
            )
            staged = subprocess.check_output(
                ["git", "-C", str(repo), "diff", "--cached", "--name-only"], text=True,
            ).splitlines()
            self.assertEqual(staged, [generated_rel])

    def test_main_stack_and_core_limits_are_scoped_to_both_keygen_processes(self):
        subshells = re.findall(r"(?ms)^          \(\n.*?^          \)\n", self.workflow)
        keygen_subshells = [block for block in subshells if "generate-vk" in block]
        self.assertEqual(len(keygen_subshells), 2)
        self.assertEqual(self.workflow.count("ulimit -S -s 300000"), 2)
        self.assertEqual(self.workflow.count("ulimit -c 0"), 2)
        for block in keygen_subshells:
            self.assertLess(block.index("ulimit -S -s 300000"), block.index("generate-vk"))
            self.assertLess(block.index("ulimit -c 0"), block.index("generate-vk"))
            self.assertIn('RUST_MIN_STACK=268435456 cargo "+${RUST_TOOLCHAIN}" run', block)
            self.assertIn("--check-aux-params", block)
            self.assertIn('--trusted-setup "${WORK_DIR}/setup_cpu.key"', block)
            self.assertNotIn("compute-aux-params", block)
            self.assertNotIn("vk-hash", block)
            self.assertNotIn("ulimit -H", block)


if __name__ == "__main__":
    unittest.main()
