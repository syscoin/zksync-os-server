import copy
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import bootstrap_enrollment as enrollment
import post_deployment as setup
import roster
import service as s
import test_bootstrap_enrollment as enrollment_tests
from test_keeper import setup as keeper_setup
from test_service import fixture, a, h


class PostDeploymentTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        enrollment_tests.BootstrapEnrollmentTests.setUpClass()
        cls.fixture = fixture()

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        self.ecfg = copy.deepcopy(enrollment_tests.BootstrapEnrollmentTests.cfg)
        self.signed = copy.deepcopy(enrollment_tests.BootstrapEnrollmentTests.signed)
        self.signature = enrollment_tests.BootstrapEnrollmentTests.signature
        self.rpc = enrollment_tests.EnrollmentRpc(self.ecfg, self.signed, enrollment_tests.BootstrapEnrollmentTests.prepared)
        self.kcfg, _, _ = keeper_setup(self.fixture)
        self.kcfg['settings'] = self.ecfg['settings']
        self.kcfg['policy']['expected_operator'] = self.ecfg['settings']['sequencer']
        self.kcfg['policy']['coordinator_code_hash'] = s.ZERO
        self.rcfg = {'chain_id': s.uint(self.ecfg['settings']['registry_chain_id']),
                     'genesis_hash': self.ecfg['genesis_hash'], 'registry': self.ecfg['settings']['registry'],
                     'registry_code_hash': self.ecfg['registry_code_hash'], 'publisher': a(90),
                     'rpc_url': self.ecfg['rpc_url'], 'max_head_age_seconds': 60, 'rpc_timeout_seconds': 15,
                     'gas_limit': 1000000, 'gas_price': 1, 'keystore': '/private/test-key',
                     'password_file': '/private/test-password'}
        for name, obj in (('keeper.json', self.kcfg), ('enrollment.json', self.ecfg), ('roster.json', self.rcfg),
                          ('signed.json', self.signed)):
            self.write(name, json.dumps(obj))
        self.write('operator.sig', self.signature)
        for name in ('enrollment-state', 'roster-state', 'fri-checks'):
            (self.root / name).mkdir(mode=0o700)
        self.cfg = {'schema_version': 1, 'bootstrap_keeper_config': str(self.root / 'keeper.json'),
                    'enrollment_config': str(self.root / 'enrollment.json'), 'roster_config': str(self.root / 'roster.json'),
                    'enrollment_state_dir': str(self.root / 'enrollment-state'),
                    'roster_state_dir': str(self.root / 'roster-state'), 'fri_check_dir': str(self.root / 'fri-checks')}

    def write(self, name, contents):
        path = self.root / name
        path.write_text(contents)
        path.chmod(0o600)

    def test_enrollment_preview_send_and_restart_use_existing_durable_journal(self):
        inputs = {'subscription': str(self.root / 'signed.json'), 'operator_signature': str(self.root / 'operator.sig')}
        with patch.object(setup.relay, 'Rpc', return_value=self.rpc), \
             patch.object(enrollment, 'CastSigner', enrollment_tests.FakeSigner), patch.object(enrollment, 'private_file'):
            result = setup.perform(self.cfg, 'enroll', inputs, now=1000)
            self.assertEqual(result['status'], 'ready')
            self.assertEqual(self.rpc.sent, [])
            result = setup.perform(self.cfg, 'enroll', inputs, execute=True, now=1000)
            self.assertEqual(result['status'], 'submitted')
            result = setup.perform(self.cfg, 'enroll', inputs, execute=True, now=1000)
            self.assertEqual(result['status'], 'pending')
            self.assertEqual(self.rpc.sent, [self.rpc.sent[0], self.rpc.sent[0]])

    def test_registry_and_signer_configuration_mismatch_fails_before_dispatch(self):
        self.rcfg['registry_code_hash'] = s.keccak(b'different')
        self.write('roster.json', json.dumps(self.rcfg))
        with patch.object(setup.bootstrap_control, 'prepare') as prepare:
            with self.assertRaisesRegex(s.Error, 'setup_registry_identity_changed'):
                setup.perform(self.cfg, 'manifest', {'previous_cursor': '0x' + '11' * 32}, now=1000)
            prepare.assert_not_called()
        self.rcfg['registry_code_hash'] = self.ecfg['registry_code_hash']
        self.write('roster.json', json.dumps(self.rcfg))
        self.ecfg['settings']['sequencer'] = a(99)
        self.write('enrollment.json', json.dumps(self.ecfg))
        with self.assertRaisesRegex(s.Error, 'setup_keeper_enrollment_identity_changed'):
            setup.load_configs(self.cfg)

    def test_manifest_delegates_period_zero_and_proof_action_cannot_send(self):
        inputs = {'previous_cursor': '0x' + '11' * 32}
        with patch.object(setup.keeper, 'registry_rpc_for', return_value='registry'), \
             patch.object(setup.keeper, 'rpc_for', return_value='settlement'), \
             patch.object(setup.bootstrap_control, 'prepare', return_value={'request': 'typed'}) as prepare:
            self.assertEqual(setup.perform(self.cfg, 'manifest', inputs, now=1000), {'request': 'typed'})
            self.assertEqual(prepare.call_args.args[3:], (0, inputs['previous_cursor'], 1000))
        with self.assertRaisesRegex(s.Error, 'execute_not_supported_for_step'):
            setup.perform(self.cfg, 'manifest', inputs, execute=True, now=1000)

    def test_roster_preview_does_not_send_and_passes_authenticated_artifact(self):
        artifact = roster.construct(0, [{'index': 0, 'account': a(1), 'operator': a(2), 'beneficiary': a(3)}])
        self.write('artifact.json', json.dumps(artifact))
        with patch.object(setup.relay, 'Rpc', return_value=self.rpc), \
             patch.object(setup.roster, 'plan_page', return_value={'status': 'ready'}) as plan, \
             patch.object(setup.roster, 'run_send') as send:
            self.assertEqual(setup.perform(self.cfg, 'publish-roster', {'roster': str(self.root / 'artifact.json')}, now=1000),
                             {'status': 'ready'})
            self.assertEqual(plan.call_args.args[2], artifact)
            send.assert_not_called()

    def test_published_status_requires_matching_finalized_roster(self):
        artifact = roster.construct(0, [{'index': 0, 'account': a(1), 'operator': a(2), 'beneficiary': a(3)}])
        block = {'hash': h(9)}
        state = {'root': s.ZERO, 'count': 0}
        with patch.object(setup.roster, 'read_snapshot', return_value=(block, {'blockHash': h(9)})), \
             patch.object(setup.roster, 'registry_state', return_value=state), \
             patch.object(setup.roster, 'check_anchor'):
            result = setup.finalized_roster(self.rpc, self.rcfg, artifact, {'status': 'published'})
            self.assertEqual(result['status'], 'awaiting_finalized_roster')
            state.update(root=artifact['root'], count=artifact['count'])
            result = setup.finalized_roster(self.rpc, self.rcfg, artifact, {'status': 'published'})
            self.assertEqual(result, {'status': 'published', 'root': artifact['root'], 'finalized_block_hash': h(9)})
            state['root'] = h(10)
            with self.assertRaisesRegex(s.Error, 'finalized_roster_conflict'):
                setup.finalized_roster(self.rpc, self.rcfg, artifact, {'status': 'published'})

    def test_machine_readable_report_names_unorchestrated_steps(self):
        value = setup.report('enroll', {'status': 'enrolled'})
        self.assertFalse(value['service_ready_verified'])
        self.assertEqual(value['result']['status'], 'enrolled')
        self.assertEqual(set(value['not_orchestrated']), {'proof_generation', 'qualifying_duties',
                         'prover_role_cutover', 'message_delivery', 'service_activation'})


if __name__ == '__main__':
    unittest.main()
