#!/usr/bin/env python3
"""SYSCOIN: Attest native witness capture without changing the published guest tree."""

import hashlib
import re
import subprocess
import tempfile
import unittest
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[2]
HELPER_PATH = REPO_ROOT / "scripts/apply-zksync-os-native-memory-v0.4.0-patch.sh"
PATCH_PATH = REPO_ROOT / "scripts/patches/zksync-os-native-memory-v0.4.0.patch"
NATIVE_PATHS = ["forward_system/src/run/mod.rs", "oracle_provider/src/lib.rs"]
CANONICAL_TREE = "6935489bdbc7b1ed31e608677d1b2418b10691b5"


def run(*args: str, cwd: Path) -> subprocess.CompletedProcess[str]:
    return subprocess.run(args, cwd=cwd, check=False, capture_output=True, text=True)


class NativeMemoryOverlayPinsTests(unittest.TestCase):
    def test_closed_overlay_and_build_hashes_are_consistent(self) -> None:
        helper = HELPER_PATH.read_text()
        overlay = PATCH_PATH.read_bytes()
        constants = dict(re.findall(r'^([A-Z_0-9]+)="([^"\n]+)"$', helper, re.M))
        self.assertEqual(int(constants["EXPECTED_PATCH_SIZE"]), len(overlay))
        self.assertEqual(constants["EXPECTED_PATCH_SHA256"], hashlib.sha256(overlay).hexdigest())
        paths = re.findall(r'^diff --git a/(\S+) b/\S+$', overlay.decode(), re.M)
        self.assertEqual(paths, NATIVE_PATHS)
        self.assertEqual(
            constants["EXPECTED_PATCH_PATHS_SHA256"],
            hashlib.sha256(("\n".join(paths) + "\n").encode()).hexdigest(),
        )
        self.assertEqual(constants["EXPECTED_CANONICAL_TREE"], CANONICAL_TREE)
        workspace = (REPO_ROOT / "scripts/_patched-zksync-os-workspace.sh").read_text()
        self.assertIn(f'SYSCOIN_EXPECTED_ZKSYNC_OS_NATIVE_MEMORY_TREE="{constants["EXPECTED_NATIVE_TREE"]}"', workspace)
        self.assertIn(f'SYSCOIN_EXPECTED_ZKSYNC_OS_PATCHED_TREE="{CANONICAL_TREE}"', workspace)
        guard = (REPO_ROOT / "lib/multivm/build.rs").read_text()
        self.assertIn(constants["EXPECTED_FORWARD_SHA256"], guard)
        self.assertIn(constants["EXPECTED_ORACLE_SHA256"], guard)
        # These guest/circuit checks must not be loosened to accept the new host tree.
        for digest in [
            "7db04e9a5cbc0edc4e61dcdb851e88ba1cb16ce974eb76f4bf8e5c108f7ebe56",
            "7ba8d21c59b244c090be3cda6e01581d652a79c930ff0a488172e1212b74f188",
            "cbf166eea82af6c2fc5d0570095630987498dc75bce935cb7b3e05077a8f1863",
            "8fff7414159aff9ea8fe8513e57b6cc6f31aa5ae15943066aae224f1dcff3d26",
            "39be17a6fb165137e175271758514de959c0812e1579275a6f5d4d3a386a421c",
            "929738ac17af40fa260313ed0a8ce09e396ebede3b10f32a3dd7701928078b84",
        ]:
            self.assertIn(digest, guard)
        workflow = (REPO_ROOT / ".github/workflows/syscoin-v32-v8-keygen.yml").read_text()
        self.assertIn(f"APP_IDENTITY_SOURCE_TREE: {CANONICAL_TREE}", workflow)
        self.assertNotIn(constants["EXPECTED_NATIVE_TREE"], workflow)

    def test_runner_retains_independent_canonical_checkout(self) -> None:
        runner = (REPO_ROOT / "scripts/gateway-launch/run-os-server-with-patched-zksync-os.sh").read_text()
        canonical = runner.index('ZKSYNC_OS_CANONICAL_GUEST_PATH="${ZKSYNC_OS_PATCHED_PATH}"')
        native = runner.index('prepare_zksync_os_native_memory_checkout "${ZKSYNC_OS_CANONICAL_GUEST_PATH}"')
        rewrite = runner.index("  prepare_run_workspace", native)
        self.assertLess(canonical, native)
        self.assertLess(native, rewrite)
        workspace = (REPO_ROOT / "scripts/_patched-zksync-os-workspace.sh").read_text()
        native_function = workspace.split("prepare_zksync_os_native_memory_checkout() {", 1)[1].split("prepare_run_workspace() {", 1)[0]
        self.assertIn('/native-memory"', native_function)
        self.assertIn('git clone --no-hardlinks "${canonical_path}" "${native_path}"', native_function)
        self.assertNotIn('git -C "${canonical_path}" add', native_function)
        self.assertNotIn('git -C "${canonical_path}" apply', native_function)


class NativeMemoryOverlayBehaviorTests(unittest.TestCase):
    """Exercise the real helper with a tiny fixture and its exact fixture pins."""

    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.canonical = self.root / "canonical"
        self.canonical.mkdir()
        self.git("init", cwd=self.canonical)
        self.git("config", "user.name", "native-test", cwd=self.canonical)
        self.git("config", "user.email", "native-test@local", cwd=self.canonical)
        for index, path in enumerate(NATIVE_PATHS):
            target = self.canonical / path
            target.parent.mkdir(parents=True)
            target.write_text(f"// original native source {index}\n")
        self.guest_path = "proof_running_system/src/guest.rs"
        guest = self.canonical / self.guest_path
        guest.parent.mkdir(parents=True)
        guest.write_text("// official guest\n")
        self.git("add", "--all", cwd=self.canonical)
        self.git("commit", "-m", "official fixture", cwd=self.canonical)
        base = self.git("rev-parse", "HEAD", cwd=self.canonical)
        guest.write_text("// canonical pinned guest\n")
        self.git("add", "--all", cwd=self.canonical)
        self.git("commit", "-m", "canonical guest fixture", cwd=self.canonical)
        self.canonical_tree = self.git("rev-parse", "HEAD^{tree}", cwd=self.canonical)
        self.native = self.root / "native"
        self.git("clone", "--no-hardlinks", str(self.canonical), str(self.native), cwd=self.root)
        self.git("config", "user.name", "native-test", cwd=self.native)
        self.git("config", "user.email", "native-test@local", cwd=self.native)
        for index, path in enumerate(NATIVE_PATHS):
            (self.native / path).write_text(f"// original native source {index}\n// bounded capture {index}\n")
        overlay = self.git("diff", "--unified=0", cwd=self.native) + "\n"
        self.git("add", "--all", cwd=self.native)
        native_tree = self.git("write-tree", cwd=self.native)
        native_hashes = [hashlib.sha256((self.native / path).read_bytes()).hexdigest() for path in NATIVE_PATHS]
        # Reset only our fixture contents; no user checkout or cached source is touched.
        for path in NATIVE_PATHS:
            (self.native / path).write_bytes((self.canonical / path).read_bytes())
        self.git("add", "--all", cwd=self.native)
        scripts = self.root / "scripts"
        (scripts / "patches").mkdir(parents=True)
        self.patch = scripts / "patches" / PATCH_PATH.name
        self.patch.write_text(overlay)
        self.helper = scripts / HELPER_PATH.name
        values = {
            "EXPECTED_LOCKED_BASE": base,
            "EXPECTED_CANONICAL_TREE": self.canonical_tree,
            "EXPECTED_NATIVE_TREE": native_tree,
            "EXPECTED_PATCH_SIZE": str(len(self.patch.read_bytes())),
            "EXPECTED_PATCH_SHA256": hashlib.sha256(self.patch.read_bytes()).hexdigest(),
            "EXPECTED_FORWARD_SHA256": native_hashes[0],
            "EXPECTED_ORACLE_SHA256": native_hashes[1],
        }
        helper = HELPER_PATH.read_text()
        for name, value in values.items():
            helper, count = re.subn(rf'(?m)^{name}="[^"]+"$', f'{name}="{value}"', helper)
            self.assertEqual(count, 1)
        self.helper.write_text(helper)

    def tearDown(self) -> None:
        self.temp.cleanup()

    def git(self, *args: str, cwd: Path) -> str:
        result = run("git", *args, cwd=cwd)
        self.assertEqual(result.returncode, 0, result.stderr)
        return result.stdout.strip()

    def apply(self) -> subprocess.CompletedProcess[str]:
        return run("bash", str(self.helper), str(self.native), cwd=self.root)

    def test_applies_idempotently_and_canonical_guest_is_unchanged(self) -> None:
        for _ in range(2):
            result = self.apply()
            self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.git("status", "--porcelain", cwd=self.canonical), "")
        self.assertEqual(self.git("rev-parse", "HEAD^{tree}", cwd=self.canonical), self.canonical_tree)
        self.assertEqual((self.native / self.guest_path).read_bytes(), (self.canonical / self.guest_path).read_bytes())
        self.assertEqual(self.git("diff", "--name-only", cwd=self.native).splitlines(), NATIVE_PATHS)
        self.git("add", "--all", cwd=self.native)
        self.git("commit", "-m", "bounded native fixture", cwd=self.native)
        result = self.apply()
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_rejects_unrelated_guest_mutation(self) -> None:
        result = self.apply()
        self.assertEqual(result.returncode, 0, result.stderr)
        (self.native / self.guest_path).write_text("// unauthorized guest change\n")
        result = self.apply()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("partial or unrelated changes", result.stderr)

    def test_rejects_tampered_overlay_before_source_change(self) -> None:
        self.patch.write_text(self.patch.read_text() + "\n")
        result = self.apply()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("patch size mismatch", result.stderr)
        self.assertEqual(self.git("status", "--porcelain", cwd=self.native), "")

    def test_closed_allowlist_rejects_third_path_even_with_rebound_digest(self) -> None:
        extra = "\ndiff --git a/guest-extra.rs b/guest-extra.rs\nnew file mode 100644\n--- /dev/null\n+++ b/guest-extra.rs\n@@ -0,0 +1 @@\n+// not native capture\n"
        self.patch.write_text(self.patch.read_text() + extra)
        helper = self.helper.read_text()
        for name, value in {
            "EXPECTED_PATCH_SIZE": str(len(self.patch.read_bytes())),
            "EXPECTED_PATCH_SHA256": hashlib.sha256(self.patch.read_bytes()).hexdigest(),
        }.items():
            helper = re.sub(rf'(?m)^{name}="[^"]+"$', f'{name}="{value}"', helper)
        self.helper.write_text(helper)
        result = self.apply()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("closed two-file allowlist", result.stderr)
        self.assertEqual(self.git("status", "--porcelain", cwd=self.native), "")


if __name__ == "__main__":
    unittest.main()
