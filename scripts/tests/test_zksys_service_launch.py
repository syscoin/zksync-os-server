import subprocess
import json
import os
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
SERVICE = ROOT / "scripts/gateway-launch/_zksys_service_v1.sh"
BOOTSTRAP = ROOT / "scripts/gateway-launch/zksys-l2-bootstrap.sh"
COMMON = ROOT / "scripts/gateway-launch/_common.sh"


class ServiceLaunchTests(unittest.TestCase):
    def test_staking_grant_follows_configuration_and_resumes(self):
        bootstrap = BOOTSTRAP.read_text()
        start = bootstrap.index("  zksys_assert_staking_deferred\n")
        end = bootstrap.index("  if [ ", start)
        stage = bootstrap[start:end]
        self.assertIn("zksys_setup_service_v1", stage)
        self.assertIn("zksys_enable_staking", stage)
        command = '''source "$1"
            export ZKSYS_L2_ISSUER_ADDRESS=issuer ZKSYS_L2_WEIGHT_REGISTRY_ADDRESS=weight
            export ZKSYS_L2_STAKING_VAULT_ADDRESS=vault STAKE_WEIGHT_UPDATER_ROLE=role
            call_l2() {
                if [ "$1" = issuer ]; then cat "$TEST_STATE_DIR/configured";
                else cat "$TEST_STATE_DIR/staking"; fi
            }
            assert_l2_bool_call() {
                local actual
                actual="$(call_l2 "$1" "$2")"
                [ "$actual" = "$3" ]
            }
            send_l2() {
                [ "$(cat "$TEST_STATE_DIR/configured")" = 1 ]
                echo true > "$TEST_STATE_DIR/staking"
                echo grant >> "$TEST_STATE_DIR/events"
            }
            zksys_setup_service_v1() {
                if [ "$(cat "$TEST_STATE_DIR/configured")" = 0 ]; then
                    [ "$(cat "$TEST_STATE_DIR/staking")" = false ]
                    echo 1 > "$TEST_STATE_DIR/configured"
                    echo configure >> "$TEST_STATE_DIR/events"
                fi
            }
        ''' + stage
        for configured, staking, expected, events in [
            (0, False, 0, ["configure", "grant"]),
            (1, False, 0, ["grant"]),
            (1, True, 0, []),
            (0, True, 1, []),
        ]:
            with self.subTest(configured=configured, staking=staking), tempfile.TemporaryDirectory() as directory:
                state = Path(directory)
                (state / "configured").write_text(f"{configured}\n")
                (state / "staking").write_text(f"{str(staking).lower()}\n")
                (state / "events").write_text("")
                env = {**os.environ, "TEST_STATE_DIR": directory}
                result = subprocess.run(["bash", "-ec", command, "test", str(SERVICE)], env=env)
                self.assertEqual(result.returncode, expected)
                self.assertEqual((state / "events").read_text().splitlines(), events)

    def test_sink_and_registry_must_use_same_bridgehub(self):
        command = ('source "$1"; gl_to_lower() { tr "[:upper:]" "[:lower:]" <<< "$1"; }; '
                   'gl_die() { exit 1; }; zksys_assert_shared_bridgehub "$2" "$3"')
        for first, second, expected in [('0xABC', '0xabc', 0), ('0xabc', '0xdef', 1)]:
            with self.subTest(first=first, second=second):
                result = subprocess.run(['bash', '-c', command, 'test', str(SERVICE), first, second])
                self.assertEqual(result.returncode, expected)

    def test_bridge_mode_rejects_unconfigured_seniority(self):
        for mode, first, second, expected in [
            ("false", "0", "0", 0),
            ("false", "3500", "10000", 1),
            ("true", "0", "0", 1),
            ("true", "3500", "10000", 0),
        ]:
            with self.subTest(mode=mode, first=first, second=second):
                command = 'source "$1"; gl_die() { exit 1; }; zksys_assert_service_mode_matches_bridge "$2" "$3" "$4"'
                result = subprocess.run(["bash", "-c", command, "test", str(SERVICE), mode, first, second])
                self.assertEqual(result.returncode, expected)

    def test_v1_rejects_mock_proofs(self):
        command = ('source "$1"; export L1_NETWORK=localhost PROVER_MODE=no-proofs '
                   'GATEWAY_PROVER_MODE=no-proofs EDGE_PROVER_MODE=no-proofs '
                   'SYSCOIN_ZKSYNC_OS_MOCK_VERIFIER=true ZKSYS_ENABLE_PROVER_SERVICE_V1=true; '
                   'gl_normalize_canonical_deployment_inputs')
        result = subprocess.run(["bash", "-c", command, "test", str(COMMON)], capture_output=True, text=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("requires real proofs", result.stderr)

    def test_connected_resume_requires_bound_period_zero_source(self):
        with tempfile.TemporaryDirectory() as directory:
            manifest = Path(directory) / 'zksys-service-v1.json'
            service = '0x' + '12' * 20
            manifest.write_text(json.dumps({'service': service}))
            os.chmod(manifest, 0o600)
            command = '''source "$1"
                gl_checkpoint_state_dir() { echo "$TEST_STATE_DIR"; }
                assert_l2_uint_call() { [ "$3" = 1 ]; }
                assert_l2_address_call() { [ "$3" = "$EXPECTED_SOURCE" ]; }
                assert_l2_bool_call() { [ "$3" = false ]; }
                export ZKSYS_L2_ISSUER_ADDRESS=issuer ZKSYS_L2_WEIGHT_REGISTRY_ADDRESS=weight
                zksys_assert_connected_service_v1'''
            for expected, result_code in [(service, 0), ('0x' + '34' * 20, 1)]:
                env = {**os.environ, 'EXPECTED_SOURCE': expected, 'TEST_STATE_DIR': directory}
                result = subprocess.run(['bash', '-ec', command, 'test', str(SERVICE), directory], env=env)
                self.assertEqual(result.returncode, result_code)


if __name__ == "__main__":
    unittest.main()
