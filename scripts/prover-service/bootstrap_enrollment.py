#!/usr/bin/env python3
"""Enroll one pre-consented Sentry for period-zero proving with a durable transaction."""

import argparse
import fcntl
import hashlib
import json
import os
from pathlib import Path
import re
import stat
import sys
import time

import keeper as k
import relay as r
import service as s
import workflow_io as io

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from zksys_keeper_cast import CastSigner
from zksys_keeper_transactions import TransactionError, _same_transaction
from service_transaction_driver import ServiceTransactionDriver


def configuration(value):
    s.exact(value, ('settings', 'rpc_url', 'genesis_hash', 'registry_code_hash', 'max_head_age_seconds',
                    'rpc_timeout_seconds', 'gas_limit', 'gas_price', 'keystore', 'password_file'))
    s.config(value['settings'])
    for key in ('genesis_hash', 'registry_code_hash'):
        s.nonzero(value[key])
    for key in ('max_head_age_seconds', 'rpc_timeout_seconds', 'gas_limit', 'gas_price'):
        s.require(type(value[key]) is int and value[key] > 0, 'invalid_enrollment_setting')
    s.require(value['rpc_timeout_seconds'] <= 30, 'rpc_timeout_too_large')
    for key in ('keystore', 'password_file'):
        s.require(type(value[key]) is str and Path(value[key]).is_absolute(), 'absolute_signer_file_required')
    r.Rpc(value['rpc_url'], timeout=value['rpc_timeout_seconds'])
    return value


def private_file(path):
    target = Path(path)
    for parent in target.parents:
        s.require(not parent.is_symlink(), 'signer_path_symlink_forbidden')
    info = target.lstat()
    s.require(stat.S_ISREG(info.st_mode) and info.st_uid == os.getuid() and info.st_nlink == 1
              and not info.st_mode & 0o077, 'unsafe_signer_file')


def head(rpc, cfg, tag, now=None):
    settings = cfg['settings']
    s.require(rpc.call('eth_chainId', []) == settings['registry_chain_id'], 'registry_chain_changed')
    genesis = rpc.call('eth_getBlockByNumber', ['0x0', False])
    s.require(type(genesis) is dict and genesis.get('hash') == cfg['genesis_hash'], 'registry_genesis_changed')
    block = rpc.call('eth_getBlockByNumber', [tag, False])
    s.require(type(block) is dict, 'missing_registry_block')
    s.nonzero(block.get('hash'))
    s.uint(block.get('number'))
    timestamp = s.uint(block.get('timestamp'))
    if now is not None:
        s.require(0 <= now - timestamp <= cfg['max_head_age_seconds'], 'stale_or_future_registry_head')
    anchor = {'blockHash': block['hash'], 'requireCanonical': True}
    k.check_code(rpc, settings['registry'], cfg['registry_code_hash'], anchor, 'registry_code_changed')
    return block, anchor


def check_anchor(rpc, cfg, block):
    canonical = rpc.call('eth_getBlockByNumber', [block['number'], False])
    s.require(type(canonical) is dict and canonical.get('hash') == block['hash']
              and rpc.call('eth_chainId', []) == cfg['settings']['registry_chain_id'],
              'registry_reorg_or_chain_changed')


def view(rpc, settings, anchor, signature, args=b'', kind='uint'):
    return r.call_word(rpc, settings['registry'], signature, anchor, args, kind)


def subscription_state(rpc, cfg, block, anchor, signed, subscription_hash):
    settings, sub = cfg['settings'], signed['subscription']
    call = lambda signature, args=b'', kind='uint': view(rpc, settings, anchor, signature, args, kind)
    s.require(call('policyHash()', kind='bytes32') == settings['policy_hash']
              and call('sharedSequencer()', kind='address') == settings['sequencer']
              and call('dutiesPerRound()') == settings['duties_per_round']
              and call('firstServicePeriod()') == 0, 'registry_configuration_changed')
    address = bytes(12) + s.raw_hex(sub['account'], 20)
    sequencer = bytes(12) + s.raw_hex(sub['sequencer'], 20)
    stored_hash = call('subscriptionAt(address,address,uint64)', address + sequencer + s.word(0), 'bytes32')
    if stored_hash == s.ZERO:
        check_anchor(rpc, cfg, block)
        return False
    s.require(stored_hash == subscription_hash, 'period_zero_subscription_conflict')
    operator = bytes(12) + s.raw_hex(sub['operator'], 20)
    s.require(call('operatorAccountAt(address,uint64)', operator + s.word(0), 'address') == sub['account'],
              'stored_operator_mismatch')
    raw = k.raw_call(rpc, settings['registry'], 'subscription(bytes32)', anchor, s.raw_hex(subscription_hash),
                     size=32 * len(s.SUBSCRIPTION))
    s.require(raw == s.encode_fields(s.SUBSCRIPTION, sub), 'stored_subscription_mismatch')
    check_anchor(rpc, cfg, block)
    return True


def prepare(rpc, cfg, signed, operator_signature, now):
    settings = cfg['settings']
    prepared = s.prepare_enrollment(settings, signed, operator_signature)
    sub, tx = signed['subscription'], prepared['transaction']
    s.require(sub['firstPeriod'] == 0 and sub['lastPeriod'] >= 0, 'period_zero_subscription_required')
    finalized, finalized_anchor = head(rpc, cfg, 'finalized')
    if subscription_state(rpc, cfg, finalized, finalized_anchor, signed, prepared['subscription_hash']):
        return {'status': 'enrolled', 'subscription_hash': prepared['subscription_hash']}
    s.require(view(rpc, settings, finalized_anchor, 'seniorBonus(address)',
                   bytes(12) + s.raw_hex(sub['account'], 20)) > 0, 'finalized_senior_membership_required')
    check_anchor(rpc, cfg, finalized)
    latest, anchor = head(rpc, cfg, 'latest', now)
    s.require(view(rpc, settings, anchor, 'firstServicePeriod()') == 0
              and not view(rpc, settings, anchor, 'serviceActive()', kind='bool')
              and view(rpc, settings, anchor, 'nextAdmissionPeriod()') == 0,
              'period_zero_enrollment_closed')
    result = rpc.call('eth_call', [{'from': sub['account'], 'to': settings['registry'],
                                   'data': tx['data'], 'value': '0x0'}, anchor])
    s.require(result == prepared['subscription_hash'], 'enrollment_simulation_changed')
    check_anchor(rpc, cfg, latest)
    return {'status': 'ready', 'subscription_hash': prepared['subscription_hash'], 'transaction': tx,
            'finalized_block_hash': finalized['hash'], 'anchor': latest['hash']}


def run_send(rpc, cfg, signed, operator_signature, directory, now):
    root = io.directory(directory)
    lock = os.open(root / 'enrollment.lock', os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW, 0o600)
    try:
        info = os.fstat(lock)
        s.require(stat.S_ISREG(info.st_mode) and info.st_uid == os.getuid() and info.st_nlink == 1
                  and not info.st_mode & 0o077, 'unsafe_enrollment_lock')
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        prepared = s.prepare_enrollment(cfg['settings'], signed, operator_signature)
        sub, tx = signed['subscription'], prepared['transaction']
        s.require(sub['firstPeriod'] == 0, 'period_zero_subscription_required')
        identity = hashlib.sha256(s.canonical({'config': cfg, 'enrollment': prepared})).hexdigest()
        path = root / 'enrollment.json'
        if path.exists() or path.is_symlink():
            journal = io.private_json(path)
            s.require(journal.get('version') == 1 and journal.get('identity') == identity,
                      'enrollment_journal_changed')
        else:
            journal = {'version': 1, 'identity': identity, 'transactions': {}}
        s.require(type(journal.get('transactions')) is dict and set(journal['transactions']) <= {'enrollment'},
                  'invalid_enrollment_journal')
        private_file(cfg['keystore'])
        private_file(cfg['password_file'])
        signer = CastSigner(cfg['rpc_url'], cfg['keystore'], cfg['password_file'])
        signer_address = signer._run(['wallet', 'address', '--keystore', cfg['keystore'],
                                      '--password-file', cfg['password_file']]).lower()
        s.require(signer_address == sub['account'], 'enrollment_signer_changed')
        driver = ServiceTransactionDriver(journal, lambda obj: r.atomic_json(path, obj), signer.sign, signer.decode, signer.hash_raw)
        entry = journal['transactions'].get('enrollment')
        if entry:
            expected = {'from': sub['account'], 'to': cfg['settings']['registry'],
                        'chainId': cfg['settings']['registry_chain_id'], 'nonce': entry['nonce'],
                        'value': 0, 'gas': cfg['gas_limit'], 'gasPrice': cfg['gas_price'], 'data': tx['data']}
            s.require(entry.get('kind') == 'enrollment'
                      and entry.get('metadata') == {'subscription_hash': prepared['subscription_hash']}
                      and entry.get('chainId') == s.uint(cfg['settings']['registry_chain_id'])
                      and entry.get('sender') == sub['account']
                      and signer.hash_raw(entry['raw']) == entry['txHash']
                      and _same_transaction(expected, signer.decode(entry['raw'])),
                      'enrollment_journal_transaction_changed')
            block, anchor = head(rpc, cfg, 'finalized')
            check_anchor(rpc, cfg, block)
            receipt = driver.receipt('enrollment', rpc.call)
            if receipt is None:
                driver.rebroadcast_if_missing('enrollment', rpc.call)
                return {'status': 'pending', 'transaction_hash': entry['txHash']}
            block, anchor = head(rpc, cfg, 'finalized')
            s.require(subscription_state(rpc, cfg, block, anchor, signed, prepared['subscription_hash']),
                      'finalized_enrollment_not_reflected')
            driver.complete('enrollment')
            return {'status': 'enrolled', 'subscription_hash': prepared['subscription_hash']}
        result = prepare(rpc, cfg, signed, operator_signature, now)
        if result['status'] == 'enrolled':
            return result
        driver.begin('enrollment', rpc.call, cfg['settings']['registry_chain_id'], sub['account'],
                     cfg['settings']['registry'], tx['data'], 0, cfg['gas_limit'], cfg['gas_price'],
                     kind='enrollment', metadata={'subscription_hash': prepared['subscription_hash']})
        return {'status': 'submitted', 'subscription_hash': prepared['subscription_hash'],
                'transaction_hash': journal['transactions']['enrollment']['txHash']}
    finally:
        os.close(lock)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--config', required=True)
    parser.add_argument('--subscription', required=True, help='private account-signed subscription JSON')
    parser.add_argument('--operator-signature', required=True, help='private file with operator signature')
    parser.add_argument('--state-dir')
    parser.add_argument('--send', action='store_true')
    args = parser.parse_args()
    cfg = configuration(io.private_json(args.config))
    signed = io.private_json(args.subscription)
    operator_signature = s.read(Path(args.operator_signature), private=True).decode().strip()
    rpc = r.Rpc(cfg['rpc_url'], timeout=cfg['rpc_timeout_seconds'])
    if args.send:
        s.require(args.state_dir is not None, 'state_directory_required')
        result = run_send(rpc, cfg, signed, operator_signature, args.state_dir, int(time.time()))
    else:
        result = prepare(rpc, cfg, signed, operator_signature, int(time.time()))
    print(json.dumps(result, sort_keys=True))


if __name__ == '__main__':
    try:
        main()
    except (s.Error, TransactionError, OSError, ValueError, TypeError, KeyError) as error:
        print(str(error) if isinstance(error, (s.Error, TransactionError)) else 'bootstrap_enrollment_failed', file=sys.stderr)
        sys.exit(1)
