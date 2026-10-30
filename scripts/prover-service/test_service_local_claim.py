import copy
import subprocess
import unittest

import service as s
import service_local_claim as local
import service_receipt_runner as machine
from test_service import fixture, h
from test_service_receipt_relay import FakeDriver, FakeHooks, report_values, tuple_text


class EncodingTests(unittest.TestCase):
    def test_mixed_claim_matches_cast(self):
        f = fixture()
        child, root = report_values(f), report_values(f, True)
        proof = {'blockOrBatchNumber': 9, 'messageIndex': 3, 'txNumberInBatch': 2,
                 'merkleProof': [local.METADATA] + [h(i) for i in range(1, 16)]}
        def report_text(report):
            accepted = tuple_text(s.PACKAGE, report['accepted'])
            duties = '[' + ','.join(tuple_text(s.DUTY + [('operatorSignature', 'bytes')], d)
                                    for d in report['duties']) + ']'
            return accepted + ',' + duties + ',false'
        proof_text = '(9,3,2,[' + ','.join(proof['merkleProof']) + '])'
        expected = subprocess.check_output(['cast', 'calldata', local.MIXED,
            '[(' + report_text(child) + ',' + proof_text + ')]',
            '[(' + report_text(root) + ')]'], text=True).strip().lower()
        self.assertEqual(local.calldata([child, root], [proof, None]), expected)


class Hooks(FakeHooks):
    def __init__(self, reports):
        super().__init__(reports)
        self.ready = set(r['id'] for r in reports)
        self.reject = lambda reports: False
        self.confirm = True
    def readiness(self, report):
        return {'proof': None} if report['id'] in self.ready else None
    def discover(self, cursor): return {'gateway': 2, 'l1': 2}, copy.deepcopy(self.reports)
    def collect_requests(self, records, previous, assigned, unrooted): return previous
    def publish_requests(self, requests): pass
    def already_claimed(self, report): return None
    def prepare_claim(self, reports, proofs):
        if self.reject(reports):
            raise machine.BatchRejected('simulated')
        return {'chain_id': 1, 'sender': '0x' + '01' * 20, 'to': '0x' + '02' * 20,
                'data': '0x12', 'value': 0, 'gas': 1, 'gas_price': 1}, {'proofs': proofs}
    def verify_claim_journal(self, entry, reports): pass
    def finalized_claims(self, receipt, reports): return self.confirm


class RunnerTests(unittest.TestCase):
    def setUp(self):
        self.reports = [{'id': str(i), 'kind': 'accepted', 'fromRoot': True, 'bootstrap': False,
                         'sourceBlockNumber': '0x1', 'sourceLogIndex': hex(i)} for i in range(1, 4)]
        self.state = {'transactions': {}}
        self.saved = []
        self.driver = FakeDriver(self.state)
        self.hooks = Hooks(self.reports)
        self.runner = local.ClaimRunner(self.state, lambda state: self.saved.append(copy.deepcopy(state)),
                                       self.driver, lambda *_: None, self.hooks, clock=lambda: 100)

    def test_waits_without_root_and_keeps_records(self):
        self.hooks.ready.clear()
        self.assertEqual(self.runner.run_once(), 'awaiting_roots_or_reports')
        self.assertEqual(len(self.state['records']), 3)
        self.assertEqual(self.driver.sent, 0)

    def test_dropped_transaction_restart_rebroadcast_and_confirm(self):
        self.assertEqual(self.runner.run_once(), 'claim_submitted')
        persisted = copy.deepcopy(self.state)
        self.driver.missing = True
        restarted = local.ClaimRunner(persisted, lambda state: None, FakeDriver(persisted),
                                     lambda *_: None, self.hooks, clock=lambda: 100)
        restarted.driver.missing = True
        self.assertEqual(restarted.run_once(), 'claim_pending')
        self.assertEqual(restarted.driver.rebroadcasts, 1)
        restarted.driver.missing = False
        self.hooks.confirm = False
        with self.assertRaisesRegex(local.TransactionError, 'state mismatch'):
            restarted.run_once()
        self.hooks.confirm = True
        self.assertEqual(restarted.run_once(), 'claimed')
        self.assertEqual([x['status'] for x in persisted['records'].values()], ['complete'] * 3)
        self.assertEqual(persisted['transactions'], {})

    def test_failed_batch_splits_without_losing_unready_members(self):
        self.hooks.reject = lambda batch: len(batch) > 1
        self.assertEqual(self.runner.run_once(), 'claim_split_or_retried')
        self.assertEqual(self.state['split'], [['1'], ['2', '3']])
        self.hooks.ready = {'2'}
        self.assertEqual(self.runner.run_once(), 'claim_submitted')
        self.assertEqual(self.runner.run_once(), 'claimed')
        self.assertEqual(self.state['split'], [['1'], ['3']])
        self.assertEqual(self.state['records']['3']['status'], 'pending')

    def test_unresolved_bootstrap_blocks_activation(self):
        self.reports[:] = [dict(self.reports[0], kind='unresolved', bootstrap=True),
                           dict(self.reports[1], kind='activation', fromRoot=False, roster_root=h(9)),
                           dict(self.reports[2], kind='activation', fromRoot=True, roster_root=h(9))]
        self.hooks.ready = {'2'}
        self.assertEqual(self.runner.run_once(), 'awaiting_bootstrap_or_retry')
        self.assertEqual(self.driver.sent, 0)

    def test_activation_prioritized_only_after_both_lanes_discovered(self):
        self.reports[:] = [self.reports[0], dict(self.reports[1], kind='activation', fromRoot=False, roster_root=h(9)),
                           dict(self.reports[2], kind='activation', fromRoot=True, roster_root=h(9))]
        self.assertEqual(self.runner.run_once(), 'claim_submitted')
        self.assertEqual(self.state['transactions']['claim']['metadata']['report_ids'], ['2'])

    def test_activation_does_not_wait_for_moving_source_head(self):
        self.reports[:] = [dict(self.reports[0], kind='activation', fromRoot=False, roster_root=h(9)),
                           dict(self.reports[1], kind='activation', fromRoot=True, roster_root=h(9))]
        self.assertEqual(self.runner.run_once(), 'claim_submitted')

    def test_activation_waits_for_matching_other_lane(self):
        self.reports[:] = [dict(self.reports[0], kind='activation', fromRoot=False, roster_root=h(9)),
                           dict(self.reports[1], kind='activation', fromRoot=True, roster_root=h(8))]
        self.assertEqual(self.runner.run_once(), 'awaiting_bootstrap_or_retry')
        self.assertEqual(self.driver.sent, 0)

    def test_external_activation_cannot_be_undone_by_worker_ordering(self):
        self.reports[:] = [dict(self.reports[0], bootstrap=True),
                           dict(self.reports[1], kind='activation', fromRoot=False, roster_root=h(9)),
                           dict(self.reports[2], kind='activation', fromRoot=True, roster_root=h(9))]
        self.hooks.already_claimed = lambda report: h(12) if report['kind'] == 'activation' else None
        self.hooks.reject = lambda reports: any(report['bootstrap'] for report in reports)
        self.assertEqual(self.runner.run_once(), 'claim_split_or_retried')
        self.assertEqual(self.state['records']['1']['status'], 'pending')
        self.assertEqual(self.state['records']['2']['status'], 'complete')
        self.assertEqual(self.state['records']['3']['status'], 'complete')


class RequestTests(unittest.TestCase):
    def test_root_requests_survive_and_only_bundle_on_new_batches(self):
        hooks = local.Hooks({}, None, None, None, None, 1)
        hooks.proof = lambda report: {'blockOrBatchNumber': report['batch']}
        native = {'kind': 'unresolved', 'fromRoot': True, 'bootstrap': False, 'packageHash': h(8)}
        child = {'kind': 'activation', 'fromRoot': False, 'batch': 9}
        records = {'a': {'report': native, 'status': 'pending'}, 'b': {'report': child, 'status': 'pending'}}
        requests, assigned, unrooted = {}, {}, {'b': True}
        result = hooks.collect_requests(records, requests, assigned, unrooted)
        digest = s.keccak(local.receipts.report_message(native))
        self.assertEqual(result, {'9': {'batch': 9, 'messageHash': digest}})
        self.assertEqual(hooks.collect_requests({}, result, assigned, unrooted), result)
        native2 = dict(native, packageHash=h(9))
        records['c'] = {'report': native2, 'status': 'pending'}
        result2 = hooks.collect_requests(records, result, assigned, unrooted)
        self.assertEqual(result2, result)
        records['d'] = {'report': dict(child, batch=10), 'status': 'pending'}
        unrooted['d'] = True
        result3 = hooks.collect_requests(records, result2, assigned, unrooted)
        self.assertIn({'batch': 10, 'messageHash': s.keccak(local.receipts.report_message(native2))}, result3.values())

    def test_consensus_limit_splits_before_signing(self):
        class Rpc:
            def call(self, method, params):
                if method == 'eth_estimateGas': return hex(local.MAX_GAS + 1)
                raise AssertionError(method)
        cfg = {'signer': '0x' + '01' * 20, 'receiver': '0x' + '02' * 20, 'zksys_chain_id': 5}
        hooks = local.Hooks(cfg, None, None, Rpc(), None, 1)
        with self.assertRaises(machine.BatchRejected):
            hooks.prepare_claim([report_values(fixture(), True)], [None])

    def test_transport_retry_is_bounded_and_security_failures_propagate(self):
        class FakeWorker:
            def __init__(self): self.calls = 0
            def run_once(self):
                self.calls += 1
                if self.calls <= 3: raise s.Error('rpc_transport_failure')
                raise s.Error('claim_journal_identity_changed')
        waits, lines = [], []
        with self.assertRaisesRegex(s.Error, 'claim_journal_identity_changed'):
            local.run_loop(FakeWorker(), 25, False, sleep=waits.append, emit=lines.append)
        self.assertEqual(waits, [25, 50, 60])
        self.assertEqual(len(lines), 3)
        with self.assertRaisesRegex(s.Error, 'rpc_transport_failure'):
            local.run_loop(FakeWorker(), 25, True, sleep=waits.append, emit=lines.append)


if __name__ == '__main__':
    unittest.main()
