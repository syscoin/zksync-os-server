#!/usr/bin/env python3
"""Relay finalized Gateway batch roots to zkSYS using one durable NEVM outbox.

The root is read by the sink from canonical MessageRoot on NEVM; no relayer-
provided root or duty evidence is sent in the Bridgehub transaction.
"""
import argparse
import fcntl
import hashlib
import json
import os
from pathlib import Path
import stat
import sys
import time

import bootstrap_enrollment
import keeper
import relay
import receipt_storage as storage
import service as s
import workflow_io as io
from service_transaction_driver import ServiceTransactionDriver

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from zksys_keeper_cast import CastSigner, hash_bytes
from zksys_keeper_transactions import FinalizedRevert, TransactionError, _same_transaction, finalized_receipt, priority_payload

PRIORITY_EVENT = '0x4531cd5795773d7101c17bdeb9f5ab7f47d7056017506f937083be5d6e77a382'
ROOT_EVENT = s.keccak(b'GatewayBatchRootRelayed(uint256,bytes32,bytes32)')
RELAY_EVENT = s.keccak(b'MessageRelayed(bytes32,bytes32)')
BUNDLE_EVENT = s.keccak(b'GatewayBatchRootMessageRelayed(uint256,bytes32,bytes32)')
IMPLEMENTATION_SLOT = '0x360894a13ba1a3210667c828492db98dca3e2076cc3735a920a3ca505d382bbc'


def aword(address): return bytes(12) + s.raw_hex(address, 20)


def call(signature, values):
    return '0x' + (relay.selector(signature) + b''.join(values)).hex()


def alias(address):
    return '0x' + format((int(address, 16) + int('1111000000000000000000000000000000001111', 16)) % (1 << 160), '040x')


def configuration(cfg):
    fields = ('schema_version', 'l1_rpc_url', 'zksys_rpc_url', 'rpc_timeout_seconds', 'l1_chain_id',
              'zksys_chain_id', 'gateway_chain_id', 'l1_genesis', 'zksys_genesis', 'root_sink', 'bridgehub',
              'message_root', 'gateway_diamond', 'zksys_diamond', 'receiver', 'signer', 'refund',
              'bridgehub_implementation', 'message_root_implementation', 'gateway_executed_facet', 'priority_facet',
              'root_sink_code_hash', 'bridgehub_code_hash', 'message_root_code_hash', 'gateway_diamond_code_hash',
              'zksys_diamond_code_hash', 'receiver_code_hash', 'bridgehub_implementation_code_hash',
              'message_root_implementation_code_hash', 'gateway_executed_facet_code_hash', 'priority_facet_code_hash',
              'keystore', 'password_file', 'requests_file', 'requests_identity', 'signer_lock_dir', 'l1_gas_limit', 'l1_gas_price',
              'priority_gas_limit', 'gas_per_pubdata', 'poll_seconds')
    s.exact(cfg, fields)
    s.require(cfg['schema_version'] == 1, 'unsupported_root_relayer_config')
    for key in ('l1_chain_id', 'zksys_chain_id', 'gateway_chain_id', 'rpc_timeout_seconds',
                'l1_gas_limit', 'l1_gas_price', 'priority_gas_limit', 'gas_per_pubdata', 'poll_seconds'):
        s.require(type(cfg[key]) is int and cfg[key] > 0, 'invalid_root_relayer_number')
    s.require(len({cfg['l1_chain_id'], cfg['zksys_chain_id'], cfg['gateway_chain_id']}) == 3
              and cfg['rpc_timeout_seconds'] <= 30 and cfg['poll_seconds'] <= 3600, 'invalid_root_relayer_timing')
    for key in ('root_sink', 'bridgehub', 'message_root', 'gateway_diamond', 'zksys_diamond', 'receiver', 'signer',
                'refund', 'bridgehub_implementation', 'message_root_implementation', 'gateway_executed_facet', 'priority_facet'):
        s.nonzero(cfg[key], 20)
    for key in ('l1_genesis', 'zksys_genesis', 'root_sink_code_hash', 'bridgehub_code_hash', 'message_root_code_hash',
                'gateway_diamond_code_hash', 'zksys_diamond_code_hash', 'receiver_code_hash',
                'bridgehub_implementation_code_hash', 'message_root_implementation_code_hash',
                'gateway_executed_facet_code_hash', 'priority_facet_code_hash', 'requests_identity'):
        s.nonzero(cfg[key])
    for key in ('keystore', 'password_file', 'requests_file', 'signer_lock_dir'):
        s.require(type(cfg[key]) is str and Path(cfg[key]).is_absolute(), 'absolute_root_signer_path_required')
    for key in ('l1_rpc_url', 'zksys_rpc_url'):
        relay.Rpc(cfg[key], timeout=cfg['rpc_timeout_seconds'])
    return cfg


class Hooks:
    def __init__(self, cfg, l1, child, signer): self.cfg, self.l1, self.child, self.signer = cfg, l1, child, signer

    def head(self, rpc, chain, genesis):
        s.require(s.uint(rpc.call('eth_chainId', [])) == chain, 'root_relayer_chain_changed')
        first = rpc.call('eth_getBlockByNumber', ['0x0', False])
        s.require(type(first) is dict and first.get('hash') == genesis, 'root_relayer_genesis_changed')
        head = rpc.call('eth_getBlockByNumber', ['finalized', False])
        s.require(type(head) is dict, 'root_relayer_finality_missing')
        s.nonzero(head.get('hash')); s.uint(head.get('number'))
        return head, {'blockHash': head['hash'], 'requireCanonical': True}

    def recheck(self, rpc, head):
        again = rpc.call('eth_getBlockByNumber', [head['number'], False])
        s.require(type(again) is dict and again.get('hash') == head['hash'], 'root_relayer_anchor_changed')

    def verify(self):
        c = self.cfg
        l1_head, l1 = self.head(self.l1, c['l1_chain_id'], c['l1_genesis'])
        child_head, child = self.head(self.child, c['zksys_chain_id'], c['zksys_genesis'])
        for name in ('root_sink', 'bridgehub', 'message_root', 'gateway_diamond', 'zksys_diamond',
                     'bridgehub_implementation', 'message_root_implementation', 'gateway_executed_facet', 'priority_facet'):
            keeper.check_code(self.l1, c[name], c[name + '_code_hash'], l1, 'root_relayer_l1_code_changed')
        keeper.check_code(self.child, c['receiver'], c['receiver_code_hash'], child, 'root_relayer_receiver_code_changed')
        view = lambda rpc, address, sig, args=b'', kind='uint', anchor=l1: relay.call_word(rpc, address, sig, anchor, args, kind)
        for proxy, impl in (('bridgehub', 'bridgehub_implementation'), ('message_root', 'message_root_implementation')):
            raw = self.l1.call('eth_getStorageAt', [c[proxy], IMPLEMENTATION_SLOT, l1])
            s.require(s.raw_hex(raw, 32) == aword(c[impl]), 'root_relayer_implementation_changed')
        for diamond, signature, facet in (
            ('gateway_diamond', 'getTotalBatchesExecuted()', 'gateway_executed_facet'),
            ('zksys_diamond', 'bridgehubRequestL2Transaction((address,address,uint256,uint256,bytes,uint256,uint256,bytes[],address))', 'priority_facet')):
            selector = relay.selector(signature)
            s.require(view(self.l1, c[diamond], 'facetAddress(bytes4)', selector + bytes(28), 'address') == c[facet],
                      'root_relayer_facet_changed')
        s.require(view(self.l1, c['bridgehub'], 'messageRoot()', kind='address') == c['message_root']
                  and view(self.l1, c['bridgehub'], 'getZKChain(uint256)', s.word(c['gateway_chain_id']), 'address') == c['gateway_diamond']
                  and view(self.l1, c['bridgehub'], 'getZKChain(uint256)', s.word(c['zksys_chain_id']), 'address') == c['zksys_diamond']
                  and view(self.l1, c['bridgehub'], 'settlementLayer(uint256)', s.word(c['zksys_chain_id'])) == c['gateway_chain_id'],
                  'root_relayer_topology_changed')
        s.require(view(self.l1, c['root_sink'], 'bridgehub()', kind='address') == c['bridgehub']
                  and view(self.l1, c['root_sink'], 'gatewayChainAddress()', kind='address') == c['gateway_diamond']
                  and view(self.l1, c['root_sink'], 'registryReceiver()', kind='address') == c['receiver']
                  and view(self.l1, c['root_sink'], 'gatewayChainId()') == c['gateway_chain_id']
                  and view(self.l1, c['root_sink'], 'registryChainId()') == c['zksys_chain_id']
                  and view(self.child, c['receiver'], 'rootMessageSink()', kind='address', anchor=child) == c['root_sink'],
                  'root_relayer_sink_binding_changed')
        self.recheck(self.l1, l1_head); self.recheck(self.child, child_head)

    def snapshot(self, batch, message_hash=s.ZERO):
        c = self.cfg
        l1_head, l1 = self.head(self.l1, c['l1_chain_id'], c['l1_genesis'])
        child_head, child = self.head(self.child, c['zksys_chain_id'], c['zksys_genesis'])
        executed = relay.call_word(self.l1, c['gateway_diamond'], 'getTotalBatchesExecuted()', l1)
        if executed < batch:
            self.recheck(self.l1, l1_head); self.recheck(self.child, child_head)
            return None
        root = relay.call_word(self.l1, c['message_root'], 'chainBatchRoots(uint256,uint256)', l1,
                               s.word(c['gateway_chain_id']) + s.word(batch), 'bytes32')
        s.nonzero(root)
        imported = relay.call_word(self.child, c['receiver'], 'gatewayBatchRoots(uint256)', child,
                                   s.word(batch), 'bytes32')
        s.require(imported in (s.ZERO, root), 'gateway_root_conflict')
        authorized = (message_hash == s.ZERO or relay.call_word(
            self.child, c['receiver'], 'authorizedRootMessages(bytes32)', child, s.raw_hex(message_hash, 32)) == 1)
        if message_hash != s.ZERO:
            s.require(relay.call_word(self.l1, c['root_sink'], 'messages(bytes32)', l1,
                                      s.raw_hex(message_hash, 32)) == 1, 'root_message_not_published')
        self.recheck(self.l1, l1_head); self.recheck(self.child, child_head)
        return {'root': root, 'imported': imported, 'authorized': authorized, 'l1': l1, 'child': child}

    def prepare(self, batch, message_hash, snapshot):
        c = self.cfg
        if message_hash == s.ZERO:
            data = call('relayGatewayBatchRoot(uint256,uint256,uint256,address)',
                        [s.word(batch), s.word(c['priority_gas_limit']), s.word(c['gas_per_pubdata']), aword(c['refund'])])
            child_data = call('receiveGatewayBatchRoot(uint256,bytes32)', [s.word(batch), s.raw_hex(snapshot['root'], 32)])
        else:
            data = call('relayGatewayBatchRootWithMessage(uint256,bytes32,uint256,uint256,address)',
                        [s.word(batch), s.raw_hex(message_hash, 32), s.word(c['priority_gas_limit']),
                         s.word(c['gas_per_pubdata']), aword(c['refund'])])
            child_data = call('receiveGatewayBatchRootWithMessage(uint256,bytes32,bytes32)',
                              [s.word(batch), s.raw_hex(snapshot['root'], 32), s.raw_hex(message_hash, 32)])
        fee = relay.call_word(self.l1, c['bridgehub'], 'l2TransactionBaseCost(uint256,uint256,uint256,uint256)',
                              snapshot['l1'], s.word(c['zksys_chain_id']) + s.word(c['l1_gas_price'])
                              + s.word(c['priority_gas_limit']) + s.word(c['gas_per_pubdata']))
        s.require(fee > 0, 'root_relayer_fee_quote_invalid')
        result = self.child.call('eth_call', [{'from': alias(c['root_sink']), 'to': c['receiver'], 'data': child_data,
                                               'gas': hex(c['priority_gas_limit'])}, snapshot['child']])
        s.require(result == '0x', 'root_receiver_simulation_changed')
        self.l1.call('eth_call', [{'from': c['signer'], 'to': c['root_sink'], 'data': data,
                                  'gas': hex(c['l1_gas_limit']), 'gasPrice': hex(c['l1_gas_price']), 'value': hex(fee)}, snapshot['l1']])
        request = dict(chain_id=c['l1_chain_id'], sender=c['signer'], to=c['root_sink'], data=data,
                       value=fee, gas=c['l1_gas_limit'], gas_price=c['l1_gas_price'])
        return request, {'batch': batch, 'messageHash': message_hash, 'root': snapshot['root'], 'childData': child_data, 'request': request}

    def verify_journal(self, entry):
        c, meta = self.cfg, entry['metadata']
        batch, root, message_hash, request = meta['batch'], meta['root'], meta['messageHash'], meta['request']
        s.raw_hex(message_hash, 32)
        if message_hash == s.ZERO:
            expected_data = call('relayGatewayBatchRoot(uint256,uint256,uint256,address)',
                                 [s.word(batch), s.word(c['priority_gas_limit']), s.word(c['gas_per_pubdata']), aword(c['refund'])])
            expected_child = call('receiveGatewayBatchRoot(uint256,bytes32)', [s.word(batch), s.raw_hex(root, 32)])
        else:
            expected_data = call('relayGatewayBatchRootWithMessage(uint256,bytes32,uint256,uint256,address)',
                                 [s.word(batch), s.raw_hex(message_hash, 32), s.word(c['priority_gas_limit']),
                                  s.word(c['gas_per_pubdata']), aword(c['refund'])])
            expected_child = call('receiveGatewayBatchRootWithMessage(uint256,bytes32,bytes32)',
                                  [s.word(batch), s.raw_hex(root, 32), s.raw_hex(message_hash, 32)])
        expected = {'from': c['signer'], 'to': c['root_sink'], 'chainId': c['l1_chain_id'], 'nonce': entry['nonce'],
                    'gas': c['l1_gas_limit'], 'gasPrice': c['l1_gas_price'], 'value': request['value'], 'data': expected_data}
        s.require(entry['kind'] == 'delivery' and entry['chainId'] == c['l1_chain_id'] and entry['sender'] == c['signer']
                  and request['data'] == expected_data and meta['childData'] == expected_child
                  and self.signer.hash_raw(entry['raw']) == entry['txHash']
                  and _same_transaction(expected, self.signer.decode(entry['raw'])), 'root_journal_transaction_changed')

    def priority_hash(self, receipt, entry):
        c, meta = self.cfg, entry['metadata']
        commitment = s.keccak(s.word(c['gateway_chain_id']) + s.word(meta['batch']) + s.raw_hex(meta['root'], 32) + s.raw_hex(meta['messageHash'], 32))
        matches = [log for log in receipt.get('logs', []) if not log.get('removed')
                   and log.get('address', '').lower() == c['root_sink'] and type(log.get('topics')) is list
                   and len(log['topics']) == 4 and log['topics'][0].lower() == ROOT_EVENT
                   and log['topics'][1].lower() == '0x' + s.word(meta['batch']).hex()
                   and log['topics'][2].lower() == meta['root']]
        s.require(len(matches) == 1, 'root_relay_event_missing')
        digest = matches[0]['topics'][3].lower(); s.raw_hex(digest, 32)
        other = [log for log in receipt.get('logs', []) if not log.get('removed')
                 and log.get('address', '').lower() == c['root_sink'] and log.get('topics') == [RELAY_EVENT, commitment, digest]]
        s.require(len(other) == 1, 'root_message_event_missing')
        if meta['messageHash'] != s.ZERO:
            bundled = [log for log in receipt.get('logs', []) if not log.get('removed')
                       and log.get('address', '').lower() == c['root_sink']
                       and log.get('topics') == [BUNDLE_EVENT, '0x' + s.word(meta['batch']).hex(), meta['messageHash'], digest]]
            s.require(len(bundled) == 1, 'root_bundle_event_missing')
        payload = priority_payload(receipt, c['zksys_diamond'], PRIORITY_EVENT, digest, hash_bytes)
        s.require(payload['transactionType'] == 127 and payload['from'] == alias(c['root_sink'])
                  and payload['to'] == c['receiver'] and payload['input'] == meta['childData'], 'root_priority_payload_changed')
        return digest

    def received(self, receipt, batch, root, message_hash):
        c = self.cfg
        anchor = {'blockHash': receipt['blockHash'], 'requireCanonical': True}
        return (relay.call_word(self.child, c['receiver'], 'gatewayBatchRoots(uint256)', anchor,
                                s.word(batch), 'bytes32') == root
                and (message_hash == s.ZERO or relay.call_word(self.child, c['receiver'],
                     'authorizedRootMessages(bytes32)', anchor, s.raw_hex(message_hash, 32)) == 1))


def step(cfg, journal, save, driver, hooks, now):
    hooks.verify()
    entries = journal['transactions']
    s.require(not set(entries) - {'root'}, 'unexpected_root_transaction')
    entry = entries.get('root')
    if entry:
        hooks.verify_journal(entry)
        meta = entry['metadata']
        s.require(type(meta['batch']) is int and meta['batch'] > 0, 'root_batch_changed')
        try:
            receipt = driver.receipt('root', hooks.l1.call)
        except FinalizedRevert as error:
            journal['history'].append({'batch': meta['batch'], 'status': 'l1_reverted', 'txHash': entry['txHash'],
                                       'blockHash': error.receipt['blockHash']})
            del entries['root']
            journal['nextRetryAt'] = now + 60
            save(journal)
            return 'l1_reverted_retry_scheduled'
        if receipt is None:
            driver.rebroadcast_if_missing('root', hooks.l1.call)
            return 'l1_pending'
        digest = hooks.priority_hash(receipt, entry)
        s.require(entry.get('canonicalHash', digest) == digest, 'root_priority_hash_changed')
        if 'canonicalHash' not in entry:
            entry['canonicalHash'] = digest
            save(journal)
        s.require(s.uint(hooks.child.call('eth_chainId', [])) == cfg['zksys_chain_id'], 'root_destination_chain_changed')
        child = finalized_receipt(hooks.child.call, digest)
        if child is None: return 'l2_pending'
        status = s.uint(child['status'])
        s.require(status in (0, 1), 'root_invalid_destination_status')
        if status == 0:
            snapshot = hooks.snapshot(meta['batch'], meta['messageHash'])
            if snapshot is None or (snapshot['imported'] != meta['root'] or not snapshot['authorized']):
                journal['history'].append({'batch': meta['batch'], 'status': 'l2_reverted', 'txHash': entry['txHash'],
                                           'canonicalHash': digest, 'blockHash': child['blockHash']})
                del entries['root']
                journal['nextRetryAt'] = now + 60
                save(journal)
                return 'l2_reverted_retry_scheduled'
        else:
            s.require(hooks.received(child, meta['batch'], meta['root'], meta['messageHash']), 'root_delivery_state_mismatch')
        journal['history'].append({'batch': meta['batch'], 'status': 'delivered', 'txHash': entry['txHash'],
                                   'canonicalHash': digest, 'blockHash': child['blockHash']})
        item = {'batch': meta['batch'], 'messageHash': meta['messageHash']}
        journal['completed'][storage.completed_key(item)] = item
        journal['nextRetryAt'] = 0
        del entries['root']
        save(journal)
        return 'delivered'
    if now < journal['nextRetryAt']: return 'retry_wait'
    inbox_path = Path(cfg['requests_file'])
    if not inbox_path.exists() and not inbox_path.is_symlink(): return 'awaiting_request_inbox'
    requests = []
    seen = set()
    with storage.request_snapshot(inbox_path, cfg['requests_identity'][2:]) as (items, _):
        for item in items:
            key = storage.completed_key(item)
            if key not in journal['completed'] and key not in seen:
                requests.append(item)
                seen.add(key)
    requests.sort(key=lambda item: (item['batch'], item['messageHash']))
    if not requests: return 'no_requested_roots'
    for item in requests:
        batch, message_hash = item['batch'], item['messageHash']
        snapshot = hooks.snapshot(batch, message_hash)
        if snapshot is None: continue
        if snapshot['imported'] == snapshot['root'] and snapshot['authorized']:
            journal['history'].append({'batch': batch, 'messageHash': message_hash, 'status': 'already_delivered'})
            journal['completed'][storage.completed_key(item)] = item
            save(journal)
            return 'already_delivered'
        request, metadata = hooks.prepare(batch, message_hash, snapshot)
        break
    else:
        return 'awaiting_gateway_execution'
    driver.begin('root', hooks.l1.call, kind='delivery', metadata=metadata, **request)
    return 'submitted'


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--config', required=True)
    parser.add_argument('--state-dir', required=True)
    parser.add_argument('--execute', action='store_true')
    parser.add_argument('--once', action='store_true')
    args = parser.parse_args()
    cfg = configuration(io.private_json(args.config))
    if not args.execute:
        print(json.dumps({'status': 'configuration_validated', 'broadcast': False})); return
    root = io.directory(args.state_dir)
    lock = os.open(root / 'gateway-roots.lock', os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW, 0o600)
    signer_lock = None
    try:
        signer_dir = io.directory(cfg['signer_lock_dir'])
        signer_lock = os.open(signer_dir / ('l1-' + str(cfg['l1_chain_id']) + '-' + cfg['signer'][2:] + '.lock'),
                              os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW, 0o600)
        info = os.fstat(signer_lock)
        s.require(stat.S_ISREG(info.st_mode) and info.st_uid == os.getuid() and info.st_nlink == 1
                  and not info.st_mode & 0o077, 'unsafe_signer_lock')
        fcntl.flock(signer_lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        info = os.fstat(lock)
        s.require(stat.S_ISREG(info.st_mode) and info.st_uid == os.getuid() and info.st_nlink == 1
                  and not info.st_mode & 0o077, 'unsafe_root_relayer_lock')
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        for name in ('keystore', 'password_file'): bootstrap_enrollment.private_file(cfg[name])
        signer = CastSigner(cfg['l1_rpc_url'], cfg['keystore'], cfg['password_file'])
        s.require(signer._run(['wallet', 'address', '--keystore', cfg['keystore'], '--password-file', cfg['password_file']]).lower()
                  == cfg['signer'], 'root_relayer_signer_changed')
        l1 = relay.Rpc(cfg['l1_rpc_url'], timeout=cfg['rpc_timeout_seconds'])
        child = relay.Rpc(cfg['zksys_rpc_url'], timeout=cfg['rpc_timeout_seconds'])
        identity = hashlib.sha256(s.canonical(cfg)).hexdigest()
        path = root / 'gateway-roots.json'
        if path.exists() or path.is_symlink():
            journal = storage.load(path, 'root')
            s.require(journal.get('version') == 1 and journal.get('identity') == identity, 'root_journal_identity_changed')
        else:
            journal = storage.journal.State({'version': 1, 'identity': identity, 'completed': {}, 'nextRetryAt': 0,
                       'transactions': {}, 'history': []})
            storage.save(path, journal, 'root')
        save = lambda value: storage.save(path, value, 'root')
        driver = ServiceTransactionDriver(journal, save, signer.sign, signer.decode, signer.hash_raw)
        hooks = Hooks(cfg, l1, child, signer)
        failures = 0
        while True:
            try:
                status = step(cfg, journal, save, driver, hooks, int(time.time()))
                failures = 0
            except s.Error as error:
                if str(error) != 'rpc_transport_failure' or args.once: raise
                failures += 1
                print(json.dumps({'status': 'rpc_transport_retry', 'failures': failures}), flush=True)
                time.sleep(min(60, cfg['poll_seconds'] * 2 ** min(failures - 1, 6)))
                continue
            print(json.dumps({'status': status}, sort_keys=True), flush=True)
            if args.once: return
            time.sleep(cfg['poll_seconds'])
    finally:
        if signer_lock is not None: os.close(signer_lock)
        os.close(lock)


if __name__ == '__main__':
    try:
        main()
    except (s.Error, TransactionError, OSError, ValueError, TypeError, KeyError) as error:
        print(str(error) if isinstance(error, (s.Error, TransactionError)) else 'root_relayer_failed', file=sys.stderr)
        sys.exit(1)
