#!/usr/bin/env python3
"""Test that optional external tracking rejects incomplete credentials."""

import importlib.util
import itertools
import os
from pathlib import Path
import subprocess
import sys
import unittest

SCRIPT = Path(__file__).with_name("dependency-track-config.py")
SPEC = importlib.util.spec_from_file_location("dependency_track_config", SCRIPT)
config = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(config)


class DependencyTrackConfigTests(unittest.TestCase):
    def test_unconfigured_or_complete_and_every_partial_combination(self):
        names = ("DT_HOSTNAME", "DT_API_KEY", "DT_PARENT_UUID")
        for flags in itertools.product((False, True), repeat=3):
            with self.subTest(flags=flags):
                env = {name: "present" for name, present in zip(names, flags) if present}
                if all(flags):
                    self.assertTrue(config.enabled(env))
                elif not any(flags):
                    self.assertFalse(config.enabled(env))
                else:
                    with self.assertRaisesRegex(ValueError, "must all be configured"):
                        config.enabled(env)

    def test_workflow_preserves_sboms_when_external_tracking_is_unavailable(self):
        workflow = (SCRIPT.parents[1] / "workflows/sbom-analysis-reusable.yaml").read_text()
        code = workflow.split("  sbom-code:\n", 1)[1].split("  sbom-image:\n", 1)[0]
        image = workflow.split("  sbom-image:\n", 1)[1]
        for job in (code, image):
            self.assertIn("needs: [identity, dependency-track-config, dependency-track-project]", job)
            self.assertIn("!cancelled() && needs.identity.result == 'success'", job)
            self.assertIn("if: ${{ needs.dependency-track-project.result == 'success' }}", job)
            self.assertNotIn("continue-on-error", job)
            self.assertLess(job.index("uses: actions/upload-artifact@"),
                            job.index("name: Upload"))
        self.assertIn("external vulnerability tracking was not run", workflow)

    def test_partial_configuration_does_not_print_the_key(self):
        key = "unique-dummy-key-do-not-echo"
        env = {**os.environ, "DT_HOSTNAME": "", "DT_API_KEY": key, "DT_PARENT_UUID": ""}
        result = subprocess.run([sys.executable, "-B", str(SCRIPT)], env=env,
                                text=True, capture_output=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertNotIn(key, result.stdout + result.stderr)


if __name__ == "__main__":
    unittest.main()
