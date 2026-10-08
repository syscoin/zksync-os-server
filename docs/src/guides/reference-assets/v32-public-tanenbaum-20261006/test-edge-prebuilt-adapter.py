#!/usr/bin/env python3
"""SYSCOIN: Dated mock-testnet reference tests; no live config/service access."""
import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location("adapter", Path(__file__).with_name("generate-edge-prebuilt-adapter.py"))
adapter = importlib.util.module_from_spec(spec)
spec.loader.exec_module(adapter)


class AdapterTest(unittest.TestCase):
    def setUp(self):
        self.source = Path("/owned/source")
        self.runtime = Path("/owned/runtime")
        self.prefix = (
            '#!/usr/bin/env bash\nset -euo pipefail\n'
            'cd "/owned/source"\n'
            'export GATEWAY_DIR="/owned/runtime"\n'
            'export GATEWAY_CHAIN_NAME="gateway"\n'
            'export EDGE_CHAIN_NAME="zksys"\n'
            'export EDGE_CHAIN_ID="57057"\n'
            'export PROTOCOL_VERSION="v32.0"\n'
            'resolve_syscoin_cookie_file() {\n  :\n}\n'
            'source "/owned/source/scripts/gateway-launch/_execute_operator_lock.sh"\n'
            'gateway_acquire_execute_operator_lock "zksys" "/owned/runtime/os-server-configs/zksys/config.yaml"\n'
        ).encode()
        self.terminal = b'exec bash "/owned/source/scripts/gateway-launch/run-os-server-with-patched-zksync-os.sh" "zksys" -- run --release -- --config "/owned/runtime/os-server-configs/zksys/config.yaml"\n'
        self.original = self.prefix + self.terminal

    def transform(self, data):
        return adapter.adapt_edge(data, self.source, self.runtime)

    def test_exact_prefix_preserved(self):
        result = self.transform(self.original)
        self.assertEqual(result[:len(self.prefix)], self.prefix)
        self.assertEqual(result, self.original.replace(b' -- run --release -- --config ', b' -- exec-prebuilt -- --config '))

    def test_deterministic(self):
        self.assertEqual(self.transform(self.original), self.transform(self.original))

    def test_wrong_config_rejected(self):
        with self.assertRaises(ValueError):
            self.transform(self.original.replace(b'/owned/runtime/os-server-configs/zksys/config.yaml', b'/other/config.yaml'))

    def test_changed_mode_rejected(self):
        with self.assertRaises(ValueError):
            self.transform(self.original.replace(b'run --release --', b'run --debug --'))

    def test_extra_terminal_or_argument_rejected(self):
        for changed in (self.original + b'echo after\n', self.original + self.terminal,
                        self.original.replace(b'-- run --release -- --config', b'-- run --release -- --help --config')):
            with self.subTest(changed=changed), self.assertRaises(ValueError):
                self.transform(changed)

    def test_missing_or_duplicate_lock_rejected(self):
        lock = b'gateway_acquire_execute_operator_lock "zksys" "/owned/runtime/os-server-configs/zksys/config.yaml"\n'
        for changed in (self.original.replace(lock, b''), lock + self.original):
            with self.assertRaises(ValueError):
                self.transform(changed)

    def test_wrong_context_rejected(self):
        for old, new in ((b'export PROTOCOL_VERSION="v32.0"', b'export PROTOCOL_VERSION="v31.0"'),
                         (b'export EDGE_CHAIN_ID="57057"', b'export EDGE_CHAIN_ID="99"'),
                         (b'cd "/owned/source"', b'cd "/other/source"')):
            with self.subTest(old=old), self.assertRaises(ValueError):
                self.transform(self.original.replace(old, new))

    def test_missing_cookie_rejected(self):
        with self.assertRaises(ValueError):
            self.transform(self.original.replace(b'resolve_syscoin_cookie_file() {\n', b'renamed() {\n'))


if __name__ == '__main__':
    unittest.main()
