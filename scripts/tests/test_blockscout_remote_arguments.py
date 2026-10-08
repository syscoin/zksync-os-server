"""SYSCOIN: Exercise OpenSSH's joined remote command, including its empty fourth arg."""
import base64
import os
from pathlib import Path
import re
import subprocess
import tempfile
import unittest
import yaml

ROOT = Path(__file__).resolve().parents[2]
SOURCE = (ROOT / "scripts/explorer/blockscout/deploy-remote.sh").read_text()
FUNCTION = re.search(r"quote_remote_argument\(\) \{.*?\n\}", SOURCE, re.S).group()
REMOTE_SCRIPT = SOURCE.split("<<'REMOTE_SCRIPT'\n", 1)[1].split("\nREMOTE_SCRIPT", 1)[0]


def command(arguments):
    code = FUNCTION + '\nprintf "bash -s --"\nfor arg in "$@"; do printf " %s" "$(quote_remote_argument "$arg")"; done\n'
    return subprocess.check_output(["bash", "-c", code, "test", *arguments], text=True)


class RemoteArgumentTests(unittest.TestCase):
    def test_empty_fourth_survives_openssh_join_and_remote_shell(self):
        args = ["/tmp/a b's directory", "zksys", "blockscout-zksys", ""]
        result = subprocess.run(["bash", "-c", command(args)], input=b'printf "%s\\0" "$@"\n', capture_output=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.split(b"\0")[:-1], [x.encode() for x in args])

    def test_original_join_loses_empty_argument(self):
        result = subprocess.run(["bash", "-c", "bash -s -- /tmp/zksys zksys blockscout-zksys "], input=b'set -u\nprintf "%s" "$4"\n', capture_output=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn(b"unbound variable", result.stderr)

    def test_source_uses_one_quoted_remote_command(self):
        self.assertIn('"${REMOTE_HOST}" "${remote_command}"', SOURCE)
        self.assertIn('quote_remote_argument "${API_SENSITIVE_ENDPOINTS_KEY_B64}"', SOURCE)
        for name in ("IMAGE", "PULL_POLICY", "SOLC_LIST_URL"):
            self.assertIn(f'quote_remote_argument "${{SMART_CONTRACT_VERIFIER_{name}:-}}"', SOURCE)

    def remote(self, key, overrides=("", "", ""), via_deployer=False):
        with tempfile.TemporaryDirectory() as directory:
            remote = Path(directory) / "a b's blockscout"
            (remote / "envs").mkdir(parents=True)
            (remote / "envs/zksys.env").write_text("COMPOSE_PROFILES=user-ops\n")
            secret = remote / "envs/zksys.secrets.env"
            old = b"POSTGRES_PASSWORD=synthetic-only\nSECRET_KEY_BASE=synthetic-only\nAPI_SENSITIVE_ENDPOINTS_KEY=keep-test-value\n"
            secret.write_bytes(old); secret.chmod(0o600)
            binaries = Path(directory) / "bin"; binaries.mkdir()
            docker = binaries / "docker"
            docker.write_text('#!/usr/bin/env bash\nprintf "%s\\t%s\\t%s\\t%s\\n" "$*" "${SMART_CONTRACT_VERIFIER_IMAGE-unset}" "${SMART_CONTRACT_VERIFIER_PULL_POLICY-unset}" "${SMART_CONTRACT_VERIFIER_SOLC_LIST_URL-unset}" >> "$TEST_DOCKER_CALLS"\n')
            docker.chmod(0o700)
            calls = Path(directory) / "docker-calls"
            env = dict(os.environ, PATH=str(binaries) + os.pathsep + os.environ["PATH"], TEST_DOCKER_CALLS=str(calls))
            for name in ("IMAGE", "PULL_POLICY", "SOLC_LIST_URL"):
                env.pop(f"SMART_CONTRACT_VERIFIER_{name}", None)
            encoded = base64.b64encode(key.encode()).decode()
            if via_deployer:
                # SYSCOIN: Run the real local forwarding path, without SSH or Docker.
                ssh = binaries / "ssh"
                ssh.write_text('#!/usr/bin/env bash\nremote_command="${!#}"\nif [[ "$remote_command" == REMOTE_DIR_B64=* ]]; then cat >/dev/null; else env -u SMART_CONTRACT_VERIFIER_IMAGE -u SMART_CONTRACT_VERIFIER_PULL_POLICY -u SMART_CONTRACT_VERIFIER_SOLC_LIST_URL bash -c "$remote_command"; fi\n')
                ssh.chmod(0o700)
                env.update(REMOTE_HOST="synthetic-only", REMOTE_DIR=str(remote), API_SENSITIVE_ENDPOINTS_KEY=key)
                env.update({f"SMART_CONTRACT_VERIFIER_{name}": value for name, value in zip(("IMAGE", "PULL_POLICY", "SOLC_LIST_URL"), overrides)})
                result = subprocess.run(["bash", str(ROOT / "scripts/explorer/blockscout/deploy-remote.sh"), "zksys"], text=True, capture_output=True, env=env)
            else:
                result = subprocess.run(["bash", "-c", command([str(remote), "zksys", "blockscout-zksys", encoded, *overrides])], input=REMOTE_SCRIPT, text=True, capture_output=True, env=env)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(len(calls.read_text().splitlines()), 2)
            self.assertEqual(secret.stat().st_mode & 0o777, 0o600)
            return old, secret.read_bytes(), calls.read_text().splitlines()

    def test_empty_key_keeps_existing_secret_file_bytes(self):
        before, after, calls = self.remote("")
        self.assertEqual(after, before)
        self.assertTrue(all(call.split("\t")[1:] == ["unset"] * 3 for call in calls))

    def test_nonempty_key_updates_only_selected_secret(self):
        before, after, _ = self.remote("new-synthetic-only")
        self.assertEqual(after, before.replace(b"API_SENSITIVE_ENDPOINTS_KEY=keep-test-value", b"API_SENSITIVE_ENDPOINTS_KEY=new-synthetic-only"))

    def test_deployer_forwards_verifier_overrides_to_both_compose_calls(self):
        overrides = ("sha256:" + "a" * 64, "never", "https://example.invalid/solc/list.json?note=a'b&x=$(false)")
        before, after, calls = self.remote("", overrides, via_deployer=True)
        self.assertEqual(after, before)
        self.assertTrue(all(call.split("\t")[1:] == list(overrides) for call in calls))

    def test_deployer_empty_verifier_overrides_preserve_defaults(self):
        before, after, calls = self.remote("", via_deployer=True)
        self.assertEqual(after, before)
        self.assertTrue(all(call.split("\t")[1:] == ["unset"] * 3 for call in calls))


class VerifierCompatibilityTests(unittest.TestCase):
    """SYSCOIN: A floating verifier upgrade must not disable public verification."""

    def setUp(self):
        compose = yaml.safe_load((ROOT / "scripts/explorer/blockscout/docker-compose.yml").read_text())
        self.service = compose["services"]["smart-contract-verifier"]

    def test_compatibility_version_and_exact_image_override(self):
        self.assertEqual(self.service["image"], "${SMART_CONTRACT_VERIFIER_IMAGE:-ghcr.io/blockscout/smart-contract-verifier:${SMART_CONTRACT_VERIFIER_TAG:-v1.10.3}}")
        self.assertNotIn("latest", self.service["image"])

    def test_cached_image_pin_can_disable_pulling(self):
        self.assertEqual(self.service["pull_policy"], "${SMART_CONTRACT_VERIFIER_PULL_POLICY:-always}")

    def test_no_new_native_execution_or_host_docker_mount(self):
        self.assertEqual(self.service["environment"]["SMART_CONTRACT_VERIFIER__SOLIDITY__ENABLED"], "true")
        self.assertNotIn("SMART_CONTRACT_VERIFIER__COMPILERS__EXECUTION__TYPE", self.service["environment"])
        self.assertFalse(self.service.get("volumes"))

    def test_official_compiler_mirror_is_configurable(self):
        self.assertEqual(self.service["environment"]["SMART_CONTRACT_VERIFIER__SOLIDITY__FETCHER__LIST__LIST_URL"], "${SMART_CONTRACT_VERIFIER_SOLC_LIST_URL:-https://raw.githubusercontent.com/ethereum/solc-bin/gh-pages/linux-amd64/list.json}")


if __name__ == "__main__":
    unittest.main()
