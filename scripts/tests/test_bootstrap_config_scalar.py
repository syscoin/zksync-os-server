"""SYSCOIN: Terminal persistence must not rewrite deployed genesis/bytecode fields."""
import importlib.util
from pathlib import Path
import os
import stat
import tempfile
import unittest

import yaml

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("contracts_scalar", ROOT / "scripts/gateway-launch/_contracts_yaml_scalar.py")
helper = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(helper)
ADDRESS = "0xb49943ea232624dd4aa63e18186076c6c99a68ef"
FIELD = b'  zksys_gas_tank_addr: "' + ADDRESS.encode() + b'"\n'


class BootstrapScalarTests(unittest.TestCase):
    def test_huge_hex_failure_is_not_reintroduced(self):
        raw = b"genesis: 0x" + b"f" * 20000 + b"\nl2:\n  keep: yes\n"
        # The original terminal safe_load/safe_dump fails only after the sends.
        with self.assertRaises(ValueError):
            yaml.safe_dump(yaml.safe_load(raw))
        self.assertEqual(helper.updated_bytes(raw, ADDRESS), raw.replace(b"l2:\n", b"l2:\n" + FIELD))

    def test_crlf_unicode_and_comments_are_byte_preserved(self):
        raw = "# café\r\ngenesis: 0xffff\r\nl2: # retain this\r\n  keep: true # retain too\r\n".encode()
        expected = raw.replace(b"l2: # retain this\r\n", b"l2: # retain this\r\n" + FIELD.replace(b"\n", b"\r\n"))
        self.assertEqual(helper.updated_bytes(raw, ADDRESS), expected)

    def test_existing_scalar_replaces_only_its_token(self):
        raw = b"l2:\n  zksys_gas_tank_addr: '0x1111111111111111111111111111111111111111' # note\n  x: 0xffff\n"
        self.assertEqual(helper.updated_bytes(raw, ADDRESS), raw.replace(b"'0x1111111111111111111111111111111111111111'", b'"' + ADDRESS.encode() + b'"'))

    def test_existing_canonical_scalar_is_noop(self):
        raw = b"l2:\n" + FIELD + b"  x: 0xfeed\n"
        self.assertEqual(helper.updated_bytes(raw, ADDRESS), raw)

    def test_missing_l2_adds_only_that_path(self):
        raw = b"genesis: 0xffff # tail"
        self.assertEqual(helper.updated_bytes(raw, ADDRESS), raw + b"\nl2:\n" + FIELD)

    def test_empty_flow_l2_preserves_other_tokens(self):
        raw = b"genesis: 0xffff\nl2: { } # same comment\n"
        self.assertEqual(helper.updated_bytes(raw, ADDRESS), raw.replace(b"{ }", b'{ zksys_gas_tank_addr: "' + ADDRESS.encode() + b'"}'))

    def test_duplicate_and_unsupported_shapes_fail_before_write(self):
        for raw in (b"l2: {}\nl2: {}\n", b"l2:\n  x: 1\n  x: 2\n", b"l2: []\n", b"l2: null\n", b"l2: {x: 1}\n", b"l2:\n  zksys_gas_tank_addr: []\n", b"x: &alias 1\nl2: {}\n", b"l2: {}\n---\nl2: {}\n", b"l2: {}\n...\n", b"x: !custom value\nl2: {}\n"):
            with self.subTest(raw=raw), self.assertRaises((ValueError, yaml.YAMLError)):
                helper.updated_bytes(raw, ADDRESS)

    def test_reserved_or_malformed_addresses_fail(self):
        for addr in ("0x" + "0" * 40, "0x" + "0" * 36 + "1234", "0xbeef", "not-an-address"):
            with self.subTest(addr=addr), self.assertRaises(ValueError):
                helper.updated_bytes(b"l2: {}\n", addr)

    def test_atomic_owned_write_preserves_mode_and_noop_inode(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "contracts.yaml"
            path.write_bytes(b"genesis: 0x" + b"f" * 20000 + b"\nl2:\n  keep: 1\n")
            path.chmod(0o640)
            self.assertTrue(helper.persist(path, ADDRESS))
            self.assertEqual(stat.S_IMODE(path.stat().st_mode), 0o640)
            self.assertEqual(path.stat().st_uid, os.geteuid())
            before = helper.identity(path.stat())
            self.assertFalse(helper.persist(path, ADDRESS))
            self.assertEqual(helper.identity(path.stat()), before)
            self.assertEqual(list(Path(directory).iterdir()), [path])

    def test_refusal_preserves_target_bytes(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "contracts.yaml"
            raw = b"l2: []\n"
            path.write_bytes(raw); path.chmod(0o600)
            before = helper.identity(path.stat())
            with self.assertRaises(ValueError):
                helper.persist(path, ADDRESS)
            self.assertEqual(path.read_bytes(), raw)
            self.assertEqual(helper.identity(path.stat()), before)

    def test_symlink_hardlink_and_writable_mode_refused(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "contracts.yaml"
            path.write_bytes(b"l2: {}\n"); path.chmod(0o600)
            link = Path(directory) / "link.yaml"; link.symlink_to(path)
            with self.assertRaises(OSError):
                helper.persist(link, ADDRESS)
            hard = Path(directory) / "hard.yaml"; os.link(path, hard)
            with self.assertRaises(ValueError):
                helper.persist(path, ADDRESS)
            hard.unlink(); path.chmod(0o660)
            with self.assertRaises(ValueError):
                helper.persist(path, ADDRESS)

    def test_bootstrap_calls_field_helper_not_full_dump(self):
        source = (ROOT / "scripts/gateway-launch/zksys-l2-bootstrap.sh").read_text()
        terminal = source[source.index('if [ -f "${zksys_contracts_yaml}" ]; then'):]
        self.assertIn('"${SCRIPT_DIR}/_contracts_yaml_scalar.py"', terminal)
        self.assertNotIn("yaml.safe_dump", terminal)


if __name__ == "__main__":
    unittest.main()
