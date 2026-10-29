"""Discover finalized service-gate messages without losing unmatched report data.

The caller must persist the returned cursor and all reports atomically. A report
whose preimages are unavailable remains an unresolved, non-deliverable item. The
chain provider must be independently trusted; receipt tries are not reconstructed.
"""
from pathlib import Path
import re

import relay
import service as s
import workflow_io as io

ACCEPTED = s.keccak(b'ServicePackageAccepted(bytes32,uint64,uint64,bool)')
ACTIVATED = s.keccak(b'ServiceActivated(address,bytes32)')
CONTROL = s.keccak(b'ServiceControlPackageAccepted(bytes32)')
PUBLISHED = s.keccak(b'MessagePublished(bytes32)')
MESSENGER = '0x0000000000000000000000000000000000008008'


def q(value):
    s.require(isinstance(value, str) and re.fullmatch(r'0x(?:0|[1-9a-f][0-9a-f]*)', value), 'invalid_rpc_quantity')
    return int(value, 16)


def hex32(value):
    s.raw_hex(value, 32)
    return value


def addr(value):
    s.nonzero(value, 20)
    return value


def read_word(data, offset):
    s.require(offset >= 0 and offset % 32 == 0 and offset + 32 <= len(data), 'malformed_gate_calldata')
    return int.from_bytes(data[offset:offset + 32], 'big')


def decode_static(data, fields):
    s.require(len(data) == 32 * len(fields), 'malformed_gate_calldata')
    value = {}
    for i, (name, kind) in enumerate(fields):
        raw = data[i * 32:(i + 1) * 32]
        if kind == 'address':
            s.require(raw[:12] == bytes(12), 'noncanonical_gate_address')
            value[name] = '0x' + raw[12:].hex()
        elif kind == 'bytes32':
            value[name] = '0x' + raw.hex()
        else:
            bits = int(kind[4:])
            number = int.from_bytes(raw, 'big')
            s.require(number < 2**bits, 'noncanonical_gate_integer')
            value[name] = hex(number) if bits == 256 else number
    s.require(s.encode_fields(fields, value) == data, 'noncanonical_gate_fields')
    return value


def decode_gate_calldata(raw):
    """Recover report preimages from a direct canonical proof-gate transaction."""
    data = s.raw_hex(raw)
    p, d, o = relay.abi_tuple(s.PACKAGE), relay.abi_tuple(s.DUTY + [('operatorSignature', 'bytes')]), relay.abi_tuple(s.OUTPUT)
    bootstrap_sig = f'submitBootstrap({p},{d}[],{o}[],bytes,bytes)'
    service_sig = f'submit({p},{d}[],{o}[],bytes,{relay.abi_tuple(s.CANDIDATE)},bytes32[],bytes,bytes)'
    s.require(len(data) >= 4 + 32 * (len(s.PACKAGE) + 4), 'malformed_gate_calldata')
    selector = data[:4]
    s.require(selector in (relay.selector(bootstrap_sig), relay.selector(service_sig)), 'unknown_gate_submission')
    bootstrap = selector == relay.selector(bootstrap_sig)
    args = data[4:]
    accepted = decode_static(args[:32 * len(s.PACKAGE)], s.PACKAGE)
    offset = read_word(args, 32 * len(s.PACKAGE))
    minimum = 32 * (len(s.PACKAGE) + (4 if bootstrap else 10))
    s.require(offset >= minimum and offset % 32 == 0 and offset + 32 <= len(args), 'malformed_gate_duties')
    count = read_word(args, offset)
    s.require(count <= 100 and offset + 32 * (1 + count) <= len(args), 'malformed_gate_duties')
    base = offset + 32
    duties = []
    encoded = []
    expected_offset = count * 32
    for i in range(count):
        relative = read_word(args, base + 32 * i)
        s.require(relative == expected_offset, 'noncanonical_gate_duties')
        start = base + relative
        static_size = len(s.DUTY) * 32
        s.require(start + static_size + 64 <= len(args), 'malformed_gate_duty')
        duty = decode_static(args[start:start + static_size], s.DUTY)
        sig_offset = read_word(args, start + static_size)
        s.require(sig_offset == static_size + 32, 'noncanonical_gate_duty')
        sig_len = read_word(args, start + sig_offset)
        s.require(0 < sig_len <= 4096, 'invalid_duty_signature')
        size = static_size + 32 + 32 + ((sig_len + 31) // 32) * 32
        s.require(start + size <= len(args), 'malformed_gate_duty')
        duty['operatorSignature'] = '0x' + args[start + sig_offset + 32:start + sig_offset + 32 + sig_len].hex()
        item = s.encode_duty(duty)
        s.require(item == args[start:start + size], 'noncanonical_gate_duty')
        duties.append(duty)
        encoded.append(item)
        expected_offset += size
    s.require(s.report_hash(duties) == accepted['reportHash'], 'duty_report_hash_mismatch')
    return accepted, duties, bootstrap


def sidecar(artifact_roots, package_hash):
    found = []
    for root in artifact_roots:
        root = io.directory(root)
        path = root / (package_hash[2:] + '.json')
        if not path.exists() and not path.is_symlink():
            continue
        item = s.read_json(path, private=True)
        s.require(type(item) is dict and item.get('package_hash') == package_hash
                  and type(item.get('sidecar')) is dict, 'invalid_relay_artifact')
        accepted, duties = item['sidecar'].get('accepted_package'), item['sidecar'].get('duties')
        bootstrap = item.get('mode') == 'bootstrap'
        s.require(item.get('mode') in ('bootstrap', 'service') and s.struct_hash('AcceptedPackageV1', s.PACKAGE, accepted) == package_hash
                  and s.report_hash(duties) == accepted['reportHash'], 'relay_artifact_mismatch')
        found.append((accepted, duties, bootstrap))
    s.require(not found or all(v == found[0] for v in found), 'conflicting_relay_artifacts')
    return found[0] if found else None


def canonical_block(rpc, number, block_hash):
    block = rpc.call('eth_getBlockByNumber', [number, False])
    s.require(type(block) is dict and block.get('hash') == block_hash and block.get('number') == number,
              'source_block_changed')


def receipt_for(rpc, event, gate):
    s.require(type(event) is dict and event.get('address') == gate and event.get('removed', False) is False,
              'invalid_source_event')
    hex32(event['transactionHash']); hex32(event['blockHash'])
    q(event['blockNumber']); q(event['logIndex']); q(event['transactionIndex'])
    receipt = rpc.call('eth_getTransactionReceipt', [event['transactionHash']])
    s.require(type(receipt) is dict and receipt.get('transactionHash') == event['transactionHash']
              and receipt.get('blockHash') == event['blockHash'] and receipt.get('blockNumber') == event['blockNumber']
              and q(receipt['transactionIndex']) == q(event['transactionIndex']) and q(receipt['status']) == 1,
              'source_receipt_mismatch')
    matches = [log for log in receipt.get('logs', []) if log.get('address') == gate
               and log.get('logIndex') == event['logIndex'] and log.get('topics') == event['topics']
               and log.get('data') == event['data'] and not log.get('removed', False)]
    s.require(len(matches) == 1, 'source_event_not_in_receipt')
    canonical_block(rpc, event['blockNumber'], event['blockHash'])
    return receipt


def block_receipts(rpc, number):
    """Match every returned receipt to the canonical block's transaction inventory."""
    block = rpc.call('eth_getBlockByNumber', [number, False])
    s.require(type(block) is dict and block.get('number') == number, 'source_block_unavailable')
    block_hash = hex32(block['hash'])
    txs = block.get('transactions')
    s.require(type(txs) is list and all(isinstance(tx, str) for tx in txs), 'source_transactions_unavailable')
    for tx in txs:
        hex32(tx)
    s.require(len(txs) == len(set(txs)), 'duplicate_source_transaction')
    try:
        receipts = rpc.call('eth_getBlockReceipts', [number])
    except relay.RpcError as error:
        if error.code != -32601:
            raise
        receipts = [rpc.call('eth_getTransactionReceipt', [tx]) for tx in txs]
    except s.Error as error:
        if str(error) != 'rpc_response_limit':
            raise
        receipts = [rpc.call('eth_getTransactionReceipt', [tx]) for tx in txs]
    s.require(type(receipts) is list and len(receipts) == len(txs), 'incomplete_source_receipts')
    for index, (tx, receipt) in enumerate(zip(txs, receipts)):
        s.require(type(receipt) is dict and receipt.get('transactionHash') == tx
                  and receipt.get('blockHash') == block_hash and receipt.get('blockNumber') == number
                  and q(receipt.get('transactionIndex')) == index and q(receipt.get('status')) in (0, 1)
                  and type(receipt.get('logs')) is list, 'source_block_receipt_mismatch')
    canonical_block(rpc, number, block_hash)
    return receipts


def messenger_index(receipt, gate, payload):
    logs = receipt.get('l2ToL1Logs')
    s.require(type(logs) is list, 'gateway_messages_unavailable')
    candidates = []
    for index, log in enumerate(logs):
        if (log.get('sender') == MESSENGER and log.get('key') == '0x' + '00' * 12 + gate[2:]
                and log.get('value') == s.keccak(payload)):
            s.require(q(log['transactionLogIndex']) == index and q(log['transactionIndex']) == q(receipt['transactionIndex'])
                      and q(log['shardId']) == 0 and log.get('isService') is True, 'gateway_message_metadata_mismatch')
            candidates.append(index)
    s.require(len(candidates) <= 1, 'ambiguous_gateway_message')
    return candidates[0] if candidates else None


def sink_published(receipt, sink, payload):
    expected = [PUBLISHED, s.keccak(payload)]
    matches = [log for log in receipt.get('logs', []) if log.get('address') == sink
               and log.get('topics') == expected and log.get('data') == '0x' and not log.get('removed', False)]
    s.require(len(matches) <= 1, 'ambiguous_sink_message')
    return bool(matches)


def discover_one(rpc, gate, sink, event, from_root, cfg, artifact_roots, anchor):
    topics = event.get('topics')
    s.require(type(topics) is list and topics, 'invalid_source_topics')
    receipt = receipt_for(rpc, event, gate)
    txhash = event['transactionHash']
    common = {'fromRoot': from_root, 'sourceTransactionHash': txhash, 'sourceBlockHash': event['blockHash'],
              'sourceBlockNumber': event['blockNumber'], 'sourceLogIndex': event['logIndex'],
              'transactionIndex': q(event['transactionIndex'])}
    if topics[0] == ACCEPTED:
        s.require(len(topics) == 4 and all(isinstance(v, str) for v in topics), 'invalid_acceptance_event')
        package_hash = hex32(topics[1]); from_batch = int.from_bytes(s.raw_hex(topics[2], 32), 'big')
        to_batch = int.from_bytes(s.raw_hex(topics[3], 32), 'big')
        raw_bootstrap = s.raw_hex(event['data'], 32)
        bootstrap_number = int.from_bytes(raw_bootstrap, 'big')
        s.require(bootstrap_number in (0, 1), 'invalid_bootstrap_flag')
        bootstrap = bool(bootstrap_number)
        control = any(log.get('address') == gate and log.get('topics') == [CONTROL, package_hash]
                      and log.get('data') == '0x' and not log.get('removed', False) for log in receipt.get('logs', []))
        payload = s.raw_hex(s.keccak(b'ZKSYS_ACCEPTED_SERVICE_V1')) + s.raw_hex(package_hash) + s.word(int(bootstrap))
        present = sink_published(receipt, sink, payload) if from_root else messenger_index(receipt, gate, payload)
        if control:
            s.require(present is False if from_root else present is None, 'control_message_unexpected')
            return None
        s.require(present is True if from_root else present is not None, 'acceptance_message_missing')
        value = sidecar(artifact_roots, package_hash)
        if value is None:
            tx = rpc.call('eth_getTransactionByHash', [txhash])
            if tx is not None:
                s.require(type(tx) is dict and tx.get('hash') == txhash
                          and tx.get('blockHash') == event['blockHash']
                          and tx.get('blockNumber') == event['blockNumber'], 'source_transaction_mismatch')
            if tx is None or tx.get('to') != gate:
                identity = ('root:' if from_root else 'child:') + s.keccak(payload)[2:]
                return {**common, 'id': identity, 'kind': 'unresolved', 'bootstrap': bootstrap,
                        'packageHash': package_hash, 'batchFrom': from_batch, 'batchTo': to_batch,
                        'transactionLogIndex': None if from_root else present}
            value = decode_gate_calldata(tx['input'])
        accepted, duties, artifact_bootstrap = value
        expected_chain = cfg['gateway_chain_id'] if from_root else cfg['zksys_chain_id']
        expected_address = cfg['gateway_chain_address'] if from_root else cfg['child_chain_address']
        s.require(s.struct_hash('AcceptedPackageV1', s.PACKAGE, accepted) == package_hash
                  and s.uint(accepted['chainId']) == expected_chain and accepted['chainAddress'] == expected_address
                  and accepted['batchFrom'] == from_batch and accepted['batchTo'] == to_batch
                  and artifact_bootstrap == bootstrap and s.report_hash(duties) == accepted['reportHash'],
                  'accepted_report_identity_mismatch')
        identity = ('root:' if from_root else 'child:') + s.keccak(payload)[2:]
        return {**common, 'id': identity, 'kind': 'accepted', 'bootstrap': bootstrap,
                'accepted': accepted, 'duties': duties, 'transactionLogIndex': None if from_root else present}
    s.require(topics[0] == ACTIVATED and len(topics) == 2 and len(s.raw_hex(topics[1], 32)) == 32,
              'invalid_activation_event')
    root = '0x' + s.raw_hex(event['data'], 32).hex()
    s.nonzero(root)
    vk_hash = relay.call_word(rpc, gate, 'productionVkHash()', anchor, kind='bytes32')
    s.nonzero(vk_hash)
    payload = s.raw_hex(s.keccak(b'ZKSYS_SERVICE_ACTIVATION_V1')) + s.raw_hex(vk_hash) + s.raw_hex(root)
    present = sink_published(receipt, sink, payload) if from_root else messenger_index(receipt, gate, payload)
    s.require(present is True if from_root else present is not None, 'activation_message_missing')
    identity = ('root:' if from_root else 'child:') + s.keccak(payload)[2:]
    return {**common, 'id': identity, 'kind': 'activation', 'bootstrap': False, 'vk_hash': vk_hash,
            'roster_root': root, 'transactionLogIndex': None if from_root else present}


def discover(gateway_rpc, l1_rpc, cfg, cursor, artifact_roots):
    """Scan both finalized source ranges. Persist results and next cursor atomically."""
    s.exact(cursor, ('gateway', 'l1'))
    s.exact(artifact_roots, ('child', 'root'))
    s.require(type(cfg['scan_blocks']) is int and cfg['scan_blocks'] > 0, 'invalid_scan_blocks')
    reports, next_cursor = [], {}
    for name, rpc, gate, chain_id, from_root in (
            ('gateway', gateway_rpc, addr(cfg['gateway_gate']), cfg['gateway_chain_id'], False),
            ('l1', l1_rpc, addr(cfg['root_gate']), cfg['l1_chain_id'], True)):
        s.require(type(cursor[name]) is int and cursor[name] >= 0, 'invalid_discovery_cursor')
        s.require(q(rpc.call('eth_chainId', [])) == chain_id, 'source_chain_changed')
        head = rpc.call('eth_getBlockByNumber', ['finalized', False])
        s.require(type(head) is dict, 'source_finality_unavailable')
        height, block_hash = q(head['number']), hex32(head['hash'])
        s.require(cursor[name] <= height + 1, 'source_cursor_ahead_of_finality')
        anchor = {'blockHash': block_hash, 'requireCanonical': True}
        end = min(height, cursor[name] + cfg['scan_blocks'] - 1)
        if cursor[name] <= end:
            seen = set()
            previous = None
            for block in range(cursor[name], end + 1):
                for receipt in block_receipts(rpc, hex(block)):
                    for event in receipt['logs']:
                        s.require(type(event) is dict, 'invalid_source_log')
                        topics = event.get('topics')
                        if event.get('address') != gate or type(topics) is not list or not topics or topics[0] not in (ACCEPTED, ACTIVATED):
                            continue
                        s.require(event.get('transactionHash') == receipt['transactionHash']
                                  and event.get('blockHash') == receipt['blockHash']
                                  and event.get('blockNumber') == receipt['blockNumber']
                                  and q(event.get('transactionIndex')) == q(receipt['transactionIndex']),
                                  'source_event_receipt_mismatch')
                        position = (block, q(event['transactionIndex']), q(event['logIndex']))
                        s.require((previous is None or previous < position)
                                  and (event['transactionHash'], event['logIndex']) not in seen,
                                  'invalid_source_log_order')
                        previous = position; seen.add((event['transactionHash'], event['logIndex']))
                        result = discover_one(rpc, gate, addr(cfg['root_sink']), event, from_root, cfg,
                                              [artifact_roots['root' if from_root else 'child']], anchor)
                        if result is not None:
                            reports.append(result)
        canonical_block(rpc, head['number'], block_hash)
        s.require(q(rpc.call('eth_chainId', [])) == chain_id, 'source_chain_changed')
        next_cursor[name] = end + 1
    return next_cursor, reports


def hydrate(rpc, cfg, unresolved, artifact_root):
    """Resolve a durable placeholder without trusting its retained source data."""
    s.require(type(unresolved) is dict and unresolved.get('kind') == 'unresolved'
              and type(unresolved.get('fromRoot')) is bool and type(unresolved.get('bootstrap')) is bool,
              'invalid_unresolved_receipt')
    from_root = unresolved['fromRoot']
    gate = addr(cfg['root_gate'] if from_root else cfg['gateway_gate'])
    chain_id = cfg['l1_chain_id'] if from_root else cfg['gateway_chain_id']
    s.require(q(rpc.call('eth_chainId', [])) == chain_id, 'source_chain_changed')
    head = rpc.call('eth_getBlockByNumber', ['finalized', False])
    s.require(type(head) is dict and q(head['number']) >= q(unresolved['sourceBlockNumber']),
              'unresolved_source_not_finalized')
    hex32(head['hash']); canonical_block(rpc, unresolved['sourceBlockNumber'], unresolved['sourceBlockHash'])
    txhash = hex32(unresolved['sourceTransactionHash'])
    receipt = rpc.call('eth_getTransactionReceipt', [txhash])
    s.require(type(receipt) is dict and type(receipt.get('logs')) is list, 'unresolved_receipt_unavailable')
    matches = [log for log in receipt['logs'] if type(log) is dict and log.get('address') == gate
               and log.get('logIndex') == unresolved['sourceLogIndex']
               and type(log.get('topics')) is list and len(log['topics']) == 4
               and log['topics'][0] == ACCEPTED and log['topics'][1] == unresolved['packageHash']]
    s.require(len(matches) == 1, 'unresolved_event_missing')
    event = matches[0]
    result = discover_one(rpc, gate, addr(cfg['root_sink']), event, from_root, cfg, [artifact_root],
                          {'blockHash': head['hash'], 'requireCanonical': True})
    s.require(result is not None and all(result[k] == unresolved[k] for k in
              ('id', 'fromRoot', 'bootstrap', 'sourceTransactionHash', 'sourceBlockHash', 'sourceBlockNumber',
               'sourceLogIndex', 'transactionIndex', 'transactionLogIndex')), 'unresolved_identity_changed')
    s.require((result['batchFrom'] if result['kind'] == 'unresolved' else result['accepted']['batchFrom'])
              == unresolved['batchFrom']
              and (result['batchTo'] if result['kind'] == 'unresolved' else result['accepted']['batchTo'])
              == unresolved['batchTo'], 'unresolved_range_changed')
    canonical_block(rpc, head['number'], head['hash'])
    return None if result['kind'] == 'unresolved' else result
