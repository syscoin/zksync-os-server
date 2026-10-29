import copy
import json
import subprocess
import unittest
from unittest.mock import patch

import relay
import service as s
import service_receipt_relay as worker
import service_receipt_runner as machine
from test_service import fixture, h, a


def report_values(f, root=False):
    accepted = copy.deepcopy(f['proposal']['accepted_package'])
    accepted.update(manifestHash=h(1), reportHash=s.report_hash(f['duties']), proofHash=h(2))
    return {'id': ('root:' if root else 'child:') + '11' * 32, 'kind': 'accepted', 'fromRoot': root,
            'bootstrap': False, 'accepted': accepted, 'duties': f['duties'],
            'sourceBlockNumber': '0x1', 'sourceLogIndex': '0x0'}


def tuple_text(fields, value):
    return '(' + ','.join(str(value[name]) for name, _ in fields) + ')'


class FakeDriver:
    def __init__(self, state): self.state, self.sent, self.missing, self.rebroadcasts = state, 0, False, 0
    def begin(self, name, rpc, *, kind, metadata, **request):
        self.sent += 1
        self.state['transactions'][name] = {'kind': kind, 'metadata': copy.deepcopy(metadata),
                                            'txHash': h(7), 'raw': '0x12', 'state': 'submitted'}
    def receipt(self, name, rpc):
        return None if self.missing else {'transactionHash': h(7), 'blockHash': h(8)}
    def rebroadcast_if_missing(self, name, rpc): self.rebroadcasts += 1


class FakeHooks:
    def __init__(self, reports): self.reports, self.error, self.resolutions = reports, None, {}
    def verify_deployment(self): return True
    def discover(self, cursor): return {'at': 1}, copy.deepcopy(self.reports)
    def validate_report(self, report):
        if 'id' not in report: raise ValueError('bad report')
    def hydrate(self, report): return copy.deepcopy(self.resolutions.get(report['id']))
    def validate_resolution(self, prior, result):
        if prior['id'] != result['id']: raise ValueError('resolution mismatch')
    def prepare(self, reports):
        if self.error and self.error(reports): raise machine.BatchRejected('reverted')
        return ({'chain_id': 1, 'sender': a(1), 'to': a(2), 'data': '0x12', 'value': 1, 'gas': 1, 'gas_price': 1},
                {'childData': '0x12'})
    def verify_journal(self, entry, reports): pass
    def priority_hash(self, receipt, entry, reports): return h(9)
    def l2_chain_id(self, rpc): return True
    def verify_delivery(self, receipt, reports): return True


class RunnerTests(unittest.TestCase):
    def report(self, n, root=False, bootstrap=False, kind='accepted'):
        return {'id': str(n), 'kind': kind, 'fromRoot': root, 'bootstrap': bootstrap,
                'sourceBlockNumber': '0x1', 'sourceLogIndex': hex(n)}
    def setup_runner(self, reports):
        state, saved = {'transactions': {}}, []
        driver = FakeDriver(state)
        hooks = FakeHooks(reports)
        child = {'status': None}
        def l2(method, args):
            if method == 'eth_getTransactionReceipt':
                return None if child['status'] is None else {'transactionHash': h(9), 'blockHash': h(10),
                    'blockNumber': '0x5', 'status': child['status']}
            if method == 'eth_getBlockByNumber': return {'number': '0x5', 'hash': h(10)}
            raise AssertionError(method)
        save = lambda value: saved.append(copy.deepcopy(value))
        runner = machine.ReceiptRunner(state, save, driver, lambda *_: None, l2, hooks, clock=lambda: 100)
        return runner, state, saved, driver, hooks, child
    def test_aggregate_more_than_32_mixed_reports(self):
        runner, state, _, driver, _, _ = self.setup_runner([self.report(i, i % 2 == 0) for i in range(100)])
        self.assertEqual(runner.run_once(), 'submitted')
        self.assertEqual(len(state['transactions']['receipt']['metadata']['report_ids']), 100)
        self.assertEqual(driver.sent, 1)
    def test_restart_waits_for_finalized_child_and_rebroadcasts_l1(self):
        runner, state, _, driver, _, child = self.setup_runner([self.report(1)])
        self.assertEqual(runner.run_once(), 'submitted')
        driver.missing = True
        self.assertEqual(runner.run_once(), 'l1_pending')
        self.assertEqual(driver.rebroadcasts, 1)
        driver.missing = False
        self.assertEqual(runner.run_once(), 'l2_pending')
        self.assertIn('receipt', state['transactions'])
        child['status'] = '0x1'
        self.assertEqual(runner.run_once(), 'delivered')
        self.assertEqual(state['records']['1']['status'], 'complete')
        self.assertEqual(state['transactions'], {})
    def test_finalized_child_revert_splits_and_retains_every_report(self):
        runner, state, _, _, _, child = self.setup_runner([self.report(1), self.report(2), self.report(3)])
        runner.run_once()
        child['status'] = '0x0'
        self.assertEqual(runner.run_once(), 'l2_reverted_requeued')
        self.assertEqual(state['split'], [['1'], ['2', '3']])
        self.assertTrue(all(v['status'] == 'pending' for v in state['records'].values()))
        self.assertEqual(runner.run_once(), 'submitted')
        self.assertEqual(state['transactions']['receipt']['metadata']['report_ids'], ['1'])
    def test_simulation_split_never_drops_or_blocks_unrelated_report(self):
        runner, state, _, _, hooks, _ = self.setup_runner([self.report(1), self.report(2), self.report(3)])
        hooks.error = lambda reports: any(x['id'] == '1' for x in reports)
        self.assertEqual(runner.run_once(), 'simulation_reverted_requeued')
        self.assertEqual(runner.run_once(), 'simulation_reverted_requeued')
        self.assertEqual(runner.run_once(), 'submitted')
        self.assertEqual(set(state['transactions']['receipt']['metadata']['report_ids']), {'2', '3'})
        self.assertGreater(state['records']['1']['nextRetryAt'], 100)
    def test_pending_bootstrap_reports_precede_activation(self):
        runner, state, _, _, hooks, _ = self.setup_runner([self.report(1, bootstrap=True), self.report(2, kind='activation')])
        hooks.error = lambda reports: reports[0]['kind'] == 'accepted'
        self.assertEqual(runner.run_once(), 'simulation_reverted_requeued')
        self.assertEqual(runner.run_once(), 'idle')
        self.assertEqual(state['records']['2']['status'], 'pending')
    def test_active_transaction_resumes_even_if_new_discovery_fails(self):
        runner, state, _, driver, hooks, _ = self.setup_runner([self.report(1)])
        runner.run_once()
        hooks.discover = lambda cursor: (_ for _ in ()).throw(ValueError('artifact missing'))
        driver.missing = True
        self.assertEqual(runner.run_once(), 'l1_pending')
        self.assertIn('receipt', state['transactions'])

    def test_missing_artifact_is_retained_while_unrelated_report_proceeds(self):
        missing = self.report(1, kind='unresolved')
        runner, state, _, _, hooks, child = self.setup_runner([missing, self.report(2)])
        self.assertEqual(runner.run_once(), 'submitted')
        self.assertEqual(state['transactions']['receipt']['metadata']['report_ids'], ['2'])
        self.assertEqual(state['records']['1']['report']['kind'], 'unresolved')
        child['status'] = '0x1'
        self.assertEqual(runner.run_once(), 'delivered')
        hooks.reports = []
        hooks.resolutions['1'] = self.report(1)
        self.assertEqual(runner.run_once(), 'submitted')
        self.assertEqual(state['transactions']['receipt']['metadata']['report_ids'], ['1'])

    def test_unresolved_bootstrap_blocks_activation_not_other_reports(self):
        missing = self.report(1, kind='unresolved', bootstrap=True)
        runner, state, _, _, _, child = self.setup_runner([missing, self.report(2), self.report(3, kind='activation')])
        self.assertEqual(runner.run_once(), 'submitted')
        self.assertEqual(state['transactions']['receipt']['metadata']['report_ids'], ['2'])
        child['status'] = '0x1'
        self.assertEqual(runner.run_once(), 'delivered')
        self.assertEqual(runner.run_once(), 'awaiting_retained_artifacts')
        self.assertEqual(state['records']['3']['status'], 'pending')


if __name__ == '__main__': unittest.main()
