import copy
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import dispatcher
import enrollment
import keeper
import bootstrap_control
import bootstrap_package
import service as s
from test_dispatcher import Rpc
from test_keeper import setup
from test_service import fixture, h, sign


class EmptyBootstrapControlTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.base = fixture()

    def setUp(self):
        self.f = copy.deepcopy(self.base)
        self.f['settings']['coordinator'] = s.ZERO_ADDRESS
        self.f['subscriptions'] = []
        self.f['duties'] = []
        self.f['proposal'].update(mode='bootstrap', candidate=None, candidate_proof=[])
        self.f['proposal']['accepted_package'].update(wrapper=s.ZERO_ADDRESS, wrapperBeneficiary=s.ZERO_ADDRESS,
                                                     turn=0, rosterRoot=s.ZERO)
        payload = self.f['manifest']['payload']
        payload.update(subscription_snapshot_hash=s.keccak(s.canonical([])), assignments=[], retries=[])
        self.f['manifest']['sequencer_signature'] = sign(s.manifest_request(self.f['settings'], payload), 'sequencer')
        self.rpc = Rpc(self.f['settings'], [])
        self.rpc.active = False

    def authority(self):
        return s.EnrollmentAuthority(self.f['settings'], [], 5, self.rpc, h(100))

    def prepare(self, authority):
        return s.prepare_package(self.f['settings'], self.f['evidence'], self.f['manifest'], self.f['subscriptions'],
                                 self.f['duties'], self.f['proposal'], self.f['snark'], self.f['fri_payload'],
                                 enrollment=authority)

    def test_empty_bootstrap_proves_nonempty_batches_without_duty_credit(self):
        self.assertGreater(int(self.f['evidence']['batches'][0]['output']['l1TxCount'], 16), 0)
        authority = self.authority()
        self.assertTrue(authority.empty_bootstrap)
        prepared = self.prepare(authority)
        self.assertEqual(prepared['sidecar']['duties'], [])
        self.assertEqual(prepared['sidecar']['accepted_package']['reportHash'], s.report_hash([]))
        self.assertIsNone(prepared['wrapper_request'])
        request = {name: self.f[name] for name in ('proposal', 'manifest', 'subscriptions', 'duties')}
        self.assertEqual(keeper.prepare(self.f['settings'], request, self.f['evidence'], self.f['fri_payload'],
                                        enrollment=authority)['duties'], [])
        sidecar = s.complete_package(self.f['settings'], prepared, sign(prepared['sequencer_request'], 'sequencer'), '0x')
        self.assertEqual(sidecar['duties'], [])

    def test_empty_snapshot_cannot_omit_enrolled_account_or_enter_service(self):
        self.rpc.extra_enrolled = [self.base['subscription']['account']]
        with self.assertRaisesRegex(s.Error, 'omits_or_adds'):
            self.authority()
        self.rpc.extra_enrolled = []
        self.rpc.active = True
        with self.assertRaisesRegex(s.Error, 'empty_snapshot_requires_bootstrap'):
            self.authority()

    def test_empty_authority_cannot_create_paid_or_service_work(self):
        authority = self.authority()
        self.f['duties'] = copy.deepcopy(self.base['duties'])
        with self.assertRaisesRegex(s.Error, 'empty_bootstrap_control_only'):
            self.prepare(authority)
        self.f['duties'] = []
        self.f['proposal']['mode'] = 'service'
        with self.assertRaisesRegex(s.Error, 'empty_bootstrap_control_only'):
            self.prepare(authority)
        request = {name: self.f[name] for name in ('proposal', 'manifest', 'subscriptions', 'duties')}
        with self.assertRaisesRegex(s.Error, 'empty_bootstrap_control_only'):
            keeper.prepare(self.f['settings'], request, self.f['evidence'], self.f['fri_payload'], enrollment=authority)
        with self.assertRaisesRegex(s.Error, 'empty_bootstrap_cannot_join_service_group'):
            s.EnrollmentGroup(self.f['settings'], {h(12): authority}, h(12), self.rpc)

    def test_empty_bootstrap_cannot_create_paid_dispatcher(self):
        with tempfile.TemporaryDirectory() as parent:
            root = Path(parent).resolve() / 'journal'
            with self.assertRaisesRegex(s.Error, 'dispatcher_requires_enrolled_subscribers'):
                dispatcher.initialize(root, self.f['settings'], [], 5, 'http://127.0.0.1:3050', self.rpc)
            self.assertFalse(root.exists())
        class Store:
            root = Path('/private/unused')
        with patch.object(dispatcher.journal, 'load', return_value={'schema_version': 1, 'subscriptions': []}):
            with self.assertRaisesRegex(s.Error, 'dispatcher_requires_enrolled_subscribers'):
                dispatcher.Dispatcher(Store(), registry_rpc=self.rpc)

    def test_control_manifest_builder_pins_empty_enrollment_and_bootstrap_phase(self):
        config, _, _ = setup(self.base)
        config['settings'] = self.f['settings']
        config['policy']['coordinator_code_hash'] = s.ZERO
        config['policy']['expected_operator'] = self.f['settings']['sequencer']
        with patch.object(keeper, 'status', return_value={'mode': 'bootstrap'}):
            result = bootstrap_control.prepare(config, self.rpc, object(), 5, h(60), 450)
        self.assertEqual(result['subscriptions'], [])
        self.assertEqual(result['duties'], [])
        self.assertEqual(result['payload']['assignments'], [])
        self.assertEqual(result['enrollment_block_hash'], h(100))
        with patch.object(keeper, 'status', return_value={'mode': 'service'}):
            with self.assertRaisesRegex(s.Error, 'bootstrap_phase_closed'):
                bootstrap_control.prepare(config, self.rpc, object(), 5, h(60), 450)
        self.rpc.extra_enrolled = [self.base['subscription']['account']]
        with self.assertRaisesRegex(s.Error, 'omits_or_adds'):
            bootstrap_control.prepare(config, self.rpc, object(), 5, h(60), 450)

    def test_bootstrap_package_requires_both_native_verifications_and_open_gate(self):
        config, _, _ = setup(self.base)
        config['settings'] = self.f['settings']
        config['policy']['coordinator_code_hash'] = s.ZERO
        config['policy']['expected_operator'] = self.f['settings']['sequencer']
        request = {name: self.f[name] for name in ('proposal', 'manifest', 'subscriptions', 'duties')}
        release = {'stage': 'SNARK', 'vk_hash': self.f['settings']['vk_hash']}
        verifier = {'executable': '/trusted/verifier', 'sha256': 'a' * 64, 'timeout_seconds': 120}
        args = (config, self.rpc, object(), request, self.f['evidence'], self.f['fri_payload'],
                self.f['snark'], b'release', verifier, '/trusted/checks', '/trusted/lease', 450)
        with patch.object(bootstrap_package.wrapper.job, 'release_identity', return_value=release), \
             patch.object(bootstrap_package, 'retained_lease'), \
             patch.object(keeper, 'inspect', return_value={}) as inspected, \
             patch.object(bootstrap_package.wrapper, 'verify_fris', return_value={'verified': 'FRI'}) as fri, \
             patch.object(bootstrap_package.proof_check, 'verify_snark', return_value={'verified': 'SNARK'}) as snark:
            result = bootstrap_package.verify(*args)
            self.assertEqual(result['package']['sidecar']['duties'], [])
            self.assertEqual(result['fri_check'], {'verified': 'FRI'})
            self.assertEqual(result['snark_check'], {'verified': 'SNARK'})
            self.assertEqual(inspected.call_count, 2)
            fri.assert_called_once()
            snark.assert_called_once()
            fri.side_effect = s.Error('native_fri_verification_failed')
            with self.assertRaisesRegex(s.Error, 'native_fri_verification_failed'):
                bootstrap_package.verify(*args)
            self.assertEqual(snark.call_count, 1)
            fri.side_effect = None
            snark.side_effect = s.Error('native_snark_rejected')
            with self.assertRaisesRegex(s.Error, 'native_snark_rejected'):
                bootstrap_package.verify(*args)

    def test_bootstrap_request_binds_empty_signed_manifest_and_native_range(self):
        config, _, _ = setup(self.base)
        config['settings'] = self.f['settings']
        config['policy']['coordinator_code_hash'] = s.ZERO
        config['policy']['expected_operator'] = self.f['settings']['sequencer']
        manifest = self.f['manifest']
        expected = {'payload': manifest['payload'], 'request': s.manifest_request(self.f['settings'], manifest['payload'])}
        state = {'mode': 'bootstrap', 'package_open': False, 'parent': self.f['proposal']['accepted_package']['parent']}
        with patch.object(bootstrap_control, 'prepare', return_value=expected), \
             patch.object(keeper, 'status', return_value=state), \
             patch.object(keeper, 'package_call', return_value={'action': 'openBootstrapPackage'}) as call:
            result = bootstrap_package.request(config, self.rpc, object(), manifest,
                self.f['proposal']['accepted_package']['sequencerBeneficiary'], self.f['evidence'], self.f['fri_payload'], 450)
            self.assertEqual(result['request']['proposal']['mode'], 'bootstrap')
            self.assertEqual(result['request']['proposal']['accepted_package']['batchFrom'],
                             self.f['fri_payload']['from_batch_number'])
            self.assertEqual(result['request']['duties'], [])
            call.assert_called_once()
            manifest = copy.deepcopy(manifest)
            manifest['payload']['assignments'] = [self.base['manifest']['payload']['assignments'][0]]
            with self.assertRaisesRegex(s.Error, 'bootstrap_manifest_changed'):
                bootstrap_package.request(config, self.rpc, object(), manifest,
                    self.f['proposal']['accepted_package']['sequencerBeneficiary'], self.f['evidence'], self.f['fri_payload'], 450)

    def test_bootstrap_package_rejects_nonempty_subscribers_or_missing_pins(self):
        config, _, _ = setup(self.base)
        config['settings'] = self.f['settings']
        config['policy']['coordinator_code_hash'] = s.ZERO
        config['policy']['expected_operator'] = self.f['settings']['sequencer']
        request = {name: self.f[name] for name in ('proposal', 'manifest', 'subscriptions', 'duties')}
        request['subscriptions'] = self.base['subscriptions']
        args = (config, self.rpc, object(), request, self.f['evidence'], self.f['fri_payload'],
                self.f['snark'], b'release', {}, '/trusted/checks', '/trusted/lease', 450)
        with self.assertRaisesRegex(s.Error, 'empty_bootstrap_control_only'):
            bootstrap_package.verify(*args)

    def test_retained_lease_binds_accepted_exact_submission(self):
        job = bootstrap_package.wrapper.job
        release = b'reviewed-release'
        payload, proof = self.f['fri_payload'], self.f['snark']
        token = h(70)
        submission = job.encode({**proof, 'lease_token': token})
        authority = {'stage': 'SNARK', 'status': 'accepted', 'release_sha256': job.hash_bytes(release),
                     'bounds': {key: payload[key] for key in ('from_batch_number', 'to_batch_number')},
                     'submission_sha256': job.hash_bytes(submission), 'lease_token': token}
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            for name, value in (('authority.json', job.encode(authority)), ('release.json', release),
                                ('payload.json', job.encode(payload)), ('submission.json', submission)):
                path = root / name
                path.write_bytes(value)
                path.chmod(0o600)
            bootstrap_package.retained_lease(root, payload, proof, release)
            changed = {**proof, 'proof': self.base['snark']['proof'] + 'A'}
            with self.assertRaisesRegex(s.Error, 'retained_submission_changed'):
                bootstrap_package.retained_lease(root, payload, changed, release)
            authority['status'] = 'submission_pending'
            (root / 'authority.json').write_bytes(job.encode(authority))
            with self.assertRaisesRegex(s.Error, 'accepted_native_lease_required'):
                bootstrap_package.retained_lease(root, payload, proof, release)


if __name__ == '__main__':
    unittest.main()
