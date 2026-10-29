import sys
from pathlib import Path
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'prover-service'))
import prepare_message_relay as prep


def h(i):
    return '0x' + format(i, '064x')


def a(i):
    return '0x' + format(i, '040x')


class Gateway:
    def __init__(self, data):
        self.data = data
        self.proof = {'batchNumber': 12, 'id': 4, 'proof': [h(3)], 'root': h(4)}
    def call(self, method, args):
        if method == 'eth_chainId': return hex(57001)
        if method == 'eth_getTransactionReceipt':
            return {'transactionHash': h(10), 'status': '0x1', 'blockHash': h(11), 'blockNumber': '0xc',
                    'transactionIndex': '0x2', 'l2ToL1Logs': [{'sender': prep.MESSENGER,
                    'key': '0x' + '00' * 12 + a(22)[2:], 'value': prep.s.keccak(self.data),
                    'shardId': '0x0', 'isService': True, 'transactionIndex': '0x2',
                    'transactionLogIndex': '0x0'}]}
        if method == 'eth_getBlockByNumber': return {'hash': h(1) if args[0] == '0x0' else h(11), 'number': '0xc'}
        if method == 'zks_getL2ToL1LogProof': return self.proof
        raise AssertionError(method)


class Child:
    def __init__(self):
        self.verdict = h(1)
        self.calldata = None
    def call(self, method, args):
        if method == 'eth_chainId': return hex(57057)
        if method == 'eth_getBlockByNumber': return {'hash': h(2) if args[0] == '0x0' else h(15), 'number': '0xf'}
        if method == 'eth_getCode': return '0x6000'
        if method == 'eth_call':
            tx = args[0]
            if tx['to'] == prep.VERIFIER: return self.verdict
            if tx['data'] == '0x' + prep.relay.selector('gatewayGate()').hex(): return h(22)
            if tx['data'] == '0x' + prep.relay.selector('gatewayChainId()').hex(): return h(57001)
            self.calldata = tx['data']
            return '0x'
        raise AssertionError(method)


class PreparationTest(unittest.TestCase):
    def setUp(self):
        self.request = {'kind': 'activation', 'vk_hash': h(2), 'roster_root': h(3)}
        self.data = prep.message_and_parts(self.request)[0]
        self.source = Gateway(self.data)
        self.destination = Child()
        self.cfg = {'gateway_chain_id': 57001, 'child_chain_id': 57057,
                    'gateway_genesis_hash': h(1), 'child_genesis_hash': h(2), 'gateway_gate': a(22),
                    'receiver': a(33), 'receiver_code_hash': prep.common.digest('0x6000'), 'sender': a(44),
                    'transaction_hash': h(10), 'transaction_log_index': 0, 'proof_target': 'l1BatchRoot'}

    def test_verifies_and_prepares_exact_receiver_call(self):
        result = prep.prepare(self.cfg, self.source, self.destination, self.request)
        expected = prep.common.cast('calldata', 'relayActivation(bytes32,bytes32,(uint256,uint256,uint16,bytes32[]))',
                                    h(2), h(3), f'(12,4,2,[{h(3)}])')
        self.assertEqual(result['data'], expected)
        self.assertEqual(result['status'], 'verified_at_block')
        self.assertEqual(self.destination.calldata, expected)

    def test_accepted_call_encodes_actual_solidity_abi(self):
        accepted = {}
        for field, kind in prep.s.PACKAGE:
            if kind == 'address': accepted[field] = a(8)
            elif kind == 'bytes32': accepted[field] = h(8)
            elif kind == 'uint256': accepted[field] = '0xde'
            else: accepted[field] = 1
        duty = {}
        for field, kind in prep.s.DUTY:
            if kind == 'address': duty[field] = a(9)
            elif kind == 'bytes32': duty[field] = h(9)
            else: duty[field] = 1
        duty['operatorSignature'] = '0x' + 'aa' * 65
        accepted['reportHash'] = prep.s.report_hash([duty])
        request = {'kind': 'accepted', 'accepted_package': accepted, 'duties': [duty], 'bootstrap': True}
        data, _, signature = prep.message_and_parts(request)
        self.source.data = data
        actual = prep.prepare(self.cfg, self.source, self.destination, request)['data']
        package = '(' + ','.join(str(accepted[key]) for key, _ in prep.s.PACKAGE) + ')'
        duty_tuple = '(' + ','.join(str(duty[key]) for key, _ in prep.s.DUTY) + ',' + duty['operatorSignature'] + ')'
        expected = prep.common.cast('calldata', signature, package, '[' + duty_tuple + ']', 'true', f'(12,4,2,[{h(3)}])')
        self.assertEqual(actual, expected)

    def test_rejects_unavailable_or_unverified_proofs(self):
        self.source.proof = None
        with self.assertRaisesRegex(prep.s.Error, 'proof_unavailable'):
            prep.prepare(self.cfg, self.source, self.destination, self.request)
        self.source.proof = {'batchNumber': 12, 'id': 4, 'proof': [h(3)]}
        self.destination.verdict = h(0)
        with self.assertRaisesRegex(prep.s.Error, 'message_proof_not_verified'):
            prep.prepare(self.cfg, self.source, self.destination, self.request)
        self.assertIsNone(self.destination.calldata)

    def test_rejects_wrong_message(self):
        self.source.data = b'other'
        with self.assertRaisesRegex(prep.s.Error, 'message_log_mismatch'):
            prep.prepare(self.cfg, self.source, self.destination, self.request)


if __name__ == '__main__':
    unittest.main()
