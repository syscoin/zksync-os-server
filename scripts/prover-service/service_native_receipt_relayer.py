#!/usr/bin/env python3
"""Deliver Gateway-native receipts via the existing NEVM sink when needed."""
import argparse
import copy
import fcntl
import hashlib
import json
import os
from pathlib import Path
import stat
import sys
import time

import bootstrap_enrollment
import relay
import service as s
import service_gateway_root_relayer as roots
import service_receipt_relay as receipts
import service_receipt_runner as machine
import workflow_io as io
from service_transaction_driver import ServiceTransactionDriver

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from zksys_keeper_cast import CastSigner
from zksys_keeper_transactions import TransactionError, _same_transaction


def encode(report, cfg):
    s.require(report['fromRoot'] and report['kind'] in ('accepted', 'activation'), 'native_report_required')
    suffix = [(s.word(cfg['priority_gas_limit']), False), (s.word(cfg['gas_per_pubdata']), False),
              (receipts.address_word(cfg['refund']), False)]
    if report['kind'] == 'activation':
        fields = [(s.raw_hex(report['vk_hash'], 32), False), (s.raw_hex(report['roster_root'], 32), False)]
        data = receipts.call_data('relayActivation(bytes32,bytes32,uint256,uint256,address)', fields + suffix)
        child = receipts.call_data('receiveRootActivation(bytes32,bytes32)', fields)
    else:
        duties = receipts.dynamic_array([s.encode_duty(d) for d in report['duties']])
        fields = [(s.encode_fields(s.PACKAGE, report['accepted']), False), (duties, True),
                  (s.word(int(report['bootstrap'])), False)]
        package = relay.abi_tuple(s.PACKAGE)
        duty = relay.abi_tuple(s.DUTY + [('operatorSignature', 'bytes')])
        data = receipts.call_data('relayAccepted(' + package + ',' + duty + '[],bool,uint256,uint256,address)', fields + suffix)
        child = receipts.call_data('receiveRootAccepted(' + package + ',' + duty + '[],bool)', fields)
    return data, child, s.keccak(receipts.report_message(report))


class Hooks(receipts.Hooks):
    def prepare(self, reports):
        s.require(len(reports) == 1, 'native_single_receipt_required')
        c, report = self.cfg, reports[0]
        self.validate_report(report)
        data, child_data, commitment = encode(report, c)
        _, l1 = self.anchor(self.l1, c['l1_chain_id'], c['l1_genesis'])
        _, child = self.anchor(self.zksys, c['zksys_chain_id'], c['zksys_genesis'])
        fee = relay.call_word(self.l1, c['bridgehub'], 'l2TransactionBaseCost(uint256,uint256,uint256,uint256)', l1,
                              s.word(c['zksys_chain_id']) + s.word(c['l1_gas_price'])
                              + s.word(c['priority_gas_limit']) + s.word(c['gas_per_pubdata']))
        s.require(fee > 0, 'native_receipt_fee_invalid')
        try:
            result = self.zksys.call('eth_call', [{'from': roots.alias(c['root_sink']), 'to': c['receiver'],
                'data': child_data, 'gas': hex(c['priority_gas_limit'])}, child])
            s.require(result == '0x', 'native_receiver_simulation_changed')
            self.l1.call('eth_call', [{'from': c['signer'], 'to': c['root_sink'], 'data': data,
                                      'gas': hex(c['l1_gas_limit']), 'gasPrice': hex(c['l1_gas_price']), 'value': hex(fee)}, l1])
        except relay.RpcError:
            raise machine.BatchRejected('native_receipt_simulation_reverted') from None
        request = dict(chain_id=c['l1_chain_id'], sender=c['signer'], to=c['root_sink'], data=data,
                       value=fee, gas=c['l1_gas_limit'], gas_price=c['l1_gas_price'])
        return request, {'request': request, 'childData': child_data, 'commitment': commitment}

    def verify_journal(self, entry, reports):
        s.require(len(reports) == 1, 'native_single_receipt_required')
        c, meta = self.cfg, entry['metadata']
        data, child, commitment = encode(reports[0], c)
        expected = {'from': c['signer'], 'to': c['root_sink'], 'chainId': c['l1_chain_id'],
                    'nonce': entry['nonce'], 'gas': c['l1_gas_limit'], 'gasPrice': c['l1_gas_price'],
                    'value': meta['request']['value'], 'data': data}
        s.require(entry['kind'] == 'delivery' and entry['sender'] == c['signer']
                  and entry['chainId'] == c['l1_chain_id'] and meta['request']['data'] == data
                  and meta['childData'] == child and meta['commitment'] == commitment
                  and self.signer.hash_raw(entry['raw']) == entry['txHash']
                  and _same_transaction(expected, self.signer.decode(entry['raw'])), 'native_journal_changed')


class Runner(machine.ReceiptRunner):
    def __init__(self, *args, claim_journal, claim_identity, requests_file=None, requests_identity=None,
                 fallback_delay_seconds=300, **kwargs):
        super().__init__(*args, **kwargs)
        self.claim_journal, self.claim_identity = claim_journal, claim_identity
        self.requests_file, self.requests_identity = requests_file, requests_identity
        self.fallback_delay_seconds = fallback_delay_seconds
        self.bundled = set()

    def run_once(self):
        if 'receipt' not in self.journal['transactions']:
            for path in (self.claim_journal, self.requests_file):
                if path and not Path(path).exists() and not Path(path).is_symlink():
                    return 'awaiting_claim_state'
        return super().run_once()

    def discover(self):
        claim_path = Path(self.claim_journal)
        if not claim_path.exists() and not claim_path.is_symlink(): return
        source = io.private_json(claim_path)
        s.require(source.get('version') == 1 and source.get('identity') == self.claim_identity
                  and type(source.get('records')) is dict, 'native_claim_journal_changed')
        records = source['records']
        if self.requests_file:
            inbox_path = Path(self.requests_file)
            if not inbox_path.exists() and not inbox_path.is_symlink(): return
            inbox = io.private_json(inbox_path)
            s.exact(inbox, ('version', 'identity', 'requests'))
            s.require(inbox['version'] == 1 and inbox['identity'] == self.requests_identity
                      and type(inbox['requests']) is list, 'native_request_inbox_changed')
            self.bundled = set()
            for item in inbox['requests']:
                s.exact(item, ('batch', 'messageHash'))
                s.require(type(item['batch']) is int and item['batch'] > 0, 'native_invalid_request')
                s.raw_hex(item['messageHash'], 32)
                self.bundled.add(item['messageHash'])
        s.exact(source.get('cursor'), ('gateway', 'l1'))
        for key, existing in self.journal['records'].items():
            observed = records.get(key)
            if existing['status'] == 'pending' and observed and observed.get('status') == 'complete':
                existing['status'] = 'complete'
        pending_bootstrap = any(v.get('status') == 'pending' and v.get('report', {}).get('bootstrap')
                                for v in records.values())
        activations = [v['report'] for v in records.values() if v.get('report', {}).get('kind') == 'activation']
        for report in activations:
            self.hooks.validate_report(report)
        pairs = {v['roster_root'] for v in activations if v['fromRoot']
                 and source['cursor']['l1'] > s.uint(v['sourceBlockNumber'])}
        pairs &= {v['roster_root'] for v in activations if not v['fromRoot']
                  and source['cursor']['gateway'] > s.uint(v['sourceBlockNumber'])}
        # Bootstrap reports from both chains are visible when both activation
        # events have passed their source cursors; only then may activation complete.
        activation_ready = not pending_bootstrap and bool(pairs)
        for key, existing in list(self.journal['records'].items()):
            if existing['status'] == 'pending' and existing['report']['kind'] == 'activation' and (
                not activation_ready or existing['report']['roster_root'] not in pairs):
                del self.journal['records'][key]
        for key, value in records.items():
            report = value.get('report', {})
            if value.get('status') != 'pending' or not report.get('fromRoot') or report.get('kind') not in ('accepted', 'activation'):
                continue
            if report['kind'] == 'activation' and (not activation_ready or report['roster_root'] not in pairs):
                continue
            self.hooks.validate_report(report)
            if key not in self.journal['records']:
                self.journal['records'][key] = {'report': copy.deepcopy(report), 'status': 'pending',
                                                'failures': 0, 'nextRetryAt': 0, 'firstSeen': int(self.clock())}
            else:
                s.require(self.journal['records'][key]['report'] == report, 'native_report_changed')
        self.save(self.journal)

    def select(self):
        now = int(self.clock())
        eligible = [(k, v) for k, v in self.journal['records'].items()
                    if v['status'] == 'pending' and v['nextRetryAt'] <= now
                    and (v['report']['kind'] != 'accepted' or not self.bundled
                         or s.keccak(receipts.report_message(v['report'])) not in self.bundled
                         or now >= v.get('firstSeen', now) + self.fallback_delay_seconds)]
        # Bootstrap precedes activation; activation precedes regular service reports.
        eligible.sort(key=lambda kv: (0 if kv[1]['report'].get('bootstrap') else
                                      1 if kv[1]['report']['kind'] == 'activation' else 2, kv[0]))
        return [eligible[0][0]] if eligible else []


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--config', required=True)
    parser.add_argument('--state-dir', required=True)
    parser.add_argument('--execute', action='store_true')
    parser.add_argument('--once', action='store_true')
    args = parser.parse_args()
    config = io.private_json(args.config)
    s.exact(config, ('receipt', 'claim_journal', 'claim_identity', 'requests_file', 'requests_identity',
                     'fallback_delay_seconds', 'signer_lock_dir'))
    cfg = receipts.configuration(config['receipt'])
    for key in ('claim_journal', 'requests_file', 'signer_lock_dir'):
        s.require(type(config[key]) is str and Path(config[key]).is_absolute(), 'native_absolute_path_required')
    s.nonzero(config['claim_identity']); s.nonzero(config['requests_identity'])
    s.require(type(config['fallback_delay_seconds']) is int and 0 < config['fallback_delay_seconds'] <= 86400,
              'native_fallback_delay_invalid')
    if not args.execute:
        print(json.dumps({'status': 'configuration_validated', 'broadcast': False})); return
    directory = io.directory(args.state_dir)
    signer_dir = io.directory(config['signer_lock_dir'])
    lock_path = signer_dir / ('l1-' + str(cfg['l1_chain_id']) + '-' + cfg['signer'][2:] + '.lock')
    lock = os.open(lock_path, os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW, 0o600)
    try:
        info = os.fstat(lock)
        s.require(stat.S_ISREG(info.st_mode) and info.st_uid == os.getuid() and info.st_nlink == 1
                  and not info.st_mode & 0o077, 'unsafe_signer_lock')
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        for key in ('keystore', 'password_file'): bootstrap_enrollment.private_file(cfg[key])
        signer = CastSigner(cfg['l1_rpc_url'], cfg['keystore'], cfg['password_file'])
        s.require(signer._run(['wallet', 'address', '--keystore', cfg['keystore'], '--password-file', cfg['password_file']]).lower()
                  == cfg['signer'], 'native_signer_changed')
        rpc = lambda key: relay.Rpc(cfg[key], timeout=cfg['rpc_timeout_seconds'])
        l1, gateway, child = rpc('l1_rpc_url'), rpc('gateway_rpc_url'), rpc('zksys_rpc_url')
        path = directory / 'service-native-receipts.json'
        identity = hashlib.sha256(s.canonical(config)).hexdigest()
        if path.exists() or path.is_symlink():
            journal = io.private_json(path)
            s.require(journal.get('version') == 1 and journal.get('identity') == identity, 'native_journal_identity_changed')
        else:
            journal = {'version': 1, 'identity': identity, 'transactions': {}, 'records': {}, 'split': [], 'history': []}
            relay.atomic_json(path, journal)
        save = lambda value: relay.atomic_json(path, value)
        driver = ServiceTransactionDriver(journal, save, signer.sign, signer.decode, signer.hash_raw)
        hooks = Hooks(cfg, l1, gateway, child, signer)
        worker = Runner(journal, save, driver, l1.call, child.call, hooks,
                        claim_journal=config['claim_journal'], claim_identity=config['claim_identity'][2:],
                        requests_file=config['requests_file'], requests_identity=config['requests_identity'][2:],
                        fallback_delay_seconds=config['fallback_delay_seconds'])
        failures = 0
        while True:
            try:
                status = worker.run_once()
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
        os.close(lock)


if __name__ == '__main__':
    try:
        main()
    except (s.Error, TransactionError, OSError, ValueError, TypeError, KeyError) as error:
        print(str(error) if isinstance(error, (s.Error, TransactionError)) else 'native_receipt_relay_failed', file=sys.stderr)
        sys.exit(1)
