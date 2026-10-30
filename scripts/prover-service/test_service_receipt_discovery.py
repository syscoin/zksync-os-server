import copy
import json
from pathlib import Path
import tempfile
import unittest

import relay
import service as s
import service_receipt_discovery as d


def h(n): return '0x' + n.to_bytes(32, 'big').hex()
def a(n): return '0x' + n.to_bytes(20, 'big').hex()


class Rpc:
    def __init__(self, chain, head, gate, event, receipt, tx=None, vk=None):
        self.chain, self.head, self.gate, self.event, self.receipt, self.tx, self.vk = chain, head, gate, event, receipt, tx, vk
        self.reorg = False
        self.truncate = False
        self.no_block_receipts = False
        self.calls = []

    def call(self, method, params):
        self.calls.append((method, params))
        if method == 'eth_chainId': return hex(self.chain)
        if method == 'eth_getBlockByNumber':
            if self.reorg and params[0] != 'finalized':
                return {**self.head, 'hash': h(999), 'transactions': [self.receipt['transactionHash']] if self.receipt else []}
            return {**copy.deepcopy(self.head), 'transactions': [self.receipt['transactionHash']] if self.receipt else []}
        if method == 'eth_getLogs':
            query = params[0]
            assert query['address'] == self.gate
            assert query['topics'] == [[d.ACCEPTED, d.ACTIVATED]]
            return ([copy.deepcopy(self.event)] if self.event is not None
                    and self.event['blockHash'] == query['blockHash'] else [])
        if method == 'eth_getTransactionReceipt': return copy.deepcopy(self.receipt)
        if method == 'eth_getTransactionByHash': return copy.deepcopy(self.tx)
        if method == 'eth_call': return self.vk
        raise AssertionError(method)


class DiscoveryTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.vector = json.loads((Path(__file__).parent / 'vector.json').read_text())

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name).resolve()
        self.head = {'number': '0x7', 'hash': h(7)}
        self.gate, self.root_gate, self.sink = a(101), a(102), a(103)
        self.item = copy.deepcopy(self.vector['sidecar'])
        self.accepted = self.item['accepted_package']
        self.duties = self.item['duties']
        self.package = s.struct_hash('AcceptedPackageV1', s.PACKAGE, self.accepted)
        self.cfg = {'gateway_gate': self.gate, 'root_gate': self.root_gate, 'root_sink': self.sink,
                    'gateway_chain_id': 99, 'l1_chain_id': 57,
                    'zksys_chain_id': s.uint(self.accepted['chainId']),
                    'gateway_chain_address': a(104), 'child_chain_address': self.accepted['chainAddress'],
                    'scan_blocks': 1000}
        self.roots = {'child': str(self.root), 'root': str(self.root)}
        self.empty_l1 = Rpc(57, self.head, self.root_gate, None, None)

    def tearDown(self): self.temp.cleanup()

    def accepted_receipt(self, control=False, mode='service', alter_message=False):
        bootstrap = mode == 'bootstrap'
        payload = s.raw_hex(s.keccak(b'ZKSYS_ACCEPTED_SERVICE_V1')) + s.raw_hex(self.package) + s.word(int(bootstrap))
        event = {'address': self.gate, 'topics': [d.ACCEPTED, self.package,
            '0x' + s.word(self.accepted['batchFrom']).hex(), '0x' + s.word(self.accepted['batchTo']).hex()],
            'data': '0x' + s.word(int(bootstrap)).hex(), 'transactionHash': h(22), 'blockHash': h(7),
            'blockNumber': '0x7', 'transactionIndex': '0x0', 'logIndex': '0x0', 'removed': False}
        logs = [event]
        messages = []
        if control:
            logs.append({'address': self.gate, 'topics': [d.CONTROL, self.package], 'data': '0x', 'removed': False})
        else:
            messages.append({'sender': d.MESSENGER, 'key': '0x' + '00' * 12 + self.gate[2:],
                'value': h(333) if alter_message else s.keccak(payload), 'transactionLogIndex': '0x0',
                'transactionIndex': '0x0', 'shardId': '0x0', 'isService': True})
        receipt = {'transactionHash': h(22), 'blockHash': h(7), 'blockNumber': '0x7',
                   'transactionIndex': '0x0', 'status': '0x1', 'logs': logs, 'l2ToL1Logs': messages}
        tx = {'hash': h(22), 'to': self.gate, 'blockHash': h(7), 'blockNumber': '0x7',
              'input': relay.gate_calldata(self.item, self.vector['proof_data'], mode)}
        return event, receipt, tx

    def test_extracts_accepted_duties_from_canonical_gate_calldata(self):
        event, receipt, tx = self.accepted_receipt()
        gateway = Rpc(99, self.head, self.gate, event, receipt, tx)
        cursor, reports = d.discover(gateway, self.empty_l1, self.cfg, {'gateway': 7, 'l1': 8}, self.roots)
        self.assertEqual(cursor, {'gateway': 8, 'l1': 8})
        self.assertEqual(len(reports), 1)
        self.assertEqual(reports[0]['accepted'], self.accepted)
        self.assertEqual(reports[0]['duties'], self.duties)
        self.assertEqual(reports[0]['transactionLogIndex'], 0)

    def test_bootstrap_submission_decodes(self):
        encoded = relay.gate_calldata(self.item, self.vector['proof_data'], 'bootstrap')
        accepted, duties, bootstrap = d.decode_gate_calldata(encoded)
        self.assertEqual((accepted, duties, bootstrap), (self.accepted, self.duties, True))

    def test_missing_artifact_is_durably_unresolved_then_hydrates(self):
        event, receipt, tx = self.accepted_receipt()
        tx['to'] = a(999)
        gateway = Rpc(99, self.head, self.gate, event, receipt, tx)
        cursor, reports = d.discover(gateway, self.empty_l1, self.cfg, {'gateway': 7, 'l1': 8}, self.roots)
        self.assertEqual(cursor['gateway'], 8)
        self.assertEqual(reports[0]['kind'], 'unresolved')
        durable = json.loads(json.dumps(reports[0]))
        self.assertIsNone(d.hydrate(gateway, self.cfg, durable, str(self.root)))
        item = {'package_hash': self.package, 'mode': 'service', 'sidecar': self.item}
        s.write_new(self.root / (self.package[2:] + '.json'), item)
        recovered = d.hydrate(gateway, self.cfg, durable, str(self.root))
        self.assertEqual(recovered['kind'], 'accepted')
        self.assertEqual(recovered['accepted'], self.accepted)
        self.assertEqual(recovered['duties'], self.duties)
        self.assertEqual(recovered['id'], durable['id'])

    def test_unresolved_tampering_and_reorg_fail_closed(self):
        event, receipt, tx = self.accepted_receipt()
        tx['to'] = a(999)
        gateway = Rpc(99, self.head, self.gate, event, receipt, tx)
        _, reports = d.discover(gateway, self.empty_l1, self.cfg, {'gateway': 7, 'l1': 8}, self.roots)
        changed = {**reports[0], 'batchTo': reports[0]['batchTo'] + 1}
        with self.assertRaisesRegex(s.Error, 'unresolved_range_changed'):
            d.hydrate(gateway, self.cfg, changed, str(self.root))
        changed = {**reports[0], 'packageHash': h(999)}
        with self.assertRaisesRegex(s.Error, 'unresolved_event_missing'):
            d.hydrate(gateway, self.cfg, changed, str(self.root))
        gateway.reorg = True
        with self.assertRaisesRegex(s.Error, 'source_block_changed'):
            d.hydrate(gateway, self.cfg, reports[0], str(self.root))

    def test_sidecar_recovers_router_submission(self):
        event, receipt, tx = self.accepted_receipt()
        tx['to'] = a(999)
        item = {'package_hash': self.package, 'mode': 'service', 'sidecar': self.item}
        s.write_new(self.root / (self.package[2:] + '.json'), item)
        gateway = Rpc(99, self.head, self.gate, event, receipt, tx)
        _, reports = d.discover(gateway, self.empty_l1, self.cfg, {'gateway': 7, 'l1': 8}, self.roots)
        payload = s.raw_hex(s.keccak(b'ZKSYS_ACCEPTED_SERVICE_V1')) + s.raw_hex(self.package) + s.word(0)
        self.assertEqual(reports[0]['id'], 'child:' + s.keccak(payload)[2:])
        self.assertFalse(any(m == 'eth_getTransactionByHash' for m, _ in gateway.calls))

    def test_router_transaction_must_match_canonical_receipt(self):
        event, receipt, tx = self.accepted_receipt()
        tx.update(to=a(999), blockHash=h(900))
        gateway = Rpc(99, self.head, self.gate, event, receipt, tx)
        with self.assertRaisesRegex(s.Error, 'source_transaction_mismatch'):
            d.discover(gateway, self.empty_l1, self.cfg, {'gateway': 7, 'l1': 8}, self.roots)

    def test_control_package_is_not_queued(self):
        event, receipt, tx = self.accepted_receipt(control=True)
        gateway = Rpc(99, self.head, self.gate, event, receipt, tx)
        cursor, reports = d.discover(gateway, self.empty_l1, self.cfg, {'gateway': 7, 'l1': 8}, self.roots)
        self.assertEqual(cursor['gateway'], 8)
        self.assertEqual(reports, [])

    def test_wrong_gateway_message_fails_closed(self):
        event, receipt, tx = self.accepted_receipt(alter_message=True)
        gateway = Rpc(99, self.head, self.gate, event, receipt, tx)
        with self.assertRaisesRegex(s.Error, 'acceptance_message_missing'):
            d.discover(gateway, self.empty_l1, self.cfg, {'gateway': 7, 'l1': 8}, self.roots)

    def test_reorg_fails_without_returning_cursor(self):
        event, receipt, tx = self.accepted_receipt()
        gateway = Rpc(99, self.head, self.gate, event, receipt, tx)
        gateway.reorg = True
        with self.assertRaisesRegex(s.Error, 'source_block_receipt_mismatch|source_block_changed'):
            d.discover(gateway, self.empty_l1, self.cfg, {'gateway': 7, 'l1': 8}, self.roots)

    def test_event_outside_block_inventory_does_not_advance_cursor(self):
        event, receipt, tx = self.accepted_receipt()
        event['transactionHash'] = h(999)
        gateway = Rpc(99, self.head, self.gate, event, receipt, tx)
        cursor = {'gateway': 7, 'l1': 8}
        with self.assertRaisesRegex(s.Error, 'source_event_inventory_mismatch'):
            d.discover(gateway, self.empty_l1, self.cfg, cursor, self.roots)
        self.assertEqual(cursor, {'gateway': 7, 'l1': 8})

    def test_event_receipt_must_match_transaction_index(self):
        event, receipt, tx = self.accepted_receipt()
        gateway = Rpc(99, self.head, self.gate, event, receipt, tx)
        gateway.receipt['transactionIndex'] = '0x1'
        with self.assertRaisesRegex(s.Error, 'source_receipt_mismatch'):
            d.discover(gateway, self.empty_l1, self.cfg, {'gateway': 7, 'l1': 8}, self.roots)

    def test_large_unrelated_receipt_is_never_requested(self):
        event, receipt, tx = self.accepted_receipt()
        unrelated = h(23)
        class UnrelatedRpc(Rpc):
            def call(self, method, params):
                if method == 'eth_getBlockByNumber':
                    block = super().call(method, params)
                    if params[0] != 'finalized':
                        block['transactions'].append(unrelated)
                    return block
                if method == 'eth_getTransactionReceipt' and params[0] == unrelated:
                    raise AssertionError('unrelated oversized receipt must not be requested')
                if method == 'eth_getBlockReceipts':
                    raise AssertionError('block receipts must not be requested')
                return super().call(method, params)
        gateway = UnrelatedRpc(99, self.head, self.gate, event, receipt, tx)
        cursor, reports = d.discover(gateway, self.empty_l1, self.cfg, {'gateway': 7, 'l1': 8}, self.roots)
        self.assertEqual(cursor['gateway'], 8)
        self.assertEqual(len(reports), 1)
        self.assertEqual([p[0] for m, p in gateway.calls if m == 'eth_getTransactionReceipt'], [receipt['transactionHash']])

    def test_oversized_matched_receipt_fails_closed(self):
        event, receipt, tx = self.accepted_receipt()
        class OversizedRpc(Rpc):
            def call(self, method, params):
                if method == 'eth_getTransactionReceipt':
                    raise s.Error('rpc_response_limit')
                return super().call(method, params)
        gateway = OversizedRpc(99, self.head, self.gate, event, receipt, tx)
        cursor = {'gateway': 7, 'l1': 8}
        with self.assertRaisesRegex(s.Error, 'rpc_response_limit'):
            d.discover(gateway, self.empty_l1, self.cfg, cursor, self.roots)
        self.assertEqual(cursor, {'gateway': 7, 'l1': 8})

    def test_oversized_filtered_logs_fail_closed(self):
        event, receipt, tx = self.accepted_receipt()
        class OversizedRpc(Rpc):
            def call(self, method, params):
                if method == 'eth_getLogs':
                    raise s.Error('rpc_response_limit')
                return super().call(method, params)
        gateway = OversizedRpc(99, self.head, self.gate, event, receipt, tx)
        cursor = {'gateway': 7, 'l1': 8}
        with self.assertRaisesRegex(s.Error, 'rpc_response_limit'):
            d.discover(gateway, self.empty_l1, self.cfg, cursor, self.roots)
        self.assertEqual(cursor, {'gateway': 7, 'l1': 8})

    def test_multiple_gate_events_are_processed_in_order(self):
        accepted_event, accepted_receipt, tx = self.accepted_receipt()
        root, vk, activation_hash = h(1011), h(1012), h(25)
        activation_event = {'address': self.gate, 'topics': [d.ACTIVATED, '0x' + '00' * 12 + a(200)[2:]],
                            'data': root, 'transactionHash': activation_hash, 'blockHash': h(7),
                            'blockNumber': '0x7', 'transactionIndex': '0x1', 'logIndex': '0x1', 'removed': False}
        payload = s.raw_hex(s.keccak(b'ZKSYS_SERVICE_ACTIVATION_V1')) + s.raw_hex(vk) + s.raw_hex(root)
        message = {'sender': d.MESSENGER, 'key': '0x' + '00' * 12 + self.gate[2:], 'value': s.keccak(payload),
                   'transactionLogIndex': '0x0', 'transactionIndex': '0x1', 'shardId': '0x0', 'isService': True}
        activation_receipt = {'transactionHash': activation_hash, 'blockHash': h(7), 'blockNumber': '0x7',
                              'transactionIndex': '0x1', 'status': '0x1', 'logs': [activation_event],
                              'l2ToL1Logs': [message]}
        class MultipleRpc(Rpc):
            def call(self, method, params):
                if method == 'eth_getBlockByNumber':
                    result = super().call(method, params)
                    if params[0] != 'finalized':
                        result['transactions'].append(activation_hash)
                    return result
                if method == 'eth_getLogs':
                    self.calls.append((method, params))
                    return [copy.deepcopy(accepted_event), copy.deepcopy(activation_event)]
                if method == 'eth_getTransactionReceipt' and params[0] == activation_hash:
                    self.calls.append((method, params))
                    return copy.deepcopy(activation_receipt)
                return super().call(method, params)
        gateway = MultipleRpc(99, self.head, self.gate, accepted_event, accepted_receipt, tx, vk)
        cursor, reports = d.discover(gateway, self.empty_l1, self.cfg, {'gateway': 7, 'l1': 8}, self.roots)
        self.assertEqual(cursor['gateway'], 8)
        self.assertEqual([report['kind'] for report in reports], ['accepted', 'activation'])
        self.assertEqual([report['sourceTransactionHash'] for report in reports], [h(22), activation_hash])
        logs_queries = [params[0] for method, params in gateway.calls if method == 'eth_getLogs']
        self.assertEqual(logs_queries, [{'blockHash': h(7), 'address': self.gate,
                                         'topics': [[d.ACCEPTED, d.ACTIVATED]]}])

    def test_malformed_filtered_event_fails_closed(self):
        event, receipt, tx = self.accepted_receipt()
        class MalformedRpc(Rpc):
            def call(self, method, params):
                if method == 'eth_getLogs':
                    return [{**self.event, 'address': a(999)}]
                return super().call(method, params)
        gateway = MalformedRpc(99, self.head, self.gate, event, receipt, tx)
        with self.assertRaisesRegex(s.Error, 'invalid_source_event'):
            d.discover(gateway, self.empty_l1, self.cfg, {'gateway': 7, 'l1': 8}, self.roots)

    def test_duplicate_or_unsorted_events_fail_closed(self):
        event, receipt, tx = self.accepted_receipt()
        class DuplicateRpc(Rpc):
            def call(self, method, params):
                if method == 'eth_getLogs':
                    return [copy.deepcopy(self.event), copy.deepcopy(self.event)]
                return super().call(method, params)
        gateway = DuplicateRpc(99, self.head, self.gate, event, receipt, tx)
        with self.assertRaisesRegex(s.Error, 'invalid_source_log_order'):
            d.discover(gateway, self.empty_l1, self.cfg, {'gateway': 7, 'l1': 8}, self.roots)

    def test_scan_block_window_never_skips_later_block(self):
        event, receipt, tx = self.accepted_receipt()
        class RangeRpc(Rpc):
            def call(self, method, params):
                if method == 'eth_getBlockByNumber' and params[0] in ('finalized', '0x8'):
                    return {'number': '0x8', 'hash': h(8), 'transactions': []}
                return super().call(method, params)
        gateway = RangeRpc(99, self.head, self.gate, event, receipt, tx)
        cfg = {**self.cfg, 'scan_blocks': 1}
        cursor, reports = d.discover(gateway, self.empty_l1, cfg, {'gateway': 7, 'l1': 8}, self.roots)
        self.assertEqual(cursor['gateway'], 8)
        self.assertEqual(len(reports), 1)
        cursor, reports = d.discover(gateway, self.empty_l1, cfg, cursor, self.roots)
        self.assertEqual(cursor['gateway'], 9)
        self.assertEqual(reports, [])

    def test_root_lane_acceptance_requires_matching_sink_event(self):
        accepted = copy.deepcopy(self.accepted)
        accepted['chainId'], accepted['chainAddress'] = hex(99), self.cfg['gateway_chain_address']
        sidecar = copy.deepcopy(self.item); sidecar['accepted_package'] = accepted
        package = s.struct_hash('AcceptedPackageV1', s.PACKAGE, accepted)
        payload = s.raw_hex(s.keccak(b'ZKSYS_ACCEPTED_SERVICE_V1')) + s.raw_hex(package) + s.word(0)
        event = {'address': self.root_gate, 'topics': [d.ACCEPTED, package,
            '0x' + s.word(accepted['batchFrom']).hex(), '0x' + s.word(accepted['batchTo']).hex()],
            'data': '0x' + s.word(0).hex(), 'transactionHash': h(23), 'blockHash': h(7),
            'blockNumber': '0x7', 'transactionIndex': '0x0', 'logIndex': '0x0', 'removed': False}
        published = {'address': self.sink, 'topics': [d.PUBLISHED, s.keccak(payload)], 'data': '0x', 'removed': False}
        receipt = {'transactionHash': h(23), 'blockHash': h(7), 'blockNumber': '0x7',
                   'transactionIndex': '0x0', 'status': '0x1', 'logs': [event, published]}
        tx = {'hash': h(23), 'to': self.root_gate, 'blockHash': h(7), 'blockNumber': '0x7',
              'input': relay.gate_calldata(sidecar, self.vector['proof_data'], 'service')}
        l1 = Rpc(57, self.head, self.root_gate, event, receipt, tx)
        empty_gw = Rpc(99, self.head, self.gate, None, None)
        _, reports = d.discover(empty_gw, l1, self.cfg, {'gateway': 8, 'l1': 7}, self.roots)
        self.assertTrue(reports[0]['fromRoot'])
        self.assertEqual(reports[0]['accepted'], accepted)
        receipt['logs'] = [event]
        with self.assertRaisesRegex(s.Error, 'acceptance_message_missing'):
            d.discover(empty_gw, l1, self.cfg, {'gateway': 8, 'l1': 7}, self.roots)

    def test_gateway_activation_binds_vk_and_roster(self):
        root, vk = h(1011), h(1012)
        event = {'address': self.gate, 'topics': [d.ACTIVATED, '0x' + '00' * 12 + a(200)[2:]],
                 'data': root, 'transactionHash': h(25), 'blockHash': h(7), 'blockNumber': '0x7',
                 'transactionIndex': '0x0', 'logIndex': '0x0', 'removed': False}
        payload = s.raw_hex(s.keccak(b'ZKSYS_SERVICE_ACTIVATION_V1')) + s.raw_hex(vk) + s.raw_hex(root)
        message = {'sender': d.MESSENGER, 'key': '0x' + '00' * 12 + self.gate[2:], 'value': s.keccak(payload),
                   'transactionLogIndex': '0x0', 'transactionIndex': '0x0', 'shardId': '0x0', 'isService': True}
        receipt = {'transactionHash': h(25), 'blockHash': h(7), 'blockNumber': '0x7',
                   'transactionIndex': '0x0', 'status': '0x1', 'logs': [event], 'l2ToL1Logs': [message]}
        gateway = Rpc(99, self.head, self.gate, event, receipt, vk=vk)
        _, reports = d.discover(gateway, self.empty_l1, self.cfg, {'gateway': 7, 'l1': 8}, self.roots)
        self.assertEqual((reports[0]['kind'], reports[0]['vk_hash'], reports[0]['roster_root']), ('activation', vk, root))
        gateway.vk = h(1013)
        with self.assertRaisesRegex(s.Error, 'activation_message_missing'):
            d.discover(gateway, self.empty_l1, self.cfg, {'gateway': 7, 'l1': 8}, self.roots)

    def test_identical_activation_data_on_both_lanes_has_distinct_ids(self):
        root, vk = h(2011), h(2012)
        payload = s.raw_hex(s.keccak(b'ZKSYS_SERVICE_ACTIVATION_V1')) + s.raw_hex(vk) + s.raw_hex(root)
        def event(gate, tx):
            return {'address': gate, 'topics': [d.ACTIVATED, '0x' + '00' * 12 + a(200)[2:]],
                    'data': root, 'transactionHash': tx, 'blockHash': h(7), 'blockNumber': '0x7',
                    'transactionIndex': '0x0', 'logIndex': '0x0', 'removed': False}
        child_event, root_event = event(self.gate, h(26)), event(self.root_gate, h(27))
        msg = {'sender': d.MESSENGER, 'key': '0x' + '00' * 12 + self.gate[2:], 'value': s.keccak(payload),
               'transactionLogIndex': '0x0', 'transactionIndex': '0x0', 'shardId': '0x0', 'isService': True}
        published = {'address': self.sink, 'topics': [d.PUBLISHED, s.keccak(payload)], 'data': '0x', 'removed': False}
        def receipt(ev, logs):
            return {'transactionHash': ev['transactionHash'], 'blockHash': h(7), 'blockNumber': '0x7',
                    'transactionIndex': '0x0', 'status': '0x1', 'logs': [ev] + logs}
        child_receipt = {**receipt(child_event, []), 'l2ToL1Logs': [msg]}
        root_receipt = receipt(root_event, [published])
        gateway = Rpc(99, self.head, self.gate, child_event, child_receipt, vk=vk)
        l1 = Rpc(57, self.head, self.root_gate, root_event, root_receipt, vk=vk)
        _, reports = d.discover(gateway, l1, self.cfg, {'gateway': 7, 'l1': 7}, self.roots)
        self.assertEqual({r['id'] for r in reports}, {'child:' + s.keccak(payload)[2:], 'root:' + s.keccak(payload)[2:]})


if __name__ == '__main__': unittest.main()
