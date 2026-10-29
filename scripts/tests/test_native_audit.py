import copy
import hashlib
import importlib.util
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch


ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location(
    "native_audit", ROOT / "scripts/check-native-audit.py"
)
audit = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(audit)

REVIEWED_ID = "RUSTSEC-2026-0387"
UNKNOWN_ID = "RUSTSEC-2099-0001"
LEGACY_ID = "RUSTSEC-2023-0071"


def policy():
    return {
        "packages": {
            "crypto-bigint": {
                "version": "0.5.5",
                "source": audit.REGISTRY,
                "checksum": "a" * 64,
            }
        },
        "advisories": {
            REVIEWED_ID: {
                "package": "crypto-bigint",
                "sha256": "b" * 64,
                "rationale": "The reviewed native caller graph excludes the affected paths.",
            }
        },
    }


def package():
    return {"name": "crypto-bigint", **policy()["packages"]["crypto-bigint"]}


def finding(advisory_id=REVIEWED_ID):
    return {
        "advisory": {"id": advisory_id, "package": "crypto-bigint"},
        "package": package(),
    }


def report(scanner="audit", findings=None, warnings=None, ignores=None):
    findings = [] if findings is None else findings
    return {
        "database": {"advisory-count": 1},
        "lockfile": {"dependency-count": 1},
        "settings": {"ignore": [] if ignores is None else ignores},
        "vulnerabilities": (
            {"found": bool(findings), "count": len(findings), "list": findings}
            if scanner == "audit" else findings
        ),
        "warnings": {} if warnings is None else warnings,
    }


class PackageGuardTests(unittest.TestCase):
    def test_exact_registry_package_is_accepted(self):
        audit.validate_packages({"package": [package()]}, policy())

    def test_unrelated_packages_do_not_expand_the_exception(self):
        lock = {"package": [package(), {"name": "unrelated", "version": "1.0.0"}]}
        audit.validate_packages(lock, policy())

    def test_version_source_and_checksum_changes_are_rejected(self):
        for field, value in [
            ("version", "0.5.6"),
            ("source", "git+https://example.invalid/crypto-bigint#revision"),
            ("checksum", "c" * 64),
            ("checksum", None),
        ]:
            with self.subTest(field=field, value=value):
                changed = package()
                changed[field] = value
                with self.assertRaises(ValueError):
                    audit.validate_packages({"package": [changed]}, policy())

    def test_missing_package_and_duplicate_name_are_rejected(self):
        other_version = {**package(), "version": "0.6.0"}
        for packages in [[], [package(), package()], [package(), other_version]]:
            with self.subTest(packages=packages):
                with self.assertRaises(ValueError):
                    audit.validate_packages({"package": packages}, policy())


class AdvisoryGuardTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.db = Path(self.directory.name)
        self.path = self.db / "crates/crypto-bigint" / f"{REVIEWED_ID}.md"
        self.path.parent.mkdir(parents=True)
        self.path.write_text(
            '```toml\n[advisory]\nid = "RUSTSEC-2026-0387"\n'
            'package = "crypto-bigint"\ndate = "2026-10-23"\n'
            '[versions]\npatched = [">=0.7.6"]\n```\n\nReviewed description.\n'
        )
        self.policy = policy()
        self.policy["advisories"][REVIEWED_ID]["sha256"] = hashlib.sha256(
            self.path.read_bytes()
        ).hexdigest()

    def test_reviewed_advisory_bytes_are_accepted(self):
        audit.validate_advisories(self.db, self.policy)

    def test_same_id_with_updated_advisory_requires_review(self):
        self.path.write_text(self.path.read_text() + "Additional affected operation.\n")
        with self.assertRaises(ValueError):
            audit.validate_advisories(self.db, self.policy)

    def test_missing_advisory_is_not_silently_accepted(self):
        self.path.unlink()
        with self.assertRaises(ValueError):
            audit.validate_advisories(self.db, self.policy)


class HostGuardTests(unittest.TestCase):
    def test_native_linux_with_reviewed_flags_is_accepted(self):
        for environ in [
            {},
            {"RUSTFLAGS": "--cfg tokio_unstable"},
            {"CARGO_ENCODED_RUSTFLAGS": "--cfg\x1ftokio_unstable"},
        ]:
            with self.subTest(environ=environ):
                audit.validate_host(environ, "Linux", "x86_64")

    def test_other_operating_systems_and_architectures_are_rejected(self):
        for system, machine in [("Darwin", "x86_64"), ("Linux", "aarch64"),
                                ("Linux", "wasm32"), ("Windows", "AMD64")]:
            with self.subTest(system=system, machine=machine):
                with self.assertRaises(ValueError):
                    audit.validate_host({}, system, machine)

    def test_compiler_and_native_library_overrides_are_rejected(self):
        for key, value in [
            ("RUSTFLAGS", "--cfg tokio_unstable --cfg unreviewed"),
            ("CARGO_ENCODED_RUSTFLAGS", "--cfg\x1funreviewed"),
            ("CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTFLAGS", "--cfg unreviewed"),
            ("RUSTC", "/tmp/unreviewed-rustc"),
            ("CARGO_PROFILE_RELEASE_OPT_LEVEL", "0"),
            ("ZSTD_SYS_USE_PKG_CONFIG", "1"),
            ("ZSTD_LIB_DIR", "/tmp/unreviewed-zstd"),
            ("LIBZ_SYS_STATIC", "0"),
            ("CC", "/tmp/unreviewed-cc"),
            ("CC_x86_64_unknown_linux_gnu", "/tmp/unreviewed-cc"),
            ("TARGET_CFLAGS", "-DUNREVIEWED"),
            ("CC_ENABLE_DEBUG_OUTPUT", "1"),
            ("CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER", "/tmp/unreviewed-linker"),
        ]:
            with self.subTest(key=key):
                with self.assertRaises(ValueError):
                    audit.validate_host({key: value}, "Linux", "x86_64")


def metadata(root):
    own_id = f"path+file://{root}/node/bin#zksync_os_server@0.23.0"
    dep_id = f"{audit.REGISTRY}#crypto-bigint@0.5.5"
    unrelated_id = f"{audit.REGISTRY}#unrelated@1.0.0"
    return {
        "version": 1,
        "workspace_root": str(root),
        "target_directory": str(root / "target"),
        "workspace_members": [own_id],
        "workspace_default_members": [own_id],
        "packages": [
            {"id": own_id, "name": "zksync_os_server", "version": "0.23.0",
             "source": None, "manifest_path": str(root / "node/bin/Cargo.toml"),
             "targets": [{"src_path": str(root / "node/bin/src/lib.rs")}],
             "dependencies": []},
            {"id": dep_id, "name": "crypto-bigint", "version": "0.5.5",
             "source": audit.REGISTRY, "manifest_path": "/cache/crypto-bigint/Cargo.toml",
             "targets": [], "dependencies": []},
            {"id": unrelated_id, "name": "unrelated", "version": "1.0.0",
             "source": audit.REGISTRY, "manifest_path": "/cache/unrelated/Cargo.toml",
             "targets": [], "dependencies": []},
        ],
        "resolve": {"root": None, "nodes": [
            {"id": own_id, "features": [], "dependencies": [dep_id],
             "deps": [{"name": "crypto_bigint", "pkg": dep_id,
                       "dep_kinds": [{"kind": None, "target": None}]}]},
            {"id": dep_id, "features": ["generic-array", "zeroize"],
             "dependencies": [], "deps": []},
            {"id": unrelated_id, "features": [], "dependencies": [], "deps": []},
        ]},
    }


class LibzScopeTests(unittest.TestCase):
    def setUp(self):
        self.policy = {
            "packages": {
                "libz-sys": {"version": "1.1.22", "source": audit.REGISTRY,
                             "checksum": "8b70e7a7df205e92a1a4cd9aaae7898dac0aa555503cc0a649494d0d60e7651d"},
                "cc": {"version": "1.2.62", "source": audit.REGISTRY,
                       "checksum": "a1dce859f0832a7d088c4f1119888ab94ef4b5d6795d1ce05afb7fe159d79f98"},
            },
            "advisories": {
                audit.LIBZ_ADVISORY: {"package": "libz-sys", "sha256": "d" * 64,
                                      "rationale": "Only the pinned static Linux build with reviewed cc."},
            },
        }
        self.metadata = {
            "packages": [
                {"id": "libz", "name": "libz-sys", "version": "1.1.22", "source": audit.REGISTRY},
                {"id": "cc", "name": "cc", "version": "1.2.62", "source": audit.REGISTRY},
            ],
            "resolve": {"nodes": [
                {"id": "libz", "features": ["static"], "deps": [
                    {"name": "cc", "pkg": "cc", "dep_kinds": [{"kind": "build", "target": None}]},
                ]},
                {"id": "cc", "features": [], "deps": []},
            ]},
        }

    def test_pinned_static_graph_and_build_dependency_are_required(self):
        audit.validate_libz_scope(self.metadata, self.policy)
        changes = []
        no_static = copy.deepcopy(self.metadata)
        no_static["resolve"]["nodes"][0]["features"] = []
        changes.append(no_static)
        wrong_cc = copy.deepcopy(self.metadata)
        wrong_cc["resolve"]["nodes"][0]["deps"][0]["pkg"] = "other"
        changes.append(wrong_cc)
        wrong_kind = copy.deepcopy(self.metadata)
        wrong_kind["resolve"]["nodes"][0]["deps"][0]["dep_kinds"][0]["kind"] = "normal"
        changes.append(wrong_kind)
        changed_version = copy.deepcopy(self.metadata)
        changed_version["packages"][1]["version"] = "1.6.0"
        changes.append(changed_version)
        changed_source = copy.deepcopy(self.metadata)
        changed_source["packages"][1]["source"] = "git+https://example.invalid/cc"
        changes.append(changed_source)
        missing_node = copy.deepcopy(self.metadata)
        missing_node["resolve"]["nodes"].pop()
        changes.append(missing_node)
        changes.extend([{}, {"packages": None}, {"packages": [], "resolve": {"nodes": None}}])
        for changed in changes:
            with self.subTest(changed=changed), self.assertRaises(ValueError):
                audit.validate_libz_scope(changed, self.policy)

    def test_exact_libz_and_cc_lock_identities_are_required(self):
        locked = {"package": [{"name": name, **value} for name, value in self.policy["packages"].items()]}
        audit.validate_packages(locked, self.policy)
        for name in ("cc", "libz-sys"):
            changed = copy.deepcopy(locked)
            next(p for p in changed["package"] if p["name"] == name)["checksum"] = "e" * 64
            with self.subTest(name=name), self.assertRaises(ValueError):
                audit.validate_packages(changed, self.policy)

    def test_actual_server_feature_tree_must_contain_only_static_libz(self):
        audit.validate_libz_server_tree("other v1.0.0 []\nlibz-sys v1.1.22 [static]\n")
        for tree in ("", "libz-sys v1.1.22 []", "libz-sys v1.1.22 [static,zlib-ng]",
                     "libz-sys v1.1.31 [static]", "libz-sys v1.1.22 [static]\nlibz-sys v1.1.22 []"):
            with self.subTest(tree=tree), self.assertRaises(ValueError):
                audit.validate_libz_server_tree(tree)

    def test_default_and_gcp_server_feature_trees_are_both_checked(self):
        with patch.object(audit, "run", return_value="libz-sys v1.1.22 [static]\n") as call:
            audit.validate_libz_server_builds(Path("/tmp/server"), self.policy)
        self.assertEqual(call.call_count, 2)
        self.assertNotIn("--features", call.call_args_list[0].args[0])
        self.assertEqual(call.call_args_list[1].args[0][-2:], ["--features", "gcp"])
        with patch.object(audit, "run", side_effect=["libz-sys v1.1.22 [static]", "libz-sys v1.1.22 []"]):
            with self.assertRaises(ValueError):
                audit.validate_libz_server_builds(Path("/tmp/server"), self.policy)

    def test_only_exact_reported_libz_identity_is_accepted_by_both_scanners(self):
        expected = self.policy["packages"]["libz-sys"]
        for scanner in ("audit", "deny"):
            item = {"advisory": {"id": audit.LIBZ_ADVISORY, "package": "libz-sys"},
                    "package": {"name": "libz-sys", **expected}}
            if scanner == "deny":
                item["package"]["checksum"] = None
            accepted, remaining = audit.filter_report(report(scanner, [item]), self.policy, scanner, set())
            self.assertEqual((len(accepted), len(remaining)), (1, 0))
            for field, bad in (("version", "1.1.23"), ("source", "git+https://example.invalid/repo")):
                modified = copy.deepcopy(item)
                modified["package"][field] = bad
                with self.subTest(scanner=scanner, field=field), self.assertRaises(ValueError):
                    audit.filter_report(report(scanner, [modified]), self.policy, scanner, set())
            changed = copy.deepcopy(item)
            changed["package"]["checksum"] = "e" * 64
            with self.subTest(scanner=scanner, field="checksum"), self.assertRaises(ValueError):
                audit.filter_report(report(scanner, [changed]), self.policy, scanner, set())

    def test_libz_advisory_record_bytes_must_match_review(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            path = root / "crates/libz-sys" / f"{audit.LIBZ_ADVISORY}.md"
            path.parent.mkdir(parents=True)
            path.write_text("Reviewed advisory record.\n")
            self.policy["advisories"][audit.LIBZ_ADVISORY]["sha256"] = hashlib.sha256(path.read_bytes()).hexdigest()
            audit.validate_advisories(root, self.policy)
            path.write_text("Changed advisory record.\n")
            with self.assertRaises(ValueError):
                audit.validate_advisories(root, self.policy)


class GraphGuardTests(unittest.TestCase):
    def test_relocated_workspace_has_same_graph_fingerprint(self):
        first = Path("/tmp/first-server")
        second = Path("/home/runner/work/server")
        self.assertEqual(audit.graph_fingerprint(metadata(first), first),
                         audit.graph_fingerprint(metadata(second), second))

    def test_added_feature_changes_graph_fingerprint(self):
        root = Path("/tmp/server")
        before = metadata(root)
        after = copy.deepcopy(before)
        after["resolve"]["nodes"][1]["features"].append("unreviewed")
        self.assertNotEqual(audit.graph_fingerprint(before, root),
                            audit.graph_fingerprint(after, root))

    def test_new_consumer_edge_changes_graph_fingerprint(self):
        root = Path("/tmp/server")
        before = metadata(root)
        after = copy.deepcopy(before)
        node = after["resolve"]["nodes"][0]
        other_id = after["packages"][2]["id"]
        node["dependencies"].append(other_id)
        node["deps"].append({"name": "unrelated", "pkg": other_id,
                             "dep_kinds": [{"kind": None, "target": None}]})
        self.assertNotEqual(audit.graph_fingerprint(before, root),
                            audit.graph_fingerprint(after, root))

    def test_workspace_package_cannot_resolve_outside_reviewed_root(self):
        root = Path("/tmp/server")
        changed = metadata(root)
        changed["packages"][0]["manifest_path"] = "/tmp/unreviewed/Cargo.toml"
        with self.assertRaises(ValueError):
            audit.graph_fingerprint(changed, root)


class SourceGuardTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        (self.root / "node/bin/src").mkdir(parents=True)
        (self.root / "node/bin/src/lib.rs").write_text("pub fn reviewed() {}\n")
        (self.root / "Cargo.toml").write_text('[workspace]\nmembers = ["node/bin"]\n')
        (self.root / "Cargo.lock").write_text("version = 4\n")
        (self.root / ".gitignore").write_text("/target/\n")
        (self.root / "README.md").write_text("Review notes.\n")
        subprocess.run(["git", "init", "-q", str(self.root)], check=True,
                       capture_output=True)
        subprocess.run(["git", "-C", str(self.root), "add", "."], check=True,
                       capture_output=True)

    def test_new_call_in_existing_source_requires_review(self):
        before = audit.source_fingerprint(self.root)
        with (self.root / "node/bin/src/lib.rs").open("a") as source:
            source.write("pub fn new_affected_call() {}\n")
        self.assertNotEqual(before, audit.source_fingerprint(self.root))

    def test_new_untracked_source_cannot_hide_from_review(self):
        before = audit.source_fingerprint(self.root)
        (self.root / "node/bin/src/extra.rs").write_text("pub fn new_call() {}\n")
        self.assertNotEqual(before, audit.source_fingerprint(self.root))

    def test_markdown_and_ignored_build_outputs_do_not_change_source(self):
        before = audit.source_fingerprint(self.root)
        (self.root / "README.md").write_text("Updated review notes.\n")
        (self.root / "target").mkdir()
        (self.root / "target/generated.rs").write_text("build output\n")
        bytecode = self.root / "scripts/__pycache__/check-native-audit.cpython-314.pyc"
        bytecode.parent.mkdir(parents=True)
        bytecode.write_bytes(b"generated Python bytecode")
        self.assertEqual(before, audit.source_fingerprint(self.root))

    def test_policy_and_guard_files_are_excluded_without_excluding_other_scripts(self):
        before = audit.source_fingerprint(self.root)
        for name in ["scripts/security/native-linux-review.json",
                     "scripts/check-native-audit.py", "scripts/tests/test_native_audit.py"]:
            path = self.root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text("review infrastructure\n")
        self.assertEqual(before, audit.source_fingerprint(self.root))
        (self.root / "scripts/another-build-input.sh").write_text("changed build input\n")
        self.assertNotEqual(before, audit.source_fingerprint(self.root))


class ReportGuardTests(unittest.TestCase):
    def filtered(self, data, scanner="audit", legacy=None):
        return audit.filter_report(data, policy(), scanner,
                                   set() if legacy is None else legacy)

    def assert_match_rejected(self, data, scanner="audit"):
        with self.assertRaises(ValueError):
            self.filtered(data, scanner)

    def test_exact_reviewed_vulnerability_is_accepted_by_both_scanners(self):
        for scanner in ["audit", "deny"]:
            with self.subTest(scanner=scanner):
                accepted, remaining = self.filtered(report(scanner, [finding()]), scanner)
                self.assertEqual(len(accepted), 1)
                self.assertFalse(remaining)

    def test_reviewed_unsound_warning_is_accepted_by_both_scanners(self):
        for scanner in ["audit", "deny"]:
            with self.subTest(scanner=scanner):
                data = report(scanner, warnings={"unsound": [finding()]})
                accepted, remaining = self.filtered(data, scanner)
                self.assertEqual(len(accepted), 1)
                self.assertFalse(remaining)

    def test_filtering_does_not_mutate_the_raw_report(self):
        for scanner in ["audit", "deny"]:
            with self.subTest(scanner=scanner):
                data = report(scanner, [finding()], {"unsound": [finding(UNKNOWN_ID)]})
                before = copy.deepcopy(data)
                accepted, remaining = self.filtered(data, scanner)
                self.assertEqual(len(accepted), 1)
                self.assertEqual(len(remaining), 1)
                self.assertEqual(data, before)

    def test_unknown_vulnerabilities_and_unsoundness_remain_blocking(self):
        for scanner in ["audit", "deny"]:
            for warning in [False, True]:
                with self.subTest(scanner=scanner, warning=warning):
                    data = (report(scanner, warnings={"unsound": [finding(UNKNOWN_ID)]})
                            if warning else report(scanner, [finding(UNKNOWN_ID)]))
                    accepted, remaining = self.filtered(data, scanner)
                    self.assertFalse(accepted)
                    self.assertEqual(len(remaining), 1)

    def test_nonblocking_notice_categories_preserve_existing_policy(self):
        for scanner in ["audit", "deny"]:
            for category in ["unmaintained", "notice", "yanked"]:
                with self.subTest(scanner=scanner, category=category):
                    accepted, remaining = self.filtered(
                        report(scanner, warnings={category: [finding(UNKNOWN_ID)]}), scanner
                    )
                    self.assertFalse(accepted)
                    self.assertFalse(remaining)

    def test_known_id_cannot_hide_changed_package_identity(self):
        for scanner in ["audit", "deny"]:
            for field, value in [("name", "another-crate"), ("version", "0.5.6"),
                                 ("source", "git+https://example.invalid/replacement"),
                                 ("checksum", "c" * 64)]:
                with self.subTest(scanner=scanner, field=field):
                    changed = finding()
                    changed["package"][field] = value
                    self.assert_match_rejected(report(scanner, [changed]), scanner)

    def test_advisory_package_must_match_reviewed_identity(self):
        changed = finding()
        changed["advisory"]["package"] = "another-crate"
        self.assert_match_rejected(report("audit", [changed]))

    def test_deny_may_omit_checksum_but_audit_may_not(self):
        changed = finding()
        changed["package"]["checksum"] = None
        accepted, remaining = self.filtered(report("deny", [changed]), "deny")
        self.assertEqual(len(accepted), 1)
        self.assertFalse(remaining)
        self.assert_match_rejected(report("audit", [changed]))

    def test_legacy_ignore_requires_both_declared_and_reported_ignore(self):
        data = report("audit", [finding(LEGACY_ID)])
        accepted, remaining = self.filtered(data, legacy={LEGACY_ID})
        self.assertFalse(accepted)
        self.assertEqual(len(remaining), 1)
        data["settings"]["ignore"] = [LEGACY_ID]
        _, remaining = self.filtered(data, legacy={LEGACY_ID})
        self.assertFalse(remaining)

    def test_scanner_cannot_preignore_new_or_unknown_advisories(self):
        for advisory_id in [REVIEWED_ID, UNKNOWN_ID]:
            for scanner in ["audit", "deny"]:
                with self.subTest(scanner=scanner, advisory_id=advisory_id):
                    with self.assertRaises(ValueError):
                        self.filtered(report(scanner, ignores=[advisory_id]), scanner)

    def test_audit_count_and_found_flag_must_match_list(self):
        for field, value in [("count", 0), ("count", "1"), ("found", False),
                             ("list", None), ("list", {})]:
            with self.subTest(field=field, value=value):
                data = report("audit", [finding()])
                data["vulnerabilities"][field] = value
                with self.assertRaises(ValueError):
                    self.filtered(data)

    def test_missing_and_malformed_report_sections_fail_closed(self):
        for scanner in ["audit", "deny"]:
            for section in ["vulnerabilities", "warnings", "settings"]:
                with self.subTest(scanner=scanner, section=section):
                    data = report(scanner)
                    del data[section]
                    with self.assertRaises(ValueError):
                        self.filtered(data, scanner)
            malformed = report(scanner, warnings={"unsound": None})
            with self.assertRaises(ValueError):
                self.filtered(malformed, scanner)


class DenyDiagnosticTests(unittest.TestCase):
    def test_known_advisory_diagnostics_are_accepted(self):
        for code in ["vulnerability", "unsound"]:
            with self.subTest(code=code):
                audit.deny_diagnostics(
                    '{"fields":{"severity":"error","code":"' + code + '"}}\n'
                )

    def test_operational_error_log_is_rejected(self):
        with self.assertRaises(ValueError):
            audit.deny_diagnostics(
                '{"fields":{"level":"ERROR","message":"database unavailable"}}\n'
            )

    def test_non_advisory_error_diagnostic_is_rejected(self):
        with self.assertRaises(ValueError):
            audit.deny_diagnostics(
                '{"fields":{"severity":"error","code":"banned"}}\n'
            )

    def test_malformed_diagnostic_json_is_rejected(self):
        with self.assertRaises(ValueError):
            audit.deny_diagnostics("not JSON\n")


if __name__ == "__main__":
    unittest.main()
