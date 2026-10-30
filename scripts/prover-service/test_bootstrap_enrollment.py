import copy
import json
from pathlib import Path
import shutil
import socket
import subprocess
import tempfile
import time
import unittest
import urllib.request
from unittest.mock import patch

import bootstrap_enrollment as enrollment
import relay as r
import service as s
from service_transaction_driver import ServiceTransactionDriver
from test_service import fixture, h, sign


class EnrollmentRpc:
    def __init__(self, cfg, signed, prepared):
        self.cfg, self.signed, self.prepared = cfg, signed, prepared
        self.block = {'number': '0x14', 'hash': h(19), 'timestamp': '0x3e8'}
        self.genesis = {'number': '0x0', 'hash': cfg['genesis_hash'], 'timestamp': '0x1'}
        self.stored, self.conflict, self.reorg, self.closed, self.missing_senior = False, False, False, False, False
        self.sent, self.receipt = [], None

    def call(self, method, params):
        if method == 'eth_chainId': return self.cfg['settings']['registry_chain_id']
        if method == 'eth_getBlockByNumber':
            if params[0] == '0x0': return self.genesis
            block = dict(self.block)
            if self.reorg and params[0] not in ('latest', 'finalized'): block['hash'] = h(99)
            return block
        if method == 'eth_getCode': return '0x' + b'registry-code'.hex()
        if method == 'eth_getTransactionCount': return '0x0'
        if method == 'eth_getTransactionReceipt': return self.receipt
        if method == 'eth_getTransactionByHash': return None
        if method == 'eth_sendRawTransaction':
            self.sent.append(params[0]); return h(70)
        if method != 'eth_call': raise AssertionError(method)
        tx, anchor = params
        assert anchor.get('requireCanonical') is True
        data = s.raw_hex(tx['data'])
        selector = data[:4]
        settings, sub = self.cfg['settings'], self.signed['subscription']
        if selector == r.selector('policyHash()'): return settings['policy_hash']
        if selector == r.selector('sharedSequencer()'): return '0x' + (bytes(12) + s.raw_hex(settings['sequencer'])).hex()
        if selector == r.selector('dutiesPerRound()'): return '0x' + s.word(settings['duties_per_round']).hex()
        if selector == r.selector('firstServicePeriod()'): return '0x' + s.word(0).hex()
        if selector == r.selector('subscriptionAt(address,address,uint64)'):
            return h(77) if self.conflict else self.prepared['subscription_hash'] if self.stored else s.ZERO
        if selector == r.selector('operatorAccountAt(address,uint64)'):
            return '0x' + (bytes(12) + s.raw_hex(sub['account'])).hex()
        if selector == r.selector('subscription(bytes32)'): return '0x' + s.encode_fields(s.SUBSCRIPTION, sub).hex()
        if selector == r.selector('seniorBonus(address)'): return '0x' + s.word(0 if self.missing_senior else 35000).hex()
        if selector == r.selector('serviceActive()'): return '0x' + s.word(int(self.closed)).hex()
        if selector == r.selector('nextAdmissionPeriod()'): return '0x' + s.word(int(self.closed)).hex()
        if tx['data'] == self.prepared['transaction']['data']: return self.prepared['subscription_hash']
        raise AssertionError(tx)


class FakeSigner:
    saved = {}
    account = None
    def __init__(self, *_): pass
    def _run(self, _): return self.account
    def sign(self, tx):
        raw = '0x' + b'signed-enrollment-once'.hex()
        self.saved[raw] = copy.deepcopy(tx)
        return raw
    def decode(self, raw): return self.saved[raw]
    def hash_raw(self, _): return h(70)


class BootstrapEnrollmentTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        fixture_data = fixture()
        settings = fixture_data['settings']
        settings['coordinator'] = s.ZERO_ADDRESS
        sub = copy.deepcopy(fixture_data['subscription'])
        sub.update(firstPeriod=0, lastPeriod=1, nonce=0)
        account_request = s.subscription_request(settings, sub)
        signed = {'subscription': sub, 'signature': sign(account_request, 'account')}
        operator_request = s.operator_subscription_request(settings, signed)
        signature = sign(operator_request, 'operator')
        prepared = s.prepare_enrollment(settings, signed, signature)
        cls.cfg = {'settings': settings, 'rpc_url': 'http://127.0.0.1:8545',
                   'genesis_hash': h(7), 'registry_code_hash': s.keccak(b'registry-code'),
                   'max_head_age_seconds': 60, 'rpc_timeout_seconds': 15, 'gas_limit': 1000000,
                   'gas_price': 1, 'keystore': '/private/test-key', 'password_file': '/private/test-password'}
        cls.signed, cls.signature, cls.prepared = signed, signature, prepared
        FakeSigner.account = sub['account']

    def setUp(self):
        self.rpc = EnrollmentRpc(self.cfg, self.signed, self.prepared)

    def test_requires_finalized_senior_membership_and_exact_simulation(self):
        plan = enrollment.prepare(self.rpc, self.cfg, self.signed, self.signature, 1000)
        self.assertEqual(plan['status'], 'ready')
        self.assertEqual(plan['transaction'], self.prepared['transaction'])
        self.rpc.missing_senior = True
        with self.assertRaisesRegex(s.Error, 'finalized_senior_membership_required'):
            enrollment.prepare(self.rpc, self.cfg, self.signed, self.signature, 1000)

    def test_rejects_closed_window_changed_registry_or_reorg(self):
        for field in ('closed', 'conflict', 'reorg'):
            with self.subTest(field=field):
                setattr(self.rpc, field, True)
                with self.assertRaises(s.Error):
                    enrollment.prepare(self.rpc, self.cfg, self.signed, self.signature, 1000)
                setattr(self.rpc, field, False)

    def test_existing_finalized_exact_subscription_is_idempotent(self):
        self.rpc.stored = True
        result = enrollment.prepare(self.rpc, self.cfg, self.signed, self.signature, 1000)
        self.assertEqual(result['status'], 'enrolled')
        self.assertEqual(self.rpc.sent, [])

    def test_restart_rebroadcasts_original_bytes_and_preserves_unfinalized_entry(self):
        with tempfile.TemporaryDirectory() as path, patch.object(enrollment, 'CastSigner', FakeSigner), \
                patch.object(enrollment, 'private_file'):
            state = str(Path(path).resolve())
            first = enrollment.run_send(self.rpc, self.cfg, self.signed, self.signature, state, 1000)
            self.assertEqual(first['status'], 'submitted')
            second = enrollment.run_send(self.rpc, self.cfg, self.signed, self.signature, state, 1000)
            self.assertEqual(second['status'], 'pending')
            self.assertEqual(self.rpc.sent, [self.rpc.sent[0], self.rpc.sent[0]])
            journal = json.loads((Path(state) / 'enrollment.json').read_text())
            self.assertEqual(journal['transactions']['enrollment']['kind'], 'enrollment')
            self.assertEqual(journal['transactions']['enrollment']['raw'], self.rpc.sent[0])

    def test_finalized_receipt_requires_exact_onchain_subscription(self):
        with tempfile.TemporaryDirectory() as path, patch.object(enrollment, 'CastSigner', FakeSigner), \
                patch.object(enrollment, 'private_file'):
            state = str(Path(path).resolve())
            enrollment.run_send(self.rpc, self.cfg, self.signed, self.signature, state, 1000)
            self.rpc.receipt = {'transactionHash': h(70), 'blockNumber': '0x14', 'blockHash': self.rpc.block['hash'], 'status': '0x1'}
            with self.assertRaisesRegex(s.Error, 'finalized_enrollment_not_reflected'):
                enrollment.run_send(self.rpc, self.cfg, self.signed, self.signature, state, 1000)
            self.rpc.stored = True
            self.assertEqual(enrollment.run_send(self.rpc, self.cfg, self.signed, self.signature, state, 1000)['status'], 'enrolled')
            journal = json.loads((Path(state) / 'enrollment.json').read_text())
            self.assertEqual(journal['transactions'], {})

    def test_changed_signed_intent_cannot_reuse_existing_nonce(self):
        with tempfile.TemporaryDirectory() as path, patch.object(enrollment, 'CastSigner', FakeSigner), \
                patch.object(enrollment, 'private_file'):
            state = str(Path(path).resolve())
            enrollment.run_send(self.rpc, self.cfg, self.signed, self.signature, state, 1000)
            changed = copy.deepcopy(self.cfg); changed['gas_price'] += 1
            with self.assertRaisesRegex(s.Error, 'enrollment_journal_changed'):
                enrollment.run_send(self.rpc, changed, self.signed, self.signature, state, 1000)
            self.assertEqual(len(self.rpc.sent), 1)

    def test_recovery_rejects_journal_hash_not_bound_to_signed_bytes(self):
        with tempfile.TemporaryDirectory() as path, patch.object(enrollment, 'CastSigner', FakeSigner), \
                patch.object(enrollment, 'private_file'):
            state = Path(path).resolve()
            enrollment.run_send(self.rpc, self.cfg, self.signed, self.signature, str(state), 1000)
            journal_path = state / 'enrollment.json'
            journal = json.loads(journal_path.read_text())
            journal['transactions']['enrollment']['txHash'] = h(71)
            journal_path.write_text(json.dumps(journal))
            with self.assertRaisesRegex(s.Error, 'enrollment_journal_transaction_changed'):
                enrollment.run_send(self.rpc, self.cfg, self.signed, self.signature, str(state), 1000)
            self.assertEqual(len(self.rpc.sent), 1)

    def test_rejects_operator_consent_from_another_key(self):
        with self.assertRaises(s.Error):
            enrollment.prepare(self.rpc, self.cfg, self.signed, self.signed['signature'], 1000)
        self.assertEqual(self.rpc.sent, [])

    def test_service_driver_rejects_unsupported_kind(self):
        driver = ServiceTransactionDriver({}, lambda _: None, lambda _: '0x1234', lambda _: {}, lambda _: h(1))
        with self.assertRaisesRegex(Exception, 'invalid service transaction kind'):
            driver.begin('x', self.rpc.call, 1, self.signed['subscription']['account'],
                         self.cfg['settings']['registry'], '0x', 0, 1, 1, kind='bridge')


@unittest.skipUnless(shutil.which('anvil') and shutil.which('cast'), 'Foundry tools are required')
class ServiceTransactionAnvilTest(unittest.TestCase):
    def test_signed_transaction_is_rebroadcast_unchanged_after_eviction(self):
        key = '0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80'
        sender = '0xf39fd6e51aad88f6f4ce6ab8827279cfffb92266'
        with socket.socket() as probe:
            probe.bind(('127.0.0.1', 0))
            port = probe.getsockname()[1]
        process = subprocess.Popen(['anvil', '--host', '127.0.0.1', '--port', str(port), '--no-mining', '--silent'],
                                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        self.addCleanup(lambda: (process.terminate(), process.wait(timeout=5)))
        url = f'http://127.0.0.1:{port}'
        def rpc(method, params):
            request = urllib.request.Request(url, json.dumps({'jsonrpc': '2.0', 'id': 1,
                                                              'method': method, 'params': params}).encode(),
                                             {'Content-Type': 'application/json'})
            with urllib.request.urlopen(request, timeout=3) as response:
                result = json.load(response)
            if 'error' in result:
                raise RuntimeError(result['error'])
            return result['result']
        for attempt in range(50):
            try:
                rpc('eth_chainId', [])
                break
            except OSError:
                if process.poll() is not None or attempt == 49:
                    self.fail('Anvil did not start')
                time.sleep(0.1)
        def sign_tx(tx):
            return subprocess.check_output(['cast', 'mktx', tx['to'], tx['data'], '--chain', str(tx['chainId']),
                '--nonce', str(tx['nonce']), '--gas-limit', str(tx['gas']), '--gas-price', str(tx['gasPrice']),
                '--value', str(tx['value']), '--legacy', '--private-key', key], text=True).strip()
        signer = enrollment.CastSigner(url, '/unused', '/unused')
        journal, saved = {}, []
        driver = ServiceTransactionDriver(journal, lambda state: saved.append(copy.deepcopy(state)),
                                          sign_tx, signer.decode, signer.hash_raw)
        digest = driver.begin('enrollment', rpc, 31337, sender, '0x' + '22' * 20,
                              '0x', 0, 21000, 2_000_000_000, kind='enrollment')
        raw = journal['transactions']['enrollment']['raw']
        self.assertEqual(saved[0]['transactions']['enrollment']['state'], 'signed')
        self.assertEqual(saved[0]['transactions']['enrollment']['raw'], raw)
        rpc('anvil_dropTransaction', [digest])
        self.assertTrue(driver.rebroadcast_if_missing('enrollment', rpc))
        self.assertEqual(journal['transactions']['enrollment']['raw'], raw)
        self.assertEqual(rpc('eth_getTransactionByHash', [digest])['hash'], digest)


if __name__ == '__main__':
    unittest.main()
