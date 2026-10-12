#!/usr/bin/env python3
"""No-build regressions for the server's Cargo pin and prebuilt stamp."""

import hashlib
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import tomllib
import unittest

ROOT = Path(__file__).resolve().parents[2]
FORK = "https://github.com/syscoin/zksync-airbender"
COMMIT = "0d8c852954afcae1f160151a17fe80886f1755b0"
TREE = "a41a4a18eeb102b405bcb63ad7ca85e9b6b13213"
SOURCE = f"git+{FORK}?rev={COMMIT}#{COMMIT}"
LEGACY = (
    "git+https://github.com/matter-labs/zksync-airbender"
    "?rev=73d69b5#73d69b5346b3c2350fa104a56ec4df78840cea99"
)


class ServerAirbenderPinTests(unittest.TestCase):
    def test_server_graph_uses_one_immutable_fork_and_preserves_legacy(self):
        manifest = tomllib.loads((ROOT / "Cargo.toml").read_text())
        lock = tomllib.loads((ROOT / "Cargo.lock").read_text())
        deps = manifest["workspace"]["dependencies"]
        for name in ("execution_utils", "riscv_transpiler", "verifier_common"):
            self.assertEqual(deps[name]["git"], FORK)
            self.assertEqual(deps[name]["rev"], COMMIT)
            self.assertNotIn("tag", deps[name])
        sources = {row.get("source") for row in lock["package"]
                   if "zksync-airbender" in row.get("source", "")}
        self.assertEqual(sources, {SOURCE, LEGACY})
        text = (ROOT / "Cargo.lock").read_text()
        self.assertNotIn("?tag=v0.6.0-rc.2#", text)

    def test_launcher_uses_locked_cargo_without_patch_replay(self):
        launcher = (ROOT / "scripts/gateway-launch/run-os-server-with-patched-zksync-os.sh").read_text()
        helper = (ROOT / "scripts/_patched-zksync-os-workspace.sh").read_text()
        self.assertNotIn("prepare_server_airbender", launcher + helper)
        self.assertNotIn("check-server-airbender", launcher + helper)
        self.assertNotIn("airbender-server-security.patch", launcher + helper)
        self.assertIn('"Cargo.toml", "Cargo.lock"', launcher)
        self.assertIn('set -- build --locked --release --bin zksync-os-server', launcher)
        self.assertEqual(launcher.count('run_cargo_with_stable_workspace_graph "$@"'), 2)
        self.assertIn("workspace_cargo_graph_digest", helper)

    def test_fork_tree_is_recorded_for_keygen(self):
        workflow = (ROOT / ".github/workflows/syscoin-v32-v8-keygen.yml").read_text()
        self.assertIn(f"AIRBENDER_COMMIT: {COMMIT}", workflow)
        self.assertIn(f"AIRBENDER_TREE: {TREE}", workflow)
        self.assertIn("rev-parse 'HEAD^{tree}'", workflow)


class ShellBoundaryTests(unittest.TestCase):
    def test_disposable_workspace_graph_is_stable_through_cargo(self):
        helper = ROOT / "scripts/_patched-zksync-os-workspace.sh"
        for cargo_code, mutate_graph in ((0, False), (7, False), (0, True), (7, True)):
            with self.subTest(cargo_code=cargo_code, mutate_graph=mutate_graph):
                with tempfile.TemporaryDirectory() as temp:
                    run_path = Path(temp) / "run"
                    run_path.mkdir()
                    (run_path / "Cargo.toml").write_text("[workspace]\n")
                    (run_path / "Cargo.lock").write_text("version = 3\n")
                    command = r'''
set -euo pipefail
source "$HELPER"
cargo() {
  if [ "$MUTATE_GRAPH" = true ]; then
    printf 'changed\n' >> "$RUN_PATH/Cargo.lock"
  fi
  return "$CARGO_CODE"
}
run_cargo_with_stable_workspace_graph test --locked
'''
                    env = dict(os.environ, HELPER=str(helper), CARGO_CODE=str(cargo_code),
                               MUTATE_GRAPH=str(mutate_graph).lower(), RUN_PATH=str(run_path))
                    result = subprocess.run(["bash", "-c", command], env=env,
                                            text=True, capture_output=True)
                    self.assertEqual(result.returncode == 0,
                                     cargo_code == 0 and not mutate_graph)
                    if mutate_graph:
                        self.assertIn("Cargo graph changed", result.stderr)
                    elif cargo_code:
                        self.assertEqual(result.returncode, cargo_code)

    def test_prebuilt_stamp_tracks_server_sources(self):
        launcher = ROOT / "scripts/gateway-launch/run-os-server-with-patched-zksync-os.sh"
        with tempfile.TemporaryDirectory() as temp:
            directory = Path(temp).resolve()
            server = directory / "server"
            shutil.copytree(ROOT / "scripts", server / "scripts",
                            ignore=shutil.ignore_patterns("__pycache__"))
            gateway = server / "scripts/gateway-launch"
            gateway.mkdir(parents=True, exist_ok=True)
            (gateway / launcher.name).write_bytes(launcher.read_bytes())
            (server / ".cargo").mkdir()
            (server / ".cargo/config.toml").write_text("fake cargo config\n")
            (server / "rust-toolchain.toml").write_text("fake toolchain\n")
            (gateway / "_common.sh").write_text(r'''
gl_require() { :; }
gl_die() { printf '%s\n' "$*" >&2; exit 1; }
gl_export_syscoin_edge_da_commit_target_from_gateway_config() { :; }
git() { printf '1111111111111111111111111111111111111111\n'; }
cargo() {
  printf '%s\n' "$@" > "$CARGO_ARGS"
  mkdir -p "$CARGO_TARGET_DIR/release"
  printf '#!/bin/sh\nprintf "exec\\n" >> "$EVENTS"\n' > "$CARGO_TARGET_DIR/release/zksync-os-server"
  chmod +x "$CARGO_TARGET_DIR/release/zksync-os-server"
  if [ "${MUTATE_SOURCE_ON_BUILD:-false}" = true ]; then
    printf 'changed during build\n' >> "$ZKSYNC_OS_SERVER_PATH/lib/types/src/protocol/proving_version.rs"
  fi
}
''')
            (server / "scripts/_patched-zksync-os-workspace.sh").write_text(r'''
extract_zksync_os_tag() { printf 'v0.4.0\n'; }
extract_zksync_os_git_url() { printf 'https://github.com/matter-labs/zksync-os.git\n'; }
require_official_zksync_os_source() { :; }
extract_locked_rev() { printf '1111111111111111111111111111111111111111\n'; }
prepare_zksync_os_checkout() { printf '%s/fake-os\n' "$ZKSYNC_OS_SERVER_PATH"; }
prepare_run_workspace() {
  mkdir -p "$1"
  cp "$ZKSYNC_OS_SERVER_PATH/Cargo.toml" "$1/Cargo.toml"
  cp "$ZKSYNC_OS_SERVER_PATH/Cargo.lock" "$1/Cargo.lock"
}
clear_multivm_build_script_cache() { :; }
run_cargo_with_stable_workspace_graph() { cargo "$@"; }
''')
            sources = {
                "Cargo.toml": "old server graph\n",
                "Cargo.lock": "old locked graph\n",
                "lib/types/src/protocol/proving_version.rs": "old vk\n",
                "node/bin/src/lib.rs": "old chain checks\n",
                "scripts/releases/era-v32/generated-verifier-overlay.patch": "old overlay\n",
                "scripts/releases/era-v32/generated-verifier-manifest.json": "old release manifest\n",
                "scripts/releases/era-v32/check-release-overlay.py": "old release checker\n",
                "scripts/apply-era-contracts-syscoin-release.py": "old materializer\n",
            }
            for name, data in sources.items():
                path = server / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(data)
            events = directory / "events"
            env = {key: value for key, value in os.environ.items()
                   if not key.startswith(("SYSCOIN_", "ZKSYNC_OS_"))}
            env.update(GATEWAY_DIR=str(directory / "gateway"),
                       ZKSYNC_OS_SERVER_PATH=str(server),
                       ZKSYNC_OS_STATIC_BUILD_CONTEXT="true", PROTOCOL_VERSION="v32.0",
                       EVENTS=str(events), CARGO_ARGS=str(directory / "cargo-args"))

            def launch(*args):
                return subprocess.run(["bash", str(gateway / launcher.name), "lane", "--", *args],
                                      env=env, text=True, capture_output=True)

            binary = directory / "gateway/.gateway-launch/target/lane/release/zksync-os-server"
            binary.parent.mkdir(parents=True)
            binary.write_text('#!/bin/sh\nprintf "exec\\n" >> "$EVENTS"\n')
            binary.chmod(0o755)
            old_payload = (hashlib.sha256(binary.read_bytes()).hexdigest(), "lane", "v32.0",
                           "0xabb69e8e899c06e51414efde62d4423de4f35004",
                           "0xb49943ea232624dd4aa63e18186076c6c99a68ef")
            old_stamp = hashlib.sha256(("\0".join(old_payload) + "\0").encode()).hexdigest()
            stamp = Path(str(binary) + ".sha256")
            stamp.write_text(old_stamp + "\n")
            historical = launch("exec-prebuilt", "--", "--version")
            self.assertNotEqual(historical.returncode, 0, historical.stderr)
            self.assertFalse(events.exists(), "historical binary executed")

            built = launch("build-prebuilt")
            self.assertEqual(built.returncode, 0, built.stderr)
            self.assertEqual((directory / "cargo-args").read_text().splitlines(),
                             ["build", "--locked", "--release", "--bin", "zksync-os-server"])
            direct = launch("run", "--release", "--", "--help")
            self.assertEqual(direct.returncode, 0, direct.stderr)
            self.assertEqual((directory / "cargo-args").read_text().splitlines(),
                             ["run", "--locked", "--release", "--", "--help"])
            already_locked = launch("run", "--locked", "--release", "--", "--help")
            self.assertEqual(already_locked.returncode, 0, already_locked.stderr)
            self.assertEqual((directory / "cargo-args").read_text().splitlines(),
                             ["run", "--locked", "--release", "--", "--help"])
            current = launch("exec-prebuilt", "--", "--version")
            self.assertEqual(current.returncode, 0, current.stderr)
            self.assertEqual(events.read_text().splitlines(), ["exec"])

            for name in sources:
                path = server / name
                with self.subTest(changed_source=name):
                    path.write_text(sources[name] + "changed\n")
                    stale = launch("exec-prebuilt", "--", "--version")
                    self.assertNotEqual(stale.returncode, 0, stale.stderr)
                    self.assertEqual(events.read_text().splitlines(), ["exec"])
                    path.write_text(sources[name])

            newly_added = server / "node/bin/src/new_security_check.rs"
            newly_added.write_text("new source\n")
            stale = launch("exec-prebuilt", "--", "--version")
            self.assertNotEqual(stale.returncode, 0, stale.stderr)
            newly_added.unlink()
            current = launch("exec-prebuilt", "--", "--version")
            self.assertEqual(current.returncode, 0, current.stderr)
            self.assertEqual(events.read_text().splitlines(), ["exec", "exec"])

            documentation = server / "scripts/prover-service/node-publication.md"
            documentation.write_text(documentation.read_text() + "updated guide\n")
            current = launch("exec-prebuilt", "--", "--version")
            self.assertEqual(current.returncode, 0, current.stderr)
            self.assertEqual(events.read_text().splitlines(), ["exec", "exec", "exec"])

            env["MUTATE_SOURCE_ON_BUILD"] = "true"
            changed_during_build = launch("build-prebuilt")
            self.assertNotEqual(changed_during_build.returncode, 0, changed_during_build.stderr)
            self.assertFalse(stamp.exists(), "changed source was stamped")



if __name__ == "__main__":
    unittest.main()
