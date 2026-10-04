#!/usr/bin/env python3
"""Inert identity, graph and shell-boundary regressions; never invoke Cargo."""
import copy
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
try:
    import tomllib
except ImportError:
    import tomli as tomllib
import unittest
from unittest import mock

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("server_airbender", ROOT / "scripts/prepare-server-airbender.py")
H = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(H)


class GraphTests(unittest.TestCase):
    def setUp(self):
        self.toml = (ROOT / "Cargo.toml").read_text()
        self.lock = (ROOT / "Cargo.lock").read_text()
        self.parsed = tomllib.loads(self.lock)
        self.url = Path("/tmp/server airbender identity").as_uri()
        self.revision = "a" * 40

    def test_reviewed_manifest_is_server_not_prover_closure(self):
        pins = H.load_pins()
        self.assertEqual((pins["selected_package_count"], pins["selected_qualified_reference_count"],
                          pins["legacy_package_count"], pins["legacy_qualified_reference_count"]),
                         (44, 57, 6, 9))
        self.assertEqual(len(pins["changed_files"]), 31)

    def test_real_graph_rewrites_only_exact_sources_and_references(self):
        # The already-completed OS rewrite must survive byte-for-byte.
        toml = self.toml.replace("https://github.com/matter-labs/zksync-os.git", "file:///tmp/patched-os")
        lock = self.lock.replace("https://github.com/matter-labs/zksync-os.git", "file:///tmp/patched-os")
        new_toml, new_lock = H.rewrite(toml, lock, self.url, self.revision)
        source = f"git+{self.url}?tag={H.TAG}#{self.revision}"
        self.assertEqual(new_toml, toml.replace(f'git = "{H.UPSTREAM}"', f'git = "{self.url}"'))
        self.assertEqual(new_lock, lock.replace(H.SELECTED, source).replace(
            H.SELECTED.split("#")[0] + ")", source.split("#")[0] + ")"))
        self.assertEqual(new_lock.count(source), 44)
        self.assertEqual(new_lock.count(source.split("#")[0] + ")"), 57)
        old_legacy = [block for block in lock.split("[[package]]") if H.LEGACY in block]
        new_legacy = [block for block in new_lock.split("[[package]]") if H.LEGACY in block]
        self.assertEqual(new_legacy, old_legacy)
        self.assertEqual(len(new_legacy), 6)
        self.assertEqual(new_lock.count(H.LEGACY.split("#")[0] + ")"), 9)

    def test_partial_selected_graph_rejected(self):
        self.parsed["package"] = [p for p in self.parsed["package"]
                                  if not (p["name"] == "execution_utils" and p.get("source") == H.SELECTED)]
        with self.assertRaisesRegex(ValueError, "partial or mixed"):
            H.validate_lock(self.parsed)

    def test_mixed_graph_and_source_aliases_rejected(self):
        for replacement in (H.SELECTED.replace(H.COMMIT, "f" * 40),
                            H.SELECTED.replace("airbender?", "airbender.git?"),
                            H.SELECTED.replace("airbender?", "airbender%2f?"),
                            H.SELECTED.replace(H.UPSTREAM, "file:///tmp/other")):
            with self.subTest(replacement=replacement), self.assertRaises(ValueError):
                H.validate_lock(tomllib.loads(self.lock.replace(H.SELECTED, replacement, 1)))

    def test_legacy_source_rewrite_rejected(self):
        with self.assertRaisesRegex(ValueError, "duplicate|partial or mixed"):
            H.validate_lock(tomllib.loads(self.lock.replace(H.LEGACY, H.SELECTED)))

    def test_legacy_dependency_delta_rejected(self):
        next(p for p in self.parsed["package"] if p.get("source") == H.LEGACY)["dependencies"].append("unexpected")
        with self.assertRaisesRegex(ValueError, "legacy Airbender closure"):
            H.validate_lock(self.parsed)

    def test_selected_dependency_delta_rejected(self):
        next(p for p in self.parsed["package"] if p.get("source") == H.SELECTED)["dependencies"].append("unexpected")
        with self.assertRaisesRegex(ValueError, "selected Airbender closure"):
            H.validate_lock(self.parsed)

    def test_partial_qualified_reference_rewrite_rejected(self):
        _, new_lock = H.rewrite(self.toml, self.lock, self.url, self.revision)
        source = f"git+{self.url}?tag={H.TAG}#{self.revision}"
        new_lock = new_lock.replace(source.split("#")[0] + ")", H.SELECTED.split("#")[0] + ")", 1)
        with self.assertRaises(ValueError):
            H.validate_lock(tomllib.loads(new_lock), source)

    def test_reference_on_unrelated_package_cannot_change(self):
        text = self.lock.replace(f'riscv_transpiler 0.1.0 ({H.LEGACY.split("#")[0]})',
                                 f'field 0.1.0 ({H.LEGACY.split("#")[0]})', 1)
        with self.assertRaises(ValueError):
            H.validate_lock(tomllib.loads(text))

    def test_direct_alias_features_or_patch_override_rejected(self):
        for text in (
            self.toml.replace("execution_utils = ", "renamed_execution_utils = ", 1),
            self.toml.replace('"verifier_binaries"', '"gpu"', 1),
            self.toml.replace(f'git = "{H.UPSTREAM}"', f'git = "{H.UPSTREAM}.git"', 1),
            self.toml + f'\n[patch."{H.UPSTREAM}"]\ncs = {{ path = "/tmp/other" }}\n',
        ):
            with self.subTest(text=text[-90:]), self.assertRaises(ValueError):
                H.validate_manifest(tomllib.loads(text))

    def test_duplicate_package_rejected(self):
        self.parsed["package"].append(copy.deepcopy(self.parsed["package"][0]))
        with self.assertRaisesRegex(ValueError, "duplicate"):
            H.validate_lock(self.parsed)

    def test_manifest_duplicate_keys_and_bad_patch_digest_rejected(self):
        with tempfile.TemporaryDirectory() as temp:
            directory = Path(temp)
            pin = directory / H.MANIFEST.name
            contents = H.MANIFEST.read_text()
            pin.write_text(contents[:-2] + ', "schema_version": 1\n}\n')
            with self.assertRaisesRegex(ValueError, "duplicate"):
                H.load_pins(pin)
            pin.write_text(contents)
            (directory / "airbender-server-security.patch").write_bytes(b"tampered\n")
            with self.assertRaisesRegex(ValueError, "patch SHA"):
                H.load_pins(pin)

    def test_prover_count_or_unknown_manifest_field_rejected(self):
        with tempfile.TemporaryDirectory() as temp:
            pin = Path(temp) / H.MANIFEST.name
            for change in ({"selected_package_count": 46}, {"unreviewed": True}):
                values = json.loads(H.MANIFEST.read_text())
                values.update(change)
                pin.write_text(json.dumps(values))
                with self.assertRaises(ValueError):
                    H.load_pins(pin)


class CheckoutTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.directory = Path(self.temp.name).resolve()
        self.source = self.directory / "source"
        self.source.mkdir()
        H.git(self.source, "init", "-q")
        (self.source / "a.rs").write_text("before\n")
        (self.source / "payload.bin").write_bytes(bytes(range(256)))
        (self.source / ".gitignore").write_text("ignored/\n")
        H.git(self.source, "add", "--all")
        H.git(self.source, "-c", "user.name=test", "-c", "user.email=test@local",
              "commit", "-qm", "immutable fixture", extra_env={
                  "GIT_AUTHOR_DATE": "2000-01-01T00:00:00+00:00",
                  "GIT_COMMITTER_DATE": "2000-01-01T00:00:00+00:00",
              })
        self.commit = H.git(self.source, "rev-parse", "HEAD")
        self.tree = H.git(self.source, "rev-parse", "HEAD^{tree}")
        before = {name: H.sha(self.source / name) for name in ("a.rs", "payload.bin")}
        (self.source / "a.rs").write_text("after\n")
        (self.source / "payload.bin").write_bytes(bytes(reversed(range(256))))
        (self.source / "new.rs").write_text("new\n")
        H.git(self.source, "add", "--all")
        self.post_tree = H.git(self.source, "write-tree")
        self.patch = self.directory / "reviewed.patch"
        self.patch.write_bytes(H.git(self.source, "diff", "--cached", "--binary", binary=True))
        self.pins = {"changed_files": {
            name: {"preimage_sha256": before.get(name), "postimage_sha256": H.sha(self.source / name),
                   "postimage_size": (self.source / name).stat().st_size}
            for name in ("a.rs", "payload.bin", "new.rs")
        }}
        self.constants = mock.patch.multiple(H, COMMIT=self.commit, TREE=self.tree, PATCHED_TREE=self.post_tree)
        self.constants.start()
        self.addCleanup(self.constants.stop)

    def prepare(self):
        return H.prepare_checkout(self.directory, self.pins, self.patch, self.source)

    def test_legitimate_local_clone_is_deterministic_and_source_is_read_only(self):
        status = H.git(self.source, "status", "--porcelain")
        first, first_rev = self.prepare()
        second, second_rev = self.prepare()
        self.assertNotEqual(first, second)
        self.assertEqual(first_rev, second_rev)
        self.assertEqual(H.git(self.source, "rev-parse", "HEAD"), self.commit)
        self.assertEqual(H.git(self.source, "status", "--porcelain"), status)
        self.assertEqual(H.sha(first / "payload.bin"), H.sha(self.source / "payload.bin"))
        H.verify_checkout(first, self.pins, first_rev)

    def test_preimage_postimage_inventory_and_full_tree_fail_closed(self):
        for problem in ("preimage", "postimage", "inventory", "tree"):
            with self.subTest(problem=problem):
                pins = copy.deepcopy(self.pins)
                if problem == "preimage":
                    pins["changed_files"]["a.rs"]["preimage_sha256"] = "f" * 64
                elif problem == "postimage":
                    pins["changed_files"]["a.rs"]["postimage_sha256"] = "f" * 64
                elif problem == "inventory":
                    del pins["changed_files"]["new.rs"]
                with mock.patch.object(H, "PATCHED_TREE", "f" * 40 if problem == "tree" else self.post_tree):
                    with self.assertRaises(ValueError):
                        H.prepare_checkout(self.directory, pins, self.patch, self.source)

    def test_stale_checkout_and_ignored_cache_are_rejected(self):
        for path in ("a.rs", "ignored/cache", "untracked"):
            with self.subTest(path=path):
                repo, revision = self.prepare()
                target = repo / path
                target.parent.mkdir(exist_ok=True)
                target.write_text("unreviewed\n")
                with self.assertRaisesRegex(ValueError, "dirty"):
                    H.verify_checkout(repo, self.pins, revision)

    def test_local_tag_drift_rejected(self):
        repo, revision = self.prepare()
        H.git(repo, "tag", "-f", H.TAG, self.commit)
        with self.assertRaisesRegex(ValueError, "tag drift"):
            H.verify_checkout(repo, self.pins, revision)

    def test_hidden_index_worktree_changes_rejected(self):
        repo, revision = self.prepare()
        H.git(repo, "update-index", "--assume-unchanged", "a.rs")
        (repo / "a.rs").write_text("hidden\n")
        with self.assertRaisesRegex(ValueError, "hidden"):
            H.verify_checkout(repo, self.pins, revision)

    def test_unrelated_staged_source_cannot_be_committed(self):
        original = H.source_images
        def injected(repo, pins, post):
            original(repo, pins, post)
            if post:
                (repo / "extra.rs").write_text("injected\n")
                H.git(repo, "add", "extra.rs")
        with mock.patch.object(H, "source_images", injected):
            with self.assertRaisesRegex(ValueError, "full staged"):
                self.prepare()

    def test_unstaged_postimage_cannot_be_committed(self):
        original = H.source_images
        def injected(repo, pins, post):
            original(repo, pins, post)
            if post:
                (repo / "a.rs").write_text("unreviewed\n")
        with mock.patch.object(H, "source_images", injected):
            with self.assertRaisesRegex(ValueError, "unstaged"):
                self.prepare()

    def test_symlink_clone_alias_rejected(self):
        alias = self.directory / "alias"
        alias.symlink_to(self.source, target_is_directory=True)
        with self.assertRaisesRegex(ValueError, "noncanonical"):
            H.prepare_checkout(self.directory, self.pins, self.patch, alias)


class AttestationTests(unittest.TestCase):
    def test_record_rejects_inputs_workspace_and_route_drift(self):
        with tempfile.TemporaryDirectory() as temp:
            workspace = Path(temp).resolve()
            record = {
                "schema_version": 1, "server": str(ROOT), "workspace": str(workspace),
                "airbender": str(workspace.parent / "server-airbender-test"), "patched_commit": "a" * 40,
                "inputs": {"example": "bound"}, "workspace_inputs": {"Cargo.toml": "bound", "Cargo.lock": "bound"},
            }
            (workspace / H.RECORD).write_text(json.dumps(record))
            with mock.patch.object(H, "inputs", return_value={"example": "drift"}):
                with self.assertRaisesRegex(ValueError, "tooling input drift"):
                    H.verify(ROOT, workspace)
            pins = H.load_pins()
            with mock.patch.object(H, "load_pins", return_value=pins), \
                    mock.patch.object(H, "inputs", return_value=record["inputs"]), \
                    mock.patch.object(H, "sha", return_value="drift"):
                with self.assertRaisesRegex(ValueError, "workspace input drift"):
                    H.verify(ROOT, workspace)
            record["workspace"] = str(workspace) + "/alias"
            (workspace / H.RECORD).write_text(json.dumps(record))
            with self.assertRaisesRegex(ValueError, "route mismatch"):
                H.verify(ROOT, workspace)

    @unittest.skipUnless(os.environ.get("SERVER_AIRBENDER_TEST_SOURCE"),
                         "optional immutable local clone; never fetch in tests")
    def test_real_patch_preparation_and_drift_checks_without_cargo(self):
        with tempfile.TemporaryDirectory() as temp:
            workspace = Path(temp).resolve() / "run"
            workspace.mkdir()
            originals = {name: (ROOT / name).read_bytes() for name in ("Cargo.toml", "Cargo.lock")}
            for name, data in originals.items():
                (workspace / name).write_bytes(data)
            env = dict(os.environ, SERVER_AIRBENDER_SOURCE_DIR=str(
                Path(os.environ["SERVER_AIRBENDER_TEST_SOURCE"]).resolve()))
            command = [sys.executable, "-B", str(ROOT / "scripts/prepare-server-airbender.py"),
                       "--server", str(ROOT), "--workspace", str(workspace)]
            result = subprocess.run(command, env=env, capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            result = subprocess.run(command + ["--verify"], env=env, capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(originals, {name: (ROOT / name).read_bytes() for name in originals})
            record = H.document(workspace / H.RECORD)
            self.assertEqual(H.git(Path(record["airbender"]), "rev-parse", "HEAD^{tree}"), H.PATCHED_TREE)
            with (workspace / "Cargo.lock").open("a") as out:
                out.write("# input drift\n")
            result = subprocess.run(command + ["--verify"], env=env, capture_output=True, text=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("workspace input drift", result.stderr)


class ShellBoundaryTests(unittest.TestCase):
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
prepare_run_workspace() { mkdir -p "$1"; }
prepare_server_airbender() { :; }
clear_multivm_build_script_cache() { :; }
run_cargo_with_verified_server_airbender() { cargo "$@"; }
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
                "scripts/prepare-server-airbender.py": "old airbender preparation\n",
                "scripts/patches/airbender-server-security.patch": "old airbender\n",
                "scripts/patches/airbender-server-security.json": "old airbender pins\n",
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
                       EVENTS=str(events))

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

    def run_boundary(self, cargo_status=0, fail_verify=0):
        with tempfile.TemporaryDirectory() as temp:
            log = Path(temp) / "log"
            command = r'''
set -euo pipefail
source "$HELPER"
verify_count=0
python3() {
  verify_count=$((verify_count + 1))
  printf 'verify\n' >> "$LOG"
  [ "$verify_count" != "$FAIL_VERIFY" ]
}
cargo() {
  printf 'cargo %s\n' "$*" >> "$LOG"
  return "$CARGO_STATUS"
}
run_cargo_with_verified_server_airbender test --locked --example native
'''
            env = dict(os.environ, HELPER=str(ROOT / "scripts/_patched-zksync-os-workspace.sh"),
                       LOG=str(log), FAIL_VERIFY=str(fail_verify), CARGO_STATUS=str(cargo_status),
                       ZKSYNC_OS_SERVER_PATH=str(ROOT), RUN_PATH=str(Path(temp) / "run"))
            result = subprocess.run(["bash", "-c", command], env=env, text=True, capture_output=True)
            return result, log.read_text().splitlines()

    def test_success_and_cargo_failure_both_reverify(self):
        for code in (0, 7):
            result, events = self.run_boundary(code)
            self.assertEqual(result.returncode, code, result.stderr)
            self.assertEqual(events, ["verify", "cargo test --locked --example native", "verify"])

    def test_preverify_failure_never_runs_cargo(self):
        result, events = self.run_boundary(fail_verify=1)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(events, ["verify"])

    def test_postverify_failure_rejects_successful_cargo(self):
        result, events = self.run_boundary(fail_verify=2)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(events, ["verify", "cargo test --locked --example native", "verify"])

    def test_actual_launcher_composes_both_cargo_paths_and_fails_closed(self):
        # Run the real launcher and new shared functions, stubbing only OS
        # preparation and process execution so no Cargo or network can run.
        launcher = ROOT / "scripts/gateway-launch/run-os-server-with-patched-zksync-os.sh"
        for prebuilt, failure in ((False, ""), (True, ""), (True, "cargo"), (False, "prepare"),
                                  (True, "before"), (True, "after")):
            with self.subTest(prebuilt=prebuilt, failure=failure), tempfile.TemporaryDirectory() as temp:
                directory = Path(temp).resolve()
                scripts = directory / "server/scripts"
                gateway = scripts / "gateway-launch"
                shutil.copytree(ROOT / "scripts", scripts,
                                ignore=shutil.ignore_patterns("__pycache__"))
                gateway.mkdir(parents=True, exist_ok=True)
                (gateway / launcher.name).write_bytes(launcher.read_bytes())
                (scripts.parent / "Cargo.toml").write_text("fake workspace\n")
                (scripts.parent / "Cargo.lock").write_text("fake lock\n")
                (scripts.parent / "rust-toolchain.toml").write_text("fake toolchain\n")
                (scripts.parent / ".cargo").mkdir()
                (scripts.parent / ".cargo/config.toml").write_text("fake cargo config\n")
                (scripts.parent / "lib").mkdir()
                (scripts.parent / "node").mkdir()
                (gateway / "_common.sh").write_text(r'''
gl_require() { :; }
gl_die() { printf '%s\n' "$*" >&2; exit 1; }
gl_export_syscoin_edge_da_commit_target_from_gateway_config() { :; }
git() { printf '1111111111111111111111111111111111111111\n'; }
verify_count=0
python3() {
  if [ "${1:-}" = - ]; then command python3 "$@"; return $?; fi
  if [ "$#" = 6 ] && [ "$6" = "--verify" ]; then
    verify_count=$((verify_count + 1))
    printf 'verify\n' >> "$EVENTS"
    if [ "$FAILURE" = before ] && [ "$verify_count" = 1 ]; then return 1; fi
    if [ "$FAILURE" = after ] && [ "$verify_count" = 2 ]; then return 1; fi
  else
    printf 'air\n' >> "$EVENTS"
    [ "$FAILURE" != prepare ] || return 1
  fi
}
cargo() {
  printf 'cargo\n' >> "$EVENTS"
  if [ "$1" = build ]; then
    mkdir -p "$CARGO_TARGET_DIR/release"
    printf '#!/usr/bin/env bash\nexit 0\n' > "$CARGO_TARGET_DIR/release/zksync-os-server"
    chmod +x "$CARGO_TARGET_DIR/release/zksync-os-server"
  fi
  if [ "$FAILURE" = cargo ]; then return 7; fi
}
''')
                helper = (ROOT / "scripts/_patched-zksync-os-workspace.sh").read_text()
                (scripts / "_patched-zksync-os-workspace.sh").write_text(helper + r'''
extract_zksync_os_tag() { printf 'v0.4.0\n'; }
extract_zksync_os_git_url() { printf 'https://github.com/matter-labs/zksync-os.git\n'; }
require_official_zksync_os_source() { :; }
extract_locked_rev() { printf '1111111111111111111111111111111111111111\n'; }
prepare_zksync_os_checkout() { printf '%s/fake-os\n' "$ZKSYNC_OS_SERVER_PATH"; }
prepare_run_workspace() { mkdir -p "$1"; printf 'os\n' >> "$EVENTS"; }
clear_multivm_build_script_cache() { printf 'cache\n' >> "$EVENTS"; }
''')
                events = directory / "events"
                env = {key: value for key, value in os.environ.items()
                       if not key.startswith(("SYSCOIN_", "ZKSYNC_OS_"))}
                env.update(GATEWAY_DIR=str(directory / "gateway"),
                           ZKSYNC_OS_SERVER_PATH=str(scripts.parent),
                           ZKSYNC_OS_STATIC_BUILD_CONTEXT="true", PROTOCOL_VERSION="v32.0",
                           EVENTS=str(events), FAILURE=failure)
                args = ["build-prebuilt"] if prebuilt else ["test", "--locked"]
                result = subprocess.run(["bash", str(gateway / launcher.name), "lane", "--", *args],
                                        env=env, text=True, capture_output=True)
                actual = events.read_text().splitlines()
                expected = ["os", "air"]
                if failure != "prepare":
                    expected += ["cache", "verify"]
                if failure not in ("prepare", "before"):
                    expected += ["cargo", "verify"]
                self.assertEqual(actual, expected, result.stderr)
                self.assertEqual(result.returncode == 0, not failure, result.stderr)
                stamp = directory / "gateway/.gateway-launch/target/lane/release/zksync-os-server.sha256"
                self.assertEqual(stamp.exists(), prebuilt and not failure)


    def test_single_launcher_has_no_optional_airbender_gate(self):
        text = (ROOT / "scripts/gateway-launch/run-os-server-with-patched-zksync-os.sh").read_text()
        start = text.index('  prepare_run_workspace \\\n')
        air = text.index('  prepare_server_airbender "${RUN_PATH}"', start)
        cache = text.index('  clear_multivm_build_script_cache', start)
        self.assertLess(start, air)
        self.assertLess(air, cache)
        self.assertEqual(text.count('run_cargo_with_verified_server_airbender "$@"'), 2)
        self.assertNotIn("SKIP_AIRBENDER", text)


if __name__ == "__main__":
    unittest.main()
