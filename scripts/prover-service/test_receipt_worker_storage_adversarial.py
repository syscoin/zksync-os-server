"""Cross-worker persistence and interruption regressions for receipt journals."""
import copy
import sqlite3
from pathlib import Path
import tempfile
import threading
import unittest
from unittest.mock import patch

import journal
import receipt_storage as storage
import relay
import service as s
import service_local_claim as local
import service_gateway_root_relayer as gateway
import service_native_receipt_relayer as native
from test_service_local_claim import Hooks as ClaimHooks
from test_service_native_receipt_relayer import FakeHooks as NativeHooks
from test_service_gateway_root_relayer import Hooks as GatewayHooks, Driver as GatewayDriver
from test_service_receipt_relay import FakeDriver, report_values
from test_service import fixture, h


def state():
    return {'version': 1, 'identity': 'a' * 64, 'transactions': {}, 'records': {},
            'cursor': {'gateway': 10, 'l1': 11}, 'split': [], 'history': [],
            'rootRequests': {}, 'rootRequestHashes': {}}


class ReceiptStorageAdversarialTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.path = Path(self.tmp.name) / 'claim.json'

    def legacy(self, value):
        relay.atomic_json(self.path, value)
        return storage.load(self.path, 'claim')

    def test_claim_worker_external_completion_survives_indexed_restart(self):
        old = state()
        old['records'] = {str(i): {'report': {'id': str(i), 'fromRoot': True, 'kind': 'accepted'},
                                     'status': 'complete'} for i in range(512)}
        pending = {'id': 'pending', 'kind': 'accepted', 'fromRoot': True, 'bootstrap': False,
                   'sourceBlockNumber': '0x1', 'sourceLogIndex': '0x0'}
        old['records']['pending'] = {'report': pending, 'status': 'pending', 'nextRetryAt': 0, 'failures': 0}
        live = self.legacy(old)
        storage.save(self.path, live, 'claim')
        hooks = ClaimHooks([])
        hooks.already_claimed = lambda report: h(12)
        worker = local.ClaimRunner(live, lambda value: storage.save(self.path, value, 'claim'),
                                   FakeDriver(live), lambda *_: None, hooks, clock=lambda: 100)
        self.assertEqual(worker.run_once(), 'awaiting_roots_or_reports')
        live.database.close()
        restored = storage.load(self.path, 'claim')
        self.addCleanup(restored.database.close)
        self.assertEqual(restored['records']['pending']['status'], 'complete')
        self.assertEqual(restored['records']['pending']['observedFinalizedBlockHash'], h(12))

    def test_inflight_claim_restarts_with_identical_saved_transaction(self):
        old = state()
        live = self.legacy(old)
        report = {'id': 'pending', 'kind': 'accepted', 'fromRoot': True, 'bootstrap': False,
                  'sourceBlockNumber': '0x1', 'sourceLogIndex': '0x0'}
        hooks = ClaimHooks([report])
        with patch.object(journal, 'MAX_ROWS', 1):
            driver = FakeDriver(live)
            worker = local.ClaimRunner(live, lambda value: storage.save(self.path, value, 'claim'),
                                       driver, lambda *_: None, hooks, clock=lambda: 100)
            self.assertEqual(worker.run_once(), 'claim_submitted')
            original = copy.deepcopy(live['transactions']['claim'])
            storage.save(self.path, live, 'claim')
            live.database.close()
            restarted = storage.load(self.path, 'claim')
            driver = FakeDriver(restarted)
            driver.missing = True
            worker = local.ClaimRunner(restarted, lambda value: storage.save(self.path, value, 'claim'),
                                       driver, lambda *_: None, hooks, clock=lambda: 100)
            self.assertEqual(worker.run_once(), 'claim_pending')
            self.assertEqual(driver.rebroadcasts, 1)
            self.assertEqual(restarted['transactions']['claim'], original)
            driver.missing = False
            self.assertEqual(worker.run_once(), 'claimed')
            restarted.database.close()
            final = storage.load(self.path, 'claim')
            self.addCleanup(final.database.close)
            self.assertEqual(final['records']['pending']['status'], 'complete')
            self.assertNotIn('claim', final['transactions'])

    def test_native_releases_claim_snapshot_before_hook_work(self):
        old = state()
        report = report_values(fixture(), True)
        report['id'] = 'pending'
        old['records']['pending'] = {'report': report, 'status': 'pending', 'nextRetryAt': 0, 'failures': 0}
        live = self.legacy(old)
        with patch.object(journal, 'MAX_ROWS', 1):
            storage.save(self.path, live, 'claim')
        live.database.close()
        database = self.path.with_name(self.path.name + '.sqlite3')
        class Hook(NativeHooks):
            def validate_report(self, report):
                con = sqlite3.connect(database, timeout=.1)
                try:
                    con.execute('BEGIN EXCLUSIVE')
                    con.execute('COMMIT')
                finally:
                    con.close()
        native_state = {'transactions': {}, 'records': {}, 'split': [], 'history': []}
        worker = native.Runner(native_state, lambda _: None, None, None, None, Hook(),
                               claim_journal=str(self.path), claim_identity=old['identity'])
        worker.discover()
        self.assertIn('pending', native_state['records'])

    def test_root_worker_restarts_inflight_and_persists_delivery(self):
        root_path = Path(self.tmp.name) / 'root.json'
        digest, root = h(55), h(33)
        old = {'version': 1, 'identity': 'root', 'completed': [{'batch': 1, 'messageHash': s.ZERO}],
               'transactions': {'root': {'state': 'submitted', 'raw': '0x1234', 'txHash': h(4),
                  'metadata': {'batch': 9, 'messageHash': digest, 'root': root}}}, 'nextRetryAt': 0, 'history': []}
        relay.atomic_json(root_path, old)
        live = storage.load(root_path, 'root')
        with patch.object(journal, 'MAX_ROWS', 1):
            storage.save(root_path, live, 'root')
        live.database.close()
        restarted = storage.load(root_path, 'root')
        driver, hooks = GatewayDriver(restarted), GatewayHooks()
        cfg = {'zksys_chain_id': 2}
        self.assertEqual(gateway.step(cfg, restarted, lambda value: storage.save(root_path, value, 'root'),
                                      driver, hooks, 1), 'l1_pending')
        self.assertEqual(driver.rebroadcast, 1)
        self.assertEqual(restarted['transactions']['root']['raw'], '0x1234')
        driver.result = {'blockHash': root}
        with patch.object(gateway, 'finalized_receipt', return_value={'status': '0x1', 'blockHash': root}):
            self.assertEqual(gateway.step(cfg, restarted, lambda value: storage.save(root_path, value, 'root'),
                                          driver, hooks, 2), 'delivered')
        restarted.database.close()
        final = storage.load(root_path, 'root')
        self.addCleanup(final.database.close)
        self.assertNotIn('root', final['transactions'])
        self.assertEqual(final['completed'][storage.completed_key({'batch': 9, 'messageHash': digest})],
                         {'batch': 9, 'messageHash': digest})

    def test_gateway_releases_request_snapshot_before_hook_work(self):
        old = state()
        old['rootRequests'] = {'9': {'batch': 9, 'messageHash': s.ZERO}}
        live = self.legacy(old)
        with patch.object(journal, 'MAX_ROWS', 1):
            storage.save(self.path, live, 'claim')
        live.database.close()
        database = self.path.with_name(self.path.name + '.sqlite3')
        class Hook(GatewayHooks):
            def snapshot(self, batch, message_hash):
                con = sqlite3.connect(database, timeout=.1)
                try:
                    con.execute('BEGIN EXCLUSIVE')
                    con.execute('COMMIT')
                finally:
                    con.close()
                return {'root': h(99), 'imported': h(99), 'authorized': True}
        inbox = Path(self.tmp.name) / 'requests.json'
        storage.publish_descriptor(inbox, 'a' * 64, self.path, old['identity'])
        cfg = {'requests_file': str(inbox), 'requests_identity': '0x' + 'a' * 64, 'zksys_chain_id': 2}
        root_state = {'transactions': {}, 'completed': {}, 'history': [], 'nextRetryAt': 0}
        result = gateway.step(cfg, root_state, lambda _: None, GatewayDriver(root_state), Hook(), 1)
        self.assertEqual(result, 'already_delivered')
        self.assertIn('9:' + s.ZERO, root_state['completed'])

    def test_real_request_collector_persists_proof_and_index_removal(self):
        old = state()
        old['records'] = {str(i): {'report': {'id': str(i), 'fromRoot': True, 'kind': 'accepted'},
                                     'status': 'complete'} for i in range(512)}
        old['records']['child'] = {'report': {'id': 'child', 'fromRoot': False, 'kind': 'activation'},
                                   'status': 'pending'}
        live = self.legacy(old)
        storage.save(self.path, live, 'claim')
        hooks = local.Hooks.__new__(local.Hooks)
        hooks.proof = lambda report: {'blockOrBatchNumber': 9}
        hooks.collect_requests(live['records'], live['rootRequests'], live['rootRequestHashes'], live['rootPending'])
        storage.save(self.path, live, 'claim')
        live.database.close()
        restored = storage.load(self.path, 'claim')
        self.addCleanup(restored.database.close)
        self.assertEqual(restored['records']['child']['rootProof'], {'blockOrBatchNumber': 9})
        self.assertNotIn('child', restored['rootPending'])
        self.assertEqual(restored['rootRequests']['9'], {'batch': 9, 'messageHash': s.ZERO})

    def test_legacy_cached_proof_without_request_is_recovered(self):
        old = state()
        old['records'] = {str(i): {'report': {'id': str(i), 'fromRoot': True, 'kind': 'accepted'},
                                     'status': 'complete'} for i in range(512)}
        old['records']['child'] = {'report': {'id': 'child', 'fromRoot': False, 'kind': 'activation'},
                                   'status': 'complete', 'rootProof': {'blockOrBatchNumber': 9}}
        live = self.legacy(old)
        storage.save(self.path, live, 'claim')
        self.assertIn('child', live['rootPending'])
        hooks = local.Hooks.__new__(local.Hooks)
        def no_refetch(report):
            raise AssertionError('cached proof should be used')
        hooks.proof = no_refetch
        hooks.collect_requests(live['records'], live['rootRequests'], live['rootRequestHashes'], live['rootPending'])
        storage.save(self.path, live, 'claim')
        live.database.close()
        restored = storage.load(self.path, 'claim')
        self.addCleanup(restored.database.close)
        self.assertEqual(restored['rootRequests']['9'], {'batch': 9, 'messageHash': s.ZERO})
        self.assertNotIn('child', restored['rootPending'])

    def test_proof_transport_failure_cannot_drop_earlier_root_request(self):
        old = state()
        old['records'] = {str(i): {'report': {'id': str(i), 'fromRoot': True, 'kind': 'accepted'},
                                     'status': 'complete'} for i in range(512)}
        for name in ('child-a', 'child-b'):
            old['records'][name] = {'report': {'id': name, 'fromRoot': False, 'kind': 'activation'},
                                    'status': 'pending'}
        live = self.legacy(old)
        storage.save(self.path, live, 'claim')
        hooks = local.Hooks.__new__(local.Hooks)
        def fail_second(report):
            if report['id'] == 'child-b':
                raise s.Error('rpc_transport_failure')
            return {'blockOrBatchNumber': 9}
        hooks.proof = fail_second
        def collect():
            return hooks.collect_requests(live['records'], live['rootRequests'],
                                          live['rootRequestHashes'], live['rootPending'])
        with self.assertRaisesRegex(s.Error, 'rpc_transport_failure'):
            collect()
        self.assertNotIn('rootProof', live['records']['child-a'])
        self.assertIn('child-a', live['rootPending'])
        hooks.proof = lambda report: {'blockOrBatchNumber': 9 if report['id'] == 'child-a' else 10}
        collect()
        storage.save(self.path, live, 'claim')
        live.database.close()
        restored = storage.load(self.path, 'claim')
        self.addCleanup(restored.database.close)
        self.assertEqual(restored['rootRequests']['9']['batch'], 9)
        self.assertEqual(restored['rootRequests']['10']['batch'], 10)
        self.assertFalse(restored['rootPending'])

    def test_native_worker_observes_claim_completion_after_indexed_restart(self):
        report = report_values(fixture(), True)
        report['id'] = 'pending'
        old = state()
        old['records'] = {str(i): {'report': {'id': str(i), 'fromRoot': True, 'kind': 'accepted'},
                                     'status': 'complete'} for i in range(512)}
        old['records']['pending'] = {'report': report, 'status': 'complete', 'nextRetryAt': 0, 'failures': 0}
        source = self.legacy(old)
        storage.save(self.path, source, 'claim')
        source.database.close()
        native_path = Path(self.tmp.name) / 'native.json'
        native_state = {'version': 1, 'identity': 'native-id', 'transactions': {}, 'records': {
            str(i): {'report': {'id': str(i), 'fromRoot': True, 'kind': 'accepted'}, 'status': 'complete'}
            for i in range(512)}, 'split': [], 'history': []}
        native_state['records']['pending'] = {'report': report, 'status': 'pending', 'nextRetryAt': 0, 'failures': 0}
        relay.atomic_json(native_path, native_state)
        live = storage.load(native_path, 'native')
        storage.save(native_path, live, 'native')
        worker = native.Runner(live, lambda value: storage.save(native_path, value, 'native'), None, None, None,
                               NativeHooks(), claim_journal=str(self.path), claim_identity=old['identity'])
        worker.discover()
        live.database.close()
        restored = storage.load(native_path, 'native')
        self.addCleanup(restored.database.close)
        self.assertEqual(restored['records']['pending']['status'], 'complete')

    def test_completed_history_can_exceed_old_16_mib_limit(self):
        old = state()
        old['records'] = {str(i): {'report': {'id': str(i), 'fromRoot': True, 'kind': 'accepted'}, 'status': 'complete'} for i in range(512)}
        live = self.legacy(old)
        storage.save(self.path, live, 'claim')
        for i in range(100000):
            live['history'].append({'ids': ['child:0x' + format(i, '064x')],
                                    'status': 'complete', 'txHash': '0x' + format(i, '064x')})
        storage.save(self.path, live, 'claim')
        self.assertGreater(self.path.with_name(self.path.name + '.sqlite3').stat().st_size, relay.MAX_STATE)
        live.database.close()
        restored = storage.load(self.path, 'claim')
        self.addCleanup(restored.database.close)
        self.assertEqual(len(restored['history']), 100000)
        self.assertEqual(restored['history'][99999]['ids'], ['child:0x' + format(99999, '064x')])

    def test_migration_preserves_active_transaction_and_mutations_across_restart(self):
        old = state()
        old['transactions'] = {'claim': {'state': 'submitted', 'raw': '0x1234', 'txHash': '0x' + '42' * 32}}
        old['records'] = {str(i): {'report': {'id': str(i), 'fromRoot': True, 'kind': 'accepted'}, 'status': 'complete'} for i in range(512)}
        old['records']['pending'] = {'report': {'id': 'pending', 'fromRoot': True, 'kind': 'accepted'}, 'status': 'pending'}
        old['history'] = [{'ids': [str(i)], 'status': 'complete'} for i in range(1100)]
        old['rootRequests'] = {'9': {'batch': 9, 'messageHash': '0x' + '33' * 32}}
        old['rootRequestHashes'] = {'0x' + '33' * 32: True}
        live = self.legacy(old)
        storage.save(self.path, live, 'claim')
        self.assertTrue(self.path.with_name(self.path.name + '.sqlite3').is_file())
        live['records']['pending']['status'] = 'complete'
        live['records']['pending']['rootProof'] = {'blockOrBatchNumber': 9}
        live['cursor']['gateway'] = 12
        storage.save(self.path, live, 'claim')
        live.database.close()
        restored = storage.load(self.path, 'claim')
        self.addCleanup(restored.database.close)
        self.assertEqual(restored['records']['pending']['status'], 'complete')
        self.assertEqual(restored['records']['pending']['rootProof'], {'blockOrBatchNumber': 9})
        self.assertEqual(restored['cursor'], {'gateway': 12, 'l1': 11})
        self.assertEqual(restored['transactions'], old['transactions'])
        self.assertEqual(len(restored['history']), 1100)
        self.assertEqual(restored['rootRequests']['9'], old['rootRequests']['9'])

    def test_crash_between_database_commit_and_marker_recovers_exact_source(self):
        old = state()
        old['records'] = {str(i): {'report': {'id': str(i), 'fromRoot': True, 'kind': 'accepted'}, 'status': 'complete'} for i in range(512)}
        live = self.legacy(old)
        real_atomic = journal._atomic
        def interrupted(path, value):
            if value.get('storage_version') == 2:
                raise OSError('simulated marker publication interruption')
            return real_atomic(path, value)
        with patch.object(journal, '_atomic', side_effect=interrupted):
            with self.assertRaisesRegex(OSError, 'simulated marker'):
                storage.save(self.path, live, 'claim')
        if live.database is not None:
            live.database.close()
        restored = storage.load(self.path, 'claim')
        self.addCleanup(restored.database.close)
        self.assertEqual(len(restored['records']), 512)
        self.assertEqual(restored['cursor'], old['cursor'])
        self.assertTrue(restored.marker_pending)
        storage.save(self.path, restored, 'claim')
        self.assertEqual(s.read_json(self.path, private=True)['storage_version'], 2)

    def test_legacy_request_and_root_completed_lists_survive_migration(self):
        old = state()
        old['rootRequests'] = [{'batch': 1, 'messageHash': s.ZERO},
                               {'batch': 2, 'messageHash': '0x' + '22' * 32}]
        old.pop('rootRequestHashes')
        old['records'] = {str(i): {'report': {'id': str(i), 'fromRoot': True, 'kind': 'accepted'},
                                     'status': 'complete'} for i in range(512)}
        old['records']['child'] = {'report': {'id': 'child', 'fromRoot': False, 'kind': 'activation'},
                                   'status': 'pending'}
        claim = self.legacy(old)
        storage.save(self.path, claim, 'claim')
        claim.database.close()
        restored = storage.load(self.path, 'claim')
        self.addCleanup(restored.database.close)
        self.assertEqual(restored['rootRequests']['1'], old['rootRequests'][0])
        self.assertEqual(restored['rootRequests']['2'], old['rootRequests'][1])
        self.assertIn('0x' + '22' * 32, restored['rootRequestHashes'])
        self.assertIn('child', restored['rootPending'])
        self.assertEqual(restored['activations']['child'], old['records']['child']['report'])
        root_path = Path(self.tmp.name) / 'root.json'
        completed = [{'batch': 1, 'messageHash': s.ZERO}, {'batch': 1, 'messageHash': '0x' + '22' * 32}]
        root = {'version': 1, 'identity': 'root', 'transactions': {}, 'nextRetryAt': 0,
                'completed': completed, 'history': []}
        relay.atomic_json(root_path, root)
        with patch.object(journal, 'MAX_ROWS', 1):
            root_live = storage.load(root_path, 'root')
            storage.save(root_path, root_live, 'root')
        root_live.database.close()
        root_restored = storage.load(root_path, 'root')
        self.addCleanup(root_restored.database.close)
        self.assertEqual(len(root_restored['completed']), 2)
        self.assertEqual(root_restored['completed'][storage.completed_key(completed[0])], completed[0])
        self.assertEqual(root_restored['completed'][storage.completed_key(completed[1])], completed[1])

    def test_migration_rejects_altered_legacy_source(self):
        old = state()
        old['records'] = {str(i): {'report': {'id': str(i), 'fromRoot': True, 'kind': 'accepted'}, 'status': 'complete'} for i in range(512)}
        live = self.legacy(old)
        real_atomic = journal._atomic
        def interrupted(path, value):
            if value.get('storage_version') == 2:
                raise OSError('simulated marker publication interruption')
            return real_atomic(path, value)
        with patch.object(journal, '_atomic', side_effect=interrupted):
            with self.assertRaises(OSError):
                storage.save(self.path, live, 'claim')
        if live.database is not None:
            live.database.close()
        altered = copy.deepcopy(old)
        altered['cursor']['gateway'] = 13
        relay.atomic_json(self.path, altered)
        with self.assertRaisesRegex(s.Error, 'journal_migration_source_changed'):
            storage.load(self.path, 'claim')

    def test_version_two_snapshot_holds_one_committed_generation(self):
        old = state()
        old['records'] = {str(i): {'report': {'id': str(i), 'fromRoot': True, 'kind': 'accepted'}, 'status': 'complete'} for i in range(512)}
        old['rootRequests'] = {'9': {'batch': 9, 'messageHash': s.ZERO}}
        live = self.legacy(old)
        storage.save(self.path, live, 'claim')
        live.database.close()
        inbox = Path(self.tmp.name) / 'requests.json'
        storage.publish_descriptor(inbox, 'request-identity', self.path, old['identity'])
        started, finished, errors = threading.Event(), threading.Event(), []
        def update():
            try:
                writer = storage.load(self.path, 'claim')
                try:
                    writer['rootRequests']['9'] = {'batch': 9, 'messageHash': '0x' + '55' * 32}
                    writer['cursor']['gateway'] = 15
                    started.set()
                    storage.save(self.path, writer, 'claim')
                finally:
                    writer.database.close()
            except Exception as error:
                errors.append(error)
            finally:
                finished.set()
        with storage.request_snapshot(inbox, 'request-identity') as (requests, descriptor):
            self.assertEqual(descriptor['claimIdentity'], old['identity'])
            thread = threading.Thread(target=update)
            thread.start()
            self.assertTrue(started.wait(2))
            self.assertFalse(finished.wait(.1), 'snapshot should pin one SQLite generation')
            self.assertEqual(list(requests), [{'batch': 9, 'messageHash': s.ZERO}])
        thread.join(3)
        self.assertFalse(thread.is_alive())
        self.assertEqual(errors, [])
        with storage.request_snapshot(inbox, 'request-identity') as (requests, _):
            self.assertEqual(list(requests), [{'batch': 9, 'messageHash': '0x' + '55' * 32}])

    def test_descriptor_rejects_wrong_identity_and_missing_or_symlink_journal(self):
        old = state()
        self.legacy(old)
        inbox = Path(self.tmp.name) / 'requests.json'
        storage.publish_descriptor(inbox, 'request-identity', self.path, 'wrong-claim-identity')
        with self.assertRaisesRegex(s.Error, 'claim_journal_identity_changed'):
            with storage.request_snapshot(inbox, 'request-identity'):
                pass
        storage.publish_descriptor(inbox, 'request-identity', self.path, old['identity'])
        with self.assertRaisesRegex(s.Error, 'invalid_root_requests'):
            with storage.request_snapshot(inbox, 'wrong-request-identity'):
                pass
        link = Path(self.tmp.name) / 'link.json'
        link.symlink_to(self.path)
        storage.publish_descriptor(inbox, 'request-identity', link, old['identity'])
        with self.assertRaises(OSError):
            with storage.request_snapshot(inbox, 'request-identity'):
                pass
        storage.publish_descriptor(inbox, 'request-identity', Path(self.tmp.name) / 'missing', old['identity'])
        with self.assertRaises(FileNotFoundError):
            with storage.request_snapshot(inbox, 'request-identity'):
                pass

    def test_legacy_request_snapshot_validates_identity(self):
        inbox = Path(self.tmp.name) / 'requests.json'
        relay.atomic_json(inbox, {'version': 1, 'identity': 'good', 'requests': [{'batch': 1, 'messageHash': s.ZERO}]})
        with storage.request_snapshot(inbox, 'good') as (requests, descriptor):
            self.assertEqual(list(requests), [{'batch': 1, 'messageHash': s.ZERO}])
            self.assertIsNone(descriptor)
        with self.assertRaisesRegex(s.Error, 'invalid_root_requests'):
            with storage.request_snapshot(inbox, 'other'):
                pass


if __name__ == '__main__':
    unittest.main()
