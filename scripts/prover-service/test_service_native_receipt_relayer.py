import json
import subprocess
from pathlib import Path
import tempfile
import unittest
from unittest import mock

import service as s
import service_native_receipt_relayer as n
import service_receipt_relay as receipts
from test_service_receipt_relay import report_values, tuple_text
from test_service import fixture, a


class FakeHooks:
    cfg = {'gateway_chain_id': 3, 'gateway_genesis': 'x', 'l1_chain_id': 1, 'l1_genesis': 'y'}
    gateway, l1 = 'gateway', 'l1'
    def anchor(self, rpc, *args): return {'number': '0x9'}, {}
    def validate_report(self, report): pass


class Tests(unittest.TestCase):
    def test_activation_calldata(self):
        report = {'fromRoot': True, 'kind': 'activation', 'vk_hash': '0x'+'11'*32,
                  'roster_root': '0x'+'22'*32}
        cfg = {'priority_gas_limit': 100, 'gas_per_pubdata': 10, 'refund': '0x'+'33'*20}
        encoded, child, digest = n.encode(report, cfg)
        self.assertEqual(encoded[:10], '0x' + n.relay.selector('relayActivation(bytes32,bytes32,uint256,uint256,address)').hex())
        self.assertEqual(child[:10], '0x' + n.relay.selector('receiveRootActivation(bytes32,bytes32)').hex())
        self.assertEqual(digest, s.keccak(receipts.report_message(report)))

    def test_accepted_calldata_matches_cast(self):
        report = report_values(fixture(), True)
        cfg = {'priority_gas_limit': 12000000, 'gas_per_pubdata': 800, 'refund': a(5)}
        encoded, child, _ = n.encode(report, cfg)
        package = n.relay.abi_tuple(s.PACKAGE)
        duty = n.relay.abi_tuple(s.DUTY + [('operatorSignature', 'bytes')])
        args = [tuple_text(s.PACKAGE, report['accepted']),
                '[' + ','.join(tuple_text(s.DUTY + [('operatorSignature', 'bytes')], d) for d in report['duties']) + ']',
                'false']
        signature = 'relayAccepted(' + package + ',' + duty + '[],bool,uint256,uint256,address)'
        expected = subprocess.check_output(['cast', 'calldata', signature, *args, '12000000', '800', a(5)], text=True).strip().lower()
        self.assertEqual(encoded, expected)
        signature = 'receiveRootAccepted(' + package + ',' + duty + '[],bool)'
        expected = subprocess.check_output(['cast', 'calldata', signature, *args], text=True).strip().lower()
        self.assertEqual(child, expected)

    def test_waits_for_both_lanes_bootstrap_and_source_cursors(self):
        root = {'id':'root:r', 'fromRoot':True, 'kind':'activation', 'bootstrap':False,
                'vk_hash':'0x'+'11'*32, 'roster_root':'0x'+'22'*32, 'sourceBlockNumber':'0x9'}
        child = dict(root, id='child:r', fromRoot=False, vk_hash='0x'+'44'*32)
        boot = {'id':'child:b', 'fromRoot':False, 'kind':'unresolved', 'bootstrap':True}
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp)/'claim.json'
            journal = {'transactions': {}, 'records': {}, 'split': [], 'history': []}
            worker = n.Runner(journal, lambda _: None, None, None, None, FakeHooks(), claim_journal=str(path), claim_identity='id')
            source = {'version':1, 'identity':'id', 'cursor':{'gateway':10,'l1':10},
                      'records':{'root:r':{'status':'pending','report':root}}}
            def run():
                path.write_text(json.dumps(source)); path.chmod(0o600); worker.discover()
            run(); self.assertEqual(journal['records'], {})
            source['records']['child:r'] = {'status':'pending','report':child}
            source['records']['child:b'] = {'status':'pending','report':boot}
            run(); self.assertEqual(journal['records'], {})
            source['records']['child:b']['status'] = 'complete'
            source['cursor']['gateway'] = 9
            run(); self.assertEqual(journal['records'], {})
            source['cursor']['gateway'] = 10
            run(); self.assertEqual(worker.select(), ['root:r'])
            source['records']['child:b']['status'] = 'pending'
            run(); self.assertEqual(journal['records'], {})

    def test_bundled_hash_gets_grace_then_falls_back(self):
        report = report_values(fixture(), True)
        record = {'status':'pending', 'nextRetryAt':0, 'firstSeen':10, 'report':report}
        journal = {'transactions':{}, 'records':{'one':record}, 'split':[], 'history':[]}
        now = [11]
        worker = n.Runner(journal, lambda _:None, None, None, None, FakeHooks(), claim_journal='/tmp/unused',
                          claim_identity='id', fallback_delay_seconds=60, clock=lambda:now[0])
        worker.bundled.add(s.keccak(receipts.report_message(report)))
        self.assertEqual(worker.select(), [])
        now[0] = 70
        self.assertEqual(worker.select(), ['one'])

    def test_prioritizes_bootstrap_then_activation(self):
        records = {key: {'status':'pending','nextRetryAt':0,'report':{'kind':kind,'bootstrap':bootstrap}}
                   for key,kind,bootstrap in [('a','accepted',False),('b','activation',False),('c','accepted',True)]}
        journal={'transactions':{},'records':records,'split':[],'history':[]}
        worker=n.Runner(journal, lambda _:None, None, None, None, FakeHooks(), claim_journal='/tmp/unused', claim_identity='id')
        self.assertEqual(worker.select(), ['c'])
        records['c']['status']='complete'
        self.assertEqual(worker.select(), ['b'])


if __name__ == '__main__': unittest.main()
