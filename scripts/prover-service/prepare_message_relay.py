#!/usr/bin/env python3
"""Diagnose the direct Gateway-message receiver path with read-only on-chain verification.

This is not the NEVM relay. The command does not sign or broadcast, and it refuses
proofs that 0x10009 or the receiver reject. Gateway-origin direct-L1 batch proofs
are not supported by the child shared-root verifier on the intended topology.
"""
import argparse
import json
import os
from pathlib import Path
import sys

import deploy_gate as common
import relay
import service as s

MESSENGER = '0x0000000000000000000000000000000000008008'
VERIFIER = '0x0000000000000000000000000000000000010009'


def require(value, reason):
    s.require(value, reason)


def quantity(value):
    require(type(value) is int or isinstance(value, str), 'invalid_quantity')
    try:
        number = value if type(value) is int else int(value, 16 if value.startswith('0x') else 10)
    except ValueError:
        raise s.Error('invalid_quantity') from None
    require(number >= 0, 'invalid_quantity')
    return number


def addr(value):
    s.nonzero(value, 20)
    return value.lower()


def hash32(value):
    s.raw_hex(value, 32)
    return value.lower()


def encoded_call(signature, parts):
    return '0x' + (relay.selector(signature) + s.encode_dynamic_tuple(parts)).hex()


def message_and_parts(request):
    kind = request['kind']
    if kind == 'activation':
        require(set(request) == {'kind', 'vk_hash', 'roster_root'}, 'invalid_activation')
        vk, root = hash32(request['vk_hash']), hash32(request['roster_root'])
        domain = s.keccak(b'ZKSYS_SERVICE_ACTIVATION_V1')
        data = s.raw_hex(domain) + s.raw_hex(vk) + s.raw_hex(root)
        return data, [(s.raw_hex(vk), False), (s.raw_hex(root), False)], 'relayActivation(bytes32,bytes32,(uint256,uint256,uint16,bytes32[]))'
    require(kind == 'accepted' and set(request) == {'kind', 'accepted_package', 'duties', 'bootstrap'},
            'invalid_accepted_request')
    accepted, duties, bootstrap = request['accepted_package'], request['duties'], request['bootstrap']
    require(type(bootstrap) is bool, 'invalid_bootstrap')
    require(s.report_hash(duties).lower() == hash32(accepted['reportHash']), 'report_hash_mismatch')
    package_hash = s.struct_hash('AcceptedPackageV1', s.PACKAGE, accepted)
    domain = s.keccak(b'ZKSYS_ACCEPTED_SERVICE_V1')
    data = s.raw_hex(domain) + s.raw_hex(package_hash) + s.word(int(bootstrap))
    encoded_duties = s.word(len(duties)) + s.encode_dynamic_tuple([(s.encode_duty(duty), True) for duty in duties])
    signature = f"relayAccepted({relay.abi_tuple(s.PACKAGE)},{relay.abi_tuple(s.DUTY + [('operatorSignature', 'bytes')])}[],bool,(uint256,uint256,uint16,bytes32[]))"
    return data, [(s.encode_fields(s.PACKAGE, accepted), False), (encoded_duties, True),
                  (s.word(int(bootstrap)), False)], signature


def prepare(cfg, gateway, child, request):
    require(type(cfg) is dict and set(cfg) == {'gateway_chain_id', 'child_chain_id', 'gateway_genesis_hash', 'child_genesis_hash', 'gateway_gate', 'receiver',
        'receiver_code_hash', 'sender', 'transaction_hash', 'transaction_log_index', 'proof_target'}, 'invalid_config')
    gateway_id, child_id = quantity(cfg['gateway_chain_id']), quantity(cfg['child_chain_id'])
    require(gateway_id > 0 and child_id > 0 and gateway_id != child_id, 'invalid_chain_ids')
    require(quantity(gateway.call('eth_chainId', [])) == gateway_id
            and quantity(child.call('eth_chainId', [])) == child_id, 'chain_id_mismatch')
    for rpc, key in ((gateway, 'gateway_genesis_hash'), (child, 'child_genesis_hash')):
        genesis = rpc.call('eth_getBlockByNumber', ['0x0', False])
        require(type(genesis) is dict and genesis.get('hash', '').lower() == hash32(cfg[key]),
                'genesis_mismatch')
    gate, receiver, sender = addr(cfg['gateway_gate']), addr(cfg['receiver']), addr(cfg['sender'])
    txhash, codehash = hash32(cfg['transaction_hash']), hash32(cfg['receiver_code_hash'])
    index = quantity(cfg['transaction_log_index'])
    require(index <= 16383 and cfg['proof_target'] in ('l1BatchRoot', 'messageRoot'), 'invalid_proof_request')
    destination = child.call('eth_getBlockByNumber', ['latest', False])
    require(type(destination) is dict, 'destination_block_unavailable')
    dest_number = hex(quantity(destination['number']))
    dest_hash = hash32(destination['hash'])
    code = child.call('eth_getCode', [receiver, dest_number])
    require(isinstance(code, str) and code != '0x' and common.digest(code) == codehash, 'receiver_code_mismatch')
    def read_word(signature):
        data = '0x' + relay.selector(signature).hex()
        value = child.call('eth_call', [{'to': receiver, 'data': data}, dest_number])
        require(isinstance(value, str) and len(value) == 66, 'invalid_receiver_response')
        return int(value, 16)
    require(read_word('gatewayGate()') == int(gate, 16), 'receiver_gate_mismatch')
    require(read_word('gatewayChainId()') == gateway_id, 'receiver_gateway_mismatch')
    data, parts, signature = message_and_parts(request)
    receipt = gateway.call('eth_getTransactionReceipt', [txhash])
    require(type(receipt) is dict and receipt.get('transactionHash', '').lower() == txhash
            and quantity(receipt.get('status')) == 1, 'receipt_unavailable_or_failed')
    block_hash = hash32(receipt['blockHash'])
    block = gateway.call('eth_getBlockByNumber', [receipt['blockNumber'], False])
    require(type(block) is dict and block.get('hash', '').lower() == block_hash, 'receipt_not_canonical')
    logs = receipt.get('l2ToL1Logs')
    require(type(logs) is list and index < len(logs), 'message_log_unavailable')
    log = logs[index]
    require(type(log) is dict and log.get('sender', '').lower() == MESSENGER
            and log.get('key', '').lower() == '0x' + '00' * 12 + gate[2:]
            and log.get('value', '').lower() == s.keccak(data)
            and quantity(log.get('shardId')) == 0 and log.get('isService') is True
            and quantity(log.get('transactionLogIndex')) == index, 'message_log_mismatch')
    tx_index = quantity(log.get('transactionIndex'))
    require(tx_index < 65536 and tx_index == quantity(receipt['transactionIndex']), 'transaction_index_mismatch')
    proof = gateway.call('zks_getL2ToL1LogProof', [txhash, hex(index), cfg['proof_target']])
    require(type(proof) is dict, 'proof_unavailable')
    words = proof.get('proof')
    require(type(words) is list and 0 < len(words) <= 128, 'invalid_proof')
    proof_words = b''.join(s.raw_hex(word, 32) for word in words)
    message_index = quantity(proof['id'])
    block_or_batch = quantity(proof['batchNumber'])
    array = s.word(len(words)) + proof_words
    inclusion = s.encode_dynamic_tuple([(s.word(block_or_batch), False), (s.word(message_index), False),
                                        (s.word(tx_index), False), (array, True)])
    message = s.encode_dynamic_tuple([(s.word(tx_index), False), (s.word(int(gate, 16)), False),
                                      (s.encode_bytes(data), True)])
    verifier_call = encoded_call('proveL2MessageInclusionShared(uint256,uint256,uint256,(uint16,address,bytes),bytes32[])',
        [(s.word(gateway_id), False), (s.word(block_or_batch), False), (s.word(message_index), False),
         (message, True), (array, True)])
    verdict = child.call('eth_call', [{'to': VERIFIER, 'data': verifier_call}, dest_number])
    require(isinstance(verdict, str) and len(verdict) == 66 and int(verdict, 16) == 1, 'message_proof_not_verified')
    calldata = encoded_call(signature, parts + [(inclusion, True)])
    require(child.call('eth_call', [{'from': sender, 'to': receiver, 'data': calldata}, dest_number]) == '0x',
            'receiver_simulation_failed')
    require(child.call('eth_getBlockByNumber', [dest_number, False])['hash'].lower() == dest_hash,
            'destination_block_changed')
    require(gateway.call('eth_getBlockByNumber', [receipt['blockNumber'], False])['hash'].lower() == block_hash,
            'source_block_changed')
    return {'status': 'verified_at_block', 'to': receiver, 'from': sender, 'data': calldata,
            'value': '0x0', 'chain_id': child_id, 'source_transaction_hash': txhash,
            'source_block_hash': block_hash, 'source_batch_number': block_or_batch,
            'destination_block_hash': dest_hash, 'destination_block_number': dest_number,
            'proof_target': cfg['proof_target']}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--config', required=True)
    parser.add_argument('--request', required=True)
    parser.add_argument('--output', required=True)
    args = parser.parse_args()
    gateway_url, child_url = os.environ.get('ZKSYS_SERVICE_GATEWAY_RPC_URL'), os.environ.get('ZKSYS_SERVICE_REGISTRY_RPC_URL')
    require(gateway_url and child_url, 'missing_rpc_url')
    gateway = relay.Rpc(gateway_url, authorization=os.environ.get('ZKSYS_SERVICE_GATEWAY_RPC_AUTHORIZATION'))
    child = relay.Rpc(child_url, authorization=os.environ.get('ZKSYS_SERVICE_REGISTRY_RPC_AUTHORIZATION'))
    result = prepare(s.read_json(Path(args.config), private=True), gateway, child,
                     s.read_json(Path(args.request), private=True))
    s.write_new(Path(args.output), result)
    print(json.dumps({'status': result['status'], 'output': args.output}, sort_keys=True))


if __name__ == '__main__':
    try:
        main()
    except (s.Error, common.TransactionError, OSError, KeyError, TypeError, ValueError) as error:
        print(str(error), file=sys.stderr)
        sys.exit(1)
