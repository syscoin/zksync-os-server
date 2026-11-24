# SYSCOIN: Bounded private-file regressions, with no deployment or live RPC.
from __future__ import annotations

import ast
import importlib.util
import os
import re
import stat
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch


ROOT = Path(__file__).resolve().parents[2]
LOG_HELPER = ROOT / "scripts/gateway-launch/_private_log.py"
LAUNCHER = ROOT / "scripts/gateway-launch/run-gateway-launch.sh"
EXPLORER = ROOT / "scripts/explorer/blockscout/deploy-zksys-en-rpc.sh"


def load_log_helper():
    spec = importlib.util.spec_from_file_location("private_launch_log", LOG_HELPER)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def load_explorer_helpers():
    source = EXPLORER.read_text(encoding="utf-8")
    start = source.index("import base64\n", source.index('"${ZKSYS_GAS_TANK_ADDRESS}" <<\'PY\''))
    code = source[start:source.index("\nPY\n", start)]
    tree = ast.parse(code)
    selected = ast.Module(
        body=[node for node in tree.body if isinstance(node, (ast.Import, ast.ImportFrom, ast.FunctionDef))],
        type_ignores=[],
    )
    namespace = {}
    exec(compile(selected, str(EXPLORER), "exec"), namespace)
    return namespace


class PrivateLaunchFileTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.directory = Path(self.temporary.name).resolve()
        self.log = load_log_helper()
        self.explorer = load_explorer_helpers()

    def test_log_private_at_creation_even_with_permissive_umask(self):
        old_umask = os.umask(0)
        self.addCleanup(os.umask, old_umask)
        path = self.directory / "launch.log"
        descriptor = self.log.open_private_log(path)
        try:
            self.assertEqual(stat.S_IMODE(os.fstat(descriptor).st_mode), 0o600)
            self.assertEqual(path.read_bytes(), b"")
            os.write(descriptor, b"operator output\n")
        finally:
            os.close(descriptor)
        self.assertEqual(path.read_bytes(), b"operator output\n")

    def test_existing_private_log_is_supported_but_unsafe_files_are_not_truncated(self):
        path = self.directory / "launch.log"
        path.write_text("previous log", encoding="utf-8")
        path.chmod(0o600)
        os.close(self.log.open_private_log(path))
        self.assertEqual(path.read_text(), "")
        path.write_text("keep this", encoding="utf-8")
        path.chmod(0o644)
        with self.assertRaises(ValueError):
            self.log.open_private_log(path)
        self.assertEqual(path.read_text(), "keep this")

    def test_log_rejects_symlink_hardlink_fifo_and_writable_parent(self):
        target = self.directory / "target"
        target.write_text("untouched", encoding="utf-8")
        target.chmod(0o600)
        link = self.directory / "link"
        link.symlink_to(target)
        with self.assertRaises(OSError):
            self.log.open_private_log(link)
        link.unlink()
        os.link(target, link)
        with self.assertRaises(ValueError):
            self.log.open_private_log(link)
        link.unlink()
        os.mkfifo(link)
        with self.assertRaises(OSError):
            self.log.open_private_log(link)
        self.directory.chmod(0o777)
        with self.assertRaises(ValueError):
            self.log.open_private_log(self.directory / "other.log")
        self.directory.chmod(0o700)
        self.assertEqual(target.read_text(), "untouched")

    def test_log_descriptor_survives_reexec_and_descriptor_writer(self):
        path = self.directory / "launch.log"
        result = subprocess.run(
            [sys.executable, str(LOG_HELPER), str(path), "bash", "-c",
             'printf "normal launch output\\n" | "$1" "$2" --tee "${GATEWAY_LAUNCH_LOG_FD}"',
             "test-log", sys.executable, str(LOG_HELPER)],
            capture_output=True, text=True,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, "normal launch output\n")
        self.assertEqual(path.read_text(), result.stdout)
        self.assertEqual(stat.S_IMODE(path.stat().st_mode), 0o600)

    def test_public_raw_funder_requires_an_explicit_non_development_key(self):
        common = LAUNCHER.with_name("_common.sh")
        development_key = re.search(r"local development_key=(0x[0-9a-f]{64})", common.read_text())[1]
        env = {
            "PATH": os.environ["PATH"],
            "COMMON": str(common),
            "FUNDER_SIGNER": "private-key",
            "GATEWAY_ALLOW_INSECURE_PRIVATE_KEY_ARGV": "true",
        }
        command = [
            "bash", "-ec",
            'source "$COMMON"; gl_validate_funder_signer_config; test -n "$FUNDER_PRIVATE_KEY"',
        ]
        for network, chain in (("tanenbaum", "5700"), ("mainnet", "57")):
            for key, error in (
                (None, "unset required env: FUNDER_PRIVATE_KEY"),
                ("", "unset required env: FUNDER_PRIVATE_KEY"),
                (development_key.upper(), "Anvil development key"),
            ):
                with self.subTest(network=network, error=error):
                    overrides = {} if key is None else {"FUNDER_PRIVATE_KEY": key}
                    result = subprocess.run(
                        command, env={**env, **overrides, "L1_NETWORK": network, "L1_CHAIN_ID": chain},
                        capture_output=True, text=True,
                    )
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn(error, result.stderr)
            result = subprocess.run(
                command, env={**env, "FUNDER_PRIVATE_KEY": "0x" + "4" * 64,
                              "L1_NETWORK": network, "L1_CHAIN_ID": chain},
                capture_output=True, text=True,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
        result = subprocess.run(
            command, env={**env, "L1_NETWORK": "localhost", "L1_CHAIN_ID": "31337"},
            capture_output=True, text=True,
        )
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_public_funding_rejects_imported_development_account_before_rpc(self):
        common = LAUNCHER.with_name("_common.sh")
        wallets = self.directory / "wallets.yaml"
        wallets.write_text("governor:\n  address: '0x" + "5" * 40 + "'\n")
        cast_calls = self.directory / "cast-calls"
        cast = self.directory / "cast"
        cast.write_text(
            '#!/usr/bin/env bash\nprintf "%s\n" "$1" >> "$CAST_CALLS"\n'
            '[[ "$1" == "wallet" ]] || exit 1\n'
            'printf "0xf39fd6e51aad88f6f4ce6ab8827279cfffb92266\n"\n'
        )
        cast.chmod(0o700)
        result = subprocess.run(
            ["bash", "-ec", 'source "$COMMON"; gl_fund_wallets_yaml'],
            env={
                "PATH": str(self.directory) + os.pathsep + os.environ["PATH"],
                "PYTHONDONTWRITEBYTECODE": "1",
                "COMMON": str(common),
                "WALLETS_YAML_PATHS": str(wallets),
                "GATEWAY_DIR": str(self.directory),
                "L1_RPC_URL": "https://root.example.invalid",
                "L1_NETWORK": "tanenbaum",
                "L1_CHAIN_ID": "5700",
                "FUNDER_SIGNER": "ledger",
                "CAST_CALLS": str(cast_calls),
            },
            text=True, capture_output=True,
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("development account cannot fund a public network", result.stderr)
        self.assertEqual(cast_calls.read_text(), "wallet\n")

    def test_failed_rpc_diagnostic_omits_the_url(self):
        source = LAUNCHER.read_text(encoding="utf-8")
        start = source.index("wait_for_rpc() {")
        function = source[start:source.index("\n}\n", start) + 3]
        result = subprocess.run(
            ["bash", "-c", function + '\nseq() { echo 1; }; sleep() { :; }; '
             'json_rpc_hex_to_dec() { return 1; }; gl_die() { echo "$*"; return 1; }; wait_for_rpc'],
            env={**os.environ, "L1_RPC_URL": "https://user:credential@rpc.invalid/token"},
            capture_output=True, text=True,
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("L1 RPC not responding", result.stdout)
        self.assertNotIn("credential", result.stdout + result.stderr)
        self.assertNotIn("rpc.invalid", result.stdout + result.stderr)

    def test_secret_descriptor_is_private_before_writing_and_replacement_is_atomic(self):
        path = self.directory / "instance/config.yaml"
        mkstemp = tempfile.mkstemp
        created = []

        def checked_mkstemp(*args, **kwargs):
            descriptor, temporary = mkstemp(*args, **kwargs)
            self.assertEqual(stat.S_IMODE(os.fstat(descriptor).st_mode), 0o600)
            self.assertEqual(os.fstat(descriptor).st_size, 0)
            created.append(Path(temporary))
            return descriptor, temporary

        old_umask = os.umask(0)
        self.addCleanup(os.umask, old_umask)
        with patch.object(tempfile, "mkstemp", checked_mkstemp):
            self.explorer["write_secret"](path, "private content\n")
        self.assertEqual(path.read_text(), "private content\n")
        self.assertEqual(stat.S_IMODE(path.stat().st_mode), 0o600)
        self.assertEqual(stat.S_IMODE(path.parent.stat().st_mode), 0o700)
        self.assertTrue(all(not temporary.exists() for temporary in created))
        self.explorer["write_secret"](path, "replacement\n")
        self.assertEqual(path.read_text(), "replacement\n")

    def test_secret_failure_preserves_old_content_and_cleans_temporary_file(self):
        path = self.directory / "config.yaml"
        path.write_text("old content", encoding="utf-8")
        with patch.object(os, "replace", side_effect=OSError("bounded test failure")):
            with self.assertRaises(OSError):
                self.explorer["write_secret"](path, "new content")
        self.assertEqual(path.read_text(), "old content")
        self.assertEqual(list(self.directory.glob(".config.yaml.*")), [])

    def test_secret_directory_symlink_and_unsafe_existing_key_are_rejected(self):
        link = self.directory / "instance"
        link.symlink_to(self.directory, target_is_directory=True)
        with self.assertRaises(OSError):
            self.explorer["write_secret"](link / "config.yaml", "secret")
        path = self.directory / "network.secret"
        path.write_text("existing key", encoding="utf-8")
        path.chmod(0o644)
        with self.assertRaises(SystemExit):
            self.explorer["read_or_create_key"](path)
        path.chmod(0o600)
        self.assertEqual(self.explorer["read_or_create_key"](path), "existing key")


if __name__ == "__main__":
    unittest.main()
