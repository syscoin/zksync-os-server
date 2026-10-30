import base64
import copy
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import dispatcher
import qualification_runner as runner
import service as s
import test_dispatcher as td
from test_service import fixture, h, sign


class BootstrapRpc(td.Rpc):
    def __init__(self, settings, subscriptions):
        super().__init__(settings, subscriptions)
        self.active = False

    def contract(self, address, signature, values, anchor):
        if signature == 'firstServicePeriod()':
            return s.word(0)
        if signature == 'startTime()':
            return s.word(1000)
        return super().contract(address, signature, values, anchor)


class FakePool:
    def __init__(self, root, settings):
        self.root, self.settings, self.state = root, {'lanes': {'child': {
            'endpoint': 'https://trusted.example/', 'native_lease_seconds': 100,
            'identity': {'chain_id': settings['execution_chain_id'], 'chain_address': settings['chain_address'],
                         'settlement_chain_id': settings['settlement_chain_id'], 'vk_hash': settings['vk_hash']},
            'stages': {'FRI': {'acquisition': 'external'}}}}}, {'operations': {}}
        self.enqueues = 0

    def clock(self): return 450
    def bind_evidence(self, lane, payload, evidence, start, end):
        return ({'payload': s.keccak(payload), 'evidence': s.keccak(s.canonical(evidence)), 'lane': lane}, b'')
    def enqueue(self, lane, stage, identity, payload, evidence, deadline):
        self.enqueues += 1
        binding, _ = self.bind_evidence(lane, payload, evidence, 1, 1)
        self.state['operations']['pool-1'] = {'job_id': identity, 'mode': 'external', 'lane': lane,
            'stage': stage, 'deadline': deadline, 'chain_binding': binding, 'status': 'ready'}
        (self.root / 'pool-1').mkdir(exist_ok=True)
        return 'pool-1'
    def operation(self, key): return self.state['operations'][key]
    def directory(self, key): return self.root / key
    def bound_authority(self, key): return None
    def controller_store(self, op): return object()


class QualificationRunnerTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.base = fixture()

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        self.f = copy.deepcopy(self.base)
        self.f['subscription'].update(firstPeriod=0, lastPeriod=5)
        self.f['subscriptions'][0]['subscription'] = self.f['subscription']
        self.rpc = BootstrapRpc(self.f['settings'], self.f['subscriptions'])
        self.network = td.Network()
        self.store = dispatcher.initialize(self.root / 'dispatch', self.f['settings'], self.f['subscriptions'],
                                           0, 'https://trusted.example/', self.rpc)
        self.d = dispatcher.Dispatcher(self.store, self.network, 'Basic host-only', now=450, registry_rpc=self.rpc)
        self.pool = FakePool(self.root, self.f['settings'])
        self.account = self.f['subscription']['account']
        self.signatures = self.root / 'signatures'
        self.signatures.mkdir(mode=0o700)

    def sign_request(self, root, request, wallet, execute):
        return sign(request, 'sequencer' if request['signer'] == self.f['settings']['sequencer'] else 'operator')

    def step(self):
        with patch.object(runner.io, 'typed_signature', side_effect=self.sign_request), \
             patch.object(runner.s, 'round_closed', return_value=False):
            return runner.step(self.d, self.pool, self.rpc, object(), object(), self.signatures, self.account, 100, 450)

    def native_pick(self):
        evidence = copy.deepcopy(self.f['evidence'])
        evidence['batches'] = [evidence['batches'][0]]
        self.network.responses += [(200, {}, {'batch_number': 1, 'vk_hash': self.f['settings']['vk_hash'],
            'prover_input': base64.b64encode(b'\1\0\0\0').decode(), 'lease_token': h(101)}), (200, {}, evidence)]
        return self.step()

    def test_real_dispatcher_pick_offer_pool_and_native_acceptance(self):
        self.assertEqual(self.native_pick()['result'], 'job-000000')
        queued = self.step()
        self.assertEqual(queued['next_action'], 'await_pool_result')
        self.assertEqual(self.pool.enqueues, 1)
        self.assertEqual(self.step()['next_action'], 'await_pool_result')
        self.assertEqual(self.pool.enqueues, 1)
        proof = self.f['proofs'][0]
        path = self.pool.directory('pool-1') / 'returned-proof.json'
        path.write_text(json.dumps(proof))
        path.chmod(0o600)
        self.pool.operation('pool-1').update(status='returned', rental_operation='rental-1')
        self.network.responses.append((204, {'x-syscoin-prover-disposition': 'accepted'}, b''))
        with patch.object(runner.sentry, 'verify_input_result'):
            result = self.step()
        self.assertEqual(result['result'], 'accepted')
        self.assertEqual(self.d.state['operations']['job-000000']['status'], 'accepted')
        self.assertEqual(self.d.report()['accounts'][self.account]['credited_accepted'], 1)
        self.assertTrue((self.d.directory('job-000000') / 'duty.json').exists())

    def test_pending_submission_retries_saved_proof_and_never_picks_again(self):
        self.native_pick()
        self.step()
        operation = 'job-000000'
        proof = self.f['proofs'][0]
        signature = sign(self.d.proof_request(operation, proof), 'operator')
        self.network.responses.append(RuntimeError('network lost'))
        with self.assertRaisesRegex(RuntimeError, 'network lost'):
            self.d.submit(operation, proof, signature)
        self.d = dispatcher.Dispatcher(self.store, self.network, 'Basic host-only', now=450, registry_rpc=self.rpc)
        self.network.responses += [(404, {}, b''), (204, {'x-syscoin-prover-disposition': 'accepted'}, b'')]
        result = self.step()
        self.assertEqual(result['result'], 'accepted')
        self.assertEqual(self.pool.enqueues, 1)
        self.assertEqual(self.d.report()['accounts'][self.account]['credited_accepted'], 1)

    def test_uncertain_pick_fails_closed(self):
        self.network.responses.append(RuntimeError('lost pick'))
        with self.assertRaisesRegex(RuntimeError, 'lost pick'):
            self.step()
        self.d = dispatcher.Dispatcher(self.store, self.network, 'Basic host-only', now=450, registry_rpc=self.rpc)
        with self.assertRaisesRegex(s.Error, 'native_pick_uncertain_preserve_journal'):
            self.step()
        self.assertEqual(self.pool.enqueues, 0)

    def test_changed_or_expired_pool_job_fails_closed(self):
        self.native_pick()
        self.step()
        item = self.pool.operation('pool-1')
        item['deadline'] += 1
        with self.assertRaisesRegex(s.Error, 'qualification_pool_job_changed'):
            self.step()
        item['deadline'] -= 1
        item['status'] = 'authorization_expired'
        with self.assertRaisesRegex(s.Error, 'qualification_pool_job_failed'):
            self.step()
        self.assertEqual(self.pool.enqueues, 1)

    def test_restart_repairs_offer_file_after_journal_persisted(self):
        self.native_pick()
        original = dispatcher.atomic_json
        def interrupt(path, value):
            if Path(path).name == 'manifest.json':
                raise RuntimeError('interrupted offer')
            return original(path, value)
        with patch.object(dispatcher, 'atomic_json', side_effect=interrupt):
            with self.assertRaisesRegex(RuntimeError, 'interrupted offer'):
                self.step()
        self.d = dispatcher.Dispatcher(self.store, self.network, 'Basic host-only', now=450, registry_rpc=self.rpc)
        self.assertEqual(self.d.state['operations']['job-000000']['status'], 'offered')
        self.assertEqual(self.step()['next_action'], 'await_pool_result')
        self.assertTrue((self.d.directory('job-000000') / 'manifest.json').exists())
        self.assertEqual(self.d.state['accounts'][self.account]['offered'], 1)


if __name__ == '__main__':
    unittest.main()
