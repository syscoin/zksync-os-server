import importlib.util
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch


spec = importlib.util.spec_from_file_location("trust_store", Path(__file__).resolve().parents[1] / "check-trust-store.py")
trust = importlib.util.module_from_spec(spec)
spec.loader.exec_module(trust)
FIXTURES = Path(__file__).with_name("test-data") / "trust"


class TrustStoreTests(unittest.TestCase):
    def test_compromised_root_is_rejected_but_unaffected_emsign_is_retained(self):
        with self.assertRaisesRegex(ValueError, "distrusted emSign"):
            trust.check_fingerprints("old bundle", trust.fingerprints((FIXTURES / "emsign-g3.pem").read_text()))
        trust.check_fingerprints("unaffected emSign", trust.fingerprints((FIXTURES / "emsign-g1.pem").read_text()))

    def test_empty_bundle_is_rejected(self):
        with self.assertRaisesRegex(ValueError, "no certificates"):
            trust.fingerprints("")
        with self.assertRaisesRegex(ValueError, "empty trust store"):
            trust.check_fingerprints("default SSL", set())

    def test_system_refresh_disables_only_fingerprint_match_and_is_idempotent(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            ca = root / "mozilla"
            ca.mkdir()
            (ca / "arbitrary-name.crt").write_bytes((FIXTURES / "emsign-g3.pem").read_bytes())
            (ca / "emsign-g1.crt").write_bytes((FIXTURES / "emsign-g1.pem").read_bytes())
            config = root / "ca-certificates.conf"
            config.write_text("# retained comment\nmozilla/arbitrary-name.crt\nmozilla/emsign-g1.crt\n!disabled.crt\n")
            with patch.object(trust.subprocess, "run") as update:
                trust.refresh_debian_store(config, root)
                updated = config.read_text()
                self.assertEqual(updated, "# retained comment\n!mozilla/arbitrary-name.crt\nmozilla/emsign-g1.crt\n!disabled.crt\n")
                trust.refresh_debian_store(config, root)
                self.assertEqual(config.read_text(), updated)
                self.assertEqual(update.call_count, 2)
                update.assert_called_with(["update-ca-certificates", "--fresh"], check=True)

    def test_lazy_custom_capath_is_checked_even_when_cafile_is_clean(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            good = root / "good.pem"
            good.write_bytes((FIXTURES / "emsign-g1.pem").read_bytes())
            first, second = root / "first", root / "second"
            first.mkdir()
            second.mkdir()
            (first / "non-certificate").write_bytes(b"not a PEM certificate")
            (second / "12345678.0").write_bytes((FIXTURES / "emsign-g3.pem").read_bytes())
            paths = SimpleNamespace(cafile=str(good), openssl_capath_env="SSL_CERT_DIR", openssl_capath=None)
            context = SimpleNamespace(get_ca_certs=lambda **_: [])
            with patch.object(trust.ssl, "get_default_verify_paths", return_value=paths), \
                    patch.object(trust.ssl, "create_default_context", return_value=context), \
                    patch.dict(trust.os.environ, {"SSL_CERT_DIR": str(first) + trust.os.pathsep + str(second)}):
                with self.assertRaisesRegex(ValueError, "distrusted emSign"):
                    trust.check_fingerprints("custom capath", trust.system_fingerprints())


if __name__ == "__main__":
    unittest.main()
