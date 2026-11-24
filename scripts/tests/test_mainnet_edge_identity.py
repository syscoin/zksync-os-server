"""Exercise canonical mainnet identities without RPC, builds or deployments."""
import json
from pathlib import Path
import subprocess
import tempfile
import unittest

from scripts.tests import test_local_patched_zksync_os as support


ROOT = Path(__file__).resolve().parents[2]
LAUNCH = ROOT / "scripts/gateway-launch"


class MainnetEdgeIdentityTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.env = support.canonical_gateway_fingerprint_env(
            self.root, HOME=str(self.root), L1_NETWORK="mainnet", L1_CHAIN_ID="57",
            GATEWAY_CHAIN_ID="57570", EDGE_CHAIN_ID="5757", PROVER_MODE="gpu",
            GATEWAY_PROVER_MODE="gpu", EDGE_PROVER_MODE="gpu",
            SYSCOIN_ZKSYNC_OS_MOCK_VERIFIER="false", PYTHONDONTWRITEBYTECODE="1",
            BITCOIN_DA_RPC_USER="fixture-user", BITCOIN_DA_RPC_PASSWORD="fixture-password",
        )

    def run_common(self, command, **overrides):
        env = {**self.env, **overrides}
        env = {key: value for key, value in env.items() if value is not None}
        return subprocess.run(["bash", "-ec", 'source "$COMMON"\n' + command],
                              env=env, capture_output=True, text=True)

    def write_index(self, name, chain_id):
        path = Path(self.env["GATEWAY_DIR"]) / "chains" / name / "ZkStack.yaml"
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps({"chain_id": chain_id}) + "\n")
        return path

    def initialize(self):
        result = self.run_common("gl_bind_edge_launch_context")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.write_index("gateway", 57570)
        self.write_index("zksys", 5757)
        return support.gateway_checkpoint_state_file(self.env)

    def test_actual_launcher_prefix_checks_the_approved_mainnet_pair(self):
        source = (LAUNCH / "run-gateway-launch.sh").read_text()
        prefix = source[source.index('case "${L1_PROFILE}" in'):
                        source.index("\ngl_assert_mainnet_app_gateway_identity")]
        probe = 'gl_export_foundry_evm_version() { :; }\n' + prefix + '\nprintf "accepted\\n"'
        for overrides in ({}, {"GATEWAY_CHAIN_ID": "057570", "EDGE_CHAIN_ID": "05757"},
                          {"L1_PROFILE": "tanenbaum", "GATEWAY_CHAIN_ID": None,
                           "EDGE_CHAIN_ID": "57057"}):
            with self.subTest(overrides=overrides):
                result = self.run_common(probe, **{"L1_PROFILE": "mainnet", **overrides})
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertIn("accepted", result.stdout)
        for overrides in ({"EDGE_CHAIN_NAME": "custom", "EDGE_CHAIN_ID": "70002"},
                          {"GATEWAY_CHAIN_ID": None}, {"EDGE_CHAIN_ID": None},
                          {"EDGE_CHAIN_ID": "57570"}, {"EDGE_CHAIN_ID": "057057"},
                          {"EDGE_CHAIN_ID": "70002"}, {"GATEWAY_CHAIN_ID": "70001"},
                          {"GATEWAY_CHAIN_ID": "57001"}, {"EDGE_CHAIN_ID": "4294967296"},
                          {"L1_PROFILE": "tanenbaum", "EDGE_CHAIN_ID": "70002"}):
            with self.subTest(overrides=overrides):
                result = self.run_common(probe, **{"L1_PROFILE": "mainnet", **overrides})
                self.assertNotEqual(result.returncode, 0)
                self.assertNotIn("accepted", result.stdout)
        self.assertFalse(Path(self.env["GATEWAY_DIR"]).exists())
        self.assertFalse((self.root / ".gateway-launch-state").exists())

    def test_mainnet_launcher_stops_before_creating_a_workspace(self):
        source = (LAUNCH / "run-gateway-launch.sh").read_text()
        prefix = source[source.index('case "${L1_PROFILE}" in'):
                        source.index("gl_resolve_gateway_dir planned")]
        probe = 'gl_export_foundry_evm_version() { :; }\n' + prefix + '\necho reached'
        result = self.run_common(probe, L1_PROFILE="mainnet")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("this release is bound to 57001", result.stderr)
        self.assertNotIn("reached", result.stdout)
        self.assertFalse(Path(self.env["GATEWAY_DIR"]).exists())

    def test_standalone_broadcast_preflight_rejects_before_rpc(self):
        probe = 'gl_assert_l1_chain_id_matches_rpc() { echo reached; }\ngl_l1_broadcast_preflight'
        result = self.run_common(probe)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("Gateway chain ID 57570 needs a matching guest and V8 key", result.stderr)
        self.assertNotIn("reached", result.stdout)
        result = self.run_common(probe, L1_NETWORK="tanenbaum", L1_CHAIN_ID="5700")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("reached", result.stdout)

    def test_registered_gateway_matches_the_selected_app(self):
        result = subprocess.run(
            ["python3", "-B", str(ROOT / "scripts/apply-era-contracts-syscoin-release.py"),
             "--check-gateway-chain-id", "57001"],
            capture_output=True, text=True,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(result.stdout), {"gateway_chain_id": 57001})

    def test_mainnet_canonical_identity_keeps_exact_fingerprint(self):
        state = self.initialize()
        original = state.read_bytes()
        result = self.run_common("gl_assert_edge_launch_context")
        self.assertEqual(result.returncode, 0, result.stderr)
        for overrides in ({"EDGE_CHAIN_ID": "70003"}, {"GATEWAY_CHAIN_ID": "70004"}):
            with self.subTest(overrides=overrides):
                result = self.run_common("gl_bind_edge_launch_context", **overrides)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("mainnet deployment requires", result.stderr)
                self.assertEqual(state.read_bytes(), original)

    def test_additional_mainnet_edge_uses_persisted_canonical_id(self):
        state = self.initialize()
        original = state.read_bytes()
        extra = {"EDGE_CHAIN_NAME": "custom", "EDGE_CHAIN_ID": "70003"}
        result = self.run_common("gl_bind_edge_launch_context", **extra)
        self.assertEqual(result.returncode, 0, result.stderr)
        result = self.run_common("gl_assert_edge_launch_context", **extra)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("not present in the zkstack chain index", result.stderr)
        self.write_index("custom", 70003)
        result = self.run_common("gl_assert_edge_launch_context", **extra)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(state.read_bytes(), original)
        for chain_id in ("5757", "57570", "57057"):
            result = self.run_common("gl_bind_edge_launch_context",
                                     EDGE_CHAIN_NAME="another", EDGE_CHAIN_ID=chain_id)
            self.assertNotEqual(result.returncode, 0)
        self.write_index("zksys", 70004)
        result = self.run_common("gl_assert_edge_launch_context", **extra)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("must be 5757", result.stderr)
        self.assertEqual(state.read_bytes(), original)

    def test_mainnet_canonical_migration_keeps_checkpoint_owner_guard(self):
        source = (LAUNCH / "edge-chain-migrate-to-gateway.sh").read_text()
        start = source.index("assert_canonical_migration_mutation_authorized() {")
        function = source[start:source.index("\n}\n", start) + 3]
        probe = function + '\ngl_checkpoint_get_status() { printf "%s\\n" "$STATUS"; }\n'
        probe += "assert_canonical_migration_mutation_authorized"
        for status, inherited, expected in (("passed", "9", 1), ("in_progress", "", 1),
                                             ("in_progress", "9", 0)):
            with self.subTest(status=status, inherited=inherited):
                result = self.run_common(probe, STATUS=status,
                                         GATEWAY_EXECUTE_OPERATOR_LOCK_FD="9",
                                         GATEWAY_EXECUTE_OPERATOR_LOCK_INHERIT_FD=inherited)
                self.assertEqual(result.returncode, expected, result.stderr)

    def test_custom_name_never_becomes_canonical(self):
        for network, root_id, edge_id in (("mainnet", "57", "5757"),
                                          ("tanenbaum", "5700", "57057"),
                                          ("localhost", "31337", "57057")):
            result = self.run_common("! gl_is_canonical_edge_context", EDGE_CHAIN_NAME="custom",
                                     L1_NETWORK=network, L1_CHAIN_ID=root_id, EDGE_CHAIN_ID=edge_id)
            self.assertEqual(result.returncode, 0, result.stderr)
        for overrides in ({"L1_CHAIN_ID": "5700"}, {"EDGE_CHAIN_ID": None},
                          {"GATEWAY_CHAIN_ID": "57001"}):
            # Conditional callers must fail closed, not treat malformed zksys
            # input as an additional edge and skip canonical checkpoint checks.
            result = self.run_common("if gl_is_canonical_edge_context; then :; fi; echo bypass",
                                     **overrides)
            self.assertNotEqual(result.returncode, 0)
            self.assertNotIn("bypass", result.stdout)


if __name__ == "__main__":
    unittest.main()
