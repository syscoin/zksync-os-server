from __future__ import annotations

import ast
import json
import os
import stat
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

import yaml

ROOT = Path(__file__).resolve().parents[2]
LAUNCHER = ROOT / "scripts/gateway-launch"
GENERATOR = LAUNCHER / "generate-os-server-configs.sh"
COMMON = LAUNCHER / "_common.sh"
RUNNER = LAUNCHER / "run-os-server-with-patched-zksync-os.sh"


class BitcoinDaCookieConfigTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.directory = Path(temporary.name)
        self.config = self.directory / "config.yaml"
        self.cookie = self.directory / "testnet3" / ".cookie"
        self.cookie.parent.mkdir()
        source = GENERATOR.read_text(encoding="utf-8")
        code = source.split("python3 - <<'PY'\n", 1)[1].split("\nPY\n", 1)[0]
        tree = ast.parse(code)
        names = {"yaml_scalar", "require_regular_file", "write_secret_text"}
        functions = [n for n in tree.body if isinstance(n, ast.FunctionDef) and n.name in names]
        self.assertEqual({n.name for n in functions}, names)
        self.helpers = {"Path": Path, "json": json, "os": os, "stat": stat, "check_only": True}
        exec(compile(ast.Module(body=functions, type_ignores=[]), str(GENERATOR), "exec"), self.helpers)
        self.url_renderer = next(
            n for n in ast.walk(tree)
            if isinstance(n, ast.JoinedStr) and isinstance(n.values[0], ast.Constant)
            and n.values[0].value.startswith("  bitcoin_da_rpc_url: ")
        )
        self.auth_renderer = next(
            n for n in ast.walk(tree)
            if isinstance(n, ast.IfExp) and isinstance(n.body, ast.List) and n.body.elts
            and isinstance(n.body.elts[0], ast.JoinedStr)
            and n.body.elts[0].values[0].value.startswith("  bitcoin_da_cookie_file: ")
        )

    def render(self, url, *, user="", password="", cookie=""):
        with patch.dict(
            os.environ,
            BITCOIN_DA_RPC_URL=url,
            BITCOIN_DA_RPC_USER=user,
            BITCOIN_DA_RPC_PASSWORD=password,
            BITCOIN_DA_COOKIE_FILE=str(cookie),
        ):
            url_line = eval(compile(ast.Expression(self.url_renderer), str(GENERATOR), "eval"), self.helpers)
            auth_lines = eval(compile(ast.Expression(self.auth_renderer), str(GENERATOR), "eval"), self.helpers)
        return "\n".join(["batcher:", url_line, *auth_lines, ""])

    def validate(self, url, *, user="", password="", cookie=""):
        return subprocess.run(
            ["bash", "-euc", 'source "$1"; gl_validate_bitcoin_da_credentials', "test", str(COMMON)],
            env={
                **os.environ,
                "BITCOIN_DA_RPC_URL": url,
                "BITCOIN_DA_RPC_USER": user,
                "BITCOIN_DA_RPC_PASSWORD": password,
                "BITCOIN_DA_COOKIE_FILE": str(cookie),
                "SYSCOIN_DATADIR": str(self.directory),
                "SYSCOIN_NETWORK": "testnet3",
            },
            text=True,
            capture_output=True,
            check=False,
        )

    def check(self, expected):
        self.helpers["write_secret_text"](self.config, expected)

    def test_cookie_mode_renders_only_the_absolute_path(self):
        url = "http://127.0.0.1:8370"
        self.cookie.write_text("__cookie__:first\n", encoding="utf-8")
        expected = self.render(url, cookie=self.cookie)
        self.config.write_text(expected, encoding="utf-8")
        self.config.chmod(0o600)
        self.cookie.write_text("__cookie__:rotated\n", encoding="utf-8")
        self.check(self.render(url, cookie=self.cookie))
        self.assertEqual(
            yaml.safe_load(expected)["batcher"],
            {
                "bitcoin_da_rpc_url": url,
                "bitcoin_da_cookie_file": str(self.cookie),
                "bitcoin_da_cookie_rpc_url": url,
            },
        )
        result = self.validate(url, cookie=self.cookie)
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_explicit_https_provider_works_with_a_local_cookie_present(self):
        self.cookie.write_text("__cookie__:local-only\n", encoding="utf-8")
        url = "https://core.example.com:8370"
        user, password = "unit\r-user", "pass:'\"\\\n\t\r雪"
        result = self.validate(url, user=user, password=password)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(
            yaml.safe_load(self.render(url, user=user, password=password))["batcher"],
            {"bitcoin_da_rpc_url": url, "bitcoin_da_rpc_user": user, "bitcoin_da_rpc_password": password},
        )

    def test_blocks_incompatible_authentication_and_transport_configurations(self):
        for url, auth, error in (
            ("https://core.example.com", {"cookie": self.cookie}, "requires a loopback IP"),
            ("http://core.example.com", {"user": "user", "password": "password"}, "requires HTTPS"),
            ("http://127.0.0.1", {"cookie": self.cookie, "user": "user"}, "choose either"),
            ("http://127.0.0.1", {"cookie": ".cookie"}, "must be absolute"),
            ("http://127.0.0.1", {"user": "user"}, "BITCOIN_DA_RPC_PASSWORD"),
            ("http://127.0.0.1", {}, "BITCOIN_DA_RPC_USER"),
        ):
            with self.subTest(url=url, error=error):
                result = self.validate(url, **auth)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn(error, result.stderr)

    def test_direct_runner_keeps_layered_configs_read_only(self):
        self.cookie.write_text("__cookie__:local-only\n", encoding="utf-8")
        self.config.write_text(self.render("https://core.example.com", user="user", password="pass"), encoding="utf-8")
        self.config.chmod(0o644)
        overlay = self.directory / "overlay.yaml"
        overlay.write_text("batcher:\n  bitcoin_da_rpc_password: overlay\n", encoding="utf-8")
        before = [(p.read_bytes(), p.stat().st_mode, p.stat().st_mtime_ns) for p in (self.config, overlay)]
        result = subprocess.run(
            ["bash", str(RUNNER), "gateway", "--", "exec-prebuilt", "--",
             f"--config={self.config}:{overlay}"],
            env={
                **os.environ,
                "BITCOIN_DA_COOKIE_FILE": str(self.cookie),
                "GATEWAY_DIR": str(self.directory),
                "PROTOCOL_VERSION": "test-runner",
                "ZKSYNC_OS_FORCE_PATCHED_WORKSPACE": "false",
                "ZKSYNC_OS_SERVER_PATH": str(ROOT),
            },
            text=True,
            capture_output=True,
            check=False,
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("prebuilt zksync-os-server binary is missing", result.stderr)
        after = [(p.read_bytes(), p.stat().st_mode, p.stat().st_mtime_ns) for p in (self.config, overlay)]
        self.assertEqual(before, after)

    def test_check_only_still_requires_exact_secret_config_bytes(self):
        expected = self.render("http://127.0.0.1", user="unit-user", password="unit-password")
        different = expected.replace('"unit-user"', "'unit-user'")
        self.assertEqual(yaml.safe_load(expected), yaml.safe_load(different))
        self.config.write_text(different, encoding="utf-8")
        self.config.chmod(0o600)
        before = self.config.stat()
        with self.assertRaisesRegex(SystemExit, "materialized secret config differs"):
            self.check(expected)
        self.assertEqual(self.config.read_text(encoding="utf-8"), different)
        self.assertEqual(self.config.stat().st_mtime_ns, before.st_mtime_ns)

    def test_check_only_keeps_secret_mode_requirement(self):
        expected = self.render("http://127.0.0.1", cookie=self.cookie)
        self.config.write_text(expected, encoding="utf-8")
        self.config.chmod(0o644)
        with self.assertRaisesRegex(SystemExit, "unsafe ownership/mode"):
            self.check(expected)
        self.assertEqual(stat.S_IMODE(self.config.stat().st_mode), 0o644)


if __name__ == "__main__":
    unittest.main()
