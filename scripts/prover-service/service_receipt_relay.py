#!/usr/bin/env python3
"""Common service receipt discovery, deployment checks and ABI helpers."""
from pathlib import Path
import sys

import keeper
import relay
import service as s
import service_receipt_discovery as discovery

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from zksys_keeper_cast import hash_bytes
from zksys_keeper_transactions import priority_payload

RELAY_EVENT = s.keccak(b'MessageRelayed(bytes32,bytes32)')
PRIORITY_EVENT = '0x4531cd5795773d7101c17bdeb9f5ab7f47d7056017506f937083be5d6e77a382'


def address_word(address):
    return bytes(12) + s.raw_hex(address, 20)


def dynamic_array(items):
    return s.word(len(items)) + s.encode_dynamic_tuple([(item, True) for item in items])


def inclusion_bytes(proof):
    s.exact(proof, ('blockOrBatchNumber', 'messageIndex', 'txNumberInBatch', 'merkleProof'))
    words = proof['merkleProof']
    s.require(type(words) is list, 'invalid_inclusion_proof')
    array = s.word(len(words)) + b''.join(s.raw_hex(word, 32) for word in words)
    return s.encode_dynamic_tuple([(s.word(s.uint(proof['blockOrBatchNumber'])), False),
        (s.word(s.uint(proof['messageIndex'])), False), (s.word(s.uint(proof['txNumberInBatch'], 16)), False),
        (array, True)])


def call_data(signature, parts):
    return '0x' + (relay.selector(signature) + s.encode_dynamic_tuple(parts)).hex()


def report_message(report):
    if report['kind'] == 'unresolved':
        return (s.raw_hex(s.keccak(b'ZKSYS_ACCEPTED_SERVICE_V1')) + s.raw_hex(report['packageHash'], 32)
                + s.word(int(report['bootstrap'])))
    if report['kind'] == 'activation':
        return (s.raw_hex(s.keccak(b'ZKSYS_SERVICE_ACTIVATION_V1')) + s.raw_hex(report['vk_hash'], 32)
                + s.raw_hex(report['roster_root'], 32))
    s.require(report['kind'] == 'accepted' and type(report['bootstrap']) is bool, 'invalid_accepted_request')
    accepted = report['accepted']
    s.raw_hex(accepted['reportHash'], 32)
    s.require(s.report_hash(report['duties']).lower() == accepted['reportHash'].lower(), 'report_hash_mismatch')
    package_hash = s.struct_hash('AcceptedPackageV1', s.PACKAGE, accepted)
    return (s.raw_hex(s.keccak(b'ZKSYS_ACCEPTED_SERVICE_V1')) + s.raw_hex(package_hash, 32)
            + s.word(int(report['bootstrap'])))


def configuration(cfg):
    keys = ('schema_version', 'l1_rpc_url', 'gateway_rpc_url', 'zksys_rpc_url', 'rpc_timeout_seconds',
            'l1_chain_id', 'gateway_chain_id', 'zksys_chain_id', 'l1_genesis', 'gateway_genesis', 'zksys_genesis',
            'root_gate', 'gateway_gate', 'root_sink', 'bridgehub', 'gateway_chain_address', 'child_chain_address',
            'l1_zksys_diamond', 'bridgehub_implementation', 'gateway_mailbox_facet', 'l1_priority_facet',
            'receiver', 'registry', 'policy_hash', 'root_gate_code_hash', 'gateway_gate_code_hash', 'root_sink_code_hash',
            'bridgehub_code_hash', 'gateway_chain_code_hash', 'receiver_code_hash', 'registry_code_hash',
            'l1_zksys_diamond_code_hash', 'bridgehub_implementation_code_hash', 'gateway_mailbox_facet_code_hash',
            'l1_priority_facet_code_hash',
            'signer', 'refund', 'keystore', 'password_file', 'l1_gas_limit', 'l1_gas_price',
            'priority_gas_limit', 'gas_per_pubdata', 'child_artifact_dir', 'root_artifact_dir',
            'gateway_start_block', 'l1_start_block', 'scan_blocks', 'poll_seconds')
    s.exact(cfg, keys)
    s.require(cfg['schema_version'] == 1, 'unsupported_receipt_relayer_config')
    for key in ('l1_chain_id', 'gateway_chain_id', 'zksys_chain_id', 'rpc_timeout_seconds', 'l1_gas_limit',
                'l1_gas_price', 'priority_gas_limit', 'gas_per_pubdata', 'scan_blocks', 'poll_seconds'):
        s.require(type(cfg[key]) is int and cfg[key] > 0, 'invalid_receipt_relayer_number')
    for key in ('gateway_start_block', 'l1_start_block'):
        s.require(type(cfg[key]) is int and cfg[key] >= 0, 'invalid_receipt_start')
    s.require(len({cfg['l1_chain_id'], cfg['gateway_chain_id'], cfg['zksys_chain_id']}) == 3,
              'receipt_chain_ids_must_differ')
    s.require(cfg['rpc_timeout_seconds'] <= 30 and cfg['poll_seconds'] <= 3600, 'invalid_receipt_timeout')
    for key in ('root_gate', 'gateway_gate', 'root_sink', 'bridgehub', 'gateway_chain_address',
                'child_chain_address', 'l1_zksys_diamond', 'bridgehub_implementation', 'gateway_mailbox_facet',
                'l1_priority_facet', 'receiver', 'registry', 'signer', 'refund'):
        s.nonzero(cfg[key], 20)
    for key in ('l1_genesis', 'gateway_genesis', 'zksys_genesis', 'policy_hash', 'root_gate_code_hash', 'gateway_gate_code_hash',
                'root_sink_code_hash', 'bridgehub_code_hash', 'gateway_chain_code_hash', 'receiver_code_hash',
                'registry_code_hash', 'l1_zksys_diamond_code_hash', 'bridgehub_implementation_code_hash',
                'gateway_mailbox_facet_code_hash', 'l1_priority_facet_code_hash'):
        s.nonzero(cfg[key])
    for key in ('keystore', 'password_file', 'child_artifact_dir', 'root_artifact_dir'):
        s.require(type(cfg[key]) is str and Path(cfg[key]).is_absolute(), 'absolute_receipt_path_required')
    for key in ('l1_rpc_url', 'gateway_rpc_url', 'zksys_rpc_url'):
        relay.Rpc(cfg[key], timeout=cfg['rpc_timeout_seconds'])
    return cfg


class Hooks:
    def __init__(self, cfg, l1, gateway, zksys, signer):
        self.cfg, self.l1, self.gateway, self.zksys, self.signer = cfg, l1, gateway, zksys, signer

    def anchor(self, rpc, chain_id, genesis):
        s.require(s.uint(rpc.call('eth_chainId', [])) == chain_id, 'receipt_chain_changed')
        first = rpc.call('eth_getBlockByNumber', ['0x0', False])
        s.require(type(first) is dict and first.get('hash') == genesis, 'receipt_genesis_changed')
        block = rpc.call('eth_getBlockByNumber', ['finalized', False])
        s.require(type(block) is dict, 'receipt_finality_unavailable')
        s.nonzero(block.get('hash'))
        s.uint(block.get('number'))
        return block, {'blockHash': block['hash'], 'requireCanonical': True}

    def verify_deployment(self):
        c = self.cfg
        for rpc, name, checks in ((self.l1, 'l1', [('root_gate', 'root_gate_code_hash'), ('root_sink', 'root_sink_code_hash'),
                                                   ('bridgehub', 'bridgehub_code_hash'), ('gateway_chain_address', 'gateway_chain_code_hash'),
                                                   ('l1_zksys_diamond', 'l1_zksys_diamond_code_hash'),
                                                   ('bridgehub_implementation', 'bridgehub_implementation_code_hash'),
                                                   ('gateway_mailbox_facet', 'gateway_mailbox_facet_code_hash'),
                                                   ('l1_priority_facet', 'l1_priority_facet_code_hash')]),
                                  (self.gateway, 'gateway', [('gateway_gate', 'gateway_gate_code_hash')]),
                                  (self.zksys, 'zksys', [('receiver', 'receiver_code_hash'), ('registry', 'registry_code_hash')])):
            block, anchor = self.anchor(rpc, c[name + '_chain_id'], c[name + '_genesis'])
            for addr, codehash in checks:
                keeper.check_code(rpc, c[addr], c[codehash], anchor, 'receipt_contract_code_changed')
            again = rpc.call('eth_getBlockByNumber', [block['number'], False])
            s.require(type(again) is dict and again.get('hash') == block['hash'], 'receipt_anchor_changed')
        _, l1 = self.anchor(self.l1, c['l1_chain_id'], c['l1_genesis'])
        _, gateway = self.anchor(self.gateway, c['gateway_chain_id'], c['gateway_genesis'])
        _, child = self.anchor(self.zksys, c['zksys_chain_id'], c['zksys_genesis'])
        view = lambda rpc, target, sig, args=b'', kind='uint', anchor=l1: relay.call_word(rpc, target, sig, anchor, args, kind)
        for sig, expected in (('gatewayGate()', c['gateway_gate']), ('gatewayChainAddress()', c['gateway_chain_address']),
                              ('bridgehub()', c['bridgehub']), ('registryReceiver()', c['receiver']), ('publisher()', c['root_gate'])):
            s.require(view(self.l1, c['root_sink'], sig, kind='address') == expected, 'receipt_sink_binding_changed')
        s.require(view(self.l1, c['bridgehub'], 'getZKChain(uint256)', s.word(c['gateway_chain_id']), 'address') == c['gateway_chain_address'],
                  'receipt_gateway_diamond_changed')
        s.require(view(self.l1, c['bridgehub'], 'getZKChain(uint256)', s.word(c['zksys_chain_id']), 'address') == c['l1_zksys_diamond'],
                  'receipt_zksys_diamond_changed')
        s.require(view(self.l1, c['bridgehub'], 'settlementLayer(uint256)', s.word(c['zksys_chain_id'])) == c['gateway_chain_id'],
                  'receipt_settlement_layer_changed')
        slot = '0x360894a13ba1a3210667c828492db98dca3e2076cc3735a920a3ca505d382bbc'
        raw = self.l1.call('eth_getStorageAt', [c['bridgehub'], slot, l1])
        s.require(s.raw_hex(raw, 32) == address_word(c['bridgehub_implementation']), 'receipt_bridgehub_implementation_changed')
        selector = relay.selector('proveL2MessageInclusion(uint256,uint256,(uint16,address,bytes),bytes32[])')
        s.require(view(self.l1, c['gateway_chain_address'], 'facetAddress(bytes4)', selector + bytes(28), 'address')
                  == c['gateway_mailbox_facet'], 'receipt_gateway_mailbox_facet_changed')
        selector = relay.selector('bridgehubRequestL2Transaction((address,address,uint256,uint256,bytes,uint256,uint256,bytes[],address))')
        s.require(view(self.l1, c['l1_zksys_diamond'], 'facetAddress(bytes4)', selector + bytes(28), 'address')
                  == c['l1_priority_facet'], 'receipt_l1_priority_facet_changed')
        s.require(view(self.l1, c['root_sink'], 'registryChainId()') == c['zksys_chain_id']
                  and view(self.l1, c['root_sink'], 'gatewayChainId()') == c['gateway_chain_id'], 'receipt_sink_chain_changed')
        s.require(view(self.zksys, c['receiver'], 'registry()', kind='address', anchor=child) == c['registry']
                  and view(self.zksys, c['receiver'], 'rootMessageSink()', kind='address', anchor=child) == c['root_sink']
                  and view(self.zksys, c['receiver'], 'gatewayGate()', kind='address', anchor=child) == c['gateway_gate']
                  and view(self.zksys, c['receiver'], 'gatewayChainId()', anchor=child) == c['gateway_chain_id'],
                  'receipt_receiver_binding_changed')
        policy = view(self.l1, c['root_sink'], 'policyHash()', kind='bytes32')
        s.require(policy == c['policy_hash'], 'receipt_policy_changed')
        s.require(view(self.l1, c['root_gate'], 'chain()', kind='address') == c['gateway_chain_address']
                  and view(self.l1, c['root_gate'], 'messageSink()', kind='address') == c['root_sink']
                  and view(self.l1, c['root_gate'], 'childChainId()') == c['gateway_chain_id']
                  and view(self.l1, c['root_gate'], 'policyHash()', kind='bytes32') == policy
                  and view(self.gateway, c['gateway_gate'], 'chain()', kind='address', anchor=gateway) == c['child_chain_address']
                  and view(self.gateway, c['gateway_gate'], 'messageSink()', kind='address', anchor=gateway) == s.ZERO_ADDRESS
                  and view(self.gateway, c['gateway_gate'], 'childChainId()', anchor=gateway) == c['zksys_chain_id']
                  and view(self.gateway, c['gateway_gate'], 'policyHash()', kind='bytes32', anchor=gateway) == policy
                  and view(self.zksys, c['registry'], 'policyHash()', kind='bytes32', anchor=child) == policy,
                  'receipt_gate_policy_changed')
        return True

    def discover(self, cursor):
        if cursor is None:
            cursor = {'gateway': self.cfg['gateway_start_block'], 'l1': self.cfg['l1_start_block']}
        return discovery.discover(self.gateway, self.l1, self.cfg, cursor,
            {'child': self.cfg['child_artifact_dir'], 'root': self.cfg['root_artifact_dir']})

    def validate_report(self, report):
        s.require(type(report) is dict and report.get('kind') in ('accepted', 'activation', 'unresolved')
                  and type(report.get('fromRoot')) is bool and type(report.get('bootstrap')) is bool,
                  'invalid_receipt_report')
        data = report_message(report)
        expected = ('root:' if report['fromRoot'] else 'child:') + s.keccak(data)[2:]
        s.require(report.get('id') == expected, 'receipt_report_identity_changed')
        if report['kind'] == 'accepted':
            chain = self.cfg['gateway_chain_id'] if report['fromRoot'] else self.cfg['zksys_chain_id']
            address = self.cfg['gateway_chain_address'] if report['fromRoot'] else self.cfg['child_chain_address']
            s.require(s.uint(report['accepted']['chainId']) == chain and report['accepted']['chainAddress'] == address,
                      'receipt_report_lane_changed')
        rpc = self.l1 if report['fromRoot'] else self.gateway
        block = rpc.call('eth_getBlockByNumber', [report['sourceBlockNumber'], False])
        s.require(type(block) is dict and block.get('hash') == report['sourceBlockHash'], 'receipt_source_reorganized')

    def hydrate(self, report):
        rpc = self.l1 if report['fromRoot'] else self.gateway
        root = self.cfg['root_artifact_dir'] if report['fromRoot'] else self.cfg['child_artifact_dir']
        return discovery.hydrate(rpc, self.cfg, report, root)

    def validate_resolution(self, prior, resolved):
        fields = ('id', 'fromRoot', 'bootstrap', 'sourceTransactionHash', 'sourceBlockHash',
                  'sourceBlockNumber', 'sourceLogIndex', 'transactionIndex', 'transactionLogIndex')
        s.require(prior['kind'] == 'unresolved' and resolved.get('kind') == 'accepted'
                  and all(prior[key] == resolved.get(key) for key in fields)
                  and s.struct_hash('AcceptedPackageV1', s.PACKAGE, resolved['accepted']) == prior['packageHash']
                  and resolved['accepted']['batchFrom'] == prior['batchFrom']
                  and resolved['accepted']['batchTo'] == prior['batchTo'], 'receipt_resolution_changed')

    def proof(self, report):
        if report['fromRoot']:
            return {'blockOrBatchNumber': 0, 'messageIndex': 0, 'txNumberInBatch': 0, 'merkleProof': []}
        data = self.gateway.call('zks_getL2ToL1LogProof', [report['sourceTransactionHash'],
                                                          hex(s.uint(report['transactionLogIndex'])), 'l1BatchRoot'])
        if data is None:
            return None
        s.require(type(data) is dict and type(data.get('proof')) is list and data['proof'], 'gateway_message_proof_invalid')
        return {'blockOrBatchNumber': s.uint(data['batchNumber']), 'messageIndex': s.uint(data['id']),
                'txNumberInBatch': s.uint(s.uint(report['transactionIndex']), 16), 'merkleProof': data['proof']}

    def priority_hash(self, receipt, entry, reports):
        cfg, meta = self.cfg, entry['metadata']
        matching = [log for log in receipt.get('logs', []) if not log.get('removed')
                    and log.get('address', '').lower() == cfg['root_sink'] and type(log.get('topics')) is list
                    and len(log['topics']) == 3 and log['topics'][0].lower() == RELAY_EVENT
                    and log['topics'][1].lower() == meta['commitment']]
        s.require(len(matching) == 1, 'receipt_sink_event_missing')
        digest = matching[0]['topics'][2].lower()
        s.raw_hex(digest, 32)
        # On L1 Bridgehub, the configured child diamond emits the canonical priority transaction.
        payload = priority_payload(receipt, cfg['l1_zksys_diamond'], PRIORITY_EVENT, digest, hash_bytes)
        alias = '0x' + format((int(cfg['root_sink'], 16) + int('1111000000000000000000000000000000001111', 16)) % (1 << 160), '040x')
        s.require(payload['transactionType'] == 127 and payload['from'] == alias
                  and payload['to'] == cfg['receiver'] and payload['input'] == meta['childData'],
                  'receipt_priority_payload_changed')
        return digest

    def l2_chain_id(self, rpc):
        return s.uint(rpc('eth_chainId', [])) == self.cfg['zksys_chain_id']

    def verify_delivery(self, receipt, reports):
        anchor = {'blockHash': receipt['blockHash'], 'requireCanonical': True}
        for report in reports:
            mapping = 'relayedRootMessages(bytes32)' if report['fromRoot'] else 'relayedMessages(bytes32)'
            if not relay.call_word(self.zksys, self.cfg['receiver'], mapping, anchor,
                                   s.raw_hex(s.keccak(report_message(report))), 'bool'):
                return False
        return True



if __name__ == '__main__':
    print('This module provides shared helpers; run the root publisher, local claimant and native relayer.', file=sys.stderr)
    sys.exit(1)
