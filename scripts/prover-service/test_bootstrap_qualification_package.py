import base64
import copy
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import bootstrap_qualification_package as q
import dispatcher
import keeper
import service as s
import test_dispatcher as td
import test_qualification_runner as tr
from test_keeper import setup as keeper_setup
from test_service import fixture, h, sign


class BootstrapQualificationPackageTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.base = fixture()

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        self.f = copy.deepcopy(self.base)
        self.f['settings']['coordinator'] = s.ZERO_ADDRESS
        self.f['subscription'].update(firstPeriod=0, lastPeriod=5)
        self.f['subscriptions'][0]['subscription'] = self.f['subscription']
        self.rpc = tr.BootstrapRpc(self.f['settings'], self.f['subscriptions'])
        self.network = td.Network()
        self.store = dispatcher.initialize(self.root / 'dispatch', self.f['settings'], self.f['subscriptions'],
                                           0, 'https://trusted.example/', self.rpc)
        self.d = dispatcher.Dispatcher(self.store, self.network, 'Basic host-only', now=450, registry_rpc=self.rpc)
        self.config, _, _ = keeper_setup(self.base)
        self.config['settings'] = self.f['settings']
        self.config['policy']['expected_operator'] = self.f['settings']['sequencer']
        self.config['policy']['coordinator_code_hash'] = s.ZERO
        self.account = self.f['subscription']['account']
        for number in (1, 2):
            self.make_accepted(number)
        self.payload = {'from_batch_number': 1, 'to_batch_number': 2, 'vk_hash': self.f['settings']['vk_hash'],
                        'fri_proofs': [value['proof'] for value in self.f['proofs']]}
        self.trust = {'schema_version': 1, 'journal_id': self.d.state['journal_id'], 'enrollment_block_hash': h(100),
            'minimum_checkpoint': {'event_count': 0, 'event_head': self.d.state['journal_id'],
                                   'assignment_cursor': self.d.state['journal_id']},
            'required_readiness': [], 'required_duties': []}
        self.events = self.root / 'events'
        self.events.mkdir(mode=0o700)
        self.signatures = self.root / 'signatures'
        self.signatures.mkdir(mode=0o700)

    def evidence(self, number):
        value = copy.deepcopy(self.f['evidence'])
        if number > 1:
            value['previous_batch'] = value['batches'][number - 2]['stored']
        value['batches'] = [value['batches'][number - 1]]
        return value

    def make_accepted(self, number):
        ready = self.d.request(self.account, 500)
        self.d.ready(ready, sign(ready, 'operator'))
        self.network.responses += [(200, {}, {'batch_number': number, 'vk_hash': self.f['settings']['vk_hash'],
            'prover_input': base64.b64encode(b'\1\0\0\0').decode(), 'lease_token': h(150 + number)}),
            (200, {}, self.evidence(number))]
        operation = self.d.pick()
        self.d.authorize(operation, sign(self.d.offer_request(operation), 'sequencer'))
        proof = self.f['proofs'][number - 1]
        signature = sign(self.d.proof_request(operation, proof), 'operator')
        self.network.responses.append((204, {'x-syscoin-prover-disposition': 'accepted'}, b''))
        self.d.submit(operation, proof, signature)

    def native_peeks(self, count=1):
        for _ in range(count):
            for number in (1, 2):
                self.network.responses += [(200, {}, {'from_batch_number': number, 'to_batch_number': number,
                    'vk_hash': self.f['settings']['vk_hash'], 'fri_proofs': [self.f['proofs'][number - 1]['proof']]}),
                    (200, {}, self.evidence(number))]

    def create(self):
        self.native_peeks(2)
        parent = self.f['proposal']['accepted_package']['parent']
        with patch.object(q.io, 'typed_signature', side_effect=lambda root, req, wallet, execute: sign(req, 'sequencer')), \
             patch.object(keeper, 'status', return_value={'mode': 'bootstrap', 'package_open': False, 'parent': parent}), \
             patch.object(keeper, 'package_call', side_effect=lambda cfg, rpc, req, ev, pay, now, action, **kw:
                          {'prepared': keeper.prepare(cfg['settings'], req, ev, pay, **kw)}):
            return q.request(self.config, self.d, self.rpc, object(), self.f['evidence'], self.payload,
                self.f['proposal']['accepted_package']['sequencerBeneficiary'], object(), self.signatures,
                self.trust, self.events, 450)

    def test_real_audit_replay_and_package_carries_accepted_duties(self):
        result = self.create()
        request = result['request']
        self.assertEqual([duty['batchNumber'] for duty in request['duties']], [1, 2])
        self.assertEqual([duty['slot'] for duty in request['duties']], [0, 1])
        self.assertEqual(result['open_call']['prepared']['accepted_package']['reportHash'], s.report_hash(request['duties']))
        self.native_peeks()
        release = {'stage': 'SNARK', 'vk_hash': self.f['settings']['vk_hash']}
        verifier = {'executable': '/trusted/fri', 'sha256': 'a' * 64, 'timeout_seconds': 120}
        with patch.object(q.bootstrap_package, 'retained_lease') as lease, \
             patch.object(q.wrapper.job, 'release_identity', return_value=release), \
             patch.object(q.wrapper, 'verify_fris', return_value={'fri': 'verified'}) as fri, \
             patch.object(q.proof_check, 'verify_snark', return_value={'snark': 'verified'}) as snark, \
             patch.object(keeper, 'inspect'):
            checked = q.verify(self.config, self.d, self.rpc, object(), self.f['evidence'], self.payload,
                self.f['snark'], request, result['audit'], self.trust, b'release', verifier, self.root,
                '/trusted/lease', self.events, 450)
        self.assertEqual(checked['package']['sidecar']['duties'], request['duties'])
        lease.assert_called_once()
        fri.assert_called_once()
        snark.assert_called_once()

    def test_changed_native_fri_cannot_receive_credit(self):
        self.native_peeks()
        changed = copy.deepcopy(self.payload)
        changed['fri_proofs'][0] = changed['fri_proofs'][1]
        with self.assertRaisesRegex(s.Error, 'qualification_duty_changed'):
            q.snapshot(self.config, self.d, self.f['evidence'], changed)

    def test_omitted_duty_or_untrusted_audit_rejected(self):
        result = self.create()
        request = copy.deepcopy(result['request'])
        request['duties'].pop()
        self.native_peeks()
        with self.assertRaisesRegex(s.Error, 'qualification_journal_range_changed'):
            q.validate_audit(self.config, self.d, self.rpc, self.f['evidence'], self.payload,
                             request, result['audit'], self.trust, self.events)
        trust = {**self.trust, 'journal_id': h(200)}
        self.native_peeks()
        with self.assertRaisesRegex(s.Error, 'audit_untrusted_journal'):
            q.validate_audit(self.config, self.d, self.rpc, self.f['evidence'], self.payload,
                             result['request'], result['audit'], trust, self.events)


if __name__ == '__main__':
    unittest.main()
