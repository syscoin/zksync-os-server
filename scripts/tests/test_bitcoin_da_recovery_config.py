"""Recovery scheduling must not silently change the chain's acceptance policy."""
import ast
import json
import os
import subprocess
from pathlib import Path
import unittest
from unittest.mock import patch

import yaml

ROOT = Path(__file__).resolve().parents[2]
GENERATOR = ROOT / "scripts/gateway-launch/generate-os-server-configs.sh"
VARIABLE = "BITCOIN_DA_MISSING_BLOB_RECOVERY_TIMEOUT"
FIELD = "bitcoin_da_missing_blob_recovery_timeout"


class RecoveryConfigTests(unittest.TestCase):
    def setUp(self):
        source = GENERATOR.read_text()
        code = source.split("python3 - <<'PY'\n", 1)[1].split("\nPY\n", 1)[0]
        tree = ast.parse(code)
        scalar = next(n for n in tree.body if isinstance(n, ast.FunctionDef) and n.name == "yaml_scalar")
        self.helpers = {"json": json, "os": os}
        exec(compile(ast.Module(body=[scalar], type_ignores=[]), str(GENERATOR), "exec"), self.helpers)
        expressions = [n for n in ast.walk(tree) if isinstance(n, ast.IfExp)
                       and VARIABLE in ast.unparse(n) and FIELD in ast.unparse(n)]
        self.assertEqual(len(expressions), 1)
        self.expression = expressions[0]
        self.assertIn(f"export {VARIABLE}", source)
        da_only = next(n for n in ast.walk(tree) if isinstance(n, ast.If)
                       and isinstance(n.test, ast.Name) and n.test.id == "uses_syscoin_da_refs")
        self.assertIn(self.expression, list(ast.walk(da_only)))

    def render(self, value):
        with patch.dict(os.environ, {VARIABLE: value}):
            lines = eval(compile(ast.Expression(self.expression), str(GENERATOR), "eval"), self.helpers)
        return yaml.safe_load("batcher:\n  bitcoin_da_finality_mode: Confirmations\n"
                              "  bitcoin_da_finality_confirmations: 1\n" + "\n".join(lines))

    def test_omitted_override_preserves_existing_default(self):
        self.assertNotIn(FIELD, self.render("")["batcher"])

    def test_override_renders_without_changing_confirmation_policy(self):
        config = self.render("5m")["batcher"]
        self.assertEqual(config[FIELD], "5m")
        self.assertEqual(config["bitcoin_da_finality_mode"], "Confirmations")
        self.assertEqual(config["bitcoin_da_finality_confirmations"], 1)
        self.assertNotIn("bitcoin_da_finality_timeout", config)

    def test_override_cannot_inject_other_yaml_fields(self):
        value = "5m\n  bitcoin_da_finality_confirmations: 0"
        config = self.render(value)["batcher"]
        self.assertEqual(config[FIELD], value)
        self.assertEqual(config["bitcoin_da_finality_confirmations"], 1)

    def test_testnet_profiles_only_choose_shorter_initial_grace(self):
        for name in ("run-gateway-launch.sh", "gateway-launch-repair.sh"):
            source = (GENERATOR.parent / name).read_text()
            profiles = source.split('case "${L1_PROFILE}" in', 1)[1]
            testnet, mainnet = profiles.split("mainnet)", 1)
            self.assertIn(f': "${{{VARIABLE}:=5m}}"', testnet)
            self.assertNotIn(f"{VARIABLE}:=", mainnet.split(";;", 1)[0])

    def test_profile_default_preserves_explicit_operator_override(self):
        for name in ("run-gateway-launch.sh", "gateway-launch-repair.sh"):
            source = (GENERATOR.parent / name).read_text()
            assignment = next(line.strip() for line in source.splitlines()
                              if f"{VARIABLE}:=" in line)
            command = assignment + '; printf "%s" "$' + VARIABLE + '"'
            for configured, expected in ((None, "5m"), ("20m", "20m")):
                env = dict(os.environ)
                env.pop(VARIABLE, None)
                if configured is not None:
                    env[VARIABLE] = configured
                self.assertEqual(subprocess.check_output(["bash", "-c", command],
                                                         env=env, text=True), expected)


if __name__ == "__main__":
    unittest.main()
