import copy
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import publish_initial_roster as publisher
import relay as r
import roster
import service as s
from test_service import a, h


class RosterRpc:
    def __init__(self, artifact, cfg):
        self.artifact, self.cfg = artifact, cfg
        self.progress, self.root, self.bad_candidate, self.reorg, self.cutoff = 0, s.ZERO, False, False, 900
        self.head = {'number': '0x14', 'hash': h(19), 'timestamp': '0x3e8'}
        self.genesis = {'number': '0x0', 'hash': cfg['genesis_hash'], 'timestamp': '0x1'}
        self.sent, self.receipt = [], None

    def call(self, method, params):
        if method == 'eth_chainId': return hex(self.cfg['chain_id'])
        if method == 'eth_getBlockByNumber':
            if params[0] == '0x0': return self.genesis
            result = dict(self.head)
            if self.reorg and params[0] not in ('latest', 'finalized'): result['hash'] = h(99)
            return result
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
        if selector == r.selector('firstServicePeriod()'): return '0x' + s.word(0).hex()
        if selector == r.selector('qualifiedWrapperCount(uint64)'): return '0x' + s.word(self.artifact['count']).hex()
        if selector == r.selector('rosterCutoff(uint64)'): return '0x' + s.word(self.cutoff).hex()
        if selector == r.selector('publishedRosterRoot(uint64)'): return self.root
        if selector == r.selector('publicationProgress(address,uint64)'):
            last = s.ZERO_ADDRESS if self.progress == 0 else self.artifact['candidates'][self.progress - 1]['account']
            return '0x' + (s.word(self.progress) + bytes(12) + s.raw_hex(last, 20)).hex()
        if selector == r.selector('qualifiedWrapper(address,uint64)'):
            account = '0x' + data[16:36].hex()
            candidate = next(c for c in self.artifact['candidates'] if c['account'] == account)
            values = [candidate[x] for x in ('account', 'operator', 'beneficiary')]
            if self.bad_candidate: values[1] = a(200)
            return '0x' + (s.word(0) + b''.join(bytes(12) + s.raw_hex(v, 20) for v in values)).hex()
        if selector == r.selector('publishRosterPage(uint64,(uint32,address,address,address)[])'):
            return self.artifact['root']
        raise AssertionError(tx)


class FakeSigner:
    saved = {}
    def __init__(self, *_): pass
    def _run(self, _): return a(90)
    def sign(self, tx):
        raw = '0x' + b'signed-once'.hex()
        self.saved[raw] = copy.deepcopy(tx)
        return raw
    def decode(self, raw): return self.saved[raw]
    def hash_raw(self, _): return h(70)


class PublishInitialRosterTests(unittest.TestCase):
    def setUp(self):
        self.artifact = roster.construct(0, [
            {'index': 0, 'account': a(1), 'operator': a(2), 'beneficiary': a(3)},
            {'index': 1, 'account': a(4), 'operator': a(5), 'beneficiary': a(6)}])
        self.cfg = {'chain_id': 123, 'genesis_hash': h(7), 'registry': a(8),
                    'registry_code_hash': s.keccak(b'registry-code'), 'publisher': a(90),
                    'rpc_url': 'http://127.0.0.1:8545', 'max_head_age_seconds': 60,
                    'rpc_timeout_seconds': 15, 'gas_limit': 1000000, 'gas_price': 1,
                    'keystore': '/private/test-key', 'password_file': '/private/test-password'}
        self.rpc = RosterRpc(self.artifact, self.cfg)

    def test_plan_checks_candidate_and_simulates_exact_page(self):
        plan = publisher.plan_page(self.rpc, self.cfg, self.artifact, 1000)
        self.assertEqual((plan['status'], plan['start'], plan['count']), ('ready', 0, 2))
        self.assertEqual(plan['transaction']['from'], self.cfg['publisher'])
        self.assertTrue(plan['transaction']['data'].startswith('0x' + r.selector('publishRosterPage(uint64,(uint32,address,address,address)[])').hex()))

    def test_rejects_changed_candidates_cutoff_or_anchor(self):
        for field, value in [('bad_candidate', True), ('cutoff', 1001), ('reorg', True)]:
            with self.subTest(field=field):
                old = getattr(self.rpc, field); setattr(self.rpc, field, value)
                with self.assertRaises(s.Error): publisher.plan_page(self.rpc, self.cfg, self.artifact, 1000)
                setattr(self.rpc, field, old)

    def test_restart_rebroadcasts_same_signed_bytes(self):
        with tempfile.TemporaryDirectory() as path, patch.object(publisher, 'CastSigner', FakeSigner), \
                patch.object(publisher, 'private_file'):
            state = str(Path(path).resolve())
            first = publisher.run_send(self.rpc, self.cfg, self.artifact, state, 1000)
            self.assertEqual(first['status'], 'submitted')
            second = publisher.run_send(self.rpc, self.cfg, self.artifact, state, 1000)
            self.assertEqual(second['status'], 'pending')
            self.assertEqual(self.rpc.sent, [self.rpc.sent[0], self.rpc.sent[0]])
            journal = json.loads((Path(state) / 'roster-publish.json').read_text())
            self.assertEqual(journal['transactions']['roster']['raw'], self.rpc.sent[0])

    def test_published_root_is_idempotent(self):
        self.rpc.root = self.artifact['root']
        self.assertEqual(publisher.plan_page(self.rpc, self.cfg, self.artifact, 1000)['status'], 'published')

    def test_finalized_page_requires_state_before_retiring_journal(self):
        with tempfile.TemporaryDirectory() as path, patch.object(publisher, 'CastSigner', FakeSigner), \
                patch.object(publisher, 'private_file'):
            state = str(Path(path).resolve())
            publisher.run_send(self.rpc, self.cfg, self.artifact, state, 1000)
            self.rpc.receipt = {'transactionHash': h(70), 'blockNumber': '0x14', 'blockHash': self.rpc.head['hash'], 'status': '0x1'}
            with self.assertRaisesRegex(s.Error, 'finalized_page_not_reflected'):
                publisher.run_send(self.rpc, self.cfg, self.artifact, state, 1000)
            self.rpc.root = self.artifact['root']
            result = publisher.run_send(self.rpc, self.cfg, self.artifact, state, 1000)
            self.assertEqual(result['status'], 'published')
            journal = json.loads((Path(state) / 'roster-publish.json').read_text())
            self.assertEqual(journal['transactions'], {})


if __name__ == '__main__':
    unittest.main()
