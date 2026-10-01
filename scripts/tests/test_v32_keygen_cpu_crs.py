"""No-download regressions for the Security100 CPU CRS and its workflow gates."""

import os
from pathlib import Path
import re
import subprocess
import textwrap
import unittest


WORKFLOW = Path(__file__).resolve().parents[2] / ".github/workflows/syscoin-v32-v8-keygen.yml"
EXPECTED = {
    "CRS_URL": "https://storage.googleapis.com/matterlabs-setup-keys-us/setup-keys/"
               "setup_2%5E25.key?generation=1682086010630627",
    "CRS_SIZE": "2147483920",
    "CRS_SHA256": "021fcc36428ff74352a94ff9ccdd6ef234e99e33e670c2507612994849aa1421",
    "CRS_MD5": "6f8362e49c0dce401bed2cebcf090b06",
}


class V32KeygenCpuCrsTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.workflow = WORKFLOW.read_text()
        cls.pins = dict(re.findall(r'^  (CRS_\w+): "?([^"\n]+)"?$', cls.workflow, re.MULTILINE))
        cls.download_blocks = [
            block for block in cls.workflow.split("      - name: ")[1:]
            if '"${CRS_URL}"' in block
        ]

    def test_exact_immutable_cpu_crs_pins(self):
        self.assertEqual(self.pins, EXPECTED)
        self.assertNotIn("setup_compact", self.workflow)
        self.assertNotIn("setup_2%5E24.key", self.workflow)

    def test_uncompressed_capacity_is_security100_not_security80(self):
        # Bellman's full encoding has two u64 counts, 64-byte G1 points and
        # 128-byte G2 points. Security100 needs 2^25 G1 entries; the upstream
        # Security80 README's 2^24 example cannot back its degree-25 setup.
        g1_count, g2_count = 1 << 25, 2
        count_bytes, g1_bytes, g2_bytes = 8, 64, 128
        self.assertEqual(int(self.pins["CRS_SIZE"]),
                         count_bytes + g1_count * g1_bytes + count_bytes + g2_count * g2_bytes)
        self.assertGreater(int(self.pins["CRS_SIZE"]),
                           count_bytes + (1 << 24) * g1_bytes + count_bytes + g2_count * g2_bytes)
        self.assertIn('grep -Fqx \'default = ["security_100"]\'', self.workflow)
        self.assertIn("Security100 needs the full Bellman 2^25 CRS", self.workflow)

    def test_both_download_lanes_attest_before_cpu_keygen(self):
        self.assertEqual(len(self.download_blocks), 2)
        for block in self.download_blocks:
            with self.subTest(step=block.splitlines()[0]):
                self.assertIn("--fail", block)
                self.assertIn("--proto '=https'", block)
                checks = re.findall(r'^          \[\[ .*\$\{CRS_(?:SIZE|SHA256|MD5)\}.*$',
                                    block, re.MULTILINE)
                self.assertEqual(len(checks), 3)
                for pin in ("CRS_SIZE", "CRS_SHA256", "CRS_MD5"):
                    self.assertEqual(sum("${" + pin + "}" in check for check in checks), 1)
                self.assertNotIn("generate-vk", block)
                self.assertNotIn("|| true", "\n".join(checks))
        candidate = self.download_blocks[0]
        self.assertIn('crs_part="${WORK_DIR}/setup_cpu.key.part"', candidate)
        self.assertIn('crs_file="${WORK_DIR}/setup_cpu.key"', candidate)
        self.assertLess(candidate.index('"${CRS_MD5}"'),
                        candidate.index('mv "${crs_part}" "${crs_file}"'))

    def test_actual_attestation_predicates_reject_each_wrong_pin(self):
        # Execute the production shell predicates with tiny command doubles;
        # this never downloads, allocates or reads a multi-gigabyte CRS.
        commands = textwrap.dedent("""\
            stat() { printf '%s\\n' "${ACTUAL_SIZE}"; }
            sha256sum() { printf '%s  fixture\\n' "${ACTUAL_SHA256}"; }
            md5sum() { printf '%s  fixture\\n' "${ACTUAL_MD5}"; }
        """)
        for block in self.download_blocks:
            checks = re.findall(
                r'^          \[\[ .*\$\{CRS_(?:SIZE|SHA256|MD5)\}.*$', block, re.MULTILINE)
            for mismatch in (None, "SIZE", "SHA256", "MD5"):
                with self.subTest(step=block.splitlines()[0], mismatch=mismatch):
                    env = {**os.environ, **EXPECTED, "WORK_DIR": "/unused", "crs_part": "/unused",
                           **{"ACTUAL_" + key.removeprefix("CRS_"): value
                              for key, value in EXPECTED.items() if key != "CRS_URL"}}
                    if mismatch:
                        env["ACTUAL_" + mismatch] = "incorrect"
                    # Check each predicate's status directly, including on
                    # macOS Bash 3.2, whose errexit ignores intermediate [[ ]].
                    for check in checks:
                        result = subprocess.run(
                            ["bash", "-euo", "pipefail", "-c", commands + check],
                            env=env, capture_output=True, text=True, timeout=5, check=False,
                        )
                        should_pass = mismatch is None or "${CRS_" + mismatch + "}" not in check
                        self.assertEqual(result.returncode == 0, should_pass, result.stderr)

    def test_candidate_and_stock_use_same_cpu_crs_with_checked_aux_params(self):
        subshells = re.findall(r"(?ms)^          \(\n.*?^          \)\n", self.workflow)
        keygen = [block for block in subshells if "generate-vk" in block]
        self.assertEqual(len(keygen), 2)
        for block in keygen:
            with self.subTest(block=block):
                self.assertIn('--trusted-setup "${WORK_DIR}/setup_cpu.key"', block)
                self.assertIn("--check-aux-params", block)
                self.assertIn("--locked", block)
                self.assertNotIn("--features", block)
                self.assertNotIn("--no-default-features", block)
                self.assertNotIn("security_80", block)
                self.assertNotIn("setup_compact", block)

    def test_stock_control_and_candidate_identity_gates_are_not_bypassed(self):
        self.assertIn('[[ "${stock_commitment}" == "${STOCK_SECURITY100_COMMITMENT}" ]]', self.workflow)
        self.assertIn('[[ "${hashes[0]}" == "${STOCK_APP_VK_HASH}" ]]', self.workflow)
        self.assertIn('[[ "${hashes[0]}" != "${STOCK_APP_VK_HASH}" ]]', self.workflow)
        self.assertIn('[[ "${commitment}" == "${SECURITY100_COMMITMENT}" ]]', self.workflow)
        self.assertIn('if [[ "${APP_IDENTITY_STATUS}" != "attested" ]]; then', self.workflow)
        self.assertIn('if [[ "${GATEWAY_TARGET_IDENTITY_STATUS}" != "attested" ]]; then', self.workflow)


if __name__ == "__main__":
    unittest.main()
