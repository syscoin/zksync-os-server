#!/usr/bin/env python3
"""Publish one verified period-zero roster page; retry the same journaled transaction."""

import argparse
import fcntl
import hashlib
import json
import os
from pathlib import Path
import stat
import sys
import time

import keeper as k
import relay as r
import roster
import service as s
import workflow_io as io

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from zksys_keeper_cast import CastSigner
from zksys_keeper_transactions import TransactionError, _same_transaction
from service_transaction_driver import ServiceTransactionDriver


PAGE_SIZE = 64


def validate_config(cfg):
    s.exact(cfg, ('chain_id', 'genesis_hash', 'registry', 'registry_code_hash', 'publisher', 'rpc_url',
                  'max_head_age_seconds', 'rpc_timeout_seconds', 'gas_limit', 'gas_price', 'keystore', 'password_file'))
    for key in ('genesis_hash', 'registry_code_hash'):
        s.nonzero(cfg[key])
    for key in ('registry', 'publisher'):
        s.nonzero(cfg[key], 20)
    for key in ('chain_id', 'max_head_age_seconds', 'rpc_timeout_seconds', 'gas_limit', 'gas_price'):
        s.require(type(cfg[key]) is int and cfg[key] > 0, 'invalid_roster_setting')
    s.require(cfg['rpc_timeout_seconds'] <= 30, 'rpc_timeout_too_large')
    for key in ('keystore', 'password_file'):
        s.require(Path(cfg[key]).is_absolute(), 'absolute_signer_file_required')
    r.Rpc(cfg['rpc_url'], timeout=cfg['rpc_timeout_seconds'])
    return cfg


def validated_artifact(value):
    s.exact(value, ('schema_version', 'period', 'root', 'count', 'candidates', 'proofs'))
    s.require(value['period'] == 0, 'initial_roster_must_be_period_zero')
    return roster.authenticate(value, {name: value[name] for name in ('period', 'root', 'count')})


def encode_page(artifact, start, count):
    candidates = artifact['candidates'][start:start + count]
    s.require(0 < count <= PAGE_SIZE and len(candidates) == count, 'invalid_page')
    tuples = ','.join('({},{},{},{})'.format(c['index'], c['account'], c['operator'], c['beneficiary']) for c in candidates)
    return s.cast('calldata', 'publishRosterPage(uint64,(uint32,address,address,address)[])', '0', '[' + tuples + ']').lower()


def read_snapshot(rpc, cfg, tag, now=None):
    s.require(s.uint(rpc.call('eth_chainId', [])) == cfg['chain_id'], 'registry_chain_changed')
    genesis = rpc.call('eth_getBlockByNumber', ['0x0', False])
    s.require(type(genesis) is dict and genesis.get('hash') == cfg['genesis_hash'], 'registry_genesis_changed')
    head = rpc.call('eth_getBlockByNumber', [tag, False])
    s.require(type(head) is dict, 'missing_registry_head')
    s.nonzero(head.get('hash'))
    s.uint(head.get('number'))
    timestamp = s.uint(head.get('timestamp'))
    if now is not None:
        s.require(0 <= now - timestamp <= cfg['max_head_age_seconds'], 'stale_or_future_registry_head')
    anchor = {'blockHash': head['hash'], 'requireCanonical': True}
    k.check_code(rpc, cfg['registry'], cfg['registry_code_hash'], anchor, 'registry_code_changed')
    return head, anchor


def check_anchor(rpc, cfg, head):
    again = rpc.call('eth_getBlockByNumber', [head['number'], False])
    s.require(type(again) is dict and again.get('hash') == head['hash']
              and s.uint(rpc.call('eth_chainId', [])) == cfg['chain_id'], 'registry_reorg_or_chain_changed')


def registry_state(rpc, cfg, anchor):
    target = cfg['registry']
    call = lambda signature, encoded=b'', kind='uint': r.call_word(rpc, target, signature, anchor, encoded, kind)
    s.require(call('firstServicePeriod()') == 0, 'registry_not_period_zero')
    count = call('qualifiedWrapperCount(uint64)', s.word(0))
    cutoff = call('rosterCutoff(uint64)', s.word(0))
    root = call('publishedRosterRoot(uint64)', s.word(0), 'bytes32')
    raw = k.raw_call(rpc, target, 'publicationProgress(address,uint64)', anchor,
                     bytes(12) + s.raw_hex(cfg['publisher'], 20) + s.word(0), size=64)
    s.require(raw[32:44] == bytes(12), 'invalid_progress_address')
    return {'count': count, 'cutoff': cutoff, 'root': root,
            'progress': int.from_bytes(raw[:32], 'big'), 'last': '0x' + raw[44:].hex()}


def validate_progress(state, artifact):
    s.require(state['count'] == artifact['count'], 'qualified_count_changed')
    s.require(state['root'] in (s.ZERO, artifact['root']), 'published_roster_conflict')
    if state['root'] != s.ZERO:
        return 'published'
    index = state['progress']
    s.require(index < artifact['count'], 'invalid_publication_progress')
    expected_last = s.ZERO_ADDRESS if index == 0 else artifact['candidates'][index - 1]['account']
    s.require(state['last'] == expected_last, 'publisher_prefix_changed')
    return 'pending'


def plan_page(rpc, cfg, artifact, now):
    head, anchor = read_snapshot(rpc, cfg, 'latest', now)
    state = registry_state(rpc, cfg, anchor)
    phase = validate_progress(state, artifact)
    if phase == 'published':
        check_anchor(rpc, cfg, head)
        return {'status': 'published', 'root': artifact['root'], 'anchor': head['hash']}
    s.require(s.uint(head['timestamp']) >= state['cutoff'], 'roster_not_frozen')
    start = state['progress']
    count = min(PAGE_SIZE, artifact['count'] - start)
    for candidate in artifact['candidates'][start:start + count]:
        raw = k.raw_call(rpc, cfg['registry'], 'qualifiedWrapper(address,uint64)', anchor,
                         bytes(12) + s.raw_hex(candidate['account'], 20) + s.word(0), size=128)
        s.require(raw[:32] == bytes(32), 'invalid_qualified_wrapper_index')
        for index, field in enumerate(('account', 'operator', 'beneficiary'), 1):
            word = raw[index * 32:(index + 1) * 32]
            s.require(word[:12] == bytes(12) and '0x' + word[12:].hex() == candidate[field],
                      'qualified_candidate_changed')
    data = encode_page(artifact, start, count)
    result = s.raw_hex(rpc.call('eth_call', [{'from': cfg['publisher'], 'to': cfg['registry'], 'data': data}, anchor]), 32)
    expected = s.raw_hex(artifact['root'] if start + count == artifact['count'] else s.ZERO, 32)
    s.require(result == expected, 'publication_simulation_changed')
    check_anchor(rpc, cfg, head)
    return {'status': 'ready', 'start': start, 'count': count, 'root': artifact['root'], 'anchor': head['hash'],
            'transaction': {'chainId': cfg['chain_id'], 'from': cfg['publisher'], 'to': cfg['registry'],
                            'value': 0, 'data': data}}


def private_file(path):
    target = Path(path)
    for parent in target.parents:
        s.require(not parent.is_symlink(), 'signer_path_symlink_forbidden')
    info = target.lstat()
    s.require(stat.S_ISREG(info.st_mode) and info.st_uid == os.getuid() and info.st_nlink == 1
              and not info.st_mode & 0o077, 'unsafe_signer_file')


def run_send(rpc, cfg, artifact, directory, now):
    root = io.directory(directory)
    lock = os.open(root / 'roster-publish.lock', os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW, 0o600)
    try:
        info = os.fstat(lock)
        s.require(stat.S_ISREG(info.st_mode) and info.st_uid == os.getuid() and info.st_nlink == 1
                  and not info.st_mode & 0o077, 'unsafe_roster_lock')
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        identity = hashlib.sha256(s.canonical({'config': cfg, 'roster': artifact})).hexdigest()
        path = root / 'roster-publish.json'
        if path.exists() or path.is_symlink():
            journal = io.private_json(path)
            s.require(journal.get('version') == 1 and journal.get('identity') == identity, 'roster_journal_changed')
        else:
            journal = {'version': 1, 'identity': identity, 'transactions': {}}
        s.require(type(journal.get('transactions')) is dict and set(journal['transactions']) <= {'roster'},
                  'invalid_roster_journal')
        head, _ = read_snapshot(rpc, cfg, 'latest', now)
        check_anchor(rpc, cfg, head)
        private_file(cfg['keystore'])
        private_file(cfg['password_file'])
        signer = CastSigner(cfg['rpc_url'], cfg['keystore'], cfg['password_file'])
        signer_address = signer._run(['wallet', 'address', '--keystore', cfg['keystore'], '--password-file', cfg['password_file']]).lower()
        s.require(signer_address == cfg['publisher'], 'publisher_signer_changed')
        save = lambda obj: r.atomic_json(path, obj)
        driver = ServiceTransactionDriver(journal, save, signer.sign, signer.decode, signer.hash_raw)
        entry = journal['transactions'].get('roster')
        if entry:
            meta = entry['metadata']
            s.require(type(meta) is dict and set(meta) == {'start', 'count'} and type(meta['start']) is int
                      and type(meta['count']) is int and 0 <= meta['start'] < artifact['count'], 'invalid_roster_journal')
            data = encode_page(artifact, meta['start'], meta['count'])
            expected = {'from': cfg['publisher'], 'to': cfg['registry'], 'chainId': cfg['chain_id'], 'nonce': entry['nonce'],
                        'value': 0, 'gas': cfg['gas_limit'], 'gasPrice': cfg['gas_price'], 'data': data}
            s.require(entry['kind'] == 'roster' and _same_transaction(expected, signer.decode(entry['raw'])),
                      'journal_transaction_changed')
            receipt = driver.receipt('roster', rpc.call)
            if receipt is None:
                driver.rebroadcast_if_missing('roster', rpc.call)
                return {'status': 'pending', 'transaction_hash': entry['txHash']}
            head, anchor = read_snapshot(rpc, cfg, 'finalized')
            state = registry_state(rpc, cfg, anchor)
            phase = validate_progress(state, artifact)
            s.require(phase == 'published' or state['progress'] >= meta['start'] + meta['count'],
                      'finalized_page_not_reflected')
            check_anchor(rpc, cfg, head)
            driver.complete('roster')
        plan = plan_page(rpc, cfg, artifact, now)
        if plan['status'] == 'published':
            return plan
        tx = plan['transaction']
        driver.begin('roster', rpc.call, cfg['chain_id'], cfg['publisher'], cfg['registry'], tx['data'], 0,
                     cfg['gas_limit'], cfg['gas_price'], kind='roster',
                     metadata={'start': plan['start'], 'count': plan['count']})
        return {'status': 'submitted', 'start': plan['start'], 'count': plan['count'],
                'transaction_hash': journal['transactions']['roster']['txHash']}
    finally:
        os.close(lock)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--config', required=True)
    parser.add_argument('--roster', required=True)
    parser.add_argument('--state-dir')
    parser.add_argument('--send', action='store_true')
    args = parser.parse_args()
    cfg = validate_config(io.private_json(args.config))
    artifact = validated_artifact(io.private_json(args.roster))
    rpc = r.Rpc(cfg['rpc_url'], timeout=cfg['rpc_timeout_seconds'])
    if args.send:
        s.require(args.state_dir is not None, 'state_directory_required')
        result = run_send(rpc, cfg, artifact, args.state_dir, int(time.time()))
    else:
        result = plan_page(rpc, cfg, artifact, int(time.time()))
    print(json.dumps(result, sort_keys=True))


if __name__ == '__main__':
    try:
        main()
    except (s.Error, TransactionError, OSError, ValueError, TypeError, KeyError) as error:
        print(str(error) if isinstance(error, (s.Error, TransactionError)) else 'roster_publication_failed', file=sys.stderr)
        sys.exit(1)
