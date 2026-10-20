import copy
import importlib.util
from pathlib import Path
import tomllib
import unittest


ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("root_compat", ROOT / "scripts/check-root-compat.py")
compat = importlib.util.module_from_spec(spec)
spec.loader.exec_module(compat)


class RootCompatibilityTests(unittest.TestCase):
    def setUp(self):
        with (ROOT / "Cargo.lock").open("rb") as source:
            self.lock = tomllib.load(source)

    def package(self, name, version):
        return next(package for package in self.lock["package"]
                    if package["name"] == name and package["version"] == version)

    def test_actual_lock_authenticates_both_forwarders_and_patched_parents(self):
        compat.check_lock(self.lock)

    def test_downgrading_each_real_forwarding_parent_is_rejected(self):
        for name in compat.FORWARDERS:
            with self.subTest(name=name):
                lock = copy.deepcopy(self.lock)
                for package in lock["package"]:
                    if package["name"] == name and package["version"] == "0.26.11":
                        package["dependencies"] = [f"{name} 1.0.9"]
                    elif package["name"] == name and package["version"] == "1.0.10":
                        package["version"] = "1.0.9"
                with self.assertRaisesRegex(ValueError, "patched|vulnerable"):
                    compat.check_lock(lock)

    def test_adding_another_vulnerable_version_cannot_hide_behind_the_ignore(self):
        for name in compat.FORWARDERS:
            with self.subTest(name=name):
                lock = copy.deepcopy(self.lock)
                lock["package"].append({"name": name, "version": "0.26.10", "source": compat.REGISTRY})
                with self.assertRaisesRegex(ValueError, "vulnerable"):
                    compat.check_lock(lock)

    def test_forwarder_source_checksum_and_dependency_graph_are_bound(self):
        for field, value in [("source", "git+https://example.invalid/roots"),
                             ("checksum", "0" * 64),
                             ("dependencies", ["unrelated 1.0.10"]),
                             ("dependencies", ["webpki-roots 1.0.10", "unrelated 1.0.0"])]:
            with self.subTest(field=field, value=value):
                lock = copy.deepcopy(self.lock)
                package = next(package for package in lock["package"]
                               if package["name"] == "webpki-roots" and package["version"] == "0.26.11")
                package[field] = value
                with self.assertRaises(ValueError):
                    compat.check_lock(lock)

    def test_missing_or_ambiguous_parent_is_rejected(self):
        parent = self.package("webpki-roots", "1.0.10")
        for duplicate in [False, True]:
            with self.subTest(duplicate=duplicate):
                lock = copy.deepcopy(self.lock)
                if duplicate:
                    lock["package"].append(dict(parent))
                else:
                    lock["package"].remove(parent)
                with self.assertRaisesRegex(ValueError, "absent or ambiguous"):
                    compat.check_lock(lock)

    def test_prerelease_parent_is_not_assumed_patched(self):
        self.package("webpki-roots", "0.26.11")["dependencies"] = ["webpki-roots 1.0.10-alpha.1"]
        with self.assertRaisesRegex(ValueError, "patched"):
            compat.check_lock(self.lock)


if __name__ == "__main__":
    unittest.main()
