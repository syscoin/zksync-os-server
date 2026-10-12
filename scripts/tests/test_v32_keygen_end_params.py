"""No-build regressions for the pinned, locked upstream end_params harness."""

import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import textwrap
import tomllib
import unittest


REPO = Path(__file__).resolve().parents[2]
KEYGEN = REPO / "scripts/keygen"
COMMIT = "553fdd31b9d7a0b354845c7b6b8e7c3d13337c52"
TREE = "6693fdafa31c45ebf8acf26e9a1f3dfc3edcffdc"
SOURCE_HASH = "95a59eca0f59f7de4d2e256a6edde5eb4248a563e2ff506d9a8fa3dd136ba2c7"
MANIFEST_HASH = "0e8cfad6794132cc2f1fc15b00b4c8ea2b62932c096bc82bb6df4deb42ef736c"
LOCK_HASH = "c66345caf250a899a3106094ff5cd6c4aaf2edf6c737e300a22ef8d2efe795de"


class EndParamsAssetTests(unittest.TestCase):
    def test_reviewed_inputs_are_byte_identical(self):
        for name, digest in (("Cargo.toml", MANIFEST_HASH), ("Cargo.lock", LOCK_HASH),
                             ("src/end_params.rs", SOURCE_HASH)):
            with self.subTest(name=name):
                self.assertEqual(hashlib.sha256((KEYGEN / "end-params" / name).read_bytes()).hexdigest(), digest)

    def test_cpu_graph_and_release_profile_are_explicit(self):
        manifest = tomllib.loads((KEYGEN / "end-params/Cargo.toml").read_text())
        self.assertEqual(set(manifest["dependencies"]), {"execution_utils", "riscv_transpiler", "verifier_common"})
        for dependency in manifest["dependencies"].values():
            self.assertEqual(dependency["rev"], COMMIT)
            self.assertEqual(dependency["git"], "https://github.com/syscoin/zksync-airbender")
            self.assertIs(dependency["default-features"], False)
        self.assertEqual(manifest["dependencies"]["execution_utils"]["features"], ["prover", "verifier_binaries"])
        self.assertEqual(manifest["profile"]["release"], {"opt-level": 3, "lto": "fat", "codegen-units": 1, "debug": False})
        packages = tomllib.loads((KEYGEN / "end-params/Cargo.lock").read_text())["package"]
        self.assertEqual(len(packages), 203)
        self.assertFalse({package["name"] for package in packages} & {
            "gpu_prover", "zksync-gpu-prover", "zksync-gpu-ffi", "shivini", "proof-compression",
        })
        for package in packages:
            source = package.get("source", "")
            if "zksync-airbender" in source:
                self.assertEqual(source, f"git+https://github.com/syscoin/zksync-airbender?rev={COMMIT}#{COMMIT}")
            if source.startswith("registry+"):
                self.assertRegex(package["checksum"], r"^[0-9a-f]{64}$")

    def test_workflow_retains_security100_and_production_gates(self):
        workflow = (REPO / ".github/workflows/syscoin-v32-v8-keygen.yml").read_text()
        start = workflow.index("      - name: Recompute Security100 program identity")
        block = workflow[start:workflow.index("      - name:", start + 1)]
        self.assertIn('bash "${GITHUB_WORKSPACE}/scripts/keygen/build-end-params.sh"', block)
        self.assertIn('"${WORK_DIR}/end-params-helper/target/release/end_params"', block)
        self.assertNotIn('--manifest-path "${WORK_DIR}/airbender/Cargo.toml"', block)
        self.assertIn('grep -Fqx "app_end_params = ${APP_END_PARAMS}"', block)
        self.assertIn('--manifest-path "${WORK_DIR}/candidate-proving-sources/zkos-wrapper/Cargo.toml"', block)
        self.assertIn("compute-aux-params", block)
        self.assertIn('[[ "${commitment}" == "${SECURITY100_COMMITMENT}" ]]', block)
        self.assertIn("${SECURITY100_WORDS}", block)
        self.assertIn("SERVER_FRI_VERIFIER", block)
        self.assertIn('if [[ "${GATEWAY_TARGET_IDENTITY_STATUS}" != "attested" ]]; then', workflow)
        self.assertIn('if [[ "${APP_IDENTITY_STATUS}" != "attested" ]]; then', workflow)

    def test_candidate_wrapper_uses_fork_and_stock_control_stays_upstream(self):
        workflow = (REPO / ".github/workflows/syscoin-v32-v8-keygen.yml").read_text()
        candidate = workflow.split("      - name: Prepare exact Airbender, wrapper, and common crypto sources", 1)[1]
        candidate = candidate.split("      - name:", 1)[0]
        stock = workflow.split("  stock-control:", 1)[1].split("  attest:", 1)[0]
        self.assertIn('scripts/keygen/prepare-candidate-wrapper.py', candidate)
        self.assertIn('"${WORK_DIR}/prover-tooling" "${WORK_DIR}/airbender"', candidate)
        self.assertIn('?rev=${AIRBENDER_COMMIT}#${AIRBENDER_COMMIT}', candidate)
        self.assertIn('HEAD^{tree}', candidate)
        self.assertLess(workflow.index('scripts/keygen/prepare-candidate-wrapper.py'),
                        workflow.index('compute-aux-params'))
        self.assertIn('PROVER_TOOLING_COMMIT: 3e76c396ac5acef199b1fbd65b0846963c9a08dc', workflow)
        self.assertIn('PROVER_TOOLING_TREE: ca726e425dd2a8ab6c9ac593f154ec764a07552f', workflow)
        self.assertIn('?tag=${AIRBENDER_RELEASE}#${STOCK_AIRBENDER_COMMIT}', stock)
        self.assertNotIn('scripts/keygen/prepare-candidate-wrapper.py', stock)
        self.assertIn('--manifest-path "${WORK_DIR}/wrapper/Cargo.toml"', stock)
        self.assertNotIn("V8_VK_REGENERATION_REQUIRED", workflow)
        self.assertNotIn("V8_APP_IDENTITY_REGENERATION_REQUIRED", workflow)

    def test_candidate_vk_replacement_updates_both_registered_literals(self):
        workflow = (REPO / ".github/workflows/syscoin-v32-v8-keygen.yml").read_text()
        candidate = workflow.split("      - name: Produce fail-closed server candidate", 1)[1]
        candidate = candidate.split("      - name:", 1)[0]
        self.assertEqual(candidate.count('"${proving_version}")" == "2" ]]'), 3)
        self.assertIn("'s/\\Q$ENV{OLD_HASH}\\E/$ENV{VK_HASH}/g'", candidate)
        self.assertIn('checked_out_server_vk_remains_registered: true', candidate)
        self.assertNotIn('checked_out_server_vk_remains_zero', candidate)
        original = (REPO / "lib/types/src/protocol/proving_version.rs").read_text()
        old_hash = '0x2ac3231439b0ba30b688a78eba0119fdfcf7a8364cf75037606cfb61f92c0b90'
        new_hash = '0x' + 'ab' * 32
        self.assertEqual(original.count(f'"{old_hash}"'), 2)
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source = root / "lib/types/src/protocol/proving_version.rs"
            source.parent.mkdir(parents=True)
            source.write_text(original)
            subprocess.run(["git", "init", "-q"], cwd=root, check=True)
            subprocess.run(["git", "add", "lib/types/src/protocol/proving_version.rs"],
                           cwd=root, check=True)
            subprocess.run(["git", "-c", "user.name=Test", "-c", "user.email=test@example.com",
                            "commit", "-qm", "fixture"], cwd=root, check=True)
            output = root / "work/final"
            output.mkdir(parents=True)
            (output / "syscoin-v8-vk-hash.txt").write_text(new_hash + "\n")
            script = candidate.split("        run: |\n", 1)[1]
            script = textwrap.dedent(script[:script.index("          plonk_key_sha=")])
            result = subprocess.run(["bash", "-c", script], cwd=root,
                                    capture_output=True, text=True,
                                    env={**os.environ, "WORK_DIR": str(root / "work"),
                                         "REGISTERED_V8_VK_HASH": old_hash})
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(source.read_text(), original)
            patch = output / "zksync-os-server-v8-plonk-candidate.patch"
            self.assertTrue(patch.is_file())
            additions = [line for line in patch.read_text().splitlines()
                         if line.startswith("+") and f'"{new_hash}"' in line]
            self.assertEqual(len(additions), 2)


class EndParamsBuildWrapperTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name).resolve()
        self.helper = self.root / "server/scripts/keygen"
        shutil.copytree(KEYGEN, self.helper, ignore=shutil.ignore_patterns("__pycache__"))
        self.airbender = self.root / "airbender"
        source = self.airbender / "tools/cli/src/bin/end_params.rs"
        source.parent.mkdir(parents=True)
        shutil.copyfile(self.helper / "end-params/src/end_params.rs", source)
        self.fake_bin = self.root / "fake-bin"
        self.fake_bin.mkdir()
        self.trace = self.root / "cargo-args.json"
        self.output = self.root / "result"
        self.env = {**os.environ, "PATH": f"{self.fake_bin}:{os.environ['PATH']}",
                    "FAKE_AIRBENDER": str(self.airbender), "FAKE_CARGO_TRACE": str(self.trace),
                    "PYTHONDONTWRITEBYTECODE": "1"}
        for name in ("RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS", "RUST_TOOLCHAIN"):
            self.env.pop(name, None)
        self.tool("git", """
            import os, sys
            args = sys.argv[1:]
            if args[-1] == '--show-toplevel':
                print(os.environ['FAKE_AIRBENDER'])
            elif args[-1] == 'HEAD':
                print(os.environ.get('FAKE_COMMIT', '553fdd31b9d7a0b354845c7b6b8e7c3d13337c52'))
            elif args[-1] == 'HEAD^{tree}':
                print(os.environ.get('FAKE_TREE', '6693fdafa31c45ebf8acf26e9a1f3dfc3edcffdc'))
            elif args[-1] == '--porcelain':
                print(os.environ.get('FAKE_STATUS', ''), end='')
            else:
                sys.exit('unexpected git invocation')
        """)
        self.tool("rustc", """
            import os
            print('rustc 1.95.0-nightly (18d13b533 2026-02-09)')
            print('commit-hash: ' + os.environ.get('FAKE_RUSTC_COMMIT', '18d13b5332916ffca8eadb9106d54b5b434e9978'))
            print('host: ' + os.environ.get('FAKE_RUSTC_HOST', 'x86_64-unknown-linux-gnu'))
        """)
        self.tool("cargo", """
            import json, os
            from pathlib import Path
            import sys
            args = sys.argv[1:]
            Path(os.environ['FAKE_CARGO_TRACE']).write_text(json.dumps(args))
            manifest = Path(args[args.index('--manifest-path') + 1])
            if os.environ.get('FAKE_CHANGE_LOCK'):
                with manifest.with_name('Cargo.lock').open('a') as handle:
                    handle.write('# changed lock\\n')
            if os.environ.get('FAKE_CARGO_FAIL'):
                sys.exit(47)
            target = Path(args[args.index('--target-dir') + 1]) / 'release/end_params'
            target.parent.mkdir(parents=True)
            target.write_text('#!/bin/sh\\nexit 0\\n')
            target.chmod(0o700)
        """)

    def tool(self, name, body):
        path = self.fake_bin / name
        path.write_text(f"#!{sys.executable}\n" + textwrap.dedent(body))
        path.chmod(0o700)

    def run_builder(self, output=None):
        return subprocess.run(["bash", str(self.helper / "build-end-params.sh"),
                               str(self.airbender), str(output or self.output)],
                              text=True, capture_output=True, env=self.env, timeout=10)

    def test_build_uses_locked_pinned_toolchain_and_preserves_inputs(self):
        result = self.run_builder()
        self.assertEqual(result.returncode, 0, result.stderr)
        args = json.loads(self.trace.read_text())
        self.assertEqual(args[:4], ["+nightly-2026-02-10", "build", "--locked", "--release"])
        self.assertNotIn("generate-lockfile", args)
        self.assertEqual(args[args.index("--bin") + 1], "end_params")
        self.assertEqual(hashlib.sha256((self.output / "Cargo.lock").read_bytes()).hexdigest(), LOCK_HASH)
        self.assertIn(LOCK_HASH + "  Cargo.lock", (self.output / "SHA256SUMS").read_text())
        self.assertEqual(result.stdout.strip(), str(self.output / "target/release/end_params"))

    def test_existing_output_is_never_overwritten(self):
        self.output.mkdir()
        sentinel = self.output / "sentinel"
        sentinel.write_text("preserve")
        self.assertNotEqual(self.run_builder().returncode, 0)
        self.assertEqual(sentinel.read_text(), "preserve")
        self.assertFalse(self.trace.exists())

    def test_missing_output_parent_fails_before_build(self):
        self.assertNotEqual(self.run_builder(self.root / "missing/result").returncode, 0)
        self.assertFalse(self.trace.exists())
        self.assertFalse((self.root / "missing").exists())

    def test_symlinked_output_is_never_followed(self):
        destination = self.root / "preserved"
        destination.mkdir()
        self.output.symlink_to(destination, target_is_directory=True)
        self.assertNotEqual(self.run_builder().returncode, 0)
        self.assertEqual(list(destination.iterdir()), [])
        self.assertFalse(self.trace.exists())

    def test_output_cannot_be_inside_either_checkout(self):
        for output in (self.root / "server/build-result", self.airbender / "build-result"):
            with self.subTest(output=output):
                self.assertNotEqual(self.run_builder(output).returncode, 0)
                self.assertFalse(output.exists())

    def test_source_drift_is_rejected_before_build(self):
        source = self.airbender / "tools/cli/src/bin/end_params.rs"
        source.write_text(source.read_text() + "\n// drift\n")
        self.assertNotEqual(self.run_builder().returncode, 0)
        self.assertFalse(self.trace.exists())
        self.assertFalse(self.output.exists())

    def test_wrong_revision_dirty_checkout_or_overrides_fail_before_build(self):
        for name, value in (("FAKE_COMMIT", "0" * 40), ("FAKE_TREE", "0" * 40),
                            ("FAKE_STATUS", " M source.rs\n"),
                            ("RUST_TOOLCHAIN", "stable"), ("RUSTFLAGS", "-C opt-level=0")):
            with self.subTest(name=name):
                self.env[name] = value
                self.assertNotEqual(self.run_builder().returncode, 0)
                self.assertFalse(self.output.exists())
                self.assertFalse(self.trace.exists())
                del self.env[name]

    def test_packaged_lock_drift_fails_before_build(self):
        lock = self.helper / "end-params/Cargo.lock"
        lock.write_text(lock.read_text() + "\n")
        self.assertNotEqual(self.run_builder().returncode, 0)
        self.assertFalse(self.trace.exists())

    def test_cargo_cannot_silently_replace_lock(self):
        self.env["FAKE_CHANGE_LOCK"] = "1"
        result = self.run_builder()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("Cargo changed the reviewed lock", result.stderr)
        self.assertFalse((self.output / "SHA256SUMS").exists())

    def test_build_failure_is_preserved(self):
        self.env["FAKE_CARGO_FAIL"] = "1"
        self.assertEqual(self.run_builder().returncode, 47)
        self.assertFalse((self.output / "SHA256SUMS").exists())

    def test_wrong_rustc_fails_before_cargo(self):
        self.env["FAKE_RUSTC_COMMIT"] = "0" * 40
        self.assertNotEqual(self.run_builder().returncode, 0)
        self.assertFalse(self.trace.exists())


if __name__ == "__main__":
    unittest.main()
