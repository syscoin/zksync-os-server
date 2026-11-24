"""Exercise explorer SSH arguments and protected input with synthetic values."""
import ast
import base64
import json
import os
from pathlib import Path
import re
import subprocess
import tarfile
import tempfile
import unittest
import yaml

ROOT = Path(__file__).resolve().parents[2]
SOURCE = (ROOT / "scripts/explorer/blockscout/deploy-remote.sh").read_text()
EN_DEPLOYER = ROOT / "scripts/explorer/blockscout/deploy-zksys-en-rpc.sh"
FUNCTION = re.search(r"quote_remote_argument\(\) \{.*?\n\}", SOURCE, re.S).group()
REMOTE_SCRIPT = SOURCE.split("<<'REMOTE_SCRIPT'\n", 1)[1].split("\nREMOTE_SCRIPT", 1)[0]


def command(arguments):
    code = FUNCTION + '\nprintf "bash -s --"\nfor arg in "$@"; do printf " %s" "$(quote_remote_argument "$arg")"; done\n'
    return subprocess.check_output(["bash", "-c", code, "test", *arguments], text=True)


class RemoteArgumentTests(unittest.TestCase):
    def test_deployer_transfers_metadata_helper_and_adjacent_manifest(self):
        for instance in ("gateway", "zksys"):
            with self.subTest(instance=instance), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                remote = root / "remote explorer"
                binaries = root / "bin"
                binaries.mkdir()
                archive = root / "payload.tar"
                ssh = binaries / "ssh"
                # Run only the real archive extraction/copy stage locally.
                # The second (Compose/service) SSH call is consumed, never run.
                ssh.write_text('#!/usr/bin/env bash\nset -euo pipefail\nremote_command="${!#}"\nif [[ "$remote_command" == REMOTE_DIR_B64=* ]]; then\n  cat > "$TEST_ARCHIVE"\n  bash -c "$remote_command" < "$TEST_ARCHIVE"\nelse\n  cat >/dev/null\nfi\n')
                ssh.chmod(0o700)
                env = dict(os.environ, PATH=str(binaries) + os.pathsep + os.environ["PATH"],
                           REMOTE_HOST="synthetic-only", REMOTE_DIR=str(remote), SSH_KEY_PATH="",
                           API_SENSITIVE_ENDPOINTS_KEY="", TEST_ARCHIVE=str(archive))
                result = subprocess.run(["bash", str(ROOT / "scripts/explorer/blockscout/deploy-remote.sh"), instance],
                                        cwd=root, text=True, capture_output=True, env=env)
                self.assertEqual(result.returncode, 0, result.stderr)
                with tarfile.open(archive) as payload:
                    names = payload.getnames()
                    for name in ("publish-token-metadata.py", "token-metadata.zksys.json"):
                        self.assertIn(name, names)
                        expected = (ROOT / "scripts/explorer/blockscout" / name).read_bytes()
                        self.assertEqual(payload.extractfile(name).read(), expected)
                        self.assertEqual((remote / name).read_bytes(), expected)
                    self.assertIn(f"envs/{instance}.env", names)
                    self.assertFalse(any("secrets.env" in name for name in names))

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
        for name in ("IMAGE", "PULL_POLICY"):
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
            encoded_url = base64.b64encode(overrides[2].encode()).decode()
            if via_deployer:
                # SYSCOIN: Run the real local forwarding path, without SSH or Docker.
                ssh = binaries / "ssh"
                ssh.write_text('''#!/usr/bin/env bash
set -euo pipefail
remote_command="${!#}"
if [[ "$remote_command" == REMOTE_DIR_B64=* ]]; then
  cat >/dev/null
else
  printf '%s' "$remote_command" > "$TEST_SSH_COMMAND"
  cat > "$TEST_SSH_INPUT"
  env -u SMART_CONTRACT_VERIFIER_IMAGE -u SMART_CONTRACT_VERIFIER_PULL_POLICY -u SMART_CONTRACT_VERIFIER_SOLC_LIST_URL bash -c "$remote_command" < "$TEST_SSH_INPUT"
fi
''')
                ssh.chmod(0o700)
                ssh_command = Path(directory) / "ssh-command"
                ssh_input = Path(directory) / "ssh-input"
                env.update(REMOTE_HOST="synthetic-only", REMOTE_DIR=str(remote), API_SENSITIVE_ENDPOINTS_KEY=key,
                           TEST_SSH_COMMAND=str(ssh_command), TEST_SSH_INPUT=str(ssh_input))
                env.update({f"SMART_CONTRACT_VERIFIER_{name}": value for name, value in zip(("IMAGE", "PULL_POLICY", "SOLC_LIST_URL"), overrides)})
                result = subprocess.run(["bash", str(ROOT / "scripts/explorer/blockscout/deploy-remote.sh"), "zksys"], text=True, capture_output=True, env=env)
                for value in (key, encoded, overrides[2], encoded_url):
                    if value:
                        self.assertNotIn(value, ssh_command.read_text())
                self.assertTrue(ssh_input.read_text().startswith(
                    f"api_key_b64={encoded}\nsolc_list_url_b64={encoded_url}\n"
                ))
            else:
                result = subprocess.run(
                    ["bash", "-c", command([str(remote), "zksys", "blockscout-zksys", *overrides[:2]])],
                    input=f"api_key_b64={encoded}\nsolc_list_url_b64={encoded_url}\n{REMOTE_SCRIPT}",
                    text=True, capture_output=True, env=env,
                )
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(len(calls.read_text().splitlines()), 2)
            self.assertEqual(secret.stat().st_mode & 0o777, 0o600)
            return old, secret.read_bytes(), calls.read_text().splitlines()

    def test_empty_key_keeps_existing_secret_file_bytes(self):
        before, after, calls = self.remote("")
        self.assertEqual(after, before)
        self.assertTrue(all(call.split("\t")[1:] == ["unset"] * 3 for call in calls))

    def test_nonempty_key_updates_only_selected_secret(self):
        before, after, _ = self.remote("new-synthetic-only", via_deployer=True)
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


class ExternalNodeDeploymentTests(unittest.TestCase):
    def test_upload_uses_committed_source_and_sends_provider_values_via_input(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "source"
            deployer = source / "scripts/explorer/blockscout/deploy-zksys-en-rpc.sh"
            deployer.parent.mkdir(parents=True)
            deployer.write_bytes(EN_DEPLOYER.read_bytes())
            ignored = "scripts/explorer/blockscout/envs/*.secrets.env"
            (source / ".gitignore").write_text(ignored + "\n")
            (source / "Cargo.toml").write_text("reviewed build input\n")
            subprocess.run(["git", "init", "-q", str(source)], check=True)
            subprocess.run(["git", "-C", str(source), "add", "."], check=True)
            subprocess.run(
                ["git", "-C", str(source), "-c", "user.name=Test", "-c", "user.email=test@example.invalid",
                 "commit", "-qm", "Test tracked source"],
                check=True,
            )
            (source / "Cargo.toml").write_text("uncommitted build input\n")
            (source / "local-notes").write_text("local state\n")
            (deployer.parent / "envs").mkdir()
            (deployer.parent / "envs/zksys.secrets.env").write_text("PRIVATE=local-only\n")

            binaries = root / "bin"
            binaries.mkdir()
            ssh = binaries / "ssh"
            ssh.write_text('''#!/usr/bin/env bash
set -euo pipefail
remote_command="${!#}"
case "$remote_command" in
  *'tar -C '*) cat > "$TEST_ARCHIVE" ;;
  'bash -s -- '*) printf '%s' "$remote_command" > "$TEST_SSH_COMMAND"; cat > "$TEST_SSH_INPUT" ;;
  'rm -f -- '*) ;;
  *) exit 1 ;;
esac
''')
            ssh.chmod(0o700)
            archive = root / "payload.tar"
            ssh_command = root / "ssh-command"
            ssh_input = root / "ssh-input"
            urls = {
                "ZKSYS_EN_L1_RPC_URL": "https://user:root-token@root.example.invalid",
                "ZKSYS_EN_GATEWAY_RPC_URL": "https://gateway.example.invalid/gateway-token",
                "MAIN_NODE_RPC_URL": "https://main.example.invalid/sequencer-token",
            }
            result = subprocess.run(
                ["bash", str(deployer)], capture_output=True, text=True,
                env={
                    **os.environ, **urls,
                    "PATH": str(binaries) + os.pathsep + os.environ["PATH"],
                    "TEST_ARCHIVE": str(archive),
                    "TEST_SSH_COMMAND": str(ssh_command),
                    "TEST_SSH_INPUT": str(ssh_input),
                    "REMOTE_HOST": "test-ssh",
                    "MAIN_NODE_ENODE": "enode://synthetic",
                    "UPLOAD_REPO": "true",
                    "SYSCOIN_EDGE_DA_COMMIT_TARGET": "0xabb69e8e899c06e51414efde62d4423de4f35004",
                    "ZKSYS_GAS_TANK_ADDRESS": "0xb49943ea232624dd4aa63e18186076c6c99a68ef",
                },
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            preamble = ssh_input.read_text().splitlines()[0]
            prefix, _, encoded = preamble.partition("=")
            self.assertEqual(prefix, "connection_b64")
            self.assertNotIn(encoded, ssh_command.read_text())
            for url in urls.values():
                self.assertNotIn(url, ssh_command.read_text())

            # Run the installer's descriptor reader without the remote build or service setup.
            script = EN_DEPLOYER.read_text()
            reader = re.search(r"^def read_connection\(\):\n(?:    .*\n)+", script, re.M).group()
            python = "import os, json, base64\n" + reader + "\nprint(json.dumps(read_connection()))"
            observed = subprocess.check_output(
                ["bash", "-c", 'IFS= read -r payload; python3 -c "$1" 3<<<"$payload"', "test", python],
                input=encoded + "\n", text=True,
            )
            providers, main_url = json.loads(observed)
            self.assertEqual(providers["l1_provider"]["rpc_url"], urls["ZKSYS_EN_L1_RPC_URL"])
            self.assertEqual(providers["gateway_provider"]["rpc_url"], urls["ZKSYS_EN_GATEWAY_RPC_URL"])
            self.assertEqual(main_url, urls["MAIN_NODE_RPC_URL"])
            with tarfile.open(archive) as payload:
                self.assertEqual(payload.extractfile("Cargo.toml").read(), b"reviewed build input\n")
                self.assertNotIn("local-notes", payload.getnames())
                self.assertFalse(any(".secrets.env" in name for name in payload.getnames()))

    def test_generated_gateway_keeps_allowlisted_rpc_behind_same_host_nginx(self):
        launcher = ROOT / "scripts/gateway-launch/generate-os-server-configs.sh"
        code = launcher.read_text().split("python3 - <<'PY'\n", 1)[1].split("\nPY\n", 1)[0]
        tree = ast.parse(code)
        materializers = [
            {argument.arg: argument.value for argument in call.keywords}
            for call in ast.walk(tree) if isinstance(call, ast.Call)
            and isinstance(call.func, ast.Name) and call.func.id == "materialize_chain"
        ]
        binds = {entry["pubdata_mode"].value: entry["rpc_bind_host"].value for entry in materializers}
        self.assertEqual(binds, {"Blobs": "127.0.0.1", "RelayedL2Calldata": "0.0.0.0"})
        self.assertIn('address: {rpc_bind_host}:{rpc_port}', code)
        edge = next(entry for entry in materializers if entry["pubdata_mode"].value == "RelayedL2Calldata")
        self.assertIn('http://127.0.0.1:', ast.unparse(edge["gateway_rpc_url"]))
        nginx = (ROOT / "scripts/explorer/blockscout/deploy-rpc-nginx.sh").read_text()
        self.assertIn('GATEWAY_RPC_UPSTREAM:-http://127.0.0.1:3052', nginx)


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
