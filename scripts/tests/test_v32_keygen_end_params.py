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
COMMIT = "03454c7a41053a4b88bb421e97fb9efe893a92f5"
SOURCE_HASH = "95a59eca0f59f7de4d2e256a6edde5eb4248a563e2ff506d9a8fa3dd136ba2c7"
MANIFEST_HASH = "c58938395721dec889f6fe49f991f1dd1ae7cd8b457dd1434f4512d572e30335"
LOCK_HASH = "06d8dd4fd5efc9ad7db969dbee9949f6bd7af4fc2920d784f33047fac6acc293"


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
            self.assertEqual(dependency["git"], "https://github.com/matter-labs/zksync-airbender")
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
                self.assertEqual(source, f"git+https://github.com/matter-labs/zksync-airbender?rev={COMMIT}#{COMMIT}")
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
        self.assertIn('--manifest-path "${WORK_DIR}/wrapper/Cargo.toml"', block)
        self.assertIn("compute-aux-params", block)
        self.assertIn('[[ "${commitment}" == "${SECURITY100_COMMITMENT}" ]]', block)
        self.assertIn("${SECURITY100_WORDS}", block)
        self.assertIn("SERVER_FRI_VERIFIER", block)
        self.assertIn('if [[ "${GATEWAY_TARGET_IDENTITY_STATUS}" != "attested" ]]; then', workflow)
        self.assertIn('if [[ "${APP_IDENTITY_STATUS}" != "attested" ]]; then', workflow)


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
                print(os.environ.get('FAKE_COMMIT', '03454c7a41053a4b88bb421e97fb9efe893a92f5'))
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
        for name, value in (("FAKE_COMMIT", "0" * 40), ("FAKE_STATUS", " M source.rs\n"),
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
