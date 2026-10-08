# SYSCOIN: Exercise real renderer/refresh/check helpers using only temporary synthetic cookies.
from __future__ import annotations

import ast
import json
import os
import stat
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

import yaml

ROOT = Path(__file__).resolve().parents[2]
GENERATOR = ROOT / "scripts/gateway-launch/generate-os-server-configs.sh"
COMMON = ROOT / "scripts/gateway-launch/_common.sh"


class BitcoinDaCookieConfigTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.config = Path(temporary.name) / "config.yaml"
        self.cookie = Path(temporary.name) / ".cookie"
        source = GENERATOR.read_text(encoding="utf-8")
        code = source.split("python3 - <<'PY'\n", 1)[1].split("\nPY\n", 1)[0]
        tree = ast.parse(code)
        names = {"yaml_scalar", "require_regular_file", "write_secret_text"}
        functions = [n for n in tree.body if isinstance(n, ast.FunctionDef) and n.name in names]
        self.assertEqual({n.name for n in functions}, names)
        self.helpers = {"Path": Path, "json": json, "os": os, "stat": stat, "check_only": True}
        exec(compile(ast.Module(body=functions, type_ignores=[]), str(GENERATOR), "exec"), self.helpers)
        self.renderers = [n for n in ast.walk(tree) if isinstance(n, ast.JoinedStr)
                          and isinstance(n.values[0], ast.Constant)
                          and n.values[0].value.startswith(("  bitcoin_da_rpc_user: ", "  bitcoin_da_rpc_password: "))]
        self.assertEqual(len(self.renderers), 2)
        refresh = next(n.value for n in ast.walk(tree) if isinstance(n, ast.Assign)
                       and isinstance(n.value, ast.JoinedStr)
                       and any(isinstance(t, ast.Name) and t.id == "refresh_cookie_block" for t in n.targets))
        generated = eval(compile(ast.Expression(refresh), str(GENERATOR), "eval"), {"config_path": self.config})
        start_code = generated.split("<<'PYCOOKIE'\n", 1)[1].split("\nPYCOOKIE\n", 1)[0]
        common = COMMON.read_text(encoding="utf-8").split("gl_refresh_bitcoin_da_config_from_cookie() {", 1)[1]
        common_code = common.split("<<'PY'\n", 1)[1].split("\nPY\n", 1)[0]
        self.refreshers = {"normal-help": common_code, "canonical-start": start_code}

    def render(self, user, password):
        with patch.dict(os.environ, BITCOIN_DA_RPC_USER=user, BITCOIN_DA_RPC_PASSWORD=password):
            lines = [eval(compile(ast.Expression(n), str(GENERATOR), "eval"), self.helpers) for n in self.renderers]
        return "batcher:\n" + "\n".join(lines) + "\n  bitcoin_da_wallet_name: unit-only\n"

    def check(self, expected):
        self.helpers["write_secret_text"](self.config, expected)

    def test_generate_refresh_and_check_only_are_byte_idempotent(self):
        for user, password in [("unit-user", "unit-password"),
                               ("user'\"\\snow☃", "pass:'\"\\#雪"),
                               (" unit-user ", "prefix\n\t:trailer")]:
            for name, code in self.refreshers.items():
                with self.subTest(credentials=(user, password), refresher=name):
                    expected = self.render(user, password)
                    self.config.write_text(expected, encoding="utf-8")
                    self.config.chmod(0o600)
                    self.cookie.write_text(user + ":" + password + "\n", encoding="utf-8")
                    self.cookie.chmod(0o600)
                    for _ in range(2):
                        with patch.object(sys, "argv", ["source-refresh", str(self.config), str(self.cookie)]), patch("builtins.print"):
                            exec(compile(code, name, "exec"), {})
                        self.assertEqual(self.config.read_text(encoding="utf-8"), expected)
                        self.check(expected)
                    parsed = yaml.safe_load(expected)["batcher"]
                    self.assertEqual((parsed["bitcoin_da_rpc_user"], parsed["bitcoin_da_rpc_password"]), (user, password))
                    self.assertEqual(parsed["bitcoin_da_wallet_name"], "unit-only")

    def test_renderer_escapes_control_characters_without_cookie_transport(self):
        # Cookie read_text normalizes CR; test scalar escaping separately from that transport.
        user, password = "unit\r-user", "pass:'\"\\\n\t\r雪"
        parsed = yaml.safe_load(self.render(user, password))["batcher"]
        self.assertEqual((parsed["bitcoin_da_rpc_user"], parsed["bitcoin_da_rpc_password"]), (user, password))

    def test_check_only_still_rejects_semantically_equal_different_quotes(self):
        expected = self.render("unit-user", "unit-password")
        different = expected.replace('"unit-user"', "'unit-user'").replace('"unit-password"', "'unit-password'")
        self.assertEqual(yaml.safe_load(expected), yaml.safe_load(different))
        self.config.write_text(different, encoding="utf-8")
        self.config.chmod(0o600)
        before = self.config.stat()
        with self.assertRaisesRegex(SystemExit, "materialized secret config differs"):
            self.check(expected)
        self.assertEqual(self.config.read_text(encoding="utf-8"), different)
        self.assertEqual(self.config.stat().st_mtime_ns, before.st_mtime_ns)

    def test_check_only_rejects_changed_credentials_without_rewriting(self):
        expected = self.render("unit-user", "unit-password")
        changed = self.render("unit-user", "different-password")
        self.config.write_text(changed, encoding="utf-8")
        self.config.chmod(0o600)
        with self.assertRaisesRegex(SystemExit, "materialized secret config differs"):
            self.check(expected)
        self.assertEqual(self.config.read_text(encoding="utf-8"), changed)

    def test_check_only_keeps_secret_mode_requirement(self):
        expected = self.render("unit-user", "unit-password")
        self.config.write_text(expected, encoding="utf-8")
        self.config.chmod(0o644)
        with self.assertRaisesRegex(SystemExit, "unsafe ownership/mode"):
            self.check(expected)
        self.assertEqual(stat.S_IMODE(self.config.stat().st_mode), 0o644)


if __name__ == "__main__":
    unittest.main()
