"""Offline tests of hashing, ABI layout, and explicit identity-input rejection."""
import copy
import hashlib
from pathlib import Path
import tempfile
import unittest

import check_identity as check

TASK = Path(__file__).resolve().parent


class HashingTests(unittest.TestCase):
    def test_keccak_empty(self):
        self.assertEqual(check.as_hex(check.keccak256(b"")), "0xc5d2460186f7233c927e7db2dcc703c0e500b653ca82273b7bfad8045d85a470")

    def test_keccak_abc(self):
        self.assertEqual(check.as_hex(check.keccak256(b"abc")), "0x4e03657aea45a94fc7d47ba826c8d667c0d1e6e33a64a036ec44f58fa12d6c45")

    def test_eip1014_zero_example(self):
        self.assertEqual(check.create2("0x" + "00" * 20, "0x" + "00" * 32, b"\0"), "0x4d1a2e2bb4f88f0250f26ffff098b0b30b26bf38")

    def test_uint_rejects_bool_negative_and_overflow(self):
        for value in (True, -1, 1 << 256):
            with self.subTest(value=value), self.assertRaises(ValueError):
                check.word(value)

    def test_hex_requires_even_explicit_prefix(self):
        for value in ("aabb", "0xa", "0xgg", 12):
            with self.subTest(value=value), self.assertRaises(ValueError):
                check.hex_bytes(value)

    def test_alias_wraps_modulo_160(self):
        self.assertEqual(check.alias("0x" + "ff" * 20), "0x1111000000000000000000000000000000001110")

    def test_proxy_constructor_layout(self):
        implementation, admin, owner = ["0x" + f"{i:040x}" for i in (1, 2, 3)]
        encoded = check.proxy_args(implementation, admin, owner)
        self.assertEqual(len(encoded), 224)
        self.assertEqual(encoded[:32], check.word(1))
        self.assertEqual(encoded[32:64], check.word(2))
        self.assertEqual(encoded[64:96], check.word(96))
        self.assertEqual(encoded[96:128], check.word(68))
        self.assertEqual(encoded[128:132], check.keccak256(b"initialize(address,uint32)")[:4])
        self.assertEqual(encoded[132:164], check.word(3))
        self.assertEqual(encoded[164:196], check.word(0))
        self.assertEqual(encoded[196:], b"\0" * 28)


class InputTests(unittest.TestCase):
    def setUp(self):
        _, self.input = check.load_json(TASK / "critical-input.json")

    def test_selected_input_is_valid(self):
        self.assertEqual(check.validate_input(self.input), self.input["config"])

    def test_missing_full_configuration_is_rejected(self):
        self.input["scope"] = "full-ctm"
        with self.assertRaises(KeyError):
            check.validate_input(self.input)

    def test_bad_identity_inputs_fail(self):
        for name, mutate in (
            ("claimed deployment", lambda d: d.update(deployed=True)),
            ("numeric false", lambda d: d.update(deployed=0)),
            ("unknown scope", lambda d: d.update(scope="production")),
            ("empty namespace", lambda d: d.update(namespace="")),
            ("wrong factory", lambda d: d.update(factory="0x" + "01" * 20)),
            ("wrong alias", lambda d: d["config"].update(aliasedGovernanceAddress="0x" + "01" * 20)),
            ("wrong inner salt", lambda d: d["config"].update(salt="0x" + "01" * 32)),
            ("mock verifier", lambda d: d["config"].update(testnetVerifier=True)),
            ("Era mode", lambda d: d["config"].update(isZKsyncOS=False)),
            ("unset source", lambda d: d["source_bindings"].update(era_base_commit="0" * 40)),
            ("zero admin", lambda d: d["root_governance_constructor"].update(admin="0x" + "00" * 20)),
        ):
            with self.subTest(name=name):
                changed = copy.deepcopy(self.input)
                mutate(changed)
                with self.assertRaises(ValueError):
                    check.validate_input(changed)

    def test_source_path_cannot_escape_checkout(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            era = root / "era-contracts"
            (era / "l1-contracts").mkdir(parents=True)
            (root / "outside.sol").write_text("pragma solidity 0.8.28;\n")
            with self.assertRaises(ValueError):
                check.resolve_source(era, "../../outside.sol")


class PublishedEvidenceTests(unittest.TestCase):
    def test_asset_inventory_hashes_every_public_reproduction_file(self):
        _, manifest = check.load_json(TASK / "asset-hashes.json")
        expected = {
            "README.md", "REPRODUCTION.md", "DeriveGuestBoundIdentity.s.sol", "check_identity.py",
            "prepare_input.py", "run_offline.sh", "foundry.toml", "test_check_identity.py",
            "planned-v32-namespace.json", "planned-root-config.json", "critical-input.json", "helper-critical.json",
            "offline-critical-identity.json",
        }
        expected.update("superseded-legacy-owner/" + name for name in (
            "README.md", "asset-hashes.json", "planned-v32-namespace.json", "critical-input.json",
            "helper-critical.json", "offline-critical-identity.json",
        ))
        self.assertEqual(set(manifest["sha256"]), expected)
        for name, digest in manifest["sha256"].items():
            with self.subTest(name=name):
                self.assertEqual(hashlib.sha256((TASK / name).read_bytes()).hexdigest(), digest)

    def test_fresh_root_plan_binds_namespace_without_claiming_derived_outputs(self):
        raw, namespace = check.load_json(TASK / "planned-v32-namespace.json")
        _, plan = check.load_json(TASK / "planned-root-config.json")
        self.assertEqual(plan["namespace_plan_sha256"], hashlib.sha256(raw).hexdigest())
        self.assertEqual(plan["create2_salt"], namespace["create2_salt"])
        self.assertIs(plan["deployed"], False)
        self.assertIs(plan["production_attested"], False)
        self.assertEqual(plan["owner_address"], namespace["root_governance_constructor"]["admin"])
        self.assertEqual(plan["deployer_address"], plan["owner_address"])
        self.assertEqual(plan["era_chain_id"], 270)
        self.assertEqual(plan["max_number_of_chains"], 100)
        self.assertEqual(plan["legacy_era_diamond_proxy"], "0x" + "00" * 20)
        self.assertEqual(plan["legacy_gateway_chain_id"], 0)
        token = plan["zk_sys_token_plan"]
        self.assertEqual(token["administrator"], plan["owner_address"])
        self.assertEqual(token["origin_chain_id"], namespace["edge_chain_id"])
        self.assertNotIn("token_address", token)
        self.assertNotIn("asset_id", token)

    def test_record_binds_exact_inputs_checker_helper_and_plan_without_attesting_deployment(self):
        raw, inputs = check.load_json(TASK / "critical-input.json")
        helper_raw, helper = check.load_json(TASK / "helper-critical.json")
        plan_raw, plan = check.load_json(TASK / "planned-v32-namespace.json")
        _, record = check.load_json(TASK / "offline-critical-identity.json")
        self.assertEqual(record["input_sha256"], hashlib.sha256(raw).hexdigest())
        self.assertEqual(record["helper_output_sha256"], hashlib.sha256(helper_raw).hexdigest())
        self.assertEqual(record["checker_sha256"], hashlib.sha256((TASK / "check_identity.py").read_bytes()).hexdigest())
        self.assertEqual(helper["input_keccak256"], check.as_hex(check.keccak256(raw)))
        self.assertEqual(record["inputs"], inputs)
        self.assertEqual(record["source_bindings"], inputs["source_bindings"])
        self.assertEqual(inputs["plan_sha256"], hashlib.sha256(plan_raw).hexdigest())
        self.assertEqual(inputs["outer_salt"], plan["create2_salt"])
        self.assertEqual(record["scope"], "critical-subtree")
        self.assertEqual(record["status"], "offline-derived-not-deployed")
        for key in ("deployed", "production_attested", "independent_host_reproduction",
                    "full_ctm_calculate_addresses_executed"):
            self.assertIs(record[key], False)
        self.assertIs(plan["signer_control_verified"], True)
        self.assertEqual(plan["root_governance_constructor"]["admin"],
                         "0x622a54ea3a123127ca5fe8b98de90e957471093a")
        self.assertEqual(plan["root_governance_constructor"]["security_council"],
                         plan["root_governance_constructor"]["admin"])
        self.assertEqual(plan["signer_readiness_evidence_sha256"],
                         "3e84797e3ce0ef63ce2e6780c8846d69320a8aa26486bffb7ce68d6a28c273e8")
        for key in ("l1_governance", "proxy_admin_deployer", "proxy_admin", "validator_timelock_deployer",
                    "validator_timelock_implementation", "validator_timelock", "relay"):
            self.assertEqual(helper[key].lower(), record["derivations"][key]["address"])

    def test_superseded_legacy_owner_evidence_remains_byte_identical(self):
        directory = TASK / "superseded-legacy-owner"
        _, old_manifest = check.load_json(directory / "asset-hashes.json")
        for name in ("planned-v32-namespace.json", "critical-input.json", "helper-critical.json",
                     "offline-critical-identity.json"):
            self.assertEqual(hashlib.sha256((directory / name).read_bytes()).hexdigest(),
                             old_manifest["sha256"][name])
        _, old_plan = check.load_json(directory / "planned-v32-namespace.json")
        self.assertIs(old_plan["signer_control_verified"], False)
        self.assertIs(old_plan["deployed"], False)
        self.assertEqual(old_plan["root_governance_constructor"]["admin"],
                         "0x5b612a8914c3a891d235d0744a7d77685c6998da")


if __name__ == "__main__":
    unittest.main()
