"""SYSCOIN: Whole-fleet release checks must not silently omit external nodes."""

import ast
from copy import deepcopy
from datetime import datetime, timezone
import importlib.util
import json
import os
from pathlib import Path
import re
import shlex
import stat
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("inventory", ROOT / "scripts/gateway-launch/check-server-release-inventory.py")
inventory = importlib.util.module_from_spec(spec)
spec.loader.exec_module(inventory)
NOW = datetime(2026, 1, 1, tzinfo=timezone.utc)


def fixtures():
    manifest = {"schema": inventory.SCHEMA, "source_sha256": "a" * 64, "role_classes": {x: {"status": "deployed"} for x in inventory.CLASSES}, "instances": []}
    snapshot = {"schema": inventory.SCHEMA, "observed_at": NOW.isoformat(), "host": "example-host", "instances": []}
    for i, role in enumerate(sorted(inventory.CLASSES)):
        unit = f"example-{i}.service"
        manifest["instances"].append({"host": "example-host", "unit": unit, "role_class": role, "binary_sha256": "b" * 64, "config_sha256": ["d" * 64]})
        snapshot["instances"].append({"unit": unit, "pid": i + 1, "active": True, "source_sha256": "a" * 64, "running_elf_sha256": "b" * 64, "on_disk_elf_sha256": "b" * 64, "stock_stamp_matches": True, "config_sha256": ["d" * 64]})
    return manifest, snapshot


class ReleaseInventoryTests(unittest.TestCase):
    def test_current_fleet_passes_but_is_not_semantic_acceptance(self):
        manifest, snapshot = fixtures()
        result = inventory.verify_inventory(manifest, [snapshot], now=NOW)
        self.assertEqual(result["verified_instances"], 3)
        self.assertFalse(result["semantic_acceptance"])

    def test_current_sequencer_with_old_en_is_rejected(self):
        manifest, snapshot = fixtures()
        row = next(x for x in manifest["instances"] if x["role_class"] == "public_external_node")
        next(x for x in snapshot["instances"] if x["unit"] == row["unit"])["source_sha256"] = "c" * 64
        with self.assertRaisesRegex(ValueError, "stale_instance_source"):
            inventory.verify_inventory(manifest, [snapshot], now=NOW)

    def test_missing_en_observation_is_rejected(self):
        manifest, snapshot = fixtures()
        snapshot["instances"].pop()
        with self.assertRaisesRegex(ValueError, "missing_required_instance"):
            inventory.verify_inventory(manifest, [snapshot], now=NOW)

    def test_manifest_cannot_silently_omit_en_classes(self):
        manifest, snapshot = fixtures()
        del manifest["role_classes"]["debug_external_node"]
        with self.assertRaisesRegex(ValueError, "declare_all_role_classes"):
            inventory.verify_inventory(manifest, [snapshot], now=NOW)

    def test_non_deployed_role_needs_explicit_reason(self):
        manifest, snapshot = fixtures()
        manifest["role_classes"]["debug_external_node"] = {"status": "not_deployed"}
        with self.assertRaisesRegex(ValueError, "undeployed_role_requires_reason"):
            inventory.verify_inventory(manifest, [snapshot], now=NOW)

    def test_declared_deployed_en_cannot_have_no_instance(self):
        manifest, snapshot = fixtures()
        manifest["instances"] = [x for x in manifest["instances"] if x["role_class"] != "debug_external_node"]
        with self.assertRaisesRegex(ValueError, "deployed_role_has_no_instance"):
            inventory.verify_inventory(manifest, [snapshot], now=NOW)

    def test_replaced_disk_binary_is_not_running_acceptance(self):
        manifest, snapshot = fixtures()
        snapshot["instances"][0]["running_elf_sha256"] = "c" * 64
        with self.assertRaisesRegex(ValueError, "instance_binary_mismatch"):
            inventory.verify_inventory(manifest, [snapshot], now=NOW)

    def test_invalid_stock_stamp_is_rejected(self):
        manifest, snapshot = fixtures()
        snapshot["instances"][0]["stock_stamp_matches"] = False
        with self.assertRaisesRegex(ValueError, "instance_stock_stamp_mismatch"):
            inventory.verify_inventory(manifest, [snapshot], now=NOW)

    def test_changed_config_is_rejected(self):
        manifest, snapshot = fixtures()
        snapshot["instances"][0]["config_sha256"] = ["e" * 64]
        with self.assertRaisesRegex(ValueError, "instance_config_mismatch"):
            inventory.verify_inventory(manifest, [snapshot], now=NOW)

    def test_unexpected_observed_instance_is_not_silently_ignored(self):
        manifest, snapshot = fixtures()
        extra = deepcopy(snapshot["instances"][0])
        extra["unit"] = "undeclared-en.service"
        snapshot["instances"].append(extra)
        with self.assertRaisesRegex(ValueError, "unexpected_observed_instance"):
            inventory.verify_inventory(manifest, [snapshot], now=NOW)

    def test_duplicate_observations_are_rejected(self):
        manifest, snapshot = fixtures()
        with self.assertRaisesRegex(ValueError, "duplicate_observation"):
            inventory.verify_inventory(manifest, [snapshot, deepcopy(snapshot)], now=NOW)

    def test_stale_or_future_snapshot_is_rejected(self):
        manifest, snapshot = fixtures()
        for at in ("2025-12-31T23:54:59+00:00", "2026-01-01T00:00:01+00:00"):
            snapshot["observed_at"] = at
            with self.assertRaisesRegex(ValueError, "stale_snapshot"):
                inventory.verify_inventory(manifest, [snapshot], now=NOW)

    def test_unit_options_cannot_be_injected(self):
        with self.assertRaisesRegex(ValueError, "invalid_unit"):
            inventory.unit_state("--all")

    def test_context_matches_stock_six_field_nul_encoding(self):
        import hashlib
        values = ("a" * 64, "example", "v0.0", "0x1", "0x2", "b" * 64)
        expected = hashlib.sha256(("\0".join(values) + "\0").encode()).hexdigest()
        self.assertEqual(inventory.stock_context(*values), expected)

    def test_source_inputs_are_parsed_without_executing_host_code(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            runner = root / "scripts/gateway-launch/run-os-server-with-patched-zksync-os.sh"
            runner.parent.mkdir(parents=True)
            (root / "lib").mkdir()
            (root / "node").mkdir()
            text = "prebuilt_source_digest() {\npython3 - <<'PY'\nrust_roots = ('lib', 'node')\nbuild_inputs = ('safe.rs',)\nexcluded = {'.git', 'target', '.cursor', '.gateway-launch'}\nraise RuntimeError('AUDITED_SOURCE_MUST_NOT_EXECUTE')\nPY\n}\n"
            runner.write_text(text)
            (root / "safe.rs").write_text("safe\n")
            inventory.source_fingerprint(root)
            self.assertFalse((root / "SHOULD_NOT_EXIST").exists())

    def test_source_fingerprint_matches_the_actual_stock_helper(self):
        runner = (ROOT / "scripts/gateway-launch/run-os-server-with-patched-zksync-os.sh").read_text()
        program = re.search(r"prebuilt_source_digest\(\) \{.*?<<'PY'\n(.*?)\nPY\n\}", runner, re.S).group(1)
        expected = subprocess.check_output(["python3", "-", str(ROOT)], input=program, text=True).strip()
        self.assertEqual(inventory.source_fingerprint(ROOT), expected)

    def make_capture_fixture(self, directory):
        root = Path(directory)
        source = root / "source"
        (source / "lib").mkdir(parents=True)
        (source / "node").mkdir()
        runner = source / "scripts/gateway-launch/run-os-server-with-patched-zksync-os.sh"
        runner.parent.mkdir(parents=True)
        runner.write_text("prebuilt_source_digest() {\npython3 - <<'PY'\nrust_roots = ('lib', 'node')\nbuild_inputs = ('safe.rs',)\nexcluded = {'.git', 'target', '.cursor', '.gateway-launch'}\nPY\n}\n")
        (source / "safe.rs").write_text("safe\n")
        gateway = root / "gateway"
        binary = gateway / ".gateway-launch/target/example/release/zksync-os-server"
        binary.parent.mkdir(parents=True)
        binary.write_bytes(b"synthetic server ELF")
        proc = root / "proc/41"
        proc.mkdir(parents=True)
        (proc / "exe").symlink_to(binary)
        config = root / "config.yaml"
        config.write_text("password: SHOULD_NEVER_APPEAR\n")
        environment = {"ZKSYNC_OS_SERVER_PATH": str(source), "GATEWAY_DIR": str(gateway), "PROTOCOL_VERSION": "v0.0", "SYSCOIN_EDGE_DA_COMMIT_TARGET": "0x1", "SYSCOIN_GAS_TANK_ADDRESS": "0x2", "PRIVATE_KEY": "SHOULD_NEVER_APPEAR"}
        (proc / "environ").write_bytes(b"".join(f"{k}={v}".encode() + b"\0" for k, v in environment.items()))
        (proc / "cmdline").write_bytes(str(binary).encode() + b"\0--config\0" + str(config).encode() + b"\0")
        (proc / "stat").write_text("41 (server) " + " ".join(["S"] + ["0"] * 18 + ["12345"]))
        stamp = inventory.stock_context(inventory.file_digest(binary), "example", "v0.0", "0x1", "0x2", inventory.source_fingerprint(source))
        stamp_path = Path(str(binary) + ".sha256")
        stamp_path.write_text(stamp + "\n")
        state = {"MainPID": "41", "ActiveState": "active", "SubState": "running"}
        return root, source, proc, config, stamp_path, state

    def test_capture_hashes_private_files_without_disclosing_them(self):
        with tempfile.TemporaryDirectory() as directory:
            root, _, _, config, _, state = self.make_capture_fixture(directory)
            row = inventory.capture_unit("example.service", proc=root / "proc", get_state=lambda _: dict(state))
            self.assertTrue(row["stock_stamp_matches"])
            self.assertEqual(row["config_sha256"], [inventory.file_digest(config)])
            self.assertNotIn("SHOULD_NEVER_APPEAR", json.dumps(row))

    def test_restart_during_final_source_hash_rejects_old_pid(self):
        with tempfile.TemporaryDirectory() as directory:
            root, _, _, _, _, state = self.make_capture_fixture(directory)
            original = inventory.source_fingerprint
            calls = 0

            def fingerprint(source):
                nonlocal calls
                calls += 1
                value = original(source)
                if calls == 2:
                    state["MainPID"] = "42"
                return value

            with patch.object(inventory, "source_fingerprint", side_effect=fingerprint):
                with self.assertRaisesRegex(ValueError, "process_changed_during_capture"):
                    inventory.capture_unit("example.service", proc=root / "proc", get_state=lambda _: dict(state))

    def test_stamp_change_during_final_source_hash_is_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            root, _, _, _, stamp, state = self.make_capture_fixture(directory)
            original = inventory.source_fingerprint
            calls = 0

            def fingerprint(source):
                nonlocal calls
                calls += 1
                value = original(source)
                if calls == 2:
                    stamp.write_text("e" * 64 + "\n")
                return value

            with patch.object(inventory, "source_fingerprint", side_effect=fingerprint):
                with self.assertRaisesRegex(ValueError, "stamp_changed_during_capture"):
                    inventory.capture_unit("example.service", proc=root / "proc", get_state=lambda _: dict(state))

    def test_capture_requires_explicit_exported_bindings(self):
        missing = {"ZKSYNC_OS_SERVER_PATH": "missing_source_binding", "GATEWAY_DIR": "missing_workspace_binding", "PROTOCOL_VERSION": "missing_protocol_binding", "SYSCOIN_EDGE_DA_COMMIT_TARGET": "missing_compiled_input_binding", "SYSCOIN_GAS_TANK_ADDRESS": "missing_compiled_input_binding"}
        for name, code in missing.items():
            with self.subTest(name=name), tempfile.TemporaryDirectory() as directory:
                root, _, proc, _, _, state = self.make_capture_fixture(directory)
                entries = (proc / "environ").read_bytes().split(b"\0")
                (proc / "environ").write_bytes(b"\0".join(x for x in entries if not x.startswith(name.encode() + b"=")))
                with self.assertRaisesRegex(ValueError, code):
                    inventory.capture_unit("example.service", proc=root / "proc", get_state=lambda _: dict(state))

    def test_cli_syntax_error_is_sanitized_without_source_or_private_marker(self):
        with tempfile.TemporaryDirectory() as directory:
            _, source, _, _, _, _ = self.make_capture_fixture(directory)
            runner = source / "scripts/gateway-launch/run-os-server-with-patched-zksync-os.sh"
            runner.write_text("prebuilt_source_digest() {\npython3 - <<'PY'\nrust_roots = ('PRIVATE_DIAGNOSTIC_MARKER'\nPY\n}\n")
            tool = ROOT / "scripts/gateway-launch/check-server-release-inventory.py"
            code = "import importlib.util,sys;from pathlib import Path;s=importlib.util.spec_from_file_location('inventory',sys.argv[1]);m=importlib.util.module_from_spec(s);s.loader.exec_module(m);source=Path(sys.argv[2]);m.capture_unit=lambda unit:m.source_fingerprint(source);sys.argv=['inventory','capture','--host','fixture','--unit','example.service'];m.main()"
            result = subprocess.run([sys.executable, "-c", code, str(tool), str(source)], capture_output=True, text=True)
            self.assertEqual(result.returncode, 1)
            self.assertEqual(json.loads(result.stdout)["error"], "SyntaxError")
            self.assertEqual(result.stderr, "")
            self.assertNotIn("PRIVATE_DIAGNOSTIC_MARKER", result.stdout + result.stderr)


class StockLauncherBindingTests(unittest.TestCase):
    """Exercise emitted startup contracts without a compiler, RPC or service."""

    BINDINGS = ("ZKSYNC_OS_SERVER_PATH", "GATEWAY_DIR", "PROTOCOL_VERSION",
                "SYSCOIN_EDGE_DA_COMMIT_TARGET", "SYSCOIN_GAS_TANK_ADDRESS")

    def clean_env(self, root):
        env = {key: value for key, value in os.environ.items()
               if not key.startswith(("SYSCOIN_", "ZKSYNC_OS_", "GATEWAY_", "GL_"))}
        env.pop("PROTOCOL_VERSION", None)
        env.update(HOME=str(root), OS_SERVER_NOFILE_TARGET="1",
                   OS_SERVER_NOFILE_RECOMMENDED="1", OS_SERVER_NOFILE_MIN="1")
        return env

    def probe_binary(self, path):
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text("#!/usr/bin/env bash\nexec " + shlex.quote(sys.executable) +
                        " -c " + shlex.quote("import json,os,sys; print(json.dumps({"
                        "'env':{name:os.environ.get(name) for name in " +
                        repr(self.BINDINGS) + "}, 'argv':sys.argv[1:]}))") + ' "$@"\n')
        path.chmod(0o755)

    def test_common_exports_the_validated_canonical_root_to_children(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            common = ROOT / "scripts/gateway-launch/_common.sh"
            code = 'source "$COMMON"; exec ' + shlex.quote(sys.executable) + " -c " + shlex.quote(
                "import os; print(os.environ.get('ZKSYNC_OS_SERVER_PATH', 'MISSING'))")
            result = subprocess.run(["bash", "-c", code], capture_output=True, text=True,
                                    env=dict(self.clean_env(root), COMMON=str(common)))
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(result.stdout.strip(), str(ROOT.resolve()))
            rejected = subprocess.run(["bash", "-c", code], capture_output=True, text=True,
                                      env=dict(self.clean_env(root), COMMON=str(common),
                                               ZKSYNC_OS_SERVER_PATH=str(root)))
            self.assertNotEqual(rejected.returncode, 0)
            self.assertIn("ZKSYNC_OS_SERVER_PATH must resolve", rejected.stderr)

    def test_generated_main_launchers_export_their_pinned_source_root(self):
        generator = (ROOT / "scripts/gateway-launch/generate-os-server-configs.sh").read_text()
        # Evaluate the actual emitted template, without executing config/deployment jobs.
        template = re.search(r'(?m)^    start_script = f""".*?^"""', generator, re.S).group()
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "server source"
            runner = source / "scripts/gateway-launch/run-os-server-with-patched-zksync-os.sh"
            self.probe_binary(runner)
            gateway = root / "gateway"
            env = {"GATEWAY_CHAIN_NAME": "gateway", "EDGE_CHAIN_NAME": "edge",
                   "EDGE_CHAIN_ID": "57057", "PROTOCOL_VERSION": "v32.0"}
            for chain in ("gateway", "edge"):
                with self.subTest(chain=chain), patch.dict(os.environ, env):
                    config = gateway / chain / "config.yaml"
                    context = dict(server_root=source, gateway_dir=gateway, os=os,
                                   chain_name=chain, runner_mode="exec-prebuilt --",
                                   start_config_args=f'--config "{config}"',
                                   refresh_cookie_block="", operator_lock_block="")
                    exec(compile(ast.parse(template.lstrip()), "main-start-template", "exec"), context)
                    result = subprocess.run(["bash"], input=context["start_script"],
                                            capture_output=True, text=True, env=self.clean_env(root))
                    self.assertEqual(result.returncode, 0, result.stderr)
                    observed = json.loads(result.stdout)
                    self.assertEqual(observed["env"]["ZKSYNC_OS_SERVER_PATH"], str(source))
                    self.assertEqual(observed["env"]["GATEWAY_DIR"], str(gateway))
                    self.assertEqual(observed["env"]["PROTOCOL_VERSION"], "v32.0")
                    self.assertEqual(observed["argv"], [chain, "--", "exec-prebuilt", "--",
                                                       "--config", str(config)])

    def test_generated_en_launchers_already_export_their_pinned_source_root(self):
        deployer = (ROOT / "scripts/explorer/blockscout/deploy-zksys-en-rpc.sh").read_text()
        function = re.search(r"(?m)^def write_node_script\(.*?^providers =", deployer, re.S).group()
        function = function.rsplit("\nproviders =", 1)[0]
        context = dict(Path=Path, stat=stat, q=shlex.quote)
        exec(compile(ast.parse(function), "en-start-template", "exec"), context)
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "server source"
            self.probe_binary(source / "scripts/gateway-launch/run-os-server-with-patched-zksync-os.sh")
            for role in ("zksys-public", "zksys-debug"):
                with self.subTest(role=role):
                    workspace = root / role
                    workspace.mkdir()
                    script = workspace / "start-node.sh"
                    config = workspace / "config.yaml"
                    args = ["exec-prebuilt", "--", "--config", str(config)]
                    context["write_node_script"](script, source, workspace, role, config,
                                                 "v32.0", "0x1", args, True)
                    result = subprocess.run(["bash", str(script)], capture_output=True,
                                            text=True, env=self.clean_env(root))
                    self.assertEqual(result.returncode, 0, result.stderr)
                    observed = json.loads(result.stdout)
                    self.assertEqual(observed["env"]["ZKSYNC_OS_SERVER_PATH"], str(source))
                    self.assertEqual(observed["env"]["GATEWAY_DIR"], str(workspace))
                    self.assertEqual(observed["env"]["PROTOCOL_VERSION"], "v32.0")
                    self.assertEqual(observed["argv"], [role, "--", *args])

    def test_actual_prebuilt_runner_exports_defaults_after_stamp_validation(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            source = root / "source"
            (source / "lib").mkdir(parents=True)
            (source / "node").mkdir()
            actual_runner = ROOT / "scripts/gateway-launch/run-os-server-with-patched-zksync-os.sh"
            text = actual_runner.read_text()
            program = re.search(r"prebuilt_source_digest\(\) \{.*?<<'PY'\n(.*?)\nPY\n\}", text, re.S).group(1)
            inputs = next(ast.literal_eval(node.value) for node in ast.parse(program).body
                          if isinstance(node, ast.Assign) and any(
                              isinstance(target, ast.Name) and target.id == "build_inputs"
                              for target in node.targets))
            for name in inputs:
                destination = source / name
                destination.parent.mkdir(parents=True, exist_ok=True)
                destination.write_bytes((ROOT / name).read_bytes())
            gateway = root / "gateway"
            binary = gateway / ".gateway-launch/target/example/release/zksync-os-server"
            self.probe_binary(binary)
            target = re.search(r"(?m)^PUBLISHED_EDGE_DA_COMMIT_TARGET=(.*)$", text).group(1)
            tank = re.search(r"(?m)^PUBLISHED_GAS_TANK_ADDRESS=(.*)$", text).group(1)
            stamp = inventory.stock_context(inventory.file_digest(binary), "example", "v32.0",
                                            target, tank, inventory.source_fingerprint(source))
            Path(str(binary) + ".sha256").write_text(stamp + "\n")
            result = subprocess.run(["bash", str(source / actual_runner.relative_to(ROOT)),
                                     "example", "--", "exec-prebuilt", "--", "--help"],
                                    capture_output=True, text=True,
                                    env=dict(self.clean_env(root), GATEWAY_DIR=str(gateway),
                                             ZKSYNC_OS_STATIC_BUILD_CONTEXT="true"))
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(json.loads(result.stdout)["env"], dict(zip(self.BINDINGS,
                             (str(source), str(gateway), "v32.0", target, tank))))


if __name__ == "__main__":
    unittest.main()
